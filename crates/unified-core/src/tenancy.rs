// Schema-per-tenant isolation: the single source of truth for how
// tenant data is separated from control-plane data.
//
// # The split
//
// The control plane lives in the `public` schema: tenants,
// subscriptions, domains, billing, and licensing records. Tenant
// business data lives in one PostgreSQL schema per tenant, named by
// `schema_name()` from the tenant's uuidv7 id. Shared lookup tables
// that must stay global (provider registries, country lists) are read
// from `public` via qualified names; the integrator enforces the exact
// table list, this module only states the rule.
//
// # TENANT-SCOPED DATA ACCESS (the contract every module converges on)
//
// Every module resolves ALL tenant business data through the tenant
// context and NEVER queries the control-plane database directly for
// tenant business data. The ONLY sanctioned path to tenant data is:
//
//   1. `TenantContext` (the extractor below) - the tenant id (uuidv7),
//      the derived schema name, and the company/site scope for this
//      request, read from the context the tenant-resolution middleware
//      attached. No handler invents, guesses, or defaults a tenant.
//   2. `with_tenant_connection` - the ONLY sanctioned way module code
//      touches a tenant schema through Diesel. It checks a connection
//      out of the pool, opens a transaction, sets a transaction-scoped
//      search path to the tenant schema, runs the caller's closure,
//      and commits. A pooled connection is never left pointing at a
//      tenant schema after checkout returns it.
//   3. An entitlement check - the purchased package determines which
//      modules and data the tenant may see. The check is resolved at
//      request time from the tenant context against the subscription /
//      package record held by the control plane (read through the
//      subscriptions / tenants module or its hook events, never by raw
//      SQL from a tenant handler). Until that wiring exists for a
//      module, the module fails closed for gated features instead of
//      serving data it has not proven the tenant is entitled to.
//
// The three-part resolution, in order, is: uuidv7 tenant id ->
// derived schema name via `schema_name()` -> search path (set by
// `with_tenant_connection`) + entitlement. See also the
// `TENANT_SCOPE_CONTRACT` constant, which states the same contract as
// data so other modules and the integrator can quote it verbatim.
//
// Two prohibitions apply to every module:
//
//   (a) No direct queries against the control-plane database for
//       tenant business data. Control-plane tables are accessed only
//       through their own modules / events, and only by code that
//       explicitly opted into control-plane access (see
//       `ControlPlaneGuard`).
//   (b) No hard-coded tenant ids, schema names, or package ids
//       anywhere. All of them are resolved at request time from the
//       tenant context. The only fixed strings in this module are
//       standard constants (`SCHEMA_PREFIX`, `CONTROL_PLANE_SCHEMA`,
//       the hook event names); behaviour, lists, and rules stay
//       data- / config-driven.
//
// # Fail closed
//
// If the plumbing is not yet wired for a module - no resolved tenant
// context on the request, no entitlement answer, no tenant schema -
// the module REJECTS the request. It never falls back to `public`
// tenant data, never serves cross-tenant data, and never serves a
// tenant's data to an unresolved host. The convergence point (what is
// missing and who owns it) is recorded in that module's integration
// notes for the integrator; the request still fails closed meanwhile.
//
// # Control-plane access is an explicit opt-in
//
// `TenantContext` has no control-plane mode and no constructor that
// yields a `public`-schema context: a handler that extracts it always
// gets a tenant, or the request is rejected. Control-plane handlers
// use `ControlPlaneGuard` instead, which succeeds only when the
// request carries NO resolved tenant context and rejects tenant-host
// requests outright, so tenant modules cannot accidentally use the
// control-plane path and control-plane modules cannot accidentally
// use the tenant path. A control-plane handler then uses a plain
// pooled connection (whose default search path is `public`); it never
// calls `set_tenant_search_path` or `with_tenant_connection`.
//
// # Safety invariants (enforced here, not by convention)
//
// - `schema_name()` is the ONLY place a tenant schema name is ever
//   built. Every DDL call in the whole codebase must go through this
//   helper (or `schema_name_for_str()` for external input).
// - `is_valid_schema_name()` validates every schema name before ANY
//   DDL executes. No user input ever reaches DDL: schema names are
//   built from a parsed uuidv7, validated against a strict pattern,
//   and only then interpolated into a quoted identifier.
// - Capability checks are the module layer's job; this module's job
//   is that the plumbing itself cannot leak across tenants.

