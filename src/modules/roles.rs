// Roles module — the Role Manager.
//
// Static default roles (admin, editor, author, member, customer,
// reseller, finance) are
// seeded in memory with permission sets; dynamic roles can be created,
// updated and deleted through the API. Permissions are a JSON map of
// resource -> actions, for example `{"posts": ["create", "read"]}`.
//
// The role factory is `POST /api/v1/roles/check`: it answers whether a
// role is allowed a permission (`resource:action`, or `*` for any). The
// check is hookable: the module itself answers `role.check` hook events,
// and any other module may override the result by setting `allowed` on
// the hook payload.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus};
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use uuid::Uuid;

pub type RolesStore = Arc<DashMap<Uuid, RoleRecord>>;

pub struct RolesModule {
    store: RolesStore,
}

impl RolesModule {
    pub fn new() -> Self {
        let store: RolesStore = Arc::new(DashMap::new());
        for (name, slug, description, permissions) in default_roles() {
            let record = RoleRecord {
                id: Uuid::new_v7(),
                name: name.to_string(),
                slug: slug.to_string(),
                description: Some(description.to_string()),
                is_system: true,
                permissions,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            store.insert(record.id, record);
        }
        Self { store }
    }
}

impl Default for RolesModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(RolesModule::new()));
}

