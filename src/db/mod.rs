// Database layer: Diesel with PostgreSQL by default.
//
// `DbPool` is the shared connection pool type used by the application
// context and by every module that touches the database. Migrations live
// in `migrations/` at the repository root and are embedded at compile time.

use crate::core::error::{UnifiedError, UnifiedResult};
use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, Pool};

/// Shared Diesel connection pool (PostgreSQL).
pub type DbPool = Pool<ConnectionManager<PgConnection>>;

/// Build a connection pool from the application configuration.
pub fn pool_from_config(
    config: &crate::core::config::AppConfig,
) -> UnifiedResult<DbPool> {
    let url = config.database_url()?;
    let manager = ConnectionManager::<PgConnection>::new(url);
    Pool::builder()
        .max_size(config.database.max_connections)
        .min_idle(Some(config.database.min_connections))
        .connection_timeout(std::time::Duration::from_secs(
            config.database.acquire_timeout,
        ))
        .build(manager)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot create database pool: {error}"))
                .with_trace(format!("{error:?}"))
        })
}

/// Run the embedded Diesel migrations against the pool.
pub fn run_migrations(pool: &DbPool) -> UnifiedResult<()> {
    use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
    const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");
    let mut connection = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    connection.run_pending_migrations(MIGRATIONS).map_err(|error| {
        UnifiedError::internal(format!("Database migration failed: {error}"))
            .with_trace(format!("{error:?}"))
    })?;
    Ok(())
}

pub mod models;
pub mod schema;
