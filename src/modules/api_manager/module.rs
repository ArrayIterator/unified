// Api Manager - Module struct, route table and registration.

// API Manager module.
//
// The public / client API surface, separate from the core API: a registry
// of client-facing endpoints with payload validation rules, a version
// overview, a simple per-endpoint per-minute rate limiter, and API key
// management. The key endpoints under `/api/v1/api-manager/keys` are a
// thin adapter over the API Keys module (`crate::modules::apikeys`),
// which is the single source of truth for key storage, hashing, and
// verification; this module keeps no key store of its own.
//
// Versioned public API (see the "Version registry" section below):
//
//   - `ApiVersion` records describe the public API versions (v1, v2,
//     v3, ...). Each version has its own settings: enabled/disabled,
//     deprecated flag plus sunset date, per-version rate limit, auth
//     requirement, and notes. A version can be created blank or by
//     copying the settings of an existing version.
//   - The global public API prefix (default `"v"`, producing `/v1/...`,
//     `/v2/...`) is a setting on this module's state. It governs the
//     PUBLIC versioned surface only. The internal module routes stay
//     mounted at `/api/v1/<module>` by default; changing the prefix
//     does not move them. The prefix setting carries an explicit
//     `migrate_internal` flag: only when an admin enables it does the
//     deployment intend to remount the internal routes under the new
//     prefix, which requires the server factory to mount them there
//     (see the remount snippet in the coordinator report -- the server
//     factory is not edited by this module, and an in-memory setting
//     alone cannot remount routes that Actix already registered).
//   - Every endpoint declares version support: all versions, a
//     minimum version, a maximum version, a range, or an explicit
//     list of versions. `resolve_endpoint` answers, for a given
//     method + path + version, which endpoint matches or precisely
//     why it does not (unknown version, disabled version, endpoint
//     not in that version with `available_in` metadata).
//   - Built-in endpoints are collected at startup from every
//     module's `route_info()` data via the module registry catalog.
//     A module declares which versions expose its routes through
//     `Module::route_version_support` (default: all versions); the
//     collector reads that declaration per owning module, so a
//     built-in route registers with real version support instead of
//     everything defaulting to `All`. The API Manager is the single
//     source of truth for what the public API exposes per version.
//   - Custom endpoints are checked against the built-in registry at
//     creation time: a method + path collision in any chosen version
//     is rejected with 422 and emits `api.endpoint.conflict`.
//
// Note on execution: `POST /endpoints/{id}/execute` validates the payload
// against the endpoint rules and returns the validation result plus an
// echo. Dispatching the request into a registered core route handler is
// out of scope for this milestone.
//
// Access control (default deny): public access is governed by
// `ApiAccessRule` rows (per endpoint template and version, with a
// per-endpoint public-access flag) plus the API Keys module's
// per-key scopes and per-minute limits. Nothing on the public
// surface is reachable without an explicit allow rule: a request to
// a public or key-presented internal route that matches no allow
// rule is rejected with 403 before any handler runs. Enforcement
// lives in `PublicApiAccessMiddleware` (registered through the
// module middleware chain, priority 850) and, for the public
// gateway, inside `resolve_public` itself; the two never overlap
// (the gateway's own `/api/v1/api-manager/` prefix is exempt from
// the middleware), so a request is checked exactly once. The admin
// routes under that same prefix are NOT covered by the middleware:
// each admin handler gates itself with `caller` + `require_permission`
// (`api-manager:read` for reads, `api-manager:manage` for
// mutations). There is no session or JWT middleware on this server,
// so the admin surface must never be described as session-protected.
// Every access-control change is written to an `ApiAccessAuditEntry` and,
// when a database pool is attached, persisted to the migration
// 000004 tables (`api_access_rules`, `api_access_audit`); the same
// tables are loaded back into memory at startup, so rules and the
// audit trail survive restarts. Key lifecycle changes
// (`apikey.created` / `revoked` / `rotated`, emitted by the API Keys
// module) are audited here too, through hook listeners registered
// in `register_hooks`.