/// Every capability string checked anywhere in the codebase, as the
/// single definition list. Capability checks go through the
/// `role.check` hook and fail closed: a permission that is not
/// granted to the caller's role (and is not covered by the admin
/// role's `"*": ["*"]` wildcard) is denied, so a capability only
/// becomes usable once it is listed here AND granted to a role.
/// Nothing below grants anything to the customer, editor, author,
/// or member roles — the only non-admin role seeded with new
/// capabilities is Finance (cards, batch 18). Two naming shapes
/// coexist in the tree and both are listed verbatim as checked:
/// `resource:action` (cards, pricing, entitlements, calendar,
/// holidays, backup, tax, advisor, risk, hr, and the older
/// colon-form modules) and `resource.action` (the dot-form modules
/// such as printing, manufacturing, quality, expenses, approvals,
/// studio, and the batch-4 modules).
///
/// Known gaps, recorded honestly rather than papered over:
/// - Webinars gates on the role names `admin` / `webinars.admin`
///   plus host ownership (`is_admin` in webinars.rs), not on
///   `role.check`, so `webinars.view` / `webinars.manage` /
///   `webinars.host` are defined here per the batch-10 notes but
///   are not consulted by that module yet.
/// - Appointments checks `appointments.create`, not the
///   `appointments.book` name the batch-10 notes use; `book` is
///   therefore not listed.
/// - Academic checks the colon form (`academic:read`,
///   `academic:read_all`, `academic:manage`, `academic:advise`,
///   `academic:grade`, `academic:transcript`), not the
///   `academic:view` spelling the batch-13 notes use; `view` is
///   therefore not listed. Admissions likewise checks
///   `admissions:read` / `admissions:read_all` /
///   `admissions:manage` / `admissions:apply` /
///   `admissions:score`.
pub const KNOWN_CAPABILITIES: &[&str] = &[
    // AI and usage.
    "ai:use", "ai:approve", "ai:read_usage", "ai:manage_providers",
    // Events, time tracking, todos, products (batch 2).
    "events.view", "events.create", "events.manage", "events.checkin",
    "timetracker:read", "timetracker:write", "timetracker:approve",
    "timetracker:manage", "timetracker:manage_staff",
    "timetracker:manage_rates", "timetracker:manage_settings",
    "timetracker:manage_buyer_settings", "timetracker:view_team",
    "timetracker:view_team_screenshots", "timetracker:payroll",
    "todos:read", "todos:write", "todos:share",
    "products:read",
    // Storage Manager (batch 2).
    "storage_manager.read", "storage_manager.manage_buckets",
    "storage_manager.manage_bindings", "storage_manager.manage_quotas",
    "storage_manager.manage_signed_urls", "storage_manager.manage_plans",
    "storage_manager.manage_byo", "storage_manager.use_byo",
    "storage_manager.manage_lifecycle", "storage_manager.browse",
    "storage_manager.upload", "storage_manager.health",
    // Tax, advisor, risk, HR (batch 3).
    "tax:view", "tax:manage", "tax:file", "tax:manage_connectors",
    "advisor:view", "advisor:manage",
    "risk:view", "risk:create", "risk:edit", "risk:delete",
    "hr:read", "hr:read_all", "hr:manage", "hr:payroll",
    // Subscriptions (batch 3).
    "calculator:use", "calculator:manage",
    "marketplace:read", "marketplace:manage", "marketplace:vendor",
    "pricing_tables:read", "pricing_tables:manage",
    // Batch 4 modules: `<module>.view` + `<module>.manage`.
    "workflows.view", "workflows.manage",
    "bi.view", "bi.manage",
    "okrs.view", "okrs.manage",
    "affiliates.view", "affiliates.manage",
    "loyalty.view", "loyalty.manage",
    "portals.view", "portals.manage",
    "forms.view", "forms.manage",
    "contracts.view", "contracts.manage",
    "procurement.view", "procurement.manage",
    "assets.view", "assets.manage",
    "recruitment.view", "recruitment.manage",
    "vault.view", "vault.manage",
    "incidents.view", "incidents.manage",
    "inbox.view", "inbox.manage",
    "surveys.view", "surveys.manage",
    "community.view", "community.manage",
    "compliance.view", "compliance.manage",
    "gamification.view", "gamification.manage",
    // Auditors, sharing, supervision, logistics, properties (batch 5).
    "auditors.view", "auditors.create", "auditors.manage", "auditors.revoke",
    "sharing.view", "sharing.create", "sharing.revoke",
    "supervision.view", "supervision.create", "supervision.manage",
    "supervision.approve",
    "logistics.view", "logistics.create", "logistics.manage",
    "logistics.driver", "logistics.quote",
    "properties.view", "properties.create", "properties.manage",
    // Documents, privacy, web3 (batch 6).
    "documents:read", "documents:write", "documents:admin",
    "privacy:read", "privacy:write", "privacy:admin",
    "web3:read", "web3:write", "web3:admin",
    // Licensing and network tools (batch 7).
    "licensing:view", "licensing:manage",
    "network_tools:view", "network_tools:run", "network_tools:manage",
    "network_tools:probe_private",
    // SaaS domains, domain verification, certificates (batch 8).
    "saas_domains.read", "saas_domains.manage", "saas_domains.self_assign",
    "domain_verification.read", "domain_verification.manage",
    "domain_verification.issue", "domain_verification.verify",
    "certificates.view", "certificates.manage", "certificates.manage_own",
    // Whitelabel and presets (batch 9).
    "whitelabel.view", "whitelabel.manage", "whitelabel.branding.manage",
    "presets:view", "presets:manage", "presets:apply",
    // Webinars, e-learning, POS, appointments, marketing, field
    // service, digital products (batch 10).
    "webinars.view", "webinars.manage", "webinars.host",
    "elearning.view", "elearning.manage", "elearning.instruct",
    "pos.view", "pos.sell", "pos.manage", "pos.refund",
    "appointments.view", "appointments.manage", "appointments.create",
    "marketing.view", "marketing.manage", "marketing.send",
    "field_service.view", "field_service.manage", "field_service.technician",
    "store.digital:read", "store.digital:manage",
    // Backup, clusters, import/export, cache, storage, security
    // (batch 11). `app-sync` is the module id as checked.
    "backup:view", "backup:manage",
    "clusters:view", "clusters:manage",
    "app-sync:view", "app-sync:manage",
    "import_export:read", "import_export:import", "import_export:export",
    "import_export:export_pii", "import_export:manage",
    "cache.read", "cache.write", "cache.manage",
    "storage.read", "storage.write", "storage.manage_targets",
    "security:view", "security:manage",
    // Printing, manufacturing, quality, expenses, approvals, studio
    // (batch 12).
    "billing.template.view", "billing.template.manage", "billing.print",
    "printing.view", "printing.manage", "printing.job",
    "manufacturing.view", "manufacturing.manage", "manufacturing.produce",
    "manufacturing.admin",
    "quality.view", "quality.manage", "quality.inspect",
    "expenses.view", "expenses.submit", "expenses.manage",
    "approvals.view", "approvals.manage", "approvals.decide",
    "studio.view", "studio.customize", "studio.admin",
    // Academic, curriculum, admissions, school fees (batch 13).
    "academic:read", "academic:read_all", "academic:manage",
    "academic:advise", "academic:grade", "academic:transcript",
    "curriculum:view", "curriculum:manage", "curriculum:teach",
    "admissions:read", "admissions:read_all", "admissions:manage",
    "admissions:apply", "admissions:score",
    "school_fees.view", "school_fees.manage",
    // Exams, library, SaaS packages (batch 14).
    "exams.view", "exams.manage", "exams.take", "exams.grade",
    "library.view", "library.manage", "library.circulate", "library.catalog",
    "saas_packages.view", "saas_packages.manage", "saas_packages.purchase",
    "saas_packages.provision",
    // Entitlements, pricing, product subdomains (batch 15).
    "entitlements:read", "entitlements:manage", "entitlements:grant",
    "pricing:read", "pricing:manage", "pricing:approve",
    // Calendar and holidays (batch 16).
    "calendar:read", "calendar:manage",
    "holidays:read", "holidays:manage", "holidays:sync",
    // Tenants and tenant destroy (batch 17).
    "tenants:view", "tenants:manage", "tenants:provision", "tenants:suspend",
    "tenants:destroy", "tenants:destroy_read",
    // Cards and reference ids (batch 18).
    "cards:read", "cards:validate", "cards:manage",
    "store.reference:read", "store.reference:manage",
    "billing.reference:read", "billing.reference:manage",
    // Pre-existing capability strings checked by older modules.
    "admin:access", "admin:view_as_user", "apikeys:read", "licenses:read",
    "logs:view", "modules:manage", "orders:manage", "orders:read",
    "reseller:access", "reseller:clients.view", "reseller:clients.manage",
    "reseller:payouts.view", "servers:manage",
    "services:read", "settings:manage", "tickets:read", "tickets:view_all",
    "user:access", "users:manage", "projects:view",
];

