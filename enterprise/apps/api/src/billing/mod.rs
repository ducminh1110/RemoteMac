//! Billing: checkout, the webhook inbox, subscriptions and invoices.
//!
//! Stripe is the source of payment events; Taro decides what a rental does with them. A
//! browser coming back from Checkout proves nothing: only a verified webhook marks a rental
//! paid. Webhooks are verified on the raw body, stored once by event id, answered at once, and
//! processed by a worker (retries with backoff, dead after 8 tries with an incident). Events
//! may come late, twice or out of order: each handler checks where the rental is before moving
//! it, and subscription rows keep the newest event time they reflect.

pub mod stripe;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit::{self, Actor, Event};
use crate::authz::Permission;
use crate::error::{ApiError, ApiResult};
use crate::identity::Auth;
use crate::rentals::{self, Status};
use crate::state::AppState;

const MAX_ATTEMPTS: i32 = 8;

fn billing(state: &AppState) -> ApiResult<&std::sync::Arc<dyn stripe::Billing>> {
    state.billing.as_ref().ok_or_else(|| {
        ApiError::Unavailable("Billing is not configured on this deployment.".into())
    })
}

/// The organization's Stripe customer, made on first use.
async fn customer(state: &AppState, org: Uuid, email: &str) -> ApiResult<String> {
    if let Some((c,)) = sqlx::query_as::<_, (String,)>(
        "SELECT stripe_customer_id FROM billing_accounts WHERE organization_id = $1",
    )
    .bind(org)
    .fetch_optional(&state.db)
    .await?
    {
        return Ok(c);
    }
    let (name,): (String,) = sqlx::query_as("SELECT name FROM organizations WHERE id = $1")
        .bind(org)
        .fetch_one(&state.db)
        .await?;
    let b = billing(state)?;
    let id = b
        .create_customer(org, &name, email)
        .await
        .map_err(|e| ApiError::Unavailable(format!("Stripe: {e}")))?;
    sqlx::query("INSERT INTO billing_accounts (organization_id, stripe_customer_id, livemode) VALUES ($1, $2, $3) ON CONFLICT (organization_id) DO NOTHING").bind(org).bind(&id).bind(b.livemode()).execute(&state.db).await?;
    let (c,): (String,) = sqlx::query_as(
        "SELECT stripe_customer_id FROM billing_accounts WHERE organization_id = $1",
    )
    .bind(org)
    .fetch_one(&state.db)
    .await?;
    Ok(c)
}

async fn checkout(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let r = rentals::load(&state.db, id).await?;
    auth.require(&state, r.organization_id, Permission::RentalsCreate)
        .await?;
    if r.status != "pending_payment" {
        return Err(ApiError::conflict(format!(
            "This rental is {}: there is nothing to pay.",
            r.status
        )));
    }
    if !crate::ops::flag(&state, "billing.checkout").await {
        return Err(ApiError::Unavailable(
            "Checkout is paused for a moment. Try again later.".into(),
        ));
    }
    let b = billing(&state)?;
    // an open session is reused; an expired one is replaced
    let (existing,): (Option<String>,) =
        sqlx::query_as("SELECT stripe_checkout_session_id FROM rentals WHERE id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    if let Some(s) = existing.filter(|_| r.billing_state == "checkout_open") {
        if let Ok(cs) = b.get_checkout(&s).await {
            if cs.status.as_deref() == Some("open") {
                if let Some(url) = cs.url {
                    return Ok(Json(json!({ "url": url, "simulated": b.simulated() })));
                }
            }
        }
    }
    let p = rentals::plan(&state.db, r.plan_id).await?;
    let cust = customer(&state, r.organization_id, &auth.email).await?;
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM rental_events WHERE rental_id = $1 AND reason = 'checkout started'",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    let origin = state.config.public_url.origin().ascii_serialization();
    let req = stripe::CheckoutRequest {
        customer: cust,
        mode: if r.billing == "monthly" {
            "subscription"
        } else {
            "payment"
        },
        product_name: p.name.clone(),
        // the price is the server's, from the catalog when the rental was made
        amount_minor: r.amount_minor,
        currency: r.currency.clone(),
        rental_id: id,
        organization_id: r.organization_id,
        success_url: format!("{origin}/rentals/{id}?checkout=done"),
        cancel_url: format!("{origin}/rentals/{id}?checkout=cancelled"),
        idempotency_key: format!("checkout-{id}-{n}"),
    };
    let cs = b
        .create_checkout(&req)
        .await
        .map_err(|e| ApiError::Unavailable(format!("Stripe: {e}")))?;
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE rentals SET stripe_checkout_session_id = $2, billing_state = 'checkout_open', updated_at = now() WHERE id = $1").bind(id).bind(&cs.id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO rental_events (rental_id, from_status, to_status, reason, actor) VALUES ($1, 'pending_payment', 'pending_payment', 'checkout started', $2)").bind(id).bind(format!("user:{}", auth.user_id)).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "billing.checkout_started", "rental")
            .org(r.organization_id)
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "simulated": b.simulated() })),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({ "url": cs.url, "simulated": b.simulated() })))
}

