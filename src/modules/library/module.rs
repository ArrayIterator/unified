// Library - Module struct, route table and registration.

// Library module - catalog, copies, members, loans, holds, fines,
// digital access, inventory audits, reminders, and AI-assisted
// search and recommendations.
//
// Routes live under `/api/v1/library`:
//
//   Catalog items:
//     GET/POST /items - list (paginated, filtered) / create.
//     GET/PATCH/DELETE /items/{id}.
//     POST /items/{id}/withdraw - active -> withdrawn.
//   Copies:
//     GET/POST /items/{id}/copies - list / add a copy.
//     GET/PATCH/DELETE /copies/{id}.
//   Public catalog:
//     GET /catalog/search - read-only catalog search (rate-limited).
//     GET /catalog/{id} - one catalog entry with its copies.
//   Categories (data-driven taxonomy, admin-editable):
//     GET/POST /categories - list / create.
//     GET/PATCH/DELETE /categories/{id}.
//   Members (store only `user_id` plus a member type and a status;
//   user records themselves live in the users module):
//     GET/POST /members - list / register.
//     GET/PATCH/DELETE /members/{id}.
//     GET /members/{id}/loans - the member's loans.
//     GET /members/{id}/history - loan history of the member.
//     GET /my/loans - the caller's loans.
//   Loans and returns:
//     POST /loans - checkout (physical copy or digital item).
//     GET /loans - list (filter by member, item, copy, status).
//     GET /loans/{id}.
//     POST /loans/{id}/renew - renew (blocked by a hold or by an
//          overdue loan, per the active loan policy).
//     POST /loans/{id}/return - check-in; overdue returns raise a
//          fine through a billing event, never a direct charge.
//     POST /loans/{id}/mark-lost - lost handling with a replacement
//          charge requested through billing.
//     POST /loans/{id}/mark-damaged - damaged handling, same flow.
//     GET /loans/overdue - overdue loans.
//     POST /loans/scan-overdue - sweep: mark overdue loans, raise
//          fines, notify members.
//   Holds (FIFO queue):
//     POST /holds - reserve an item.
//     GET /holds - list (filter by member, item, status).
//     GET /holds/{id}.
//     POST /holds/{id}/cancel.
//     POST /holds/{id}/fulfill - staff marks a hold ready and the
//          member is notified.
//     POST /holds/{id}/expire - expire a ready hold and offer the
//          item to the next hold in the queue.
//   Fines (billing owns the money; this module only requests):
//     GET /fines - list (filter by member, loan, status).
//     GET /fines/{id}.
//     POST /fines/{id}/waive - waive a pending fine (manager).
//   Loan policies (all loan numbers are data, never constants):
//     GET/POST /policies - list / create.
//     GET/PATCH/DELETE /policies/{id}.
//   Digital collection (references only - bytes live in the
//   documents / storage modules):
//     POST /items/{id}/digital-access - issue a signed expiring
//          access token for an active digital loan.
//     GET /digital-access/{token} - redeem a token (validates the
//          signature and the expiry, then returns the stored
//          document / storage reference, never bytes).
//     GET /items/{id}/digital - the digital references of an item.
//   Inventory audits:
//     POST /inventory-audits - open an audit.
//     GET /inventory-audits - list.
//     GET /inventory-audits/{id}.
//     POST /inventory-audits/{id}/complete - close the audit and
//          emit the completion event with the discrepancy report.
//     POST /inventory-audits/{id}/discrepancies - record one.
//     POST /inventory-audits/{id}/discrepancies/{discrepancy_id}/resolve
//          - resolve (write-off needs an explicit approval flag).
//     GET /inventory-audits/{id}/report - the discrepancy report.
//   Reminders (delivery belongs to the notifications module):
//     POST /reminders/send-due-soon - notify members whose loans
//          fall due inside the configured reminder window.
//     POST /reminders/send-overdue - notify members with overdue
//          loans (skips loans already reminded in this overdue run).
//   Settings:
//     GET/PUT /settings - the configurable lists and defaults.
//   AI (smart module; the AI gate in the ai module owns the call):
//     POST /ai/search-assist - grounded catalog search help.
//     POST /ai/recommendations - suggestions only; never loans or
//          reserves anything.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory catalog / copy / member / loan / hold / fine / policy /
// audit management with ISBN-10/13 checksum validation, per-copy
// barcodes, data-driven loan policies (loan period, concurrent
// limit, renewal limit, fine rate, and grace period all come from
// the active policy record), FIFO hold queues with expiry,
// overdue sweeps that raise fines as billing events, signed
// expiring digital-access tokens (HMAC-SHA-256, same token form as
// the documents module), inventory audits with discrepancy
// reports and approved write-offs, and due-soon / overdue
// reminders emitted as notification events. What is NOT built
// here: durable storage (the DashMap stores are authoritative;
// migration `2026-11-22-000091_library` is the persistence
// target); money movement itself (billing owns it - this module
// only emits `library.fine.requested` and listens for the billing
// paid events); file bytes (ebook files are document / storage
// references only); notification delivery (the notifications
// module owns it - this module only emits
// `notification.requested`); and user records (members store only
// `user_id` plus a member type and a status).
//
// CONFIG-DRIVEN - READ THIS FIRST. Nothing behavioural is
// hard-coded: categories, languages, member types, item types,
// copy conditions, and every status vocabulary live in
// `LibrarySettings`, editable through `PUT /settings`. Loan
// periods, concurrent-loan limits, renewal limits, fine rates,
// and grace periods live in `LoanPolicy` records, editable
// through the policy endpoints; when no policy matches a member
// type and item type, checkout fails with a 409 instead of
// falling back to a baked-in number. The values in
// `LibrarySettings::default` are CONFIGURABLE DEFAULTS only.
// Languages are ISO codes stored as data, never a fixed list in
// logic; the default list only seeds the admin-editable setting.
//
// TENANT SCOPING - READ THIS FIRST (integration round, 2026-11-22).
// Every tenant-data handler in this module resolves the tenant
// FIRST via `tenancy::TenantContext::from_http_request` (see
// `require_tenant` below): no resolved tenant context means the
// request is rejected (401 missing context / 403 invalid tenant
// id), before caller identity, capability checks, store access,
// or any emit. No handler falls back to `public` and no tenant
// id, schema name, or package id is hard-coded - all are resolved
// at request time from the tenant context, per
// `unified_core::tenancy::TENANT_SCOPE_CONTRACT`. Company scoping
// is enforced at the handler level on top of that guard
// (`auth::caller_company`, `auth::require_company_match`,
// `auth::require_record_in_company_scope`,
// `auth::in_company_scope`): a body `company_id` outside the
// caller's scope is rejected with a 403, an omitted body company
// adopts the caller's scope, single-record reads and mutations
// reject records of another company, and every list handler
// filters to the caller's company. This module has no
// entitlement check of its own yet: the entitlements module
// answers `entitlements.check_requested`, but no grant records
// exist for this module and handlers do not reliably carry a
// company id, so a blocking entitlement gate would reject all
// traffic today; wiring it (grant per tenant + company id from
// the tenant context) is OPEN for the integrator. Likewise the
// stores are still in-memory DashMaps with no `DbPool`, so
// `with_tenant_connection` has no call site here yet and the maps
// are not partitioned by tenant - the handler checks above
// partition by `company_id`, not by tenant, so a tenant whose
// context carries no company scope sees unfiltered records, and
// hook listeners (which take no `HttpRequest`) carry no scope
// until event payloads carry `tenant_id`. Full per-tenant
// isolation lands with the Diesel writers (migration
// `2026-11-22-000091_library`) and a `tenant_id` on every record.
// The public catalog endpoints
// (`search_catalog`, `get_catalog_item`) are tenant catalogs, not
// control-plane data, so they are guarded too. Hook listeners
// (`on_*_grounding`, `on_billing_paid`, `on_user_deleted`) take
// no `HttpRequest` and read event payloads defensively; they
// carry no tenant scope until event payloads carry `tenant_id`.
//
// Actor identity: the caller is identified by the `X-User-Id`
// header (required) and `X-Role` carries the role slug the
// capability check runs against, exactly like the e-learning
// module. Capability checks use the `role.check` hook and fail
// closed (no answer means not allowed). The digital-access redeem
// endpoint additionally requires a valid signed token: the token
// is an extra check, never a replacement for the capability
// check.
//
// Events emitted on the hook bus:
//   - `library.item.created`, `library.item.updated`,
//     `library.item.withdrawn`
//   - `library.copy.created`, `library.copy.updated`,
//     `library.copy.deleted`
//   - `library.member.created`, `library.member.updated`,
//     `library.member.suspended`
//   - `library.loan.created`, `library.loan.renewed`,
//     `library.loan.returned`, `library.loan.overdue`,
//     `library.loan.marked_lost`, `library.loan.marked_damaged`
//   - `library.hold.created`, `library.hold.fulfilled`,
//     `library.hold.expired`, `library.hold.cancelled`
//   - `library.fine.requested` (`{"fine_id", "loan_id",
//     "member_id", "user_id", "amount_minor", "currency",
//     "reason", "module": "library"}` - the billing module owns
//     the charge itself), `library.fine.waived`,
//     `library.fine.paid`
//   - `library.policy.created`, `library.policy.updated`,
//     `library.policy.deleted`
//   - `library.digital_access.issued`,
//     `library.digital_access.redeemed`
//   - `library.inventory.completed` (with the discrepancy report),
//     `library.inventory.discrepancy.recorded`,
//     `library.inventory.discrepancy.resolved`
//   - `library.settings.updated`
//   - `notification.requested` (hold ready, due-soon, overdue -
//     in the notifications module's payload shape).
// Events this module listens to (all read defensively - a missing
// field or an unknown record is skipped, never an error):
//   - `invoice.paid`, `payment.completed` - the names billing
//     and the store actually emit. A payment whose payload names
//     a library fine (`fine_id`, or `reference_type` =
//     `"library.fine"` with `reference_id`, read from the nested
//     `invoice` record when one is present) marks that fine paid.
//   - `user.deleted` - suspends the library members tied to that
//     user id.
//   - `library.ai.search_assist_requested`,
//     `library.ai.recommendations_requested` - grounding hooks:
//     handlers add catalog matches (search) or anonymised
//     loan-history category counts (recommendations) via
//     `ctx.add_result(...)`; no member personal data is ever
//     added.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai::register_ai_capabilities;
use actix_web::web;
use std::sync::Arc;

