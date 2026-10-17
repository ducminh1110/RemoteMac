//! Taro Enterprise's API: a modular monolith (ADR-001). Each module owns its tables and routes;
//! they meet only through the functions they export.

// sqlx rows read as tuples are clearer here than a struct per query
#![allow(clippy::type_complexity)]

pub mod audit;
pub mod authz;
pub mod billing;
pub mod branding;
pub mod config;
pub mod connectivity;
pub mod crypto;
pub mod db;
pub mod devices;
pub mod domains;
pub mod error;
pub mod http;
pub mod identity;
pub mod incidents;
pub mod mail;
pub mod metrics;
pub mod notifications;
pub mod ops;
pub mod organizations;
pub mod provider;
pub mod provisioning;
pub mod ratelimit;
pub mod reconcile;
pub mod redact;
pub mod rentals;
pub mod state;

use std::sync::Arc;

use serde::Deserialize;

/// Paging for lists: newest first, before an id, at most `limit`.
#[derive(Debug, Default, Deserialize)]
pub struct Page {
    pub before: Option<i64>,
    pub limit: Option<i64>,
}

/// Everything the server needs, ready: the pool, the provider row, Stripe or its stand-in, mail.
pub async fn build_state(
    config: config::Config,
    db: sqlx::PgPool,
    provider: Arc<dyn provider::Provider>,
    billing: Option<Arc<dyn billing::stripe::Billing>>,
    mailer: Option<Arc<dyn mail::Mailer>>,
) -> anyhow::Result<state::AppState> {
    authz::sync_catalog(&db).await?;
    let (provider_id,): (uuid::Uuid,) = sqlx::query_as("INSERT INTO provider_integrations (id, kind, name) VALUES ($1, $2, $3) ON CONFLICT (name) DO UPDATE SET kind = EXCLUDED.kind RETURNING id")
        .bind(uuid::Uuid::new_v4())
        .bind(provider.kind())
        .bind(format!("{} (built in)", provider.kind()))
        .fetch_one(&db)
        .await?;
    let http = reqwest::Client::builder()
        .user_agent(concat!("taro-enterprise/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mailer: Arc<dyn mail::Mailer> = match (mailer, &config.smtp_url) {
        (Some(m), _) => m,
        (None, Some(url)) => Arc::new(mail::Smtp::new(url.expose(), &config.mail_from)?),
        (None, None) => Arc::new(mail::DevOutbox { db: db.clone() }),
    };
    let billing = billing.or_else(|| match (&config.stripe, config.env) {
        (Some(s), _) => Some(Arc::new(billing::stripe::Stripe {
            http: http.clone(),
            key: s.secret_key.clone(),
            base: s.api_base.clone(),
            live: s.live,
        }) as Arc<dyn billing::stripe::Billing>),
        // development without Stripe: simulated checkout, labelled as such
        (None, config::Env::Development) => Some(Arc::new(billing::stripe::Simulated {
            public_url: config.public_url.origin().ascii_serialization(),
        }) as Arc<dyn billing::stripe::Billing>),
        (None, _) => None,
    });
    Ok(state::AppState(Arc::new(state::Inner {
        config,
        db,
        provider,
        provider_id,
        billing,
        mailer,
        metrics: metrics::Metrics::default(),
        limiter: ratelimit::Limiter::default(),
        http,
        jwks: tokio::sync::RwLock::new(None),
    })))
}

/// The background workers: provisioning, the Stripe inbox, reconciliation, domain checks.
pub fn spawn_workers(state: &state::AppState) {
    tokio::spawn(provisioning::run(state.clone()));
    tokio::spawn(billing::run(state.clone()));
    tokio::spawn(reconcile::run(state.clone()));
    tokio::spawn(domains::run(state.clone()));
}

/// Example plans for development and demos (`taro-enterprise seed-dev`).
pub async fn seed_dev(db: &sqlx::PgPool) -> anyhow::Result<()> {
    let plans = [
        (
            "m4-day",
            "Mac mini M4 · Day pass",
            "24 hours on a Mac mini M4 with 24 GB of memory.",
            "M4",
            10,
            24,
            512,
            "eu-west",
            "one_time",
            Some(24),
            1900,
        ),
        (
            "m4-month",
            "Mac mini M4 · Monthly",
            "A Mac mini M4 that is yours month to month.",
            "M4",
            10,
            24,
            512,
            "eu-west",
            "monthly",
            None,
            9900,
        ),
        (
            "m4-month-us",
            "Mac mini M4 · Monthly (US East)",
            "A Mac mini M4 in US East, month to month.",
            "M4",
            10,
            24,
            512,
            "us-east",
            "monthly",
            None,
            9900,
        ),
        (
            "m4pro-month",
            "Mac mini M4 Pro · Monthly",
            "14 cores, 48 GB of memory, 1 TB.",
            "M4 Pro",
            14,
            48,
            1024,
            "us-east",
            "monthly",
            None,
            19900,
        ),
    ];
    for (slug, name, desc, chip, cores, mem, disk, region, billing, hours, price) in plans {
        sqlx::query("INSERT INTO mac_plans (id, slug, name, description, chip, cpu_cores, memory_gb, storage_gb, region, billing, duration_hours, price_minor, currency) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'usd') ON CONFLICT (slug) DO NOTHING")
            .bind(uuid::Uuid::new_v4())
            .bind(slug)
            .bind(name)
            .bind(desc)
            .bind(chip)
            .bind(cores)
            .bind(mem)
            .bind(disk)
            .bind(region)
            .bind(billing)
            .bind(hours)
            .bind(price)
            .execute(db)
            .await?;
    }
    Ok(())
}
