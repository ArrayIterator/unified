// Printing module — paper sizes, thermal (ESC/POS) printing,
// printer profiles, and the print-job queue.
//
// Routes live under `/api/v1/printing`:
//
//   Paper sizes:
//     GET/POST /paper-sizes — list (standards + custom) / create a
//       custom size. Standards are read-only through the API.
//     GET/PUT/DELETE /paper-sizes/{code} — one size. Only custom
//       sizes can be updated or deleted.
//     GET/PUT /paper-sizes/defaults/{company_id} — the default
//       paper size of one company.
//   Printer profiles:
//     GET/POST /printer-profiles — list / create.
//     GET/PUT/DELETE /printer-profiles/{id} — one profile.
//     POST /printer-profiles/{id}/set-default — default profile
//       for the profile's company (or the global default).
//   Print jobs:
//     GET/POST /print-jobs — list (paginated, filtered) / create.
//     GET  /print-jobs/{id} — one job.
//     GET  /print-jobs/{id}/escpos — ESC/POS stream (base64) plus
//       the human-readable text preview of a thermal job.
//     POST /print-jobs/{id}/process — hand the job to the
//       deployment-side dispatcher and mark it done.
//     POST /print-jobs/{id}/fail — mark a job failed with a reason.
//     POST /print-jobs/{id}/cancel — cancel a queued job.
//     POST /print-jobs/{id}/reprint — queue a fresh copy of a job.
//   Thermal preview (no job stored):
//     POST /thermal/preview — render thermal lines as text preview
//       plus the ESC/POS stream (base64).
//
// PAPER-SIZE RULE — READ THIS FIRST. Standard paper sizes ARE
// hard-coded here as constants (ISO 216 A0-A6, Letter, Legal,
// Tabloid, and the common thermal roll widths 58mm and 80mm)
// because they ARE international standards: their dimensions do
// not change per deployment. The same rows are seeded by migration
// 2026-11-22-000086_printing so the durable store agrees with the
// in-memory store. Everything else about paper is data: an admin
// can add custom sizes (width, height, margins) through the API,
// and each company can set its own default size. A standard size
// can never be edited or deleted through the API; a custom size
// can, and deleting one never deletes the standard rows.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// paper-size registry (standards + custom), printer profiles as
// data-driven records, ESC/POS byte-stream generation for thermal
// receipts and invoices (text, bold, double size, alignment, cut,
// cash-drawer kick where the profile supports it), a text preview
// of the thermal layout, and the print-job queue with history,
// reprint, and status transitions. What is NOT built here:
//   - A direct printer driver. The server EMITS the ESC/POS
//     stream (returned base64 from `/escpos` and from job
//     creation) and hands the job to the deployment through the
//     `printing.job.dispatch_requested` event. The actual
//     connection to hardware (USB, network socket, serial port)
//     is deployment-specific and is configured through the
//     printer profile (`connection_type` + `connection_target`);
//     no code here opens a device, socket, or serial port.
//   - A binary PDF generator. A `pdf` print job renders the
//     print-ready HTML document with `@page` rules for the chosen
//     paper size (built by the billing module from the invoice
//     template). Converting that HTML to PDF bytes is a deployment
//     step (headless browser or a configured converter hook);
//     this module never claims to produce PDF bytes.
//   - Mail attachments. The mail module accepts `to`, `subject`,
//     `body_text`, `body_html`, and `template_id` on
//     `mail.send_requested` and has no attachment field, so an
//     emailed invoice carries the print-ready document as the
//     HTML body. Attachment support belongs to the mail module.
//
// Interconnection happens only through the hook bus: this module
// listens to `printing.job.requested` (create a job, used by the
// billing module's invoice-print endpoints),
// `printing.job.reprint_requested`, and
// `printing.jobs.list_requested`, and it emits
// `printing.job.completed`, `printing.job.failed`,
// `printing.job.dispatch_requested`, `notification.requested`
// (to the user who requested the job, on completion and on
// failure), and `printing.paper_size.*` /
// `printing.printer_profile.*` change events for the audit log.
// Nothing in this module calls another module directly, and no
// behaviour is hard-coded outside the standard paper sizes, the
// ESC/POS command bytes (a published printer standard), and the
// code-page table below.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy::TenantContext as TenancyContext;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events and permissions
// ---------------------------------------------------------------------------

/// Create a print job. Payload: a `CreatePrintJobRequest` plus
/// `requested_by`. The handler answers by setting `job_id` and
/// `status` on the event payload (and adding the same object as a
/// result); on failure it sets `error` instead.
pub const EVENT_PRINT_JOB_REQUESTED: &str = "printing.job.requested";
/// Queue a fresh copy of an existing job. Payload: `{"job_id",
/// "requested_by"}`. Answers with `job_id` (the new job) and
/// `previous_job_id`.
pub const EVENT_PRINT_JOB_REPRINT_REQUESTED: &str = "printing.job.reprint_requested";
/// List print jobs for one invoice. Payload: `{"invoice_id",
/// "status"?, "company_id"?}`. Answers by setting `jobs` (an array
/// of `PrintJobSummary`) and `total` on the event payload.
pub const EVENT_PRINT_JOBS_LIST_REQUESTED: &str = "printing.jobs.list_requested";
/// Emitted when a job reaches `done`. Payload: the job summary.
pub const EVENT_PRINT_JOB_COMPLETED: &str = "printing.job.completed";
/// Emitted when a job reaches `failed`. Payload: the job summary
/// plus `error`.
pub const EVENT_PRINT_JOB_FAILED: &str = "printing.job.failed";
/// Emitted by the billing module when a school-fee receipt is
/// issued (`school_fees.receipt.print_requested`). The payload
/// carries the receipt fields (`receipt_number`,
/// `student_user_id`, `fee_type`, `period_label`, `amount_text`,
/// `currency`, `method`, `reference`, `invoice_id`, `issued_by`)
/// but no rendered document, so this module renders it as a
/// thermal receipt (80mm) from those fields. The billing module
/// only documents the event today — no emitter was found when this
/// handler was written — so the handler is forward-compatible: it
/// answers with `job_id` and `status` whenever the event arrives.
pub const EVENT_SCHOOL_FEE_RECEIPT_PRINT_REQUESTED: &str =
    "school_fees.receipt.print_requested";
/// Emitted by `process`: the deployment-side dispatcher owns
/// delivery to hardware from here. Payload: the job summary plus
/// the ESC/POS stream (base64) for thermal jobs.
pub const EVENT_PRINT_JOB_DISPATCH_REQUESTED: &str = "printing.job.dispatch_requested";
/// Paper-size change events, for the audit log.
pub const EVENT_PAPER_SIZE_CREATED: &str = "printing.paper_size.created";
pub const EVENT_PAPER_SIZE_UPDATED: &str = "printing.paper_size.updated";
pub const EVENT_PAPER_SIZE_DELETED: &str = "printing.paper_size.deleted";
/// Printer-profile change events, for the audit log.
pub const EVENT_PRINTER_PROFILE_CREATED: &str = "printing.printer_profile.created";
pub const EVENT_PRINTER_PROFILE_UPDATED: &str = "printing.printer_profile.updated";
pub const EVENT_PRINTER_PROFILE_DELETED: &str = "printing.printer_profile.deleted";

