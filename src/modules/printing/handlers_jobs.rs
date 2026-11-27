//! printing - handlers jobs.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};
use std::sync::{Arc};
use super::{events::*, paper::*, thermal::*, profiles::*, jobs::*, state::*, auth::*, hooks::*, handlers_paper::*, handlers_profiles::*, module::*};



// Print-job handlers
pub(crate) async fn list_print_jobs(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<PrintJobSummary>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<PrintJobSummary> = state
            .print_jobs
            .iter()
            .map(|entry| job_summary(entry.value()))
            .filter(|job| {
                tenant_visible(job.tenant_id.as_deref(), &caller.tenant_id)
                    && matches_search(&format!("{} {}", job.invoice_id, job.id), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| job.status == *status)
                    && params
                        .filters
                        .get("output_kind")
                        .is_none_or(|kind| job.output_kind == *kind)
                    && params.filters.get("invoice_id").is_none_or(|invoice_id| {
                        job.invoice_id == *invoice_id
                    })
                    && params.filters.get("company_id").is_none_or(|company_id| {
                        job.company_id.as_deref() == Some(company_id.as_str())
                    })
                    && params
                        .filters
                        .get("printer_profile_id")
                        .is_none_or(|profile_id| {
                            job.printer_profile_id.as_deref() == Some(profile_id.as_str())
                        })
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn create_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreatePrintJobRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PrintJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_JOB)?;
        state.create_job(body.into_inner(), &caller.actor, Some(&caller.tenant_id))
    })();
    match result {
        Ok(job) => json::success_response(job, 201),
        Err(error) => json::error_response(&error, ctx.is_debug()),
    }
}


pub(crate) async fn get_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PrintJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let job = state
            .print_jobs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Print job not found"))?;
        require_same_tenant(job.tenant_id.as_deref(), &caller.tenant_id)?;
        Ok(job)
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn get_print_job_escpos(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let job = state
            .print_jobs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Print job not found"))?;
        require_same_tenant(job.tenant_id.as_deref(), &caller.tenant_id)?;
        if job.output_kind != "thermal" {
            return Err(UnifiedError::bad_request(
                "Only thermal print jobs carry an ESC/POS stream",
            ));
        }
        Ok(json!({
            "job_id": job.id,
            "invoice_id": job.invoice_id,
            "status": job.status,
            "escpos_base64": job.escpos_base64,
            "preview": job.escpos_preview,
        }))
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) fn load_job_for_transition(
    state: &PrintingState,
    job_id: &str,
) -> UnifiedResult<PrintJob> {
    state
        .print_jobs
        .get(job_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Print job not found"))
}


pub(crate) async fn process_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_JOB)?;
        let id = path.into_inner();
        let job = load_job_for_transition(&state, &id)?;
        require_same_tenant(job.tenant_id.as_deref(), &caller.tenant_id)?;
        let summary = dispatch_job(&state, &id, &caller.actor)?;
        Ok(json!({ "job": summary, "dispatched": true }))
    })();
    json::respond(result, ctx.is_debug())
}


/// The dispatcher's success report: a `printing` job is confirmed
/// printed and becomes `done`.
pub(crate) async fn complete_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PrintJobSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_JOB)?;
        let id = path.into_inner();
        let job = load_job_for_transition(&state, &id)?;
        require_same_tenant(job.tenant_id.as_deref(), &caller.tenant_id)?;
        complete_job(&state, &id)
    })();
    json::respond(result, ctx.is_debug())
}