/// True when `permission` is a capability this codebase defines
/// (see `KNOWN_CAPABILITIES`). This only answers whether the string
/// exists; whether a role holds it is still decided by
/// `permission_allowed` on that role's permission map.
pub fn capability_defined(permission: &str) -> bool {
    KNOWN_CAPABILITIES.contains(&permission)
}

/// The seven static default roles and their permission sets.
fn default_roles() -> Vec<(&'static str, &'static str, &'static str, Value)> {
    vec![
        (
            "Administrator",
            "admin",
            "Full access to every resource.",
            json!({ "*": ["*"] }),
        ),
        (
            "Editor",
            "editor",
            "Can manage content, pages, comments and media.",
            json!({
                "posts": ["create", "read", "update", "publish", "delete"],
                "pages": ["create", "read", "update", "publish", "delete"],
                "comments": ["read", "update", "delete"],
                "media": ["create", "read", "update", "delete"],
            }),
        ),
        (
            "Author",
            "author",
            "Can write and manage their own content.",
            json!({
                "posts": ["create", "read", "update"],
                "media": ["create", "read", "update"],
                "comments": ["read"],
            }),
        ),
        (
            "Member",
            "member",
            "Can read content and take part in discussions.",
            json!({
                "posts": ["read"],
                "pages": ["read"],
                "comments": ["create", "read"],
            }),
        ),
        (
            "Customer",
            "customer",
            "Can browse the store, place orders and view licenses.",
            json!({
                "store": ["read", "purchase"],
                "orders": ["create", "read"],
                "licenses": ["read"],
            }),
        ),
        (
            "Reseller",
            "reseller",
            "Can manage linked clients and view commissions and payouts.",
            json!({
                "reseller": ["access", "clients.view", "clients.manage", "payouts.view"],
                "user": ["access"],
                "orders": ["read"],
                "licenses": ["read"],
            }),
        ),
        (
            // Finance role (batch 18): the only non-admin role seeded
            // with card capabilities. Never grant `cards:validate`
            // to customer, editor, author, or member roles — an open
            // validation oracle lets anyone probe card numbers.
            "Finance",
            "finance",
            "Manages card validation settings and custom rules.",
            json!({ "cards": ["read", "validate", "manage"], "payments": ["read"] }),
        ),
    ]
}

