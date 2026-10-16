//! The database pool and its migrations (versioned SQL files in migrations/, embedded in the
//! binary).

use sqlx::postgres::PgPoolOptions;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> anyhow::Result<sqlx::PgPool> {
    Ok(PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(url)
        .await?)
}

pub async fn migrate(db: &sqlx::PgPool) -> anyhow::Result<()> {
    MIGRATOR.run(db).await?;
    Ok(())
}