/// Dispatch a queued job to the deployment-side dispatcher: the
/// job moves to `printing` and stays there. It is NOT marked done
/// here - delivery is unconfirmed until the dispatcher reports
/// back through `/complete` (printed) or `/fail` (failed).
/// Marking it done at dispatch time emitted
/// `printing.job.completed` and notified the user of a print that
/// may never have reached the printer.
pub(crate) fn dispatch_job(
    state: &PrintingState,
    job_id: &str,
    requested_by: &str,
) -> UnifiedResult<PrintJobSummary> {
    let mut job = load_job_for_transition(state, job_id)?;
    if job.status != "queued" {
        return Err(UnifiedError::conflict(format!(
            "Only a queued print job can be processed (current status: {})",
            job.status
        )));
    }
    let started = now();
    job.status = "printing".to_string();
    job.started_at = Some(started);
    job.attempts += 1;
    job.updated_at = started;
    state.print_jobs.insert(job_id.to_string(), job.clone());
    let summary = job_summary(&job);
    state.emit(
        EVENT_PRINT_JOB_DISPATCH_REQUESTED,
        json!({
            "job": summary.clone(),
            "escpos_base64": job.escpos_base64,
            "requested_by": requested_by,
        }),
    );
    Ok(summary)
}


/// Confirm a dispatched job printed: only a `printing` job can
/// complete. This is the moment the job becomes `done`, the
/// completion event fires, and the user is notified.
pub(crate) fn complete_job(
    state: &PrintingState,
    job_id: &str,
) -> UnifiedResult<PrintJobSummary> {
    let mut job = load_job_for_transition(state, job_id)?;
    if job.status != "printing" {
        return Err(UnifiedError::conflict(format!(
            "Only a printing print job can be completed (current status: {})",
            job.status
        )));
    }
    let completed = now();
    job.status = "done".to_string();
    job.completed_at = Some(completed);
    job.updated_at = completed;
    state.print_jobs.insert(job_id.to_string(), job.clone());
    let summary = job_summary(&job);
    state.emit(EVENT_PRINT_JOB_COMPLETED, json!({ "job": summary.clone() }));
    notify_user(
        state,
        &job.requested_by,
        "Print job completed",
        &format!("Print job {} for invoice {} is done.", job.id, job.invoice_id),
        &job.id,
    );
    Ok(summary)
}


/// Mark a job failed. Allowed from `queued`, `printing`, and
/// `done`: a job that reached `done` before delivery was
/// confirmed (the old dispatch behaviour) can still have failed
/// at the printer, and must be allowed to fail. `failed` and
/// `cancelled` are terminal and cannot fail again.
pub(crate) fn fail_job(
    state: &PrintingState,
    job_id: &str,
    reason: &str,
) -> UnifiedResult<PrintJobSummary> {
    let mut job = load_job_for_transition(state, job_id)?;
    if job.status == "failed" || job.status == "cancelled" {
        return Err(UnifiedError::conflict(format!(
            "A {} print job cannot be marked failed",
            job.status
        )));
    }
    let reason = reason.trim().to_string();
    if reason.is_empty() || reason.len() > 1000 {
        return Err(UnifiedError::bad_request(
            "Failure reason must be 1 to 1000 characters",
        ));
    }
    job.status = "failed".to_string();
    job.error_message = Some(reason.clone());
    job.completed_at = Some(now());
    job.updated_at = now();
    state.print_jobs.insert(job_id.to_string(), job.clone());
    let summary = job_summary(&job);
    state.emit(
        EVENT_PRINT_JOB_FAILED,
        json!({ "job": summary.clone(), "error": reason.clone() }),
    );
    notify_user(
        state,
        &job.requested_by,
        "Print job failed",
        &format!(
            "Print job {} for invoice {} failed: {}",
            job.id, job.invoice_id, reason
        ),
        &job.id,
    );
    Ok(summary)
}