/// Read paper sizes, printer profiles, and print jobs.
const PERM_PRINTING_VIEW: &str = "printing.view";
/// Create, update, and delete paper sizes and printer profiles.
const PERM_PRINTING_MANAGE: &str = "printing.manage";
/// Create, process, fail, cancel, and reprint print jobs.
const PERM_PRINTING_JOB: &str = "printing.job";

/// Output kinds a print job may request. Kept as data (validated
/// against this list), not as an enum, so a deployment can add a
/// kind without a code change only by extending this list — the
/// list itself is the closed vocabulary the job state machine
/// understands.
pub const OUTPUT_KINDS: &[&str] = &["paper", "thermal", "pdf", "email"];
/// Job lifecycle states, in order. `cancelled` is not a state: a
/// cancelled job is recorded as `failed` with the error message
/// `Cancelled by the requester`, so the vocabulary stays closed.
pub const PRINT_JOB_STATUSES: &[&str] = &["queued", "printing", "done", "failed"];
/// Connection kinds a printer profile may declare.
pub const PRINTER_CONNECTION_TYPES: &[&str] = &["network", "usb", "serial", "file", "cloud"];
/// Character sets / code pages a printer profile may declare.
/// The values are the names the ESC/POS `ESC t n` command
/// understands; `code_page_value` maps them to command bytes.
pub const SUPPORTED_CODE_PAGES: &[&str] = &[
    "cp437", "cp850", "cp852", "cp858", "cp860", "cp863", "cp865", "cp866", "cp1252",
];