use crate::db::DbPool;
use crate::error::{UnifiedError, UnifiedResult};
use crate::middleware::TenantContext as ResolvedTenantContext;
use actix_web::{FromRequest, HttpRequest, dev::Payload};
use diesel::{Connection, RunQueryDsl, pg::PgConnection, sql_query};
use futures_util::future::{Ready, ready};
use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;
use uuid::Uuid;

/// Prefix of every tenant schema name. A standard constant, not
/// behaviour: the schema name is always `tenant_` + the tenant id's
/// 32 lowercase hex characters, built only by `schema_name()`.
pub const SCHEMA_PREFIX: &str = "tenant_";

/// Name of the control-plane schema. A standard constant: the schema
/// that holds tenants, subscriptions, domains, billing, and licensing.
pub const CONTROL_PLANE_SCHEMA: &str = "public";

/// The tenant-scoped data access contract, stated as data so modules
/// and the integrator can quote it verbatim. The normative statement
/// is the module header doc comment above; this constant mirrors it.
pub const TENANT_SCOPE_CONTRACT: &str = "Tenant-scoped data access: \
every module resolves tenant business data through the tenant context \
(uuidv7 tenant id, derived tenant schema, package entitlements) via \
TenantContext plus with_tenant_connection plus an entitlement check. \
No module queries the control-plane database directly for tenant \
business data. No tenant id, schema name, or package id is hard-coded; \
all are resolved at request time from the tenant context. No tenant \
context, no entitlement answer, or no tenant schema means the request \
is rejected: fail closed, never fall back to the public schema.";

/// Hook event: a tenant finished provisioning (schema created,
/// migrations applied, subscription linked). Emitted by the tenants
/// module; this constant is the wire string only.
pub const EVENT_TENANT_PROVISIONED: &str = "tenant.provisioned";

/// Hook event: a tenant was suspended. Emitted by the tenants module;
/// this constant is the wire string only.
pub const EVENT_TENANT_SUSPENDED: &str = "tenant.suspended";

/// Hook event: tenant destruction was requested (export first,
/// confirmation, audit). Emitted by the tenants module; this constant
/// is the wire string only.
pub const EVENT_TENANT_DESTROY_REQUESTED: &str = "tenant.destroy_requested";

/// Hook event: a tenant was destroyed (schema dropped). Emitted by the
/// tenants module; this constant is the wire string only.
pub const EVENT_TENANT_DESTROYED: &str = "tenant.destroyed";

/// Hook event: a tenant schema was created. Emitted by the tenants
/// module (or by `run_tenant_migrations` callers through it); this
/// constant is the wire string only.
pub const EVENT_TENANT_SCHEMA_CREATED: &str = "tenant.schema_created";

/// Hook event: a tenant schema was dropped. Emitted by the tenants
/// module; this constant is the wire string only.
pub const EVENT_TENANT_SCHEMA_DROPPED: &str = "tenant.schema_dropped";

/// The schema name the nil UUID would derive to. A nil UUID is
/// never a tenant id, so this name is never a valid tenant schema:
/// `is_valid_schema_name()` rejects it even though it matches the
/// `tenant_` + 32-hex pattern. The choke point for the nil-UUID gap:
/// provisioning with a nil id would otherwise create a bogus
/// `tenant_000...0` schema.
pub const NIL_TENANT_SCHEMA_NAME: &str = "tenant_00000000000000000000000000000000";

/// Strict pattern for a tenant schema name: the `tenant_` prefix
/// followed by exactly 32 lowercase hex characters, nothing else.
/// Compiled once; `regex` is already a project dependency.
fn schema_name_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^tenant_[0-9a-f]{32}$").expect("tenant schema name pattern is a valid regex")
    })
}

/// Build the PostgreSQL schema name for a tenant.
///
/// THE ONLY place a tenant schema name is ever built. Every DDL call
/// in the whole codebase must go through this helper (or
/// `schema_name_for_str()` for external input) - never build a
/// `tenant_...` name inline anywhere else.
///
/// The name is `tenant_` + the uuidv7's simple hex form (32 lowercase
/// hex characters, no hyphens, no separators). A raw UUID cannot be a
/// bare Postgres identifier: it starts with a digit and contains
/// hyphens. The prefixed simple form is a valid bare identifier and
/// is still a one-to-one mapping from the tenant id, so the schema can
/// always be derived back from the id and vice versa.
pub fn schema_name(tenant_id: Uuid) -> String {
    format!("{SCHEMA_PREFIX}{}", tenant_id.simple())
}