/// One role. `permissions` is a JSON object mapping a resource name to the
/// list of actions allowed on it; `"*"` matches any resource or action.
#[derive(Debug, Clone, Serialize)]
pub struct RoleRecord {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub is_system: bool,
    pub permissions: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRoleInput {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "empty_permissions")]
    pub permissions: Value,
}

fn empty_permissions() -> Value {
    Value::Object(Map::new())
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRoleInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub permissions: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckRoleInput {
    /// Role id (UUID) or slug.
    pub role: String,
    /// Permission to check, as `resource:action` (or `*` for any).
    pub permission: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckRoleResponse {
    pub role: String,
    pub permission: String,
    pub allowed: bool,
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Validate a permissions value: it must be an object mapping resource
/// names to arrays of action strings.
fn validate_permissions(permissions: &Value) -> UnifiedResult<()> {
    let map = match permissions.as_object() {
        Some(map) => map,
        None => {
            return Err(UnifiedError::bad_request(
                "Permissions must be a JSON object mapping resource names to action lists",
            ))
        }
    };
    for (resource, actions) in map {
        if resource.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Permission resource names must not be empty",
            ));
        }
        let actions = match actions.as_array() {
            Some(actions) => actions,
            None => {
                return Err(UnifiedError::bad_request(format!(
                    "Permissions for resource \"{resource}\" must be an array of actions"
                )))
            }
        };
        for action in actions {
            if action.as_str().is_none_or(|s| s.trim().is_empty()) {
                return Err(UnifiedError::bad_request(format!(
                    "Actions for resource \"{resource}\" must be non-empty strings"
                )));
            }
        }
    }
    Ok(())
}

/// Resolve a role by id (UUID) or slug. Returns `None` when unknown.
pub fn find_role(store: &RolesStore, role: &str) -> Option<RoleRecord> {
    if let Ok(id) = role.parse::<Uuid>() {
        if let Some(record) = store.get(&id) {
            return Some(record.value().clone());
        }
    }
    store
        .iter()
        .find(|entry| entry.value().slug == role)
        .map(|entry| entry.value().clone())
}

/// Evaluate one permission against a permissions map. `permission` is
/// `resource:action`; a bare `resource` checks any action on it, and `*`
/// is granted only by a literal `"*": ["*"]` entry in the map.
pub fn permission_allowed(permissions: &Value, permission: &str) -> bool {
    let map = match permissions.as_object() {
        Some(map) => map,
        None => return false,
    };
    // A wildcard resource grants everything.
    if let Some(actions) = map.get("*") {
        if actions
            .as_array()
            .is_some_and(|list| list.iter().any(|a| a.as_str() == Some("*")))
        {
            return true;
        }
    }
    if permission == "*" {
        // The wildcard-resource check above already returned true for a
        // literal `"*": ["*"]` grant; any other map holds no wildcard,
        // so `"*"` must not be granted here. Returning true for any
        // non-empty map would grant every permission to any role that
        // has any permission at all.
        return false;
    }
    let (resource, action) = match permission.split_once(':') {
        Some((resource, action)) => (resource, Some(action)),
        None => (permission, None),
    };
    let actions = match map.get(resource) {
        Some(actions) => actions,
        None => return false,
    };
    let actions = match actions.as_array() {
        Some(actions) => actions,
        None => return false,
    };
    match action {
        None => !actions.is_empty(),
        Some(action) => actions.iter().any(|a| {
            a.as_str().is_some_and(|name| name == "*" || name == action)
        }),
    }
}

/// Check a permission for a role looked up by id or slug. Returns
/// `Ok(None)` when the role does not exist.
pub fn check_role(store: &RolesStore, role: &str, permission: &str) -> Option<bool> {
    find_role(store, role).map(|record| permission_allowed(&record.permissions, permission))
}