// ------------------------------------------------------------------ webhook inbox

/// `POST /webhooks/stripe`: verify, store once, answer. Processing happens in the worker.
async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let Some(cfg) = state.config.stripe.as_ref() else {
        return (StatusCode::NOT_FOUND, "billing is not configured").into_response();
    };
    let sig = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Err(why) = stripe::verify_signature(
        sig,
        &body,
        cfg.webhook_secret.expose(),
        Utc::now().timestamp(),
        300,
    ) {
        state
            .metrics
            .inc("taro_stripe_webhooks_total", &[("result", "bad_signature")]);
        tracing::warn!(why, "Stripe webhook refused");
        return (StatusCode::BAD_REQUEST, "invalid signature").into_response();
    }
    #[derive(Deserialize)]
    struct Envelope {
        id: String,
        #[serde(rename = "type")]
        kind: String,
        livemode: bool,
        created: i64,
    }
    let Ok(env) = serde_json::from_slice::<Envelope>(&body) else {
        return (StatusCode::BAD_REQUEST, "not an event").into_response();
    };
    if env.livemode != cfg.live {
        // a live event at a test deployment (or the reverse): never acted on
        state
            .metrics
            .inc("taro_stripe_webhooks_total", &[("result", "wrong_mode")]);
        return (StatusCode::BAD_REQUEST, "wrong mode").into_response();
    }
    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match store_event(
        &state,
        &env.id,
        &env.kind,
        env.livemode,
        env.created,
        payload,
    )
    .await
    {
        Ok(new) => {
            state.metrics.inc(
                "taro_stripe_webhooks_total",
                &[("result", if new { "stored" } else { "duplicate" })],
            );
            (StatusCode::OK, "ok").into_response()
        }
        // Stripe retries on a 5xx: nothing is lost
        Err(e) => {
            tracing::error!(error = %e, "webhook not stored");
            (StatusCode::SERVICE_UNAVAILABLE, "retry").into_response()
        }
    }
}

/// Into the inbox once (true), or already there (false).
pub async fn store_event(
    state: &AppState,
    id: &str,
    kind: &str,
    livemode: bool,
    created: i64,
    payload: Value,
) -> anyhow::Result<bool> {
    let r = sqlx::query("INSERT INTO stripe_webhook_events (id, type, livemode, created, payload) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO NOTHING").bind(id).bind(kind).bind(livemode).bind(created).bind(payload).execute(&state.db).await?;
    Ok(r.rows_affected() == 1)
}