/// Validate a tenant schema name against the strict pattern:
/// starts with `tenant_`, then exactly 32 lowercase hex characters,
/// nothing else. Use this to validate every schema name before ANY
/// DDL executes; a name that fails here never reaches SQL.
///
/// The nil-derived name `tenant_000...0` is rejected even though it
/// matches the pattern: a nil UUID is never a tenant id, so the name
/// it derives to is never a valid tenant schema. This is the choke
/// point - every validator call site inherits the fix.
pub fn is_valid_schema_name(name: &str) -> bool {
    name != NIL_TENANT_SCHEMA_NAME && schema_name_pattern().is_match(name)
}

/// Parse an external tenant id string and build its schema name.
///
/// The ONLY entry point for converting an external tenant id into a
/// schema name. Rejects any input that is not a valid UUID (400), and
/// defensively re-validates the built name before returning it, so a
/// bug in `schema_name()` can never smuggle an invalid name into DDL.
pub fn schema_name_for_str(raw: &str) -> UnifiedResult<String> {
    let tenant_id = Uuid::parse_str(raw.trim()).map_err(|_| {
        UnifiedError::bad_request("Invalid tenant id: expected a UUID").with_metadata(
            "kind",
            serde_json::Value::String("invalid_tenant_id".to_string()),
        )
    })?;
    if tenant_id.is_nil() {
        return Err(
            UnifiedError::forbidden("Refusing tenant schema access without a tenant id")
                .with_metadata(
                    "kind",
                    serde_json::Value::String("invalid_tenant_id".to_string()),
                ),
        );
    }
    let name = schema_name(tenant_id);
    if !is_valid_schema_name(&name) {
        return Err(UnifiedError::internal(
            "Built tenant schema name failed validation",
        ));
    }
    Ok(name)
}

/// The tenant a request belongs to, as seen by a handler.
///
/// Built from the `ResolvedTenantContext` the tenant-resolution
/// middleware attached to the request extensions. The extractor
/// (`FromRequest`) fails closed: a request with no resolved tenant
/// context, or with a tenant id that is not a uuidv7 UUID, is
/// rejected and never falls back to `public` tenant data.
///
/// There is deliberately no control-plane mode on this type and no
/// constructor that yields a `public`-schema context. Control-plane
/// handlers must explicitly opt into public-schema access with
/// `ControlPlaneGuard` instead of using this extractor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TenantContext {
    /// The tenant id (uuidv7, time-ordered).
    pub tenant_id: Uuid,
    /// The tenant's PostgreSQL schema name, built only by
    /// `schema_name()` from `tenant_id`.
    pub schema_name: String,
    /// Company scope from the resolved context, when present.
    pub company_id: Option<String>,
    /// Site scope from the resolved context, when present.
    pub site_id: Option<String>,
}

impl TenantContext {
    /// Build from the middleware's resolved context. Fails closed
    /// when the resolved tenant id is not a parseable UUID: the
    /// middleware accepts opaque ids, but tenancy only ever serves
    /// uuidv7 tenants, so a non-UUID id is rejected rather than
    /// served against the wrong schema. Empty company / site ids
    /// become `None`.
    pub fn from_resolved(resolved: &ResolvedTenantContext) -> UnifiedResult<Self> {
        let tenant_id = Uuid::parse_str(&resolved.tenant_id).map_err(|_| {
            UnifiedError::forbidden("Resolved tenant id is not a valid tenant UUID").with_metadata(
                "kind",
                serde_json::Value::String("invalid_tenant_id".to_string()),
            )
        })?;
        if tenant_id.is_nil() {
            return Err(
                UnifiedError::forbidden("Resolved tenant id is not a valid tenant UUID")
                    .with_metadata(
                        "kind",
                        serde_json::Value::String("invalid_tenant_id".to_string()),
                    ),
            );
        }
        let schema_name = schema_name(tenant_id);
        Ok(Self {
            tenant_id,
            schema_name,
            company_id: none_if_empty(&resolved.company_id),
            site_id: none_if_empty(&resolved.site_id),
        })
    }

    /// Build from an `HttpRequest` that already passed through the
    /// tenant-resolution middleware. Returns an error (never `None`,
    /// never a fallback) when the request carries no resolved tenant
    /// context.
    pub fn from_http_request(req: &HttpRequest) -> UnifiedResult<Self> {
        let resolved = ResolvedTenantContext::from_request(req).ok_or_else(|| {
            UnifiedError::unauthorized("Request did not resolve to a tenant").with_metadata(
                "kind",
                serde_json::Value::String("missing_tenant_context".to_string()),
            )
        })?;
        Self::from_resolved(&resolved)
    }
}

fn none_if_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