use super::{AI_CAPABILITIES, LibraryState, add_discrepancy, ai_recommendations, ai_search_assist,
    audit_report, cancel_hold, complete_audit, create_audit, create_category, create_copy,
    create_hold, create_item, create_loan, create_member, create_policy, delete_category,
    delete_copy, delete_item, delete_member, delete_policy, expire_hold, fulfill_hold, get_audit,
    get_catalog_item, get_category, get_copy, get_fine, get_hold, get_item, get_item_digital,
    get_loan, get_member, get_policy, get_settings, issue_digital_access, list_audits,
    list_categories, list_fines, list_holds, list_item_copies, list_items, list_loans,
    list_member_loans, list_members, list_my_loans, list_overdue_loans, list_policies,
    mark_loan_damaged, mark_loan_lost, member_history, on_billing_paid,
    on_recommendations_grounding, on_search_assist_grounding, on_user_deleted,
    redeem_digital_access, renew_loan, resolve_discrepancy, return_loan, scan_overdue_loans,
    search_catalog, send_due_soon_reminders, send_overdue_reminders, update_category, update_copy,
    update_item, update_member, update_policy, update_settings, waive_fine, withdraw_item};

// Module

pub struct LibraryModule {
    state: LibraryState,
}

impl LibraryModule {
    pub fn new() -> Self {
        Self {
            state: LibraryState::default(),
        }
    }
}

