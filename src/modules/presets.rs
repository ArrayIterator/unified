// Presets + Setup Wizard module.
//
// One-click module presets per business type, plus a resumable,
// skippable setup wizard for a new tenant. Presets are DATA, not
// behaviour:
//
//   - A preset is a stored definition: a name, a business type, a
//     list of module entries (each flagged required or optional),
//     settings to apply on activation, and the wizard step sequence
//     for that preset. Admins create, clone, edit, and archive
//     presets through the endpoints below; the four built-in presets
//     are seeded as rows (see `builtin_preset_seeds`, the one place
//     that names them, read once at startup) and are then edited
//     only by cloning them.
//   - No handler, validator, or wizard step branches on a business
//     type or a preset slug. Business types come from the preset
//     definitions themselves; the wizard step list lives in the
//     stored wizard state and navigation is index-based over that
//     list, so adding a business type or reordering steps is a data
//     change, not a code change.
//   - The only hard-coded vocabulary is the closed wizard step
//     vocabulary (`WIZARD_STEP_NAMES`, same closed-set pattern as
//     `ProductCategory` in `crate::core::modules`): a step name
//     outside it is rejected. The default step sequence
//     (`DEFAULT_WIZARD_STEPS`) is a configurable default, used only
//     when a preset stores no step list of its own.
//
// Applying a preset NEVER disables a module the tenant already
// uses: it only enables and configures. Modules are enabled in
// dependency order (topological sort over the modules' declared
// dependencies, via the registry), and fail closed on a missing
// dependency. Entitlements are respected through the licensing
// module (`check_module_entitlement`): a module the tenant is not
// entitled to is reported as unavailable and is never enabled.
// Every module enabled by a preset gets a `module.enabled` event
// (which the audit module already subscribes to) plus a
// `preset.module.enabled` event, so other modules can react. The
// preset's module list itself is exposed through the
// `presets.preset.modules_requested` hook query (answered in
// `register_hooks`) and emitted on every application by
// `finish_application`, so the tenants provisioning flow and the
// Extensions Manager can resolve a `preset_id` alone into
// grantable modules.
//
// Branding and domains are NOT implemented here: the wizard stores
// the answers (company name, branding, domain) and hands them off
// through `presets.wizard.settings.completed` /
// `preset.settings.applied` events for the owning modules.
//
// Persistence: the in-memory DashMap stores below are authoritative
// at runtime, the same pattern as the other modules in this round;
// migration `000070_presets` ships the PostgreSQL tables and the
// integrator wires them into `src/db/schema.rs` /
// `src/db/models.rs` for durable persistence. The migration also
// carries the same four built-in presets as `INSERT` rows, with the
// same ids as the in-memory seeds.
//
// Capability model (same trusted-parameter pattern as the projects
// and network-tools modules: `viewer_id` / `viewer_role` arrive as
// query parameters on reads and as body fields on writes until the
// auth extractor lands; every handler still performs the check, so
// only the identity source needs to change):
//   - `admin` / `super_admin`: view, manage, apply.
//   - `operator` / `developer`: view, apply.
//   - `viewer` / `auditor`: view only.
//   - Unknown or absent roles hold nothing.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus};
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::modules::licensing::{check_module_entitlement, has_module_entitlement};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events emitted by this module. Every payload is audit-shaped:
// `actor_id`, `action`, `target_type`, `target_id`, optional
// `company_id`, and `details`, plus `recorded_at`, so the audit
// module can record them once it subscribes to these names.
// ---------------------------------------------------------------------------

/// A preset definition was created. Payload: audit shape, details
/// carry the preset slug and business type.
pub const PRESET_CREATED: &str = "preset.created";
/// A preset definition was edited. Payload: audit shape.
pub const PRESET_UPDATED: &str = "preset.updated";
/// A preset definition was archived. Payload: audit shape.
pub const PRESET_ARCHIVED: &str = "preset.archived";
/// A preset definition was cloned. Payload: audit shape, details
/// carry `source_preset_id`.
pub const PRESET_CLONED: &str = "preset.cloned";
/// A preset was applied to a tenant. Payload: audit shape, details
/// carry the application id and the per-status counts.
pub const PRESET_APPLIED: &str = "preset.applied";
/// One module was enabled by a preset application. Payload:
/// `{"preset_id", "application_id", "company_id", "module_id",
/// "actor_id", "auto_activated"}`. Emitted next to the core
/// `module.enabled` event for the same module.
pub const PRESET_MODULE_ENABLED: &str = "preset.module.enabled";
/// One module was skipped by a preset application (not entitled,
/// dependency missing, optional and toggled off, or failed).
/// Payload: `{"preset_id", "application_id", "company_id",
/// "module_id", "actor_id", "reason"}`.
pub const PRESET_MODULE_SKIPPED: &str = "preset.module.skipped";
/// A preset's settings were applied to a tenant. Payload:
/// `{"preset_id", "application_id", "company_id", "actor_id",
/// "settings"}`. The settings, company, branding, and domains
/// modules can subscribe to receive the handoff.
pub const PRESET_SETTINGS_APPLIED: &str = "preset.settings.applied";
/// The module list of one preset, as a hook query AND as the
/// apply-time handoff. Request payload: `{"preset_id"}` (or
/// `{"preset_slug"}`). Answer result: `{"preset_id",
/// "preset_slug", "module_ids": ["<module id>", ...] (stored
/// order), "required_module_ids": [...]}`; an unknown preset (or
/// a request without an id) answers with empty lists plus an
/// `"error"`. Tenants provisioning asks this event to resolve a
/// `preset_id` alone into grantable modules (the same shape as
/// `saas_packages.package.modules_requested`, which carries
/// `{"package_id"}` -> `{"module_ids": [...]}`); until this
/// answerer existed, a preset id alone granted no modules. On
/// every preset application `finish_application` also EMITS this
/// event with `{"preset_id", "preset_slug", "application_id",
/// "company_id", "actor_id", "module_ids": [...]}` where
/// `module_ids` is the set the application enabled (or found
/// already enabled), in preset order, including transitive
/// dependencies pulled in by the apply — so the Extensions
/// Manager / entitlements / tenants grant flow can enable them
/// without re-reading the preset.
pub const PRESET_MODULES_REQUESTED: &str = "presets.preset.modules_requested";
/// A setup wizard was started. Payload: audit shape.
pub const WIZARD_STARTED: &str = "presets.wizard.started";
/// A wizard step was saved. Payload: audit shape, details carry the
/// step name and which fields were stored.
pub const WIZARD_STEP_SAVED: &str = "presets.wizard.step.saved";
/// A wizard step was skipped. Payload: audit shape, details carry
/// the skipped step name.
pub const WIZARD_SKIPPED: &str = "presets.wizard.step.skipped";
/// A wizard was resumed. Payload: audit shape.
pub const WIZARD_RESUMED: &str = "presets.wizard.resumed";
/// A wizard was restarted. Payload: audit shape.
pub const WIZARD_RESTARTED: &str = "presets.wizard.restarted";
/// A wizard was completed. Payload: audit shape, details carry the
/// application id and the done summary.
pub const WIZARD_COMPLETED: &str = "presets.wizard.completed";
/// A wizard's settings step was completed. Payload:
/// `{"company_id", "actor_id", "company_name", "branding",
/// "domain"}`. The branding and domains modules own those answers;
/// this module only stores and hands them off.
pub const WIZARD_SETTINGS_COMPLETED: &str = "presets.wizard.settings.completed";

// ---------------------------------------------------------------------------
// Wizard steps.
//
// Closed vocabulary of step names (see the module header: the same
// closed-set pattern as `ProductCategory`). A preset may store any
// ordered subset of these names as its step list; the wizard stores
// that list and navigates by index, so step order is data.
// ---------------------------------------------------------------------------

/// Every step name a preset or wizard may use.
pub const WIZARD_STEP_NAMES: &[&str] = &[
    "business_type",
    "review_modules",
    "apply",
    "settings",
    "done",
];

/// Configurable default step sequence, used only when a preset
/// stores no step list of its own. Presets created or cloned through
/// the API may store a different sequence.
pub const DEFAULT_WIZARD_STEPS: &[&str] = &[
    "business_type",
    "review_modules",
    "apply",
    "settings",
    "done",
];

/// Outcome status of one module inside a preset application.
pub const OUTCOME_ENABLED: &str = "enabled";
/// The module was already enabled for the tenant before this apply.
pub const OUTCOME_ALREADY_ENABLED: &str = "already_enabled";
/// The tenant holds no entitlement for a sellable module.
pub const OUTCOME_SKIPPED_NOT_ENTITLED: &str = "skipped_not_entitled";
/// A declared dependency is not registered, or the dependency graph
/// has a cycle.
pub const OUTCOME_SKIPPED_DEPENDENCY_MISSING: &str = "skipped_dependency_missing";
/// An optional module the caller toggled off in the review step.
pub const OUTCOME_SKIPPED_OPTIONAL_OFF: &str = "skipped_optional_off";
/// Enabling failed for another reason (see `reason`).
pub const OUTCOME_FAILED: &str = "failed";

// ---------------------------------------------------------------------------
// Stored records.
// ---------------------------------------------------------------------------

/// One module entry inside a preset definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetModuleEntry {
    pub module_id: String,
    pub required: bool,
    pub sort_order: i32,
}

/// A preset definition. `business_type` is a free-form key taken
/// from the data (for example `"freelancer"`); nothing in this
/// module branches on its value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub business_type: String,
    pub built_in: bool,
    pub archived: bool,
    pub modules: Vec<PresetModuleEntry>,
    /// Settings applied to the tenant on activation (a JSON object).
    pub settings: Value,
    /// Wizard step sequence for this preset, in order. Empty means
    /// "use the default sequence" at wizard start.
    pub steps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One module outcome inside a preset application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleApplyOutcome {
    pub module_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Dependencies enabled as part of enabling this module.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_activated: Vec<String>,
}