impl FromRequest for TenantContext {
    type Error = UnifiedError;
    type Future = Ready<Result<Self, Self::Error>>;

    /// Fail-closed extractor: a request without a resolved tenant
    /// context is rejected (401); a resolved context whose tenant id
    /// is not a uuidv7 UUID is rejected (403). There is no fallback
    /// to the `public` schema and no default tenant.
    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        ready(Self::from_http_request(req))
    }
}

/// Explicit opt-in for control-plane access.
///
/// A handler that legitimately needs the control plane (tenants,
/// subscriptions, domains, billing, licensing in the `public`
/// schema) extracts `ControlPlaneGuard` instead of `TenantContext`.
/// The guard succeeds ONLY when the request carries NO resolved
/// tenant context - that is, the request arrived on the control
/// plane - and rejects (403) any request that resolved to a tenant,
/// so a tenant host can never reach a control-plane handler through
/// this guard, and a tenant module cannot accidentally use the
/// control-plane path. A handler holding this guard uses a plain
/// pooled connection (default search path `public`); it never calls
/// `set_tenant_search_path` or `with_tenant_connection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlPlaneGuard;

impl ControlPlaneGuard {
    /// True when the request is a control-plane request: no resolved
    /// tenant context is attached to it.
    pub fn is_control_plane_request(req: &HttpRequest) -> bool {
        ResolvedTenantContext::from_request(req).is_none()
    }
}

impl FromRequest for ControlPlaneGuard {
    type Error = UnifiedError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        if ResolvedTenantContext::from_request(req).is_some() {
            return ready(Err(UnifiedError::forbidden(
                "Tenant requests cannot access the control plane",
            )
            .with_metadata(
                "kind",
                serde_json::Value::String("tenant_control_plane_blocked".to_string()),
            )));
        }
        ready(Ok(ControlPlaneGuard))
    }
}

/// The company scope a caller acts under, decided once per request
/// and then carried everywhere a record's company is checked.
///
/// # Fail-closed rule
///
/// A caller may read any company's records ONLY when the scope is
/// the explicitly-decided `Platform` variant. The absence of a
/// company - a caller no source resolves a company for - is
/// `Unresolved`, never platform: an unresolved caller reads global
/// (unscoped, empty-company) rows only and is denied every
/// company-owned record. There is no "no company means platform"
/// fallback anywhere; the old no-scope-matches-everything shape was the HIGH-3
/// hole this type closes.
///
/// # Platform decision rule (identical in every module)
///
/// A caller is `Platform` ONLY when ALL three hold:
///
/// 1. no company resolves from any source (verified caller company,
///    tenant-context company, `user.company_requested` hook answer);
/// 2. the request is a control-plane request - no resolved tenant
///    context is attached (`ControlPlaneGuard::is_control_plane_request`);
/// 3. the verified caller holds an admin role (`admin` or
///    `super_admin`, server-resolved only, never a header).
///
/// Otherwise a resolved company yields `Company`, and no company
/// yields `Unresolved`. Company resolution precedence, most
/// authoritative first: verified caller company
/// (`caller::caller_from(req).company_id`), then the tenant-context
/// company, then the `user.company_requested` hook answer. A
/// body/query `company_id` is NEVER a source - it is verified
/// against the scope instead.
///
/// # Adopted rules (C3 tasks 315/320, 2026-11-23)
///
/// - Read rule (320, canonical rule A): `Company(c)` reads its own
///   rows PLUS global rows (`company_id` NULL); `Platform` reads
///   any; `Unresolved` reads global only. The per-module
///   `in_company_scope` read helpers implement this rule.
/// - Write rule (315, strict by default): writes require a resolved
///   company and a match, 403 otherwise. `require_company_scope`
///   below is the shared write guard: `Platform` may write any
///   scope; `Company(c)` writes its own company only - the global
///   scope is platform-only; `Unresolved` writes nothing. Named
///   carve-outs only, listed in the C3 guard map
///   (`docs/integration/C3_MODULE_GUARD_MAP.md`): (1) self-service
///   writes (applicant submit/withdraw, consent grant/withdraw,
///   own notifications); (2) platform-scope writes through an
///   explicit, audited platform scope (`ControlPlaneGuard` +
///   `CallerScope::decide` - never silent, never by default);
///   (3) genuinely tenant-scoped writes where the record carries
///   no company field yet - tenant-gate-only until C11 lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerScope {
    /// The caller acts for one company: reads that company's rows
    /// plus global rows, writes that company only.
    Company(String),
    /// Explicitly-decided platform scope (control-plane request by
    /// a verified admin with no resolvable company): reads and
    /// writes any company, including global rows.
    Platform,
    /// No company resolved and the platform decision did not hold:
    /// reads global rows only, writes nothing.
    Unresolved,
}

