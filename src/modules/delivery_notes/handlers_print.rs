//! HTTP handlers for printing a delivery note: the
//! print-ready document (queued through the printing module)
//! and a preview that renders the same document without
//! queueing a job.
//!
//! Printing reuses the printing module's rendering path: a
//! paper / pdf / email job carries the rendered HTML document
//! (`document_html`) and a thermal job carries thermal lines,
//! both sent through the `printing.job.requested` hook event
//! exactly like the billing module's invoice printing. The
//! printing module owns the queue, the ESC/POS stream, and
//! delivery to hardware — this module never opens a device,
//! socket, or serial port, and never claims to produce PDF
//! bytes (converting the HTML to PDF is a deployment step, as
//! in printing's own honesty note).
//!
//! Company header: the DO stores `company_id` by reference
//! only. The company module answers no header-lookup hook
//! today (it answers `company.exists` with a boolean), so the
//! printed header is the DO's snapshotted sender block — the
//! issuing party as frozen at creation time — plus the
//! `company_id` reference in the payload. When the company
//! module grows a header lookup, the print payload is the
//! place to resolve it.

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};

use super::helpers::{
    caller, caller_company, err, load_note, note_event, note_view, ok,
    require_note_in_company_scope, require_tenant, validate_output_kind,
    validate_paper_size, validate_template_id,
};
use super::state::DeliveryNoteState;
use super::types::{
    render_print_html, render_thermal_lines, DeliveryNoteSettings, PrintRequest, EVENT_PRINTED,
    PERM_DELIVERY_NOTES_MANAGE, PERM_DELIVERY_NOTES_VIEW,
};

/// Path carrying a delivery note id.
#[derive(Debug, Deserialize)]
struct NotePath {
    note_id: String,
}

/// Queue a print job through the printing module and return the
/// `(job_id, status)` it answers with. Fails when the printing
/// module has no `printing.job.requested` handler or when it
/// reports an error — the same contract the billing module uses
/// for invoice printing.
fn request_print_job(state: &DeliveryNoteState, payload: Value) -> UnifiedResult<(String, String)> {
    let Some(context) = state.query("printing.job.requested", payload) else {
        return Err(UnifiedError::internal(
            "Printing module is not registered: printing.job.requested has no handler",
        ));
    };
    match context.get("job_id").and_then(|value| value.as_str()) {
        Some(job_id) => {
            let status = context
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("queued")
                .to_string();
            Ok((job_id.to_string(), status))
        }
        None => {
            let detail = context
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown error")
                .to_string();
            Err(UnifiedError::internal(format!(
                "Printing module did not accept the print job: {detail}"
            )))
        }
    }
}

/// Print a DO: render the print-ready document, queue it as a
/// print job through the printing module, and return the
/// document inline. When the printing module is not registered
/// the document is still returned with a warning and a null
/// `print_job_id`, so the caller can always print — the job
/// queue is a convenience, not the document itself.
pub(crate) async fn print_note(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
    body: web::Json<PrintRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_in_company_scope(&note.company_id, &_scope)?;
        let input = body.into_inner();
        let settings = state.current_settings(&tenant_id);
        let output_kind = match &input.output_kind {
            Some(kind) => validate_output_kind(kind)?,
            None => settings
                .default_output_kind
                .clone()
                .unwrap_or_else(|| "paper".to_string()),
        };
        let paper_size_code = match &input.paper_size_code {
            Some(code) => validate_paper_size(code)?,
            None => settings
                .default_paper_size_code
                .clone()
                .unwrap_or_else(|| "A4".to_string()),
        };
        let template_id = match &input.template_id {
            Some(id) => Some(validate_template_id(id)?),
            None => settings.default_template_id.clone(),
        };
        let items = state.note_items(note.id);
        let document_html = render_print_html(&note, &items, &settings);
        let mut payload = json!({
            "invoice_id": note.number,
            "company_id": note.company_id.map(|id| id.to_string()),
            "template_id": template_id,
            "paper_size_code": paper_size_code,
            "output_kind": output_kind,
            "document_ref": format!("delivery_notes:{}", note.id),
            "requested_by": caller.actor,
            "tenant_id": tenant_id,
        });
        if output_kind == "thermal" {
            payload["thermal_lines"] = Value::Array(render_thermal_lines(&note, &items));
            payload.as_object_mut().map(|map| {
                map.remove("document_html");
            });
        } else {
            payload["document_html"] = Value::String(document_html.clone());
        }
        if let Some(profile_id) = &input.printer_profile_id {
            payload["printer_profile_id"] = Value::String(profile_id.clone());
        }
        let (print_job_id, print_job_status, print_warning) =
            match request_print_job(&state, payload) {
                Ok((job_id, status)) => (Some(job_id), Some(status), None),
                Err(error) => (None, None, Some(error.message.clone())),
            };
        note_event(&state, EVENT_PRINTED, &note, &caller.actor, None);
        let view = note_view(&state, &note);
        let mut body = json!({
            "delivery_note": view,
            "output_kind": output_kind,
            "paper_size_code": paper_size_code,
            "template_id": template_id,
            "document_html": document_html,
            "thermal_lines": render_thermal_lines(&note, &items),
            "print_job_id": print_job_id,
            "print_job_status": print_job_status,
        });
        if let (Value::Object(map), Some(warning)) = (&mut body, print_warning) {
            map.insert("print_warning".to_string(), Value::String(warning));
        }
        Ok(body)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Preview the print-ready document of a DO without queueing
/// a print job: the same HTML the print endpoint queues, plus
/// the thermal lines, so a client can render or print locally.
pub(crate) async fn preview_note(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_VIEW)?;
        let note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_in_company_scope(&note.company_id, &_scope)?;
        let settings: DeliveryNoteSettings = state.current_settings(&tenant_id);
        let items = state.note_items(note.id);
        let document_html = render_print_html(&note, &items, &settings);
        Ok(json!({
            "delivery_note": note_view(&state, &note),
            "output_kind": settings.default_output_kind,
            "paper_size_code": settings.default_paper_size_code,
            "template_id": settings.default_template_id,
            "document_html": document_html,
            "thermal_lines": render_thermal_lines(&note, &items),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
