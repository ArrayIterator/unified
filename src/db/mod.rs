// Database layer: Diesel with PostgreSQL by default.
//
// `DbPool` is the shared connection pool type used by the application
// context and by every module that touches the database. Migrations live
// in `migrations/` at the repository root and are embedded at compile time.

use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::tenancy::is_valid_schema_name;
use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::{Connection, QueryableByName, RunQueryDsl};

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

/// Run the embedded Diesel migrations inside one schema.
///
/// The schema name must already be validated by
/// `crate::core::tenancy::is_valid_schema_name` (only names built by
/// `crate::core::tenancy::schema_name` ever reach here); anything else
/// is refused before any SQL runs. The search path is set with
/// `SET LOCAL` inside a transaction and the same embedded migration
/// set as `run_migrations` then runs against that schema, so Diesel
/// records applied migrations in the schema-local
/// `__diesel_schema_migrations` table and upgrades are resumable and
/// never re-run. Called by `crate::core::tenancy::run_tenant_migrations`
/// after the schema is created; `run_migrations` itself is untouched.
pub fn run_migrations_in_schema(pool: &DbPool, schema: &str) -> UnifiedResult<()> {
    use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
    if !is_valid_schema_name(schema) {
        return Err(UnifiedError::internal(
            "Refusing to run migrations in an invalid tenant schema name",
        ));
    }
    const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");
    let mut connection = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    connection
        .transaction(|conn| {
            crate::core::tenancy::set_tenant_search_path(conn, schema)?;
            conn.run_pending_migrations(MIGRATIONS).map_err(|error| {
                UnifiedError::internal(format!("Database migration failed: {error}"))
                    .with_trace(format!("{error:?}"))
            })?;
            Ok(())
        })
        .map_err(|error: UnifiedError| error)
}

/// One row of a `schema_name` lookup against `information_schema`.
#[derive(Debug, QueryableByName)]
struct SchemaNameRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    schema_name: String,
}

/// True when a schema with this name exists.
///
/// The name is validated before any SQL runs and is passed as a bound
/// parameter (`$1`) to `information_schema.schemata`, never
/// interpolated into the query text.
pub fn schema_exists(pool: &DbPool, schema: &str) -> UnifiedResult<bool> {
    if !is_valid_schema_name(schema) {
        return Err(UnifiedError::internal(
            "Refusing to check existence of an invalid tenant schema name",
        ));
    }
    let mut connection = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    let rows = diesel::sql_query(
        "SELECT schema_name FROM information_schema.schemata WHERE schema_name = $1",
    )
    .bind::<diesel::sql_types::Text, _>(schema)
    .load::<SchemaNameRow>(&mut connection)?;
    Ok(!rows.is_empty())
}

/// Drop a tenant schema, with everything in it.
///
/// `DROP SCHEMA "<validated name>" CASCADE`. The name is validated
/// before any SQL runs and is only ever interpolated as a quoted
/// identifier after that validation. Irreversible: call only from the
/// destroy flow (export, confirmation, audit first), never from
/// provisioning. See `crate::core::tenancy::drop_tenant_schema`.
pub fn drop_tenant_schema(pool: &DbPool, schema: &str) -> UnifiedResult<()> {
    if !is_valid_schema_name(schema) {
        return Err(UnifiedError::internal(
            "Refusing to drop an invalid tenant schema name",
        ));
    }
    let mut connection = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    diesel::sql_query(format!("DROP SCHEMA \"{schema}\" CASCADE")).execute(&mut connection)?;
    Ok(())
}

pub mod models;
pub mod schema;