impl CallerScope {
    /// Decide the scope from a resolved company (if any), the
    /// request, and whether the verified caller holds an admin role
    /// (server-resolved only). A resolved company always wins over
    /// the platform decision; platform requires a control-plane
    /// request AND an admin caller; anything else is unresolved.
    pub fn decide(company: Option<String>, req: &HttpRequest, is_admin: bool) -> Self {
        match company.filter(|value| !value.is_empty()) {
            Some(company) => Self::Company(company),
            None => {
                if is_admin && ControlPlaneGuard::is_control_plane_request(req) {
                    Self::Platform
                } else {
                    Self::Unresolved
                }
            }
        }
    }

    /// Scope from a resolved company only, without a platform
    /// decision: `Company` when a company resolved, `Unresolved`
    /// otherwise. Use when the caller cannot be platform (for
    /// example a handler that already required a tenant context).
    pub fn from_company(company: Option<String>) -> Self {
        match company.filter(|value| !value.is_empty()) {
            Some(company) => Self::Company(company),
            None => Self::Unresolved,
        }
    }

    /// The company this scope acts for, when it is a company scope.
    pub fn company(&self) -> Option<&str> {
        match self {
            Self::Company(company) => Some(company),
            Self::Platform | Self::Unresolved => None,
        }
    }

    /// True only for the explicitly-decided platform scope.
    pub fn is_platform(&self) -> bool {
        matches!(self, Self::Platform)
    }

    /// The company id to stamp on a record this caller creates:
    /// the caller's company, or empty (a global row) for platform
    /// and unresolved callers. Platform-created rows are global
    /// because no company owns them.
    pub fn stored_company_id(&self) -> &str {
        match self {
            Self::Company(company) => company,
            Self::Platform | Self::Unresolved => "",
        }
    }

    /// True when a caller in this scope may read a record owned by
    /// `record_company` (empty means a global/shared row).
    /// Platform reads any record; a company caller reads their own
    /// company's rows plus global rows; an unresolved caller reads
    /// global rows ONLY - every company-owned record is denied.
    /// A manage capability deliberately does NOT widen this scope.
    pub fn can_access_record(&self, record_company: &str) -> bool {
        match self {
            Self::Platform => true,
            Self::Company(company) => record_company.is_empty() || record_company == company,
            Self::Unresolved => record_company.is_empty(),
        }
    }

    /// Write scope (C3, strict by default - the shared
    /// write guard): a caller may only write rows of the target
    /// company. Platform may write any scope, including the global
    /// (empty) scope. A company caller may write only their own
    /// company - the global scope is platform-only, because writing
    /// it would change shared data for every company. An
    /// unresolved caller may write nothing.
    pub fn require_company_scope(&self, target_company: &str) -> UnifiedResult<()> {
        match self {
            Self::Platform => Ok(()),
            Self::Company(company) if company == target_company => Ok(()),
            Self::Company(_) | Self::Unresolved => Err(UnifiedError::forbidden(
                "Company scope mismatch: the request company_id does not match the caller's company",
            )),
        }
    }

    /// Resolve the company scope for a read that names a company
    /// (query filter or path-adjacent parameter). A platform caller
    /// reads exactly what was requested; a company caller reads
    /// their own company (an empty request defaults to it), and any
    /// other explicitly requested company fails closed with 403.
    /// An unresolved caller reads global rows only: an empty
    /// request resolves to the empty (global) scope, and any named
    /// company is refused.
    pub fn scoped_read_company(&self, requested: &str) -> UnifiedResult<String> {
        match self {
            Self::Platform => Ok(requested.to_string()),
            Self::Company(company) => {
                if requested.is_empty() || requested == company {
                    Ok(company.clone())
                } else {
                    Err(UnifiedError::forbidden(
                        "Company scope mismatch: the requested company_id does not match the caller's company",
                    ))
                }
            }
            Self::Unresolved => {
                if requested.is_empty() {
                    Ok(String::new())
                } else {
                    Err(UnifiedError::forbidden(
                        "Company scope mismatch: the requested company_id does not match the caller's company",
                    ))
                }
            }
        }
    }
}