/// The recorded result of applying one preset to one tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetApplication {
    pub id: Uuid,
    pub preset_id: Uuid,
    pub preset_slug: String,
    pub company_id: Uuid,
    pub actor_id: String,
    /// `"completed"` (nothing failed or was skipped for a dependency
    /// reason), `"partial"` (at least one module was skipped or
    /// failed), or `"failed"` (no module could be enabled).
    pub status: String,
    pub outcomes: Vec<ModuleApplyOutcome>,
    /// The settings object actually applied (preset settings merged
    /// with the caller-supplied overrides).
    pub settings_applied: Value,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

/// Setup wizard state for one tenant. `steps` is the stored step
/// sequence (copied from the chosen preset, or the default);
/// navigation is by position in that list, never by matching a
/// business-type name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WizardState {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<Uuid>,
    pub steps: Vec<String>,
    pub current_step: String,
    /// `"in_progress"` or `"completed"`.
    pub status: String,
    /// Steps the caller skipped, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_steps: Vec<String>,
    /// Optional modules the caller toggled off in the review step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_optional_modules: Vec<String>,
    /// Answers from the settings step (company name, branding,
    /// domain), stored as given and handed off via events.
    pub settings: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The done summary returned by the wizard complete endpoint and
/// stored on the wizard: what was enabled, what was skipped, and
/// what needs attention.
#[derive(Debug, Clone, Serialize)]
pub struct WizardSummary {
    pub wizard_id: Uuid,
    pub company_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_id: Option<Uuid>,
    pub enabled: Vec<String>,
    pub already_enabled: Vec<String>,
    pub skipped_not_entitled: Vec<String>,
    pub skipped_dependency_missing: Vec<String>,
    pub skipped_optional_off: Vec<String>,
    pub failed: Vec<String>,
    /// Everything that needs attention: not-entitled, missing
    /// dependency, and failed modules, with reasons.
    pub needs_attention: Vec<ModuleApplyOutcome>,
}

// ---------------------------------------------------------------------------
// In-memory stores (authoritative at runtime; see the module header).
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PresetsState {
    pub presets: Arc<DashMap<Uuid, Preset>>,
    /// Wizard state keyed by tenant (company id): one wizard per
    /// tenant, resumable across requests.
    pub wizards: Arc<DashMap<Uuid, WizardState>>,
    pub applications: Arc<DashMap<Uuid, PresetApplication>>,
    /// Modules a preset application enabled, keyed
    /// `"<company_id>|<module_id>"`. This is this module's own
    /// record of what it enabled; modules enabled through the
    /// Extensions Manager are not visible here (see OPEN notes).
    pub tenant_modules: Arc<DashMap<String, bool>>,
}

impl PresetsState {
    pub fn new() -> Self {
        Self {
            presets: Arc::new(DashMap::new()),
            wizards: Arc::new(DashMap::new()),
            applications: Arc::new(DashMap::new()),
            tenant_modules: Arc::new(DashMap::new()),
        }
    }

    fn tenant_module_key(company_id: &Uuid, module_id: &str) -> String {
        format!("{company_id}|{module_id}")
    }

    pub fn tenant_has_module(&self, company_id: &Uuid, module_id: &str) -> bool {
        self.tenant_modules
            .get(&Self::tenant_module_key(company_id, module_id))
            .map(|entry| *entry.value())
            .unwrap_or(false)
    }

    pub fn mark_tenant_module(&self, company_id: &Uuid, module_id: &str) {
        self.tenant_modules
            .insert(Self::tenant_module_key(company_id, module_id), true);
    }
}

impl Default for PresetsState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct PresetsModule {
    state: PresetsState,
}

impl PresetsModule {
    pub fn new() -> Self {
        Self {
            state: PresetsState::new(),
        }
    }
}

impl Default for PresetsModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PresetsModule::new()));
}

// ---------------------------------------------------------------------------
// SEED DATA — the four built-in presets as initial rows.
//
// This table is read exactly once, at startup (`init`), to insert
// rows into the preset store, and is never read again: every other
// line of this module reads presets from the store only. The same
// four rows, with the same ids, are the `INSERT` statements in
// migration `000070_presets`, so the database and the in-memory
// store agree. Editing a built-in preset happens by cloning it
// through the API; the seeded rows themselves are never special
// after startup.
//
// Every module id below was verified against the registered module
// list in `src/modules/mod.rs` (the modules the registry actually
// knows). Module ids named in the original request that are NOT
// registered there were dropped, and are listed here so the gap is
// visible instead of silent:
//   - "properties" and "web3" (Property Seller): files exist on
//     disk with those ids, but neither is registered in `mod.rs`,
//     so the registry cannot enable them.
//   - "timetracker", "todos", and "invoices" (Freelancer):
//     "timetracker" and "todos" exist as files but are not
//     registered in `mod.rs`; no "invoices" module exists at all,
//     and "billing" is the real billing/invoice module. The
//     registered "tasks" module stands in as the optional
//     task-tracking module for freelancers.
//   - "portals" and "workflows" (Agency): files exist on disk but
//     are not registered in `mod.rs`.
//   - "network_tools" (SaaS Seller): the file exists on disk but
//     is not registered in `mod.rs`; the registered "network"
//     module is a different module and was not substituted.
// Once those modules are registered in `mod.rs`, an admin can add
// them to any preset (or a clone) through the API — no code change.
// ---------------------------------------------------------------------------

/// Fixed ids for the seeded presets, shared with the migration
/// `INSERT`s so the database rows and the in-memory rows match.
pub const SEED_PROPERTY_SELLER_ID: &str = "10000000-0000-4000-8000-000000000001";
pub const SEED_FREELANCER_ID: &str = "10000000-0000-4000-8000-000000000002";
pub const SEED_AGENCY_ID: &str = "10000000-0000-4000-8000-000000000003";
pub const SEED_SAAS_SELLER_ID: &str = "10000000-0000-4000-8000-000000000004";

fn seed_preset(
    id: &str,
    slug: &str,
    name: &str,
    description: &str,
    business_type: &str,
    modules: &[(&str, bool)],
) -> Preset {
    let now = Utc::now();
    Preset {
        id: Uuid::parse_str(id).expect("seed preset id is a valid UUID"),
        slug: slug.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        business_type: business_type.to_string(),
        built_in: true,
        archived: false,
        modules: modules
            .iter()
            .enumerate()
            .map(|(index, (module_id, required))| PresetModuleEntry {
                module_id: (*module_id).to_string(),
                required: *required,
                sort_order: index as i32,
            })
            .collect(),
        settings: Value::Object(Map::new()),
        steps: DEFAULT_WIZARD_STEPS
            .iter()
            .map(|step| (*step).to_string())
            .collect(),
        created_by: None,
        created_at: now,
        updated_at: now,
    }
}

/// The built-in presets as seed rows. See the block comment above.
fn builtin_preset_seeds() -> Vec<Preset> {
    vec![
        seed_preset(
            SEED_PROPERTY_SELLER_ID,
            "property-seller",
            "Property Seller",
            "For businesses selling property: customer relations, billing, and notifications.",
            "property_seller",
            &[("crm", true), ("billing", true), ("notifications", false)],
        ),
        seed_preset(
            SEED_FREELANCER_ID,
            "freelancer",
            "Freelancer",
            "For independent professionals: projects, billing, and task tracking.",
            "freelancer",
            &[("projects", true), ("billing", true), ("tasks", false)],
        ),
        seed_preset(
            SEED_AGENCY_ID,
            "agency",
            "Agency",
            "For agencies: projects, customer relations, and support tickets.",
            "agency",
            &[("projects", true), ("crm", true), ("tickets", false)],
        ),
        seed_preset(
            SEED_SAAS_SELLER_ID,
            "saas-seller",
            "SaaS Seller",
            "For software sellers: store, licensing, billing, and the public API.",
            "saas_seller",
            &[
                ("store", true),
                ("licensing", true),
                ("billing", true),
                ("api_manager", false),
            ],
        ),
    ]
}

// ---------------------------------------------------------------------------
// Viewer identity and capability checks (same trusted-parameter
// pattern as the projects and network-tools modules; see the module
// header).
// ---------------------------------------------------------------------------

/// Capabilities granted to each built-in role. `admin` and
/// `super_admin` hold everything; `operator` and `developer` may
/// view and apply presets and use the wizard but may not manage
/// preset definitions; `viewer` and `auditor` are read-only.
/// Unknown or absent roles hold nothing.
fn role_capabilities(role: &str) -> &'static [&'static str] {
    match role {
        "admin" | "super_admin" => &["presets:view", "presets:manage", "presets:apply"],
        "operator" | "developer" => &["presets:view", "presets:apply"],
        "viewer" | "auditor" => &["presets:view"],
        _ => &[],
    }
}

fn has_capability(role: Option<&str>, capability: &str) -> bool {
    match role {
        Some(role) => role_capabilities(role).contains(&capability),
        None => false,
    }
}

fn require_capability(role: Option<&str>, capability: &str) -> UnifiedResult<()> {
    if has_capability(role, capability) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role lacks required capability: {capability}"
        )))
    }
}

/// Caller identity carried on write request bodies.
#[derive(Debug, Deserialize)]
struct ActorInput {
    #[serde(default)]
    viewer_id: Option<String>,
    #[serde(default)]
    viewer_role: Option<String>,
    #[serde(default)]
    company_id: Option<String>,
}