/// ESC/POS code-page table (ESC/POS standard): name to the `n`
/// byte of `ESC t n`. Unknown names fall back to 0 (PC437), the
/// printer default, instead of failing the job.
fn code_page_value(name: &str) -> u8 {
    match name {
        "cp437" => 0,
        "cp850" => 2,
        "cp860" => 3,
        "cp863" => 5,
        "cp865" => 7,
        "cp866" => 17,
        "cp852" => 18,
        "cp858" => 19,
        "cp1252" => 16,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Paper sizes
// ---------------------------------------------------------------------------

/// One standard paper size, as a compile-time constant. See the
/// module docs for why standards are constants while custom sizes
/// are data.
#[derive(Debug, Clone, Copy)]
pub struct StandardPaperSize {
    pub code: &'static str,
    pub name: &'static str,
    pub width_mm: f64,
    pub height_mm: Option<f64>,
    pub is_thermal: bool,
    pub thermal_width_mm: Option<u32>,
}

/// The standard paper sizes. ISO 216 A0-A6, Letter, Legal,
/// Tabloid, and the common thermal roll widths 58mm and 80mm.
/// Thermal rows have no height: a roll is continuous.
pub const STANDARD_PAPER_SIZES: &[StandardPaperSize] = &[
    StandardPaperSize { code: "a0", name: "A0", width_mm: 841.0, height_mm: Some(1189.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a1", name: "A1", width_mm: 594.0, height_mm: Some(841.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a2", name: "A2", width_mm: 420.0, height_mm: Some(594.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a3", name: "A3", width_mm: 297.0, height_mm: Some(420.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a4", name: "A4", width_mm: 210.0, height_mm: Some(297.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a5", name: "A5", width_mm: 148.0, height_mm: Some(210.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "a6", name: "A6", width_mm: 105.0, height_mm: Some(148.0), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "letter", name: "Letter", width_mm: 215.9, height_mm: Some(279.4), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "legal", name: "Legal", width_mm: 215.9, height_mm: Some(355.6), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "tabloid", name: "Tabloid", width_mm: 279.4, height_mm: Some(431.8), is_thermal: false, thermal_width_mm: None },
    StandardPaperSize { code: "thermal-58", name: "Thermal 58mm", width_mm: 58.0, height_mm: None, is_thermal: true, thermal_width_mm: Some(58) },
    StandardPaperSize { code: "thermal-80", name: "Thermal 80mm", width_mm: 80.0, height_mm: None, is_thermal: true, thermal_width_mm: Some(80) },
];

/// True when `code` names a standard paper size.
pub fn is_standard_paper_size(code: &str) -> bool {
    STANDARD_PAPER_SIZES
        .iter()
        .any(|size| size.code == code)
}

/// The standard size as a `PaperSize` record, with the seeded
/// margins (10mm for sheet sizes, 2mm for thermal rolls).
pub fn standard_paper_size(code: &str) -> Option<PaperSize> {
    let standard = STANDARD_PAPER_SIZES
        .iter()
        .find(|size| size.code == code)?;
    let margin = if standard.is_thermal { 2.0 } else { 10.0 };
    let now = Utc::now();
    Some(PaperSize {
        code: standard.code.to_string(),
        name: standard.name.to_string(),
        width_mm: standard.width_mm,
        height_mm: standard.height_mm,
        margin_top_mm: margin,
        margin_bottom_mm: margin,
        margin_left_mm: margin,
        margin_right_mm: margin,
        is_standard: true,
        is_thermal: standard.is_thermal,
        thermal_width_mm: standard.thermal_width_mm,
        company_id: None,
        created_by: None,
        created_at: now,
        updated_at: now,
    })
}

/// One paper size: a standard row or an admin-created custom row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperSize {
    pub code: String,
    pub name: String,
    pub width_mm: f64,
    pub height_mm: Option<f64>,
    pub margin_top_mm: f64,
    pub margin_bottom_mm: f64,
    pub margin_left_mm: f64,
    pub margin_right_mm: f64,
    pub is_standard: bool,
    pub is_thermal: bool,
    pub thermal_width_mm: Option<u32>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a custom paper size. Standards cannot be
/// created through the API: their codes are rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePaperSize {
    pub code: String,
    pub name: String,
    pub width_mm: f64,
    #[serde(default)]
    pub height_mm: Option<f64>,
    #[serde(default)]
    pub margin_top_mm: Option<f64>,
    #[serde(default)]
    pub margin_bottom_mm: Option<f64>,
    #[serde(default)]
    pub margin_left_mm: Option<f64>,
    #[serde(default)]
    pub margin_right_mm: Option<f64>,
    #[serde(default)]
    pub is_thermal: bool,
    #[serde(default)]
    pub thermal_width_mm: Option<u32>,
    #[serde(default)]
    pub company_id: Option<String>,
}

/// Input for updating a custom paper size. `height_mm` uses the
/// nested option so JSON `null` clears the height (thermal roll)
/// while an absent key leaves it alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePaperSize {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub width_mm: Option<f64>,
    #[serde(default)]
    pub height_mm: Option<Option<f64>>,
    #[serde(default)]
    pub margin_top_mm: Option<f64>,
    #[serde(default)]
    pub margin_bottom_mm: Option<f64>,
    #[serde(default)]
    pub margin_left_mm: Option<f64>,
    #[serde(default)]
    pub margin_right_mm: Option<f64>,
    #[serde(default)]
    pub is_thermal: Option<bool>,
    #[serde(default)]
    pub thermal_width_mm: Option<Option<u32>>,
    #[serde(default)]
    pub company_id: Option<Option<String>>,
}

/// A paper-size code is a lowercase slug.
fn valid_paper_size_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Validate the dimensions of a (custom) paper size. Width is
/// 20mm to 1500mm; sheet sizes need a height in the same range;
/// thermal sizes must have no height and must name their roll
/// width; margins must fit inside the sheet.
fn validate_paper_size_dimensions(
    width_mm: f64,
    height_mm: Option<f64>,
    margin_top_mm: f64,
    margin_bottom_mm: f64,
    margin_left_mm: f64,
    margin_right_mm: f64,
    is_thermal: bool,
    thermal_width_mm: Option<u32>,
) -> UnifiedResult<()> {
    if !width_mm.is_finite() || !(20.0..=1500.0).contains(&width_mm) {
        return Err(UnifiedError::bad_request(
            "Paper width must be between 20mm and 1500mm",
        ));
    }
    if is_thermal {
        if height_mm.is_some() {
            return Err(UnifiedError::bad_request(
                "A thermal (roll) paper size has no height",
            ));
        }
        if thermal_width_mm.is_none() {
            return Err(UnifiedError::bad_request(
                "A thermal paper size needs its roll width in millimetres",
            ));
        }
    } else {
        if thermal_width_mm.is_some() {
            return Err(UnifiedError::bad_request(
                "Only a thermal paper size carries a roll width",
            ));
        }
        match height_mm {
            Some(height) if height.is_finite() && (20.0..=1500.0).contains(&height) => {}
            _ => {
                return Err(UnifiedError::bad_request(
                    "A sheet paper size needs a height between 20mm and 1500mm",
                ));
            }
        }
    }
    for (label, margin) in [
        ("top margin", margin_top_mm),
        ("bottom margin", margin_bottom_mm),
        ("left margin", margin_left_mm),
        ("right margin", margin_right_mm),
    ] {
        if !margin.is_finite() || !(0.0..=100.0).contains(&margin) {
            return Err(UnifiedError::bad_request(format!(
                "Paper {label} must be between 0mm and 100mm"
            )));
        }
    }
    if margin_left_mm + margin_right_mm >= width_mm {
        return Err(UnifiedError::bad_request(
            "Left and right margins must fit inside the paper width",
        ));
    }
    if let Some(height) = height_mm {
        if margin_top_mm + margin_bottom_mm >= height {
            return Err(UnifiedError::bad_request(
                "Top and bottom margins must fit inside the paper height",
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Thermal (ESC/POS) rendering
// ---------------------------------------------------------------------------

/// ESC/POS control bytes. These are the published ESC/POS command
/// set, not application behaviour: `ESC @` initialise, `ESC a n`
/// alignment, `ESC E n` bold, `GS ! n` character size, `ESC t n`
/// code page, `ESC p m t1 t2` drawer kick, `GS V m n` cut.
const ESC: u8 = 0x1B;
const GS: u8 = 0x1D;

/// One line of a thermal document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalLine {
    pub text: String,
    /// `left`, `center`, or `right`. Defaults to `left`.
    #[serde(default = "default_thermal_align")]
    pub align: String,
    #[serde(default)]
    pub bold: bool,
    /// Double width and double height.
    #[serde(default)]
    pub double_size: bool,
}

fn default_thermal_align() -> String {
    "left".to_string()
}

/// A whole thermal document: lines plus the page-level options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalDocument {
    pub lines: Vec<ThermalLine>,
    pub width_columns: u32,
    #[serde(default)]
    pub cut: bool,
    #[serde(default)]
    pub drawer_kick: bool,
}

/// Validate one thermal line: text at most 1000 chars, alignment
/// from the closed vocabulary.
fn validate_thermal_line(line: &ThermalLine) -> UnifiedResult<()> {
    if line.text.chars().count() > 1000 {
        return Err(UnifiedError::bad_request(
            "A thermal line must be at most 1000 characters",
        ));
    }
    if !["left", "center", "right"].contains(&line.align.as_str()) {
        return Err(UnifiedError::bad_request(
            "Thermal line alignment must be left, center, or right",
        ));
    }
    Ok(())
}

/// Wrap `text` to `width` columns, splitting over-long words.
/// Character-count based: ESC/POS printers are monospace, so one
/// character is one column for the Latin scripts these printers
/// target.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        if raw.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in raw.split(' ') {
            let word_len = word.chars().count();
            if word_len > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                let mut rest = word;
                while rest.chars().count() > width {
                    let cut: String = rest.chars().take(width).collect();
                    lines.push(cut.clone());
                    rest = &rest[cut.len()..];
                }
                current = rest.to_string();
                continue;
            }
            if current.is_empty() {
                current = word.to_string();
            } else if current.chars().count() + 1 + word_len <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(std::mem::take(&mut current));
                current = word.to_string();
            }
        }
        lines.push(current);
    }
    lines
}

/// Pad one already-wrapped line to `width` columns per its
/// alignment. Lines at or over the width are returned unchanged.
fn align_text(text: &str, width: usize, align: &str) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.to_string();
    }
    match align {
        "center" => {
            let left = (width - len) / 2;
            format!("{}{}", " ".repeat(left), text)
        }
        "right" => format!("{}{}", " ".repeat(width - len), text),
        _ => text.to_string(),
    }
}

/// Render a thermal document as human-readable text: the preview
/// shown before printing, with alignment applied and no control
/// bytes.
pub fn render_thermal_preview(doc: &ThermalDocument) -> String {
    let width = doc.width_columns.max(16) as usize;
    let mut out = String::new();
    for line in &doc.lines {
        for wrapped in wrap_text(&line.text, width) {
            out.push_str(&align_text(&wrapped, width, &line.align));
            out.push('\n');
        }
    }
    out
}

/// Generate the ESC/POS byte stream for a thermal document.
/// The stream is what a printer consumes; the caller decides how
/// it reaches the printer (see the module docs).
pub fn escpos_bytes(doc: &ThermalDocument, code_page: &str) -> Vec<u8> {
    let mut out = vec![ESC, b'@', ESC, b't', code_page_value(code_page)];
    let width = doc.width_columns.max(16) as usize;
    for line in &doc.lines {
        let align_byte = match line.align.as_str() {
            "center" => 1,
            "right" => 2,
            _ => 0,
        };
        out.extend_from_slice(&[ESC, b'a', align_byte]);
        out.extend_from_slice(&[ESC, b'E', u8::from(line.bold)]);
        out.extend_from_slice(&[GS, b'!', if line.double_size { 0x11 } else { 0x00 }]);
        for wrapped in wrap_text(&line.text, width) {
            out.extend_from_slice(wrapped.as_bytes());
            out.push(b'\n');
        }
        out.extend_from_slice(&[ESC, b'E', 0, GS, b'!', 0x00]);
    }
    out.extend_from_slice(&[ESC, b'a', 0]);
    if doc.drawer_kick {
        out.extend_from_slice(&[ESC, b'p', 0, 25, 250]);
    }
    if doc.cut {
        out.extend_from_slice(&[GS, b'V', 66, 0]);
    }
    out
}