async fn list_roles(
    store: web::Data<RolesStore>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let sort = query.allowed_sort(&["name", "slug", "created_at"], "created_at");
    let descending = pagination.order == "desc";
    let search = pagination.search.clone().map(|s| s.to_lowercase());
    let is_system_filter = pagination
        .filters
        .get("is_system")
        .and_then(|value| value.parse::<bool>().ok());

    let mut records: Vec<RoleRecord> = store
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            if let Some(search) = &search {
                let haystack = format!("{} {}", record.name, record.slug).to_lowercase();
                if !haystack.contains(search.as_str()) {
                    return false;
                }
            }
            if let Some(is_system) = is_system_filter {
                if record.is_system != is_system {
                    return false;
                }
            }
            true
        })
        .collect();
    records.sort_by(|a, b| match sort.as_str() {
        "name" => a.name.cmp(&b.name),
        "slug" => a.slug.cmp(&b.slug),
        _ => a.created_at.cmp(&b.created_at),
    });
    if descending {
        records.reverse();
    }
    let total = records.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(records.len());
    let items = if start < records.len() {
        records[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
}

async fn get_role(store: web::Data<RolesStore>, id: web::Path<String>) -> HttpResponse {
    let key = id.into_inner();
    match find_role(&store, &key) {
        Some(record) => success_response(record, 200),
        None => error_response(
            &UnifiedError::not_found(format!("Role \"{key}\" not found")),
            false,
        ),
    }
}

async fn create_role(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    body: web::Json<CreateRoleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<RoleRecord> = (|| {
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Role name must not be empty"));
        }
        if !valid_slug(&input.slug) {
            return Err(UnifiedError::bad_request(
                "Role slug must be lowercase letters, digits, '-' or '_'",
            ));
        }
        validate_permissions(&input.permissions)?;
        if store.iter().any(|entry| entry.value().slug == input.slug) {
            return Err(UnifiedError::conflict(format!(
                "A role with slug \"{}\" already exists",
                input.slug
            )));
        }
        let now = Utc::now();
        let record = RoleRecord {
            id: Uuid::new_v7(),
            name: input.name.trim().to_string(),
            slug: input.slug,
            description: input.description,
            is_system: false,
            permissions: input.permissions,
            created_at: now,
            updated_at: now,
        };
        store.insert(record.id, record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => {
            ctx.hooks.emit(
                events::ROLE_CREATED,
                json!({ "id": record.id, "slug": record.slug, "name": record.name }),
            );
            success_response(record, 201)
        }
        Err(error) => error_response(&error, debug),
    }
}

async fn update_role(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    id: web::Path<String>,
    body: web::Json<UpdateRoleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    let input = body.into_inner();
    let result: UnifiedResult<RoleRecord> = (|| {
        let mut record = match find_role(&store, &key) {
            Some(record) => record,
            None => {
                return Err(UnifiedError::not_found(format!(
                    "Role \"{key}\" not found"
                )))
            }
        };
        if let Some(name) = &input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Role name must not be empty"));
            }
            record.name = name.trim().to_string();
        }
        if let Some(slug) = &input.slug {
            if record.is_system {
                return Err(UnifiedError::conflict(
                    "System roles cannot be renamed",
                ));
            }
            if !valid_slug(slug) {
                return Err(UnifiedError::bad_request(
                    "Role slug must be lowercase letters, digits, '-' or '_'",
                ));
            }
            if store
                .iter()
                .any(|entry| entry.key() != &record.id && entry.value().slug == *slug)
            {
                return Err(UnifiedError::conflict(format!(
                    "A role with slug \"{slug}\" already exists"
                )));
            }
            record.slug = slug.clone();
        }
        if let Some(description) = input.description {
            record.description = description;
        }
        if let Some(permissions) = &input.permissions {
            validate_permissions(permissions)?;
            record.permissions = permissions.clone();
        }
        record.updated_at = Utc::now();
        store.insert(record.id, record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => success_response(record, 200),
        Err(error) => error_response(&error, debug),
    }
}

async fn delete_role(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    let record = match find_role(&store, &key) {
        Some(record) => record,
        None => {
            return error_response(
                &UnifiedError::not_found(format!("Role \"{key}\" not found")),
                debug,
            )
        }
    };
    if record.is_system {
        return error_response(
            &UnifiedError::conflict(format!(
                "System role \"{}\" cannot be deleted",
                record.slug
            )),
            debug,
        );
    }
    store.remove(&record.id);
    success_response(json!({ "deleted": true, "id": record.id }), 200)
}

async fn get_role_permissions(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    match find_role(&store, &key) {
        Some(record) => success_response(
            json!({ "id": record.id, "slug": record.slug, "permissions": record.permissions }),
            200,
        ),
        None => error_response(
            &UnifiedError::not_found(format!("Role \"{key}\" not found")),
            debug,
        ),
    }
}

async fn check_permission(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<RolesStore>,
    body: web::Json<CheckRoleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let allowed = match check_role(&store, &input.role, &input.permission) {
        Some(allowed) => allowed,
        None => {
            return error_response(
                &UnifiedError::not_found(format!("Role \"{}\" not found", input.role)),
                debug,
            )
        }
    };
    // The hook lets other modules override the check result (for example
    // a company-scoped role manager). The payload value wins over the
    // locally computed one.
    let hook = ctx.hooks.emit(
        "role.check",
        json!({ "role": input.role, "permission": input.permission, "allowed": allowed }),
    );
    let allowed = hook
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(allowed);
    success_response(
        CheckRoleResponse {
            role: input.role,
            permission: input.permission,
            allowed,
        },
        200,
    )
}

impl Module for RolesModule {
    fn id(&self) -> &'static str {
        "roles"
    }

    fn name(&self) -> &'static str {
        "Roles"
    }

    fn description(&self) -> &'static str {
        "Role manager: static default roles, dynamic role CRUD, and a hookable permission check factory."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let store = self.store.clone();
        // Default implementation of the `role.check` hook: answer from the
        // role store. Other modules may register their own handlers and
        // override `allowed` on the payload.
        bus.register("role.check", "roles", Arc::new(move |context| {
            let permission = context
                .get("permission")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            // A `role_id` payload (from the users module) asks whether the
            // role exists at all; existence alone is enough there, even
            // for a role with an empty permission set.
            if let Some(role_id) = context
                .get("role_id")
                .and_then(|value| value.as_str())
            {
                let exists = find_role(&store, role_id).is_some();
                context.set("allowed", Value::Bool(exists));
                context.add_result(json!({
                    "role": role_id,
                    "permission": permission,
                    "allowed": exists,
                }));
                return;
            }
            let role_key = context
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            match check_role(&store, &role_key, &permission) {
                Some(allowed) => {
                    context.set("allowed", Value::Bool(allowed));
                    context.add_result(json!({
                        "role": role_key,
                        "permission": permission,
                        "allowed": allowed,
                    }));
                }
                None => {
                    context.set("allowed", Value::Bool(false));
                    context.add_result(json!({
                        "role": role_key,
                        "permission": permission,
                        "allowed": false,
                    }));
                }
            }
        }));
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/roles", owner: "roles", description: "List roles" },
            RouteInfo { method: "POST", path: "/api/v1/roles", owner: "roles", description: "Create role" },
            RouteInfo { method: "POST", path: "/api/v1/roles/check", owner: "roles", description: "Check permission" },
            RouteInfo { method: "GET", path: "/api/v1/roles/{id}", owner: "roles", description: "Get role" },
            RouteInfo { method: "PUT", path: "/api/v1/roles/{id}", owner: "roles", description: "Update role" },
            RouteInfo { method: "DELETE", path: "/api/v1/roles/{id}", owner: "roles", description: "Delete role" },
            RouteInfo { method: "GET", path: "/api/v1/roles/{id}/permissions", owner: "roles", description: "Get role permissions" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/roles")
                .app_data(web::Data::new(self.store.clone()))
                .route("", web::get().to(list_roles))
                .route("", web::post().to(create_role))
                .route("/check", web::post().to(check_permission))
                .route("/{id}", web::get().to(get_role))
                .route("/{id}", web::put().to(update_role))
                .route("/{id}", web::delete().to(delete_role))
                .route("/{id}/permissions", web::get().to(get_role_permissions)),
        );
    }
}
