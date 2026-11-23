//! printing - hooks.

use unified_core::hooks::{HookContext};
use unified_core::json;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json};
use std::sync::{Arc};
use super::{events::*, paper::*, thermal::*, profiles::*, jobs::*, state::*, auth::*, handlers_paper::*, handlers_profiles::*, handlers_jobs::*, module::*};



// ---------------------------------------------------------------------------
// Hook handlers (other modules talk to printing through these)
// ---------------------------------------------------------------------------
pub(crate) fn on_print_job_requested(state: Arc<PrintingState>, ctx: &mut HookContext) {
    let payload = ctx.data.clone();
    let requested_by = payload
        .get("requested_by")
        .and_then(|value| value.as_str())
        .unwrap_or("system")
        .to_string();
    let parsed: Result<CreatePrintJobRequest, serde_json::Error> =
        serde_json::from_value(payload);
    // A hook payload may carry the caller's tenant (billing does
    // not yet); without it the job is tenant-less and visible to
    // every tenant — see `tenant_visible` in `auth.rs`.
    let tenant_id = ctx
        .data
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    match parsed {
        Ok(input) => match state.create_job(input, &requested_by, tenant_id.as_deref()) {
            Ok(job) => {
                ctx.set("job_id", json!(job.id.clone()));
                ctx.set("status", json!(job.status.clone()));
                ctx.add_result(json!({ "job_id": job.id, "status": job.status }));
            }
            Err(error) => {
                ctx.set("error", json!(error.message.clone()));
                ctx.add_result(json!({ "error": error.message }));
            }
        },
        Err(error) => {
            ctx.set("error", json!(error.to_string()));
            ctx.add_result(json!({ "error": error.to_string() }));
        }
    }
}


pub(crate) fn on_print_job_reprint_requested(state: Arc<PrintingState>, ctx: &mut HookContext) {
    let payload = ctx.data.clone();
    let job_id = payload
        .get("job_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let requested_by = payload
        .get("requested_by")
        .and_then(|value| value.as_str())
        .unwrap_or("system")
        .to_string();
    match state.reprint_job(&job_id, &requested_by) {
        Ok(job) => {
            ctx.set("job_id", json!(job.id.clone()));
            ctx.set("previous_job_id", json!(job_id.clone()));
            ctx.set("status", json!(job.status.clone()));
            ctx.add_result(json!({
                "job_id": job.id,
                "previous_job_id": job_id,
                "status": job.status,
            }));
        }
        Err(error) => {
            ctx.set("error", json!(error.message.clone()));
            ctx.add_result(json!({ "error": error.message }));
        }
    }
}


pub(crate) fn on_print_jobs_list_requested(state: Arc<PrintingState>, ctx: &mut HookContext) {
    let payload = ctx.data.clone();
    let invoice_id = payload
        .get("invoice_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let status_filter = payload
        .get("status")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let company_filter = payload
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    // Hook callers should pass their tenant id so the list is
    // tenant-scoped; without it the list is unscoped (billing
    // does not pass one yet) — see `tenant_visible` in `auth.rs`.
    let tenant_filter = payload
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut jobs: Vec<PrintJobSummary> = state
        .print_jobs
        .iter()
        .map(|entry| job_summary(entry.value()))
        .filter(|job| {
            job.invoice_id == invoice_id
                && tenant_filter.as_deref().is_none_or(|tenant| {
                    tenant_visible(job.tenant_id.as_deref(), tenant)
                })
                && status_filter
                    .as_deref()
                    .is_none_or(|status| job.status == status)
                && company_filter.as_deref().is_none_or(|company_id| {
                    job.company_id.as_deref() == Some(company_id)
                })
        })
        .collect();
    jobs.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    let total = jobs.len();
    ctx.set("jobs", json!(jobs.clone()));
    ctx.set("total", json!(total));
    ctx.add_result(json!({ "jobs": jobs, "total": total }));
}


pub(crate) fn on_school_fee_receipt_print_requested(state: Arc<PrintingState>, ctx: &mut HookContext) {
    let payload = ctx.data.clone();
    let text_field = |key: &str| {
        payload
            .get(key)
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string()
    };
    let receipt_number = text_field("receipt_number");
    if receipt_number.is_empty() {
        ctx.set("error", json!("school_fees.receipt.print_requested carries no receipt_number"));
        ctx.add_result(json!({ "error": "receipt_number is required" }));
        return;
    }
    let invoice_id = payload
        .get("invoice_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("receipt:{receipt_number}"));
    let mut lines = vec![
        ThermalLine {
            text: "PAYMENT RECEIPT".to_string(),
            align: "center".to_string(),
            bold: true,
            double_size: true,
        },
        ThermalLine {
            text: format!("Receipt: {receipt_number}"),
            align: "center".to_string(),
            bold: false,
            double_size: false,
        },
        ThermalLine {
            text: format!("Student: {}", text_field("student_user_id")),
            align: "left".to_string(),
            bold: false,
            double_size: false,
        },
        ThermalLine {
            text: format!("Fee: {}", text_field("fee_type")),
            align: "left".to_string(),
            bold: false,
            double_size: false,
        },
        ThermalLine {
            text: format!("Period: {}", text_field("period_label")),
            align: "left".to_string(),
            bold: false,
            double_size: false,
        },
        ThermalLine {
            text: format!(
                "Amount: {} {}",
                text_field("amount_text"),
                text_field("currency")
            ),
            align: "right".to_string(),
            bold: true,
            double_size: false,
        },
        ThermalLine {
            text: format!("Method: {}", text_field("method")),
            align: "left".to_string(),
            bold: false,
            double_size: false,
        },
    ];
    let reference = text_field("reference");
    if !reference.is_empty() {
        lines.push(ThermalLine {
            text: format!("Reference: {reference}"),
            align: "left".to_string(),
            bold: false,
            double_size: false,
        });
    }
    let issued_by = text_field("issued_by");
    if !issued_by.is_empty() {
        lines.push(ThermalLine {
            text: format!("Issued by: {issued_by}"),
            align: "left".to_string(),
            bold: false,
            double_size: false,
        });
    }
    let requested_by = if issued_by.is_empty() {
        "system".to_string()
    } else {
        issued_by
    };
    let input = CreatePrintJobRequest {
        invoice_id,
        company_id: None,
        template_id: None,
        template_version: None,
        paper_size_code: Some("thermal-80".to_string()),
        output_kind: "thermal".to_string(),
        printer_profile_id: None,
        document_ref: None,
        document_html: None,
        thermal_lines: Some(lines),
        text: None,
    };
    match state.create_job(input, &requested_by, None) {
        Ok(job) => {
            ctx.set("job_id", json!(job.id.clone()));
            ctx.set("status", json!(job.status.clone()));
            ctx.add_result(json!({ "job_id": job.id, "status": job.status }));
        }
        Err(error) => {
            ctx.set("error", json!(error.message.clone()));
            ctx.add_result(json!({ "error": error.message }));
        }
    }
}