/// Render one thermal document both ways: the ESC/POS stream
/// (base64) and its text preview.
pub fn render_thermal(doc: &ThermalDocument, code_page: &str) -> ThermalRender {
    ThermalRender {
        escpos_base64: BASE64.encode(escpos_bytes(doc, code_page)),
        preview: render_thermal_preview(doc),
    }
}

/// The two renderings of a thermal document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalRender {
    pub escpos_base64: String,
    pub preview: String,
}

/// Input for the stateless thermal preview endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalPreviewRequest {
    pub lines: Vec<ThermalLine>,
    #[serde(default = "default_preview_width")]
    pub width_columns: u32,
    #[serde(default)]
    pub code_page: Option<String>,
    #[serde(default)]
    pub cut: bool,
    #[serde(default)]
    pub drawer_kick: bool,
}

fn default_preview_width() -> u32 {
    42
}

// ---------------------------------------------------------------------------
// Printer profiles
// ---------------------------------------------------------------------------

/// One thermal printer profile: how to reach the printer and what
/// it can do. `connection_target` is an address, never a secret:
/// `192.168.1.50:9100` for a network printer, `/dev/usb/lp0` for a
/// USB printer, a device path for serial, a file path for a spool
/// file, or a connector id for a cloud printer. When the printer
/// needs credentials, `auth_token_ref` holds a reference to the
/// credential in the vault or secrets store — never the credential
/// itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrinterProfile {
    pub id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    pub name: String,
    pub connection_type: String,
    #[serde(default)]
    pub connection_target: Option<String>,
    #[serde(default)]
    pub auth_token_ref: Option<String>,
    pub width_columns: u32,
    pub character_set: String,
    pub code_page: String,
    pub supports_drawer_kick: bool,
    pub supports_cut: bool,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    pub is_default: bool,
    #[serde(default)]
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a printer profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePrinterProfile {
    #[serde(default)]
    pub company_id: Option<String>,
    pub name: String,
    #[serde(default = "default_connection_type")]
    pub connection_type: String,
    #[serde(default)]
    pub connection_target: Option<String>,
    #[serde(default)]
    pub auth_token_ref: Option<String>,
    #[serde(default = "default_profile_width")]
    pub width_columns: u32,
    #[serde(default = "default_code_page")]
    pub character_set: String,
    #[serde(default = "default_code_page")]
    pub code_page: String,
    #[serde(default)]
    pub supports_drawer_kick: bool,
    #[serde(default = "default_true")]
    pub supports_cut: bool,
    #[serde(default)]
    pub paper_size_code: Option<String>,
}

fn default_connection_type() -> String {
    "network".to_string()
}

fn default_profile_width() -> u32 {
    42
}

fn default_code_page() -> String {
    "cp437".to_string()
}

fn default_true() -> bool {
    true
}

/// Input for updating a printer profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePrinterProfile {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub connection_type: Option<String>,
    #[serde(default)]
    pub connection_target: Option<Option<String>>,
    #[serde(default)]
    pub auth_token_ref: Option<Option<String>>,
    #[serde(default)]
    pub width_columns: Option<u32>,
    #[serde(default)]
    pub character_set: Option<String>,
    #[serde(default)]
    pub code_page: Option<String>,
    #[serde(default)]
    pub supports_drawer_kick: Option<bool>,
    #[serde(default)]
    pub supports_cut: Option<bool>,
    #[serde(default)]
    pub paper_size_code: Option<Option<String>>,
}