impl ActorInput {
    fn viewer_id(&self) -> String {
        self.viewer_id
            .clone()
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn viewer_role(&self) -> Option<String> {
        self.viewer_role
            .clone()
            .map(|role| role.trim().to_string())
            .filter(|role| !role.is_empty())
    }
}

/// Resolve the requester's identity from list query parameters.
/// Returns (viewer id, viewer role, company id); any side may be
/// absent.
fn viewer_of(params: &PaginationParams) -> (Option<String>, Option<String>, Option<String>) {
    let pick = |key: &str| {
        params
            .filters
            .get(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    (pick("viewer_id"), pick("viewer_role"), pick("company_id"))
}

fn parse_company_id(value: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(value.trim()).map_err(|_| {
        UnifiedError::bad_request(format!("Invalid company id \"{value}\" (expected a UUID)"))
    })
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Emit one audit-shaped event on the hook bus: the audit module
/// records events by name, so the payload carries the actor, the
/// action, and the target in a fixed shape.
fn emit_audit(
    ctx: &AppContext,
    event: &str,
    actor_id: &str,
    action: &str,
    target_type: &str,
    target_id: &str,
    company_id: Option<Uuid>,
    details: Value,
) {
    let mut payload = serde_json::json!({
        "actor_id": actor_id,
        "action": action,
        "target_type": target_type,
        "target_id": target_id,
        "details": details,
        "recorded_at": Utc::now(),
    });
    if let Some(company_id) = company_id {
        payload["company_id"] = serde_json::json!(company_id);
    }
    ctx.hooks.emit(event, payload);
}

/// Turn a name into a URL-safe slug: lowercase, runs of other
/// characters become one hyphen, leading/trailing hyphens dropped.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_hyphen = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_hyphen = false;
        } else if !last_hyphen {
            slug.push('-');
            last_hyphen = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 100
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
        && slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn valid_business_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
}

/// Validate a wizard step list from data: non-empty, at most ten
/// steps, every name from the closed step vocabulary, no repeats.
fn validate_steps(steps: &[String]) -> UnifiedResult<()> {
    if steps.is_empty() {
        return Err(UnifiedError::bad_request(
            "A wizard step list must contain at least one step",
        ));
    }
    if steps.len() > 10 {
        return Err(UnifiedError::bad_request(
            "A wizard step list must contain at most ten steps",
        ));
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for step in steps {
        if !WIZARD_STEP_NAMES.contains(&step.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown wizard step \"{step}\" (expected one of: {})",
                WIZARD_STEP_NAMES.join(", ")
            )));
        }
        if !seen.insert(step.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate wizard step \"{step}\""
            )));
        }
    }
    Ok(())
}

/// Validate a preset module list from data against the live
/// registry: at least one entry, every module id registered, no
/// duplicate module id. Entries are returned sorted by sort order.
fn validate_preset_modules(
    ctx: &AppContext,
    entries: &[PresetModuleEntryInput],
) -> UnifiedResult<Vec<PresetModuleEntry>> {
    if entries.is_empty() {
        return Err(UnifiedError::bad_request(
            "A preset must list at least one module",
        ));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut validated: Vec<PresetModuleEntry> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let module_id = entry.module_id.trim().to_string();
        if module_id.is_empty() {
            return Err(UnifiedError::bad_request(
                "A preset module entry must name a module id",
            ));
        }
        if ctx.modules.get(&module_id).is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Unknown module \"{module_id}\": no module with that id is registered"
            ))
            .with_metadata("module_id", serde_json::json!(module_id)));
        }
        if !seen.insert(module_id.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate module \"{module_id}\" in the preset module list"
            )));
        }
        validated.push(PresetModuleEntry {
            module_id,
            required: entry.required,
            sort_order: entry.sort_order.unwrap_or(index as i32),
        });
    }
    validated.sort_by_key(|entry| entry.sort_order);
    Ok(validated)
}

/// Validate the optional-modules toggle list for one preset:
/// every toggled-off module must be an optional module of the
/// preset; required modules are locked on and rejecting them is a
/// 400, not a silent skip.
fn validate_disabled_optional(
    preset: &Preset,
    disabled: &[String],
) -> UnifiedResult<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::new();
    for module_id in disabled {
        let module_id = module_id.trim().to_string();
        match preset.modules.iter().find(|entry| entry.module_id == module_id) {
            Some(entry) if entry.required => {
                return Err(UnifiedError::bad_request(format!(
                    "Required module \"{module_id}\" cannot be toggled off"
                )));
            }
            Some(_) => {
                if !cleaned.contains(&module_id) {
                    cleaned.push(module_id);
                }
            }
            None => {
                return Err(UnifiedError::bad_request(format!(
                    "Module \"{module_id}\" is not part of preset \"{}\"",
                    preset.slug
                )));
            }
        }
    }
    Ok(cleaned)
}

/// Read-only entitlement answer for preview purposes: mirrors the
/// rule in `check_module_entitlement` without emitting its granted
/// / denied events (a preview must not leave an audit trail of a
/// decision that never happened). Non-sellable modules and sellable
/// core modules need no entitlement; sellable non-core modules need
/// an active license for the company.
fn entitlement_available(ctx: &AppContext, company_id: Option<Uuid>, module_id: &str) -> bool {
    let Some(module) = ctx.modules.get(module_id) else {
        return false;
    };
    if !module.available_as_product() || module.is_core() {
        return true;
    }
    match company_id {
        Some(company_id) => has_module_entitlement(ctx, &company_id.to_string(), module_id),
        None => false,
    }
}

/// Resolve the full activation order for a set of module ids:
/// every module's transitive requirements first (deepest first),
/// then the module itself, de-duplicated with order preserved.
/// Fails closed with the registry's error (missing dependency or
/// cycle) when any module's graph is broken.
fn resolve_apply_order(ctx: &AppContext, module_ids: &[String]) -> UnifiedResult<Vec<String>> {
    let mut ordered: Vec<String> = Vec::new();
    for module_id in module_ids {
        for required in ctx.modules.transitive_requires_ordered(module_id)? {
            if !ordered.contains(&required) {
                ordered.push(required);
            }
        }
        if !ordered.contains(module_id) {
            ordered.push(module_id.clone());
        }
    }
    Ok(ordered)
}

