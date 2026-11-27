// Expenses module - employee expense reports with categories,
// policies, mileage and per-diem entries, receipt references,
// approval through the approvals flow (events only), and
// reimbursement through the billing flow (events only).
//
// Routes live under `/api/v1/expenses`:
//
//   Categories (admin-editable):
//     GET/POST /categories - list / create a category.
//     GET/PATCH/DELETE /categories/{id}.
//   Policies (per-company rules):
//     GET/POST /policies - list / create a policy.
//     GET/PATCH/DELETE /policies/{id}.
//   Reports:
//     GET/POST /reports - list / create a report (draft).
//     GET/PATCH/DELETE /reports/{id}.
//     POST /reports/{id}/submit - draft -> submitted (requests approval).
//     POST /reports/{id}/withdraw - submitted -> draft.
//     GET  /reports/{id}/export?format=csv|json - the report as CSV or JSON.
//   Expenses (lines inside a report):
//     GET/POST /reports/{id}/expenses - list / add a line.
//     GET/PATCH/DELETE /expenses/{id}.
//     POST /expenses/{id}/receipts - attach a receipt reference.
//     DELETE /expenses/{id}/receipts/{receipt_id} - remove a reference.
//   Reimbursements:
//     GET  /reimbursements - list (own only unless manager).
//     GET  /reimbursements/{id}.
//     POST /reimbursements/{id}/retry - re-request a failed reimbursement.
//   Adjustments and refunds (separate entries, never history edits):
//     GET/POST /reports/{id}/adjustments.
//   Currency rates (stored rates only - never guessed):
//     GET/POST /currency-rates - list / store a rate.
//     GET/DELETE /currency-rates/{id}.
//     POST /currency-rates/convert - convert with a stored rate.
//   Analytics:
//     GET  /analytics/spend - spend per category / employee / period.
//     GET  /analytics/pending-approvals - reports waiting on a decision.
//     GET  /analytics/reimbursement-backlog - pending / failed pay-outs.
//     GET  /analytics/violations - flagged expenses and over-limit reports.
//   Settings:
//     GET/PUT /settings - the configurable defaults (manager).
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory category / policy / report / expense / receipt /
// reimbursement / adjustment / currency-rate management; a report
// status machine (draft -> submitted -> approved / rejected ->
// reimbursed) with checked transitions; mileage amounts computed
// server-side from distance times the policy rate and per-diem
// amounts computed from days times the policy rate; policy
// violation flags (over per-expense limit, over per-day limit,
// receipt missing) that are recorded on the expense and surfaced
// to approvers, never silently auto-approved; receipt references
// only (storage references handed to the documents / storage
// modules - no receipt bytes live here); currency conversion
// from stored rates only, with the calculator module consulted
// through its `calculator.rate_requested` event when this module
// holds no stored rate for the pair. What is NOT built here:
// durable storage (the DashMap stores are authoritative;
// migration `2026-11-22-000083_expenses` is the persistence
// target); an approval engine of any kind - approval decisions
// arrive only as `expenses.approval.decided` events from the
// approvals flow, and this module never decides an approval
// itself; any payment - reimbursement pay-outs are requested
// from the billing flow with `expenses.reimbursement.requested`
// and only ever confirmed through its paid / failed events; any
// project or client data - project and client links are opaque
// ids only, resolved and owned by the projects / CRM modules.
//
// CONFIG-DRIVEN - READ THIS FIRST. Nothing behavioural is
// hard-coded: the allowed expense kinds, the supported
// currencies, the default currency, the default mileage and
// per-diem rates, and the maximum report total warning level
// live in `ExpensesSettings`, editable by an administrator
// through `PUT /settings`. Categories carry their own receipt
// requirement, receipt threshold, per-expense limit, and
// per-day limit; policies carry their own mileage rate,
// per-diem rate, report total limit, and approval chain
// reference. The values in `ExpensesSettings::default` are
// CONFIGURABLE DEFAULTS only - an admin change applies to
// every validation and default that reads them, and no code
// path consults a baked-in list instead. The one deliberate
// exception is the report status vocabulary itself
// (`draft`, `submitted`, `approved`, `rejected`,
// `reimbursed`): it is the state machine of this module, and
// the allowed transitions are the transition table in
// `allowed_report_transitions`, not a settings list.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed). Employees see only their own reports, expenses, and
// reimbursements; managers (the manage capability) see every
// company row they are allowed to see through the same check.
//
// Tenant scope: every tenant row (report, category, policy, and
// everything reached through a report) carries the resolved
// tenant's `company_id`, stamped at creation from the tenant
// context through the entitlements module's `TenantScope`
// (`TenantScope::resolve`) and checked on every read. A
// caller-supplied `company_id` is never trusted: on create it
// is passed as the `requested` argument and a mismatch is a
// 403; on read a row from another company (or with no company
// at all) is answered 404, so its existence never leaks. The
// currency rates and the settings are the only exceptions:
// rates are global stored data with no company field, and the
// settings are module-global admin configuration.
//
// Receipt verification: a receipt row is an UNVERIFIED claim.
// The storage target and key arrive as caller-supplied free
// text and no storage lookup runs at attach time - there is no
// existence-check event on the bus (the storage manager answers
// `storage.object.upload_requested` with an upload binding,
// never with "this object exists"). The row becomes verified
// only when the storage manager emits `storage.object.stored`
// for its key (matching `module: "expenses"` when the event
// names a module), and is un-verified again by
// `storage.object.deleted`. Only a verified receipt clears the
// `receipt_missing` violation, so a fabricated key can never
// unblock a submission.
//
// Spend analytics: totals are grouped by (group, currency) -
// amounts in different currencies are never summed - and count
// only `submitted`, `approved`, and `reimbursed` reports.
// `draft` reports never left the employee and `rejected`
// reports were denied, so neither counts as spend. Refunds net
// against the same (group, currency) row where they can be
// attributed (employee, period); category rows carry no refund
// netting because a report-level refund cannot be attributed
// to one category.
//
// Adjustments: a refund or adjustment against an approved
// report reduces the pending reimbursement's amount (floored
// at zero) when the currencies match; the report total and
// the expense lines are never mutated, so the submitted
// history stays intact. Once the reimbursement is paid there
// is no payable left to reduce, so the adjustment stays
// record-only and is netted in spend analytics only.
//
// Currency rates: one pair is one row. Storing a rate for a
// pair that already has one updates it in place (emitting
// `expenses.currency_rate.updated`); lookups among rows stored
// before the upsert pick the newest by `created_at`, never the
// last row the map happens to iterate.
//
// Approval contract (the approvals flow owns the decision):
//   1. `POST /reports/{id}/submit` moves a draft report to
//      `submitted`, stores a fresh `approval_request_id`, and
//      emits `expenses.approval.requested` with
//      `{approval_request_id, report_id, employee_id,
//      company_id, total_minor, currency, approval_chain_id,
//      violations}`. It does NOT approve anything. The same
//      submit ALSO emits the generic `approval.requested`
//      (module `expenses`, record_type `expense_report`,
//      record_id = the report id, requester_id = the employee,
//      summary, amount_minor, currency, company_id) so the
//      generic approvals module can run its workflow; the
//      approvals `request_id` from its synchronous answer is
//      stored on the report as `generic_approval_request_id`
//      and is the correlation id for the generic decision.
//      When the approvals module has no active definition for
//      `expenses` / `expense_report` (or is not registered) the
//      submit still succeeds and the report stays `submitted`:
//      a missing definition never auto-approves or auto-rejects.
//   2. The approvals flow runs its chain and emits
//      `approval.decided` with `{request_id, decision,
//      decided_by, module, record_type, record_id,
//      requester_id}`. This module listens to that event,
//      correlates it by the stored `generic_approval_request_id`
//      (falling back to the (`module`, `record_type`,
//      `record_id`) triple for requests created before the
//      bridge, or by an admin through the approvals API), and
//      moves the report to `approved` or `rejected`.
//      `changes_requested` leaves the report `submitted` - the
//      requester withdraws it to draft to revise it. The
//      outcome is then re-emitted as `expenses.approval.decided`
//      with `{approval_request_id, report_id, decision,
//      decided_by, reason}` for any listener still on the
//      expenses-specific name. A decision for an unknown request
//      id, a mismatched report, or a report that is no longer
//      `submitted` is ignored as a safe no-op (the request may
//      have been withdrawn). The expenses-specific
//      `expenses.approval.decided` event is still accepted
//      directly with `{approval_request_id, report_id, decision,
//      decided_by, reason}` where `decision` is `approved`
//      or `rejected`, for emitters that speak the old name.
//   3. On approval this module creates a reimbursement row in
//      `pending` and emits `expenses.reimbursement.requested`
//      with `{reimbursement_id, report_id, employee_id,
//      amount_minor, currency, payable_reference}` for the
//      billing flow. It does NOT move any money.
//   4. The billing flow confirms the pay-out by emitting
//      `expenses.reimbursement.paid` with
//      `{reimbursement_id, paid_at}` (or by emitting the
//      generic `payment.captured` carrying the same
//      `reimbursement_id`), and this module moves the
//      reimbursement to `paid` and the report to
//      `reimbursed`. A failure is confirmed with
//      `expenses.reimbursement.failed` (or `payment.failed`
//      carrying the `reimbursement_id`) with
//      `{reimbursement_id, reason}`; the report stays
//      `approved` and the reimbursement can be retried.
//   5. `POST /reports/{id}/withdraw` moves a `submitted` or
//      `rejected` report back to `draft`, clears the approval
//      correlation ids AND the stale decision metadata
//      (`decided_at`, `decided_by`, `decision_reason`) - a
//      withdrawn report is a fresh draft - and emits
//      `expenses.approval.cancelled` with
//      `{approval_request_id, report_id}` so a pending chain
//      can stand down. When a generic approvals request is
//      outstanding it also emits `approval.cancelled` with
//      `{request_id, cancelled_by}` (the approvals module's own
//      cancellation contract). Note the approvals module emits
//      that event itself when its own cancel endpoint runs and
//      does not consume it as an inbound event, so this emit is
//      the notification other listeners observe, not a call
//      into the approvals engine. There is no second approval
//      engine in this module: no endpoint approves or rejects a
//      report.
//
// Submit and retry are guarded per row: the draft check and the
// move to `submitted` happen under the per-report write guard,
// and the failed check and the move to `pending` happen under
// the per-reimbursement write guard, so concurrent submits or
// retries cannot both succeed and emit duplicate requests.
//
// Currency contract: conversion uses a stored rate only. This
// module first consults its own stored rates, then emits
// `calculator.rate_requested` with `{base_currency,
// target_currency}` and uses the first rate the calculator
// module answers with (its stored rate, if any). When neither
// side holds a rate the conversion fails with 422 - a rate is
// never guessed, interpolated, or fetched live from any
// provider by this module.
//
// Events emitted on the hook bus:
//   - `expenses.category.created`, `expenses.category.updated`,
//     `expenses.category.deleted`
//   - `expenses.policy.created`, `expenses.policy.updated`,
//     `expenses.policy.deleted`
//   - `expenses.report.created`, `expenses.report.updated`,
//     `expenses.report.deleted`, `expenses.report.submitted`,
//     `expenses.report.withdrawn`, `expenses.report.approved`,
//     `expenses.report.rejected`, `expenses.report.reimbursed`
//   - `expenses.approval.requested`, `expenses.approval.cancelled`
//   - `expenses.expense.created`, `expenses.expense.updated`,
//     `expenses.expense.deleted`
//   - `expenses.receipt.attached`, `expenses.receipt.removed`,
//     `expenses.receipt.verified`, `expenses.receipt.unverified`
//   - `expenses.reimbursement.requested`,
//     `expenses.reimbursement.retried`
//   - `expenses.adjustment.created`
//   - `expenses.currency_rate.created`,
//     `expenses.currency_rate.updated`,
//     `expenses.currency_rate.deleted`
//   - `expenses.settings.updated`
//   - `approval.requested` (generic approvals contract; see the
//     contract above) - emitted on submit alongside
//     `expenses.approval.requested`.
//   - `approval.cancelled` (generic approvals cancellation;
//     see step 5 above) - emitted on withdraw when a generic
//     approval request is outstanding.
//   - `calculator.rate_requested` (rate lookup; answered by the
//     calculator module when it holds a stored rate)
//   - `notification.requested` (approval decision and
//     reimbursement outcome, addressed to the employee)
// Events this module listens to:
//   - `storage.object.stored` - a stored object confirms a
//     receipt: receipts referencing its key become verified
//     (see the receipt verification note above).
//   - `storage.object.deleted` - a deleted object un-verifies
//     receipts referencing its key.
//   - `approval.decided` - generic approval decision from the
//     approvals flow (see the contract above); correlated by the
//     stored approvals `request_id`, then re-emitted as
//     `expenses.approval.decided`.
//   - `expenses.approval.decided` - approval decision from the
//     approvals flow (see the contract above).
//   - `expenses.reimbursement.paid` - reimbursement confirmed
//     paid by the billing flow.
//   - `expenses.reimbursement.failed` - reimbursement failed in
//     the billing flow.
//   - `payment.captured` - treated as a reimbursement payment
//     only when the payload carries a `reimbursement_id` this
//     module owns; anything else is ignored.
//   - `payment.failed` - treated as a reimbursement failure
//     only when the payload carries a `reimbursement_id` this
//     module owns; anything else is ignored.

// Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::*;

// Module

pub struct ExpensesModule {
    pub(crate) state: Arc<ExpensesState>,
}

impl ExpensesModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(ExpensesState::default()),
        }
    }
}

impl Default for ExpensesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ExpensesModule {
    fn id(&self) -> &'static str {
        "expenses"
    }

    fn name(&self) -> &'static str {
        "Expenses"
    }

    fn description(&self) -> &'static str {
        "Employee expenses: admin-editable categories with receipt and limit rules, per-company policies with mileage and per-diem rates, expense reports with a checked draft to reimbursed lifecycle, receipt references through the storage modules, approval through the approvals flow (events only, no second approval engine here), reimbursement pay-outs requested from billing (events only), stored-rate currency conversion with calculator fallback, and spend / approvals / backlog / violations analytics. All lists, rates, limits, and defaults are admin-configurable data, never hard-coded."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["billing", "storage", "roles"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["billing", "storage", "roles"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("expenses", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            APPROVAL_DECIDED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_approval_decided(state.clone(), ctx)
            }),
        );
        // Generic approvals decision (see the module header): the
        // bridge half of the submit-time `approval.requested`.
        // Correlates by the stored approvals request id and
        // re-emits as `expenses.approval.decided` for the listener
        // above, which applies the outcome exactly once.
        let state = self.state.clone();
        bus.register(
            GENERIC_APPROVAL_DECIDED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_generic_approval_decided(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            REIMBURSEMENT_PAID,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_reimbursement_paid(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            REIMBURSEMENT_FAILED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_reimbursement_failed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PAYMENT_CAPTURED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_event(state.clone(), ctx, true)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PAYMENT_FAILED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_event(state.clone(), ctx, false)
            }),
        );
        // Receipt verification (see the module header): the
        // storage manager confirms an object exists by emitting
        // `storage.object.stored` for its key, and un-confirms
        // it with `storage.object.deleted`. Only a verified
        // receipt clears the `receipt_missing` violation.
        let state = self.state.clone();
        bus.register(
            STORAGE_OBJECT_STORED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_storage_object_stored(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORAGE_OBJECT_DELETED,
            "expenses",
            Arc::new(move |ctx: &mut HookContext| {
                on_storage_object_deleted(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/expenses")
                .app_data(state)
                .route("/categories", web::get().to(list_categories))
                .route("/categories", web::post().to(create_category))
                .route("/categories/{id}", web::get().to(get_category))
                .route("/categories/{id}", web::patch().to(update_category))
                .route("/categories/{id}", web::delete().to(delete_category))
                .route("/policies", web::get().to(list_policies))
                .route("/policies", web::post().to(create_policy))
                .route("/policies/{id}", web::get().to(get_policy))
                .route("/policies/{id}", web::patch().to(update_policy))
                .route("/policies/{id}", web::delete().to(delete_policy))
                .route("/reports", web::get().to(list_reports))
                .route("/reports", web::post().to(create_report))
                .route("/reports/{id}", web::get().to(get_report))
                .route("/reports/{id}", web::patch().to(update_report))
                .route("/reports/{id}", web::delete().to(delete_report))
                .route("/reports/{id}/submit", web::post().to(submit_report))
                .route("/reports/{id}/withdraw", web::post().to(withdraw_report))
                .route("/reports/{id}/export", web::get().to(export_report))
                .route("/reports/{id}/expenses", web::get().to(list_report_expenses))
                .route("/reports/{id}/expenses", web::post().to(create_expense))
                .route("/expenses/{id}", web::get().to(get_expense))
                .route("/expenses/{id}", web::patch().to(update_expense))
                .route("/expenses/{id}", web::delete().to(delete_expense))
                .route("/expenses/{id}/receipts", web::post().to(attach_receipt))
                .route(
                    "/expenses/{id}/receipts/{receipt_id}",
                    web::delete().to(remove_receipt),
                )
                .route("/reimbursements", web::get().to(list_reimbursements))
                .route("/reimbursements/{id}", web::get().to(get_reimbursement))
                .route(
                    "/reimbursements/{id}/retry",
                    web::post().to(retry_reimbursement),
                )
                .route("/reports/{id}/adjustments", web::get().to(list_adjustments))
                .route(
                    "/reports/{id}/adjustments",
                    web::post().to(create_adjustment),
                )
                .route("/currency-rates", web::get().to(list_currency_rates))
                .route("/currency-rates", web::post().to(create_currency_rate))
                .route("/currency-rates/{id}", web::get().to(get_currency_rate))
                .route(
                    "/currency-rates/{id}",
                    web::delete().to(delete_currency_rate),
                )
                .route("/currency-rates/convert", web::post().to(convert_currency))
                .route("/analytics/spend", web::get().to(spend_analytics))
                .route(
                    "/analytics/pending-approvals",
                    web::get().to(pending_approvals_analytics),
                )
                .route(
                    "/analytics/reimbursement-backlog",
                    web::get().to(reimbursement_backlog_analytics),
                )
                .route("/analytics/violations", web::get().to(violations_analytics))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/expenses/categories", owner: "expenses", description: "List expense categories (paginated, filter by company_id, active, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/expenses/categories", owner: "expenses", description: "Create an expense category with receipt and limit rules (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/categories/{id}", owner: "expenses", description: "Get one expense category.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/expenses/categories/{id}", owner: "expenses", description: "Update an expense category (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/categories/{id}", owner: "expenses", description: "Delete a category with no expenses (manager; deactivate instead when in use).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/policies", owner: "expenses", description: "List expense policies (paginated, filter by company_id, active, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/expenses/policies", owner: "expenses", description: "Create a per-company expense policy with mileage / per-diem rates and an approval chain reference (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/policies/{id}", owner: "expenses", description: "Get one expense policy.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/expenses/policies/{id}", owner: "expenses", description: "Update an expense policy (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/policies/{id}", owner: "expenses", description: "Delete a policy with no reports (manager; deactivate instead when in use).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/reports", owner: "expenses", description: "List expense reports (own only unless manager; filter by status, employee_id, company_id, policy_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/expenses/reports", owner: "expenses", description: "Create a draft expense report.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/reports/{id}", owner: "expenses", description: "Get one report with its expense lines (own or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/expenses/reports/{id}", owner: "expenses", description: "Update a draft report (title, notes, policy).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/reports/{id}", owner: "expenses", description: "Delete a draft report with its lines, receipts, and adjustments.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/expenses/reports/{id}/submit", owner: "expenses", description: "Submit a draft report: checks receipt rules, moves to submitted under a per-report guard (concurrent submits cannot duplicate the approval request), and emits expenses.approval.requested plus the generic approval.requested for the approvals flow.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/expenses/reports/{id}/withdraw", owner: "expenses", description: "Withdraw a submitted or rejected report back to draft (cancels the pending approval request on both flows and clears the stale decision metadata).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/reports/{id}/export", owner: "expenses", description: "Export a report as JSON or CSV (?format=json|csv; CSV is a plain-text body, not the JSON envelope).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/reports/{id}/expenses", owner: "expenses", description: "List the expense lines of a report (paginated, filter by kind, category_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/expenses/reports/{id}/expenses", owner: "expenses", description: "Add an expense line to a draft report (mileage and per-diem amounts are computed server-side from policy rates).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/expenses/{id}", owner: "expenses", description: "Get one expense line with its receipt references (own or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/expenses/expenses/{id}", owner: "expenses", description: "Update an expense line on a draft report (mileage / per-diem amounts recompute from the policy rate).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/expenses/{id}", owner: "expenses", description: "Delete an expense line from a draft report (with its receipt references).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/expenses/expenses/{id}/receipts", owner: "expenses", description: "Attach a receipt reference (storage target + key; no bytes live here) to a draft-report expense. The receipt is stored unverified and only clears the receipt-missing flag once storage confirms the object exists.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/expenses/{id}/receipts/{receipt_id}", owner: "expenses", description: "Remove a receipt reference from a draft-report expense.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/reimbursements", owner: "expenses", description: "List reimbursements (own only unless manager; filter by status, employee_id, report_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/reimbursements/{id}", owner: "expenses", description: "Get one reimbursement (own or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/expenses/reimbursements/{id}/retry", owner: "expenses", description: "Re-request a failed reimbursement from billing (manager; failed only; guarded so concurrent retries cannot duplicate the request).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/reports/{id}/adjustments", owner: "expenses", description: "List refunds and adjustments of a report (paginated, filter by kind).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/expenses/reports/{id}/adjustments", owner: "expenses", description: "Record a refund or adjustment against an approved or reimbursed report (manager; reduces a still-pending reimbursement, never mutates history).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/currency-rates", owner: "expenses", description: "List stored exchange rates (paginated, filter by base_currency, target_currency).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/expenses/currency-rates", owner: "expenses", description: "Store an exchange rate (manager; one pair is one row - storing a pair again updates it in place; rates are stored data, never fetched live).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/currency-rates/{id}", owner: "expenses", description: "Get one stored exchange rate.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/expenses/currency-rates/{id}", owner: "expenses", description: "Delete a stored exchange rate (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/expenses/currency-rates/convert", owner: "expenses", description: "Convert an amount with a stored rate (own rates first, then the calculator module's stored rate; 422 when no stored rate exists).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/analytics/spend", owner: "expenses", description: "Spend analytics grouped by category, employee, or period and by currency (?group_by=category|employee|period, ?from=YYYY-MM-DD, ?to=YYYY-MM-DD; submitted, approved, and reimbursed reports only; refunds reduce net spend).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/analytics/pending-approvals", owner: "expenses", description: "Reports waiting on an approval decision (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/analytics/reimbursement-backlog", owner: "expenses", description: "Pending and failed reimbursements (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/analytics/violations", owner: "expenses", description: "Flagged expenses and over-limit reports (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/expenses/settings", owner: "expenses", description: "Get the configurable expenses settings (kinds, currencies, default rates).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/expenses/settings", owner: "expenses", description: "Update the configurable expenses settings (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ExpensesModule::new()));
}
