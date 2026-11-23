//! import export - module.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry};
use crate::modules::ai;
use actix_web::web;
use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;
use super::events::*;
use super::entities::*;
use super::store::*;
use super::export::*;
use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers::{confirm_import, create_export, create_import, create_mapping, delete_mapping, download_export, download_import_errors, get_export, get_import, get_mapping, get_settings, list_entities, list_exports, list_imports, list_mappings, update_settings};
use super::handlers_schedules::{append_chunk, complete_session, create_schedule, create_session, delete_schedule, get_schedule, get_session, list_schedules, list_sessions, run_schedule_now, update_schedule};

pub struct ImportExportModule;

impl Module for ImportExportModule {
    fn id(&self) -> &'static str {
        "import_export"
    }

    fn name(&self) -> &'static str {
        "Import / Export"
    }

    fn description(&self) -> &'static str {
        "CSV, JSON, and XML import/export for users, software, products, orders, invoices, tasks, and licenses: dry-run validation with per-row errors, confirmed staging with idempotency by external id, saved field-mapping templates, chunked imports, scheduled exports, capability-checked and tenant-scoped jobs with PII redaction through the privacy module, and audit-logged lifecycle events."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "software", "store", "billing", "tasks", "licensing"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["serde_json", "base64"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("import_export", AI_CAPABILITIES);
        let _ = HOOK_BUS.set(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Run a stored schedule when the scheduler dispatches
        // `export.run` (emitted as `export.run_requested`). The
        // scheduler has no `export.run` dispatch arm yet, so today
        // this handler is reached through `run-now` flows and through
        // any future scheduler arm; see the integration notes.
        bus.register(
            import_export_events::EXPORT_RUN_REQUESTED,
            "import_export",
            Arc::new(move |ctx: &mut HookContext| {
                let schedule_id = ctx
                    .get("export_schedule_id")
                    .or_else(|| ctx.get("job_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let Ok(schedule_id) = Uuid::parse_str(&schedule_id) else {
                    ctx.add_result(serde_json::json!({
                        "error": "export.run_requested requires 'export_schedule_id'"
                    }));
                    return;
                };
                let Some(schedule) = export_schedules_store()
                    .get(&schedule_id)
                    .map(|entry| entry.value().clone())
                else {
                    ctx.add_result(serde_json::json!({
                        "error": "Export schedule not found"
                    }));
                    return;
                };
                if !schedule.enabled {
                    ctx.add_result(serde_json::json!({
                        "skipped": true,
                        "reason": "Export schedule is disabled"
                    }));
                    return;
                }
                match run_schedule(&schedule) {
                    Ok(job) => {
                        ctx.add_result(serde_json::json!({
                            "started": true,
                            "export_schedule_id": schedule_id,
                            "export_id": job.id,
                            "row_count": job.row_count,
                        }));
                    }
                    Err(error) => {
                        let now = Utc::now();
                        if let Some(mut stored) =
                            export_schedules_store().get_mut(&schedule_id)
                        {
                            stored.last_run_at = Some(now);
                            stored.last_error = Some(error.message.clone());
                            stored.updated_at = now;
                        }
                        ctx.add_result(serde_json::json!({ "error": error.message }));
                    }
                }
            }),
        );

        // Full-tenant export, requested by the tenant-destroy flow
        // through `export.requested` with `scope:
        // "full_tenant_export"`. The destroy flow's export-first
        // gate depends on this answer: it reads `export_id` (or
        // `export_job_id` / `job_id`) and then proves the export
        // through `export.lookup` before the schema drop. The
        // `scope` gate is also the recursion guard: the
        // data-provider query `run_export` emits carries no
        // `scope` key, so this handler never answers its own
        // emit. Other scopes are ignored -- this module does not
        // turn arbitrary `export.requested` events into jobs.
        bus.register(
            import_export_events::EXPORT_REQUESTED,
            "import_export",
            Arc::new(move |ctx: &mut HookContext| {
                if ctx.get("scope").and_then(Value::as_str) != Some("full_tenant_export") {
                    return;
                }
                let entity = ctx
                    .get("entity")
                    .and_then(Value::as_str)
                    .unwrap_or("tenant")
                    .to_string();
                // A full-tenant export without a tenant id would
                // run unscoped (every tenant's rows), so the
                // request is refused instead of answered.
                let Some(tenant_id) = ctx
                    .get("tenant_id")
                    .or_else(|| ctx.get("company_id"))
                    .and_then(Value::as_str)
                    .and_then(|raw| Uuid::parse_str(raw).ok())
                else {
                    ctx.add_result(serde_json::json!({
                        "error": "export.requested with scope \"full_tenant_export\" requires 'tenant_id'"
                    }));
                    return;
                };
                let actor = ctx
                    .get("actor")
                    .and_then(Value::as_str)
                    .unwrap_or("system")
                    .to_string();
                let format = ctx
                    .get("format")
                    .and_then(Value::as_str)
                    .unwrap_or("json")
                    .to_string();
                // The destroy export must be restorable, so personal
                // data stays unredacted unless the requester
                // explicitly asks for redaction. Trust basis: the
                // destroy flow is already gated on
                // `tenants:destroy`, and the export file is the
                // only way the tenant's data survives the drop.
                let redact_pii = ctx
                    .get("redact_pii")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mut filters = HashMap::new();
                filters.insert("id".to_string(), tenant_id.to_string());
                let spec = ExportSpec {
                    entity,
                    format,
                    fields: None,
                    filters,
                    redact_pii: Some(redact_pii),
                    redact_names: None,
                    limit: None,
                    offset: None,
                };
                match run_export(&actor, Some(tenant_id), !redact_pii, &spec) {
                    Ok(job) => {
                        // A full-tenant export with no rows is not
                        // a restorable export: the tenant being
                        // destroyed exists, so zero rows means no
                        // owning module answered the data-provider
                        // query. Answer an error (the stored job
                        // stays inspectable) instead of a job id
                        // the destroy gate would accept as proof.
                        if job.data_provider == "none" || job.row_count == 0 {
                            ctx.add_result(serde_json::json!({
                                "error": format!(
                                    "Full-tenant export produced no rows for entity \"{}\": no owning module answered export.requested for it (data_provider: \"{}\")",
                                    job.entity, job.data_provider
                                ),
                            }));
                            return;
                        }
                        ctx.add_result(serde_json::json!({
                            "export_id": job.id,
                            "export_job_id": job.id,
                            "job_id": job.id,
                            "export": export_job_view(&job),
                        }));
                    }
                    Err(error) => {
                        ctx.add_result(serde_json::json!({ "error": error.message }));
                    }
                }
            }),
        );

        // Answer `export.lookup` (the tenant-destroy export-first
        // gate asks it for `{"export": {"status",
        // "download_url"}}`). Unknown job ids answer
        // `{"exists": false}` instead of staying silent, so the
        // requester can tell "no such export" from "no answerer".
        bus.register(
            import_export_events::EXPORT_LOOKUP,
            "import_export",
            Arc::new(move |ctx: &mut HookContext| {
                let raw = ctx
                    .get("export_id")
                    .or_else(|| ctx.get("export_job_id"))
                    .or_else(|| ctx.get("job_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let Ok(id) = Uuid::parse_str(&raw) else {
                    ctx.add_result(serde_json::json!({
                        "error": "export.lookup requires 'export_id'"
                    }));
                    return;
                };
                match export_jobs_store().get(&id).map(|entry| entry.value().clone()) {
                    Some(job) => {
                        ctx.add_result(serde_json::json!({
                            "exists": true,
                            "export": export_job_view(&job),
                        }));
                    }
                    None => {
                        ctx.add_result(serde_json::json!({
                            "exists": false,
                            "export_id": id,
                        }));
                    }
                }
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/import-export")
                .route("/entities", web::get().to(list_entities))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                .route("/exports", web::get().to(list_exports))
                .route("/exports", web::post().to(create_export))
                .route("/exports/{id}", web::get().to(get_export))
                .route("/exports/{id}/download", web::get().to(download_export))
                .route("/imports", web::get().to(list_imports))
                .route("/imports", web::post().to(create_import))
                .route("/imports/{id}", web::get().to(get_import))
                .route("/imports/{id}/errors/download", web::get().to(download_import_errors))
                .route("/imports/{id}/confirm", web::post().to(confirm_import))
                .route("/mappings", web::get().to(list_mappings))
                .route("/mappings", web::post().to(create_mapping))
                .route("/mappings/{id}", web::get().to(get_mapping))
                .route("/mappings/{id}", web::delete().to(delete_mapping))
                .route("/export-schedules", web::get().to(list_schedules))
                .route("/export-schedules", web::post().to(create_schedule))
                .route("/export-schedules/{id}", web::get().to(get_schedule))
                .route("/export-schedules/{id}", web::patch().to(update_schedule))
                .route("/export-schedules/{id}", web::delete().to(delete_schedule))
                .route("/export-schedules/{id}/run-now", web::post().to(run_schedule_now))
                .route("/import-sessions", web::get().to(list_sessions))
                .route("/import-sessions", web::post().to(create_session))
                .route("/import-sessions/{id}", web::get().to(get_session))
                .route("/import-sessions/{id}/chunks", web::post().to(append_chunk))
                .route("/import-sessions/{id}/complete", web::post().to(complete_session)),
        );
    }

    fn route_info(&self) -> Vec<unified_core::modules::RouteInfo> {
        use unified_core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/import-export/entities", owner: "import_export", description: "Entity catalog with columns and types", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/settings", owner: "import_export", description: "Effective import/export limits and defaults", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/import-export/settings", owner: "import_export", description: "Update import/export limits and defaults (manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/exports", owner: "import_export", description: "List export jobs (paginated)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/import-export/exports", owner: "import_export", description: "Start an export job (JSON, CSV, or XML)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/exports/{id}", owner: "import_export", description: "Export job status", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/exports/{id}/download", owner: "import_export", description: "Download the finished export file", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/imports", owner: "import_export", description: "List import jobs (paginated)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/import-export/imports", owner: "import_export", description: "Submit a file for dry-run validation", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/imports/{id}", owner: "import_export", description: "Import job with per-row results", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/imports/{id}/errors/download", owner: "import_export", description: "Download the row-error report as CSV", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/import-export/imports/{id}/confirm", owner: "import_export", description: "Confirm a validated import and stage its rows", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/mappings", owner: "import_export", description: "List saved field-mapping templates (paginated)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/import-export/mappings", owner: "import_export", description: "Save a field-mapping template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/mappings/{id}", owner: "import_export", description: "One field-mapping template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/import-export/mappings/{id}", owner: "import_export", description: "Delete a field-mapping template", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/export-schedules", owner: "import_export", description: "List scheduled exports (paginated)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/import-export/export-schedules", owner: "import_export", description: "Create a scheduled export", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/export-schedules/{id}", owner: "import_export", description: "One scheduled export", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/import-export/export-schedules/{id}", owner: "import_export", description: "Update a scheduled export", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/import-export/export-schedules/{id}", owner: "import_export", description: "Delete a scheduled export", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/import-export/export-schedules/{id}/run-now", owner: "import_export", description: "Run a scheduled export now", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/import-sessions", owner: "import_export", description: "List chunked import sessions (paginated)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/import-export/import-sessions", owner: "import_export", description: "Open a chunked import session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/import-export/import-sessions/{id}", owner: "import_export", description: "One chunked import session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/import-export/import-sessions/{id}/chunks", owner: "import_export", description: "Append one validated chunk to a session", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/import-export/import-sessions/{id}/complete", owner: "import_export", description: "Finalize a session into an import job", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ImportExportModule));
}
