//! billing - module.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use super::state::*;
use super::hooks_payments::*;
use super::hooks_subscriptions::*;
use super::school_fees::*;
use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_invoices::{create_credit_note, create_invoice, delete_invoice, from_order, get_invoice, get_settings, list_credit_notes, list_invoices, list_overdue, list_payments, pay_invoice, pdf_data, record_payment, remind_invoice, render_invoice, run_reminders, update_invoice, update_settings, void_invoice};
use super::reference::{get_reference_settings, update_reference_settings};
use super::school_fees_charges::{create_fee_discount, create_fee_guardian, delete_fee_discount, delete_fee_guardian, get_fee_discount, get_fee_receipt, issue_fee_receipt, list_fee_arrears, list_fee_discounts, list_fee_guardians, list_my_fee_charges, remind_fee_charge, run_fee_reminders, update_fee_discount};
use super::school_fees_handlers::{create_fee_schedule, create_fee_type, delete_fee_type, generate_fee_charges, get_fee_charge, get_fee_schedule, get_fee_settings, get_fee_type, list_fee_charges, list_fee_schedules, list_fee_types, pay_fee_charge, update_fee_schedule, update_fee_settings, update_fee_type, void_fee_charge};
use super::templates_handlers::{create_invoice_template, delete_invoice_template, email_invoice_pdf, get_invoice_template, list_invoice_print_jobs, list_invoice_template_versions, list_invoice_templates, preview_invoice_template, print_invoice, print_ready_invoice, reprint_invoice_print_job, restore_invoice_template_version, set_default_invoice_template, update_invoice_template};

pub struct BillingModule {
    state: Arc<BillingState>,
}

impl BillingModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(BillingState::new()),
        }
    }
}