/// The inbox worker.
pub async fn run(state: AppState) {
    loop {
        match process_once(&state).await {
            Ok(true) => continue,
            Ok(false) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
            Err(e) => {
                tracing::error!(error = %crate::redact::redact(&format!("{e:#}")), "webhook worker");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

/// Process one due event. True when one was processed (or failed).
pub async fn process_once(state: &AppState) -> anyhow::Result<bool> {
    let mut tx = state.db.begin().await?;
    let ev: Option<(String, String, i64, Value, i32)> = sqlx::query_as("SELECT id, type, created, payload, attempts FROM stripe_webhook_events WHERE state IN ('pending', 'failed') AND next_attempt_at <= now() ORDER BY created LIMIT 1 FOR UPDATE SKIP LOCKED").fetch_optional(&mut *tx).await?;
    let Some((id, kind, created, payload, attempts)) = ev else {
        return Ok(false);
    };
    let obj = payload
        .pointer("/data/object")
        .cloned()
        .unwrap_or(Value::Null);
    let result = handle(state, &mut tx, &kind, created, &obj).await;
    match result {
        Ok(Handled::Done) => {
            sqlx::query("UPDATE stripe_webhook_events SET state = 'processed', processed_at = now(), attempts = attempts + 1, last_error = NULL WHERE id = $1").bind(&id).execute(&mut *tx).await?;
            tx.commit().await?;
            state.metrics.inc(
                "taro_stripe_events_total",
                &[("type", &kind), ("result", "processed")],
            );
        }
        Ok(Handled::Ignored) => {
            sqlx::query("UPDATE stripe_webhook_events SET state = 'ignored', processed_at = now(), attempts = attempts + 1 WHERE id = $1").bind(&id).execute(&mut *tx).await?;
            tx.commit().await?;
        }
        Err(e) => {
            // the handler's changes go; the attempt is counted on its own
            tx.rollback().await?;
            let msg = crate::redact::redact(&format!("{e:#}"));
            let attempts = attempts + 1;
            let dead = attempts >= MAX_ATTEMPTS;
            let wait = crate::provisioning::backoff(attempts).as_secs_f64();
            sqlx::query("UPDATE stripe_webhook_events SET state = $2, attempts = $3, last_error = $4, next_attempt_at = now() + make_interval(secs => $5) WHERE id = $1").bind(&id).bind(if dead { "dead" } else { "failed" }).bind(attempts).bind(&msg).bind(wait).execute(&state.db).await?;
            state.metrics.inc(
                "taro_stripe_events_total",
                &[
                    ("type", &kind),
                    ("result", if dead { "dead" } else { "retry" }),
                ],
            );
            if dead {
                crate::incidents::open(
                    state,
                    crate::incidents::New {
                        severity: "sev3",
                        component: "billing",
                        title: format!("Stripe event {kind} could not be processed"),
                        impact: msg.clone(),
                        dedupe_key: Some(format!("stripe-event:{id}")),
                        linked: json!({ "event_id": id }),
                    },
                )
                .await?;
            }
        }
    }
    Ok(true)
}

enum Handled {
    Done,
    Ignored,
}

fn s<'a>(obj: &'a Value, p: &str) -> Option<&'a str> {
    obj.pointer(p).and_then(|v| v.as_str())
}

fn ts(obj: &Value, p: &str) -> Option<DateTime<Utc>> {
    obj.pointer(p)
        .and_then(|v| v.as_i64())
        .and_then(|t| Utc.timestamp_opt(t, 0).single())
}

async fn rental_of(
    tx: &mut sqlx::PgConnection,
    obj: &Value,
) -> anyhow::Result<Option<(Uuid, Uuid, String, i64, String)>> {
    let id = s(obj, "/metadata/rental_id")
        .or(s(obj, "/client_reference_id"))
        .and_then(|r| r.parse::<Uuid>().ok());
    let Some(id) = id else { return Ok(None) };
    Ok(sqlx::query_as(
        "SELECT id, organization_id, status, amount_minor, currency FROM rentals WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?)
}

async fn rental_of_subscription(
    tx: &mut sqlx::PgConnection,
    sub: &str,
) -> anyhow::Result<Option<(Uuid, Uuid, String)>> {
    Ok(sqlx::query_as(
        "SELECT id, organization_id, status FROM rentals WHERE stripe_subscription_id = $1",
    )
    .bind(sub)
    .fetch_optional(&mut *tx)
    .await?)
}

async fn handle(
    state: &AppState,
    tx: &mut sqlx::PgConnection,
    kind: &str,
    created: i64,
    obj: &Value,
) -> anyhow::Result<Handled> {
    let stripe = "system:stripe";
    match kind {
        "checkout.session.completed" | "checkout.session.async_payment_succeeded" => {
            let paid = matches!(
                s(obj, "/payment_status"),
                Some("paid" | "no_payment_required")
            );
            if !paid {
                // a delayed method: async_payment_succeeded comes when it clears
                return Ok(Handled::Ignored);
            }
            let Some((rental, org, status, amount, currency)) = rental_of(tx, obj).await? else {
                return Ok(Handled::Ignored);
            };
            let session = s(obj, "/id").unwrap_or_default();
            let (ours,): (Option<String>,) =
                sqlx::query_as("SELECT stripe_checkout_session_id FROM rentals WHERE id = $1")
                    .bind(rental)
                    .fetch_one(&mut *tx)
                    .await?;
            let total = obj.pointer("/amount_total").and_then(|v| v.as_i64());
            let cur = s(obj, "/currency");
            if ours.as_deref() != Some(session)
                || total != Some(amount)
                || cur != Some(currency.as_str())
            {
                // not the session Taro made for this rental, or not its price: nothing is provisioned
                crate::incidents::open(
                    state,
                    crate::incidents::New {
                        severity: "sev3",
                        component: "billing",
                        title: format!("A checkout for rental {rental} does not match it"),
                        impact: "Paid, but not provisioned until reviewed.".into(),
                        dedupe_key: Some(format!("checkout-mismatch:{rental}")),
                        linked: json!({ "rental_id": rental, "session": session }),
                    },
                )
                .await?;
                return Ok(Handled::Done);
            }
            match Status::parse(&status) {
                Some(Status::PendingPayment) => {
                    sqlx::query("UPDATE rentals SET billing_state = 'paid', stripe_subscription_id = $2, stripe_payment_intent_id = $3, updated_at = now() WHERE id = $1").bind(rental).bind(s(obj, "/subscription")).bind(s(obj, "/payment_intent")).execute(&mut *tx).await?;
                    rentals::transition(
                        tx,
                        rental,
                        Status::Paid,
                        "payment confirmed by Stripe",
                        stripe,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                    crate::provisioning::enqueue(tx, rental, "provision", state.provider_id)
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    audit::record(
                        &mut *tx,
                        Event::new(Actor::Stripe, "billing.paid", "rental")
                            .org(org)
                            .resource(rental)
                            .details(json!({ "amount_minor": amount, "currency": currency })),
                    )
                    .await?;
                }
                Some(Status::Cancelled) => {
                    crate::incidents::open(
                        state,
                        crate::incidents::New {
                            severity: "sev3",
                            component: "billing",
                            title: format!("Payment received for cancelled rental {rental}"),
                            impact: "Refund review needed (no automatic refund).".into(),
                            dedupe_key: Some(format!("paid-cancelled:{rental}")),
                            linked: json!({ "rental_id": rental }),
                        },
                    )
                    .await?;
                }
                // already paid (a repeat): nothing more
                _ => {}
            }
            Ok(Handled::Done)
        }
        "checkout.session.expired" => {
            let Some((rental, ..)) = rental_of(tx, obj).await? else {
                return Ok(Handled::Ignored);
            };
            sqlx::query("UPDATE rentals SET billing_state = 'expired', updated_at = now() WHERE id = $1 AND stripe_checkout_session_id = $2 AND billing_state = 'checkout_open'").bind(rental).bind(s(obj, "/id")).execute(&mut *tx).await?;
            Ok(Handled::Done)
        }
        "customer.subscription.updated"
        | "customer.subscription.created"
        | "customer.subscription.deleted" => {
            let sub = s(obj, "/id").unwrap_or_default().to_string();
            let status = s(obj, "/status").unwrap_or("unknown").to_string();
            let org: Option<Uuid> =
                s(obj, "/metadata/organization_id").and_then(|o| o.parse().ok());
            let rental: Option<Uuid> = s(obj, "/metadata/rental_id").and_then(|o| o.parse().ok());
            let Some(org) = org else {
                return Ok(Handled::Ignored);
            };
            // newer events win; an older one arriving late changes nothing
            sqlx::query(
                "INSERT INTO subscriptions (id, organization_id, rental_id, status, current_period_end, cancel_at_period_end, stripe_updated) VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status, current_period_end = EXCLUDED.current_period_end, cancel_at_period_end = EXCLUDED.cancel_at_period_end, stripe_updated = EXCLUDED.stripe_updated, updated_at = now()
                 WHERE subscriptions.stripe_updated <= EXCLUDED.stripe_updated",
            )
            .bind(&sub)
            .bind(org)
            .bind(rental)
            .bind(&status)
            .bind(ts(obj, "/current_period_end"))
            .bind(obj.pointer("/cancel_at_period_end").and_then(|v| v.as_bool()).unwrap_or(false))
            .bind(created)
            .execute(&mut *tx)
            .await?;
            if let Some((rental, org, rstatus)) = rental_of_subscription(tx, &sub).await? {
                match (kind, status.as_str()) {
                    ("customer.subscription.deleted", _) => {
                        match Status::parse(&rstatus) {
                            Some(Status::Ready | Status::Suspended) => {
                                sqlx::query(
                                    "UPDATE rentals SET billing_state = 'canceled' WHERE id = $1",
                                )
                                .bind(rental)
                                .execute(&mut *tx)
                                .await?;
                                rentals::transition(
                                    tx,
                                    rental,
                                    Status::CancelPending,
                                    "the subscription ended",
                                    stripe,
                                )
                                .await
                                .map_err(|e| anyhow::anyhow!("{e}"))?;
                                crate::provisioning::enqueue(
                                    tx,
                                    rental,
                                    "release",
                                    state.provider_id,
                                )
                                .await
                                .map_err(|e| anyhow::anyhow!("{e}"))?;
                                audit::record(
                                    &mut *tx,
                                    Event::new(
                                        Actor::Stripe,
                                        "billing.subscription_ended",
                                        "rental",
                                    )
                                    .org(org)
                                    .resource(rental),
                                )
                                .await?;
                            }
                            // not provisioned yet: wait for it (retried), never release a Mac mid-way
                            Some(Status::Paid | Status::Provisioning) => {
                                anyhow::bail!("the rental is still being provisioned")
                            }
                            _ => {
                                sqlx::query(
                                    "UPDATE rentals SET billing_state = 'canceled' WHERE id = $1",
                                )
                                .bind(rental)
                                .execute(&mut *tx)
                                .await?;
                            }
                        }
                    }
                    (_, "past_due" | "unpaid") => {
                        sqlx::query("UPDATE rentals SET billing_state = 'past_due' WHERE id = $1")
                            .bind(rental)
                            .execute(&mut *tx)
                            .await?;
                    }
                    (_, "active" | "trialing") => {
                        sqlx::query("UPDATE rentals SET billing_state = 'paid', ends_at = greatest(coalesce(ends_at, $2), $2) WHERE id = $1 AND billing_state IN ('paid', 'past_due')").bind(rental).bind(ts(obj, "/current_period_end")).execute(&mut *tx).await?;
                    }
                    _ => {}
                }
            } else if kind == "customer.subscription.deleted" {
                // the rental's link to the subscription is written with the checkout event: wait for it
                let known: Option<(String,)> =
                    sqlx::query_as("SELECT status FROM rentals WHERE id = $1")
                        .bind(rental)
                        .fetch_optional(&mut *tx)
                        .await?;
                if known.is_some_and(|(st,)| st == "pending_payment") {
                    anyhow::bail!(
                        "the checkout event for this subscription has not been processed yet"
                    );
                }
            }
            Ok(Handled::Done)
        }
        "invoice.paid" | "invoice.payment_failed" | "invoice.finalized" | "invoice.updated" => {
            let Some(org) = s(obj, "/subscription_details/metadata/organization_id")
                .or(s(obj, "/metadata/organization_id"))
                .and_then(|o| o.parse::<Uuid>().ok())
                .or(customer_org(tx, s(obj, "/customer")).await?)
            else {
                return Ok(Handled::Ignored);
            };
            let sub = s(obj, "/subscription");
            let rental = match sub {
                Some(sub) => rental_of_subscription(tx, sub).await?.map(|r| r.0),
                None => None,
            };
            let period_end = ts(obj, "/lines/data/0/period/end").or(ts(obj, "/period_end"));
            sqlx::query(
                "INSERT INTO invoices (id, organization_id, rental_id, number, status, amount_due, amount_paid, currency, hosted_invoice_url, period_end, stripe_updated) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                 ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status, amount_due = EXCLUDED.amount_due, amount_paid = EXCLUDED.amount_paid, hosted_invoice_url = EXCLUDED.hosted_invoice_url, number = EXCLUDED.number, stripe_updated = EXCLUDED.stripe_updated
                 WHERE invoices.stripe_updated <= EXCLUDED.stripe_updated",
            )
            .bind(s(obj, "/id").unwrap_or_default())
            .bind(org)
            .bind(rental)
            .bind(s(obj, "/number"))
            .bind(s(obj, "/status").unwrap_or("unknown"))
            .bind(obj.pointer("/amount_due").and_then(|v| v.as_i64()).unwrap_or(0))
            .bind(obj.pointer("/amount_paid").and_then(|v| v.as_i64()).unwrap_or(0))
            .bind(s(obj, "/currency").unwrap_or("usd"))
            .bind(s(obj, "/hosted_invoice_url"))
            .bind(period_end)
            .bind(created)
            .execute(&mut *tx)
            .await?;
            if let Some(rental) = rental {
                if kind == "invoice.paid" {
                    // a renewal: the Mac stays until the new period's end
                    sqlx::query("UPDATE rentals SET billing_state = 'paid', ends_at = greatest(coalesce(ends_at, $2), $2) WHERE id = $1 AND billing_state IN ('paid', 'past_due', 'checkout_open')").bind(rental).bind(period_end).execute(&mut *tx).await?;
                } else if kind == "invoice.payment_failed" {
                    // no automatic suspension: the policy is a grace period decided by people
                    sqlx::query("UPDATE rentals SET billing_state = 'past_due' WHERE id = $1")
                        .bind(rental)
                        .execute(&mut *tx)
                        .await?;
                    audit::record(
                        &mut *tx,
                        Event::new(Actor::Stripe, "billing.payment_failed", "rental")
                            .org(org)
                            .resource(rental),
                    )
                    .await?;
                    crate::notifications::notify_roles(state, org, &["owner", "billing_manager"], "billing.payment_failed", "A payment failed", "Stripe could not take a renewal payment. Update the payment method in Billing to keep your Mac.").await;
                }
            }
            Ok(Handled::Done)
        }
        "charge.refunded" => {
            let pi = s(obj, "/payment_intent");
            let found: Option<(Uuid, Uuid, String)> = match pi {
                Some(pi) => sqlx::query_as("SELECT id, organization_id, status FROM rentals WHERE stripe_payment_intent_id = $1").bind(pi).fetch_optional(&mut *tx).await?,
                None => None,
            };
            let Some((rental, org, status)) = found else {
                return Ok(Handled::Ignored);
            };
            sqlx::query("UPDATE rentals SET billing_state = 'refunded' WHERE id = $1")
                .bind(rental)
                .execute(&mut *tx)
                .await?;
            audit::record(
                &mut *tx,
                Event::new(Actor::Stripe, "billing.refunded", "rental")
                    .org(org)
                    .resource(rental),
            )
            .await?;
            if matches!(
                status.as_str(),
                "ready" | "suspended" | "provisioning" | "paid"
            ) {
                // refunded while the Mac is in use: people decide (no automatic cut-off)
                crate::incidents::open(
                    state,
                    crate::incidents::New {
                        severity: "sev3",
                        component: "billing",
                        title: format!("Rental {rental} was refunded but is still {status}"),
                        impact: "Decide whether to end the rental.".into(),
                        dedupe_key: Some(format!("refund:{rental}")),
                        linked: json!({ "rental_id": rental, "organization_id": org }),
                    },
                )
                .await?;
            }
            Ok(Handled::Done)
        }
        _ => Ok(Handled::Ignored),
    }
}

async fn customer_org(
    tx: &mut sqlx::PgConnection,
    customer: Option<&str>,
) -> anyhow::Result<Option<Uuid>> {
    let Some(c) = customer else { return Ok(None) };
    Ok(sqlx::query_as::<_, (Uuid,)>(
        "SELECT organization_id FROM billing_accounts WHERE stripe_customer_id = $1",
    )
    .bind(c)
    .fetch_optional(&mut *tx)
    .await?
    .map(|r| r.0))
}

// ------------------------------------------------------------------ the customer's billing pages

async fn summary(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require(&state, org, Permission::BillingRead).await?;
    #[derive(Serialize, sqlx::FromRow)]
    struct Sub {
        id: String,
        rental_id: Option<Uuid>,
        status: String,
        current_period_end: Option<DateTime<Utc>>,
        cancel_at_period_end: bool,
    }
    let subs: Vec<Sub> = sqlx::query_as("SELECT id, rental_id, status, current_period_end, cancel_at_period_end FROM subscriptions WHERE organization_id = $1 ORDER BY current_period_end NULLS LAST").bind(org).fetch_all(&state.db).await?;
    let (account,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM billing_accounts WHERE organization_id = $1)")
            .bind(org)
            .fetch_one(&state.db)
            .await?;
    let (open_due, currency): (Option<i64>, Option<String>) = sqlx::query_as("SELECT sum(amount_due - amount_paid)::bigint, max(currency) FROM invoices WHERE organization_id = $1 AND status = 'open'").bind(org).fetch_one(&state.db).await?;
    let (past_due,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM rentals WHERE organization_id = $1 AND billing_state = 'past_due')").bind(org).fetch_one(&state.db).await?;
    let renewal = subs
        .iter()
        .filter(|s| {
            matches!(s.status.as_str(), "active" | "trialing" | "past_due")
                && !s.cancel_at_period_end
        })
        .filter_map(|s| s.current_period_end)
        .min();
    Ok(Json(json!({
        "configured": state.billing.is_some(),
        "simulated": state.billing.as_ref().is_some_and(|b| b.simulated()),
        "livemode": state.billing.as_ref().is_some_and(|b| b.livemode()),
        "has_billing_account": account,
        "subscriptions": subs,
        "open_amount_minor": open_due.unwrap_or(0),
        "open_currency": currency,
        "past_due": past_due,
        "next_renewal": renewal,
    })))
}

async fn invoices(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
    Query(q): Query<crate::Page>,
) -> ApiResult<Json<Value>> {
    auth.require(&state, org, Permission::BillingRead).await?;
    #[derive(Serialize, sqlx::FromRow)]
    struct Inv {
        id: String,
        number: Option<String>,
        status: String,
        amount_due: i64,
        amount_paid: i64,
        currency: String,
        hosted_invoice_url: Option<String>,
        period_end: Option<DateTime<Utc>>,
        created_at: DateTime<Utc>,
        rental_id: Option<Uuid>,
    }
    let rows: Vec<Inv> = sqlx::query_as("SELECT id, number, status, amount_due, amount_paid, currency, hosted_invoice_url, period_end, created_at, rental_id FROM invoices WHERE organization_id = $1 ORDER BY created_at DESC LIMIT $2").bind(org).bind(q.limit.unwrap_or(50).clamp(1, 200)).fetch_all(&state.db).await?;
    Ok(Json(json!({ "invoices": rows })))
}

async fn portal(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require(&state, org, Permission::BillingManage).await?;
    let b = billing(&state)?;
    let (cust,): (String,) = sqlx::query_as(
        "SELECT stripe_customer_id FROM billing_accounts WHERE organization_id = $1",
    )
    .bind(org)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| {
        ApiError::conflict("There is no billing account yet: it is made with the first checkout.")
    })?;
    let url = b
        .portal_session(
            &cust,
            &format!(
                "{}/billing",
                state.config.public_url.origin().ascii_serialization()
            ),
        )
        .await
        .map_err(|e| ApiError::Unavailable(format!("Stripe: {e}")))?;
    audit::record(
        &state.db,
        Event::new(auth.actor(), "billing.portal_opened", "organization")
            .org(org)
            .resource(org)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    Ok(Json(json!({ "url": url })))
}

// ------------------------------------------------------------------ development: simulated checkout

#[derive(Deserialize)]
struct SimulatedPay {
    outcome: String,
}

/// Development only: what Stripe would send after a test payment (or an abandoned one), into the
/// same inbox and through the same processing.
async fn simulated_complete(
    State(state): State<AppState>,
    auth: Auth,
    Path(session): Path<String>,
    Json(b): Json<SimulatedPay>,
) -> ApiResult<Json<Value>> {
    if state.config.env != crate::config::Env::Development
        || !state.billing.as_ref().is_some_and(|b| b.simulated())
    {
        return Err(ApiError::NotFound);
    }
    let row: Option<(Uuid, Uuid, i64, String, String, Option<String>)> = sqlx::query_as("SELECT r.id, r.organization_id, r.amount_minor, r.currency, r.billing, b.stripe_customer_id FROM rentals r LEFT JOIN billing_accounts b ON b.organization_id = r.organization_id WHERE r.stripe_checkout_session_id = $1").bind(&session).fetch_optional(&state.db).await?;
    let (rental, org, amount, currency, mode, customer) = row.ok_or(ApiError::NotFound)?;
    auth.require(&state, org, Permission::RentalsCreate).await?;
    let now = Utc::now().timestamp();
    let sub = (mode == "monthly").then(|| format!("sub_sim_{}", rental.simple()));
    let mut events = vec![];
    match b.outcome.as_str() {
        "paid" => {
            events.push(("checkout.session.completed", json!({ "id": session, "object": "checkout.session", "payment_status": "paid", "amount_total": amount, "currency": currency, "customer": customer, "subscription": sub, "payment_intent": format!("pi_sim_{}", rental.simple()), "client_reference_id": rental, "metadata": { "rental_id": rental, "organization_id": org } })));
            if let Some(sub) = &sub {
                events.push(("customer.subscription.created", json!({ "id": sub, "object": "subscription", "status": "active", "current_period_end": now + 30 * 86400, "cancel_at_period_end": false, "metadata": { "rental_id": rental, "organization_id": org } })));
                events.push(("invoice.paid", json!({ "id": format!("in_sim_{}", crate::crypto::token(6)), "object": "invoice", "status": "paid", "amount_due": amount, "amount_paid": amount, "currency": currency, "customer": customer, "subscription": sub, "number": "SIM-0001", "lines": { "data": [{ "period": { "end": now + 30 * 86400 } }] }, "metadata": { "organization_id": org } })));
            }
        }
        "expired" => events.push(("checkout.session.expired", json!({ "id": session, "object": "checkout.session", "client_reference_id": rental, "metadata": { "rental_id": rental, "organization_id": org } }))),
        _ => return Err(ApiError::invalid("outcome is paid or expired")),
    }
    for (i, (kind, obj)) in events.into_iter().enumerate() {
        let id = format!("evt_sim_{}", crate::crypto::token(9));
        store_event(&state, &id, kind, false, now + i as i64, json!({ "id": id, "type": kind, "livemode": false, "created": now + i as i64, "data": { "object": obj }, "simulated": true })).await?;
    }
    audit::record(
        &state.db,
        Event::new(auth.actor(), "billing.simulated_payment", "rental")
            .org(org)
            .resource(rental)
            .details(json!({ "outcome": b.outcome })),
    )
    .await?;
    Ok(Json(json!({ "rental_id": rental })))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/rentals/{id}/checkout", post(checkout))
        .route("/api/v1/organizations/{org}/billing/summary", get(summary))
        .route(
            "/api/v1/organizations/{org}/billing/invoices",
            get(invoices),
        )
        .route(
            "/api/v1/organizations/{org}/billing/portal-session",
            post(portal),
        )
        .route(
            "/api/v1/dev/checkout/{session}/complete",
            post(simulated_complete),
        )
}

/// Stripe's endpoint (no session, no CSRF: the signature is the authentication).
pub fn webhook_routes() -> Router<AppState> {
    Router::new().route("/webhooks/stripe", post(webhook))
}