/// Validate a printer profile's connection and character settings.
/// `auth_token_ref` is checked only for shape (a short reference
/// with no whitespace): the vault owns the secret itself.
fn validate_printer_profile(
    connection_type: &str,
    connection_target: Option<&str>,
    auth_token_ref: Option<&str>,
    width_columns: u32,
    character_set: &str,
    code_page: &str,
    paper_size_code: Option<&str>,
) -> UnifiedResult<()> {
    if !PRINTER_CONNECTION_TYPES.contains(&connection_type) {
        return Err(UnifiedError::bad_request(
            "Printer connection type must be network, usb, serial, file, or cloud",
        ));
    }
    match connection_target {
        Some(target) if !target.trim().is_empty() && target.len() <= 512 => {}
        _ => {
            return Err(UnifiedError::bad_request(
                "A printer profile needs a connection target (address, device path, or connector id)",
            ));
        }
    }
    if let Some(reference) = auth_token_ref {
        if reference.trim().is_empty()
            || reference.len() > 128
            || reference.chars().any(char::is_whitespace)
        {
            return Err(UnifiedError::bad_request(
                "auth_token_ref must be a short vault reference without whitespace, never a raw secret",
            ));
        }
    }
    if !(16..=120).contains(&width_columns) {
        return Err(UnifiedError::bad_request(
            "Printer width must be between 16 and 120 columns",
        ));
    }
    if !SUPPORTED_CODE_PAGES.contains(&character_set) {
        return Err(UnifiedError::bad_request(
            "Printer character set is not one of the supported code pages",
        ));
    }
    if !SUPPORTED_CODE_PAGES.contains(&code_page) {
        return Err(UnifiedError::bad_request(
            "Printer code page is not one of the supported code pages",
        ));
    }
    if let Some(code) = paper_size_code {
        if code.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Printer paper size code must not be empty",
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Print jobs
// ---------------------------------------------------------------------------

/// One print job: a queued request to print, export, or email one
/// invoice. `document_html` is the print-ready HTML document for
/// paper, pdf, and email jobs; `escpos_base64` and
/// `escpos_preview` are the rendered thermal output for thermal
/// jobs. The ESC/POS stream is regenerated from the stored lines
/// on reprint, so the bytes are never the only copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintJob {
    pub id: String,
    #[serde(default)]
    pub company_id: Option<String>,
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

fn job_summary(job: &PrintJob) -> PrintJobSummary {
    PrintJobSummary {
        id: job.id.clone(),
        company_id: job.company_id.clone(),
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

fn default_output_kind() -> String {
    "paper".to_string()
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct PrintingState {
    hooks: RwLock<Arc<HookBus>>,
    paper_sizes: DashMap<String, PaperSize>,
    printer_profiles: DashMap<String, PrinterProfile>,
    print_jobs: DashMap<String, PrintJob>,
    company_default_paper_size: DashMap<String, String>,
}

impl PrintingState {
    fn new() -> Self {
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

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Resolve a paper size by code: a stored custom row wins,
    /// otherwise the standard constant.
    fn paper_size(&self, code: &str) -> Option<PaperSize> {
        if let Some(entry) = self.paper_sizes.get(code) {
            return Some(entry.value().clone());
        }
        standard_paper_size(code)
    }

    /// Create and queue a print job from a validated request.
    /// Thermal jobs render their ESC/POS stream and preview now,
    /// so the bytes are fixed at queue time; paper, pdf, and
    /// email jobs keep the rendered document they were given.
    fn create_job(
        &self,
        input: CreatePrintJobRequest,
        requested_by: &str,
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
    /// attempts, and gets a new id.
    fn reprint_job(&self, job_id: &str, requested_by: &str) -> UnifiedResult<PrintJob> {
        let original = self
            .print_jobs
            .get(job_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Print job not found"))?;
        let now = Utc::now();
        let copy = PrintJob {
            id: Uuid::new_v4().to_string(),
            company_id: original.company_id.clone(),
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

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// Caller identity and capability checks
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the assets and POS modules.
struct Caller {
    actor: String,
    role: String,
}

/// Resolve the tenant context for a request. THE single
/// convergence point for tenant-scoped data access in this file
/// (see `crate::core::tenancy`): this function returns the tenancy
/// `TenantContext` (tenant id as Uuid + schema name derived only
/// by `tenancy::schema_name()`), and the Diesel persistence wiring
/// — when it lands — takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means the request is refused
/// here, never served from `public`.
///
/// OPEN (not closed by this guard): the in-memory DashMap stores
/// in this module carry no `tenant_id` on their records yet, so
/// there is no per-record tenant filtering to apply once the
/// context is resolved — that needs a store-shape change (tenant
/// id on every record + a migration column) and is integrator
/// work. What this guard does close is the context requirement
/// itself: every tenant-data endpoint resolves the tenant first
/// through `caller()`, and a request with no tenant context is
/// rejected before any business data is read.
fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // Tenant first: no resolved tenant context means fail closed,
    // never fall back to the public schema. The resolved context
    // is bound for the Diesel persistence layer (see
    // `resolve_tenant`); the in-memory stores below carry no
    // tenant id yet, so per-record filtering is still open.
    let _tenant = resolve_tenant(req)?;
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the assets module's capability check).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Notify one user through `notification.requested`. The
/// notifications module resolves `user_ids`; when it is not
/// registered the event simply has no listener, which is fine.
fn notify_user(state: &PrintingState, user_id: &str, title: &str, body: &str, job_id: &str) {
    state.emit(
        "notification.requested",
        json!({
            "kind": "invoice",
            "title": title,
            "body": body,
            "user_ids": [user_id],
            "module": "printing",
            "priority": "normal",
            "link": format!("/api/v1/printing/print-jobs/{job_id}"),
        }),
    );
}

// ---------------------------------------------------------------------------
// Hook handlers (other modules talk to printing through these)
// ---------------------------------------------------------------------------

fn on_print_job_requested(state: Arc<PrintingState>, ctx: &mut HookContext) {
    let payload = ctx.data.clone();
    let requested_by = payload
        .get("requested_by")
        .and_then(|value| value.as_str())
        .unwrap_or("system")
        .to_string();
    let parsed: Result<CreatePrintJobRequest, serde_json::Error> =
        serde_json::from_value(payload);
    match parsed {
        Ok(input) => match state.create_job(input, &requested_by) {
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

fn on_print_job_reprint_requested(state: Arc<PrintingState>, ctx: &mut HookContext) {
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

fn on_print_jobs_list_requested(state: Arc<PrintingState>, ctx: &mut HookContext) {
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
    let mut jobs: Vec<PrintJobSummary> = state
        .print_jobs
        .iter()
        .map(|entry| job_summary(entry.value()))
        .filter(|job| {
            job.invoice_id == invoice_id
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

fn on_school_fee_receipt_print_requested(state: Arc<PrintingState>, ctx: &mut HookContext) {
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
    match state.create_job(input, &requested_by) {
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

// ---------------------------------------------------------------------------
// Paper-size handlers
// ---------------------------------------------------------------------------

async fn list_paper_sizes(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<PaperSize>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<PaperSize> = state
            .paper_sizes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|size| {
                matches_search(&format!("{} {}", size.code, size.name), &params)
                    && params
                        .filters
                        .get("is_standard")
                        .is_none_or(|flag| size.is_standard.to_string() == *flag)
                    && params
                        .filters
                        .get("is_thermal")
                        .is_none_or(|flag| size.is_thermal.to_string() == *flag)
                    && params.filters.get("company_id").is_none_or(|company_id| {
                        size.company_id.as_deref() == Some(company_id.as_str())
                    })
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreatePaperSize>,
) -> HttpResponse {
    let result: UnifiedResult<PaperSize> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let input = body.into_inner();
        let code = input.code.trim().to_lowercase();
        if !valid_paper_size_code(&code) {
            return Err(UnifiedError::bad_request(
                "Paper size code must be a lowercase slug (letters, digits, - and _)",
            ));
        }
        if is_standard_paper_size(&code) {
            return Err(UnifiedError::conflict(format!(
                "Paper size \"{code}\" is a standard size and cannot be created as a custom size"
            )));
        }
        if state.paper_sizes.contains_key(&code) {
            return Err(UnifiedError::conflict(format!(
                "Paper size \"{code}\" already exists"
            )));
        }
        if input.name.trim().is_empty() || input.name.len() > 120 {
            return Err(UnifiedError::bad_request(
                "Paper size name must be 1 to 120 characters",
            ));
        }
        let margins = (
            input.margin_top_mm.unwrap_or(10.0),
            input.margin_bottom_mm.unwrap_or(10.0),
            input.margin_left_mm.unwrap_or(10.0),
            input.margin_right_mm.unwrap_or(10.0),
        );
        validate_paper_size_dimensions(
            input.width_mm,
            input.height_mm,
            margins.0,
            margins.1,
            margins.2,
            margins.3,
            input.is_thermal,
            input.thermal_width_mm,
        )?;
        let now = now();
        let size = PaperSize {
            code: code.clone(),
            name: input.name.trim().to_string(),
            width_mm: input.width_mm,
            height_mm: input.height_mm,
            margin_top_mm: margins.0,
            margin_bottom_mm: margins.1,
            margin_left_mm: margins.2,
            margin_right_mm: margins.3,
            is_standard: false,
            is_thermal: input.is_thermal,
            thermal_width_mm: input.thermal_width_mm,
            company_id: input.company_id,
            created_by: Some(caller.actor.clone()),
            created_at: now,
            updated_at: now,
        };
        state.paper_sizes.insert(code, size.clone());
        state.emit(
            EVENT_PAPER_SIZE_CREATED,
            json!({ "code": size.code.clone(), "company_id": size.company_id.clone(), "created_by": caller.actor }),
        );
        Ok(size)
    })();
    match result {
        Ok(size) => json::success_response(size, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PaperSize> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        state
            .paper_size(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Paper size not found"))
    })();
    json::respond(result, false)
}

async fn update_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePaperSize>,
) -> HttpResponse {
    let result: UnifiedResult<PaperSize> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let code = path.into_inner();
        if is_standard_paper_size(&code) {
            return Err(UnifiedError::forbidden(
                "Standard paper sizes cannot be edited; create a custom size instead",
            ));
        }
        let mut size = state
            .paper_sizes
            .get(&code)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Paper size not found"))?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            if name.trim().is_empty() || name.len() > 120 {
                return Err(UnifiedError::bad_request(
                    "Paper size name must be 1 to 120 characters",
                ));
            }
            size.name = name.trim().to_string();
        }
        if let Some(width) = input.width_mm {
            size.width_mm = width;
        }
        if let Some(height) = input.height_mm {
            size.height_mm = height;
        }
        if let Some(margin) = input.margin_top_mm {
            size.margin_top_mm = margin;
        }
        if let Some(margin) = input.margin_bottom_mm {
            size.margin_bottom_mm = margin;
        }
        if let Some(margin) = input.margin_left_mm {
            size.margin_left_mm = margin;
        }
        if let Some(margin) = input.margin_right_mm {
            size.margin_right_mm = margin;
        }
        if let Some(is_thermal) = input.is_thermal {
            size.is_thermal = is_thermal;
        }
        if let Some(thermal_width) = input.thermal_width_mm {
            size.thermal_width_mm = thermal_width;
        }
        if let Some(company_id) = input.company_id {
            size.company_id = company_id;
        }
        validate_paper_size_dimensions(
            size.width_mm,
            size.height_mm,
            size.margin_top_mm,
            size.margin_bottom_mm,
            size.margin_left_mm,
            size.margin_right_mm,
            size.is_thermal,
            size.thermal_width_mm,
        )?;
        size.updated_at = now();
        state.paper_sizes.insert(code.clone(), size.clone());
        state.emit(
            EVENT_PAPER_SIZE_UPDATED,
            json!({ "code": size.code.clone(), "updated_by": caller.actor }),
        );
        Ok(size)
    })();
    json::respond(result, false)
}

async fn delete_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let code = path.into_inner();
        if is_standard_paper_size(&code) {
            return Err(UnifiedError::forbidden(
                "Standard paper sizes cannot be deleted",
            ));
        }
        let removed = state.paper_sizes.remove(&code);
        if removed.is_none() {
            return Err(UnifiedError::not_found("Paper size not found"));
        }
        state.emit(
            EVENT_PAPER_SIZE_DELETED,
            json!({ "code": code.clone(), "deleted_by": caller.actor }),
        );
        Ok(json!({ "deleted": true, "code": code }))
    })();
    json::respond(result, false)
}

async fn get_default_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let company_id = path.into_inner();
        let code = state
            .company_default_paper_size
            .get(&company_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| "a4".to_string());
        let size = state
            .paper_size(&code)
            .ok_or_else(|| UnifiedError::not_found("Default paper size not found"))?;
        Ok(json!({ "company_id": company_id, "paper_size_code": code, "paper_size": size }))
    })();
    json::respond(result, false)
}

async fn set_default_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let company_id = path.into_inner();
        if company_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("company_id must not be empty"));
        }
        let code = body
            .get("paper_size_code")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let size = state
            .paper_size(&code)
            .ok_or_else(|| UnifiedError::bad_request("Unknown paper size"))?;
        state
            .company_default_paper_size
            .insert(company_id.clone(), code.clone());
        Ok(json!({ "company_id": company_id, "paper_size_code": code, "paper_size": size, "set_by": caller.actor }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Printer-profile handlers
// ---------------------------------------------------------------------------

async fn list_printer_profiles(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<PrinterProfile>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<PrinterProfile> = state
            .printer_profiles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|profile| {
                matches_search(&format!("{} {}", profile.name, profile.connection_type), &params)
                    && params
                        .filters
                        .get("connection_type")
                        .is_none_or(|kind| profile.connection_type == *kind)
                    && params.filters.get("company_id").is_none_or(|company_id| {
                        profile.company_id.as_deref() == Some(company_id.as_str())
                    })
                    && params
                        .filters
                        .get("is_default")
                        .is_none_or(|flag| profile.is_default.to_string() == *flag)
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_printer_profile(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreatePrinterProfile>,
) -> HttpResponse {
    let result: UnifiedResult<PrinterProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let input = body.into_inner();
        if input.name.trim().is_empty() || input.name.len() > 120 {
            return Err(UnifiedError::bad_request(
                "Printer profile name must be 1 to 120 characters",
            ));
        }
        if let Some(code) = &input.paper_size_code {
            if state.paper_size(code).is_none() {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown paper size \"{code}\""
                )));
            }
        }
        validate_printer_profile(
            &input.connection_type,
            input.connection_target.as_deref(),
            input.auth_token_ref.as_deref(),
            input.width_columns,
            &input.character_set,
            &input.code_page,
            input.paper_size_code.as_deref(),
        )?;
        let now = now();
        let profile = PrinterProfile {
            id: new_id(),
            company_id: input.company_id,
            name: input.name.trim().to_string(),
            connection_type: input.connection_type,
            connection_target: input.connection_target,
            auth_token_ref: input.auth_token_ref,
            width_columns: input.width_columns,
            character_set: input.character_set,
            code_page: input.code_page,
            supports_drawer_kick: input.supports_drawer_kick,
            supports_cut: input.supports_cut,
            paper_size_code: input.paper_size_code,
            is_default: false,
            created_by: Some(caller.actor.clone()),
            created_at: now,
            updated_at: now,
        };
        state
            .printer_profiles
            .insert(profile.id.clone(), profile.clone());
        state.emit(
            EVENT_PRINTER_PROFILE_CREATED,
            json!({ "id": profile.id.clone(), "company_id": profile.company_id.clone(), "created_by": caller.actor }),
        );
        Ok(profile)
    })();
    match result {
        Ok(profile) => json::success_response(profile, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_printer_profile(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PrinterProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        state
            .printer_profiles
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Printer profile not found"))
    })();
    json::respond(result, false)
}

async fn update_printer_profile(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePrinterProfile>,
) -> HttpResponse {
    let result: UnifiedResult<PrinterProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let id = path.into_inner();
        let mut profile = state
            .printer_profiles
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Printer profile not found"))?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            if name.trim().is_empty() || name.len() > 120 {
                return Err(UnifiedError::bad_request(
                    "Printer profile name must be 1 to 120 characters",
                ));
            }
            profile.name = name.trim().to_string();
        }
        if let Some(connection_type) = input.connection_type {
            profile.connection_type = connection_type;
        }
        if let Some(target) = input.connection_target {
            profile.connection_target = target;
        }
        if let Some(reference) = input.auth_token_ref {
            profile.auth_token_ref = reference;
        }
        if let Some(width) = input.width_columns {
            profile.width_columns = width;
        }
        if let Some(character_set) = input.character_set {
            profile.character_set = character_set;
        }
        if let Some(code_page) = input.code_page {
            profile.code_page = code_page;
        }
        if let Some(supports_drawer_kick) = input.supports_drawer_kick {
            profile.supports_drawer_kick = supports_drawer_kick;
        }
        if let Some(supports_cut) = input.supports_cut {
            profile.supports_cut = supports_cut;
        }
        if let Some(paper_size_code) = input.paper_size_code {
            profile.paper_size_code = paper_size_code;
        }
        if let Some(code) = &profile.paper_size_code {
            if state.paper_size(code).is_none() {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown paper size \"{code}\""
                )));
            }
        }
        validate_printer_profile(
            &profile.connection_type,
            profile.connection_target.as_deref(),
            profile.auth_token_ref.as_deref(),
            profile.width_columns,
            &profile.character_set,
            &profile.code_page,
            profile.paper_size_code.as_deref(),
        )?;
        profile.updated_at = now();
        state
            .printer_profiles
            .insert(id.clone(), profile.clone());
        state.emit(
            EVENT_PRINTER_PROFILE_UPDATED,
            json!({ "id": profile.id.clone(), "updated_by": caller.actor }),
        );
        Ok(profile)
    })();
    json::respond(result, false)
}

