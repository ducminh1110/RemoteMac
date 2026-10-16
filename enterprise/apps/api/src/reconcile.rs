//! Reconciliation: every minute, what Taro believes is compared with what the provider reports
//! and what billing allows. New Macs are added, specs refreshed; a disagreement opens an
//! incident and, where it is safe, keeps a Mac out of new rentals. Nothing is released or
//! deleted on a guess: that is an operator's decision.

use serde_json::json;
use uuid::Uuid;

use crate::provider::{self, ResourceState};
use crate::rentals::Status;
use crate::state::AppState;

pub async fn run(state: AppState) {
    loop {
        if let Err(e) = once(&state).await {
            tracing::error!(error = %crate::redact::redact(&format!("{e:#}")), "reconciliation");
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

pub async fn once(state: &AppState) -> anyhow::Result<()> {
    sync_inventory(state).await?;
    provider_health(state).await?;
    stale_jobs(state).await?;
    orphaned_paid(state).await?;
    expire(state).await?;
    crate::connectivity::check_relays(state).await?;
    Ok(())
}

/// The provider's Macs into mac_resources, and the disagreements out.
pub async fn sync_inventory(state: &AppState) -> anyhow::Result<()> {
    let list = match provider::bounded("inventory", state.provider.list_inventory()).await {
        Ok(l) => l,
        Err(e) => {
            sqlx::query("UPDATE provider_integrations SET last_error = $2 WHERE id = $1")
                .bind(state.provider_id)
                .bind(&e.message)
                .execute(&state.db)
                .await?;
            return Ok(());
        }
    };
    let mut seen = vec![];
    for r in &list {
        seen.push(r.external_id.clone());
        let pstate = r.state.key();
        // a new Mac: available when the provider has it free
        let initial = if r.state == ResourceState::Free {
            "available"
        } else {
            "unavailable"
        };
        let row: (Uuid, String, Option<Uuid>) = sqlx::query_as(
            "INSERT INTO mac_resources (id, provider_id, external_id, display_name, model, chip, cpu_cores, memory_gb, storage_gb, region, state, provider_state, last_synced_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, now())
             ON CONFLICT (provider_id, external_id) DO UPDATE SET display_name = EXCLUDED.display_name, model = EXCLUDED.model, chip = EXCLUDED.chip, cpu_cores = EXCLUDED.cpu_cores,
                memory_gb = EXCLUDED.memory_gb, storage_gb = EXCLUDED.storage_gb, region = EXCLUDED.region, provider_state = EXCLUDED.provider_state, last_synced_at = now()
             RETURNING id, state, (SELECT id FROM rentals WHERE resource_id = mac_resources.id AND status IN ('provisioning', 'ready', 'suspended', 'cancel_pending') LIMIT 1)",
        )
        .bind(Uuid::new_v4())
        .bind(state.provider_id)
        .bind(&r.external_id)
        .bind(&r.display_name)
        .bind(&r.model)
        .bind(&r.chip)
        .bind(r.cpu_cores)
        .bind(r.memory_gb)
        .bind(r.storage_gb)
        .bind(&r.region)
        .bind(initial)
        .bind(pstate)
        .fetch_one(&state.db)
        .await?;
        let (id, ours, live_rental) = row;
        match (ours.as_str(), r.state, live_rental) {
            // a live rental's Mac the provider calls free or offline
            ("assigned", ResourceState::Free | ResourceState::Offline, Some(rental)) => {
                crate::incidents::open(
                    state,
                    crate::incidents::New {
                        severity: "sev2",
                        component: "reconciliation",
                        title: format!("{} is {} at the provider but in use by a rental", r.display_name, pstate),
                        impact: "The customer's Mac may not work.".into(),
                        dedupe_key: Some(format!("drift:{id}")),
                        linked: json!({ "resource_id": id, "rental_id": rental, "provider_state": pstate }),
                    },
                )
                .await?;
            }
            // a Mac Taro would hand out that the provider has busy or down: held from new rentals
            ("available", s, _) if s != ResourceState::Free => {
                sqlx::query("UPDATE mac_resources SET state = 'unavailable', updated_at = now() WHERE id = $1").bind(id).execute(&state.db).await?;
                crate::incidents::open(
                    state,
                    crate::incidents::New {
                        severity: "sev4",
                        component: "reconciliation",
                        title: format!(
                            "{} is {} at the provider; kept out of new rentals",
                            r.display_name, pstate
                        ),
                        impact: String::new(),
                        dedupe_key: Some(format!("drift:{id}")),
                        linked: json!({ "resource_id": id }),
                    },
                )
                .await?;
            }
            // free again at the provider, with no rental: back in service
            ("unavailable" | "discovered", ResourceState::Free, None) => {
                sqlx::query("UPDATE mac_resources SET state = 'available', updated_at = now() WHERE id = $1 AND hold_reason IS NULL").bind(id).execute(&state.db).await?;
            }
            _ => {}
        }
    }
    // Macs the provider no longer lists
    let gone: Vec<(Uuid, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT m.id, m.display_name, (SELECT id FROM rentals WHERE resource_id = m.id AND status IN ('provisioning', 'ready', 'suspended', 'cancel_pending') LIMIT 1)
         FROM mac_resources m WHERE m.provider_id = $1 AND NOT (m.external_id = ANY($2)) AND m.state NOT IN ('released', 'unavailable')",
    )
    .bind(state.provider_id)
    .bind(&seen)
    .fetch_all(&state.db)
    .await?;
    for (id, name, rental) in gone {
        sqlx::query("UPDATE mac_resources SET state = 'unavailable', provider_state = 'missing', updated_at = now() WHERE id = $1").bind(id).execute(&state.db).await?;
        crate::incidents::open(
            state,
            crate::incidents::New {
                severity: if rental.is_some() { "sev2" } else { "sev4" },
                component: "reconciliation",
                title: format!("{name} is no longer listed by the provider"),
                impact: if rental.is_some() {
                    "A rental's Mac is missing at the provider.".into()
                } else {
                    String::new()
                },
                dedupe_key: Some(format!("missing:{id}")),
                linked: json!({ "resource_id": id, "rental_id": rental }),
            },
        )
        .await?;
    }
    sqlx::query(
        "UPDATE provider_integrations SET last_sync_at = now(), last_error = NULL WHERE id = $1",
    )
    .bind(state.provider_id)
    .execute(&state.db)
    .await?;
    Ok(())
}

async fn provider_health(state: &AppState) -> anyhow::Result<()> {
    let h = provider::bounded("health", state.provider.health_check()).await;
    let (health, err) = match &h {
        Ok(provider::Health::Healthy) => ("healthy", None),
        Ok(provider::Health::Degraded) => ("degraded", None),
        Ok(provider::Health::Down) => ("down", None),
        Err(e) => ("down", Some(e.message.clone())),
    };
    sqlx::query("UPDATE provider_integrations SET health = $2, last_health_at = now(), last_error = coalesce($3, last_error) WHERE id = $1").bind(state.provider_id).bind(health).bind(&err).execute(&state.db).await?;
    if health == "down" {
        crate::incidents::open(
            state,
            crate::incidents::New {
                severity: "sev2",
                component: "provider",
                title: format!("Provider {} is down", state.provider.kind()),
                impact: "New rentals cannot be provisioned.".into(),
                dedupe_key: Some(format!("provider:{}", state.provider_id)),
                linked: json!({ "provider_id": state.provider_id }),
            },
        )
        .await?;
    }
    Ok(())
}

/// A worker that died mid-job leaves it locked: after its lock runs out it is retried.
async fn stale_jobs(state: &AppState) -> anyhow::Result<()> {
    let n = sqlx::query("UPDATE provisioning_jobs SET state = 'retryable_failed', last_error = 'the worker stopped during this step', locked_until = NULL WHERE state = 'running' AND locked_until < now()").execute(&state.db).await?;
    if n.rows_affected() > 0 {
        tracing::warn!(jobs = n.rows_affected(), "stale provisioning jobs requeued");
    }
    Ok(())
}

/// A paid rental with no provisioning job (the job is queued with the payment; this is a guard).
async fn orphaned_paid(state: &AppState) -> anyhow::Result<()> {
    let ids: Vec<(Uuid,)> = sqlx::query_as("SELECT r.id FROM rentals r WHERE r.status = 'paid' AND NOT EXISTS (SELECT 1 FROM provisioning_jobs j WHERE j.rental_id = r.id AND j.operation = 'provision') AND r.updated_at < now() - interval '1 minute'").fetch_all(&state.db).await?;
    for (id,) in ids {
        let mut tx = state.db.begin().await?;
        crate::provisioning::enqueue(&mut tx, id, "provision", state.provider_id).await?;
        tx.commit().await?;
        tracing::warn!(rental = %id, "a paid rental had no provisioning job; queued");
    }
    Ok(())
}

/// One-time rentals past their end: expired, and their Mac released. (Monthly rentals end when
/// Stripe ends their subscription.)
pub async fn expire(state: &AppState) -> anyhow::Result<()> {
    let ids: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM rentals WHERE billing = 'one_time' AND status IN ('ready', 'suspended') AND ends_at < now()").fetch_all(&state.db).await?;
    for (id,) in ids {
        let mut tx = state.db.begin().await?;
        crate::rentals::transition(
            &mut tx,
            id,
            Status::Expired,
            "the rental period ended",
            "system:reconcile",
        )
        .await?;
        crate::provisioning::enqueue(&mut tx, id, "release", state.provider_id).await?;
        tx.commit().await?;
    }
    Ok(())
}