/// Merge an override settings object over a base settings object
/// (top-level keys only; the override wins per key). Both sides
/// must be objects; a non-object override is rejected by the
/// caller before this runs.
fn merge_settings(base: &Value, override_value: &Value) -> Value {
    let mut merged = match base {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    if let Value::Object(overrides) = override_value {
        for (key, value) in overrides {
            merged.insert(key.clone(), value.clone());
        }
    }
    Value::Object(merged)
}

// ---------------------------------------------------------------------------
// Applying a preset.
//
// Only ever enables and configures: nothing here disables a module,
// and modules the tenant already uses are reported as
// `already_enabled`, never touched. A module the tenant is not
// entitled to is reported as `skipped_not_entitled` and never
// enabled; a module whose dependency graph is broken is reported
// as `skipped_dependency_missing` with the missing ids in `reason`.
// ---------------------------------------------------------------------------

/// Apply `preset` to `company_id`. `disabled_optional` lists the
/// optional modules the caller toggled off (already validated);
/// `settings_override` is merged over the preset's settings.
/// Returns the recorded application.
fn apply_preset(
    ctx: &AppContext,
    state: &PresetsState,
    preset: &Preset,
    company_id: Uuid,
    actor_id: &str,
    disabled_optional: &[String],
    settings_override: Option<&Value>,
) -> UnifiedResult<PresetApplication> {
    let now = Utc::now();
    let settings_applied = match settings_override {
        Some(overrides) => merge_settings(&preset.settings, overrides),
        None => preset.settings.clone(),
    };
    // Effective module list, in preset order: required modules plus
    // optional modules the caller left on. Optional modules toggled
    // off are recorded as skipped, not silently dropped.
    let mut effective: Vec<PresetModuleEntry> = Vec::new();
    let mut outcomes: Vec<ModuleApplyOutcome> = Vec::new();
    for entry in &preset.modules {
        if !entry.required && disabled_optional.contains(&entry.module_id) {
            outcomes.push(ModuleApplyOutcome {
                module_id: entry.module_id.clone(),
                status: OUTCOME_SKIPPED_OPTIONAL_OFF.to_string(),
                reason: Some("Optional module toggled off in the review step".to_string()),
                auto_activated: Vec::new(),
            });
            continue;
        }
        effective.push(entry.clone());
    }
    let effective_ids: Vec<String> = effective
        .iter()
        .map(|entry| entry.module_id.clone())
        .collect();
    // Dependency order for the whole set. A broken graph fails
    // closed for the affected module only: the error names the
    // missing module(s), so each module whose own requirements
    // cannot be resolved is skipped with the reason, while the
    // others still apply.
    let ordered: Vec<String> = match resolve_apply_order(ctx, &effective_ids) {
        Ok(ordered) => ordered,
        Err(error) => {
            let missing: Vec<String> = error
                .metadata
                .get("missing_required")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            for entry in &effective {
                outcomes.push(ModuleApplyOutcome {
                    module_id: entry.module_id.clone(),
                    status: OUTCOME_SKIPPED_DEPENDENCY_MISSING.to_string(),
                    reason: Some(if missing.is_empty() {
                        error.message.clone()
                    } else {
                        format!("Missing dependency module(s): {}", missing.join(", "))
                    }),
                    auto_activated: Vec::new(),
                });
            }
            return finish_application(
                ctx,
                state,
                preset,
                company_id,
                actor_id,
                outcomes,
                settings_applied,
                now,
            );
        }
    };
    // Modules in `ordered` that are not in the preset are transitive
    // dependencies pulled in by a preset module; they are enabled
    // with it and listed as auto-activated on that module's outcome.
    let preset_ids: HashSet<&str> = effective_ids.iter().map(|id| id.as_str()).collect();
    let mut enabled_before: HashSet<String> = ordered
        .iter()
        .filter(|id| state.tenant_has_module(&company_id, id))
        .cloned()
        .collect();
    for module_id in &ordered {
        let in_preset = preset_ids.contains(module_id.as_str());
        if state.tenant_has_module(&company_id, module_id) {
            if in_preset {
                outcomes.push(ModuleApplyOutcome {
                    module_id: module_id.clone(),
                    status: OUTCOME_ALREADY_ENABLED.to_string(),
                    reason: None,
                    auto_activated: Vec::new(),
                });
            }
            continue;
        }
        // Entitlement gate: a sellable module without an active
        // entitlement for this tenant is never enabled. The check
        // itself emits the granted / denied events.
        if let Err(error) = check_module_entitlement(ctx, &company_id.to_string(), module_id) {
            if error.code == 404 {
                outcomes.push(ModuleApplyOutcome {
                    module_id: module_id.clone(),
                    status: OUTCOME_FAILED.to_string(),
                    reason: Some(error.message.clone()),
                    auto_activated: Vec::new(),
                });
            } else {
                outcomes.push(ModuleApplyOutcome {
                    module_id: module_id.clone(),
                    status: OUTCOME_SKIPPED_NOT_ENTITLED.to_string(),
                    reason: Some(
                        "No active entitlement for this module for this tenant".to_string(),
                    ),
                    auto_activated: Vec::new(),
                });
                ctx.hooks.emit(
                    PRESET_MODULE_SKIPPED,
                    serde_json::json!({
                        "preset_id": preset.id,
                        "company_id": company_id,
                        "module_id": module_id,
                        "actor_id": actor_id,
                        "reason": "no_active_entitlement",
                    }),
                );
            }
            continue;
        }
        // Dependencies enabled in this run for this module: its
        // transitive requirements that were not enabled before.
        let auto_activated: Vec<String> = ctx
            .modules
            .transitive_requires_ordered(module_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|required| !enabled_before.contains(required))
            .collect();
        for required in &auto_activated {
            state.mark_tenant_module(&company_id, required);
            ctx.hooks.emit(
                events::MODULE_ENABLED,
                serde_json::json!({
                    "id": required,
                    "company_id": company_id,
                    "preset_id": preset.id,
                    "auto_activated": true,
                    "auto_activated_by": module_id,
                }),
            );
        }
        state.mark_tenant_module(&company_id, module_id);
        enabled_before.insert(module_id.clone());
        ctx.hooks.emit(
            events::MODULE_ENABLED,
            serde_json::json!({
                "id": module_id,
                "company_id": company_id,
                "preset_id": preset.id,
                "actor_id": actor_id,
            }),
        );
        ctx.hooks.emit(
            PRESET_MODULE_ENABLED,
            serde_json::json!({
                "preset_id": preset.id,
                "company_id": company_id,
                "module_id": module_id,
                "actor_id": actor_id,
                "auto_activated": auto_activated,
            }),
        );
        if in_preset {
            outcomes.push(ModuleApplyOutcome {
                module_id: module_id.clone(),
                status: OUTCOME_ENABLED.to_string(),
                reason: None,
                auto_activated,
            });
        }
    }
    // Keep outcomes in preset order (dependencies report through
    // their dependent's `auto_activated` list).
    outcomes.sort_by_key(|outcome| {
        preset
            .modules
            .iter()
            .position(|entry| entry.module_id == outcome.module_id)
            .unwrap_or(usize::MAX)
    });
    finish_application(
        ctx,
        state,
        preset,
        company_id,
        actor_id,
        outcomes,
        settings_applied,
        now,
    )
}

/// Record one application, apply its settings handoff event, and
/// emit the audit-shaped `preset.applied` event.
#[allow(clippy::too_many_arguments)]
fn finish_application(
    ctx: &AppContext,
    state: &PresetsState,
    preset: &Preset,
    company_id: Uuid,
    actor_id: &str,
    outcomes: Vec<ModuleApplyOutcome>,
    settings_applied: Value,
    started_at: DateTime<Utc>,
) -> UnifiedResult<PresetApplication> {
    let enabled_count = outcomes
        .iter()
        .filter(|outcome| outcome.status == OUTCOME_ENABLED)
        .count();
    let failed_count = outcomes
        .iter()
        .filter(|outcome| outcome.status == OUTCOME_FAILED)
        .count();
    let blocked_count = outcomes
        .iter()
        .filter(|outcome| {
            outcome.status == OUTCOME_SKIPPED_DEPENDENCY_MISSING
                || outcome.status == OUTCOME_SKIPPED_NOT_ENTITLED
        })
        .count();
    let status = if failed_count == outcomes.len() && !outcomes.is_empty() {
        "failed".to_string()
    } else if failed_count > 0 || blocked_count > 0 {
        "partial".to_string()
    } else {
        "completed".to_string()
    };
    let application = PresetApplication {
        id: Uuid::now_v7(),
        preset_id: preset.id,
        preset_slug: preset.slug.clone(),
        company_id,
        actor_id: actor_id.to_string(),
        status: status.clone(),
        outcomes,
        settings_applied: settings_applied.clone(),
        created_at: started_at,
        completed_at: Some(Utc::now()),
    };
    state
        .applications
        .insert(application.id, application.clone());
    // Settings handoff: the preset's settings (merged with the
    // caller's overrides) go out as an event for the owning modules
    // instead of being applied here.
    ctx.hooks.emit(
        PRESET_SETTINGS_APPLIED,
        serde_json::json!({
            "preset_id": preset.id,
            "application_id": application.id,
            "company_id": company_id,
            "actor_id": actor_id,
            "settings": settings_applied,
        }),
    );
    // Module-list handoff: the set this application enabled (or
    // found already enabled), in preset order, including the
    // transitive dependencies recorded on each outcome's
    // `auto_activated` list. Emitted on the same
    // `presets.preset.modules_requested` event the tenants
    // provisioning flow queries, so the Extensions Manager /
    // entitlements / tenants grant flow can enable these modules
    // without re-reading the preset. Skipped and failed modules
    // are never listed here.
    let mut module_ids: Vec<String> = Vec::new();
    for outcome in &outcomes {
        if outcome.status == OUTCOME_ENABLED || outcome.status == OUTCOME_ALREADY_ENABLED {
            if !module_ids.contains(&outcome.module_id) {
                module_ids.push(outcome.module_id.clone());
            }
            for dependency in &outcome.auto_activated {
                if !module_ids.contains(dependency) {
                    module_ids.push(dependency.clone());
                }
            }
        }
    }
    ctx.hooks.emit(
        PRESET_MODULES_REQUESTED,
        serde_json::json!({
            "preset_id": preset.id,
            "preset_slug": preset.slug,
            "application_id": application.id,
            "company_id": company_id,
            "actor_id": actor_id,
            "module_ids": module_ids,
        }),
    );
    emit_audit(
        ctx,
        PRESET_APPLIED,
        actor_id,
        "preset.applied",
        "preset",
        &preset.id.to_string(),
        Some(company_id),
        serde_json::json!({
            "preset_slug": preset.slug,
            "application_id": application.id,
            "status": status,
            "enabled": enabled_count,
            "failed": failed_count,
            "blocked": blocked_count,
        }),
    );
    Ok(application)
}

/// Build the done summary for a wizard from its application.
fn summary_for(application: &PresetApplication, wizard: &WizardState) -> WizardSummary {
    let pick = |status: &str| {
        application
            .outcomes
            .iter()
            .filter(|outcome| outcome.status == status)
            .map(|outcome| outcome.module_id.clone())
            .collect()
    };
    let needs_attention: Vec<ModuleApplyOutcome> = application
        .outcomes
        .iter()
        .filter(|outcome| {
            outcome.status == OUTCOME_SKIPPED_NOT_ENTITLED
                || outcome.status == OUTCOME_SKIPPED_DEPENDENCY_MISSING
                || outcome.status == OUTCOME_FAILED
        })
        .cloned()
        .collect();
    WizardSummary {
        wizard_id: wizard.id,
        company_id: wizard.company_id,
        preset_id: wizard.preset_id,
        application_id: Some(application.id),
        enabled: pick(OUTCOME_ENABLED),
        already_enabled: pick(OUTCOME_ALREADY_ENABLED),
        skipped_not_entitled: pick(OUTCOME_SKIPPED_NOT_ENTITLED),
        skipped_dependency_missing: pick(OUTCOME_SKIPPED_DEPENDENCY_MISSING),
        skipped_optional_off: pick(OUTCOME_SKIPPED_OPTIONAL_OFF),
        failed: pick(OUTCOME_FAILED),
        needs_attention,
    }
}

// ---------------------------------------------------------------------------
// Request bodies.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PresetModuleEntryInput {
    module_id: String,
    #[serde(default = "default_required")]
    required: bool,
    #[serde(default)]
    sort_order: Option<i32>,
}

