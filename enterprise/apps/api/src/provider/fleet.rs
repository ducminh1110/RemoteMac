//! The provider's own Macs, enrolled by Taro for Mac (Enterprise edition): the server is the
//! provider. A Mac is free while its heartbeat is recent and offline otherwise; preparing it for a
//! rental and releasing it move its session password to a new epoch, which ends every session
//! made with the old one (the Mac restarts its session when its heartbeat brings the new epoch).
//!
//! Passwords are derived from the server's secret, the Mac and the epoch: never stored, and the
//! same on every call, so the Mac and a viewer's connection details always agree.

use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;

use super::*;

/// A heartbeat older than this: the Mac is offline.
pub const ONLINE_SECONDS: i64 = 60;

pub struct Fleet {
    db: sqlx::PgPool,
    key: Vec<u8>,
    relay: Option<String>,
}

impl Fleet {
    pub fn new(db: sqlx::PgPool, secret_key: &str, relay: Option<String>) -> Fleet {
        Fleet {
            db,
            key: crate::crypto::derive(secret_key, "fleet-session-password"),
            relay,
        }
    }

    /// The Mac's session password for `epoch`.
    pub fn password(&self, agent: Uuid, epoch: i32) -> String {
        password(&self.key, agent, epoch)
    }
}

pub fn password(key: &[u8], agent: Uuid, epoch: i32) -> String {
    use base64::Engine;
    let m = crate::crypto::mac(key, format!("{agent}:{epoch}").as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&m[..18])
}

/// "Apple M2 Pro" as Taro shows chips: "M2 Pro".
pub fn chip_name(brand: &str) -> String {
    let b = brand.trim();
    b.strip_prefix("Apple ").unwrap_or(b).to_string()
}

#[derive(sqlx::FromRow)]
struct Agent {
    id: Uuid,
    display_name: String,
    model: String,
    chip: String,
    cpu_cores: i32,
    memory_gb: i32,
    storage_gb: i32,
    macos: String,
    version: String,
    taro_id: Option<String>,
    epoch: i32,
    applied_epoch: i32,
    online: bool,
    /// a live rental has it
    in_use: bool,
}

const AGENT: &str = "SELECT a.id, a.display_name, a.model, a.chip, a.cpu_cores, a.memory_gb, a.storage_gb, a.macos, a.version, a.taro_id, a.epoch, a.applied_epoch,
    coalesce(a.last_seen_at > now() - make_interval(secs => 60), false) AS online,
    EXISTS (SELECT 1 FROM mac_resources m JOIN rentals r ON r.resource_id = m.id
            WHERE m.external_id = a.id::text AND r.status IN ('provisioning', 'ready', 'suspended', 'cancel_pending')) AS in_use
    FROM mac_agents a WHERE a.removed_at IS NULL";

fn db_error(e: sqlx::Error) -> Error {
    Error::retryable(format!("database: {e}"))
}

fn parse(external_id: &str) -> Result<Uuid> {
    external_id.parse().map_err(|_| Error {
        kind: ErrorKind::NotFound,
        message: format!("{external_id} is not an enrolled Mac"),
    })
}

impl Agent {
    fn resource(&self) -> Resource {
        Resource {
            external_id: self.id.to_string(),
            display_name: self.display_name.clone(),
            model: self.model.clone(),
            chip: chip_name(&self.chip),
            cpu_cores: self.cpu_cores,
            memory_gb: self.memory_gb,
            storage_gb: self.storage_gb,
            region: "default".into(),
            state: match (self.online, self.in_use) {
                (false, _) => ResourceState::Offline,
                (true, true) => ResourceState::InUse,
                (true, false) => ResourceState::Free,
            },
        }
    }
}

impl Fleet {
    async fn agent(&self, external_id: &str) -> Result<Agent> {
        let id = parse(external_id)?;
        sqlx::query_as::<_, Agent>(&format!("{AGENT} AND a.id = $1"))
            .bind(id)
            .fetch_optional(&self.db)
            .await
            .map_err(db_error)?
            .ok_or_else(|| Error {
                kind: ErrorKind::NotFound,
                message: format!("{external_id} is no longer enrolled"),
            })
    }

    /// A new session password: the sessions made with the old one end.
    pub async fn new_epoch(db: &sqlx::PgPool, agent: Uuid) -> sqlx::Result<i32> {
        let (e,): (i32,) =
            sqlx::query_as("UPDATE mac_agents SET epoch = epoch + 1 WHERE id = $1 RETURNING epoch")
                .bind(agent)
                .fetch_one(db)
                .await?;
        Ok(e)
    }
}

