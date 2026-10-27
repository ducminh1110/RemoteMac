//! Provisioning jobs: a queue in Postgres (`FOR UPDATE SKIP LOCKED`), worked in the background.
//! One job per rental and operation, so a retry continues the same job with the same provider
//! idempotency key and never makes a second Mac. A rental becomes ready only when the provider
//! says the Mac is ready and Taro can get its connection details.

use std::time::Duration;

use chrono::Utc;
use rand::Rng;
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, Actor, Event, Outcome};
use crate::error::ApiResult;
use crate::provider::{self, ErrorKind, Provisioning};
use crate::rentals::{self, Status};
use crate::state::AppState;

/// How long a Mac may take to become ready before the attempt counts as failed.
const PROVISION_WAIT: chrono::Duration = chrono::Duration::minutes(30);

/// Queue `operation` for a rental (once: queuing again leaves the existing job).
pub async fn enqueue(
    tx: &mut sqlx::PgConnection,
    rental: Uuid,
    operation: &str,
    provider: Uuid,
) -> ApiResult<()> {
    sqlx::query("INSERT INTO provisioning_jobs (id, rental_id, provider_id, operation, state, correlation_id) VALUES ($1, $2, $3, $4, 'queued', $5) ON CONFLICT (rental_id, operation) DO NOTHING")
        .bind(Uuid::new_v4())
        .bind(rental)
        .bind(provider)
        .bind(operation)
        .bind(Uuid::new_v4())
        .execute(&mut *tx)
        .await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct Job {
    id: Uuid,
    rental_id: Uuid,
    operation: String,
    attempts: i32,
    max_attempts: i32,
    external_ref: Option<String>,
    correlation_id: Uuid,
    started_at: Option<chrono::DateTime<Utc>>,
}

/// Take the next due job (or none). Waiting on the provider does not count as an attempt.
async fn claim(state: &AppState) -> anyhow::Result<Option<Job>> {
    let job = sqlx::query_as::<_, Job>(
        "UPDATE provisioning_jobs SET
            state = CASE WHEN state = 'waiting_provider' THEN 'waiting_provider' ELSE 'running' END,
            attempts = CASE WHEN state = 'waiting_provider' THEN attempts ELSE attempts + 1 END,
            locked_until = now() + interval '2 minutes',
            started_at = coalesce(started_at, now())
         WHERE id = (
            SELECT id FROM provisioning_jobs
            WHERE state IN ('queued', 'retryable_failed', 'waiting_provider') AND run_after <= now() AND (locked_until IS NULL OR locked_until < now())
            ORDER BY run_after LIMIT 1 FOR UPDATE SKIP LOCKED)
         RETURNING id, rental_id, operation, attempts, max_attempts, external_ref, correlation_id, started_at",
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(job)
}

/// Exponential backoff with jitter: 5 s, 10 s, 20 s... at most 10 minutes.
pub fn backoff(attempt: i32) -> Duration {
    let base = 5u64
        .saturating_mul(1u64 << attempt.clamp(0, 10) as u32)
        .min(600);
    let jitter = rand::thread_rng().gen_range(0..=base / 4 + 1);
    Duration::from_secs(base + jitter)
}

enum Outcome2 {
    Done,
    /// poll again after this long (same attempt)
    Wait(Duration),
    Retry(String),
    Fail(String),
}

/// Run the background worker until the process ends.
pub async fn run(state: AppState) {
    loop {
        match work_once(&state).await {
            Ok(true) => continue,
            Ok(false) => tokio::time::sleep(Duration::from_millis(500)).await,
            Err(e) => {
                tracing::error!(error = %crate::redact::redact(&format!("{e:#}")), "provisioning worker");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

/// Work one job if one is due. True when one was worked.
pub async fn work_once(state: &AppState) -> anyhow::Result<bool> {
    if !crate::ops::flag(state, "provisioning.worker").await {
        return Ok(false);
    }
    let Some(job) = claim(state).await? else {
        return Ok(false);
    };
    let span = tracing::info_span!("job", job = %job.id, rental = %job.rental_id, op = %job.operation, correlation = %job.correlation_id);
    let _g = span.enter();
    let started = std::time::Instant::now();
    let result = match job.operation.as_str() {
        "provision" => provision(state, &job).await,
        "release" => release(state, &job).await,
        other => Ok(Outcome2::Fail(format!("unknown operation {other}"))),
    };
    let outcome = match result {
        Ok(o) => o,
        Err(e) => Outcome2::Retry(crate::redact::redact(&format!("{e:#}"))),
    };
    let op = job.operation.as_str();
    match outcome {
        Outcome2::Done => {
            sqlx::query("UPDATE provisioning_jobs SET state = 'succeeded', completed_at = now(), locked_until = NULL, last_error = NULL WHERE id = $1").bind(job.id).execute(&state.db).await?;
            state.metrics.inc(
                "taro_provisioning_jobs_total",
                &[("operation", op), ("result", "succeeded")],
            );
            if let Some(s) = job.started_at {
                state.metrics.observe(
                    "taro_provisioning_seconds",
                    &[("operation", op)],
                    (Utc::now() - s).num_milliseconds() as f64 / 1000.0,
                );
            }
        }
        Outcome2::Wait(d) => {
            sqlx::query("UPDATE provisioning_jobs SET state = 'waiting_provider', run_after = now() + make_interval(secs => $2), locked_until = NULL WHERE id = $1").bind(job.id).bind(d.as_secs_f64()).execute(&state.db).await?;
        }
        Outcome2::Retry(why) if job.attempts < job.max_attempts => {
            let d = backoff(job.attempts);
            tracing::warn!(attempt = job.attempts, retry_in = d.as_secs(), error = %why, "provisioning step failed; will retry");
            sqlx::query("UPDATE provisioning_jobs SET state = 'retryable_failed', run_after = now() + make_interval(secs => $2), locked_until = NULL, last_error = $3 WHERE id = $1").bind(job.id).bind(d.as_secs_f64()).bind(&why).execute(&state.db).await?;
            state.metrics.inc(
                "taro_provisioning_jobs_total",
                &[("operation", op), ("result", "retry")],
            );
        }
        Outcome2::Retry(why) | Outcome2::Fail(why) => {
            fail(state, &job, &why).await?;
        }
    }
    tracing::debug!(ms = started.elapsed().as_millis() as u64, "job step");
    Ok(true)
}

/// A job that will not succeed: it ends, its rental fails, its Mac goes back (or is held when
/// it is the Mac that failed), an incident opens.
async fn fail(state: &AppState, job: &Job, why: &str) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE provisioning_jobs SET state = 'permanent_failed', completed_at = now(), locked_until = NULL, last_error = $2 WHERE id = $1").bind(job.id).bind(why).execute(&mut *tx).await?;
    let r = rentals::load(&mut *tx, job.rental_id).await?;
    if job.operation == "provision" {
        let _ = rentals::transition(
            &mut tx,
            job.rental_id,
            Status::Failed,
            &format!("provisioning failed: {why}"),
            "system:provisioning",
        )
        .await;
        audit::record(
            &mut *tx,
            Event::new(Actor::System, "rental.provisioning_failed", "rental")
                .org(r.organization_id)
                .resource(job.rental_id)
                .result(Outcome::Failure)
                .details(json!({ "job": job.id, "error": why })),
        )
        .await?;
    }
    tx.commit().await?;
    state.metrics.inc(
        "taro_provisioning_jobs_total",
        &[("operation", &job.operation), ("result", "failed")],
    );
    crate::incidents::open(
        state,
        crate::incidents::New {
            severity: if r.billing_state == "paid" { "sev3" } else { "sev4" },
            component: "provisioning",
            title: format!("{} failed for rental {}", job.operation, job.rental_id),
            impact: if r.billing_state == "paid" { "A paid rental did not get its Mac; billing review needed (refund policy is manual).".into() } else { String::new() },
            dedupe_key: Some(format!("job:{}", job.id)),
            linked: json!({ "rental_id": job.rental_id, "job_id": job.id, "organization_id": r.organization_id }),
        },
    )
    .await?;
    if job.operation == "provision" {
        // the Mac it had, if any, goes back through a release (held if the Mac itself failed)
        let mut tx = state.db.begin().await?;
        let (resource,): (Option<Uuid>,) =
            sqlx::query_as("SELECT resource_id FROM rentals WHERE id = $1")
                .bind(job.rental_id)
                .fetch_one(&mut *tx)
                .await?;
        if let Some(m) = resource {
            sqlx::query(
                "UPDATE mac_resources SET hold_reason = $2, updated_at = now() WHERE id = $1",
            )
            .bind(m)
            .bind(format!("provisioning failed: {why}"))
            .execute(&mut *tx)
            .await?;
            enqueue(&mut tx, job.rental_id, "release", state.provider_id).await?;
        }
        tx.commit().await?;
        let (org_owner,): (Uuid,) = sqlx::query_as("SELECT created_by FROM rentals WHERE id = $1")
            .bind(job.rental_id)
            .fetch_one(&state.db)
            .await?;
        crate::notifications::notify(
            state,
            org_owner,
            Some(r.organization_id),
            "rental.failed",
            "Your Mac could not be prepared",
            "Taro Operations has been told and will contact you about it.",
        )
        .await;
    }
    Ok(())
}

async fn provision(state: &AppState, job: &Job) -> anyhow::Result<Outcome2> {
    let key = job.id.to_string();
    let r = rentals::load(&state.db, job.rental_id).await?;
    let status = Status::parse(&r.status).unwrap_or(Status::Failed);
    if status.is_final() {
        return Ok(Outcome2::Fail(format!("the rental is {}", r.status)));
    }
    // 1. a Mac for it (once)
    let resource: Option<(Uuid, String)> = sqlx::query_as("SELECT m.id, m.external_id FROM rentals r JOIN mac_resources m ON m.id = r.resource_id WHERE r.id = $1").bind(job.rental_id).fetch_optional(&state.db).await?;
    let (resource_id, external_id) = match resource {
        Some(x) => x,
        None => {
            let p = rentals::plan(&state.db, r.plan_id).await?;
            let mut tx = state.db.begin().await?;
            // the provider's own Macs (fleet) are one pool: any of them fits a plan
            let pick: Option<(Uuid, String)> = sqlx::query_as("SELECT id, external_id FROM mac_resources WHERE state = 'available' AND hold_reason IS NULL AND control_by IS NULL AND provider_id = $4 AND ($3 OR (chip = $1 AND region = $2)) ORDER BY last_synced_at DESC NULLS LAST LIMIT 1 FOR UPDATE SKIP LOCKED")
                .bind(&p.chip)
                .bind(&p.region)
                .bind(state.provider.kind() == "fleet")
                .bind(state.provider_id)
                .fetch_optional(&mut *tx)
                .await?;
            let Some((id, ext)) = pick else {
                return Ok(Outcome2::Fail(
                    "no Mac of this plan is free (paid, but out of capacity)".into(),
                ));
            };
            sqlx::query(
                "UPDATE mac_resources SET state = 'reserved', updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE rentals SET resource_id = $2, updated_at = now() WHERE id = $1")
                .bind(job.rental_id)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            if status == Status::Paid {
                rentals::transition(
                    &mut tx,
                    job.rental_id,
                    Status::Provisioning,
                    "a Mac was reserved",
                    "system:provisioning",
                )
                .await?;
            }
            tx.commit().await?;
            (id, ext)
        }
    };
    let p = state.provider.clone();
    // 2. reserved and started at the provider (same key on every retry)
    let external_ref = match &job.external_ref {
        Some(r) => r.clone(),
        None => {
            match provider::bounded("reserve", p.reserve_resource(&external_id, &key)).await {
                Ok(()) => {}
                Err(e) if e.kind == ErrorKind::NoCapacity => {
                    // the provider has it as taken: hand it back and pick another next time
                    let mut tx = state.db.begin().await?;
                    sqlx::query("UPDATE mac_resources SET state = 'unavailable', updated_at = now() WHERE id = $1").bind(resource_id).execute(&mut *tx).await?;
                    sqlx::query("UPDATE rentals SET resource_id = NULL WHERE id = $1")
                        .bind(job.rental_id)
                        .execute(&mut *tx)
                        .await?;
                    tx.commit().await?;
                    return Ok(Outcome2::Retry(format!(
                        "{external_id} was taken at the provider; another Mac will be tried"
                    )));
                }
                Err(e) if e.kind == ErrorKind::Retryable => return Ok(Outcome2::Retry(e.message)),
                Err(e) => return Ok(Outcome2::Fail(e.message)),
            }
            let r = match provider::bounded("provision", p.provision_resource(&external_id, &key))
                .await
            {
                Ok(r) => r,
                Err(e) if e.kind == ErrorKind::Retryable => return Ok(Outcome2::Retry(e.message)),
                Err(e) => return Ok(Outcome2::Fail(e.message)),
            };
            sqlx::query("UPDATE provisioning_jobs SET external_ref = $2 WHERE id = $1")
                .bind(job.id)
                .bind(&r)
                .execute(&state.db)
                .await?;
            sqlx::query("UPDATE mac_resources SET provider_state = 'provisioning', updated_at = now() WHERE id = $1").bind(resource_id).execute(&state.db).await?;
            return Ok(Outcome2::Wait(Duration::from_secs(1)));
        }
    };
    // 3. waiting for the provider
    match provider::bounded("status", p.get_provisioning_status(&external_ref)).await {
        Ok(Provisioning::Pending) => {
            if job
                .started_at
                .is_some_and(|s| Utc::now() - s > PROVISION_WAIT)
            {
                sqlx::query("UPDATE provisioning_jobs SET external_ref = NULL WHERE id = $1")
                    .bind(job.id)
                    .execute(&state.db)
                    .await?;
                return Ok(Outcome2::Retry(
                    "the provider took longer than 30 minutes".into(),
                ));
            }
            Ok(Outcome2::Wait(Duration::from_secs(2)))
        }
        Ok(Provisioning::Failed {
            retryable: true,
            message,
        }) => {
            sqlx::query("UPDATE provisioning_jobs SET external_ref = NULL WHERE id = $1")
                .bind(job.id)
                .execute(&state.db)
                .await?;
            Ok(Outcome2::Retry(message))
        }
        Ok(Provisioning::Failed {
            retryable: false,
            message,
        }) => Ok(Outcome2::Fail(message)),
        Err(e) if e.kind == ErrorKind::Retryable => Ok(Outcome2::Wait(Duration::from_secs(5))),
        Err(e) => Ok(Outcome2::Fail(e.message)),
        Ok(Provisioning::Ready) => {
            // 4. health: the provider says it is in use and Taro can get its connection details
            let res = provider::bounded("get", p.get_resource(&external_id))
                .await
                .map_err(|e| anyhow::anyhow!(e.message))?;
            if res.state != provider::ResourceState::InUse {
                return Ok(Outcome2::Wait(Duration::from_secs(2)));
            }
            let conn = match provider::bounded("connection", p.connection(&external_id)).await {
                Ok(c) => c,
                Err(e) if e.kind == ErrorKind::Retryable => {
                    return Ok(Outcome2::Wait(Duration::from_secs(3)))
                }
                Err(e) => {
                    return Ok(Outcome2::Fail(format!(
                        "the Mac is up but cannot be reached: {}",
                        e.message
                    )))
                }
            };
            let mut tx = state.db.begin().await?;
            sqlx::query("UPDATE mac_resources SET state = 'assigned', provider_state = 'in_use', taro_id = $2, last_synced_at = now(), updated_at = now() WHERE id = $1").bind(resource_id).bind(&conn.taro_id).execute(&mut *tx).await?;
            let ends = match r.duration_hours {
                Some(h) => Some(Utc::now() + chrono::Duration::hours(h as i64)),
                // monthly: the subscription's period end, set from Stripe; a month until then
                None => Some(
                    r.ends_at
                        .unwrap_or_else(|| Utc::now() + chrono::Duration::days(30)),
                ),
            };
            sqlx::query("UPDATE rentals SET starts_at = coalesce(starts_at, now()), ends_at = $2 WHERE id = $1").bind(job.rental_id).bind(ends).execute(&mut *tx).await?;
            rentals::transition(
                &mut tx,
                job.rental_id,
                Status::Ready,
                "the provider reports the Mac ready",
                "system:provisioning",
            )
            .await?;
            audit::record(
                &mut *tx,
                Event::new(Actor::System, "rental.ready", "rental")
                    .org(r.organization_id)
                    .resource(job.rental_id)
                    .details(json!({ "job": job.id })),
            )
            .await?;
            tx.commit().await?;
            if let Some(u) = r.assigned_user_id {
                crate::notifications::notify(
                    state,
                    u,
                    Some(r.organization_id),
                    "rental.ready",
                    "Your Mac is ready",
                    &format!("{} is ready. Open My Macs and choose Connect.", r.plan_name),
                )
                .await;
            }
            Ok(Outcome2::Done)
        }
    }
}

async fn release(state: &AppState, job: &Job) -> anyhow::Result<Outcome2> {
    let key = job.id.to_string();
    let r = rentals::load(&state.db, job.rental_id).await?;
    let resource: Option<(Uuid, String, Option<String>)> = sqlx::query_as("SELECT m.id, m.external_id, m.hold_reason FROM rentals r JOIN mac_resources m ON m.id = r.resource_id WHERE r.id = $1").bind(job.rental_id).fetch_optional(&state.db).await?;
    if let Some((id, ext, hold)) = resource {
        sqlx::query(
            "UPDATE mac_resources SET state = 'releasing', updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&state.db)
        .await?;
        match provider::bounded("release", state.provider.release_resource(&ext, &key)).await {
            Ok(()) => {}
            Err(e) if e.kind == ErrorKind::NotFound => {}
            Err(e) if e.kind == ErrorKind::Retryable => return Ok(Outcome2::Retry(e.message)),
            Err(e) => return Ok(Outcome2::Fail(e.message)),
        }
        // free again, unless an operator or a failure holds it
        let next = if hold.is_some() {
            "maintenance"
        } else {
            "available"
        };
        sqlx::query("UPDATE mac_resources SET state = $2, provider_state = 'free', taro_id = NULL, updated_at = now() WHERE id = $1").bind(id).bind(next).execute(&state.db).await?;
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE connection_grants SET revoked_at = now() WHERE rental_id = $1 AND revoked_at IS NULL AND redeemed_at IS NULL").bind(job.rental_id).execute(&mut *tx).await?;
    if r.status == "cancel_pending" {
        rentals::transition(
            &mut tx,
            job.rental_id,
            Status::Cancelled,
            "the Mac was released",
            "system:provisioning",
        )
        .await?;
    }
    audit::record(
        &mut *tx,
        Event::new(Actor::System, "rental.released", "rental")
            .org(r.organization_id)
            .resource(job.rental_id)
            .details(json!({ "job": job.id })),
    )
    .await?;
    tx.commit().await?;
    Ok(Outcome2::Done)
}

#[cfg(test)]
mod tests {
    #[test]
    fn backoff_grows_and_is_capped() {
        let a = super::backoff(0).as_secs();
        let b = super::backoff(3).as_secs();
        assert!((5..=7).contains(&a), "{a}");
        assert!((40..=51).contains(&b), "{b}");
        assert!(super::backoff(30).as_secs() <= 751);
    }
}
