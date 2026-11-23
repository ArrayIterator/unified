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
//     `ProductCategory` in `unified_core::modules`): a step name
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
// `register_hooks`); every application instead emits its applied
// module list on the separate `presets.preset.modules_applied`
// event, so the tenants provisioning flow and the Extensions
// Manager can resolve a `preset_id` alone into grantable modules
// without ever mistaking a query answer for an applied set.
//
// Branding and domains are NOT implemented here: the wizard stores
// the answers (company name, branding, domain) and hands them off
// through `presets.wizard.settings.completed` /
// `preset.settings.applied` events for the owning modules.
//
// Persistence: the in-memory DashMap stores below are authoritative
// at runtime, the same pattern as the other modules in this round;
// migration `000070_presets` ships the PostgreSQL tables and the
// integrator wires them into crates/unified-core/src/db/schema.rs` /
// crates/unified-core/src/db/models.rs` for durable persistence. The migration also
// carries the same four built-in presets as `INSERT` rows, with the
// same ids as the in-memory seeds.
//
// Capability model: caller identity comes from the `X-User-Id`
// and `X-Role` request headers, exactly like the logistics,
// curriculum, and compliance modules, and every handler checks
// the permission through the roles module's `role.check` hook,
// failing closed when the roles module does not answer. The
// `actor` fields older clients still send in request bodies are
// untrusted payload data and are never consulted for identity or
// authorization, so a body claiming an admin role grants nothing.
//   - `admin` / `super_admin`: view, manage, apply.
//   - `operator` / `developer`: view, apply.
//   - `viewer` / `auditor`: view only.
//   - Unknown or absent roles hold nothing.
// Which roles hold which of these permissions is the roles
// module's data (see its capability list), not a matrix in this
// module.

//! Presets + Setup Wizard module. — Module implementation.
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::apply::*;
use super::events::*;
use super::helpers::*;
use super::identity::*;
use super::lookups::*;
use super::preset_handlers::*;
use super::records::*;
use super::requests::*;
use super::seeds::*;
use super::stores::*;
use super::wizard_handlers::*;
use super::wizard_steps::*;
use super::ai_capabilities::AI_CAPABILITIES;

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::modules::Module;
use unified_core::modules::RouteInfo;
use crate::modules::ai;
use actix_web::web;
use serde_json::Value;
use std::sync::Arc;

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

    fn init(&self, ctx: &AppContext) -> unified_core::error::UnifiedResult<()> {

        ai::register_ai_capabilities("presets", AI_CAPABILITIES);
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
            Arc::new(move |context: &mut unified_core::hooks::HookContext| {
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
            RouteInfo { method: "GET", path: "/api/v1/presets", owner: "presets", description: "List presets (paginated, search, filter by built_in, archived, business_type).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/presets", owner: "presets", description: "Create a preset definition.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/presets/applications", owner: "presets", description: "List preset applications (paginated, filter by preset_id, company_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/presets/applications/{id}", owner: "presets", description: "Get one preset application.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/presets/wizard/{company_id}", owner: "presets", description: "Get the setup wizard state for a tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/start", owner: "presets", description: "Start (or resume) the setup wizard for a tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/{company_id}/step", owner: "presets", description: "Save the wizard's current step and advance one stored step.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/{company_id}/skip", owner: "presets", description: "Skip the wizard's current step and advance one stored step.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/{company_id}/resume", owner: "presets", description: "Resume the setup wizard for a tenant.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/{company_id}/restart", owner: "presets", description: "Restart the setup wizard for a tenant from the first stored step.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/presets/wizard/{company_id}/complete", owner: "presets", description: "Complete the setup wizard: apply the preset if not yet applied and return the done summary.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/presets/{id}", owner: "presets", description: "Get one preset by id or slug.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/presets/{id}", owner: "presets", description: "Edit a preset definition (built-in presets are edited by cloning, not in place).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/presets/{id}/clone", owner: "presets", description: "Clone a preset definition (the way to edit a built-in preset).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/presets/{id}/archive", owner: "presets", description: "Archive a preset definition.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/presets/{id}/apply", owner: "presets", description: "Apply a preset to a tenant (enables modules in dependency order, never disables).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/presets/{id}/preview", owner: "presets", description: "Preview a preset for a tenant: resolved modules, entitlement and dependency status, apply order.", auth: unified_core::auth::RouteAuth::Authenticated},
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
