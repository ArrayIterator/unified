//! import export - store.

use unified_core::hooks::HookContext;
use dashmap::DashMap;
use serde_json::Value;
use uuid::Uuid;
use super::entities::*;
use super::jobs::*;

pub(crate) fn export_jobs_store() -> &'static DashMap<Uuid, ExportJob> {
    EXPORT_JOBS.get_or_init(DashMap::new)
}

pub(crate) fn import_jobs_store() -> &'static DashMap<Uuid, ImportJob> {
    IMPORT_JOBS.get_or_init(DashMap::new)
}

pub(crate) fn mappings_store() -> &'static DashMap<Uuid, MappingTemplate> {
    MAPPINGS.get_or_init(DashMap::new)
}

pub(crate) fn export_schedules_store() -> &'static DashMap<Uuid, ExportSchedule> {
    EXPORT_SCHEDULES.get_or_init(DashMap::new)
}

pub(crate) fn import_sessions_store() -> &'static DashMap<Uuid, ImportSession> {
    IMPORT_SESSIONS.get_or_init(DashMap::new)
}

pub(crate) fn emit(event: &str, payload: Value) -> Option<HookContext> {
    HOOK_BUS.get().map(|bus| bus.emit(event, payload))
}

/// Emit a lifecycle event in the shape the audit log records:
/// `actor` / `actor_id`, `resource`, and the row counts. The audit
/// module only records events listed in its `AUDITED_EVENTS` table;
/// the integrator snippet for that lives in the integration notes.
pub(crate) fn emit_audit(event: &str, actor: &str, company_id: Option<Uuid>, resource: &str, extra: Value) {
    let mut payload = serde_json::json!({
        "actor": actor,
        "actor_id": actor,
        "resource": resource,
    });
    if let Some(company) = company_id {
        payload["company_id"] = Value::String(company.to_string());
    }
    if let (Some(target), Some(source)) = (payload.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    emit(event, payload);
}

/// The public shape of one export job as hook answers carry it:
/// id, entity, status, download URL, row count, format, and
/// redaction flag. Used by the `export.lookup` answerer and by the
/// full-tenant export answer so both speak the same shape the
/// tenant-destroy export gate reads (`export.status`,
/// `export.download_url`).
pub(crate) fn export_job_view(job: &ExportJob) -> Value {
    serde_json::json!({
        "id": job.id,
        "export_id": job.id,
        "entity": job.entity,
        "status": job.status,
        "download_url": job.download_url,
        "row_count": job.row_count,
        "format": job.format,
        "pii_redacted": job.pii_redacted,
        "data_provider": job.data_provider,
        "created_at": job.created_at,
    })
}