fn default_required() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct CreatePresetRequest {
    #[serde(flatten)]
    actor: ActorInput,
    name: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    description: Option<String>,
    business_type: String,
    modules: Vec<PresetModuleEntryInput>,
    #[serde(default)]
    settings: Option<Value>,
    #[serde(default)]
    steps: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct UpdatePresetRequest {
    #[serde(flatten)]
    actor: ActorInput,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    business_type: Option<String>,
    #[serde(default)]
    modules: Option<Vec<PresetModuleEntryInput>>,
    #[serde(default)]
    settings: Option<Value>,
    #[serde(default)]
    steps: Option<Vec<String>>,
    #[serde(default)]
    archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ClonePresetRequest {
    #[serde(flatten)]
    actor: ActorInput,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    slug: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApplyPresetRequest {
    #[serde(flatten)]
    actor: ActorInput,
    company_id: Uuid,
    #[serde(default)]
    disabled_optional_modules: Option<Vec<String>>,
    #[serde(default)]
    settings: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct StartWizardRequest {
    #[serde(flatten)]
    actor: ActorInput,
    company_id: Uuid,
    #[serde(default)]
    preset_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct SaveStepRequest {
    #[serde(flatten)]
    actor: ActorInput,
    #[serde(default)]
    step: Option<String>,
    #[serde(default)]
    preset_id: Option<Uuid>,
    #[serde(default)]
    disabled_optional_modules: Option<Vec<String>>,
    #[serde(default)]
    settings: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct WizardActionRequest {
    #[serde(flatten)]
    actor: ActorInput,
    #[serde(default)]
    preset_id: Option<Uuid>,
    #[serde(default)]
    step: Option<String>,
}

/// One module in the preview: its stored entry plus the live
/// availability answer for the tenant.
#[derive(Debug, Clone, Serialize)]
pub struct PresetPreviewModule {
    pub module_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub required: bool,
    /// `"ok"`, `"already_enabled"`, `"not_entitled"`, or `"missing"`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Transitive requirements, in activation order.
    pub will_auto_activate: Vec<String>,
    /// Required module ids that are not registered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_required: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresetPreview {
    pub preset: Preset,
    pub company_id: Option<Uuid>,
    pub modules: Vec<PresetPreviewModule>,
    /// Activation order for the modules the preview would enable.
    pub apply_order: Vec<String>,
}

// ---------------------------------------------------------------------------
// Shared lookups.
// ---------------------------------------------------------------------------

/// Find a preset by UUID or by slug. A slug lookup never collides
/// with a UUID because slugs may not contain the characters a UUID
/// needs in the same shape; the UUID parse is tried first.
fn find_preset(state: &PresetsState, key: &str) -> Option<Preset> {
    if let Ok(id) = Uuid::parse_str(key) {
        if let Some(preset) = state.presets.get(&id) {
            return Some(preset.value().clone());
        }
    }
    state
        .presets
        .iter()
        .find(|entry| entry.value().slug == key)
        .map(|entry| entry.value().clone())
}

fn slug_in_use(state: &PresetsState, slug: &str, except: Option<Uuid>) -> bool {
    state.presets.iter().any(|entry| {
        entry.value().slug == slug && Some(entry.value().id) != except
    })
}

/// Advance one step in the stored step list. Returns `None` when
/// `current` is the last step (or is not in the list at all).
fn next_step(steps: &[String], current: &str) -> Option<String> {
    steps
        .iter()
        .position(|step| step == current)
        .and_then(|index| steps.get(index + 1).cloned())
}

/// Run the wizard's preset application once, and remember it on the
/// wizard. A second call returns the stored application instead of
/// applying twice.
fn ensure_wizard_applied(
    ctx: &AppContext,
    state: &PresetsState,
    wizard: &mut WizardState,
) -> UnifiedResult<PresetApplication> {
    if let Some(application_id) = wizard.application_id {
        if let Some(application) = state.applications.get(&application_id) {
            return Ok(application.value().clone());
        }
    }
    let Some(preset_id) = wizard.preset_id else {
        return Err(UnifiedError::bad_request(
            "The wizard has no business type chosen yet: save the business_type step first",
        ));
    };
    let Some(preset) = state.presets.get(&preset_id).map(|entry| entry.value().clone()) else {
        return Err(UnifiedError::not_found(format!(
            "Preset \"{preset_id}\" not found"
        )));
    };
    if preset.archived {
        return Err(UnifiedError::conflict(format!(
            "Preset \"{}\" is archived and cannot be applied",
            preset.slug
        )));
    }
    let application = apply_preset(
        ctx,
        state,
        &preset,
        wizard.company_id,
        &wizard.actor_id.clone(),
        &wizard.disabled_optional_modules.clone(),
        Some(&wizard.settings.clone()),
    )?;
    wizard.application_id = Some(application.id);
    wizard.updated_at = now();
    state.wizards.insert(wizard.company_id, wizard.clone());
    Ok(application)
}

// ---------------------------------------------------------------------------
// Preset handlers.
// ---------------------------------------------------------------------------

fn list_presets(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_viewer_id, viewer_role, _company_id) = viewer_of(&query);
    if let Err(error) = require_capability(viewer_role.as_deref(), "presets:view") {
        return error_response(&error, debug);
    }
    let pagination = query.normalized();
    let built_in_filter = pagination
        .filters
        .get("built_in")
        .and_then(|value| value.parse::<bool>().ok());
    let archived_filter = pagination
        .filters
        .get("archived")
        .and_then(|value| value.parse::<bool>().ok());
    let business_type_filter = pagination.filters.get("business_type").cloned();
    let mut items: Vec<Preset> = state
        .presets
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|preset| {
            // Archived presets are hidden by default; pass
            // `?archived=true` to list them.
            if archived_filter.is_none() && preset.archived {
                return false;
            }
            if let Some(archived) = archived_filter {
                if preset.archived != archived {
                    return false;
                }
            }
            if let Some(built_in) = built_in_filter {
                if preset.built_in != built_in {
                    return false;
                }
            }
            if let Some(business_type) = &business_type_filter {
                if &preset.business_type != business_type {
                    return false;
                }
            }
            let haystack = format!(
                "{} {} {} {}",
                preset.slug, preset.name, preset.description, preset.business_type
            );
            matches_search(&haystack, &query)
        })
        .collect();
    items.sort_by(|a, b| a.slug.cmp(&b.slug));
    success_response(paginate(items, &query).to_json(), 200)
}

fn create_preset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    body: web::Json<CreatePresetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    if let Err(error) = require_capability(request.actor.viewer_role().as_deref(), "presets:manage")
    {
        return error_response(&error, debug);
    }
    let name = request.name.trim().to_string();
    if name.is_empty() || name.len() > 200 {
        return error_response(
            &UnifiedError::bad_request("Preset name is required (at most 200 characters)"),
            debug,
        );
    }
    let slug = match request.slug {
        Some(slug) => slug.trim().to_lowercase(),
        None => slugify(&name),
    };
    if !valid_slug(&slug) {
        return error_response(
            &UnifiedError::bad_request(
                "Invalid preset slug (lowercase letters, digits, and single hyphens only)",
            ),
            debug,
        );
    }
    if slug_in_use(&state, &slug, None) {
        return error_response(
            &UnifiedError::conflict(format!("A preset with slug \"{slug}\" already exists")),
            debug,
        );
    }
    let business_type = request.business_type.trim().to_string();
    if !valid_business_type(&business_type) {
        return error_response(
            &UnifiedError::bad_request(
                "Invalid business type (lowercase letters, digits, underscores, and hyphens only)",
            ),
            debug,
        );
    }
    let modules = match validate_preset_modules(&ctx, &request.modules) {
        Ok(modules) => modules,
        Err(error) => return error_response(&error, debug),
    };
    let settings = match request.settings {
        Some(Value::Object(_)) | None => request.settings.unwrap_or(Value::Object(Map::new())),
        Some(_) => {
            return error_response(
                &UnifiedError::bad_request("Preset settings must be a JSON object"),
                debug,
            );
        }
    };
    let steps = match request.steps {
        Some(steps) => {
            if let Err(error) = validate_steps(&steps) {
                return error_response(&error, debug);
            }
            steps
        }
        None => DEFAULT_WIZARD_STEPS
            .iter()
            .map(|step| (*step).to_string())
            .collect(),
    };
    let description = request
        .description
        .unwrap_or_default()
        .trim()
        .to_string();
    if description.len() > 2000 {
        return error_response(
            &UnifiedError::bad_request("Preset description is too long (at most 2000 characters)"),
            debug,
        );
    }
    let actor_id = request.actor.viewer_id();
    let now = now();
    let preset = Preset {
        id: Uuid::now_v7(),
        slug: slug.clone(),
        name: name.clone(),
        description,
        business_type: business_type.clone(),
        built_in: false,
        archived: false,
        modules,
        settings,
        steps,
        created_by: if actor_id.is_empty() {
            None
        } else {
            Some(actor_id.clone())
        },
        created_at: now,
        updated_at: now,
    };
    state.presets.insert(preset.id, preset.clone());
    emit_audit(
        &ctx,
        PRESET_CREATED,
        &actor_id,
        "preset.created",
        "preset",
        &preset.id.to_string(),
        None,
        serde_json::json!({ "slug": slug, "business_type": business_type }),
    );
    success_response(preset, 201)
}

fn get_preset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_viewer_id, viewer_role, _company_id) = viewer_of(&query);
    if let Err(error) = require_capability(viewer_role.as_deref(), "presets:view") {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    match find_preset(&state, &key) {
        Some(preset) => success_response(preset, 200),
        None => error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        ),
    }
}

fn update_preset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    body: web::Json<UpdatePresetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    if let Err(error) = require_capability(request.actor.viewer_role().as_deref(), "presets:manage")
    {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    let Some(mut preset) = find_preset(&state, &key) else {
        return error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        );
    };
    // Built-in presets are edited by cloning them, never in place,
    // so the seeded definitions stay intact.
    if preset.built_in {
        return error_response(
            &UnifiedError::conflict(format!(
                "Built-in preset \"{}\" cannot be edited directly; clone it and edit the copy",
                preset.slug
            ))
            .with_metadata("hint", serde_json::json!("POST /api/v1/presets/{id}/clone")),
            debug,
        );
    }
    if let Some(name) = request.name {
        let name = name.trim().to_string();
        if name.is_empty() || name.len() > 200 {
            return error_response(
                &UnifiedError::bad_request("Preset name is required (at most 200 characters)"),
                debug,
            );
        }
        preset.name = name;
    }
    if let Some(slug) = request.slug {
        let slug = slug.trim().to_lowercase();
        if !valid_slug(&slug) {
            return error_response(
                &UnifiedError::bad_request(
                    "Invalid preset slug (lowercase letters, digits, and single hyphens only)",
                ),
                debug,
            );
        }
        if slug_in_use(&state, &slug, Some(preset.id)) {
            return error_response(
                &UnifiedError::conflict(format!("A preset with slug \"{slug}\" already exists")),
                debug,
            );
        }
        preset.slug = slug;
    }
    if let Some(description) = request.description {
        let description = description.trim().to_string();
        if description.len() > 2000 {
            return error_response(
                &UnifiedError::bad_request(
                    "Preset description is too long (at most 2000 characters)",
                ),
                debug,
            );
        }
        preset.description = description;
    }
    if let Some(business_type) = request.business_type {
        let business_type = business_type.trim().to_string();
        if !valid_business_type(&business_type) {
            return error_response(
                &UnifiedError::bad_request(
                    "Invalid business type (lowercase letters, digits, underscores, and hyphens only)",
                ),
                debug,
            );
        }
        preset.business_type = business_type;
    }
    if let Some(modules) = request.modules {
        match validate_preset_modules(&ctx, &modules) {
            Ok(validated) => preset.modules = validated,
            Err(error) => return error_response(&error, debug),
        }
    }
    if let Some(settings) = request.settings {
        if !settings.is_object() {
            return error_response(
                &UnifiedError::bad_request("Preset settings must be a JSON object"),
                debug,
            );
        }
        preset.settings = settings;
    }
    if let Some(steps) = request.steps {
        if let Err(error) = validate_steps(&steps) {
            return error_response(&error, debug);
        }
        preset.steps = steps;
    }
    if let Some(archived) = request.archived {
        preset.archived = archived;
    }
    preset.updated_at = now();
    let actor_id = request.actor.viewer_id();
    state.presets.insert(preset.id, preset.clone());
    emit_audit(
        &ctx,
        PRESET_UPDATED,
        &actor_id,
        "preset.updated",
        "preset",
        &preset.id.to_string(),
        None,
        serde_json::json!({ "slug": preset.slug }),
    );
    success_response(preset, 200)
}