impl Default for LibraryModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for LibraryModule {
    fn id(&self) -> &'static str {
        "library"
    }

    fn name(&self) -> &'static str {
        "Library"
    }

    fn description(&self) -> &'static str {
        "Library: catalog of books, ebooks, and journals with ISBN validation, per-copy barcodes and shelf locations, data-driven loan policies, checkout and return with renewals, overdue fines requested through billing, FIFO hold queues with expiry, signed expiring digital access, inventory audits with discrepancy reports, due-soon and overdue reminders through notifications, and AI-assisted search and recommendations."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &[
            "documents",
            "storage_manager",
            "billing",
            "notifications",
            "users",
            "roles",
        ]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "documents",
            "storage_manager",
            "billing",
            "notifications",
            "users",
            "roles",
        ]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        register_ai_capabilities("library", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "invoice.paid",
            "library",
            Arc::new(move |ctx: &mut HookContext| on_billing_paid(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "payment.completed",
            "library",
            Arc::new(move |ctx: &mut HookContext| on_billing_paid(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "user.deleted",
            "library",
            Arc::new(move |ctx: &mut HookContext| {
                on_user_deleted(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "library.ai.search_assist_requested",
            "library",
            Arc::new(move |ctx: &mut HookContext| {
                on_search_assist_grounding(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "library.ai.recommendations_requested",
            "library",
            Arc::new(move |ctx: &mut HookContext| {
                on_recommendations_grounding(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/library")
                .app_data(state)
                .route("/items", web::get().to(list_items))
                .route("/items", web::post().to(create_item))
                .route("/items/{id}", web::get().to(get_item))
                .route("/items/{id}", web::patch().to(update_item))
                .route("/items/{id}", web::delete().to(delete_item))
                .route("/items/{id}/withdraw", web::post().to(withdraw_item))
                .route("/items/{id}/copies", web::get().to(list_item_copies))
                .route("/items/{id}/copies", web::post().to(create_copy))
                .route("/copies/{id}", web::get().to(get_copy))
                .route("/copies/{id}", web::patch().to(update_copy))
                .route("/copies/{id}", web::delete().to(delete_copy))
                .route("/catalog/search", web::get().to(search_catalog))
                .route("/catalog/{id}", web::get().to(get_catalog_item))
                .route("/categories", web::get().to(list_categories))
                .route("/categories", web::post().to(create_category))
                .route("/categories/{id}", web::get().to(get_category))
                .route("/categories/{id}", web::patch().to(update_category))
                .route("/categories/{id}", web::delete().to(delete_category))
                .route("/members", web::get().to(list_members))
                .route("/members", web::post().to(create_member))
                .route("/members/{id}", web::get().to(get_member))
                .route("/members/{id}", web::patch().to(update_member))
                .route("/members/{id}", web::delete().to(delete_member))
                .route("/members/{id}/loans", web::get().to(list_member_loans))
                .route("/members/{id}/history", web::get().to(member_history))
                .route("/my/loans", web::get().to(list_my_loans))
                .route("/loans", web::post().to(create_loan))
                .route("/loans", web::get().to(list_loans))
                .route("/loans/overdue", web::get().to(list_overdue_loans))
                .route("/loans/scan-overdue", web::post().to(scan_overdue_loans))
                .route("/loans/{id}", web::get().to(get_loan))
                .route("/loans/{id}/renew", web::post().to(renew_loan))
                .route("/loans/{id}/return", web::post().to(return_loan))
                .route("/loans/{id}/mark-lost", web::post().to(mark_loan_lost))
                .route("/loans/{id}/mark-damaged", web::post().to(mark_loan_damaged))
                .route("/holds", web::post().to(create_hold))
                .route("/holds", web::get().to(list_holds))
                .route("/holds/{id}", web::get().to(get_hold))
                .route("/holds/{id}/cancel", web::post().to(cancel_hold))
                .route("/holds/{id}/fulfill", web::post().to(fulfill_hold))
                .route("/holds/{id}/expire", web::post().to(expire_hold))
                .route("/fines", web::get().to(list_fines))
                .route("/fines/{id}", web::get().to(get_fine))
                .route("/fines/{id}/waive", web::post().to(waive_fine))
                .route("/policies", web::get().to(list_policies))
                .route("/policies", web::post().to(create_policy))
                .route("/policies/{id}", web::get().to(get_policy))
                .route("/policies/{id}", web::patch().to(update_policy))
                .route("/policies/{id}", web::delete().to(delete_policy))
                .route("/items/{id}/digital-access", web::post().to(issue_digital_access))
                .route("/digital-access/{token}", web::get().to(redeem_digital_access))
                .route("/items/{id}/digital", web::get().to(get_item_digital))
                .route("/inventory-audits", web::post().to(create_audit))
                .route("/inventory-audits", web::get().to(list_audits))
                .route("/inventory-audits/{id}", web::get().to(get_audit))
                .route("/inventory-audits/{id}/complete", web::post().to(complete_audit))
                .route(
                    "/inventory-audits/{id}/discrepancies",
                    web::post().to(add_discrepancy),
                )
                .route(
                    "/inventory-audits/{id}/discrepancies/{discrepancy_id}/resolve",
                    web::post().to(resolve_discrepancy),
                )
                .route("/inventory-audits/{id}/report", web::get().to(audit_report))
                .route("/reminders/send-due-soon", web::post().to(send_due_soon_reminders))
                .route("/reminders/send-overdue", web::post().to(send_overdue_reminders))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/ai/search-assist", web::post().to(ai_search_assist))
                .route("/ai/recommendations", web::post().to(ai_recommendations)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/library/items", owner: "library", description: "List catalog items (paginated, filter by status, item_type, language, category, author, ISBN; full-text-ish substring search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/items", owner: "library", description: "Create a catalog item (catalog; ISBN-10/13 validated, unique per edition).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/items/{id}", owner: "library", description: "Get one catalog item.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/library/items/{id}", owner: "library", description: "Update a catalog item (catalog; withdrawn items cannot be updated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/library/items/{id}", owner: "library", description: "Delete a catalog item with no copies and no loan history (catalog; withdraw otherwise).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/items/{id}/withdraw", owner: "library", description: "Withdraw a catalog item and its available copies (catalog; blocked while copies are on loan).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/items/{id}/copies", owner: "library", description: "List the copies of an item (paginated, filter by status, shelf, location).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/items/{id}/copies", owner: "library", description: "Add a copy to an item (catalog; barcode unique across the library).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/copies/{id}", owner: "library", description: "Get one copy.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/library/copies/{id}", owner: "library", description: "Update a copy (catalog; status changes never move a copy on or off loan directly).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/library/copies/{id}", owner: "library", description: "Delete a copy with no loan history (catalog; withdraw instead when it has history).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/catalog/search", owner: "library", description: "Public catalog search (read-only, rate-limited per caller, filter by item_type, language, category, author, ISBN; includes availability counts).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/catalog/{id}", owner: "library", description: "Get one catalog entry with its copies (active items only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/categories", owner: "library", description: "List categories (paginated, filter by parent_id, searchable).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/categories", owner: "library", description: "Create a category (catalog; slug unique).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/categories/{id}", owner: "library", description: "Get one category.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/library/categories/{id}", owner: "library", description: "Update a category (catalog).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/library/categories/{id}", owner: "library", description: "Delete a category with no children and no items (catalog).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/members", owner: "library", description: "List library members (paginated, filter by member_type, status, user_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/members", owner: "library", description: "Register a library member (circulate; user_id plus member type, one active membership per user).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/members/{id}", owner: "library", description: "Get one member (own record or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/library/members/{id}", owner: "library", description: "Update a member (circulate; member type, status, expiry, notes).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/library/members/{id}", owner: "library", description: "Delete a member with no active loans and no pending fines (manage; suspend otherwise).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/members/{id}/loans", owner: "library", description: "List a member's loans (paginated, filter by status; own record or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/members/{id}/history", owner: "library", description: "Loan history of a member with totals and pending fines (own record or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/my/loans", owner: "library", description: "The caller's loans (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/loans", owner: "library", description: "Check out a copy or a digital item (circulate; policy limits, hold queue, and overdue blocking apply).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/loans", owner: "library", description: "List loans (paginated, filter by member, user, item, copy, status, due date; staff see all, others see only their own).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/loans/overdue", owner: "library", description: "List overdue loans (circulate; includes active loans past their due date).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/loans/scan-overdue", owner: "library", description: "Sweep active loans past due: mark overdue, raise fines through billing, notify members (circulate).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/loans/{id}", owner: "library", description: "Get one loan (own loan or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/loans/{id}/renew", owner: "library", description: "Renew a loan (circulate; blocked by a hold queue, by overdue status per settings, and by the policy renewal limit).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/loans/{id}/return", owner: "library", description: "Check in a loan (circulate; overdue returns raise a fine through billing; the next hold is offered the item).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/loans/{id}/mark-lost", owner: "library", description: "Mark a loan lost (circulate; replacement charge requested through billing when an amount is given).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/loans/{id}/mark-damaged", owner: "library", description: "Mark a loan damaged (circulate; replacement charge requested through billing when an amount is given).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/holds", owner: "library", description: "Reserve an item (circulate; FIFO queue position assigned).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/holds", owner: "library", description: "List holds (paginated, filter by member, item, status; staff see all, others see only their own).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/holds/{id}", owner: "library", description: "Get one hold (own hold or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/holds/{id}/cancel", owner: "library", description: "Cancel a queued or ready hold (own hold or circulate staff).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/holds/{id}/fulfill", owner: "library", description: "Mark a queued hold ready and notify the member (circulate).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/holds/{id}/expire", owner: "library", description: "Expire a ready or queued hold and offer the item to the next hold (circulate).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/fines", owner: "library", description: "List fines (paginated, filter by member, loan, status, reason; staff see all, others see only their own).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/fines/{id}", owner: "library", description: "Get one fine (own fine or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/fines/{id}/waive", owner: "library", description: "Waive a pending fine (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/policies", owner: "library", description: "List loan policies (paginated, filter by member_type, item_type, active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/policies", owner: "library", description: "Create a loan policy (manage; loan days, concurrent limit, renewal limit, fine rate, grace days).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/policies/{id}", owner: "library", description: "Get one loan policy.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/library/policies/{id}", owner: "library", description: "Update a loan policy (manage; one active policy per member-type and item-type pair).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/library/policies/{id}", owner: "library", description: "Delete a loan policy (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/items/{id}/digital-access", owner: "library", description: "Issue a signed expiring digital-access token for an active digital loan.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/digital-access/{token}", owner: "library", description: "Redeem a signed digital-access token (validates signature and expiry; returns document/storage references only, never bytes).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/items/{id}/digital", owner: "library", description: "Get the digital references of an item (document/storage references only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/inventory-audits", owner: "library", description: "Open an inventory audit (manage; optional location/shelf scope).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/inventory-audits", owner: "library", description: "List inventory audits (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/inventory-audits/{id}", owner: "library", description: "Get one inventory audit.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/inventory-audits/{id}/complete", owner: "library", description: "Complete an open audit and emit the discrepancy report (manage; blocked while discrepancies are unresolved).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/inventory-audits/{id}/discrepancies", owner: "library", description: "Record a discrepancy on an open audit (circulate; missing copies are marked lost, damaged copies marked damaged).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/inventory-audits/{id}/discrepancies/{discrepancy_id}/resolve", owner: "library", description: "Resolve a discrepancy (manage; write-off needs explicit approval and withdraws the copy).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/inventory-audits/{id}/report", owner: "library", description: "Discrepancy report of an audit (counts by kind and by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/reminders/send-due-soon", owner: "library", description: "Notify members whose loans fall due inside the configured reminder window (circulate).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/reminders/send-overdue", owner: "library", description: "Notify members with overdue loans (circulate; skips loans reminded in the last day).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/library/settings", owner: "library", description: "Get the configurable library settings (lists, defaults, limits).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/library/settings", owner: "library", description: "Update the configurable library settings (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/ai/search-assist", owner: "library", description: "AI-assisted catalog search (view; delegates to the AI gate, grounded on catalog matches).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/library/ai/recommendations", owner: "library", description: "AI recommendations from the anonymised loan-history profile (view; own user_id only unless manager; suggestions only).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LibraryModule::new()));
}