impl Default for BillingModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for BillingModule {
    fn id(&self) -> &'static str {
        "billing"
    }

    fn name(&self) -> &'static str {
        "Billing"
    }

    fn description(&self) -> &'static str {
        "Billing and invoicing: invoices from orders, numbered invoices with taxes and due dates, payments, credit notes, overdue reminders and PDF-ready data, plus the subscription lifecycle engine (renewals, proration, dunning, grace periods, renewal reminders, trial ends). Invoice templates (versioned, per-company, with preview) and invoice printing (print-ready HTML with @page rules, print-job queue via the printing module, email-as-PDF) live here too."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["store"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("billing", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: unified_core::hooks::HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_order_created(state.clone(), ctx));
        bus.register("order.created", "billing", handler);
        let state = self.state.clone();
        bus.register(
            PAYMENT_CAPTURED,
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_payment_captured(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            ORDER_PAID,
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_order_paid(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            PAYMENT_REFUNDED,
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_payment_refunded(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "billing.reminders.run",
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_reminders_run(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "cleanup.requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_cleanup_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "billing.subscriptions.scan",
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_subscriptions_scan(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            SUBSCRIPTION_CREATED,
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_subscription_created(state.clone(), ctx)),
        );
        for event in [
            SUBSCRIPTION_RENEWED,
            SUBSCRIPTION_TRIAL_ENDED,
            SUBSCRIPTION_UPGRADED,
            SUBSCRIPTION_DOWNGRADED,
            SUBSCRIPTION_PAST_DUE,
            SUBSCRIPTION_CANCELLED,
        ] {
            let state = self.state.clone();
            bus.register(
                event,
                "billing",
                Arc::new(move |ctx: &mut HookContext| on_subscription_synced(state.clone(), ctx)),
            );
        }
        let state = self.state.clone();
        bus.register(
            PAYMENT_FAILED,
            "billing",
            Arc::new(move |ctx: &mut HookContext| on_payment_failed(state.clone(), ctx)),
        );
        // --- School fees: academic-module events and invoice
        // sync. Fee charges stay in step with their linked billing
        // invoice through the invoice events below, so gateway
        // captures (`payment.captured`), manual invoice payments
        // and fee payments all converge on the same charge state.
        // Canonical enrollment events: academic emits
        // `academic.enrollment.approved` / `.dropped` -- never
        // `academic.student.enrolled` / `.withdrawn`.
        let state = self.state.clone();
        bus.register(
            "academic.enrollment.approved",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_academic_enrollment_approved(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "academic.enrollment.dropped",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_academic_enrollment_dropped(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "invoice.payment.recorded",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_invoice_payment_recorded_for_fee(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "invoice.paid",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_invoice_paid_for_fee(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "invoice.voided",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_invoice_voided_for_fee(state.clone(), ctx)
            }),
        );
        // --- Integration round: invoice requests (admissions /
        // appointments), expense reimbursements, digital payouts,
        // SaaS package subscription handoffs.
        let state = self.state.clone();
        bus.register(
            "billing.invoice.requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_invoice_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "expenses.reimbursement.requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_reimbursement_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "store.digital.payout.requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_digital_payout_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "store.digital.payout.approved",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_digital_payout_approved(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "billing.subscription.renew_requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_renew_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "billing.subscription.cancel_requested",
            "billing",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_cancel_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/billing")
                .route("/invoices", web::get().to(list_invoices))
                .route("/invoices", web::post().to(create_invoice))
                .route("/invoices/from-order", web::post().to(from_order))
                .route("/invoices/{id}", web::get().to(get_invoice))
                .route("/invoices/{id}", web::put().to(update_invoice))
                .route("/invoices/{id}", web::delete().to(delete_invoice))
                .route("/invoices/{id}/pay", web::post().to(pay_invoice))
                .route("/invoices/{id}/void", web::post().to(void_invoice))
                .route("/invoices/{id}/render", web::get().to(render_invoice))
                .route("/invoices/{id}/pdf-data", web::get().to(pdf_data))
                .route("/invoices/{id}/payments", web::get().to(list_payments))
                .route("/invoices/{id}/payments", web::post().to(record_payment))
                .route("/invoices/{id}/credit-notes", web::get().to(list_credit_notes))
                .route("/invoices/{id}/credit-notes", web::post().to(create_credit_note))
                .route("/invoices/{id}/remind", web::post().to(remind_invoice))
                .route("/reminders/run", web::post().to(run_reminders))
                .route("/subscriptions", web::get().to(list_subscriptions))
                .route("/subscriptions/scan", web::post().to(run_subscription_scan_endpoint))
                .route("/subscriptions/{id}", web::get().to(get_subscription))
                .route(
                    "/subscriptions/{id}/dunning",
                    web::get().to(list_dunning_attempts),
                )
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/reference-settings", web::get().to(get_reference_settings))
                .route("/reference-settings", web::put().to(update_reference_settings))
                .route("/overdue", web::get().to(list_overdue))
                .route("/school-fees/types", web::get().to(list_fee_types))
                .route("/school-fees/types", web::post().to(create_fee_type))
                .route("/school-fees/types/{id}", web::get().to(get_fee_type))
                .route("/school-fees/types/{id}", web::put().to(update_fee_type))
                .route(
                    "/school-fees/types/{id}",
                    web::delete().to(delete_fee_type),
                )
                .route("/school-fees/settings", web::get().to(get_fee_settings))
                .route(
                    "/school-fees/settings",
                    web::put().to(update_fee_settings),
                )
                .route("/school-fees/schedules", web::post().to(create_fee_schedule))
                .route("/school-fees/schedules", web::get().to(list_fee_schedules))
                .route(
                    "/school-fees/schedules/{id}",
                    web::get().to(get_fee_schedule),
                )
                .route(
                    "/school-fees/schedules/{id}",
                    web::put().to(update_fee_schedule),
                )
                .route(
                    "/school-fees/schedules/{id}/generate",
                    web::post().to(generate_fee_charges),
                )
                .route("/school-fees/charges", web::get().to(list_fee_charges))
                .route(
                    "/school-fees/charges/{id}",
                    web::get().to(get_fee_charge),
                )
                .route(
                    "/school-fees/charges/{id}/pay",
                    web::post().to(pay_fee_charge),
                )
                .route(
                    "/school-fees/charges/{id}/void",
                    web::post().to(void_fee_charge),
                )
                .route(
                    "/school-fees/charges/{id}/remind",
                    web::post().to(remind_fee_charge),
                )
                .route(
                    "/school-fees/charges/{id}/receipt",
                    web::get().to(get_fee_receipt),
                )
                .route(
                    "/school-fees/charges/{id}/receipt",
                    web::post().to(issue_fee_receipt),
                )
                .route(
                    "/school-fees/reminders/run",
                    web::post().to(run_fee_reminders),
                )
                .route("/school-fees/arrears", web::get().to(list_fee_arrears))
                .route("/school-fees/discounts", web::get().to(list_fee_discounts))
                .route(
                    "/school-fees/discounts",
                    web::post().to(create_fee_discount),
                )
                .route(
                    "/school-fees/discounts/{id}",
                    web::get().to(get_fee_discount),
                )
                .route(
                    "/school-fees/discounts/{id}",
                    web::put().to(update_fee_discount),
                )
                .route(
                    "/school-fees/discounts/{id}",
                    web::delete().to(delete_fee_discount),
                )
                .route("/school-fees/guardians", web::get().to(list_fee_guardians))
                .route(
                    "/school-fees/guardians",
                    web::post().to(create_fee_guardian),
                )
                .route(
                    "/school-fees/guardians/{id}",
                    web::delete().to(delete_fee_guardian),
                )
                .route("/school-fees/my", web::get().to(list_my_fee_charges))
                .route("/invoice-templates", web::get().to(list_invoice_templates))
                .route(
                    "/invoice-templates",
                    web::post().to(create_invoice_template),
                )
                .route(
                    "/invoice-templates/{id}",
                    web::get().to(get_invoice_template),
                )
                .route(
                    "/invoice-templates/{id}",
                    web::put().to(update_invoice_template),
                )
                .route(
                    "/invoice-templates/{id}",
                    web::delete().to(delete_invoice_template),
                )
                .route(
                    "/invoice-templates/{id}/versions",
                    web::get().to(list_invoice_template_versions),
                )
                .route(
                    "/invoice-templates/{id}/versions/{version}/restore",
                    web::post().to(restore_invoice_template_version),
                )
                .route(
                    "/invoice-templates/{id}/set-default",
                    web::post().to(set_default_invoice_template),
                )
                .route(
                    "/invoice-templates/{id}/preview",
                    web::get().to(preview_invoice_template),
                )
                .route("/invoices/{id}/print", web::post().to(print_invoice))
                .route(
                    "/invoices/{id}/print-ready",
                    web::get().to(print_ready_invoice),
                )
                .route(
                    "/invoices/{id}/print-jobs",
                    web::get().to(list_invoice_print_jobs),
                )
                .route(
                    "/invoices/{id}/email-pdf",
                    web::post().to(email_invoice_pdf),
                )
                .route(
                    "/print-jobs/{job_id}/reprint",
                    web::post().to(reprint_invoice_print_job),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices", owner: "billing", description: "List invoices with pagination and filters.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices", owner: "billing", description: "Create an invoice with per-line taxes.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/from-order", owner: "billing", description: "Build an invoice from a known order.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}", owner: "billing", description: "Get one invoice.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/billing/invoices/{id}", owner: "billing", description: "Update an open invoice.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/billing/invoices/{id}", owner: "billing", description: "Delete a draft invoice.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/pay", owner: "billing", description: "Pay an invoice in full and record the payment.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/void", owner: "billing", description: "Void an unpaid invoice.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/render", owner: "billing", description: "Render an invoice as HTML.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/pdf-data", owner: "billing", description: "Full structured invoice payload for a PDF renderer.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/payments", owner: "billing", description: "List payments recorded against an invoice.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/payments", owner: "billing", description: "Record a payment (full or partial) against an invoice.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/credit-notes", owner: "billing", description: "List credit notes issued against an invoice.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/credit-notes", owner: "billing", description: "Create a credit note against an invoice.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/remind", owner: "billing", description: "Send one overdue reminder for an invoice (emits billing.invoice.reminder).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/reminders/run", owner: "billing", description: "Run the overdue-reminder scan for all open invoices.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/subscriptions", owner: "billing", description: "List subscription lifecycle mirrors with pagination and filters.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/subscriptions/scan", owner: "billing", description: "Run the subscription lifecycle scan (trial ends, renewal reminders and invoices, dunning retries, grace expiry, period-end cancellations).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/subscriptions/{id}", owner: "billing", description: "Get one subscription lifecycle mirror.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/subscriptions/{id}/dunning", owner: "billing", description: "List recorded dunning attempts for a subscription.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/settings", owner: "billing", description: "Read billing settings (numbering, taxes, terms, reminders). Requires billing.settings.view.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/billing/settings", owner: "billing", description: "Update billing settings. Requires billing.settings.manage.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/reference-settings", owner: "billing", description: "Read payment reference settings (default prefix, per-product prefixes).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/billing/reference-settings", owner: "billing", description: "Update payment reference settings (capability billing.reference:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/overdue", owner: "billing", description: "List overdue invoices.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/invoice-templates", owner: "billing", description: "List invoice templates with pagination and filters (company, default flag).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoice-templates", owner: "billing", description: "Create an invoice template (header/footer, logo reference, colours, columns, tax breakdown, payment instructions).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/invoice-templates/{id}", owner: "billing", description: "Get one invoice template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/billing/invoice-templates/{id}", owner: "billing", description: "Update an invoice template; the previous version is archived and the version number bumps.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/billing/invoice-templates/{id}", owner: "billing", description: "Delete an invoice template and its version history.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/invoice-templates/{id}/versions", owner: "billing", description: "List the version history of an invoice template.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoice-templates/{id}/versions/{version}/restore", owner: "billing", description: "Restore an old template version as a new version.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoice-templates/{id}/set-default", owner: "billing", description: "Set an invoice template as the default for one company.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/invoice-templates/{id}/preview", owner: "billing", description: "Preview a template rendered with a real invoice or with sample invoice data.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/print", owner: "billing", description: "Print an invoice: render the print-ready document and queue a print job through the printing module.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/print-ready", owner: "billing", description: "Get the print-ready HTML document for an invoice, with @page rules for the chosen paper size.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/invoices/{id}/print-jobs", owner: "billing", description: "List the print jobs of one invoice (served by the printing module).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/invoices/{id}/email-pdf", owner: "billing", description: "Email an invoice as a print-ready document through mail.send_requested.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/print-jobs/{job_id}/reprint", owner: "billing", description: "Queue a fresh copy of a print job through the printing module.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/types", owner: "billing", description: "List school fee types (admin-editable definitions).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/types", owner: "billing", description: "Create a school fee type (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/types/{id}", owner: "billing", description: "Get one school fee type.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/billing/school-fees/types/{id}", owner: "billing", description: "Update a school fee type (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/billing/school-fees/types/{id}", owner: "billing", description: "Delete a school fee type with no schedules (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/settings", owner: "billing", description: "Read school fee settings (configurable defaults).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/billing/school-fees/settings", owner: "billing", description: "Update school fee settings (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/schedules", owner: "billing", description: "Create a per-student fee schedule (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/schedules", owner: "billing", description: "List fee schedules with pagination and filters (scoped to the caller).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/schedules/{id}", owner: "billing", description: "Get one fee schedule (scoped to the caller).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/billing/school-fees/schedules/{id}", owner: "billing", description: "Update a fee schedule (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/schedules/{id}/generate", owner: "billing", description: "Generate a fee charge and its billing invoice for one period (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/charges", owner: "billing", description: "List fee charges with pagination, filters and aging (scoped to the caller).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/charges/{id}", owner: "billing", description: "Get one fee charge (scoped to the caller).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/charges/{id}/pay", owner: "billing", description: "Pay a fee charge through its billing invoice (manager, or the student / linked guardian).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/charges/{id}/void", owner: "billing", description: "Void a fee charge and its billing invoice (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/charges/{id}/remind", owner: "billing", description: "Send one fee reminder for a charge (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/charges/{id}/receipt", owner: "billing", description: "Get the latest issued receipt and print payload for a charge (scoped to the caller).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/charges/{id}/receipt", owner: "billing", description: "Issue a fee receipt and request printing (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/reminders/run", owner: "billing", description: "Run the fee-reminder scan for due and overdue charges (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/arrears", owner: "billing", description: "Per-student arrears and aging summary (scoped to the caller).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/discounts", owner: "billing", description: "List fee discounts and scholarships (scoped to the caller).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/discounts", owner: "billing", description: "Create a per-student discount or scholarship (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/discounts/{id}", owner: "billing", description: "Get one fee discount (scoped to the caller).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/billing/school-fees/discounts/{id}", owner: "billing", description: "Update or approve a fee discount (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/billing/school-fees/discounts/{id}", owner: "billing", description: "Delete a fee discount (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/guardians", owner: "billing", description: "List guardian links for a student (manager, or the guardian).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/billing/school-fees/guardians", owner: "billing", description: "Link a guardian to a student (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/billing/school-fees/guardians/{id}", owner: "billing", description: "Remove a guardian link (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/billing/school-fees/my", owner: "billing", description: "Fee charges for the caller: the student's own, or their children's for a guardian.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(BillingModule::new()));
}
