// Api Manager - Endpoint, version, access-rule and registry types.

use unified_core::modules::RouteVersionSupport;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{parse_version_number, sort_version_ids};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointRule {
    pub field: String,
    pub required: bool,
    #[serde(rename = "type")]
    pub field_type: String,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiEndpoint {
    pub id: Uuid,
    pub method: String,
    pub path: String,
    /// Lowest version in `version_support`, kept for backward
    /// compatibility with clients that read a single version.
    pub version: String,
    pub description: String,
    pub rate_limit: u32,
    pub enabled: bool,
    pub rules: Vec<EndpointRule>,
    pub execution_count: u64,
    /// Id of the module that owns this endpoint. Custom endpoints
    /// created through this module use `"custom"`.
    #[serde(default = "default_owner_module")]
    pub owner_module: String,
    /// Whether callers must authenticate (API key) for this endpoint.
    #[serde(default = "default_true")]
    pub auth_required: bool,
    /// `"custom"` for admin-created endpoints, `"builtin"` for
    /// endpoints imported from the module route catalog.
    #[serde(default = "default_endpoint_source")]
    pub source: String,
    /// Which API versions expose this endpoint.
    #[serde(default)]
    pub version_support: VersionSupport,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub(crate) fn default_owner_module() -> String {
    "custom".to_string()
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_endpoint_source() -> String {
    "custom".to_string()
}

#[derive(Debug, Deserialize)]
pub struct CreateEndpointInput {
    pub method: String,
    pub path: String,
    /// Legacy single version (for example `"v1"`). Optional now:
    /// prefer `versions` or `version_support`. When only this field
    /// is given it becomes a one-version list.
    pub version: Option<String>,
    /// Explicit multi-select of versions, for example `["v1", "v2"]`.
    /// Each entry must be an existing registry version id.
    pub versions: Option<Vec<String>>,
    /// Structured version support. Takes precedence over `versions`
    /// and `version`, which are ignored when this is present.
    pub version_support: Option<VersionSupport>,
    pub description: Option<String>,
    pub rate_limit: Option<u32>,
    pub enabled: Option<bool>,
    pub rules: Option<Vec<EndpointRule>>,
    /// Owner module label for the new endpoint. Defaults to
    /// `"custom"`; built-in owners are never accepted here because
    /// built-in endpoints come from the route catalog, not this form.
    pub owner_module: Option<String>,
    pub auth_required: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEndpointInput {
    pub method: Option<String>,
    pub path: Option<String>,
    pub version: Option<String>,
    pub versions: Option<Vec<String>>,
    pub version_support: Option<VersionSupport>,
    pub description: Option<String>,
    pub rate_limit: Option<u32>,
    pub enabled: Option<bool>,
    pub rules: Option<Vec<EndpointRule>>,
    pub owner_module: Option<String>,
    pub auth_required: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateKeyInput {
    pub name: String,
    pub scopes: Option<Vec<String>>,
    pub expires_in_days: Option<u32>,
}

// ---------------------------------------------------------------------------
// Version registry types
// ---------------------------------------------------------------------------

/// How an endpoint declares the API versions it is available in.
///
/// Precedence when resolving a request: an explicit list beats a
/// range, a range beats a bare minimum/maximum, and `All` matches
/// every registered version. A `Min`/`Max`/`Range` bound is
/// inclusive. `List` entries are kept sorted by version number.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VersionSupport {
    All,
    Min { min_version: String },
    Max { max_version: String },
    Range { min_version: String, max_version: String },
    List { versions: Vec<String> },
}

impl Default for VersionSupport {
    fn default() -> Self {
        VersionSupport::All
    }
}

impl VersionSupport {
    /// True when `version` is covered by this support declaration.
    /// Unknown version ids never match, not even under `All`.
    pub fn supports(&self, version: &str, known_versions: &[String]) -> bool {
        let Some(number) = parse_version_number(version) else {
            return false;
        };
        if !known_versions.iter().any(|known| known == version) {
            return false;
        }
        match self {
            VersionSupport::All => true,
            VersionSupport::Min { min_version } => {
                parse_version_number(min_version).is_some_and(|min| number >= min)
            }
            VersionSupport::Max { max_version } => {
                parse_version_number(max_version).is_some_and(|max| number <= max)
            }
            VersionSupport::Range { min_version, max_version } => {
                let min_ok = parse_version_number(min_version).is_some_and(|min| number >= min);
                let max_ok = parse_version_number(max_version).is_some_and(|max| number <= max);
                min_ok && max_ok
            }
            VersionSupport::List { versions } => versions.iter().any(|entry| entry == version),
        }
    }

    /// Every registered version this support covers, sorted by
    /// version number. Used for `available_in` metadata.
    pub fn available_in(&self, known_versions: &[String]) -> Vec<String> {
        let mut covered: Vec<String> = known_versions
            .iter()
            .filter(|version| self.supports(version, known_versions))
            .cloned()
            .collect();
        sort_version_ids(&mut covered);
        covered
    }

    /// The lowest covered version, if any.
    pub fn lowest_version(&self, known_versions: &[String]) -> Option<String> {
        self.available_in(known_versions).into_iter().next()
    }
}

impl From<&RouteVersionSupport> for VersionSupport {
    /// Convert a module's declared route version support into the
    /// registry's `VersionSupport`. `All` stays `All`; the bounded
    /// variants map one to one, with `&'static str` bounds and
    /// version lists copied into owned strings.
    fn from(support: &RouteVersionSupport) -> Self {
        match support {
            RouteVersionSupport::All => VersionSupport::All,
            RouteVersionSupport::Min { min_version } => VersionSupport::Min {
                min_version: min_version.to_string(),
            },
            RouteVersionSupport::Max { max_version } => VersionSupport::Max {
                max_version: max_version.to_string(),
            },
            RouteVersionSupport::Range {
                min_version,
                max_version,
            } => VersionSupport::Range {
                min_version: min_version.to_string(),
                max_version: max_version.to_string(),
            },
            RouteVersionSupport::List { versions } => VersionSupport::List {
                versions: versions.iter().map(|version| version.to_string()).collect(),
            },
        }
    }
}

/// One public API version with its own settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiVersion {
    /// Registry id, `v` followed by a positive number (`v1`, `v10`).
    pub id: String,
    /// Numeric part of the id, for ordering.
    pub number: u32,
    /// Disabled versions resolve to 404 with a clear message.
    pub enabled: bool,
    /// Deprecated versions still resolve, but callers are warned
    /// and the sunset date tells them when the version goes away.
    pub deprecated: bool,
    pub sunset_date: Option<DateTime<Utc>>,
    /// Per-version rate limit, requests per minute. An endpoint
    /// without its own limit falls back to this value.
    pub rate_limit_per_minute: u32,
    /// Whether callers must authenticate for this version by default.
    pub auth_required: bool,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateApiVersionInput {
    /// Version id (`v1`, `v2`, ...). `version` is accepted as an
    /// alias; at least one of the two is required.
    pub id: Option<String>,
    pub version: Option<String>,
    /// Copy the settings of this existing version as a starting
    /// point. When omitted the version starts blank (defaults).
    pub copy_from: Option<String>,
    pub enabled: Option<bool>,
    pub deprecated: Option<bool>,
    pub sunset_date: Option<DateTime<Utc>>,
    pub rate_limit_per_minute: Option<u32>,
    pub auth_required: Option<bool>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateApiVersionInput {
    pub enabled: Option<bool>,
    pub deprecated: Option<bool>,
    pub sunset_date: Option<DateTime<Utc>>,
    pub rate_limit_per_minute: Option<u32>,
    pub auth_required: Option<bool>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeprecateVersionInput {
    /// Required: the date after which the version is removed.
    /// Must be in the future.
    pub sunset_date: DateTime<Utc>,
    pub notes: Option<String>,
}

/// Global public API prefix setting.
///
/// `prefix` is the segment placed before the version in public
/// URLs: `"v"` produces `/v1/...`, `"api/v2"` produces
/// `/api/v2/v1/...` only when combined with a version id -- in
/// practice an admin sets either a bare segment such as `"v"` or a
/// full leading path such as `"api"`. `migrate_internal` records
/// the admin's explicit choice to also remount the internal
/// `/api/v1/<module>` routes under the new prefix; it is a flag
/// only -- the remount itself happens in the server factory on the
/// next startup (see the module header comment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiPrefixSettings {
    pub prefix: String,
    pub migrate_internal: bool,
    pub updated_at: DateTime<Utc>,
}

impl Default for ApiPrefixSettings {
    fn default() -> Self {
        Self {
            prefix: "v".to_string(),
            migrate_internal: false,
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdatePrefixInput {
    pub prefix: String,
    pub migrate_internal: Option<bool>,
}

/// One built-in endpoint collected from a module's `route_info()`.
///
/// The owning module declares its version support through
/// `Module::route_version_support` (read by `collect_from_catalog`
/// per owner); a module that declares nothing registers with
/// `VersionSupport::All`. `rate_limit` is `None` here, which means
/// "fall back to the owning version's rate limit".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredEndpoint {
    /// Stable key: `"<METHOD> <path template>"`.
    pub id: String,
    pub method: String,
    /// Path template as declared, for example `/api/v1/vcs/providers/{provider}`.
    pub path: String,
    pub owner_module: String,
    pub description: String,
    pub auth_required: bool,
    pub rate_limit: Option<u32>,
    pub source: String,
    pub version_support: VersionSupport,
    pub registered_at: DateTime<Utc>,
}

/// One row of the per-version endpoint registry: a single endpoint
/// as exposed in a single version.
#[derive(Debug, Clone, Serialize)]
pub struct RegistryRow {
    /// `"<METHOD> <path template> @ <version>"`.
    pub id: String,
    pub method: String,
    pub path: String,
    /// Public URL for this version under the current prefix.
    pub public_path: String,
    pub version: String,
    pub version_enabled: bool,
    pub version_deprecated: bool,
    pub owner_module: String,
    pub description: String,
    pub auth_required: bool,
    /// Effective rate limit: the endpoint's own limit, or the
    /// owning version's per-version limit when the endpoint has none.
    pub rate_limit: u32,
    pub source: String,
    pub version_support: VersionSupport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_endpoint_id: Option<Uuid>,
}

/// Query for resolving or inspecting one endpoint.
#[derive(Debug, Deserialize)]
pub struct ResolveQuery {
    pub method: Option<String>,
    pub version: Option<String>,
    pub path: Option<String>,
}

/// The version-choice shape the admin UI creation form consumes.
#[derive(Debug, Clone, Serialize)]
pub struct VersionChoice {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub deprecated: bool,
    pub sunset_date: Option<DateTime<Utc>>,
}

/// A successful endpoint resolution.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedEndpoint {
    pub endpoint: RegistryRow,
    pub version: ApiVersion,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated_warning: Option<String>,
}

// ---------------------------------------------------------------------------
// Public access control (default deny)
// ---------------------------------------------------------------------------

/// Maximum number of access-audit entries kept in memory. The
/// migration (000004, `api_access_audit`) persists the same rows;
/// this bound only caps the in-memory trail.
pub(crate) const MAX_ACCESS_AUDIT_ENTRIES: usize = 1000;

/// Header carrying a presented API key (`X-Api-Key`, the established
/// convention across the codebase).
pub(crate) const API_KEY_HEADER: &str = "x-api-key";

/// Maximum number of per-minute rate-limit counter entries kept in
/// `ApiManagerState::rate_counters`. Counters are never evicted by a
/// timer; this bound is the eviction trigger instead (see
/// `ApiManagerState::prune_rate_counters`): once the map grows past
/// it, entries from finished minutes are dropped. Configurable
/// default: raise it only when the sustained distinct-subject rate
/// genuinely needs more room; the per-minute windows stay fixed.
pub(crate) const MAX_RATE_COUNTER_ENTRIES: usize = 10_000;

/// One admin-managed rule governing public access to a single
/// endpoint template, optionally restricted to one API version
/// (`version: None` means every version). Persisted by migration
/// 000004 as `api_access_rules`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiAccessRule {
    pub id: Uuid,
    pub method: String,
    /// Full internal path template, for example
    /// `/api/v1/store/products/{id}`.
    pub path: String,
    /// `None` means the rule applies to every version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `false` denies the endpoint outright, even with a valid key.
    pub allowed: bool,
    /// When `true` the endpoint may be called without a key (still
    /// subject to the allow rule and the anonymous rate limit).
    /// When `false` a valid, in-scope key is required.
    pub public_access: bool,
    /// Who made the last change (admin identity or `"admin"`).
    pub actor: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One entry of the access-control audit trail. Persisted by
/// migration 000004 as `api_access_audit`. Actions: `rule_created`,
/// `rule_updated`, `rule_deleted`, `key_scopes_updated`,
/// `key_rate_limit_updated`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiAccessAuditEntry {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    /// What changed, for example `GET /api/v1/store/products@v1`
    /// for a rule or the key id for a key change.
    pub target: String,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

/// Input for creating an access rule. Creating a rule is itself the
/// explicit allow: omitting `allowed` allows the endpoint.
#[derive(Debug, Deserialize)]
pub struct CreateAccessRuleInput {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "default_true")]
    pub allowed: bool,
    #[serde(default)]
    pub public_access: bool,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Input for updating an access rule. Only the fields present are changed.
#[derive(Debug, Deserialize)]
pub struct UpdateAccessRuleInput {
    #[serde(default)]
    pub allowed: Option<bool>,
    #[serde(default)]
    pub public_access: Option<bool>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Input for updating one key's access: scopes and/or rate limit.
/// An omitted field is left unchanged.
#[derive(Debug, Deserialize)]
pub struct UpdateKeyAccessInput {
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    #[serde(default)]
    pub rate_tier: Option<String>,
    #[serde(default)]
    pub requests_per_minute: Option<u32>,
    #[serde(default)]
    pub actor: Option<String>,
}

/// One row of `api_access_rules`, read with a raw query (the table
/// has no Diesel schema entry, so the typed DSL cannot address it).
/// `detail` is read as text because Diesel here has no
/// `serde_json` feature, so a `Value` cannot be loaded from a
/// `jsonb` column directly; the text is parsed afterwards.
#[derive(Debug, QueryableByName)]
pub(crate) struct AccessRuleRow {
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    pub(crate) id: Uuid,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub(crate) method: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub(crate) path: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    pub(crate) version: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    pub(crate) allowed: bool,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    pub(crate) public_access: bool,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub(crate) actor: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    pub(crate) reason: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub(crate) created_at: DateTime<Utc>,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub(crate) updated_at: DateTime<Utc>,
}

/// One row of `api_access_audit`, read with a raw query. `detail`
/// is the `jsonb` column read as text and parsed afterwards.
#[derive(Debug, QueryableByName)]
pub(crate) struct AccessAuditRow {
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    pub(crate) id: Uuid,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub(crate) actor: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub(crate) action: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub(crate) target: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub(crate) detail: String,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub(crate) created_at: DateTime<Utc>,
}

/// Seeded access rules: `(method, path template, version,
/// public_access, source)`. `allowed` is always `true` for a seed —
/// the seed only opens the endpoint to the gate the notes describe;
/// the module behind the endpoint still enforces its own
/// authentication, capability, and tenant checks.
///
/// `public_access = true` means the endpoint may be called without
/// an API key (the module verifies its own credential, if any: a
/// public token, an HMAC signature, a signed link, or a license
/// key). `public_access = false` means a valid in-scope API key (or
/// a session, where the gate allows it) is required in addition to
/// the module's own checks.
///
/// Sources, per the integration notes:
/// - batch 2: the five events public routes (`public_access: true`).
/// - batch 4: the public routes of affiliates, portals, forms,
///   recruitment, community, surveys, vault, and workflows
///   (`public_access: true`).
/// - batch 5: the sharing public link, the logistics tracking
///   route, and the four properties public routes
///   (`public_access: true`).
/// - batch 6: the web3 public ledger (`public_access: true`).
/// - batch 7: the six licensing public-facing endpoints. The
///   verify and git-access endpoints take an API key
///   (`public_access: false`); offline-verify, the git webhook,
///   and the release download/file endpoints carry their own
///   credential (signed offline token, provider HMAC, license key
///   + instance, HMAC signed link), so they are keyless at this
///   gate (`public_access: true`).
/// - batch 9: the whitelabel public branding route
///   (`public_access: true`).
/// - batch 10: the e-learning certificate verification route, the
///   four appointments public routes, and the three digital
///   storefront reads plus the sample download
///   (`public_access: true`).
/// - batch 15: the 20 entitlements routes and the 58 pricing
///   routes, all keyed (`public_access: false`).
/// - batch 16: the 12 calendar routes and the 27 holidays routes,
///   all keyed (`public_access: false`). The calendar public feed
///   `GET /api/v1/calendar/feed/{token}` is deliberately NOT
///   seeded: it is token-gated, not key-gated.
/// - Cards (batch 18) have no public endpoints, so no card rows
///   are seeded here.
pub const DEFAULT_ACCESS_RULES: &[(&str, &str, &str, bool, &str)] = &[
    // Batch 2 — events public routes.
    ("GET", "/api/v1/events/public", "v1", true, "batch-2 events public"),
    ("GET", "/api/v1/events/public/{slug}", "v1", true, "batch-2 events public"),
    ("GET", "/api/v1/events/public/{slug}/ics", "v1", true, "batch-2 events public"),
    ("GET", "/api/v1/events/public/calendar.ics", "v1", true, "batch-2 events public"),
    ("GET", "/api/v1/events/public/calendar.json", "v1", true, "batch-2 events public"),
    // Batch 4 — public routes.
    ("POST", "/api/v1/affiliates/track", "v1", true, "batch-4 affiliates public"),
    ("GET", "/api/v1/portals/public/{slug}", "v1", true, "batch-4 portals public"),
    ("GET", "/api/v1/forms/public/{slug}", "v1", true, "batch-4 forms public"),
    ("POST", "/api/v1/forms/public/{slug}/submit", "v1", true, "batch-4 forms public"),
    ("GET", "/api/v1/recruitment/public/jobs", "v1", true, "batch-4 recruitment public"),
    ("GET", "/api/v1/recruitment/public/jobs/{id}", "v1", true, "batch-4 recruitment public"),
    ("POST", "/api/v1/recruitment/public/jobs/{id}/apply", "v1", true, "batch-4 recruitment public"),
    ("GET", "/api/v1/community/public/topics", "v1", true, "batch-4 community public"),
    ("GET", "/api/v1/community/public/topics/{id}", "v1", true, "batch-4 community public"),
    ("GET", "/api/v1/surveys/public/{token}", "v1", true, "batch-4 surveys public"),
    ("POST", "/api/v1/surveys/public/{token}", "v1", true, "batch-4 surveys public"),
    ("GET", "/api/v1/vault/shared/{token}", "v1", true, "batch-4 vault public"),
    ("POST", "/api/v1/workflows/hooks/{token}", "v1", true, "batch-4 workflows public"),
    // Batch 5 — sharing, logistics tracking, properties public.
    ("GET", "/api/v1/sharing/public/{token}", "v1", true, "batch-5 sharing public"),
    ("GET", "/api/v1/logistics/track/{tracking_number}", "v1", true, "batch-5 logistics public"),
    ("GET", "/api/v1/properties/public", "v1", true, "batch-5 properties public"),
    ("GET", "/api/v1/properties/public/{slug}", "v1", true, "batch-5 properties public"),
    ("GET", "/api/v1/properties/public/{slug}/plans/{plan_id}/svg", "v1", true, "batch-5 properties public"),
    ("GET", "/api/v1/properties/public/{slug}/plans/{plan_id}/print", "v1", true, "batch-5 properties public"),
    // Batch 6 — web3 public ledger.
    ("GET", "/api/v1/web3/public/ledger", "v1", true, "batch-6 web3 public"),
    // Batch 7 — licensing public-facing endpoints.
    ("POST", "/api/v1/licensing/public/verify", "v1", false, "batch-7 licensing public"),
    ("POST", "/api/v1/licensing/offline-verify", "v1", true, "batch-7 licensing public"),
    ("GET", "/api/v1/licensing/distribute/git/{product}/access", "v1", false, "batch-7 licensing public"),
    ("POST", "/api/v1/licensing/distribute/git/{product}/webhook", "v1", true, "batch-7 licensing public"),
    ("GET", "/api/v1/licensing/distribute/releases/{release_id}/download", "v1", true, "batch-7 licensing public"),
    ("GET", "/api/v1/licensing/distribute/releases/{release_id}/file", "v1", true, "batch-7 licensing public"),
    // Batch 9 — whitelabel public branding.
    ("GET", "/api/v1/whitelabel/public/branding/{company_id}", "v1", true, "batch-9 whitelabel public"),
    // Batch 10 — e-learning certificate verification, appointments
    // public routes, digital storefront reads + sample.
    ("GET", "/api/v1/elearning/certificates/verify/{code}", "v1", true, "batch-10 elearning public"),
    ("GET", "/api/v1/appointments/public/services", "v1", true, "batch-10 appointments public"),
    ("GET", "/api/v1/appointments/public/services/{id}/slots", "v1", true, "batch-10 appointments public"),
    ("POST", "/api/v1/appointments/public/book", "v1", true, "batch-10 appointments public"),
    ("GET", "/api/v1/appointments/public/bookings/{code}", "v1", true, "batch-10 appointments public"),
    ("GET", "/api/v1/store/digital", "v1", true, "batch-10 digital storefront"),
    ("GET", "/api/v1/store/digital/{id}", "v1", true, "batch-10 digital storefront"),
    ("GET", "/api/v1/store/digital/{id}/previews", "v1", true, "batch-10 digital storefront"),
    ("GET", "/api/v1/store/digital/{id}/sample", "v1", true, "batch-10 digital storefront"),
    // Batch 15 — entitlements routes (all keyed).
    ("GET", "/api/v1/entitlements/my-modules", "v1", false, "batch-15 entitlements"),
    ("GET", "/api/v1/entitlements/modules/{module_id}/access", "v1", false, "batch-15 entitlements"),
    ("POST", "/api/v1/entitlements/modules/{module_id}/enable", "v1", false, "batch-15 entitlements"),
    ("POST", "/api/v1/entitlements/modules/{module_id}/disable", "v1", false, "batch-15 entitlements"),
    ("POST", "/api/v1/entitlements/package-changes/apply", "v1", false, "batch-15 entitlements"),
    ("GET", "/api/v1/entitlements/package-changes", "v1", false, "batch-15 entitlements"),
    ("GET", "/api/v1/entitlements/package-changes/{id}", "v1", false, "batch-15 entitlements"),
    ("POST", "/api/v1/entitlements/package-changes/{id}/confirm", "v1", false, "batch-15 entitlements"),
    ("POST", "/api/v1/entitlements/ai/suggest", "v1", false, "batch-15 entitlements"),
    ("GET", "/api/v1/entitlements/ai/capabilities", "v1", false, "batch-15 entitlements"),
    ("GET", "/api/v1/entitlements/history", "v1", false, "batch-15 entitlements"),
    ("GET", "/api/v1/entitlements/denied-attempts", "v1", false, "batch-15 entitlements"),
    ("GET", "/api/v1/entitlements/settings", "v1", false, "batch-15 entitlements"),
    ("PUT", "/api/v1/entitlements/settings", "v1", false, "batch-15 entitlements"),
    ("POST", "/api/v1/entitlements/sweep", "v1", false, "batch-15 entitlements"),
    ("POST", "/api/v1/entitlements/grants", "v1", false, "batch-15 entitlements"),
    ("GET", "/api/v1/entitlements", "v1", false, "batch-15 entitlements"),
    ("GET", "/api/v1/entitlements/{id}", "v1", false, "batch-15 entitlements"),
    ("PATCH", "/api/v1/entitlements/{id}", "v1", false, "batch-15 entitlements"),
    ("DELETE", "/api/v1/entitlements/{id}", "v1", false, "batch-15 entitlements"),
    // Batch 15 — pricing routes (all keyed).
    ("POST", "/api/v1/pricing/recommendations/cost-plus", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/recommendations/tiered", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/recommendations/bundle", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/recommendations", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/recommendations/{id}", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/recommendations/{id}/confirm", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/recommendations/{id}/narrative", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/rules", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/rules", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/rules/{id}", "v1", false, "batch-15 pricing"),
    ("PUT", "/api/v1/pricing/rules/{id}", "v1", false, "batch-15 pricing"),
    ("DELETE", "/api/v1/pricing/rules/{id}", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/coupons", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/coupons", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/coupons/{id}", "v1", false, "batch-15 pricing"),
    ("PUT", "/api/v1/pricing/coupons/{id}", "v1", false, "batch-15 pricing"),
    ("DELETE", "/api/v1/pricing/coupons/{id}", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/coupons/{id}/redeem", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/coupons/{id}/redemptions", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/discounts/evaluate", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/reference-prices", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/reference-prices", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/reference-prices/summary", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/reference-prices/{id}", "v1", false, "batch-15 pricing"),
    ("DELETE", "/api/v1/pricing/reference-prices/{id}", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/price-versions", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/price-versions", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/price-versions/{id}", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/price-versions/{id}/submit", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/price-versions/{id}/approve", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/price-versions/{id}/reject", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/price-versions/{id}/activate", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/price-versions/{id}/approvals", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/tiers", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/tiers", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/tiers/price", "v1", false, "batch-15 pricing"),
    ("DELETE", "/api/v1/pricing/tiers/{id}", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/bundles", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/bundles", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/bundles/{id}", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/bundles/{id}/price", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/proration/compute", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/proration", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/proration/{id}", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/invoices/preview", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/invoices/reconcile", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/reconciliations", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/reconciliations/{id}", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/credit-notes", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/credit-notes", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/credit-notes/{id}", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/credit-notes/{id}/reconcile", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/settings", "v1", false, "batch-15 pricing"),
    ("PUT", "/api/v1/pricing/settings", "v1", false, "batch-15 pricing"),
    ("GET", "/api/v1/pricing/currencies", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/currencies", "v1", false, "batch-15 pricing"),
    ("PUT", "/api/v1/pricing/currencies/{code}", "v1", false, "batch-15 pricing"),
    ("POST", "/api/v1/pricing/convert", "v1", false, "batch-15 pricing"),
    // Batch 16 — calendar routes (all keyed; the public iCal feed
    // is token-gated and is deliberately not seeded).
    ("GET", "/api/v1/calendar", "v1", false, "batch-16 calendar"),
    ("GET", "/api/v1/calendar/items", "v1", false, "batch-16 calendar"),
    ("POST", "/api/v1/calendar/create-intent", "v1", false, "batch-16 calendar"),
    ("GET", "/api/v1/calendar/settings", "v1", false, "batch-16 calendar"),
    ("PUT", "/api/v1/calendar/settings", "v1", false, "batch-16 calendar"),
    ("GET", "/api/v1/calendar/sources", "v1", false, "batch-16 calendar"),
    ("PUT", "/api/v1/calendar/sources", "v1", false, "batch-16 calendar"),
    ("POST", "/api/v1/calendar/feeds", "v1", false, "batch-16 calendar"),
    ("GET", "/api/v1/calendar/feeds", "v1", false, "batch-16 calendar"),
    ("DELETE", "/api/v1/calendar/feeds/{id}", "v1", false, "batch-16 calendar"),
    ("POST", "/api/v1/calendar/ai/summarize-day", "v1", false, "batch-16 calendar"),
    ("POST", "/api/v1/calendar/ai/whats-next", "v1", false, "batch-16 calendar"),
    // Batch 16 — holidays routes (all keyed).
    ("GET", "/api/v1/holidays/providers", "v1", false, "batch-16 holidays"),
    ("POST", "/api/v1/holidays/providers", "v1", false, "batch-16 holidays"),
    ("GET", "/api/v1/holidays/providers/{id}", "v1", false, "batch-16 holidays"),
    ("PATCH", "/api/v1/holidays/providers/{id}", "v1", false, "batch-16 holidays"),
    ("DELETE", "/api/v1/holidays/providers/{id}", "v1", false, "batch-16 holidays"),
    ("POST", "/api/v1/holidays/providers/{id}/test", "v1", false, "batch-16 holidays"),
    ("GET", "/api/v1/holidays/countries", "v1", false, "batch-16 holidays"),
    ("POST", "/api/v1/holidays/countries", "v1", false, "batch-16 holidays"),
    ("PATCH", "/api/v1/holidays/countries/{code}", "v1", false, "batch-16 holidays"),
    ("DELETE", "/api/v1/holidays/countries/{code}", "v1", false, "batch-16 holidays"),
    ("GET", "/api/v1/holidays/configs", "v1", false, "batch-16 holidays"),
    ("POST", "/api/v1/holidays/configs", "v1", false, "batch-16 holidays"),
    ("GET", "/api/v1/holidays/configs/{id}", "v1", false, "batch-16 holidays"),
    ("PATCH", "/api/v1/holidays/configs/{id}", "v1", false, "batch-16 holidays"),
    ("DELETE", "/api/v1/holidays/configs/{id}", "v1", false, "batch-16 holidays"),
    ("POST", "/api/v1/holidays/configs/{id}/sync", "v1", false, "batch-16 holidays"),
    ("POST", "/api/v1/holidays/sync-all", "v1", false, "batch-16 holidays"),
    ("GET", "/api/v1/holidays/sync-runs", "v1", false, "batch-16 holidays"),
    ("GET", "/api/v1/holidays/sync-runs/{id}", "v1", false, "batch-16 holidays"),
    ("GET", "/api/v1/holidays", "v1", false, "batch-16 holidays"),
    ("GET", "/api/v1/holidays/is-holiday", "v1", false, "batch-16 holidays"),
    ("GET", "/api/v1/holidays/non-working-days", "v1", false, "batch-16 holidays"),
    ("GET", "/api/v1/holidays/overrides", "v1", false, "batch-16 holidays"),
    ("POST", "/api/v1/holidays/overrides", "v1", false, "batch-16 holidays"),
    ("GET", "/api/v1/holidays/overrides/{id}", "v1", false, "batch-16 holidays"),
    ("PATCH", "/api/v1/holidays/overrides/{id}", "v1", false, "batch-16 holidays"),
    ("DELETE", "/api/v1/holidays/overrides/{id}", "v1", false, "batch-16 holidays"),
];

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

pub(crate) const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];

pub(crate) const RULE_TYPES: &[&str] = &["string", "integer", "number", "boolean", "array", "object"];


#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::modules::RouteVersionSupport;

    fn known() -> Vec<String> {
        vec!["v1".to_string(), "v2".to_string(), "v10".to_string()]
    }

    #[test]
    fn supports_all_covers_known_versions() {
        assert!(VersionSupport::All.supports("v2", &known()));
    }

    #[test]
    fn supports_rejects_unknown_version() {
        assert!(!VersionSupport::All.supports("v3", &known()));
    }

    #[test]
    fn supports_rejects_non_registry_version() {
        assert!(!VersionSupport::All.supports("v1.2", &known()));
    }

    #[test]
    fn supports_min_enforces_lower_bound() {
        let support = VersionSupport::Min {
            min_version: "v2".to_string(),
        };
        assert!(!support.supports("v1", &known()));
        assert!(support.supports("v2", &known()));
        assert!(support.supports("v10", &known()));
    }

    #[test]
    fn supports_max_enforces_upper_bound() {
        let support = VersionSupport::Max {
            max_version: "v2".to_string(),
        };
        assert!(support.supports("v1", &known()));
        assert!(!support.supports("v10", &known()));
    }

    #[test]
    fn supports_list_matches_members_only() {
        let support = VersionSupport::List {
            versions: vec!["v2".to_string()],
        };
        assert!(!support.supports("v1", &known()));
        assert!(support.supports("v2", &known()));
    }

    #[test]
    fn available_in_is_sorted_numerically() {
        let support = VersionSupport::All;
        assert_eq!(support.available_in(&known()), known());
        let min = VersionSupport::Min {
            min_version: "v2".to_string(),
        };
        assert_eq!(min.available_in(&known()), vec!["v2", "v10"]);
    }

    #[test]
    fn lowest_version_is_first_covered() {
        let support = VersionSupport::Min {
            min_version: "v2".to_string(),
        };
        assert_eq!(support.lowest_version(&known()), Some("v2".to_string()));
        let none = VersionSupport::List { versions: vec![] };
        assert_eq!(none.lowest_version(&known()), None);
    }

    #[test]
    fn from_route_support_all_stays_all() {
        assert_eq!(
            VersionSupport::from(&RouteVersionSupport::All),
            VersionSupport::All
        );
    }

    #[test]
    fn from_route_support_range_maps_bounds() {
        let converted = VersionSupport::from(&RouteVersionSupport::Range {
            min_version: "v1",
            max_version: "v2",
        });
        assert_eq!(
            converted,
            VersionSupport::Range {
                min_version: "v1".to_string(),
                max_version: "v2".to_string(),
            }
        );
    }
}
