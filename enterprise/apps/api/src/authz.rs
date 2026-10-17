//! Roles and permissions, as enforced. Roles are only the names people see; every handler asks
//! for a [`Permission`] on a scope (an organization the caller is a member of, or the platform
//! for operators), and the answer comes from here and the database, never from the client.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Admin,
    Member,
    BillingManager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorRole {
    PlatformAdmin,
    OperationsOperator,
    SupportOperator,
    BillingOperator,
    SecurityAuditor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    // ---- in an organization
    OrgRead,
    OrgUpdate,
    MembersRead,
    MembersManage,
    /// every rental of the organization (members see the Macs assigned to them)
    RentalsRead,
    RentalsCreate,
    RentalsCancel,
    RentalsAssign,
    MacsConnect,
    BillingRead,
    BillingManage,
    BrandingManage,
    DomainsManage,
    AuditRead,
    // ---- on the platform (Taro Operations)
    OpsOverviewRead,
    OpsOrgsRead,
    OpsInventoryRead,
    OpsInventoryManage,
    OpsJobsRead,
    OpsJobsManage,
    OpsProvidersRead,
    OpsProvidersManage,
    OpsRelaysRead,
    OpsRelaysManage,
    OpsBillingRead,
    OpsBillingManage,
    OpsIncidentsRead,
    OpsIncidentsManage,
    OpsAuditRead,
    OpsFlagsRead,
    OpsFlagsManage,
    OpsAccessRead,
    OpsAccessManage,
    OpsConfigManage,
}

use Permission::*;

pub const ORG_PERMISSIONS: &[Permission] = &[
    OrgRead,
    OrgUpdate,
    MembersRead,
    MembersManage,
    RentalsRead,
    RentalsCreate,
    RentalsCancel,
    RentalsAssign,
    MacsConnect,
    BillingRead,
    BillingManage,
    BrandingManage,
    DomainsManage,
    AuditRead,
];
pub const OPS_PERMISSIONS: &[Permission] = &[
    OpsOverviewRead,
    OpsOrgsRead,
    OpsInventoryRead,
    OpsInventoryManage,
    OpsJobsRead,
    OpsJobsManage,
    OpsProvidersRead,
    OpsProvidersManage,
    OpsRelaysRead,
    OpsRelaysManage,
    OpsBillingRead,
    OpsBillingManage,
    OpsIncidentsRead,
    OpsIncidentsManage,
    OpsAuditRead,
    OpsFlagsRead,
    OpsFlagsManage,
    OpsAccessRead,
    OpsAccessManage,
    OpsConfigManage,
];

impl Role {
    pub const ALL: [Role; 4] = [Role::Owner, Role::Admin, Role::Member, Role::BillingManager];

    pub fn key(&self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Member => "member",
            Role::BillingManager => "billing_manager",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.key() == s)
    }

    pub fn permissions(&self) -> &'static [Permission] {
        match self {
            Role::Owner => ORG_PERMISSIONS,
            Role::Admin => &[
                OrgRead,
                OrgUpdate,
                MembersRead,
                MembersManage,
                RentalsRead,
                RentalsCreate,
                RentalsCancel,
                RentalsAssign,
                MacsConnect,
                BillingRead,
                BrandingManage,
                DomainsManage,
                AuditRead,
            ],
            Role::Member => &[OrgRead, MembersRead, MacsConnect],
            Role::BillingManager => &[
                OrgRead,
                MembersRead,
                RentalsRead,
                BillingRead,
                BillingManage,
            ],
        }
    }

    pub fn can(&self, p: Permission) -> bool {
        self.permissions().contains(&p)
    }

    /// Which roles this role may hand out or take away: only an owner makes owners.
    pub fn may_assign(&self, target: Role) -> bool {
        match self {
            Role::Owner => true,
            Role::Admin => target != Role::Owner,
            _ => false,
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Role::Owner => "Owns the organization: everything, including owners and billing",
            Role::Admin => "Manages members, rentals, branding and domains",
            Role::Member => "Uses the Macs assigned to them",
            Role::BillingManager => "Manages billing and invoices",
        }
    }
}

impl OperatorRole {
    pub const ALL: [OperatorRole; 5] = [
        OperatorRole::PlatformAdmin,
        OperatorRole::OperationsOperator,
        OperatorRole::SupportOperator,
        OperatorRole::BillingOperator,
        OperatorRole::SecurityAuditor,
    ];

