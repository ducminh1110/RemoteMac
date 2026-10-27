//! `taro-enterprise serve | migrate | seed-dev | grant-operator <email> <role> | check-config`

use std::sync::Arc;

use taro_enterprise::{config, db, http, provider};

fn usage() -> ! {
    eprintln!("usage: taro-enterprise serve | migrate | seed-dev | grant-operator <email> <role> | check-config\n(configuration from the environment: see .env.example)");
    std::process::exit(2)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("serve");
    let cfg = match config::Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration: {e}");
            std::process::exit(2)
        }
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("TARO_LOG")
        .unwrap_or_else(|_| "info,sqlx=warn,hyper=warn".into());
    if cfg.env == config::Env::Development {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    } else {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_current_span(true)
            .init();
    }
    if cmd == "check-config" {
        println!(
            "configuration is valid: {} (Google: {}, Stripe: {}, mail: {})",
            cfg.env.as_str(),
            cfg.google.is_some(),
            cfg.stripe
                .as_ref()
                .map(|s| if s.live { "live" } else { "test" })
                .unwrap_or("none"),
            if cfg.smtp_url.is_some() {
                "smtp"
            } else {
                "dev outbox"
            }
        );
        return Ok(());
    }
    let pool = db::connect(cfg.database_url.expose()).await.map_err(|e| {
        anyhow::anyhow!(
            "database: {}",
            taro_enterprise::redact::redact(&e.to_string())
        )
    })?;
    match cmd {
        "migrate" => {
            db::migrate(&pool).await?;
            println!("migrations applied");
            Ok(())
        }
        "seed-dev" => {
            if cfg.env != config::Env::Development {
                anyhow::bail!("seed-dev adds example plans: development only");
            }
            db::migrate(&pool).await?;
            taro_enterprise::seed_dev(&pool).await?;
            println!("example plans added");
            Ok(())
        }
        "grant-operator" => {
            let (Some(email), Some(role)) = (args.get(1), args.get(2)) else {
                usage()
            };
            let role = taro_enterprise::authz::OperatorRole::parse(role).ok_or_else(|| anyhow::anyhow!("roles: platform_admin, operations_operator, support_operator, billing_operator, security_auditor"))?;
            let (user,): (uuid::Uuid,) = sqlx::query_as("SELECT id FROM users WHERE email = $1")
                .bind(email.to_lowercase())
                .fetch_optional(&pool)
                .await?
                .ok_or_else(|| anyhow::anyhow!("{email} has not signed in yet"))?;
            sqlx::query("INSERT INTO operator_grants (user_id, role) VALUES ($1, $2) ON CONFLICT DO NOTHING").bind(user).bind(role.key()).execute(&pool).await?;
            taro_enterprise::audit::record(
                &pool,
                taro_enterprise::audit::Event::new(
                    taro_enterprise::audit::Actor::System,
                    "operator.granted_cli",
                    "user",
                )
                .resource(user)
                .details(serde_json::json!({ "role": role.key() })),
            )
            .await?;
            println!("{email} is now {}", role.key());
            Ok(())
        }
        "serve" => serve(cfg, pool).await,
        _ => usage(),
    }
}

async fn serve(cfg: config::Config, pool: sqlx::PgPool) -> anyhow::Result<()> {
    if cfg.env == config::Env::Development
        || std::env::var("TARO_AUTO_MIGRATE").as_deref() == Ok("1")
    {
        db::migrate(&pool).await?;
    }
    let mock = provider::mock::Mock::new(
        std::env::var("TARO_MOCK_MACS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(8),
        std::time::Duration::from_secs(
            std::env::var("TARO_MOCK_DELAY_SECS")
                .ok()
                .and_then(|n| n.parse().ok())
                .unwrap_or(8),
        ),
    );
    let (listen, ops_listen, env) = (cfg.listen, cfg.ops_listen, cfg.env);
    let provider: Arc<dyn provider::Provider> = if cfg.provider == "fleet" {
        Arc::new(provider::fleet::Fleet::new(
            pool.clone(),
            cfg.secret_key.expose(),
            cfg.relay_address.clone(),
        ))
    } else {
        Arc::new(mock)
    };
    let state = taro_enterprise::build_state(cfg, pool, provider, None, None).await?;
    tracing::info!(
        environment = env.as_str(),
        provider = state.provider.kind(),
        simulated = state.provider.is_simulated(),
        billing = state
            .billing
            .as_ref()
            .map(|b| if b.simulated() {
                "simulated"
            } else if b.livemode() {
                "live"
            } else {
                "test"
            })
            .unwrap_or("none"),
        mail = state.mailer.kind(),
        "starting"
    );
    // the inventory first, so the catalog's availability is real from the first request
    if let Err(e) = taro_enterprise::reconcile::sync_inventory(&state).await {
        tracing::warn!(error = %e, "first inventory sync");
    }
    taro_enterprise::spawn_workers(&state);
    // one web site: the console under /console of the portal (TARO_OPS_URL has that path)
    let single = state.config.ops_url.origin() == state.config.public_url.origin()
        && state.config.ops_url.path().trim_end_matches('/') == "/console";
    let customer = tokio::net::TcpListener::bind(listen).await?;
    let shutdown = || async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("signal handler");
            tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
        }
        #[cfg(not(unix))]
        let _ = ctrl_c.await;
    };
    if single {
        tracing::info!(site = %listen, "listening (portal at /, console at /console)");
        axum::serve(
            customer,
            http::single(&state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown())
        .await?;
        tracing::info!("stopped");
        return Ok(());
    }
    let ops = tokio::net::TcpListener::bind(ops_listen).await?;
    tracing::info!(portal = %listen, console = %ops_listen, "listening");
    let a = axum::serve(
        customer,
        http::customer(&state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown());
    let b = axum::serve(
        ops,
        http::ops(&state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown());
    tokio::try_join!(a, b)?;
    tracing::info!("stopped");
    Ok(())
}