/// Cancel a queued job. Cancellation is its own outcome
/// (`cancelled`), not a failure: it emits
/// `printing.job.cancelled`, leaves `error_message` empty, and
/// sends no failure notification, so cancels never corrupt
/// failure metrics.
pub(crate) fn cancel_job(
    state: &PrintingState,
    job_id: &str,
) -> UnifiedResult<PrintJobSummary> {
    let mut job = load_job_for_transition(state, job_id)?;
    if job.status != "queued" {
        return Err(UnifiedError::conflict(format!(
            "Only a queued print job can be cancelled (current status: {})",
            job.status
        )));
    }
    job.status = "cancelled".to_string();
    job.error_message = None;
    job.completed_at = Some(now());
    job.updated_at = now();
    state.print_jobs.insert(job_id.to_string(), job.clone());
    let summary = job_summary(&job);
    state.emit(
        EVENT_PRINT_JOB_CANCELLED,
        json!({ "job": summary.clone() }),
    );
    Ok(summary)
}


pub(crate) async fn fail_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let result: UnifiedResult<PrintJobSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_JOB)?;
        let id = path.into_inner();
        let job = load_job_for_transition(&state, &id)?;
        require_same_tenant(job.tenant_id.as_deref(), &caller.tenant_id)?;
        let reason = body
            .get("error")
            .and_then(|value| value.as_str())
            .unwrap_or("Print failed");
        fail_job(&state, &id, reason)
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn cancel_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PrintJobSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_JOB)?;
        let id = path.into_inner();
        let job = load_job_for_transition(&state, &id)?;
        require_same_tenant(job.tenant_id.as_deref(), &caller.tenant_id)?;
        cancel_job(&state, &id)
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn reprint_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PrintJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_JOB)?;
        let id = path.into_inner();
        let job = load_job_for_transition(&state, &id)?;
        require_same_tenant(job.tenant_id.as_deref(), &caller.tenant_id)?;
        state.reprint_job(&id, &caller.actor)
    })();
    match result {
        Ok(job) => json::success_response(job, 201),
        Err(error) => json::error_response(&error, ctx.is_debug()),
    }
}


pub(crate) async fn thermal_preview(
    _state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<ThermalPreviewRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ThermalRender> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let input = body.into_inner();
        if input.lines.is_empty() {
            return Err(UnifiedError::bad_request(
                "A thermal preview needs at least one line",
            ));
        }
        if input.lines.len() > 500 {
            return Err(UnifiedError::bad_request(
                "A thermal preview holds at most 500 lines",
            ));
        }
        for line in &input.lines {
            validate_thermal_line(line)?;
        }
        if !(16..=120).contains(&input.width_columns) {
            return Err(UnifiedError::bad_request(
                "Thermal preview width must be between 16 and 120 columns",
            ));
        }
        let code_page = input.code_page.unwrap_or_else(|| "cp437".to_string());
        if !SUPPORTED_CODE_PAGES.contains(&code_page.as_str()) {
            return Err(UnifiedError::bad_request(
                "Thermal preview code page is not one of the supported code pages",
            ));
        }
        let doc = ThermalDocument {
            lines: input.lines,
            width_columns: input.width_columns,
            cut: input.cut,
            drawer_kick: input.drawer_kick,
        };
        Ok(render_thermal(&doc, &code_page))
    })();
    json::respond(result, ctx.is_debug())
}