use unified_core::context::AppContext;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::apikeys::apikey_events;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::ai_capabilities::AI_CAPABILITIES;
use super::{ApiManagerState, PublicApiAccessMiddleware, create_access_rule, create_api_version,
    create_endpoint, create_key, delete_access_rule, delete_api_version, delete_endpoint,
    deprecate_api_version, execute_endpoint, get_api_version, get_endpoint, get_key_access,
    get_prefix, get_registry_detail, get_version_choices, list_access_audit, list_access_rules,
    list_api_versions, list_endpoints, list_keys, list_registry, list_versions, on_apikey_created,
    on_apikey_revoked, on_apikey_rotated, resolve_handler, resolve_public, revoke_key,
    update_access_rule, update_api_version, update_endpoint, update_key_access, update_prefix};

// Module

pub struct ApiManagerModule {
    pub(crate) state: ApiManagerState,
}

impl ApiManagerModule {
    pub fn new() -> Self {
        Self {
            state: ApiManagerState::default(),
        }
    }
}

impl Default for ApiManagerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ApiManagerModule {
    fn id(&self) -> &'static str {
        "api-manager"
    }

    fn name(&self) -> &'static str {
        "API Manager"
    }

    fn description(&self) -> &'static str {
        "Public client API registry with payload validation, rate limits, and API keys. Owns the versioned public API: version registry with per-version settings, global API prefix, per-version endpoint registry collected from every module's route information, and collision-checked custom endpoints. Enforces default-deny public access: per-endpoint per-version allow rules, per-key scopes and rate limits, and an audit trail of access-control changes."
    }

    fn register_middleware(&self, registry: &mut unified_core::middleware::MiddlewareManager) {
        registry.register(PublicApiAccessMiddleware::new(self.state.clone()));
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Key administration is audited here, not in the API Keys
        // module: the audit trail (and its database table) lives on
        // this state, and the keys module already emits
        // `apikey.created` / `apikey.revoked` / `apikey.rotated`
        // for every lifecycle change. `apikey.used` is deliberately
        // not audited: it fires on every authenticated request.
        let state = self.state.clone();
        bus.register(
            apikey_events::CREATED,
            "api-manager",
            std::sync::Arc::new(move |ctx: &mut HookContext| {
                on_apikey_created(&state, ctx);
            }),
        );
        let state = self.state.clone();
        bus.register(
            apikey_events::REVOKED,
            "api-manager",
            std::sync::Arc::new(move |ctx: &mut HookContext| {
                on_apikey_revoked(&state, ctx);
            }),
        );
        let state = self.state.clone();
        bus.register(
            apikey_events::ROTATED,
            "api-manager",
            std::sync::Arc::new(move |ctx: &mut HookContext| {
                on_apikey_rotated(&state, ctx);
            }),
        );
    }

    fn init(&self, ctx: &AppContext) -> unified_core::error::UnifiedResult<()> {

        ai::register_ai_capabilities("api-manager", AI_CAPABILITIES);
        // Seed the prefix and default versions from the config file
        // (`AppConfig.api`) and collect every module's routes into
        // the built-in endpoint registry. Both steps are idempotent,
        // so a second init never overwrites admin edits.
        let config = ctx.config.read().expect("config lock poisoned").clone();
        self.state.apply_config(&config);
        self.state.collect_from_catalog(&ctx.modules.route_catalog(), &ctx.modules);
        self.state.set_db_pool(ctx.db.read().expect("db lock poisoned").clone());
        // Best-effort reload of the access rules and audit trail
        // from migration 000004 (`api_access_rules`,
        // `api_access_audit`): the tables exist only once that
        // migration has run, and the in-memory state stays
        // authoritative until then. Seeded rules fill only the gaps
        // the database did not already cover, so admin edits and
        // persisted rows always win over seeds.
        let _ = self.state.load_access_from_db();
        self.state.ensure_seeded_access_rules();
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/api-manager")
                .app_data(state)
                .route("/endpoints", web::get().to(list_endpoints))
                .route("/endpoints", web::post().to(create_endpoint))
                .route("/endpoints/{id}", web::get().to(get_endpoint))
                .route("/endpoints/{id}", web::put().to(update_endpoint))
                .route("/endpoints/{id}", web::delete().to(delete_endpoint))
                .route("/endpoints/{id}/execute", web::post().to(execute_endpoint))
                .route("/versions", web::get().to(list_versions))
                .route("/api-versions", web::get().to(list_api_versions))
                .route("/api-versions", web::post().to(create_api_version))
                .route("/api-versions/{version}", web::get().to(get_api_version))
                .route("/api-versions/{version}", web::put().to(update_api_version))
                .route("/api-versions/{version}", web::delete().to(delete_api_version))
                .route(
                    "/api-versions/{version}/deprecate",
                    web::post().to(deprecate_api_version),
                )
                .route("/prefix", web::get().to(get_prefix))
                .route("/prefix", web::put().to(update_prefix))
                .route("/version-choices", web::get().to(get_version_choices))
                .route("/registry", web::get().to(list_registry))
                .route("/registry/detail", web::get().to(get_registry_detail))
                .route("/resolve", web::get().to(resolve_handler))
                .route("/public/{version}/{path:.*}", web::route().to(resolve_public))
                .route("/keys", web::post().to(create_key))
                .route("/keys", web::get().to(list_keys))
                .route("/keys/{id}", web::delete().to(revoke_key))
                .route("/keys/{id}/access", web::get().to(get_key_access))
                .route("/keys/{id}/access", web::put().to(update_key_access))
                .route("/access/rules", web::get().to(list_access_rules))
                .route("/access/rules", web::post().to(create_access_rule))
                .route("/access/rules/{id}", web::put().to(update_access_rule))
                .route("/access/rules/{id}", web::delete().to(delete_access_rule))
                .route("/access/audit", web::get().to(list_access_audit)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/api-manager/endpoints", owner: "api-manager", description: "List custom endpoints (paginated, filter by version, method, enabled, owner, source, auth).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/api-manager/endpoints", owner: "api-manager", description: "Create a custom endpoint; rejected with 422 when it collides with a built-in endpoint.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/endpoints/{id}", owner: "api-manager", description: "Get one custom endpoint.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/api-manager/endpoints/{id}", owner: "api-manager", description: "Update a custom endpoint, including its version support.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/api-manager/endpoints/{id}", owner: "api-manager", description: "Delete a custom endpoint.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/api-manager/endpoints/{id}/execute", owner: "api-manager", description: "Validate a payload against an endpoint's rules (validation only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/versions", owner: "api-manager", description: "Legacy grouped overview of custom endpoints by version string.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/api-versions", owner: "api-manager", description: "List API versions with per-version settings (paginated, filter by enabled, deprecated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/api-manager/api-versions", owner: "api-manager", description: "Create an API version, blank or by copying an existing version's settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/api-versions/{version}", owner: "api-manager", description: "Get one API version with its settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/api-manager/api-versions/{version}", owner: "api-manager", description: "Update an API version's settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/api-manager/api-versions/{version}", owner: "api-manager", description: "Delete an API version (refused while custom endpoints cover it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/api-manager/api-versions/{version}/deprecate", owner: "api-manager", description: "Deprecate an API version with a sunset date.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/prefix", owner: "api-manager", description: "Get the global public API prefix setting.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/api-manager/prefix", owner: "api-manager", description: "Update the global public API prefix setting.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/version-choices", owner: "api-manager", description: "Version choices for the custom-endpoint creation form (single, all, multi-select).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/registry", owner: "api-manager", description: "List endpoints grouped by version (paginated, filter by module, method, version, auth).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/registry/detail", owner: "api-manager", description: "Per-endpoint detail: version support, owner module, auth, rate limit.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/resolve", owner: "api-manager", description: "Resolve a method + version + path to its endpoint or a precise miss reason.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "*", path: "/api/v1/api-manager/public/{version}/{path:.*}", owner: "api-manager", description: "Public versioned gateway: resolve a request against the per-version registry.", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/api-manager/keys", owner: "api-manager", description: "Create an API key (full key shown exactly once).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/keys", owner: "api-manager", description: "List API keys (paginated, hashes never returned).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/api-manager/keys/{id}", owner: "api-manager", description: "Revoke an API key.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/keys/{id}/access", owner: "api-manager", description: "Get one API key's access: scopes, rate tier, and per-minute limit.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/api-manager/keys/{id}/access", owner: "api-manager", description: "Update one API key's scopes, rate tier, and per-minute limit.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/access/rules", owner: "api-manager", description: "List public-access rules (per endpoint and version, paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/api-manager/access/rules", owner: "api-manager", description: "Create a public-access rule; rejected with 409 when the rule already exists.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/api-manager/access/rules/{id}", owner: "api-manager", description: "Update a public-access rule (allow, public access, actor, reason).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/api-manager/access/rules/{id}", owner: "api-manager", description: "Delete a public-access rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/api-manager/access/audit", owner: "api-manager", description: "List the access-control audit trail (newest first, paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ApiManagerModule::new()));
}