async fn delete_printer_profile(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let id = path.into_inner();
        let in_use = state
            .print_jobs
            .iter()
            .any(|entry| entry.value().printer_profile_id.as_deref() == Some(id.as_str()));
        if in_use {
            return Err(UnifiedError::conflict(
                "Printer profile is referenced by print jobs and cannot be deleted",
            ));
        }
        let removed = state.printer_profiles.remove(&id);
        if removed.is_none() {
            return Err(UnifiedError::not_found("Printer profile not found"));
        }
        state.emit(
            EVENT_PRINTER_PROFILE_DELETED,
            json!({ "id": id.clone(), "deleted_by": caller.actor }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, false)
}

async fn set_default_printer_profile(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PrinterProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let id = path.into_inner();
        let mut profile = state
            .printer_profiles
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Printer profile not found"))?;
        // Clear the default flag on every other profile of the same
        // scope (same company, or global when the company is NULL),
        // then mark this one default.
        let scope = profile.company_id.clone();
        for mut entry in state.printer_profiles.iter_mut() {
            if entry.company_id == scope && entry.id != id {
                entry.is_default = false;
                entry.updated_at = now();
            }
        }
        profile.is_default = true;
        profile.updated_at = now();
        state
            .printer_profiles
            .insert(id.clone(), profile.clone());
        state.emit(
            EVENT_PRINTER_PROFILE_UPDATED,
            json!({ "id": profile.id.clone(), "is_default": true, "updated_by": caller.actor }),
        );
        Ok(profile)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Print-job handlers
// ---------------------------------------------------------------------------

async fn list_print_jobs(
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
                matches_search(&format!("{} {}", job.invoice_id, job.id), &params)
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
    json::respond(result, false)
}

async fn create_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreatePrintJobRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PrintJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_JOB)?;
        state.create_job(body.into_inner(), &caller.actor)
    })();
    match result {
        Ok(job) => json::success_response(job, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PrintJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        state
            .print_jobs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Print job not found"))
    })();
    json::respond(result, false)
}

