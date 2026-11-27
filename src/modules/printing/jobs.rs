//! printing - jobs.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use super::{events::*, paper::*, thermal::*, profiles::*, state::*, auth::*, hooks::*, handlers_paper::*, handlers_profiles::*, handlers_jobs::*, module::*};



#[derive(Debug, Clone, Serialize, Deserialize)]
// Print jobs
/// One print job: a queued request to print, export, or email one
/// invoice. `document_html` is the print-ready HTML document for
/// paper, pdf, and email jobs; `escpos_base64` and
/// `escpos_preview` are the rendered thermal output for thermal
/// jobs. The ESC/POS stream is regenerated from the stored lines
/// on reprint, so the bytes are never the only copy.
pub struct PrintJob {
    pub id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    /// Owning tenant id. `None` only for jobs created through a
    /// hook with no tenant context (billing does not pass one yet);
    /// jobs created over HTTP always carry the caller's tenant.
    #[serde(default)]
    pub tenant_id: Option<String>,
    pub invoice_id: String,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub template_version: Option<u32>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    pub output_kind: String,
    #[serde(default)]
    pub printer_profile_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub document_ref: Option<String>,
    #[serde(default)]
    pub document_html: Option<String>,
    #[serde(default)]
    pub escpos_base64: Option<String>,
    #[serde(default)]
    pub escpos_preview: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
    pub attempts: u32,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
}


/// The list view of a print job: everything except the heavy
/// `document_html` and `escpos_base64` payloads, which the
/// `/escpos` endpoint and the print-ready endpoints serve on
/// demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintJobSummary {
    pub id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub tenant_id: Option<String>,
    pub invoice_id: String,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub template_version: Option<u32>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    pub output_kind: String,
    #[serde(default)]
    pub printer_profile_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub document_ref: Option<String>,
    #[serde(default)]
    pub escpos_preview: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
    pub attempts: u32,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
}


pub(crate) fn job_summary(job: &PrintJob) -> PrintJobSummary {
    PrintJobSummary {
        id: job.id.clone(),
        company_id: job.company_id.clone(),
        tenant_id: job.tenant_id.clone(),
        invoice_id: job.invoice_id.clone(),
        template_id: job.template_id.clone(),
        template_version: job.template_version,
        paper_size_code: job.paper_size_code.clone(),
        output_kind: job.output_kind.clone(),
        printer_profile_id: job.printer_profile_id.clone(),
        status: job.status.clone(),
        document_ref: job.document_ref.clone(),
        escpos_preview: job.escpos_preview.clone(),
        error_message: job.error_message.clone(),
        attempts: job.attempts,
        requested_by: job.requested_by.clone(),
        created_at: job.created_at,
        updated_at: job.updated_at,
        started_at: job.started_at,
        completed_at: job.completed_at,
    }
}


/// Input for creating a print job, over HTTP and over the
/// `printing.job.requested` event alike. Thermal jobs carry
/// `thermal_lines` (or plain `text`); paper, pdf, and email jobs
/// carry the rendered `document_html`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePrintJobRequest {
    pub invoice_id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub template_version: Option<u32>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    #[serde(default = "default_output_kind")]
    pub output_kind: String,
    #[serde(default)]
    pub printer_profile_id: Option<String>,
    #[serde(default)]
    pub document_ref: Option<String>,
    #[serde(default)]
    pub document_html: Option<String>,
    #[serde(default)]
    pub thermal_lines: Option<Vec<ThermalLine>>,
    #[serde(default)]
    pub text: Option<String>,
}


pub(crate) fn default_output_kind() -> String {
    "paper".to_string()
}