fn clone_preset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    body: web::Json<ClonePresetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    if let Err(error) = require_capability(request.actor.viewer_role().as_deref(), "presets:manage")
    {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    let Some(source) = find_preset(&state, &key) else {
        return error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        );
    };
    let mut slug = match request.slug {
        Some(slug) => slug.trim().to_lowercase(),
        None => format!("{}-copy", source.slug),
    };
    // Keep the copy's slug unique without failing the clone: append
    // a counter until the slug is free.
    let mut counter = 2;
    while slug_in_use(&state, &slug, None) {
        slug = format!("{}-copy-{counter}", source.slug);
        counter += 1;
        if counter > 100 {
            return error_response(
                &UnifiedError::conflict("Could not find a free slug for the cloned preset"),
                debug,
            );
        }
    }
    if !valid_slug(&slug) {
        return error_response(
            &UnifiedError::bad_request(
                "Invalid preset slug (lowercase letters, digits, and single hyphens only)",
            ),
            debug,
        );
    }
    let name = match request.name {
        Some(name) => name.trim().to_string(),
        None => format!("{} (copy)", source.name),
    };
    if name.is_empty() || name.len() > 200 {
        return error_response(
            &UnifiedError::bad_request("Preset name is required (at most 200 characters)"),
            debug,
        );
    }
    let actor_id = request.actor.viewer_id();
    let now = now();
    let clone = Preset {
        id: Uuid::now_v7(),
        slug: slug.clone(),
        name,
        description: source.description.clone(),
        business_type: source.business_type.clone(),
        built_in: false,
        archived: false,
        modules: source.modules.clone(),
        settings: source.settings.clone(),
        steps: source.steps.clone(),
        created_by: if actor_id.is_empty() {
            None
        } else {
            Some(actor_id.clone())
        },
        created_at: now,
        updated_at: now,
    };
    state.presets.insert(clone.id, clone.clone());
    emit_audit(
        &ctx,
        PRESET_CLONED,
        &actor_id,
        "preset.cloned",
        "preset",
        &clone.id.to_string(),
        None,
        serde_json::json!({ "slug": slug, "source_preset_id": source.id }),
    );
    success_response(clone, 201)
}

fn archive_preset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    body: web::Json<ActorInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let actor = body.into_inner();
    if let Err(error) = require_capability(actor.viewer_role().as_deref(), "presets:manage") {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    let Some(mut preset) = find_preset(&state, &key) else {
        return error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        );
    };
    if preset.archived {
        return error_response(
            &UnifiedError::conflict(format!("Preset \"{}\" is already archived", preset.slug)),
            debug,
        );
    }
    preset.archived = true;
    preset.updated_at = now();
    let actor_id = actor.viewer_id();
    state.presets.insert(preset.id, preset.clone());
    emit_audit(
        &ctx,
        PRESET_ARCHIVED,
        &actor_id,
        "preset.archived",
        "preset",
        &preset.id.to_string(),
        None,
        serde_json::json!({ "slug": preset.slug }),
    );
    success_response(preset, 200)
}

fn apply_preset_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    body: web::Json<ApplyPresetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    if let Err(error) = require_capability(request.actor.viewer_role().as_deref(), "presets:apply")
    {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    let Some(preset) = find_preset(&state, &key) else {
        return error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        );
    };
    if preset.archived {
        return error_response(
            &UnifiedError::conflict(format!(
                "Preset \"{}\" is archived and cannot be applied",
                preset.slug
            )),
            debug,
        );
    }
    let disabled = match request.disabled_optional_modules {
        Some(disabled) => match validate_disabled_optional(&preset, &disabled) {
            Ok(cleaned) => cleaned,
            Err(error) => return error_response(&error, debug),
        },
        None => Vec::new(),
    };
    let settings_override = match &request.settings {
        Some(settings) if !settings.is_object() => {
            return error_response(
                &UnifiedError::bad_request("Settings overrides must be a JSON object"),
                debug,
            );
        }
        settings => settings.as_ref(),
    };
    let actor_id = request.actor.viewer_id();
    match apply_preset(
        &ctx,
        &state,
        &preset,
        request.company_id,
        &actor_id,
        &disabled,
        settings_override,
    ) {
        Ok(application) => success_response(application, 201),
        Err(error) => error_response(&error, debug),
    }
}

fn preview_preset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_viewer_id, viewer_role, company_id) = viewer_of(&query);
    if let Err(error) = require_capability(viewer_role.as_deref(), "presets:view") {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    let Some(preset) = find_preset(&state, &key) else {
        return error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        );
    };
    let company_id = company_id.and_then(|value| Uuid::parse_str(&value).ok());
    let mut modules: Vec<PresetPreviewModule> = Vec::new();
    for entry in &preset.modules {
        let module = ctx.modules.get(&entry.module_id);
        let (status, reason, missing_required) = match &module {
            None => (
                "missing".to_string(),
                Some(format!(
                    "Module \"{}\" is not registered",
                    entry.module_id
                )),
                vec![entry.module_id.clone()],
            ),
            Some(module) => {
                let missing: Vec<String> = ctx
                    .modules
                    .requires_of(&entry.module_id)
                    .iter()
                    .filter(|required| ctx.modules.get(required).is_none())
                    .map(|required| (*required).to_string())
                    .collect();
                if !missing.is_empty() {
                    (
                        "missing".to_string(),
                        Some(format!(
                            "Missing dependency module(s): {}",
                            missing.join(", ")
                        )),
                        missing,
                    )
                } else if state.tenant_has_module(
                    &company_id.unwrap_or(Uuid::nil()),
                    &entry.module_id,
                ) && company_id.is_some()
                {
                    ("already_enabled".to_string(), None, Vec::new())
                } else if !entitlement_available(&ctx, company_id, &entry.module_id) {
                    (
                        "not_entitled".to_string(),
                        Some(
                            "No active entitlement for this module for this tenant".to_string(),
                        ),
                        Vec::new(),
                    )
                } else {
                    ("ok".to_string(), None, Vec::new())
                }
            }
        };
        let will_auto_activate: Vec<String> = ctx
            .modules
            .transitive_requires_ordered(&entry.module_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|required| {
                !state.tenant_has_module(&company_id.unwrap_or(Uuid::nil()), required)
                    || company_id.is_none()
            })
            .collect();
        modules.push(PresetPreviewModule {
            module_id: entry.module_id.clone(),
            name: module.map(|module| module.name().to_string()),
            required: entry.required,
            status,
            reason,
            will_auto_activate,
            missing_required,
        });
    }
    let apply_order = resolve_apply_order(
        &ctx,
        &preset
            .modules
            .iter()
            .map(|entry| entry.module_id.clone())
            .collect::<Vec<String>>(),
    )
    .unwrap_or_default();
    success_response(
        PresetPreview {
            preset,
            company_id,
            modules,
            apply_order,
        },
        200,
    )
}

fn list_applications(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_viewer_id, viewer_role, _company_id) = viewer_of(&query);
    if let Err(error) = require_capability(viewer_role.as_deref(), "presets:view") {
        return error_response(&error, debug);
    }
    let pagination = query.normalized();
    let preset_filter = pagination.filters.get("preset_id").cloned();
    let company_filter = pagination.filters.get("company_id").cloned();
    let status_filter = pagination.filters.get("status").cloned();
    let mut items: Vec<PresetApplication> = state
        .applications
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|application| {
            if let Some(preset_id) = &preset_filter {
                if application.preset_id.to_string() != *preset_id
                    && application.preset_slug != *preset_id
                {
                    return false;
                }
            }
            if let Some(company_id) = &company_filter {
                if application.company_id.to_string() != *company_id {
                    return false;
                }
            }
            if let Some(status) = &status_filter {
                if &application.status != status {
                    return false;
                }
            }
            let haystack = format!("{} {}", application.preset_slug, application.actor_id);
            matches_search(&haystack, &query)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    success_response(paginate(items, &query).to_json(), 200)
}

fn get_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_viewer_id, viewer_role, _company_id) = viewer_of(&query);
    if let Err(error) = require_capability(viewer_role.as_deref(), "presets:view") {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    let Ok(application_id) = Uuid::parse_str(&key) else {
        return error_response(
            &UnifiedError::bad_request(format!("Invalid application id \"{key}\"")),
            debug,
        );
    };
    match state.applications.get(&application_id) {
        Some(application) => success_response(application.value().clone(), 200),
        None => error_response(
            &UnifiedError::not_found(format!("Preset application \"{key}\" not found")),
            debug,
        ),
    }
}

// ---------------------------------------------------------------------------
// Wizard handlers.
//
// Navigation is index-based over the step list stored on the wizard
// state: no handler matches on a business-type name. Saving a step
// merges whichever fields the payload carries (preset choice,
// optional-module toggles, settings answers) and then advances one
// stored step; the step the wizard is on only decides whether the
// preset application runs now (the stored "apply" step) or at
// completion.
// ---------------------------------------------------------------------------