#[async_trait]
impl Provider for Fleet {
    fn kind(&self) -> &'static str {
        "fleet"
    }
    fn is_simulated(&self) -> bool {
        false
    }
    async fn list_capabilities(&self) -> Result<Capabilities> {
        let chips: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT chip FROM mac_agents WHERE removed_at IS NULL ORDER BY chip",
        )
        .fetch_all(&self.db)
        .await
        .map_err(db_error)?;
        Ok(Capabilities {
            regions: vec!["default".into()],
            chips: chips.into_iter().map(|(c,)| chip_name(&c)).collect(),
            start_stop: false,
            idempotency_keys: true,
        })
    }
    async fn list_inventory(&self) -> Result<Vec<Resource>> {
        let all: Vec<Agent> = sqlx::query_as(AGENT)
            .fetch_all(&self.db)
            .await
            .map_err(db_error)?;
        Ok(all.iter().map(Agent::resource).collect())
    }
    async fn get_resource(&self, external_id: &str) -> Result<Resource> {
        Ok(self.agent(external_id).await?.resource())
    }
    async fn reserve_resource(&self, external_id: &str, _key: &str) -> Result<()> {
        self.agent(external_id).await.map(|_| ())
    }
    async fn provision_resource(&self, external_id: &str, _key: &str) -> Result<String> {
        let a = self.agent(external_id).await?;
        // a password of the rental's own: whoever used the Mac before cannot come back
        let e = Fleet::new_epoch(&self.db, a.id).await.map_err(db_error)?;
        Ok(format!("{}:{e}", a.id))
    }
    async fn get_provisioning_status(&self, external_ref: &str) -> Result<Provisioning> {
        let (id, epoch) = external_ref
            .split_once(':')
            .and_then(|(i, e)| Some((i, e.parse::<i32>().ok()?)))
            .ok_or_else(|| Error::permanent(format!("{external_ref} is not a fleet reference")))?;
        let a = self.agent(id).await?;
        // ready once the Mac runs the rental's password and the relay knows it
        Ok(
            if a.online && a.applied_epoch >= epoch && a.taro_id.is_some() {
                Provisioning::Ready
            } else {
                Provisioning::Pending
            },
        )
    }
    async fn release_resource(&self, external_id: &str, _key: &str) -> Result<()> {
        match self.agent(external_id).await {
            Ok(a) => Fleet::new_epoch(&self.db, a.id)
                .await
                .map(|_| ())
                .map_err(db_error),
            // removed meanwhile: nothing left to release
            Err(e) if e.kind == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
    async fn connection(&self, external_id: &str) -> Result<Connection> {
        let id = parse(external_id)?;
        // a new password takes a heartbeat to reach the Mac: wait for it (a few seconds)
        let mut a = self.agent(external_id).await?;
        for _ in 0..20 {
            if a.applied_epoch >= a.epoch && a.online {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            a = self.agent(external_id).await?;
        }
        if !a.online {
            return Err(Error::retryable("the Mac is offline"));
        }
        if a.applied_epoch < a.epoch {
            return Err(Error::retryable("the Mac is still getting ready"));
        }
        let taro_id = a
            .taro_id
            .clone()
            .ok_or_else(|| Error::retryable("the Mac has no ID at the relay yet"))?;
        Ok(Connection {
            taro_id,
            relay: self.relay.clone(),
            password: self.password(id, a.epoch),
        })
    }
    async fn health_check(&self) -> Result<Health> {
        Ok(Health::Healthy)
    }
    async fn safe_diagnostics(&self, external_id: &str) -> Result<serde_json::Value> {
        let a = self.agent(external_id).await?;
        Ok(serde_json::json!({
            "online": a.online,
            "macos": a.macos,
            "agent_version": a.version,
            "taro_id": a.taro_id,
            "session_epoch": a.epoch,
            "applied_epoch": a.applied_epoch,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_change_with_the_epoch_and_the_mac() {
        let k = crate::crypto::derive("server secret", "fleet-session-password");
        let a = Uuid::new_v4();
        let p1 = password(&k, a, 1);
        assert_eq!(p1, password(&k, a, 1));
        assert_eq!(p1.len(), 24);
        assert_ne!(p1, password(&k, a, 2));
        assert_ne!(p1, password(&k, Uuid::new_v4(), 1));
        assert_eq!(chip_name("Apple M2 Pro"), "M2 Pro");
        assert_eq!(chip_name("Intel(R) Core(TM) i7"), "Intel(R) Core(TM) i7");
    }
}