#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::HookContext;
    use std::sync::Mutex;

    fn paper_job_request(invoice_id: &str) -> CreatePrintJobRequest {
        CreatePrintJobRequest {
            invoice_id: invoice_id.to_string(),
            company_id: None,
            template_id: None,
            template_version: None,
            paper_size_code: None,
            output_kind: "paper".to_string(),
            printer_profile_id: None,
            document_ref: None,
            document_html: Some("<html>invoice</html>".to_string()),
            thermal_lines: None,
            text: None,
        }
    }

    fn queued_job(state: &PrintingState, invoice_id: &str) -> PrintJob {
        state
            .create_job(paper_job_request(invoice_id), "user-1", Some("tenant-a"))
            .unwrap()
    }

    /// Records every lifecycle event the state emits, in order.
    fn emitted_events(state: &PrintingState) -> Arc<Mutex<Vec<String>>> {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        for event in [
            EVENT_PRINT_JOB_DISPATCH_REQUESTED,
            EVENT_PRINT_JOB_COMPLETED,
            EVENT_PRINT_JOB_FAILED,
            EVENT_PRINT_JOB_CANCELLED,
        ] {
            let seen = seen.clone();
            state.bus().register(
                event,
                "test",
                Arc::new(move |ctx: &mut HookContext| {
                    seen.lock()
                        .expect("events lock poisoned")
                        .push(ctx.event.clone());
                }),
            );
        }
        seen
    }

    fn job_status(state: &PrintingState, job_id: &str) -> String {
        state
            .print_jobs
            .get(job_id)
            .map(|entry| entry.value().status.clone())
            .unwrap()
    }

    #[test]
    fn dispatch_leaves_job_printing_until_dispatcher_confirms() {
        let state = PrintingState::new();
        let seen = emitted_events(&state);
        let job = queued_job(&state, "INV-1");
        let summary = dispatch_job(&state, &job.id, "user-1").unwrap();
        // Dispatch alone must not mark the job done: no completion
        // event, no "done" notification, no completed_at.
        assert_eq!(summary.status, "printing");
        assert_eq!(job_status(&state, &job.id), "printing");
        assert!(state.print_jobs.get(&job.id).unwrap().completed_at.is_none());
        let events = seen.lock().expect("events lock poisoned").clone();
        assert_eq!(events, vec![EVENT_PRINT_JOB_DISPATCH_REQUESTED.to_string()]);
        // Only the dispatcher's confirmation completes the job.
        assert!(complete_job(&state, &job.id).is_ok());
        assert_eq!(job_status(&state, &job.id), "done");
        let events = seen.lock().expect("events lock poisoned").clone();
        assert!(events.contains(&EVENT_PRINT_JOB_COMPLETED.to_string()));
        // Completing twice, or completing a queued job, is refused.
        assert!(complete_job(&state, &job.id).is_err());
        let queued = queued_job(&state, "INV-2");
        assert!(complete_job(&state, &queued.id).is_err());
        assert!(dispatch_job(&state, &job.id, "user-1").is_err());
    }

    #[test]
    fn cancel_records_cancelled_not_failed() {
        let state = PrintingState::new();
        let seen = emitted_events(&state);
        let job = queued_job(&state, "INV-1");
        let summary = cancel_job(&state, &job.id).unwrap();
        assert_eq!(summary.status, "cancelled");
        assert_eq!(summary.error_message, None);
        assert!(PRINT_JOB_STATUSES.contains(&"cancelled"));
        let events = seen.lock().expect("events lock poisoned").clone();
        assert_eq!(events, vec![EVENT_PRINT_JOB_CANCELLED.to_string()]);
        // A cancelled job is terminal: no second cancel, no fail.
        assert!(cancel_job(&state, &job.id).is_err());
        assert!(fail_job(&state, &job.id, "printer jam").is_err());
        // Only queued jobs cancel.
        let printing = queued_job(&state, "INV-2");
        dispatch_job(&state, &printing.id, "user-1").unwrap();
        assert!(cancel_job(&state, &printing.id).is_err());
    }

    #[test]
    fn fail_is_allowed_on_done_dispatched_jobs() {
        let state = PrintingState::new();
        let job = queued_job(&state, "INV-1");
        dispatch_job(&state, &job.id, "user-1").unwrap();
        // Legacy path: a job marked done before delivery was
        // confirmed, whose print then failed at the printer.
        if let Some(mut stored) = state.print_jobs.get_mut(&job.id) {
            stored.status = "done".to_string();
        }
        let summary = fail_job(&state, &job.id, "printer jam").unwrap();
        assert_eq!(summary.status, "failed");
        assert_eq!(summary.error_message.as_deref(), Some("printer jam"));
        // Failed jobs cannot fail again.
        assert!(fail_job(&state, &job.id, "again").is_err());
    }
}
