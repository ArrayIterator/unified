//! printing - state.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::{Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{events::*, paper::*, thermal::*, profiles::*, jobs::*, auth::*, hooks::*, handlers_paper::*, handlers_profiles::*, handlers_jobs::*, module::*};



// State
pub struct PrintingState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) paper_sizes: DashMap<String, PaperSize>,
    pub(crate) printer_profiles: DashMap<String, PrinterProfile>,
    pub(crate) print_jobs: DashMap<String, PrintJob>,
    pub(crate) company_default_paper_size: DashMap<String, String>,
}


impl PrintingState {
    pub(crate) fn new() -> Self {
        let state = Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            paper_sizes: DashMap::new(),
            printer_profiles: DashMap::new(),
            print_jobs: DashMap::new(),
            company_default_paper_size: DashMap::new(),
        };
        // Seed the standard sizes so the in-memory store agrees
        // with the migration seed from the first request.
        for size in STANDARD_PAPER_SIZES {
            if let Some(record) = standard_paper_size(size.code) {
                state.paper_sizes.insert(record.code.clone(), record);
            }
        }
        state
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Resolve a paper size by code: a stored custom row wins,
    /// otherwise the standard constant.
    pub(crate) fn paper_size(&self, code: &str) -> Option<PaperSize> {
        if let Some(entry) = self.paper_sizes.get(code) {
            return Some(entry.value().clone());
        }
        standard_paper_size(code)
    }

    /// Create and queue a print job from a validated request.
    /// Thermal jobs render their ESC/POS stream and preview now,
    /// so the bytes are fixed at queue time; paper, pdf, and
    /// email jobs keep the rendered document they were given.
    /// `tenant_id` is the caller's resolved tenant (HTTP) or
    /// `None` for hook-created jobs with no tenant context.
    pub(crate) fn create_job(
        &self,
        input: CreatePrintJobRequest,
        requested_by: &str,
        tenant_id: Option<&str>,
    ) -> UnifiedResult<PrintJob> {
        if input.invoice_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A print job needs an invoice id",
            ));
        }
        if !OUTPUT_KINDS.contains(&input.output_kind.as_str()) {
            return Err(UnifiedError::bad_request(
                "Print output kind must be paper, thermal, pdf, or email",
            ));
        }
        if let Some(code) = &input.paper_size_code {
            if self.paper_size(code).is_none() {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown paper size \"{code}\""
                )));
            }
        }
        let mut width_columns = 42u32;
        let mut code_page = "cp437".to_string();
        let mut supports_cut = true;
        let mut supports_drawer_kick = false;
        if let Some(profile_id) = &input.printer_profile_id {
            match self.printer_profiles.get(profile_id) {
                Some(entry) => {
                    let profile = entry.value();
                    width_columns = profile.width_columns;
                    code_page = profile.code_page.clone();
                    supports_cut = profile.supports_cut;
                    supports_drawer_kick = profile.supports_drawer_kick;
                }
                None => {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown printer profile \"{profile_id}\""
                    )));
                }
            }
        }
        let now = Utc::now();
        let mut job = PrintJob {
            id: Uuid::new_v4().to_string(),
            company_id: input.company_id.clone(),
            tenant_id: tenant_id.map(str::to_string),
            invoice_id: input.invoice_id.trim().to_string(),
            template_id: input.template_id.clone(),
            template_version: input.template_version,
            paper_size_code: input.paper_size_code.clone(),
            output_kind: input.output_kind.clone(),
            printer_profile_id: input.printer_profile_id.clone(),
            status: "queued".to_string(),
            document_ref: input.document_ref.clone(),
            document_html: None,
            escpos_base64: None,
            escpos_preview: None,
            error_message: None,
            attempts: 0,
            requested_by: requested_by.to_string(),
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
        };
        if input.output_kind == "thermal" {
            let mut lines: Vec<ThermalLine> = input.thermal_lines.unwrap_or_default();
            if let Some(text) = input.text {
                if !text.trim().is_empty() {
                    lines.push(ThermalLine {
                        text,
                        align: "left".to_string(),
                        bold: false,
                        double_size: false,
                    });
                }
            }
            if lines.is_empty() {
                return Err(UnifiedError::bad_request(
                    "A thermal print job needs thermal lines or text",
                ));
            }
            if lines.len() > 500 {
                return Err(UnifiedError::bad_request(
                    "A thermal print job holds at most 500 lines",
                ));
            }
            for line in &lines {
                validate_thermal_line(line)?;
            }
            let doc = ThermalDocument {
                lines,
                width_columns,
                cut: supports_cut,
                drawer_kick: supports_drawer_kick,
            };
            let rendered = render_thermal(&doc, &code_page);
            job.escpos_base64 = Some(rendered.escpos_base64);
            job.escpos_preview = Some(rendered.preview);
        } else {
            match input.document_html {
                Some(html) if !html.trim().is_empty() => {
                    if html.len() > 2_000_000 {
                        return Err(UnifiedError::bad_request(
                            "The print-ready document is too large (2 MB limit)",
                        ));
                    }
                    job.document_html = Some(html);
                }
                _ => {
                    return Err(UnifiedError::bad_request(
                        "A paper, pdf, or email print job needs the rendered print-ready document",
                    ));
                }
            }
        }
        self.print_jobs.insert(job.id.clone(), job.clone());
        Ok(job)
    }

    /// Queue a fresh copy of an existing job (reprint). The copy
    /// keeps the original payload, restarts at `queued` with zero
    /// attempts, and gets a new id. Only a finished job - `done`
    /// or `failed` - may be reprinted: reprinting a queued or
    /// printing job would silently duplicate it in the queue.
    pub(crate) fn reprint_job(&self, job_id: &str, requested_by: &str) -> UnifiedResult<PrintJob> {
        let original = self
            .print_jobs
            .get(job_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Print job not found"))?;
        if original.status != "done" && original.status != "failed" {
            return Err(UnifiedError::conflict(format!(
                "Only a done or failed print job can be reprinted (current status: {})",
                original.status
            )));
        }
        let now = Utc::now();
        let copy = PrintJob {
            id: Uuid::new_v4().to_string(),
            company_id: original.company_id.clone(),
            tenant_id: original.tenant_id.clone(),
            invoice_id: original.invoice_id.clone(),
            template_id: original.template_id.clone(),
            template_version: original.template_version,
            paper_size_code: original.paper_size_code.clone(),
            output_kind: original.output_kind.clone(),
            printer_profile_id: original.printer_profile_id.clone(),
            status: "queued".to_string(),
            document_ref: original.document_ref.clone(),
            document_html: original.document_html.clone(),
            escpos_base64: original.escpos_base64.clone(),
            escpos_preview: original.escpos_preview.clone(),
            error_message: None,
            attempts: 0,
            requested_by: requested_by.to_string(),
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
        };
        self.print_jobs.insert(copy.id.clone(), copy.clone());
        Ok(copy)
    }
}


pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}


pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}


#[cfg(test)]
mod tests {
    use super::*;

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

    fn set_status(state: &PrintingState, job_id: &str, status: &str) {
        if let Some(mut job) = state.print_jobs.get_mut(job_id) {
            job.status = status.to_string();
        }
    }

    #[test]
    fn create_job_stamps_the_tenant_and_reprint_keeps_it() {
        let state = PrintingState::new();
        let job = state
            .create_job(paper_job_request("INV-1"), "user-1", Some("tenant-a"))
            .unwrap();
        assert_eq!(job.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(job.status, "queued");
        // A hook-created job carries no tenant.
        let system_job = state
            .create_job(paper_job_request("INV-2"), "system", None)
            .unwrap();
        assert_eq!(system_job.tenant_id, None);
        // Reprint copies the original's tenant.
        set_status(&state, &job.id, "done");
        let copy = state.reprint_job(&job.id, "user-1").unwrap();
        assert_eq!(copy.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(copy.status, "queued");
    }

    #[test]
    fn reprint_rejects_queued_and_printing_jobs() {
        let state = PrintingState::new();
        let job = state
            .create_job(paper_job_request("INV-1"), "user-1", Some("tenant-a"))
            .unwrap();
        // Queued and printing jobs must not be duplicated.
        assert!(state.reprint_job(&job.id, "user-1").is_err());
        set_status(&state, &job.id, "printing");
        assert!(state.reprint_job(&job.id, "user-1").is_err());
        // Done and failed jobs may be reprinted.
        set_status(&state, &job.id, "done");
        assert!(state.reprint_job(&job.id, "user-1").is_ok());
        set_status(&state, &job.id, "failed");
        assert!(state.reprint_job(&job.id, "user-1").is_ok());
        // Cancelled jobs may not.
        set_status(&state, &job.id, "cancelled");
        assert!(state.reprint_job(&job.id, "user-1").is_err());
    }
}