    pub fn key(&self) -> &'static str {
        match self {
            OperatorRole::PlatformAdmin => "platform_admin",
            OperatorRole::OperationsOperator => "operations_operator",
            OperatorRole::SupportOperator => "support_operator",
            OperatorRole::BillingOperator => "billing_operator",
            OperatorRole::SecurityAuditor => "security_auditor",
        }
    }

    pub fn parse(s: &str) -> Option<OperatorRole> {
        OperatorRole::ALL.into_iter().find(|r| r.key() == s)
    }

    pub fn permissions(&self) -> &'static [Permission] {
        match self {
            OperatorRole::PlatformAdmin => OPS_PERMISSIONS,
            OperatorRole::OperationsOperator => &[
                OpsOverviewRead,
                OpsOrgsRead,
                OpsInventoryRead,
                OpsInventoryManage,
                OpsJobsRead,
                OpsJobsManage,
                OpsProvidersRead,
                OpsRelaysRead,
                OpsRelaysManage,
                OpsIncidentsRead,
                OpsIncidentsManage,
                OpsFlagsRead,
            ],
            OperatorRole::SupportOperator => &[
                OpsOverviewRead,
                OpsOrgsRead,
                OpsInventoryRead,
                OpsJobsRead,
                OpsProvidersRead,
                OpsRelaysRead,
                OpsIncidentsRead,
                OpsBillingRead,
            ],
            OperatorRole::BillingOperator => &[
                OpsOverviewRead,
                OpsOrgsRead,
                OpsBillingRead,
                OpsBillingManage,
                OpsIncidentsRead,
                OpsJobsRead,
            ],
            OperatorRole::SecurityAuditor => &[
                OpsOverviewRead,
                OpsOrgsRead,
                OpsAuditRead,
                OpsAccessRead,
                OpsIncidentsRead,
            ],
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            OperatorRole::PlatformAdmin => "Administers the platform",
            OperatorRole::OperationsOperator => {
                "Runs inventory, provisioning, relays and incidents"
            }
            OperatorRole::SupportOperator => "Diagnoses customer issues (read only)",
            OperatorRole::BillingOperator => "Handles billing issues",
            OperatorRole::SecurityAuditor => "Reads audit logs and access",
        }
    }
}

impl Permission {
    pub fn key(&self) -> String {
        // OrgRead -> org.read, OpsJobsManage -> ops.jobs.manage
        let name = format!("{self:?}");
        let mut out = String::new();
        for (i, c) in name.chars().enumerate() {
            if c.is_ascii_uppercase() && i > 0 {
                out.push('.');
            }
            out.push(c.to_ascii_lowercase());
        }
        out
    }
}

/// Keep the roles, permissions and role_permissions tables equal to the code (for reporting).
pub async fn sync_catalog(db: &sqlx::PgPool) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM role_permissions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM permissions")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM roles").execute(&mut *tx).await?;
    for p in ORG_PERMISSIONS.iter().chain(OPS_PERMISSIONS) {
        sqlx::query("INSERT INTO permissions (key, description) VALUES ($1, $2)")
            .bind(p.key())
            .bind(format!("{p:?}"))
            .execute(&mut *tx)
            .await?;
    }
    for r in Role::ALL {
        sqlx::query("INSERT INTO roles (key, scope, description) VALUES ($1, 'organization', $2)")
            .bind(r.key())
            .bind(r.description())
            .execute(&mut *tx)
            .await?;
        for p in r.permissions() {
            sqlx::query("INSERT INTO role_permissions VALUES ($1, $2)")
                .bind(r.key())
                .bind(p.key())
                .execute(&mut *tx)
                .await?;
        }
    }
    for r in OperatorRole::ALL {
        sqlx::query("INSERT INTO roles (key, scope, description) VALUES ($1, 'platform', $2)")
            .bind(r.key())
            .bind(r.description())
            .execute(&mut *tx)
            .await?;
        for p in r.permissions() {
            sqlx::query("INSERT INTO role_permissions VALUES ($1, $2)")
                .bind(r.key())
                .bind(p.key())
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn customer_roles_never_hold_operator_permissions() {
        for r in Role::ALL {
            assert!(
                r.permissions().iter().all(|p| ORG_PERMISSIONS.contains(p)),
                "{r:?}"
            );
        }
        for r in OperatorRole::ALL {
            assert!(
                r.permissions().iter().all(|p| OPS_PERMISSIONS.contains(p)),
                "{r:?}"
            );
        }
    }

    #[test]
    fn who_may_do_what() {
        assert!(Role::Owner.can(BillingManage) && !Role::Admin.can(BillingManage));
        assert!(
            Role::BillingManager.can(BillingManage) && !Role::BillingManager.can(RentalsCreate)
        );
        assert!(Role::Member.can(MacsConnect) && !Role::Member.can(RentalsRead));
        assert!(
            Role::Admin.may_assign(Role::Member)
                && !Role::Admin.may_assign(Role::Owner)
                && !Role::Member.may_assign(Role::Member)
        );
        assert!(
            OperatorRole::SupportOperator
                .permissions()
                .iter()
                .all(|p| !format!("{p:?}").ends_with("Manage")),
            "support is read only"
        );
        assert_eq!(OpsJobsManage.key(), "ops.jobs.manage");
        assert_eq!(Role::parse("billing_manager"), Some(Role::BillingManager));
    }
}