/// Set the search path of a connection to one tenant schema.
///
/// Runs `SET LOCAL search_path = "<validated schema>", public` (the
/// schema name is validated by `is_valid_schema_name()` first and
/// still double-quoted; it contains only `tenant_` + 32 lowercase
/// hex characters, so no quoting escape is possible - and no
/// unvalidated string is ever interpolated into SQL here).
///
/// `SET LOCAL` is transaction-scoped, not per-connection pool setup,
/// and that is deliberate: a pooled connection is checked out, used,
/// and returned to the pool many times, and a session-level `SET`
/// would survive the checkout and leak the tenant schema into the
/// next, unrelated request that reuses the connection. `SET LOCAL`
/// dies with the transaction instead, so a connection returned to the
/// pool is always back to the default search path. Callers MUST call
/// this inside a transaction - `with_tenant_connection` guarantees
/// that; calling it outside a transaction leaves the setting without
/// effect (PostgreSQL warns and discards it), so direct callers that
/// ignore the transaction requirement fail open and must not exist.
/// The trailing `, public` keeps shared global lookup tables
/// (provider registries, country lists) readable by unqualified name
/// as a fallback; tenant tables always resolve first because the
/// tenant schema precedes `public` in the path.
pub fn set_tenant_search_path(conn: &mut PgConnection, schema: &str) -> UnifiedResult<()> {
    if !is_valid_schema_name(schema) {
        return Err(UnifiedError::internal(
            "Refusing to set search path to an invalid tenant schema name",
        ));
    }
    sql_query(format!("SET LOCAL search_path TO \"{schema}\", public")).execute(conn)?;
    Ok(())
}

/// The ONLY sanctioned way module code touches a tenant schema
/// through Diesel: check a connection out of the pool, start a
/// transaction, set the transaction-scoped search path to the
/// tenant's schema, run the closure, and commit.
///
/// The closure runs entirely inside the transaction, so either all
/// of its tenant writes land or none do, and the `SET LOCAL` search
/// path dies with the transaction - the pooled connection is never
/// left pointing at a tenant schema. The closure receives the
/// connection with the search path already set; it must not call
/// `set_tenant_search_path` again and must not run DDL. Capability
/// checks and the entitlement check stay the calling module's job
/// and happen before this call, never inside the closure as an
/// afterthought.
///
/// Fail closed at this choke point: the nil UUID is never a tenant
/// id. Without this check `schema_name(Uuid::nil())` builds the
/// syntactically valid `tenant_000...0` name and the call would
/// silently query an empty schema instead of failing - so a caller
/// that lost its tenant id would read nothing (and believe it read
/// the tenant) rather than erroring. A nil id is rejected before any
/// connection is checked out.
pub fn with_tenant_connection<T>(
    pool: &DbPool,
    tenant_id: Uuid,
    f: impl FnOnce(&mut PgConnection) -> UnifiedResult<T>,
) -> UnifiedResult<T> {
    if tenant_id.is_nil() {
        return Err(
            UnifiedError::forbidden("Refusing tenant database access without a tenant id")
                .with_metadata(
                    "kind",
                    serde_json::Value::String("invalid_tenant_id".to_string()),
                ),
        );
    }
    let schema = schema_name(tenant_id);
    let mut connection = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    connection.transaction(|conn| {
        set_tenant_search_path(conn, &schema)?;
        f(conn)
    })
}

/// Provision a tenant schema and run the embedded migrations in it.
///
/// Creates the schema if missing (`CREATE SCHEMA IF NOT EXISTS
/// "<validated name>"`), then runs the SAME embedded Diesel
/// migration set used by the control-plane `run_migrations` inside
/// that schema via `crate::db::run_migrations_in_schema`. Diesel
/// records applied migrations in the schema-local
/// `__diesel_schema_migrations` table, so upgrades are resumable
/// and never re-run, and each tenant schema migrates independently
/// of the others and of `public`.
///
/// This only creates the schema and migrates it. Subscription
/// linking, domain verification, and the `tenant.provisioned` /
/// `tenant.schema_created` hook events are the tenants module's job.
pub fn run_tenant_migrations(pool: &DbPool, tenant_id: Uuid) -> UnifiedResult<()> {
    if tenant_id.is_nil() {
        return Err(
            UnifiedError::forbidden("Refusing tenant provisioning without a tenant id")
                .with_metadata(
                    "kind",
                    serde_json::Value::String("invalid_tenant_id".to_string()),
                ),
        );
    }
    let schema = schema_name(tenant_id);
    if !is_valid_schema_name(&schema) {
        return Err(UnifiedError::internal(
            "Built tenant schema name failed validation",
        ));
    }
    let mut connection = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    sql_query(format!("CREATE SCHEMA IF NOT EXISTS \"{schema}\"")).execute(&mut connection)?;
    crate::db::run_migrations_in_schema(pool, &schema)
}