fn start_wizard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    body: web::Json<StartWizardRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    if let Err(error) = require_capability(request.actor.viewer_role().as_deref(), "presets:apply")
    {
        return error_response(&error, debug);
    }
    // A wizard is resumable: starting again for the same tenant
    // returns the in-progress wizard instead of discarding it.
    if let Some(wizard) = state.wizards.get(&request.company_id) {
        if wizard.status == "in_progress" {
            return success_response(wizard.value().clone(), 200);
        }
    }
    let steps = match request.preset_id {
        Some(preset_id) => {
            match state.presets.get(&preset_id) {
                Some(preset) => {
                    if preset.archived {
                        return error_response(
                            &UnifiedError::conflict(format!(
                                "Preset \"{}\" is archived and cannot be used",
                                preset.slug
                            )),
                            debug,
                        );
                    }
                    if preset.steps.is_empty() {
                        DEFAULT_WIZARD_STEPS
                            .iter()
                            .map(|step| (*step).to_string())
                            .collect()
                    } else {
                        preset.steps.clone()
                    }
                }
                None => {
                    return error_response(
                        &UnifiedError::not_found(format!("Preset \"{preset_id}\" not found")),
                        debug,
                    );
                }
            }
        }
        None => DEFAULT_WIZARD_STEPS
            .iter()
            .map(|step| (*step).to_string())
            .collect(),
    };
    let actor_id = request.actor.viewer_id();
    let now = now();
    let wizard = WizardState {
        id: Uuid::now_v7(),
        company_id: request.company_id,
        actor_id: actor_id.clone(),
        preset_id: request.preset_id,
        steps: steps.clone(),
        current_step: steps.first().cloned().unwrap_or_else(|| "done".to_string()),
        status: "in_progress".to_string(),
        skipped_steps: Vec::new(),
        disabled_optional_modules: Vec::new(),
        settings: Value::Object(Map::new()),
        application_id: None,
        created_at: now,
        updated_at: now,
    };
    state.wizards.insert(wizard.company_id, wizard.clone());
    emit_audit(
        &ctx,
        WIZARD_STARTED,
        &actor_id,
        "presets.wizard.started",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({ "preset_id": wizard.preset_id }),
    );
    success_response(wizard, 201)
}

fn get_wizard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_viewer_id, viewer_role, _company_id) = viewer_of(&query);
    if let Err(error) = require_capability(viewer_role.as_deref(), "presets:view") {
        return error_response(&error, debug);
    }
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    match state.wizards.get(&company_id) {
        Some(wizard) => success_response(wizard.value().clone(), 200),
        None => error_response(
            &UnifiedError::not_found(format!(
                "No setup wizard found for company \"{company_id}\""
            )),
            debug,
        ),
    }
}

fn save_wizard_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    body: web::Json<SaveStepRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    if let Err(error) = require_capability(request.actor.viewer_role().as_deref(), "presets:apply")
    {
        return error_response(&error, debug);
    }
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    let Some(mut wizard) = state.wizards.get(&company_id).map(|entry| entry.value().clone())
    else {
        return error_response(
            &UnifiedError::not_found(format!(
                "No setup wizard found for company \"{company_id}\""
            )),
            debug,
        );
    };
    if wizard.status == "completed" {
        return error_response(
            &UnifiedError::conflict("The setup wizard is already completed; restart it to run it again"),
            debug,
        );
    }
    // The payload may name the step being saved; it must be the
    // wizard's current step, so steps cannot be skipped silently by
    // saving out of order (use the skip endpoint to skip).
    if let Some(step) = &request.step {
        if step != &wizard.current_step {
            return error_response(
                &UnifiedError::bad_request(format!(
                    "Step \"{step}\" is not the wizard's current step \"{}\"",
                    wizard.current_step
                )),
                debug,
            );
        }
    }
    let saved_step = wizard.current_step.clone();
    let mut changed: Vec<String> = Vec::new();
    // Business-type choice: store the preset and adopt its stored
    // step list (restarting at the first step when the current step
    // is not part of the new list).
    if let Some(preset_id) = request.preset_id {
        let Some(preset) = state.presets.get(&preset_id).map(|entry| entry.value().clone())
        else {
            return error_response(
                &UnifiedError::not_found(format!("Preset \"{preset_id}\" not found")),
                debug,
            );
        };
        if preset.archived {
            return error_response(
                &UnifiedError::conflict(format!(
                    "Preset \"{}\" is archived and cannot be used",
                    preset.slug
                )),
                debug,
            );
        }
        wizard.preset_id = Some(preset_id);
        if !preset.steps.is_empty() {
            wizard.steps = preset.steps.clone();
            if !wizard.steps.contains(&wizard.current_step) {
                wizard.current_step = wizard
                    .steps
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "done".to_string());
            }
        }
        changed.push("preset_id".to_string());
    }
    // Review step: toggle optional modules. Required modules are
    // locked on (see `validate_disabled_optional`).
    if let Some(disabled) = request.disabled_optional_modules {
        let Some(preset_id) = wizard.preset_id else {
            return error_response(
                &UnifiedError::bad_request(
                    "Choose a business type (preset) before reviewing the module list",
                ),
                debug,
            );
        };
        let Some(preset) = state.presets.get(&preset_id).map(|entry| entry.value().clone())
        else {
            return error_response(
                &UnifiedError::not_found(format!("Preset \"{preset_id}\" not found")),
                debug,
            );
        };
        match validate_disabled_optional(&preset, &disabled) {
            Ok(cleaned) => wizard.disabled_optional_modules = cleaned,
            Err(error) => return error_response(&error, debug),
        }
        changed.push("disabled_optional_modules".to_string());
    }
    // Settings answers: merged over the stored answers. The
    // company-name / branding / domain answers are handed off via
    // event at completion; they are stored here, not acted on.
    if let Some(settings) = request.settings {
        if !settings.is_object() {
            return error_response(
                &UnifiedError::bad_request("Wizard settings must be a JSON object"),
                debug,
            );
        }
        wizard.settings = merge_settings(&wizard.settings, &settings);
        changed.push("settings".to_string());
    }
    // The step the wizard is on now: choosing a preset above may
    // have adopted a new stored step list and reset it, so it is
    // read again here instead of reusing the saved step name.
    let current_step = wizard.current_step.clone();
    // The stored "apply" step runs the preset application now, so
    // the wizard can report the result before the settings step.
    if current_step == "apply" {
        match ensure_wizard_applied(&ctx, &state, &mut wizard) {
            Ok(_) => changed.push("applied".to_string()),
            Err(error) => return error_response(&error, debug),
        }
    }
    match next_step(&wizard.steps, &current_step) {
        Some(next) => wizard.current_step = next,
        None => {
            // Saving the last stored step leaves the wizard on it;
            // completion is explicit through the complete endpoint.
            changed.push("final_step".to_string());
        }
    }
    wizard.updated_at = now();
    let actor_id = request.actor.viewer_id();
    state.wizards.insert(wizard.company_id, wizard.clone());
    emit_audit(
        &ctx,
        WIZARD_STEP_SAVED,
        &actor_id,
        "presets.wizard.step.saved",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({ "step": saved_step, "changed": changed }),
    );
    success_response(wizard, 200)
}

fn skip_wizard_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    body: web::Json<WizardActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    if let Err(error) = require_capability(request.actor.viewer_role().as_deref(), "presets:apply")
    {
        return error_response(&error, debug);
    }
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    let Some(mut wizard) = state.wizards.get(&company_id).map(|entry| entry.value().clone())
    else {
        return error_response(
            &UnifiedError::not_found(format!(
                "No setup wizard found for company \"{company_id}\""
            )),
            debug,
        );
    };
    if wizard.status == "completed" {
        return error_response(
            &UnifiedError::conflict("The setup wizard is already completed; restart it to run it again"),
            debug,
        );
    }
    if let Some(step) = &request.step {
        if step != &wizard.current_step {
            return error_response(
                &UnifiedError::bad_request(format!(
                    "Step \"{step}\" is not the wizard's current step \"{}\"",
                    wizard.current_step
                )),
                debug,
            );
        }
    }
    let skipped = wizard.current_step.clone();
    wizard.skipped_steps.push(skipped.clone());
    match next_step(&wizard.steps, &skipped) {
        Some(next) => wizard.current_step = next,
        None => {
            // Skipping the last stored step finishes the wizard
            // without applying anything that was not already applied.
            wizard.status = "completed".to_string();
        }
    }
    wizard.updated_at = now();
    let actor_id = request.actor.viewer_id();
    state.wizards.insert(wizard.company_id, wizard.clone());
    emit_audit(
        &ctx,
        WIZARD_SKIPPED,
        &actor_id,
        "presets.wizard.step.skipped",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({ "step": skipped }),
    );
    success_response(wizard, 200)
}

fn resume_wizard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    body: web::Json<WizardActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    if let Err(error) = require_capability(request.actor.viewer_role().as_deref(), "presets:apply")
    {
        return error_response(&error, debug);
    }
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    let Some(wizard) = state.wizards.get(&company_id).map(|entry| entry.value().clone()) else {
        return error_response(
            &UnifiedError::not_found(format!(
                "No setup wizard found for company \"{company_id}\""
            )),
            debug,
        );
    };
    let actor_id = request.actor.viewer_id();
    emit_audit(
        &ctx,
        WIZARD_RESUMED,
        &actor_id,
        "presets.wizard.resumed",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({ "current_step": wizard.current_step, "status": wizard.status }),
    );
    success_response(wizard, 200)
}

fn restart_wizard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    body: web::Json<WizardActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    if let Err(error) = require_capability(request.actor.viewer_role().as_deref(), "presets:apply")
    {
        return error_response(&error, debug);
    }
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    let steps = match request.preset_id {
        Some(preset_id) => {
            match state.presets.get(&preset_id) {
                Some(preset) => {
                    if preset.archived {
                        return error_response(
                            &UnifiedError::conflict(format!(
                                "Preset \"{}\" is archived and cannot be used",
                                preset.slug
                            )),
                            debug,
                        );
                    }
                    if preset.steps.is_empty() {
                        DEFAULT_WIZARD_STEPS
                            .iter()
                            .map(|step| (*step).to_string())
                            .collect()
                    } else {
                        preset.steps.clone()
                    }
                }
                None => {
                    return error_response(
                        &UnifiedError::not_found(format!("Preset \"{preset_id}\" not found")),
                        debug,
                    );
                }
            }
        }
        None => DEFAULT_WIZARD_STEPS
            .iter()
            .map(|step| (*step).to_string())
            .collect(),
    };
    let actor_id = request.actor.viewer_id();
    let now = now();
    let wizard = WizardState {
        id: Uuid::now_v7(),
        company_id,
        actor_id: actor_id.clone(),
        preset_id: request.preset_id,
        steps: steps.clone(),
        current_step: steps.first().cloned().unwrap_or_else(|| "done".to_string()),
        status: "in_progress".to_string(),
        skipped_steps: Vec::new(),
        disabled_optional_modules: Vec::new(),
        settings: Value::Object(Map::new()),
        application_id: None,
        created_at: now,
        updated_at: now,
    };
    state.wizards.insert(wizard.company_id, wizard.clone());
    emit_audit(
        &ctx,
        WIZARD_RESTARTED,
        &actor_id,
        "presets.wizard.restarted",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({ "preset_id": wizard.preset_id }),
    );
    success_response(wizard, 201)
}