async fn get_print_job_escpos(
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
    json::respond(result, false)
}

fn load_job_for_transition(
    state: &PrintingState,
    job_id: &str,
) -> UnifiedResult<PrintJob> {
    state
        .print_jobs
        .get(job_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Print job not found"))
}

async fn process_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_JOB)?;
        let id = path.into_inner();
        let mut job = load_job_for_transition(&state, &id)?;
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
        state.print_jobs.insert(id.clone(), job.clone());
        // Hand delivery to the deployment-side dispatcher: it owns
        // the hardware connection (see the module docs). The job
        // is marked done here because the stream and the document
        // were produced and handed off; a dispatcher that cannot
        // deliver marks the job failed through `/fail`.
        let completed = now();
        job.status = "done".to_string();
        job.completed_at = Some(completed);
        job.updated_at = completed;
        state.print_jobs.insert(id.clone(), job.clone());
        let summary = job_summary(&job);
        state.emit(
            EVENT_PRINT_JOB_DISPATCH_REQUESTED,
            json!({
                "job": summary.clone(),
                "escpos_base64": job.escpos_base64,
                "requested_by": caller.actor,
            }),
        );
        state.emit(EVENT_PRINT_JOB_COMPLETED, json!({ "job": summary.clone() }));
        notify_user(
            &state,
            &job.requested_by,
            "Print job completed",
            &format!("Print job {} for invoice {} is done.", job.id, job.invoice_id),
            &job.id,
        );
        Ok(json!({ "job": summary, "dispatched": true }))
    })();
    json::respond(result, false)
}

async fn fail_print_job(
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
        let mut job = load_job_for_transition(&state, &id)?;
        if job.status == "done" || job.status == "failed" {
            return Err(UnifiedError::conflict(format!(
                "A {} print job cannot be marked failed",
                job.status
            )));
        }
        let reason = body
            .get("error")
            .and_then(|value| value.as_str())
            .unwrap_or("Print failed")
            .trim()
            .to_string();
        if reason.is_empty() || reason.len() > 1000 {
            return Err(UnifiedError::bad_request(
                "Failure reason must be 1 to 1000 characters",
            ));
        }
        job.status = "failed".to_string();
        job.error_message = Some(reason.clone());
        job.completed_at = Some(now());
        job.updated_at = now();
        state.print_jobs.insert(id.clone(), job.clone());
        let summary = job_summary(&job);
        state.emit(
            EVENT_PRINT_JOB_FAILED,
            json!({ "job": summary.clone(), "error": reason.clone() }),
        );
        notify_user(
            &state,
            &job.requested_by,
            "Print job failed",
            &format!(
                "Print job {} for invoice {} failed: {}",
                job.id, job.invoice_id, reason
            ),
            &job.id,
        );
        Ok(summary)
    })();
    json::respond(result, false)
}

async fn cancel_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PrintJobSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_JOB)?;
        let id = path.into_inner();
        let mut job = load_job_for_transition(&state, &id)?;
        if job.status != "queued" {
            return Err(UnifiedError::conflict(format!(
                "Only a queued print job can be cancelled (current status: {})",
                job.status
            )));
        }
        job.status = "failed".to_string();
        job.error_message = Some("Cancelled by the requester".to_string());
        job.completed_at = Some(now());
        job.updated_at = now();
        state.print_jobs.insert(id.clone(), job.clone());
        let summary = job_summary(&job);
        state.emit(
            EVENT_PRINT_JOB_FAILED,
            json!({ "job": summary.clone(), "error": "Cancelled by the requester" }),
        );
        Ok(summary)
    })();
    json::respond(result, false)
}

async fn reprint_print_job(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PrintJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_JOB)?;
        let id = path.into_inner();
        state.reprint_job(&id, &caller.actor)
    })();
    match result {
        Ok(job) => json::success_response(job, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn thermal_preview(
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
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct PrintingModule {
    state: Arc<PrintingState>,
}

impl PrintingModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(PrintingState::new()),
        }
    }
}