/// True when the tenant's schema exists. Delegates to
/// `crate::db::schema_exists` with the schema name derived by
/// `schema_name()`; the id is never turned into SQL directly.
/// A nil tenant id is rejected before any database access: it is
/// never a tenant, so there is no schema whose existence to check.
/// (`crate::db::schema_exists` also validates the derived name,
/// which `is_valid_schema_name()` now rejects for the nil UUID -
/// this explicit check is the first layer.)
pub fn schema_exists(pool: &DbPool, tenant_id: Uuid) -> UnifiedResult<bool> {
    if tenant_id.is_nil() {
        return Err(
            UnifiedError::forbidden("Refusing tenant schema access without a tenant id")
                .with_metadata(
                    "kind",
                    serde_json::Value::String("invalid_tenant_id".to_string()),
                ),
        );
    }
    crate::db::schema_exists(pool, &schema_name(tenant_id))
}

/// Drop a tenant's schema, with everything in it.
///
/// `DROP SCHEMA "<validated name>" CASCADE`. Irreversible: every
/// table, row, and object in the tenant schema is destroyed. Used
/// ONLY by the destroy flow, after export, confirmation, and audit
/// (see the `tenant.destroy_requested` / `tenant.destroyed` events);
/// never called from provisioning, never called from a request
/// handler, and never called without an audit record of who asked
/// for the destruction and what was exported first.
pub fn drop_tenant_schema(pool: &DbPool, tenant_id: Uuid) -> UnifiedResult<()> {
    if tenant_id.is_nil() {
        return Err(
            UnifiedError::forbidden("Refusing tenant schema access without a tenant id")
                .with_metadata(
                    "kind",
                    serde_json::Value::String("invalid_tenant_id".to_string()),
                ),
        );
    }
    crate::db::drop_tenant_schema(pool, &schema_name(tenant_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::{TenantContext as MwTenantContext, TenantKind};
    use actix_web::HttpMessage;
    use diesel::r2d2::{ConnectionManager, Pool};

    /// A pool that never connects: `Pool::builder().build()` opens
    /// one connection eagerly to validate the manager, so it fails
    /// against an unreachable URL. `build_unchecked` skips that
    /// probe - the nil checks under test reject before any checkout
    /// is attempted, so no connection is ever opened.
    fn dead_pool() -> DbPool {
        Pool::builder()
            .max_size(1)
            .min_idle(None)
            .build_unchecked(ConnectionManager::<PgConnection>::new(
                "postgres://127.0.0.1:1/nope",
            ))
    }

    fn resolved_context(tenant_id: &str, company_id: &str) -> ResolvedTenantContext {
        MwTenantContext {
            tenant_id: tenant_id.to_string(),
            company_id: company_id.to_string(),
            site_id: String::new(),
            kind: TenantKind::Subdomain,
            host: "tenant.example.test".to_string(),
            scope: None,
            product_id: None,
            package_id: None,
            product_subdomain_id: None,
        }
    }

    fn plain_request() -> HttpRequest {
        actix_web::test::TestRequest::default().to_http_request()
    }

    fn tenant_request() -> HttpRequest {
        let req = plain_request();
        req.extensions_mut()
            .insert(resolved_context(&Uuid::new_v4().to_string(), ""));
        req
    }

    #[test]
    fn nil_schema_name_is_not_valid() {
        assert_eq!(
            NIL_TENANT_SCHEMA_NAME,
            "tenant_00000000000000000000000000000000"
        );
        assert_eq!(schema_name(Uuid::nil()), NIL_TENANT_SCHEMA_NAME);
        assert!(!is_valid_schema_name(NIL_TENANT_SCHEMA_NAME));
        assert!(!is_valid_schema_name(&schema_name(Uuid::nil())));
        // A real tenant name still validates.
        assert!(is_valid_schema_name(
            "tenant_0123456789abcdef0123456789abcdef"
        ));
        assert!(is_valid_schema_name(&schema_name(Uuid::new_v4())));
    }

    #[test]
    fn schema_name_for_str_rejects_nil_uuid() {
        assert!(schema_name_for_str("00000000-0000-0000-0000-000000000000").is_err());
        assert!(schema_name_for_str(&Uuid::nil().to_string()).is_err());
        assert!(schema_name_for_str(&Uuid::new_v4().to_string()).is_ok());
        assert!(schema_name_for_str("not-a-uuid").is_err());
    }

    #[test]
    fn tenant_context_from_resolved_rejects_nil_uuid() {
        assert!(
            TenantContext::from_resolved(&resolved_context(&Uuid::nil().to_string(), "")).is_err()
        );
        assert!(
            TenantContext::from_resolved(&resolved_context(
                &Uuid::new_v4().to_string(),
                "company-a"
            ))
            .is_ok()
        );
        assert!(TenantContext::from_resolved(&resolved_context("not-a-uuid", "")).is_err());
    }

    #[test]
    fn run_tenant_migrations_rejects_nil_uuid() {
        let pool = dead_pool();
        assert!(run_tenant_migrations(&pool, Uuid::nil()).is_err());
    }

    #[test]
    fn schema_exists_rejects_nil_uuid() {
        let pool = dead_pool();
        assert!(schema_exists(&pool, Uuid::nil()).is_err());
    }

    #[test]
    fn drop_tenant_schema_rejects_nil_uuid() {
        let pool = dead_pool();
        assert!(drop_tenant_schema(&pool, Uuid::nil()).is_err());
    }

    #[test]
    fn caller_scope_can_access_record_is_fail_closed() {
        // Platform (explicitly decided) reads any record.
        assert!(CallerScope::Platform.can_access_record("company-a"));
        assert!(CallerScope::Platform.can_access_record(""));
        // A company caller reads its own rows plus global rows.
        let company = CallerScope::Company("company-a".to_string());
        assert!(company.can_access_record("company-a"));
        assert!(company.can_access_record(""));
        assert!(!company.can_access_record("company-b"));
        // Unresolved reads global rows ONLY: every company-owned
        // record is denied (the old no-scope-matches-everything behaviour).
        assert!(CallerScope::Unresolved.can_access_record(""));
        assert!(!CallerScope::Unresolved.can_access_record("company-a"));
        assert!(!CallerScope::Unresolved.can_access_record("company-b"));
    }

    #[test]
    fn caller_scope_require_company_scope() {
        assert!(
            CallerScope::Platform
                .require_company_scope("company-a")
                .is_ok()
        );
        assert!(CallerScope::Platform.require_company_scope("").is_ok());
        let company = CallerScope::Company("company-a".to_string());
        assert!(company.require_company_scope("company-a").is_ok());
        assert!(company.require_company_scope("company-b").is_err());
        assert!(company.require_company_scope("").is_err());
        assert!(
            CallerScope::Unresolved
                .require_company_scope("company-a")
                .is_err()
        );
        assert!(CallerScope::Unresolved.require_company_scope("").is_err());
    }

    #[test]
    fn caller_scope_scoped_read_company() {
        assert_eq!(
            CallerScope::Platform
                .scoped_read_company("company-b")
                .unwrap(),
            "company-b"
        );
        assert_eq!(CallerScope::Platform.scoped_read_company("").unwrap(), "");
        let company = CallerScope::Company("company-a".to_string());
        assert_eq!(company.scoped_read_company("").unwrap(), "company-a");
        assert_eq!(
            company.scoped_read_company("company-a").unwrap(),
            "company-a"
        );
        assert!(company.scoped_read_company("company-b").is_err());
        // Unresolved reads global rows only.
        assert_eq!(CallerScope::Unresolved.scoped_read_company("").unwrap(), "");
        assert!(
            CallerScope::Unresolved
                .scoped_read_company("company-a")
                .is_err()
        );
    }

    #[test]
    fn caller_scope_decide_requires_control_plane_and_admin_for_platform() {
        // No company, control-plane request, admin caller: Platform.
        let scope = CallerScope::decide(None, &plain_request(), true);
        assert_eq!(scope, CallerScope::Platform);
        // No company, control-plane request, non-admin: Unresolved.
        assert_eq!(
            CallerScope::decide(None, &plain_request(), false),
            CallerScope::Unresolved
        );
        // No company, admin caller, but a tenant host resolved:
        // NOT platform - the tenant context makes it unresolved.
        assert_eq!(
            CallerScope::decide(None, &tenant_request(), true),
            CallerScope::Unresolved
        );
        // A resolved company always wins over the platform decision.
        assert_eq!(
            CallerScope::decide(Some("company-a".to_string()), &plain_request(), true),
            CallerScope::Company("company-a".to_string())
        );
        assert_eq!(
            CallerScope::from_company(Some("company-a".to_string())),
            CallerScope::Company("company-a".to_string())
        );
        assert_eq!(CallerScope::from_company(None), CallerScope::Unresolved);
        assert_eq!(
            CallerScope::from_company(Some(String::new())),
            CallerScope::Unresolved
        );
    }
}