fn complete_wizard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    body: web::Json<WizardActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    if let Err(error) = require_capability(request.actor.viewer_role().as_deref(), "presets:apply")
    {
        return error_response(&error, debug);
    }
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    let Some(mut wizard) = state.wizards.get(&company_id).map(|entry| entry.value().clone())
    else {
        return error_response(
            &UnifiedError::not_found(format!(
                "No setup wizard found for company \"{company_id}\""
            )),
            debug,
        );
    };
    if wizard.status == "completed" {
        // Completing twice returns the same summary instead of
        // applying the preset a second time.
        if let Some(application_id) = wizard.application_id {
            if let Some(application) = state.applications.get(&application_id) {
                let summary = summary_for(application.value(), &wizard);
                return success_response(summary, 200);
            }
        }
        return error_response(
            &UnifiedError::conflict("The setup wizard is already completed"),
            debug,
        );
    }
    if wizard.preset_id.is_none() {
        return error_response(
            &UnifiedError::bad_request(
                "The wizard has no business type chosen yet: save the business_type step first",
            ),
            debug,
        );
    }
    let application = match ensure_wizard_applied(&ctx, &state, &mut wizard) {
        Ok(application) => application,
        Err(error) => return error_response(&error, debug),
    };
    // Settings handoff: the stored answers go out for the owning
    // modules (company, branding, domains) instead of being applied
    // here. The wizard never implements branding or domains itself.
    let company_name = wizard
        .settings
        .get("company_name")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let branding = wizard.settings.get("branding").cloned();
    let domain = wizard
        .settings
        .get("domain")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    ctx.hooks.emit(
        WIZARD_SETTINGS_COMPLETED,
        serde_json::json!({
            "company_id": wizard.company_id,
            "actor_id": wizard.actor_id,
            "company_name": company_name,
            "branding": branding,
            "domain": domain,
        }),
    );
    wizard.status = "completed".to_string();
    wizard.current_step = wizard
        .steps
        .last()
        .cloned()
        .unwrap_or_else(|| "done".to_string());
    wizard.updated_at = now();
    state.wizards.insert(wizard.company_id, wizard.clone());
    let summary = summary_for(&application, &wizard);
    let actor_id = request.actor.viewer_id();
    emit_audit(
        &ctx,
        WIZARD_COMPLETED,
        &actor_id,
        "presets.wizard.completed",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({
            "preset_id": wizard.preset_id,
            "application_id": application.id,
            "enabled": summary.enabled,
            "skipped_not_entitled": summary.skipped_not_entitled,
            "skipped_dependency_missing": summary.skipped_dependency_missing,
            "failed": summary.failed,
        }),
    );
    success_response(summary, 200)
}

// ---------------------------------------------------------------------------
// Module implementation.
// ---------------------------------------------------------------------------

impl Module for PresetsModule {
    fn id(&self) -> &'static str {
        "presets"
    }

    fn name(&self) -> &'static str {
        "Presets"
    }

    fn description(&self) -> &'static str {
        "Module presets and setup wizard: admin-editable preset definitions per business type (module list with required/optional flags, settings, wizard steps), one-click preset application in dependency order with entitlement gating, and a resumable, skippable setup wizard per tenant."
    }

    fn requires(&self) -> &[&'static str] {
        &["extensions", "licensing", "audit"]
    }

    fn init(&self, ctx: &AppContext) -> crate::core::error::UnifiedResult<()> {
        // Seed the built-in presets as data rows, once. All runtime
        // behaviour reads the store from here on; the seed table is
        // never consulted again.
        if self.state.presets.is_empty() {
            for preset in builtin_preset_seeds() {
                self.state.presets.insert(preset.id, preset);
            }
            log::info!(
                "Presets module seeded {} built-in presets",
                self.state.presets.len()
            );
        }
        // Warn when a seeded preset names a module the registry does
        // not know, so a registration gap is visible at startup
        // instead of failing the first apply.
        let report = ctx.modules.validate_dependencies();
        if !report.valid {
            for issue in &report.issues {
                log::warn!("Module dependency problem: {}", issue.message);
            }
        }
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Answer the preset module-list query
        // (`presets.preset.modules_requested`): the tenants
        // provisioning flow asks with `{"preset_id"}` (or
        // `{"preset_slug"}`) and reads `module_ids` from the
        // result. An unknown preset, or a request without an id,
        // answers with empty lists plus an `"error"` so the
        // caller can record the skip instead of waiting on a
        // handler that never answers.
        let state = self.state.clone();
        bus.register(
            PRESET_MODULES_REQUESTED,
            "presets",
            Arc::new(move |context: &mut crate::core::hooks::HookContext| {
                let key = context
                    .get("preset_id")
                    .and_then(Value::as_str)
                    .or_else(|| context.get("preset_slug").and_then(Value::as_str))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if key.is_empty() {
                    context.add_result(serde_json::json!({
                        "module_ids": [],
                        "required_module_ids": [],
                        "error": "presets.preset.modules_requested requires 'preset_id' (or 'preset_slug')",
                    }));
                    return;
                }
                let Some(preset) = find_preset(&state, &key) else {
                    context.add_result(serde_json::json!({
                        "preset_id": key,
                        "module_ids": [],
                        "required_module_ids": [],
                        "error": format!("Preset \"{key}\" not found"),
                    }));
                    return;
                };
                let module_ids: Vec<String> = preset
                    .modules
                    .iter()
                    .map(|entry| entry.module_id.clone())
                    .collect();
                let required_module_ids: Vec<String> = preset
                    .modules
                    .iter()
                    .filter(|entry| entry.required)
                    .map(|entry| entry.module_id.clone())
                    .collect();
                context.add_result(serde_json::json!({
                    "preset_id": preset.id,
                    "preset_slug": preset.slug,
                    "module_ids": module_ids,
                    "required_module_ids": required_module_ids,
                }));
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/presets", owner: "presets", description: "List presets (paginated, search, filter by built_in, archived, business_type)." },
            RouteInfo { method: "POST", path: "/api/v1/presets", owner: "presets", description: "Create a preset definition." },
            RouteInfo { method: "GET", path: "/api/v1/presets/applications", owner: "presets", description: "List preset applications (paginated, filter by preset_id, company_id, status)." },
            RouteInfo { method: "GET", path: "/api/v1/presets/applications/{id}", owner: "presets", description: "Get one preset application." },
            RouteInfo { method: "GET", path: "/api/v1/presets/wizard/{company_id}", owner: "presets", description: "Get the setup wizard state for a tenant." },
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/start", owner: "presets", description: "Start (or resume) the setup wizard for a tenant." },
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/{company_id}/step", owner: "presets", description: "Save the wizard's current step and advance one stored step." },
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/{company_id}/skip", owner: "presets", description: "Skip the wizard's current step and advance one stored step." },
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/{company_id}/resume", owner: "presets", description: "Resume the setup wizard for a tenant." },
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/{company_id}/restart", owner: "presets", description: "Restart the setup wizard for a tenant from the first stored step." },
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/{company_id}/complete", owner: "presets", description: "Complete the setup wizard: apply the preset if not yet applied and return the done summary." },
            RouteInfo { method: "GET", path: "/api/v1/presets/{id}", owner: "presets", description: "Get one preset by id or slug." },
            RouteInfo { method: "PATCH", path: "/api/v1/presets/{id}", owner: "presets", description: "Edit a preset definition (built-in presets are edited by cloning, not in place)." },
            RouteInfo { method: "POST", path: "/api/v1/presets/{id}/clone", owner: "presets", description: "Clone a preset definition (the way to edit a built-in preset)." },
            RouteInfo { method: "POST", path: "/api/v1/presets/{id}/archive", owner: "presets", description: "Archive a preset definition." },
            RouteInfo { method: "POST", path: "/api/v1/presets/{id}/apply", owner: "presets", description: "Apply a preset to a tenant (enables modules in dependency order, never disables)." },
            RouteInfo { method: "GET", path: "/api/v1/presets/{id}/preview", owner: "presets", description: "Preview a preset for a tenant: resolved modules, entitlement and dependency status, apply order." },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        // Static segments are registered before `/{id}` so they are
        // not captured as a preset id.
        cfg.service(
            web::scope("/api/v1/presets")
                .app_data(web::Data::new(self.state.clone()))
                .route("", web::get().to(list_presets))
                .route("", web::post().to(create_preset))
                .route("/applications", web::get().to(list_applications))
                .route("/applications/{id}", web::get().to(get_application))
                .route("/wizard/start", web::post().to(start_wizard))
                .route("/wizard/{company_id}", web::get().to(get_wizard))
                .route("/wizard/{company_id}/step", web::post().to(save_wizard_step))
                .route("/wizard/{company_id}/skip", web::post().to(skip_wizard_step))
                .route(
                    "/wizard/{company_id}/resume",
                    web::post().to(resume_wizard),
                )
                .route(
                    "/wizard/{company_id}/restart",
                    web::post().to(restart_wizard),
                )
                .route(
                    "/wizard/{company_id}/complete",
                    web::post().to(complete_wizard),
                )
                .route("/{id}", web::get().to(get_preset))
                .route("/{id}", web::patch().to(update_preset))
                .route("/{id}/clone", web::post().to(clone_preset))
                .route("/{id}/archive", web::post().to(archive_preset))
                .route("/{id}/apply", web::post().to(apply_preset_endpoint))
                .route("/{id}/preview", web::get().to(preview_preset)),
        );
    }
}