impl Default for PrintingModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PrintingModule {
    fn id(&self) -> &'static str {
        "printing"
    }

    fn name(&self) -> &'static str {
        "Printing"
    }

    fn description(&self) -> &'static str {
        "Printing: paper sizes (ISO 216 A-series, Letter, Legal, Tabloid and thermal roll widths as standards, plus admin-editable custom sizes), thermal printer profiles, ESC/POS thermal rendering with text preview, and the print-job queue with history, reprint, and status tracking."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["billing"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_PRINT_JOB_REQUESTED,
            "printing",
            Arc::new(move |ctx: &mut HookContext| on_print_job_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_PRINT_JOB_REPRINT_REQUESTED,
            "printing",
            Arc::new(move |ctx: &mut HookContext| {
                on_print_job_reprint_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_PRINT_JOBS_LIST_REQUESTED,
            "printing",
            Arc::new(move |ctx: &mut HookContext| {
                on_print_jobs_list_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_SCHOOL_FEE_RECEIPT_PRINT_REQUESTED,
            "printing",
            Arc::new(move |ctx: &mut HookContext| {
                on_school_fee_receipt_print_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/printing")
                .route("/paper-sizes", web::get().to(list_paper_sizes))
                .route("/paper-sizes", web::post().to(create_paper_size))
                .route("/paper-sizes/{code}", web::get().to(get_paper_size))
                .route("/paper-sizes/{code}", web::put().to(update_paper_size))
                .route("/paper-sizes/{code}", web::delete().to(delete_paper_size))
                .route(
                    "/paper-sizes/defaults/{company_id}",
                    web::get().to(get_default_paper_size),
                )
                .route(
                    "/paper-sizes/defaults/{company_id}",
                    web::put().to(set_default_paper_size),
                )
                .route("/printer-profiles", web::get().to(list_printer_profiles))
                .route("/printer-profiles", web::post().to(create_printer_profile))
                .route(
                    "/printer-profiles/{id}",
                    web::get().to(get_printer_profile),
                )
                .route(
                    "/printer-profiles/{id}",
                    web::put().to(update_printer_profile),
                )
                .route(
                    "/printer-profiles/{id}",
                    web::delete().to(delete_printer_profile),
                )
                .route(
                    "/printer-profiles/{id}/set-default",
                    web::post().to(set_default_printer_profile),
                )
                .route("/print-jobs", web::get().to(list_print_jobs))
                .route("/print-jobs", web::post().to(create_print_job))
                .route("/print-jobs/{id}", web::get().to(get_print_job))
                .route(
                    "/print-jobs/{id}/escpos",
                    web::get().to(get_print_job_escpos),
                )
                .route(
                    "/print-jobs/{id}/process",
                    web::post().to(process_print_job),
                )
                .route("/print-jobs/{id}/fail", web::post().to(fail_print_job))
                .route(
                    "/print-jobs/{id}/cancel",
                    web::post().to(cancel_print_job),
                )
                .route(
                    "/print-jobs/{id}/reprint",
                    web::post().to(reprint_print_job),
                )
                .route("/thermal/preview", web::post().to(thermal_preview)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/printing/paper-sizes", owner: "printing", description: "List paper sizes: ISO 216 A-series, Letter, Legal, Tabloid and thermal widths, plus admin-created custom sizes." },
            RouteInfo { method: "POST", path: "/api/v1/printing/paper-sizes", owner: "printing", description: "Create a custom paper size (standard sizes cannot be created)." },
            RouteInfo { method: "GET", path: "/api/v1/printing/paper-sizes/{code}", owner: "printing", description: "Get one paper size by code." },
            RouteInfo { method: "PUT", path: "/api/v1/printing/paper-sizes/{code}", owner: "printing", description: "Update a custom paper size; standard sizes are read-only." },
            RouteInfo { method: "DELETE", path: "/api/v1/printing/paper-sizes/{code}", owner: "printing", description: "Delete a custom paper size; standard sizes cannot be deleted." },
            RouteInfo { method: "GET", path: "/api/v1/printing/paper-sizes/defaults/{company_id}", owner: "printing", description: "Get the default paper size of one company (A4 when none is set)." },
            RouteInfo { method: "PUT", path: "/api/v1/printing/paper-sizes/defaults/{company_id}", owner: "printing", description: "Set the default paper size of one company." },
            RouteInfo { method: "GET", path: "/api/v1/printing/printer-profiles", owner: "printing", description: "List thermal printer profiles with pagination and filters." },
            RouteInfo { method: "POST", path: "/api/v1/printing/printer-profiles", owner: "printing", description: "Create a thermal printer profile (connection, width, character set, code page, capabilities)." },
            RouteInfo { method: "GET", path: "/api/v1/printing/printer-profiles/{id}", owner: "printing", description: "Get one printer profile." },
            RouteInfo { method: "PUT", path: "/api/v1/printing/printer-profiles/{id}", owner: "printing", description: "Update a printer profile." },
            RouteInfo { method: "DELETE", path: "/api/v1/printing/printer-profiles/{id}", owner: "printing", description: "Delete a printer profile that no print job references." },
            RouteInfo { method: "POST", path: "/api/v1/printing/printer-profiles/{id}/set-default", owner: "printing", description: "Mark a printer profile the default for its company (or the global default)." },
            RouteInfo { method: "GET", path: "/api/v1/printing/print-jobs", owner: "printing", description: "List print jobs with pagination and filters (status, output kind, invoice, company, profile)." },
            RouteInfo { method: "POST", path: "/api/v1/printing/print-jobs", owner: "printing", description: "Create a print job; thermal jobs render their ESC/POS stream and preview at queue time." },
            RouteInfo { method: "GET", path: "/api/v1/printing/print-jobs/{id}", owner: "printing", description: "Get one print job." },
            RouteInfo { method: "GET", path: "/api/v1/printing/print-jobs/{id}/escpos", owner: "printing", description: "Get the ESC/POS stream (base64) and text preview of a thermal print job." },
            RouteInfo { method: "POST", path: "/api/v1/printing/print-jobs/{id}/process", owner: "printing", description: "Hand a queued print job to the deployment-side dispatcher and mark it done." },
            RouteInfo { method: "POST", path: "/api/v1/printing/print-jobs/{id}/fail", owner: "printing", description: "Mark a print job failed with a reason." },
            RouteInfo { method: "POST", path: "/api/v1/printing/print-jobs/{id}/cancel", owner: "printing", description: "Cancel a queued print job (recorded as failed with a cancellation reason)." },
            RouteInfo { method: "POST", path: "/api/v1/printing/print-jobs/{id}/reprint", owner: "printing", description: "Queue a fresh copy of a print job." },
            RouteInfo { method: "POST", path: "/api/v1/printing/thermal/preview", owner: "printing", description: "Render thermal lines as a text preview plus the ESC/POS stream (base64), without storing a job." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PrintingModule::new()));
}
