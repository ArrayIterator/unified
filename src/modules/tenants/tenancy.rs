//! tenants - tenancy.

use unified_core::error::{UnifiedResult};
use unified_core::tenancy::{run_tenant_migrations, schema_name, TenantContext};
use uuid::Uuid;
use super::{constants::*, vocabulary::*, events::*, settings::*, records::*, state::*, auth::*, helpers::*, provisioning::*, destroy::*, handlers::*, module::*};



// Tenancy integration seam (B1 - `unified_core::tenancy`).
//
// Every call into the B1-owned tenancy helpers goes through these
// functions and nowhere else, so a signature change in
// crates/unified-core/src/tenancy.rs` is fixed in one place. Verified against
// the real crates/unified-core/src/tenancy.rs` at integration time (the assumed
// API this seam was first written against did not exist):
//   - `tenancy::schema_name(tenant_id: Uuid) -> String`: the
//     schema name derived from a tenant id. This is the ONLY
//     place a tenant schema name is ever built.
//   - `tenancy::TenantContext`: a struct with pub fields
//     (`tenant_id`, `schema_name`, `company_id`, `site_id`); there
//     is no `TenantContext::new` constructor and no `schema()`
//     accessor. Build it literally here, or from the middleware's
//     resolved context via `TenantContext::from_resolved` /
//     `from_http_request` at the HTTP edge.
//   - `tenancy::run_tenant_migrations(pool: &DbPool, tenant_id:
//     Uuid) -> UnifiedResult<()>`: creates the schema if missing
//     (`CREATE SCHEMA IF NOT EXISTS`) and runs the embedded
//     migrations inside it. Schema creation is folded inside it;
//     there is no separate `create_tenant_schema` helper and no
//     `run_tenant_migrations(pool, schema)` form.
// No statement that creates or removes a schema is built anywhere
// in this file: all of that lives behind these helpers.
/// Schema name for a tenant id, through the B1 helper.
pub fn tenant_schema_name(tenant_id: Uuid) -> String {
    schema_name(tenant_id)
}


/// Build the tenant context for a tenant id. The B1 type has no
/// constructor: it is built literally from its pub fields, with
/// the schema name derived by `schema_name` and no company / site
/// scope (this seam serves provisioning, not a request).
pub fn tenant_context_for(tenant_id: Uuid) -> TenantContext {
    TenantContext {
        tenant_id,
        schema_name: schema_name(tenant_id),
        company_id: None,
        site_id: None,
    }
}


/// Schema a tenant context resolves to: the pub `schema_name`
/// field, never rebuilt from the id.
pub fn tenant_context_schema(context: &TenantContext) -> &str {
    &context.schema_name
}


/// Create the tenant schema. There is no separate create helper
/// in B1: `run_tenant_migrations` creates the schema if missing
/// before migrating it, so this step and the migration step below
/// both run through it (the second call is a no-op for creation
/// and resumes any unapplied migrations).
pub(crate) fn ensure_tenant_schema(pool: &unified_core::db::DbPool, tenant_id: Uuid) -> UnifiedResult<()> {
    run_tenant_migrations(pool, tenant_id)
}


/// Run the tenant migrations inside the tenant schema, through
/// the B1 helper (schema creation is folded inside it).
pub(crate) fn migrate_tenant_schema(pool: &unified_core::db::DbPool, tenant_id: Uuid) -> UnifiedResult<()> {
    run_tenant_migrations(pool, tenant_id)
}
