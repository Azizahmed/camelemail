//! Postgres pool and migrations.

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::migrate::Migrator;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str, max_connections: u32) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
        .context("can't connect to Postgres (DATABASE_URL)")
}

pub async fn migrate(db: &PgPool) -> Result<()> {
    MIGRATOR.run(db).await.context("database migration failed")
}
