//! printing - module.

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
//       deployment-side dispatcher; the job stays `printing`
//       until the dispatcher confirms delivery.
//     POST /print-jobs/{id}/complete — dispatcher confirmation
//       that the job printed; only then is it `done` and the
//       user notified.
//     POST /print-jobs/{id}/fail — mark a job failed with a reason.
//     POST /print-jobs/{id}/cancel — cancel a queued job; it
//       becomes `cancelled`, never `failed`.
//     POST /print-jobs/{id}/reprint — queue a fresh copy of a
//       done or failed job.
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
// `printing.job.cancelled`, `printing.job.dispatch_requested`,
// `notification.requested`
// (to the user who requested the job, on completion and on
// failure), and `printing.paper_size.*` /
// `printing.printer_profile.*` change events for the audit log.
// Nothing in this module calls another module directly, and no
// behaviour is hard-coded outside the standard paper sizes, the
// ESC/POS command bytes (a published printer standard), and the
// code-page table below.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::{web};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::sync::{Arc};
use super::{events::*, paper::*, thermal::*, profiles::*, jobs::*, state::*, auth::*, hooks::*, handlers_paper::*, handlers_profiles::*, handlers_jobs::*};



// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------
pub struct PrintingModule {
    pub(crate) state: Arc<PrintingState>,
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
                .route(
                    "/print-jobs/{id}/complete",
                    web::post().to(complete_print_job),
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
            RouteInfo { method: "GET", path: "/api/v1/printing/paper-sizes", owner: "printing", description: "List paper sizes: ISO 216 A-series, Letter, Legal, Tabloid and thermal widths, plus admin-created custom sizes.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/printing/paper-sizes", owner: "printing", description: "Create a custom paper size (standard sizes cannot be created).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/printing/paper-sizes/{code}", owner: "printing", description: "Get one paper size by code.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/printing/paper-sizes/{code}", owner: "printing", description: "Update a custom paper size; standard sizes are read-only.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/printing/paper-sizes/{code}", owner: "printing", description: "Delete a custom paper size; standard sizes cannot be deleted.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/printing/paper-sizes/defaults/{company_id}", owner: "printing", description: "Get the default paper size of one company (A4 when none is set).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/printing/paper-sizes/defaults/{company_id}", owner: "printing", description: "Set the default paper size of one company.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/printing/printer-profiles", owner: "printing", description: "List thermal printer profiles with pagination and filters.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/printing/printer-profiles", owner: "printing", description: "Create a thermal printer profile (connection, width, character set, code page, capabilities).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/printing/printer-profiles/{id}", owner: "printing", description: "Get one printer profile.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/printing/printer-profiles/{id}", owner: "printing", description: "Update a printer profile.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/printing/printer-profiles/{id}", owner: "printing", description: "Delete a printer profile that no print job references.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/printing/printer-profiles/{id}/set-default", owner: "printing", description: "Mark a printer profile the default for its company (or the global default).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/printing/print-jobs", owner: "printing", description: "List print jobs with pagination and filters (status, output kind, invoice, company, profile).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/printing/print-jobs", owner: "printing", description: "Create a print job; thermal jobs render their ESC/POS stream and preview at queue time.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/printing/print-jobs/{id}", owner: "printing", description: "Get one print job.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/printing/print-jobs/{id}/escpos", owner: "printing", description: "Get the ESC/POS stream (base64) and text preview of a thermal print job.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/printing/print-jobs/{id}/process", owner: "printing", description: "Hand a queued print job to the deployment-side dispatcher; the job stays printing until the dispatcher confirms delivery.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/printing/print-jobs/{id}/complete", owner: "printing", description: "Dispatcher confirmation that a printing job printed; only then is it marked done and the user notified.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/printing/print-jobs/{id}/fail", owner: "printing", description: "Mark a print job failed with a reason; allowed from queued, printing, and done.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/printing/print-jobs/{id}/cancel", owner: "printing", description: "Cancel a queued print job; it becomes cancelled, never failed.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/printing/print-jobs/{id}/reprint", owner: "printing", description: "Queue a fresh copy of a done or failed print job.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/printing/thermal/preview", owner: "printing", description: "Render thermal lines as a text preview plus the ESC/POS stream (base64), without storing a job.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PrintingModule::new()));
}
