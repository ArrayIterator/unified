// Library module — catalog, copies, members, loans, holds, fines,
// digital access, inventory audits, reminders, and AI-assisted
// search and recommendations.
//
// Routes live under `/api/v1/library`:
//
//   Catalog items:
//     GET/POST /items — list (paginated, filtered) / create.
//     GET/PATCH/DELETE /items/{id}.
//     POST /items/{id}/withdraw — active -> withdrawn.
//   Copies:
//     GET/POST /items/{id}/copies — list / add a copy.
//     GET/PATCH/DELETE /copies/{id}.
//   Public catalog:
//     GET /catalog/search — read-only catalog search (rate-limited).
//     GET /catalog/{id} — one catalog entry with its copies.
//   Categories (data-driven taxonomy, admin-editable):
//     GET/POST /categories — list / create.
//     GET/PATCH/DELETE /categories/{id}.
//   Members (store only `user_id` plus a member type and a status;
//   user records themselves live in the users module):
//     GET/POST /members — list / register.
//     GET/PATCH/DELETE /members/{id}.
//     GET /members/{id}/loans — the member's loans.
//     GET /members/{id}/history — loan history of the member.
//     GET /my/loans — the caller's loans.
//   Loans and returns:
//     POST /loans — checkout (physical copy or digital item).
//     GET /loans — list (filter by member, item, copy, status).
//     GET /loans/{id}.
//     POST /loans/{id}/renew — renew (blocked by a hold or by an
//          overdue loan, per the active loan policy).
//     POST /loans/{id}/return — check-in; overdue returns raise a
//          fine through a billing event, never a direct charge.
//     POST /loans/{id}/mark-lost — lost handling with a replacement
//          charge requested through billing.
//     POST /loans/{id}/mark-damaged — damaged handling, same flow.
//     GET /loans/overdue — overdue loans.
//     POST /loans/scan-overdue — sweep: mark overdue loans, raise
//          fines, notify members.
//   Holds (FIFO queue):
//     POST /holds — reserve an item.
//     GET /holds — list (filter by member, item, status).
//     GET /holds/{id}.
//     POST /holds/{id}/cancel.
//     POST /holds/{id}/fulfill — staff marks a hold ready and the
//          member is notified.
//     POST /holds/{id}/expire — expire a ready hold and offer the
//          item to the next hold in the queue.
//   Fines (billing owns the money; this module only requests):
//     GET /fines — list (filter by member, loan, status).
//     GET /fines/{id}.
//     POST /fines/{id}/waive — waive a pending fine (manager).
//   Loan policies (all loan numbers are data, never constants):
//     GET/POST /policies — list / create.
//     GET/PATCH/DELETE /policies/{id}.
//   Digital collection (references only — bytes live in the
//   documents / storage modules):
//     POST /items/{id}/digital-access — issue a signed expiring
//          access token for an active digital loan.
//     GET /digital-access/{token} — redeem a token (validates the
//          signature and the expiry, then returns the stored
//          document / storage reference, never bytes).
//     GET /items/{id}/digital — the digital references of an item.
//   Inventory audits:
//     POST /inventory-audits — open an audit.
//     GET /inventory-audits — list.
//     GET /inventory-audits/{id}.
//     POST /inventory-audits/{id}/complete — close the audit and
//          emit the completion event with the discrepancy report.
//     POST /inventory-audits/{id}/discrepancies — record one.
//     POST /inventory-audits/{id}/discrepancies/{discrepancy_id}/resolve
//          — resolve (write-off needs an explicit approval flag).
//     GET /inventory-audits/{id}/report — the discrepancy report.
//   Reminders (delivery belongs to the notifications module):
//     POST /reminders/send-due-soon — notify members whose loans
//          fall due inside the configured reminder window.
//     POST /reminders/send-overdue — notify members with overdue
//          loans (skips loans already reminded in this overdue run).
//   Settings:
//     GET/PUT /settings — the configurable lists and defaults.
//   AI (smart module; the AI gate in the ai module owns the call):
//     POST /ai/search-assist — grounded catalog search help.
//     POST /ai/recommendations — suggestions only; never loans or
//          reserves anything.
//
// HONESTY — READ THIS FIRST. What is real in this module:
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
// target); money movement itself (billing owns it — this module
// only emits `library.fine.requested` and listens for the billing
// paid events); file bytes (ebook files are document / storage
// references only); notification delivery (the notifications
// module owns it — this module only emits
// `notification.requested`); and user records (members store only
// `user_id` plus a member type and a status).
//
// CONFIG-DRIVEN — READ THIS FIRST. Nothing behavioural is
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
// TENANT SCOPING — READ THIS FIRST (integration round, 2026-11-22).
// Every tenant-data handler in this module resolves the tenant
// FIRST via `tenancy::TenantContext::from_http_request` (see
// `require_tenant` below): no resolved tenant context means the
// request is rejected (401 missing context / 403 invalid tenant
// id), before caller identity, capability checks, store access,
// or any emit. No handler falls back to `public` and no tenant
// id, schema name, or package id is hard-coded — all are resolved
// at request time from the tenant context, per
// `crate::core::tenancy::TENANT_SCOPE_CONTRACT`. This module has
// no entitlement check of its own yet: the entitlements module
// answers `entitlements.check_requested`, but no grant records
// exist for this module and handlers do not reliably carry a
// company id, so a blocking entitlement gate would reject all
// traffic today; wiring it (grant per tenant + company id from
// the tenant context) is OPEN for the integrator. Likewise the
// stores are still in-memory DashMaps with no `DbPool`, so
// `with_tenant_connection` has no call site here yet and the maps
// are not partitioned by tenant — the guard rejects unresolved
// tenants, but full per-tenant isolation lands with the Diesel
// writers (migration `2026-11-22-000091_library`) and a `tenant_id`
// on every record. The public catalog endpoints
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
//     "reason", "module": "library"}` — the billing module owns
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
//   - `notification.requested` (hold ready, due-soon, overdue —
//     in the notifications module's payload shape).
// Events this module listens to (all read defensively — a missing
// field or an unknown record is skipped, never an error):
//   - `billing.payment.completed`, `billing.invoice.paid` — a
//     payment whose payload names a library fine (`fine_id`, or
//     `reference_type` = `"library.fine"` with `reference_id`)
//     marks that fine paid.
//   - `user.deleted` — suspends the library members tied to that
//     user id.
//   - `library.ai.search_assist_requested`,
//     `library.ai.recommendations_requested` — grounding hooks:
//     handlers add catalog matches (search) or anonymised
//     loan-history category counts (recommendations) via
//     `ctx.add_result(...)`; no member personal data is ever
//     added.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy;
use crate::modules::ai::{register_ai_capabilities, run_ai_capability, AiCapability};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 300;
const MAX_TEXT_LEN: usize = 5000;
const MAX_SLUG_LEN: usize = 64;
const MAX_REF_LEN: usize = 500;
const MAX_CODE_LEN: usize = 64;
const MAX_NOTE_LEN: usize = 2000;
const MAX_AUTHORS: usize = 20;
const MAX_TAGS: usize = 30;
const MAX_GROUNDING_MATCHES: usize = 10;
const MAX_GROUNDING_CATEGORIES: usize = 10;

/// Capability names checked through the `role.check` hook.
const PERM_LIBRARY_VIEW: &str = "library.view";
const PERM_LIBRARY_MANAGE: &str = "library.manage";
const PERM_LIBRARY_CIRCULATE: &str = "library.circulate";
const PERM_LIBRARY_CATALOG: &str = "library.catalog";

/// Copy statuses. Stored values; the live list in settings is the
/// authority and these constants only name the values the loan flow
/// transitions between.
const COPY_AVAILABLE: &str = "available";
const COPY_ON_LOAN: &str = "on-loan";
const COPY_RESERVED: &str = "reserved";
const COPY_LOST: &str = "lost";
const COPY_DAMAGED: &str = "damaged";
const COPY_WITHDRAWN: &str = "withdrawn";

/// Loan statuses.
const LOAN_ACTIVE: &str = "active";
const LOAN_OVERDUE: &str = "overdue";
const LOAN_RETURNED: &str = "returned";
const LOAN_LOST: &str = "lost";
const LOAN_DAMAGED: &str = "damaged";

/// Hold statuses.
const HOLD_QUEUED: &str = "queued";
const HOLD_READY: &str = "ready";
const HOLD_FULFILLED: &str = "fulfilled";
const HOLD_CANCELLED: &str = "cancelled";
const HOLD_EXPIRED: &str = "expired";

/// Fine statuses.
const FINE_PENDING: &str = "pending";
const FINE_PAID: &str = "paid";
const FINE_WAIVED: &str = "waived";

/// Member statuses.
const MEMBER_ACTIVE: &str = "active";
const MEMBER_SUSPENDED: &str = "suspended";
const MEMBER_EXPIRED: &str = "expired";

/// Item statuses.
const ITEM_ACTIVE: &str = "active";
const ITEM_WITHDRAWN: &str = "withdrawn";

/// Audit statuses.
const AUDIT_OPEN: &str = "open";
const AUDIT_COMPLETED: &str = "completed";

/// Discrepancy kinds and resolutions. The kind list is validated
/// against settings; these constants name the values the audit
/// flow acts on.
const DISCREPANCY_MISSING: &str = "missing";
const DISCREPANCY_DAMAGED: &str = "damaged";
const DISCREPANCY_FOUND: &str = "found";
const DISCREPANCY_PENDING: &str = "pending";
const DISCREPANCY_RESOLVED: &str = "resolved";
const DISCREPANCY_WRITTEN_OFF: &str = "written_off";

/// AI capabilities this module exposes through the ai module's
/// capability registry. Recommendations are suggestions only.
pub const AI_CAPABILITIES: &[AiCapability] = &[
    AiCapability {
        id: "search_assist",
        module_id: "library",
        name: "Library search assist",
        description: "Helps a caller phrase and narrow a catalog search over titles, authors, ISBN values, and categories.",
        input_fields: &["query"],
        allows_pii: false,
        use_case: "library.search_assist",
    },
    AiCapability {
        id: "recommendations",
        module_id: "library",
        name: "Library recommendations",
        description: "Suggests catalog items from the anonymised category profile of a member's past loans. Suggestions only; nothing is loaned or reserved.",
        input_fields: &["user_id"],
        allows_pii: false,
        use_case: "library.recommendations",
    },
];

// ---------------------------------------------------------------------------
// Configurable settings (admin-editable; defaults are defaults only)
// ---------------------------------------------------------------------------

/// All behaviour-shaping lists and defaults of this module. An
/// administrator edits these through `PUT /settings`; no code path
/// consults a baked-in list instead of the live settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibrarySettings {
    /// Admin-editable taxonomy names (the category records carry
    /// the hierarchy; these names seed the picker defaults).
    pub categories: Vec<String>,
    /// Language codes (ISO codes) accepted for items. Data only.
    pub languages: Vec<String>,
    /// Member types. Each type maps to loan policies; the list is
    /// data, so a new type is added here plus a policy record.
    pub member_types: Vec<String>,
    /// Item types (physical and digital kinds are data).
    pub item_types: Vec<String>,
    /// Item status vocabulary.
    pub item_statuses: Vec<String>,
    /// Copy status vocabulary.
    pub copy_statuses: Vec<String>,
    /// Copy condition vocabulary (per-copy physical condition).
    pub copy_conditions: Vec<String>,
    /// Loan status vocabulary.
    pub loan_statuses: Vec<String>,
    /// Hold status vocabulary.
    pub hold_statuses: Vec<String>,
    /// Fine status vocabulary.
    pub fine_statuses: Vec<String>,
    /// Discrepancy kind vocabulary for inventory audits.
    pub discrepancy_kinds: Vec<String>,
    /// Default currency (3-letter code) for fines.
    pub default_currency: String,
    /// Days a ready hold stays claimable before it expires.
    pub hold_expiry_days: i64,
    /// Lifetime in seconds of an issued digital-access token.
    pub digital_link_ttl_secs: u64,
    /// Maximum catalog-search requests per caller per minute.
    pub public_search_rate_limit_per_minute: u64,
    /// Days before a due date that due-soon reminders go out.
    pub reminder_days_before_due: i64,
    /// When true, a member with an overdue loan cannot check out.
    pub block_checkout_when_overdue: bool,
    /// When true, an overdue loan may still be renewed.
    pub allow_renew_when_overdue: bool,
    /// Maximum matches returned by the AI grounding hooks.
    pub max_grounding_matches: usize,
}

impl Default for LibrarySettings {
    fn default() -> Self {
        Self {
            categories: vec![
                "fiction".to_string(),
                "non-fiction".to_string(),
                "science".to_string(),
                "history".to_string(),
                "reference".to_string(),
                "journals".to_string(),
            ],
            languages: vec![
                "en".to_string(),
                "id".to_string(),
                "fr".to_string(),
                "de".to_string(),
                "es".to_string(),
                "ar".to_string(),
                "zh".to_string(),
                "ja".to_string(),
            ],
            member_types: vec![
                "student".to_string(),
                "staff".to_string(),
                "public".to_string(),
            ],
            item_types: vec![
                "book".to_string(),
                "ebook".to_string(),
                "journal".to_string(),
                "magazine".to_string(),
                "thesis".to_string(),
                "multimedia".to_string(),
            ],
            item_statuses: vec![ITEM_ACTIVE.to_string(), ITEM_WITHDRAWN.to_string()],
            copy_statuses: vec![
                COPY_AVAILABLE.to_string(),
                COPY_ON_LOAN.to_string(),
                COPY_RESERVED.to_string(),
                COPY_LOST.to_string(),
                COPY_DAMAGED.to_string(),
                COPY_WITHDRAWN.to_string(),
            ],
            copy_conditions: vec![
                "new".to_string(),
                "good".to_string(),
                "fair".to_string(),
                "poor".to_string(),
                "damaged".to_string(),
            ],
            loan_statuses: vec![
                LOAN_ACTIVE.to_string(),
                LOAN_OVERDUE.to_string(),
                LOAN_RETURNED.to_string(),
                LOAN_LOST.to_string(),
                LOAN_DAMAGED.to_string(),
            ],
            hold_statuses: vec![
                HOLD_QUEUED.to_string(),
                HOLD_READY.to_string(),
                HOLD_FULFILLED.to_string(),
                HOLD_CANCELLED.to_string(),
                HOLD_EXPIRED.to_string(),
            ],
            fine_statuses: vec![
                FINE_PENDING.to_string(),
                FINE_PAID.to_string(),
                FINE_WAIVED.to_string(),
            ],
            discrepancy_kinds: vec![
                DISCREPANCY_MISSING.to_string(),
                DISCREPANCY_DAMAGED.to_string(),
                DISCREPANCY_FOUND.to_string(),
            ],
            default_currency: "USD".to_string(),
            hold_expiry_days: 7,
            digital_link_ttl_secs: 3600,
            public_search_rate_limit_per_minute: 60,
            reminder_days_before_due: 3,
            block_checkout_when_overdue: true,
            allow_renew_when_overdue: false,
            max_grounding_matches: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// Entities
// ---------------------------------------------------------------------------

/// One catalog entry: a book, ebook, journal, or other item. The
/// physical and digital forms share one record; `digital_ref` /
/// `document_ref` / `storage_ref` are opaque references handed to
/// the documents / storage modules, never bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: Uuid,
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    pub isbn: Option<String>,
    pub isbn_normalized: Option<String>,
    pub edition: Option<String>,
    pub series: Option<String>,
    pub series_position: Option<i64>,
    pub publisher: Option<String>,
    pub published_year: Option<i64>,
    pub language: String,
    pub category_id: Option<Uuid>,
    pub category_name: Option<String>,
    pub item_type: String,
    pub status: String,
    pub description: Option<String>,
    pub cover_ref: Option<String>,
    pub digital_ref: Option<String>,
    pub document_ref: Option<String>,
    pub storage_ref: Option<String>,
    pub tags: Vec<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One physical copy of a catalog item, tracked by barcode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryCopy {
    pub id: Uuid,
    pub item_id: Uuid,
    pub barcode: String,
    pub serial: Option<String>,
    pub condition: String,
    pub location: Option<String>,
    pub shelf: Option<String>,
    pub status: String,
    pub notes: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One catalog category (admin-editable taxonomy node).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryCategory {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A library member: only a link to a user record plus a member
/// type and a status. User data itself is never duplicated here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryMember {
    pub id: Uuid,
    pub user_id: String,
    pub member_type: String,
    pub status: String,
    pub joined_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One loan: a checkout of a copy (physical) or of a digital item
/// (no copy). Due dates come from the active loan policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryLoan {
    pub id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub item_id: Uuid,
    pub copy_id: Option<Uuid>,
    pub status: String,
    pub checked_out_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub returned_at: Option<DateTime<Utc>>,
    pub renewal_count: i64,
    pub last_reminded_at: Option<DateTime<Utc>>,
    pub last_overdue_noticed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One hold in the FIFO queue for an item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryHold {
    pub id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub item_id: Uuid,
    pub copy_id: Option<Uuid>,
    pub status: String,
    pub position: i64,
    pub ready_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One fine. The amount is a request to billing, never a charge
/// made here; `status` only changes to paid when billing reports
/// the payment, or to waived by a manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryFine {
    pub id: Uuid,
    pub loan_id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub amount_minor: i64,
    pub currency: String,
    pub reason: String,
    pub status: String,
    pub waived_by: Option<String>,
    pub paid_reference: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One loan policy: the data-driven rule set for one member type
/// and one item type. `item_type` may be the literal `"all"` to
/// cover every item type of that member type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoanPolicy {
    pub id: Uuid,
    pub name: String,
    pub member_type: String,
    pub item_type: String,
    pub loan_days: i64,
    pub max_concurrent: i64,
    pub max_renewals: i64,
    pub fine_per_day_minor: i64,
    pub currency: String,
    pub grace_days: i64,
    pub active: bool,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One inventory / shelf-check audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryAudit {
    pub id: Uuid,
    pub name: String,
    pub scope_location: Option<String>,
    pub scope_shelf: Option<String>,
    pub status: String,
    pub started_by: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub items_checked: i64,
    pub discrepancies_found: i64,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One discrepancy found during an audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryDiscrepancy {
    pub id: Uuid,
    pub audit_id: Uuid,
    pub copy_id: Uuid,
    pub item_id: Uuid,
    pub kind: String,
    pub note: Option<String>,
    pub status: String,
    pub resolved_by: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One issued digital-access grant. The token itself is a stateless
/// HMAC token (`"<loan-id>.<expiry-unix>.<hmac-hex>"`); this record
/// exists so grants can be listed and revoked, and so redemption
/// can be audited without trusting the token alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigitalAccess {
    pub id: Uuid,
    pub loan_id: Uuid,
    pub item_id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
    pub redeemed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateItemRequest {
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Option<Vec<String>>,
    pub isbn: Option<String>,
    pub edition: Option<String>,
    pub series: Option<String>,
    pub series_position: Option<i64>,
    pub publisher: Option<String>,
    pub published_year: Option<i64>,
    pub language: String,
    pub category_id: Option<String>,
    pub category_name: Option<String>,
    pub item_type: String,
    pub description: Option<String>,
    pub cover_ref: Option<String>,
    pub digital_ref: Option<String>,
    pub document_ref: Option<String>,
    pub storage_ref: Option<String>,
    pub tags: Option<Vec<String>>,
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateItemRequest {
    pub title: Option<String>,
    pub subtitle: Option<Option<String>>,
    pub authors: Option<Vec<String>>,
    pub isbn: Option<Option<String>>,
    pub edition: Option<Option<String>>,
    pub series: Option<Option<String>>,
    pub series_position: Option<Option<i64>>,
    pub publisher: Option<Option<String>>,
    pub published_year: Option<Option<i64>>,
    pub language: Option<String>,
    pub category_id: Option<Option<String>>,
    pub category_name: Option<Option<String>>,
    pub item_type: Option<String>,
    pub description: Option<Option<String>>,
    pub cover_ref: Option<Option<String>>,
    pub digital_ref: Option<Option<String>>,
    pub document_ref: Option<Option<String>>,
    pub storage_ref: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCopyRequest {
    pub barcode: String,
    pub serial: Option<String>,
    pub condition: Option<String>,
    pub location: Option<String>,
    pub shelf: Option<String>,
    pub notes: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCopyRequest {
    pub barcode: Option<String>,
    pub serial: Option<Option<String>>,
    pub condition: Option<String>,
    pub location: Option<Option<String>>,
    pub shelf: Option<Option<String>>,
    pub status: Option<String>,
    pub notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCategoryRequest {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub parent_id: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCategoryRequest {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<Option<String>>,
    pub parent_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateMemberRequest {
    pub user_id: String,
    pub member_type: String,
    pub expires_at: Option<String>,
    pub notes: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMemberRequest {
    pub member_type: Option<String>,
    pub status: Option<String>,
    pub expires_at: Option<Option<String>>,
    pub notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateLoanRequest {
    pub member_id: String,
    pub item_id: String,
    pub copy_id: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateHoldRequest {
    pub member_id: String,
    pub item_id: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePolicyRequest {
    pub name: String,
    pub member_type: String,
    pub item_type: String,
    pub loan_days: i64,
    pub max_concurrent: i64,
    pub max_renewals: i64,
    pub fine_per_day_minor: i64,
    pub currency: Option<String>,
    pub grace_days: i64,
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePolicyRequest {
    pub name: Option<String>,
    pub member_type: Option<String>,
    pub item_type: Option<String>,
    pub loan_days: Option<i64>,
    pub max_concurrent: Option<i64>,
    pub max_renewals: Option<i64>,
    pub fine_per_day_minor: Option<i64>,
    pub currency: Option<String>,
    pub grace_days: Option<i64>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAuditRequest {
    pub name: String,
    pub scope_location: Option<String>,
    pub scope_shelf: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDiscrepancyRequest {
    pub copy_id: String,
    pub kind: String,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResolveDiscrepancyRequest {
    pub resolution: String,
    pub approved: Option<bool>,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MarkLoanRequest {
    pub reason: Option<String>,
    pub replacement_amount_minor: Option<i64>,
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DigitalAccessRequest {
    pub loan_id: String,
    pub expires_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub categories: Option<Vec<String>>,
    pub languages: Option<Vec<String>>,
    pub member_types: Option<Vec<String>>,
    pub item_types: Option<Vec<String>>,
    pub item_statuses: Option<Vec<String>>,
    pub copy_statuses: Option<Vec<String>>,
    pub copy_conditions: Option<Vec<String>>,
    pub loan_statuses: Option<Vec<String>>,
    pub hold_statuses: Option<Vec<String>>,
    pub fine_statuses: Option<Vec<String>>,
    pub discrepancy_kinds: Option<Vec<String>>,
    pub default_currency: Option<String>,
    pub hold_expiry_days: Option<i64>,
    pub digital_link_ttl_secs: Option<u64>,
    pub public_search_rate_limit_per_minute: Option<u64>,
    pub reminder_days_before_due: Option<i64>,
    pub block_checkout_when_overdue: Option<bool>,
    pub allow_renew_when_overdue: Option<bool>,
    pub max_grounding_matches: Option<usize>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the library. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000091_library`; until that migration is executed,
/// library data does not survive a restart.
#[derive(Clone)]
pub struct LibraryState {
    pub items: Arc<DashMap<Uuid, LibraryItem>>,
    pub copies: Arc<DashMap<Uuid, LibraryCopy>>,
    pub categories: Arc<DashMap<Uuid, LibraryCategory>>,
    pub members: Arc<DashMap<Uuid, LibraryMember>>,
    pub loans: Arc<DashMap<Uuid, LibraryLoan>>,
    pub holds: Arc<DashMap<Uuid, LibraryHold>>,
    pub fines: Arc<DashMap<Uuid, LibraryFine>>,
    pub policies: Arc<DashMap<Uuid, LoanPolicy>>,
    pub audits: Arc<DashMap<Uuid, InventoryAudit>>,
    pub discrepancies: Arc<DashMap<Uuid, InventoryDiscrepancy>>,
    pub digital_access: Arc<DashMap<Uuid, DigitalAccess>>,
    pub settings: Arc<RwLock<LibrarySettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    pub signing_key: Arc<RwLock<[u8; 32]>>,
    pub search_rate: Arc<DashMap<String, (u64, i64)>>,
}

impl Default for LibraryState {
    fn default() -> Self {
        // Per-process signing key from two random UUIDs (32 bytes),
        // same approach as the documents module: tokens issued by
        // this process cannot be forged without the key, and the key
        // never leaves memory.
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(first.as_bytes());
        key[16..].copy_from_slice(second.as_bytes());
        Self {
            items: Arc::new(DashMap::new()),
            copies: Arc::new(DashMap::new()),
            categories: Arc::new(DashMap::new()),
            members: Arc::new(DashMap::new()),
            loans: Arc::new(DashMap::new()),
            holds: Arc::new(DashMap::new()),
            fines: Arc::new(DashMap::new()),
            policies: Arc::new(DashMap::new()),
            audits: Arc::new(DashMap::new()),
            discrepancies: Arc::new(DashMap::new()),
            digital_access: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(LibrarySettings::default())),
            hooks: Arc::new(RwLock::new(None)),
            signing_key: Arc::new(RwLock::new(key)),
            search_rate: Arc::new(DashMap::new()),
        }
    }
}

impl LibraryState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn settings(&self) -> LibrarySettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "library",
                "category": "library",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Request a fine charge from billing. This module never moves
    /// money: it records the fine and emits the request event, and
    /// billing owns the charge and reports the outcome.
    fn request_fine(&self, fine: &LibraryFine) {
        self.emit(
            "library.fine.requested",
            json!({
                "fine_id": fine.id.to_string(),
                "loan_id": fine.loan_id.to_string(),
                "member_id": fine.member_id.to_string(),
                "user_id": fine.user_id,
                "amount_minor": fine.amount_minor,
                "currency": fine.currency,
                "reason": fine.reason,
                "module": "library",
            }),
        );
    }

    /// HMAC-SHA-256 signature hex for a loan id + expiry, same token
    /// form as the documents module.
    fn sign_token(&self, loan_id: &str, expiry: i64) -> String {
        let key = *self.signing_key.read().expect("signing key lock poisoned");
        let payload = format!("{loan_id}:{expiry}");
        let mut mac = Hmac::<Sha256>::new_from_slice(&key)
            .expect("HMAC key is always a valid length");
        mac.update(payload.as_bytes());
        let tag = mac.finalize().into_bytes();
        tag.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Issue a signed digital-access token for a loan.
    fn issue_signed_token(&self, loan_id: &str, expires_secs: u64) -> (String, DateTime<Utc>) {
        let expires_at = Utc::now() + chrono::Duration::seconds(expires_secs as i64);
        let expiry = expires_at.timestamp();
        let signature = self.sign_token(loan_id, expiry);
        (
            format!("{loan_id}.{expiry}.{signature}"),
            expires_at,
        )
    }

    /// Validate a signed digital-access token. Returns the loan id on
    /// success; rejects expired or tampered tokens.
    fn verify_signed_token(&self, token: &str) -> UnifiedResult<String> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(UnifiedError::forbidden("Invalid access token"));
        }
        let loan_id = parts[0].to_string();
        let expiry: i64 = parts[1]
            .parse()
            .map_err(|_| UnifiedError::forbidden("Invalid access token"))?;
        if expiry <= Utc::now().timestamp() {
            return Err(UnifiedError::forbidden("Access token has expired"));
        }
        let expected = self.sign_token(&loan_id, expiry);
        if !constant_time_eq(&expected, parts[2]) {
            return Err(UnifiedError::forbidden("Access token is invalid"));
        }
        Ok(loan_id)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Constant-time string comparison, so token verification does not
/// leak how many leading characters matched.
fn constant_time_eq(expected: &str, actual: &str) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .as_bytes()
        .iter()
        .zip(actual.as_bytes().iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the e-learning module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
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

/// Resolve the tenant for this request, failing closed when the
/// tenant-resolution middleware attached no tenant context (401)
/// or the resolved tenant id is not a uuidv7 UUID (403). Every
/// tenant-data handler in this module calls this FIRST, before
/// caller identity or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant. See
/// `crate::core::tenancy` (`TENANT_SCOPE_CONTRACT`).
///
/// Diesel persistence is not wired yet (the DashMap stores are
/// authoritative; migration `2026-11-22-000091_library` is the
/// persistence target), so there is no `DbPool` in this module
/// and `with_tenant_connection` has no call site here yet — when
/// the Diesel writers land, all data access goes through
/// `tenancy::with_tenant_connection(pool, tenant.tenant_id, ...)`.
/// Until then the guard rejects unresolved tenants, but the
/// in-memory stores are not yet partitioned by tenant: records
/// carry no `tenant_id` yet, so per-tenant isolation of the maps
/// themselves is still OPEN for the integrator (see the module
/// header convergence note).
fn require_tenant(req: &HttpRequest) -> UnifiedResult<tenancy::TenantContext> {
    tenancy::TenantContext::from_http_request(req)
}

/// Owning company, resolved from the `X-Company-Id` header when
/// present; `None` when the header is absent or not a UUID.
fn resolve_company_id(req: &HttpRequest) -> Option<Uuid> {
    req.headers()
        .get("x-company-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| Uuid::parse_str(raw.trim()).ok())
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the e-learning module).
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

/// True when the caller may circulate (loans / returns / holds):
/// the circulate capability or the manage capability.
fn can_circulate(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_LIBRARY_CIRCULATE)
        || check_capability(ctx, role, PERM_LIBRARY_MANAGE)
}

fn require_circulate(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    if can_circulate(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_LIBRARY_CIRCULATE}\" or \"{PERM_LIBRARY_MANAGE}\""
        )))
    }
}

/// True when the caller may edit the catalog: the catalog
/// capability or the manage capability.
fn can_catalog(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_LIBRARY_CATALOG)
        || check_capability(ctx, role, PERM_LIBRARY_MANAGE)
}

fn require_catalog(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    if can_catalog(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_LIBRARY_CATALOG}\" or \"{PERM_LIBRARY_MANAGE}\""
        )))
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn parse_optional_uuid(raw: &Option<String>, what: &str) -> UnifiedResult<Option<Uuid>> {
    match raw {
        Some(value) => Ok(Some(parse_uuid(value, what)?)),
        None => Ok(None),
    }
}

fn parse_datetime(raw: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    raw.parse::<DateTime<Utc>>()
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be an RFC 3339 timestamp")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// Validate a value against a settings list. The list is the live
/// admin-configured one — never a constant in this file.
fn require_in_list(value: &str, field: &str, allowed: &[String]) -> UnifiedResult<String> {
    let checked = require_text(value, field, MAX_SLUG_LEN)?;
    if !allowed.iter().any(|item| item == &checked) {
        return Err(UnifiedError::bad_request(format!(
            "{field} \"{checked}\" is not allowed; allowed values: {}",
            allowed.join(", ")
        )));
    }
    Ok(checked)
}

fn require_currency(value: &str) -> UnifiedResult<String> {
    let checked = require_text(value, "currency", 3)?;
    if checked.len() != 3 || !checked.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code",
        ));
    }
    Ok(checked.to_ascii_uppercase())
}

fn require_non_negative(value: i64, field: &str) -> UnifiedResult<i64> {
    if value < 0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be negative"
        )));
    }
    Ok(value)
}

fn require_positive(value: i64, field: &str) -> UnifiedResult<i64> {
    if value < 1 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at least 1"
        )));
    }
    Ok(value)
}

fn slugify(raw: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in raw.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
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

fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Normalize an ISBN by stripping hyphens and spaces, then
/// validate the ISBN-10 or ISBN-13 checksum. Returns the
/// normalized digits, or an error naming the problem.
fn normalize_isbn(raw: &str) -> UnifiedResult<String> {
    let digits: String = raw
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .collect();
    let upper = digits.to_ascii_uppercase();
    match upper.len() {
        10 => {
            let mut sum: i64 = 0;
            for (index, ch) in upper.chars().enumerate() {
                let value: i64 = if index == 9 && ch == 'X' {
                    10
                } else if ch.is_ascii_digit() {
                    (ch as i64) - ('0' as i64)
                } else {
                    return Err(UnifiedError::bad_request(
                        "isbn must be a valid ISBN-10 (digits, with X allowed as the last character)",
                    ));
                };
                sum += value * (10 - index as i64);
            }
            if sum % 11 != 0 {
                return Err(UnifiedError::bad_request(
                    "isbn checksum is invalid for ISBN-10",
                ));
            }
            Ok(upper)
        }
        13 => {
            if !upper.chars().all(|ch| ch.is_ascii_digit()) {
                return Err(UnifiedError::bad_request(
                    "isbn must be a valid ISBN-13 (13 digits)",
                ));
            }
            let mut sum: i64 = 0;
            for (index, ch) in upper.chars().enumerate() {
                let value: i64 = (ch as i64) - ('0' as i64);
                sum += if index % 2 == 0 { value } else { value * 3 };
            }
            if sum % 10 != 0 {
                return Err(UnifiedError::bad_request(
                    "isbn checksum is invalid for ISBN-13",
                ));
            }
            Ok(upper)
        }
        _ => Err(UnifiedError::bad_request(
            "isbn must be 10 or 13 characters after removing hyphens and spaces",
        )),
    }
}

/// Resolve the active loan policy for a member type and item type:
/// an exact match first, then the member type with item type
/// `"all"`, then a default policy for member type `"default"`.
/// Fails with 409 when no policy matches — checkout must never
/// fall back to a baked-in number.
fn resolve_policy(
    state: &LibraryState,
    member_type: &str,
    item_type: &str,
) -> UnifiedResult<LoanPolicy> {
    let mut active: Vec<LoanPolicy> = state
        .policies
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|policy| policy.active)
        .collect();
    active.sort_by_key(|policy| policy.created_at);
    let exact = active
        .iter()
        .find(|policy| policy.member_type == member_type && policy.item_type == item_type);
    if let Some(policy) = exact {
        return Ok(policy.clone());
    }
    let member_wide = active
        .iter()
        .find(|policy| policy.member_type == member_type && policy.item_type == "all");
    if let Some(policy) = member_wide {
        return Ok(policy.clone());
    }
    let fallback = active
        .iter()
        .find(|policy| policy.member_type == "default" && policy.item_type == "all");
    if let Some(policy) = fallback {
        return Ok(policy.clone());
    }
    Err(UnifiedError::conflict(format!(
        "No active loan policy covers member type \"{member_type}\" and item type \"{item_type}\"; create a loan policy first"
    )))
}

/// Overdue fine for a loan under a policy: full days past the due
/// date minus the policy grace days, times the daily fine rate.
/// Zero when the loan is inside the grace period.
fn overdue_fine_minor(policy: &LoanPolicy, due_at: DateTime<Utc>, at: DateTime<Utc>) -> i64 {
    if at <= due_at {
        return 0;
    }
    let days = (at - due_at).num_days();
    let chargeable = days - policy.grace_days;
    if chargeable <= 0 {
        return 0;
    }
    chargeable * policy.fine_per_day_minor
}

fn load_item(state: &LibraryState, id: &str) -> UnifiedResult<LibraryItem> {
    let uuid = parse_uuid(id, "item")?;
    state
        .items
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Item \"{id}\" not found")))
}

fn load_copy(state: &LibraryState, id: &str) -> UnifiedResult<LibraryCopy> {
    let uuid = parse_uuid(id, "copy")?;
    state
        .copies
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Copy \"{id}\" not found")))
}

fn load_member(state: &LibraryState, id: &str) -> UnifiedResult<LibraryMember> {
    let uuid = parse_uuid(id, "member")?;
    state
        .members
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Member \"{id}\" not found")))
}

fn load_loan(state: &LibraryState, id: &str) -> UnifiedResult<LibraryLoan> {
    let uuid = parse_uuid(id, "loan")?;
    state
        .loans
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Loan \"{id}\" not found")))
}

fn load_hold(state: &LibraryState, id: &str) -> UnifiedResult<LibraryHold> {
    let uuid = parse_uuid(id, "hold")?;
    state
        .holds
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Hold \"{id}\" not found")))
}

fn load_fine(state: &LibraryState, id: &str) -> UnifiedResult<LibraryFine> {
    let uuid = parse_uuid(id, "fine")?;
    state
        .fines
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Fine \"{id}\" not found")))
}

fn load_policy(state: &LibraryState, id: &str) -> UnifiedResult<LoanPolicy> {
    let uuid = parse_uuid(id, "policy")?;
    state
        .policies
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Loan policy \"{id}\" not found")))
}

fn load_audit(state: &LibraryState, id: &str) -> UnifiedResult<InventoryAudit> {
    let uuid = parse_uuid(id, "inventory audit")?;
    state
        .audits
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Inventory audit \"{id}\" not found")))
}

fn load_category(state: &LibraryState, id: &str) -> UnifiedResult<LibraryCategory> {
    let uuid = parse_uuid(id, "category")?;
    state
        .categories
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Category \"{id}\" not found")))
}

/// Next FIFO position for a new hold on an item.
fn next_hold_position(state: &LibraryState, item_id: Uuid) -> i64 {
    state
        .holds
        .iter()
        .filter(|entry| {
            entry.value().item_id == item_id
                && (entry.value().status == HOLD_QUEUED || entry.value().status == HOLD_READY)
        })
        .map(|entry| entry.value().position)
        .max()
        .unwrap_or(0)
        + 1
}

/// First queued hold for an item, in FIFO order.
fn first_queued_hold(state: &LibraryState, item_id: Uuid) -> Option<LibraryHold> {
    let mut queued: Vec<LibraryHold> = state
        .holds
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|hold| hold.item_id == item_id && hold.status == HOLD_QUEUED)
        .collect();
    queued.sort_by_key(|hold| (hold.position, hold.created_at));
    queued.into_iter().next()
}

/// Offer a returned item to the next queued hold: the hold becomes
/// ready with an expiry from settings, and the member is notified.
fn offer_to_next_hold(state: &LibraryState, item: &LibraryItem) {
    let Some(hold) = first_queued_hold(state, item.id) else {
        return;
    };
    let settings = state.settings();
    let now = Utc::now();
    let mut updated = hold.clone();
    updated.status = HOLD_READY.to_string();
    updated.ready_at = Some(now);
    updated.expires_at = Some(now + chrono::Duration::days(settings.hold_expiry_days));
    updated.updated_at = now;
    state.holds.insert(updated.id, updated.clone());
    state.emit(
        "library.hold.fulfilled",
        json!({
            "hold_id": updated.id.to_string(),
            "member_id": updated.member_id.to_string(),
            "user_id": updated.user_id,
            "item_id": item.id.to_string(),
            "title": item.title,
            "ready_at": now.to_rfc3339(),
            "expires_at": updated.expires_at.map(|value| value.to_rfc3339()),
        }),
    );
    state.notify(
        "library.hold_ready",
        &updated.user_id,
        "Reserved item is ready",
        &format!(
            "The item \"{}\" you reserved is ready for pickup.",
            item.title
        ),
        Some(format!("/library/items/{}", item.id)),
    );
}

/// Create a pending fine for a loan and request the charge from
/// billing. Returns `None` when the amount is zero (no fine owed).
fn create_fine_for_loan(
    state: &LibraryState,
    loan: &LibraryLoan,
    amount_minor: i64,
    currency: &str,
    reason: &str,
) -> Option<LibraryFine> {
    if amount_minor <= 0 {
        return None;
    }
    // One pending fine per loan and reason: a second sweep must not
    // double-charge the same overdue loan.
    let existing = state.fines.iter().any(|entry| {
        entry.value().loan_id == loan.id
            && entry.value().reason == reason
            && entry.value().status == FINE_PENDING
    });
    if existing {
        return None;
    }
    let now = Utc::now();
    let fine = LibraryFine {
        id: Uuid::new_v4(),
        loan_id: loan.id,
        member_id: loan.member_id,
        user_id: loan.user_id.clone(),
        amount_minor,
        currency: currency.to_string(),
        reason: reason.to_string(),
        status: FINE_PENDING.to_string(),
        waived_by: None,
        paid_reference: None,
        company_id: loan.company_id.clone(),
        created_at: now,
        updated_at: now,
    };
    state.fines.insert(fine.id, fine.clone());
    state.request_fine(&fine);
    Some(fine)
}

/// Enforce the catalog-search rate limit for one caller. The limit
/// and the window come from settings; exceeding it answers 429.
fn check_search_rate(state: &LibraryState, actor: &str) -> UnifiedResult<()> {
    let settings = state.settings();
    let limit = settings.public_search_rate_limit_per_minute;
    if limit == 0 {
        return Ok(());
    }
    let now = Utc::now().timestamp();
    let window = now / 60;
    let mut entry = state
        .search_rate
        .entry(actor.to_string())
        .or_insert((0, window));
    if entry.1 != window {
        *entry = (0, window);
    }
    entry.0 += 1;
    if entry.0 > limit {
        return Err(UnifiedError::new(
            "Catalog search rate limit exceeded; try again next minute",
            429,
        )
        .with_metadata("retry_after_secs", json!(60 - (now % 60))));
    }
    Ok(())
}

/// Substring match used by catalog search across title, authors,
/// ISBN, category name, and tags. Case-insensitive.
fn item_matches_query(item: &LibraryItem, query: &str) -> bool {
    let needle = query.to_lowercase();
    if item.title.to_lowercase().contains(&needle) {
        return true;
    }
    if item
        .authors
        .iter()
        .any(|author| author.to_lowercase().contains(&needle))
    {
        return true;
    }
    if let Some(isbn) = &item.isbn_normalized {
        if isbn.to_lowercase().contains(&needle) {
            return true;
        }
    }
    if let Some(category) = &item.category_name {
        if category.to_lowercase().contains(&needle) {
            return true;
        }
    }
    item.tags
        .iter()
        .any(|tag| tag.to_lowercase().contains(&needle))
}

// ---------------------------------------------------------------------------
// Item handlers
// ---------------------------------------------------------------------------

async fn list_items(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryItem> = state
            .items
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(status) = filters.get("status") {
            items.retain(|item| &item.status == status);
        }
        if let Some(item_type) = filters.get("item_type") {
            items.retain(|item| &item.item_type == item_type);
        }
        if let Some(language) = filters.get("language") {
            items.retain(|item| &item.language == language);
        }
        if let Some(category) = filters.get("category_id") {
            items.retain(|item| {
                item.category_id
                    .map(|id| id.to_string() == *category)
                    .unwrap_or(false)
            });
        }
        if let Some(category_name) = filters.get("category_name") {
            items.retain(|item| {
                item.category_name
                    .as_deref()
                    .map(|name| name == category_name)
                    .unwrap_or(false)
            });
        }
        if let Some(author) = filters.get("author") {
            let needle = author.to_lowercase();
            items.retain(|item| {
                item.authors
                    .iter()
                    .any(|name| name.to_lowercase().contains(&needle))
            });
        }
        if let Some(isbn) = filters.get("isbn") {
            let needle = isbn.replace('-', "").replace(' ', "").to_uppercase();
            items.retain(|item| {
                item.isbn_normalized
                    .as_deref()
                    .map(|value| value.contains(&needle))
                    .unwrap_or(false)
            });
        }
        if let Some(search) = &pagination.search {
            items.retain(|item| item_matches_query(item, search));
        }
        items.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let input = body.into_inner();
        let settings = state.settings();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let language = require_in_list(&input.language, "language", &settings.languages)?;
        let item_type = require_in_list(&input.item_type, "item_type", &settings.item_types)?;
        let authors = match &input.authors {
            Some(list) => {
                if list.len() > MAX_AUTHORS {
                    return Err(UnifiedError::bad_request(format!(
                        "authors must have at most {MAX_AUTHORS} entries"
                    )));
                }
                let mut checked = Vec::with_capacity(list.len());
                for author in list {
                    checked.push(require_text(author, "authors", MAX_TITLE_LEN)?);
                }
                checked
            }
            None => Vec::new(),
        };
        let tags = match &input.tags {
            Some(list) => {
                if list.len() > MAX_TAGS {
                    return Err(UnifiedError::bad_request(format!(
                        "tags must have at most {MAX_TAGS} entries"
                    )));
                }
                let mut checked = Vec::with_capacity(list.len());
                for tag in list {
                    checked.push(require_text(tag, "tags", MAX_SLUG_LEN)?);
                }
                checked
            }
            None => Vec::new(),
        };
        let isbn_normalized = match &input.isbn {
            Some(raw) if !raw.trim().is_empty() => Some(normalize_isbn(raw)?),
            _ => None,
        };
        if let Some(normalized) = &isbn_normalized {
            let edition = input.edition.as_deref().unwrap_or("").trim();
            let duplicate = state.items.iter().any(|entry| {
                entry.value().isbn_normalized.as_deref() == Some(normalized.as_str())
                    && entry.value().edition.as_deref().unwrap_or("") == edition
                    && entry.value().status != ITEM_WITHDRAWN
            });
            if duplicate {
                return Err(UnifiedError::conflict(format!(
                    "An active item already uses ISBN {normalized} for this edition"
                )));
            }
        }
        let published_year = match input.published_year {
            Some(year) => {
                let current = Utc::now().format("%Y").to_string().parse::<i64>().unwrap_or(0);
                if year < 0 || year > current + 1 {
                    return Err(UnifiedError::bad_request(
                        "published_year must be a plausible year",
                    ));
                }
                Some(year)
            }
            None => None,
        };
        let series_position = match input.series_position {
            Some(value) => Some(require_non_negative(value, "series_position")?),
            None => None,
        };
        let category_id = parse_optional_uuid(&input.category_id, "category")?;
        if let Some(category_uuid) = category_id {
            if !state.categories.contains_key(&category_uuid) {
                return Err(UnifiedError::not_found(format!(
                    "Category \"{category_uuid}\" not found"
                )));
            }
        }
        let category_name = optional_text(&input.category_name, "category_name", MAX_SLUG_LEN)?;
        if let Some(name) = &category_name {
            if !settings.categories.iter().any(|entry| entry == name) {
                return Err(UnifiedError::bad_request(format!(
                    "category_name \"{name}\" is not in the configured categories"
                )));
            }
        }
        let now = Utc::now();
        let item = LibraryItem {
            id: Uuid::new_v4(),
            title,
            subtitle: optional_text(&input.subtitle, "subtitle", MAX_TITLE_LEN)?,
            authors,
            isbn: input.isbn.map(|raw| raw.trim().to_string()).filter(|value| !value.is_empty()),
            isbn_normalized,
            edition: optional_text(&input.edition, "edition", MAX_SLUG_LEN)?,
            series: optional_text(&input.series, "series", MAX_TITLE_LEN)?,
            series_position,
            publisher: optional_text(&input.publisher, "publisher", MAX_TITLE_LEN)?,
            published_year,
            language,
            category_id,
            category_name,
            item_type,
            status: ITEM_ACTIVE.to_string(),
            description: optional_text(&input.description, "description", MAX_TEXT_LEN)?,
            cover_ref: optional_text(&input.cover_ref, "cover_ref", MAX_REF_LEN)?,
            digital_ref: optional_text(&input.digital_ref, "digital_ref", MAX_REF_LEN)?,
            document_ref: optional_text(&input.document_ref, "document_ref", MAX_REF_LEN)?,
            storage_ref: optional_text(&input.storage_ref, "storage_ref", MAX_REF_LEN)?,
            tags,
            company_id: input.company_id,
            created_at: now,
            updated_at: now,
        };
        state.items.insert(item.id, item.clone());
        state.emit(
            "library.item.created",
            json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "item_type": item.item_type,
                "isbn": item.isbn_normalized,
                "actor": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => created(item),
        Err(error) => err(error, debug),
    }
}

async fn get_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        load_item(&state, &path.into_inner())
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

async fn update_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut item = load_item(&state, &path.into_inner())?;
        if item.status == ITEM_WITHDRAWN {
            return Err(UnifiedError::conflict(
                "A withdrawn item cannot be updated; create a new edition instead",
            ));
        }
        let settings = state.settings();
        if let Some(title) = &input.title {
            item.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(subtitle) = &input.subtitle {
            item.subtitle = match subtitle {
                Some(value) => Some(require_text(value, "subtitle", MAX_TITLE_LEN)?),
                None => None,
            };
        }
        if let Some(authors) = &input.authors {
            if authors.len() > MAX_AUTHORS {
                return Err(UnifiedError::bad_request(format!(
                    "authors must have at most {MAX_AUTHORS} entries"
                )));
            }
            let mut checked = Vec::with_capacity(authors.len());
            for author in authors {
                checked.push(require_text(author, "authors", MAX_TITLE_LEN)?);
            }
            item.authors = checked;
        }
        if let Some(isbn) = &input.isbn {
            let normalized = match isbn {
                Some(raw) if !raw.trim().is_empty() => Some(normalize_isbn(raw)?),
                _ => None,
            };
            if let Some(value) = &normalized {
                let edition = item.edition.as_deref().unwrap_or("");
                let duplicate = state.items.iter().any(|entry| {
                    entry.value().id != item.id
                        && entry.value().isbn_normalized.as_deref() == Some(value.as_str())
                        && entry.value().edition.as_deref().unwrap_or("") == edition
                        && entry.value().status != ITEM_WITHDRAWN
                });
                if duplicate {
                    return Err(UnifiedError::conflict(format!(
                        "An active item already uses ISBN {value} for this edition"
                    )));
                }
            }
            item.isbn = isbn.clone().map(|raw| raw.trim().to_string()).filter(|value| !value.is_empty());
            item.isbn_normalized = normalized;
        }
        if let Some(edition) = &input.edition {
            item.edition = match edition {
                Some(value) => Some(require_text(value, "edition", MAX_SLUG_LEN)?),
                None => None,
            };
        }
        if let Some(series) = &input.series {
            item.series = match series {
                Some(value) => Some(require_text(value, "series", MAX_TITLE_LEN)?),
                None => None,
            };
        }
        if let Some(position) = input.series_position {
            item.series_position = match position {
                Some(value) => Some(require_non_negative(value, "series_position")?),
                None => None,
            };
        }
        if let Some(publisher) = &input.publisher {
            item.publisher = match publisher {
                Some(value) => Some(require_text(value, "publisher", MAX_TITLE_LEN)?),
                None => None,
            };
        }
        if let Some(year) = input.published_year {
            item.published_year = match year {
                Some(value) => {
                    let current = Utc::now().format("%Y").to_string().parse::<i64>().unwrap_or(0);
                    if value < 0 || value > current + 1 {
                        return Err(UnifiedError::bad_request(
                            "published_year must be a plausible year",
                        ));
                    }
                    Some(value)
                }
                None => None,
            };
        }
        if let Some(language) = &input.language {
            item.language = require_in_list(language, "language", &settings.languages)?;
        }
        if let Some(category_id) = &input.category_id {
            let parsed = match category_id {
                Some(raw) => Some(parse_uuid(raw, "category")?),
                None => None,
            };
            if let Some(uuid) = parsed {
                if !state.categories.contains_key(&uuid) {
                    return Err(UnifiedError::not_found(format!(
                        "Category \"{uuid}\" not found"
                    )));
                }
            }
            item.category_id = parsed;
        }
        if let Some(category_name) = &input.category_name {
            let checked = match category_name {
                Some(name) => Some(require_in_list(name, "category_name", &settings.categories)?),
                None => None,
            };
            item.category_name = checked;
        }
        if let Some(item_type) = &input.item_type {
            item.item_type = require_in_list(item_type, "item_type", &settings.item_types)?;
        }
        if let Some(description) = &input.description {
            item.description = match description {
                Some(value) => Some(require_text(value, "description", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(cover_ref) = &input.cover_ref {
            item.cover_ref = match cover_ref {
                Some(value) => Some(require_text(value, "cover_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(digital_ref) = &input.digital_ref {
            item.digital_ref = match digital_ref {
                Some(value) => Some(require_text(value, "digital_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(document_ref) = &input.document_ref {
            item.document_ref = match document_ref {
                Some(value) => Some(require_text(value, "document_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(storage_ref) = &input.storage_ref {
            item.storage_ref = match storage_ref {
                Some(value) => Some(require_text(value, "storage_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(tags) = &input.tags {
            if tags.len() > MAX_TAGS {
                return Err(UnifiedError::bad_request(format!(
                    "tags must have at most {MAX_TAGS} entries"
                )));
            }
            let mut checked = Vec::with_capacity(tags.len());
            for tag in tags {
                checked.push(require_text(tag, "tags", MAX_SLUG_LEN)?);
            }
            item.tags = checked;
        }
        item.updated_at = Utc::now();
        state.items.insert(item.id, item.clone());
        state.emit(
            "library.item.updated",
            json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "actor": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

async fn delete_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let item = load_item(&state, &path.into_inner())?;
        let has_copies = state
            .copies
            .iter()
            .any(|entry| entry.value().item_id == item.id);
        if has_copies {
            return Err(UnifiedError::conflict(
                "An item with copies cannot be deleted; withdraw it instead",
            ));
        }
        let has_loans = state
            .loans
            .iter()
            .any(|entry| entry.value().item_id == item.id);
        if has_loans {
            return Err(UnifiedError::conflict(
                "An item with loan history cannot be deleted; withdraw it instead",
            ));
        }
        state.items.remove(&item.id);
        state.emit(
            "library.item.updated",
            json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "deleted": true,
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "item_id": item.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn withdraw_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let mut item = load_item(&state, &path.into_inner())?;
        if item.status == ITEM_WITHDRAWN {
            return Err(UnifiedError::conflict("Item is already withdrawn"));
        }
        let on_loan = state.copies.iter().any(|entry| {
            entry.value().item_id == item.id && entry.value().status == COPY_ON_LOAN
        });
        if on_loan {
            return Err(UnifiedError::conflict(
                "An item with copies on loan cannot be withdrawn until the copies are returned",
            ));
        }
        item.status = ITEM_WITHDRAWN.to_string();
        item.updated_at = Utc::now();
        state.items.insert(item.id, item.clone());
        for mut entry in state.copies.iter_mut() {
            if entry.value().item_id == item.id && entry.value().status == COPY_AVAILABLE {
                entry.value_mut().status = COPY_WITHDRAWN.to_string();
                entry.value_mut().updated_at = Utc::now();
            }
        }
        state.emit(
            "library.item.withdrawn",
            json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "actor": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Copy handlers
// ---------------------------------------------------------------------------

async fn list_item_copies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let item = load_item(&state, &path.into_inner())?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut copies: Vec<LibraryCopy> = state
            .copies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|copy| copy.item_id == item.id)
            .collect();
        if let Some(status) = filters.get("status") {
            copies.retain(|copy| &copy.status == status);
        }
        if let Some(shelf) = filters.get("shelf") {
            copies.retain(|copy| copy.shelf.as_deref() == Some(shelf.as_str()));
        }
        if let Some(location) = filters.get("location") {
            copies.retain(|copy| copy.location.as_deref() == Some(location.as_str()));
        }
        copies.sort_by(|a, b| a.barcode.cmp(&b.barcode));
        Ok(paginate(copies, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_copy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateCopyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCopy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let item = load_item(&state, &path.into_inner())?;
        if item.status == ITEM_WITHDRAWN {
            return Err(UnifiedError::conflict(
                "Copies cannot be added to a withdrawn item",
            ));
        }
        let input = body.into_inner();
        let settings = state.settings();
        let barcode = require_text(&input.barcode, "barcode", MAX_CODE_LEN)?;
        let duplicate = state
            .copies
            .iter()
            .any(|entry| entry.value().barcode == barcode);
        if duplicate {
            return Err(UnifiedError::conflict(format!(
                "A copy already uses barcode \"{barcode}\""
            )));
        }
        let condition = match &input.condition {
            Some(value) => require_in_list(value, "condition", &settings.copy_conditions)?,
            None => "good".to_string(),
        };
        let now = Utc::now();
        let copy = LibraryCopy {
            id: Uuid::new_v4(),
            item_id: item.id,
            barcode,
            serial: optional_text(&input.serial, "serial", MAX_CODE_LEN)?,
            condition,
            location: optional_text(&input.location, "location", MAX_TITLE_LEN)?,
            shelf: optional_text(&input.shelf, "shelf", MAX_SLUG_LEN)?,
            status: COPY_AVAILABLE.to_string(),
            notes: optional_text(&input.notes, "notes", MAX_NOTE_LEN)?,
            company_id: input.company_id,
            created_at: now,
            updated_at: now,
        };
        state.copies.insert(copy.id, copy.clone());
        state.emit(
            "library.copy.created",
            json!({
                "copy_id": copy.id.to_string(),
                "item_id": item.id.to_string(),
                "barcode": copy.barcode,
                "actor": caller.actor,
            }),
        );
        Ok(copy)
    })();
    match result {
        Ok(copy) => created(copy),
        Err(error) => err(error, debug),
    }
}

async fn get_copy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCopy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        load_copy(&state, &path.into_inner())
    })();
    match result {
        Ok(copy) => ok(copy),
        Err(error) => err(error, debug),
    }
}

async fn update_copy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCopyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCopy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut copy = load_copy(&state, &path.into_inner())?;
        let settings = state.settings();
        if let Some(barcode) = &input.barcode {
            let checked = require_text(barcode, "barcode", MAX_CODE_LEN)?;
            let duplicate = state.copies.iter().any(|entry| {
                entry.value().id != copy.id && entry.value().barcode == checked
            });
            if duplicate {
                return Err(UnifiedError::conflict(format!(
                    "A copy already uses barcode \"{checked}\""
                )));
            }
            copy.barcode = checked;
        }
        if let Some(serial) = &input.serial {
            copy.serial = match serial {
                Some(value) => Some(require_text(value, "serial", MAX_CODE_LEN)?),
                None => None,
            };
        }
        if let Some(condition) = &input.condition {
            copy.condition = require_in_list(condition, "condition", &settings.copy_conditions)?;
        }
        if let Some(location) = &input.location {
            copy.location = match location {
                Some(value) => Some(require_text(value, "location", MAX_TITLE_LEN)?),
                None => None,
            };
        }
        if let Some(shelf) = &input.shelf {
            copy.shelf = match shelf {
                Some(value) => Some(require_text(value, "shelf", MAX_SLUG_LEN)?),
                None => None,
            };
        }
        if let Some(status) = &input.status {
            let checked = require_in_list(status, "status", &settings.copy_statuses)?;
            if copy.status == COPY_ON_LOAN && checked != COPY_ON_LOAN {
                return Err(UnifiedError::conflict(
                    "A copy on loan cannot change status directly; return the loan first",
                ));
            }
            if checked == COPY_ON_LOAN {
                return Err(UnifiedError::conflict(
                    "A copy cannot be set to on-loan directly; check it out through a loan",
                ));
            }
            copy.status = checked;
        }
        if let Some(notes) = &input.notes {
            copy.notes = match notes {
                Some(value) => Some(require_text(value, "notes", MAX_NOTE_LEN)?),
                None => None,
            };
        }
        copy.updated_at = Utc::now();
        state.copies.insert(copy.id, copy.clone());
        state.emit(
            "library.copy.updated",
            json!({
                "copy_id": copy.id.to_string(),
                "item_id": copy.item_id.to_string(),
                "status": copy.status,
                "actor": caller.actor,
            }),
        );
        Ok(copy)
    })();
    match result {
        Ok(copy) => ok(copy),
        Err(error) => err(error, debug),
    }
}

async fn delete_copy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let copy = load_copy(&state, &path.into_inner())?;
        if copy.status == COPY_ON_LOAN {
            return Err(UnifiedError::conflict(
                "A copy on loan cannot be deleted; return the loan first",
            ));
        }
        let has_loans = state
            .loans
            .iter()
            .any(|entry| entry.value().copy_id == Some(copy.id));
        if has_loans {
            return Err(UnifiedError::conflict(
                "A copy with loan history cannot be deleted; withdraw it instead",
            ));
        }
        state.copies.remove(&copy.id);
        state.emit(
            "library.copy.deleted",
            json!({
                "copy_id": copy.id.to_string(),
                "item_id": copy.item_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "copy_id": copy.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Catalog search handlers
// ---------------------------------------------------------------------------

async fn search_catalog(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        check_search_rate(&state, &caller.actor)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryItem> = state
            .items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.status == ITEM_ACTIVE)
            .collect();
        if let Some(search) = &pagination.search {
            if !search.trim().is_empty() {
                items.retain(|item| item_matches_query(item, search));
            }
        }
        if let Some(item_type) = filters.get("item_type") {
            items.retain(|item| &item.item_type == item_type);
        }
        if let Some(language) = filters.get("language") {
            items.retain(|item| &item.language == language);
        }
        if let Some(category_name) = filters.get("category_name") {
            items.retain(|item| {
                item.category_name
                    .as_deref()
                    .map(|name| name == category_name)
                    .unwrap_or(false)
            });
        }
        if let Some(author) = filters.get("author") {
            let needle = author.to_lowercase();
            items.retain(|item| {
                item.authors
                    .iter()
                    .any(|name| name.to_lowercase().contains(&needle))
            });
        }
        if let Some(isbn) = filters.get("isbn") {
            let needle = isbn.replace('-', "").replace(' ', "").to_uppercase();
            items.retain(|item| {
                item.isbn_normalized
                    .as_deref()
                    .map(|value| value.contains(&needle))
                    .unwrap_or(false)
            });
        }
        items.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
        // Availability summary per item, computed from live copies.
        let enriched: Vec<Value> = items
            .iter()
            .map(|item| {
                let copies: Vec<&LibraryCopy> = state
                    .copies
                    .iter()
                    .filter(|entry| entry.value().item_id == item.id)
                    .map(|entry| entry.value())
                    .collect();
                let available = copies
                    .iter()
                    .filter(|copy| copy.status == COPY_AVAILABLE)
                    .count();
                json!({
                    "item": item,
                    "total_copies": copies.len(),
                    "available_copies": available,
                    "on_loan_copies": copies.iter().filter(|copy| copy.status == COPY_ON_LOAN).count(),
                })
            })
            .collect();
        Ok(paginate(enriched, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_catalog_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let item = load_item(&state, &path.into_inner())?;
        if item.status != ITEM_ACTIVE {
            return Err(UnifiedError::not_found(format!(
                "Catalog item \"{}\" not found",
                item.id
            )));
        }
        let mut copies: Vec<LibraryCopy> = state
            .copies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|copy| copy.item_id == item.id)
            .collect();
        copies.sort_by(|a, b| a.barcode.cmp(&b.barcode));
        Ok(json!({ "item": item, "copies": copies }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Category handlers
// ---------------------------------------------------------------------------

async fn list_categories(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryCategory> = state
            .categories
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(parent_id) = filters.get("parent_id") {
            items.retain(|category| {
                category
                    .parent_id
                    .map(|id| id.to_string() == *parent_id)
                    .unwrap_or(false)
            });
        }
        if let Some(search) = &pagination.search {
            let needle = search.to_lowercase();
            items.retain(|category| {
                category.name.to_lowercase().contains(&needle)
                    || category.slug.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCategory> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_TITLE_LEN)?;
        let slug = match &input.slug {
            Some(raw) => {
                let value = slugify(raw);
                if value.is_empty() {
                    return Err(UnifiedError::bad_request("slug must not be empty"));
                }
                if value.len() > MAX_SLUG_LEN {
                    return Err(UnifiedError::bad_request(format!(
                        "slug must be at most {MAX_SLUG_LEN} characters"
                    )));
                }
                value
            }
            None => slugify(&name),
        };
        if state
            .categories
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "A category already uses slug \"{slug}\""
            )));
        }
        let parent_id = parse_optional_uuid(&input.parent_id, "category")?;
        if let Some(parent) = parent_id {
            if !state.categories.contains_key(&parent) {
                return Err(UnifiedError::not_found(format!(
                    "Parent category \"{parent}\" not found"
                )));
            }
        }
        let now = Utc::now();
        let category = LibraryCategory {
            id: Uuid::new_v4(),
            name,
            slug,
            description: optional_text(&input.description, "description", MAX_TEXT_LEN)?,
            parent_id,
            company_id: input.company_id,
            created_at: now,
            updated_at: now,
        };
        state.categories.insert(category.id, category.clone());
        Ok(category)
    })();
    match result {
        Ok(category) => created(category),
        Err(error) => err(error, debug),
    }
}

async fn get_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCategory> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        load_category(&state, &path.into_inner())
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

async fn update_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryCategory> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut category = load_category(&state, &path.into_inner())?;
        if let Some(name) = &input.name {
            category.name = require_text(name, "name", MAX_TITLE_LEN)?;
        }
        if let Some(slug) = &input.slug {
            let value = slugify(slug);
            if value.is_empty() {
                return Err(UnifiedError::bad_request("slug must not be empty"));
            }
            let duplicate = state.categories.iter().any(|entry| {
                entry.value().id != category.id && entry.value().slug == value
            });
            if duplicate {
                return Err(UnifiedError::conflict(format!(
                    "A category already uses slug \"{value}\""
                )));
            }
            category.slug = value;
        }
        if let Some(description) = &input.description {
            category.description = match description {
                Some(value) => Some(require_text(value, "description", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(parent_id) = &input.parent_id {
            let parsed = match parent_id {
                Some(raw) => Some(parse_uuid(raw, "category")?),
                None => None,
            };
            if let Some(parent) = parsed {
                if parent == category.id {
                    return Err(UnifiedError::bad_request(
                        "A category cannot be its own parent",
                    ));
                }
                if !state.categories.contains_key(&parent) {
                    return Err(UnifiedError::not_found(format!(
                        "Parent category \"{parent}\" not found"
                    )));
                }
            }
            category.parent_id = parsed;
        }
        category.updated_at = Utc::now();
        state.categories.insert(category.id, category.clone());
        Ok(category)
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

async fn delete_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_catalog(&ctx, &caller.role)?;
        let category = load_category(&state, &path.into_inner())?;
        let has_children = state
            .categories
            .iter()
            .any(|entry| entry.value().parent_id == Some(category.id));
        if has_children {
            return Err(UnifiedError::conflict(
                "A category with child categories cannot be deleted",
            ));
        }
        let in_use = state
            .items
            .iter()
            .any(|entry| entry.value().category_id == Some(category.id));
        if in_use {
            return Err(UnifiedError::conflict(
                "A category used by catalog items cannot be deleted",
            ));
        }
        state.categories.remove(&category.id);
        Ok(json!({ "deleted": true, "category_id": category.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Member handlers
// ---------------------------------------------------------------------------

async fn list_members(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryMember> = state
            .members
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(member_type) = filters.get("member_type") {
            items.retain(|member| &member.member_type == member_type);
        }
        if let Some(status) = filters.get("status") {
            items.retain(|member| &member.status == status);
        }
        if let Some(user_id) = filters.get("user_id") {
            items.retain(|member| &member.user_id == user_id);
        }
        items.sort_by_key(|member| member.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateMemberRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryMember> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let settings = state.settings();
        let user_id = require_text(&input.user_id, "user_id", MAX_REF_LEN)?;
        let member_type = require_in_list(&input.member_type, "member_type", &settings.member_types)?;
        let duplicate = state.members.iter().any(|entry| {
            entry.value().user_id == user_id && entry.value().status != MEMBER_EXPIRED
        });
        if duplicate {
            return Err(UnifiedError::conflict(format!(
                "User \"{user_id}\" already has an active library membership"
            )));
        }
        let expires_at = match &input.expires_at {
            Some(raw) => Some(parse_datetime(raw, "expires_at")?),
            None => None,
        };
        let now = Utc::now();
        let member = LibraryMember {
            id: Uuid::new_v4(),
            user_id,
            member_type,
            status: MEMBER_ACTIVE.to_string(),
            joined_at: now,
            expires_at,
            notes: optional_text(&input.notes, "notes", MAX_NOTE_LEN)?,
            company_id: input.company_id,
            created_at: now,
            updated_at: now,
        };
        state.members.insert(member.id, member.clone());
        state.emit(
            "library.member.created",
            json!({
                "member_id": member.id.to_string(),
                "user_id": member.user_id,
                "member_type": member.member_type,
                "actor": caller.actor,
            }),
        );
        Ok(member)
    })();
    match result {
        Ok(member) => created(member),
        Err(error) => err(error, debug),
    }
}

async fn get_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryMember> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let member = load_member(&state, &path.into_inner())?;
        if member.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        Ok(member)
    })();
    match result {
        Ok(member) => ok(member),
        Err(error) => err(error, debug),
    }
}

async fn update_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateMemberRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryMember> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut member = load_member(&state, &path.into_inner())?;
        let settings = state.settings();
        if let Some(member_type) = &input.member_type {
            member.member_type = require_in_list(member_type, "member_type", &settings.member_types)?;
        }
        if let Some(status) = &input.status {
            let checked = require_text(status, "status", MAX_SLUG_LEN)?;
            if !["active", "suspended", "expired"].contains(&checked.as_str()) {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, suspended, expired",
                ));
            }
            member.status = checked;
        }
        if let Some(expires_at) = &input.expires_at {
            member.expires_at = match expires_at {
                Some(raw) => Some(parse_datetime(raw, "expires_at")?),
                None => None,
            };
        }
        if let Some(notes) = &input.notes {
            member.notes = match notes {
                Some(value) => Some(require_text(value, "notes", MAX_NOTE_LEN)?),
                None => None,
            };
        }
        member.updated_at = Utc::now();
        state.members.insert(member.id, member.clone());
        let event = if member.status == MEMBER_SUSPENDED {
            "library.member.suspended"
        } else {
            "library.member.updated"
        };
        state.emit(
            event,
            json!({
                "member_id": member.id.to_string(),
                "user_id": member.user_id,
                "status": member.status,
                "actor": caller.actor,
            }),
        );
        Ok(member)
    })();
    match result {
        Ok(member) => ok(member),
        Err(error) => err(error, debug),
    }
}

async fn delete_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let member = load_member(&state, &path.into_inner())?;
        let active_loan = state.loans.iter().any(|entry| {
            entry.value().member_id == member.id
                && (entry.value().status == LOAN_ACTIVE || entry.value().status == LOAN_OVERDUE)
        });
        if active_loan {
            return Err(UnifiedError::conflict(
                "A member with active loans cannot be deleted; suspend the membership instead",
            ));
        }
        let pending_fine = state.fines.iter().any(|entry| {
            entry.value().member_id == member.id && entry.value().status == FINE_PENDING
        });
        if pending_fine {
            return Err(UnifiedError::conflict(
                "A member with pending fines cannot be deleted; resolve the fines first",
            ));
        }
        state.members.remove(&member.id);
        Ok(json!({ "deleted": true, "member_id": member.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_member_loans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let member = load_member(&state, &path.into_inner())?;
        if member.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| loan.member_id == member.id)
            .collect();
        if let Some(status) = filters.get("status") {
            items.retain(|loan| &loan.status == status);
        }
        items.sort_by_key(|loan| std::cmp::Reverse(loan.checked_out_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn member_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let member = load_member(&state, &path.into_inner())?;
        if member.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        let params = query.into_inner();
        let mut loans: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| loan.member_id == member.id)
            .collect();
        loans.sort_by_key(|loan| std::cmp::Reverse(loan.checked_out_at));
        let page = paginate(loans.clone(), &params);
        let total_loans = loans.len() as i64;
        let returned = loans
            .iter()
            .filter(|loan| loan.status == LOAN_RETURNED)
            .count() as i64;
        let overdue_now = loans
            .iter()
            .filter(|loan| loan.status == LOAN_OVERDUE)
            .count() as i64;
        let fines: Vec<LibraryFine> = state
            .fines
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|fine| fine.member_id == member.id)
            .collect();
        let pending_fine_minor: i64 = fines
            .iter()
            .filter(|fine| fine.status == FINE_PENDING)
            .map(|fine| fine.amount_minor)
            .sum();
        Ok(json!({
            "member": member,
            "loans": page,
            "total_loans": total_loans,
            "returned_loans": returned,
            "overdue_loans": overdue_now,
            "pending_fine_minor": pending_fine_minor,
            "fines": fines,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_my_loans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| loan.user_id == caller.actor)
            .collect();
        if let Some(status) = filters.get("status") {
            items.retain(|loan| &loan.status == status);
        }
        items.sort_by_key(|loan| std::cmp::Reverse(loan.checked_out_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Loan handlers
// ---------------------------------------------------------------------------

async fn create_loan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateLoanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryLoan> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let member = load_member(&state, &input.member_id)?;
        if member.status != MEMBER_ACTIVE {
            return Err(UnifiedError::conflict(format!(
                "Member \"{}\" is not active (status: {})",
                member.id, member.status
            )));
        }
        if let Some(expires_at) = member.expires_at {
            if expires_at <= Utc::now() {
                return Err(UnifiedError::conflict(
                    "The membership has expired; renew the membership first",
                ));
            }
        }
        let item = load_item(&state, &input.item_id)?;
        if item.status != ITEM_ACTIVE {
            return Err(UnifiedError::conflict("The item is withdrawn and cannot be loaned"));
        }
        let policy = resolve_policy(&state, &member.member_type, &item.item_type)?;
        let settings = state.settings();
        let now = Utc::now();
        if settings.block_checkout_when_overdue {
            let has_overdue = state.loans.iter().any(|entry| {
                entry.value().member_id == member.id && entry.value().status == LOAN_OVERDUE
            });
            if has_overdue {
                return Err(UnifiedError::conflict(
                    "The member has an overdue loan and cannot check out until it is resolved",
                ));
            }
        }
        let active_count = state
            .loans
            .iter()
            .filter(|entry| {
                entry.value().member_id == member.id
                    && (entry.value().status == LOAN_ACTIVE
                        || entry.value().status == LOAN_OVERDUE)
            })
            .count() as i64;
        if active_count >= policy.max_concurrent {
            return Err(UnifiedError::conflict(format!(
                "The member already has {} active loans, the policy maximum for this member type",
                policy.max_concurrent
            )));
        }
        let already_has_item = state.loans.iter().any(|entry| {
            entry.value().member_id == member.id
                && entry.value().item_id == item.id
                && (entry.value().status == LOAN_ACTIVE || entry.value().status == LOAN_OVERDUE)
        });
        if already_has_item {
            return Err(UnifiedError::conflict(
                "The member already has an active loan for this item",
            ));
        }
        let is_digital = item.digital_ref.is_some()
            || item.document_ref.is_some()
            || item.storage_ref.is_some();
        let copy_id = match &input.copy_id {
            Some(raw) => Some(parse_uuid(raw, "copy")?),
            None => None,
        };
        if let Some(copy_uuid) = copy_id {
            let copy = state
                .copies
                .get(&copy_uuid)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Copy \"{copy_uuid}\" not found")))?;
            if copy.item_id != item.id {
                return Err(UnifiedError::bad_request(
                    "The copy does not belong to the requested item",
                ));
            }
            if copy.status == COPY_ON_LOAN {
                return Err(UnifiedError::conflict(
                    "The copy is already on loan",
                ));
            }
            if copy.status == COPY_RESERVED {
                let held_for_member = state.holds.iter().any(|entry| {
                    entry.value().member_id == member.id
                        && entry.value().item_id == item.id
                        && entry.value().status == HOLD_READY
                });
                if !held_for_member {
                    return Err(UnifiedError::conflict(
                        "The copy is reserved for another member",
                    ));
                }
            }
            if copy.status != COPY_AVAILABLE && copy.status != COPY_RESERVED {
                return Err(UnifiedError::conflict(format!(
                    "The copy cannot be loaned (status: {})",
                    copy.status
                )));
            }
        } else if !is_digital {
            return Err(UnifiedError::bad_request(
                "copy_id is required for a physical item; digital items loan without a copy",
            ));
        }
        // A hold queue blocks checkout for anyone who is not next.
        let queued = first_queued_hold(&state, item.id);
        if let Some(hold) = queued {
            if hold.member_id != member.id {
                return Err(UnifiedError::conflict(
                    "The item has a hold queue; the next member in the queue checks out first",
                ));
            }
        }
        let due_at = now + chrono::Duration::days(policy.loan_days);
        let loan = LibraryLoan {
            id: Uuid::new_v4(),
            member_id: member.id,
            user_id: member.user_id.clone(),
            item_id: item.id,
            copy_id,
            status: LOAN_ACTIVE.to_string(),
            checked_out_at: now,
            due_at,
            returned_at: None,
            renewal_count: 0,
            last_reminded_at: None,
            last_overdue_noticed_at: None,
            company_id: input.company_id,
            created_at: now,
            updated_at: now,
        };
        state.loans.insert(loan.id, loan.clone());
        if let Some(copy_uuid) = copy_id {
            if let Some(mut entry) = state.copies.get_mut(&copy_uuid) {
                entry.value_mut().status = COPY_ON_LOAN.to_string();
                entry.value_mut().updated_at = now;
            }
        }
        // A ready hold for this member is consumed by the checkout.
        let ready_holds: Vec<Uuid> = state
            .holds
            .iter()
            .filter(|entry| {
                entry.value().member_id == member.id
                    && entry.value().item_id == item.id
                    && entry.value().status == HOLD_READY
            })
            .map(|entry| *entry.key())
            .collect();
        for hold_id in ready_holds {
            if let Some(mut entry) = state.holds.get_mut(&hold_id) {
                entry.value_mut().status = HOLD_FULFILLED.to_string();
                entry.value_mut().updated_at = now;
            }
        }
        state.emit(
            "library.loan.created",
            json!({
                "loan_id": loan.id.to_string(),
                "member_id": member.id.to_string(),
                "user_id": member.user_id,
                "item_id": item.id.to_string(),
                "copy_id": loan.copy_id.map(|id| id.to_string()),
                "due_at": due_at.to_rfc3339(),
                "actor": caller.actor,
            }),
        );
        Ok(loan)
    })();
    match result {
        Ok(loan) => created(loan),
        Err(error) => err(error, debug),
    }
}

async fn list_loans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let is_manager = check_capability(&ctx, &caller.role, PERM_LIBRARY_MANAGE)
            || check_capability(&ctx, &caller.role, PERM_LIBRARY_CIRCULATE);
        let mut items: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if !is_manager {
            items.retain(|loan| loan.user_id == caller.actor);
        }
        if let Some(member_id) = filters.get("member_id") {
            items.retain(|loan| loan.member_id.to_string() == *member_id);
        }
        if let Some(user_id) = filters.get("user_id") {
            items.retain(|loan| &loan.user_id == user_id);
        }
        if let Some(item_id) = filters.get("item_id") {
            items.retain(|loan| loan.item_id.to_string() == *item_id);
        }
        if let Some(copy_id) = filters.get("copy_id") {
            items.retain(|loan| {
                loan.copy_id
                    .map(|id| id.to_string() == *copy_id)
                    .unwrap_or(false)
            });
        }
        if let Some(status) = filters.get("status") {
            items.retain(|loan| &loan.status == status);
        }
        if let Some(due_before) = filters.get("due_before") {
            if let Ok(limit) = parse_datetime(due_before, "due_before") {
                items.retain(|loan| loan.due_at <= limit);
            }
        }
        items.sort_by_key(|loan| std::cmp::Reverse(loan.checked_out_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_loan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryLoan> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let loan = load_loan(&state, &path.into_inner())?;
        if loan.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        Ok(loan)
    })();
    match result {
        Ok(loan) => ok(loan),
        Err(error) => err(error, debug),
    }
}

async fn renew_loan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryLoan> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let mut loan = load_loan(&state, &path.into_inner())?;
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::conflict(format!(
                "Only an active loan can be renewed (status: {})",
                loan.status
            )));
        }
        let settings = state.settings();
        if loan.status == LOAN_OVERDUE && !settings.allow_renew_when_overdue {
            return Err(UnifiedError::conflict(
                "An overdue loan cannot be renewed; return it first",
            ));
        }
        let member = load_member(&state, &loan.member_id.to_string())?;
        let item = load_item(&state, &loan.item_id.to_string())?;
        let policy = resolve_policy(&state, &member.member_type, &item.item_type)?;
        if loan.renewal_count >= policy.max_renewals {
            return Err(UnifiedError::conflict(format!(
                "The loan already reached the renewal limit of {}",
                policy.max_renewals
            )));
        }
        // A hold queue blocks renewal: the item is wanted by others.
        if first_queued_hold(&state, item.id).is_some() {
            return Err(UnifiedError::conflict(
                "The loan cannot be renewed while another member holds the item",
            ));
        }
        let now = Utc::now();
        loan.due_at = loan.due_at + chrono::Duration::days(policy.loan_days);
        if loan.due_at < now {
            loan.due_at = now + chrono::Duration::days(policy.loan_days);
        }
        loan.renewal_count += 1;
        loan.status = LOAN_ACTIVE.to_string();
        loan.updated_at = now;
        state.loans.insert(loan.id, loan.clone());
        state.emit(
            "library.loan.renewed",
            json!({
                "loan_id": loan.id.to_string(),
                "member_id": loan.member_id.to_string(),
                "user_id": loan.user_id,
                "item_id": loan.item_id.to_string(),
                "due_at": loan.due_at.to_rfc3339(),
                "renewal_count": loan.renewal_count,
                "actor": caller.actor,
            }),
        );
        Ok(loan)
    })();
    match result {
        Ok(loan) => ok(loan),
        Err(error) => err(error, debug),
    }
}

async fn return_loan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let mut loan = load_loan(&state, &path.into_inner())?;
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::conflict(format!(
                "Only an active loan can be returned (status: {})",
                loan.status
            )));
        }
        let now = Utc::now();
        let was_overdue = loan.status == LOAN_OVERDUE || now > loan.due_at;
        let member = load_member(&state, &loan.member_id.to_string())?;
        let item = load_item(&state, &loan.item_id.to_string())?;
        let policy = resolve_policy(&state, &member.member_type, &item.item_type)?;
        loan.status = LOAN_RETURNED.to_string();
        loan.returned_at = Some(now);
        loan.updated_at = now;
        state.loans.insert(loan.id, loan.clone());
        if let Some(copy_uuid) = loan.copy_id {
            if let Some(mut entry) = state.copies.get_mut(&copy_uuid) {
                entry.value_mut().status = COPY_AVAILABLE.to_string();
                entry.value_mut().updated_at = now;
            }
        }
        // Revoke outstanding digital access for this loan.
        for mut entry in state.digital_access.iter_mut() {
            if entry.value().loan_id == loan.id && !entry.value().revoked {
                entry.value_mut().revoked = true;
                entry.value_mut().updated_at = now;
            }
        }
        let fine = if was_overdue {
            let amount = overdue_fine_minor(&policy, loan.due_at, now);
            create_fine_for_loan(
                &state,
                &loan,
                amount,
                &policy.currency,
                "overdue",
            )
        } else {
            None
        };
        state.emit(
            "library.loan.returned",
            json!({
                "loan_id": loan.id.to_string(),
                "member_id": loan.member_id.to_string(),
                "user_id": loan.user_id,
                "item_id": loan.item_id.to_string(),
                "copy_id": loan.copy_id.map(|id| id.to_string()),
                "was_overdue": was_overdue,
                "fine_id": fine.as_ref().map(|value| value.id.to_string()),
                "actor": caller.actor,
            }),
        );
        // Offer the item to the next hold in the FIFO queue.
        offer_to_next_hold(&state, &item);
        Ok(json!({ "loan": loan, "fine": fine }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn mark_loan_lost(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MarkLoanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut loan = load_loan(&state, &path.into_inner())?;
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::conflict(format!(
                "Only an active loan can be marked lost (status: {})",
                loan.status
            )));
        }
        let now = Utc::now();
        let member = load_member(&state, &loan.member_id.to_string())?;
        let item = load_item(&state, &loan.item_id.to_string())?;
        let policy = resolve_policy(&state, &member.member_type, &item.item_type)?;
        loan.status = LOAN_LOST.to_string();
        loan.updated_at = now;
        state.loans.insert(loan.id, loan.clone());
        if let Some(copy_uuid) = loan.copy_id {
            if let Some(mut entry) = state.copies.get_mut(&copy_uuid) {
                entry.value_mut().status = COPY_LOST.to_string();
                entry.value_mut().updated_at = now;
            }
        }
        let currency = match &input.currency {
            Some(raw) => require_currency(raw)?,
            None => policy.currency.clone(),
        };
        let fine = match input.replacement_amount_minor {
            Some(amount) => {
                let checked = require_non_negative(amount, "replacement_amount_minor")?;
                create_fine_for_loan(&state, &loan, checked, &currency, "lost_replacement")
            }
            None => None,
        };
        state.emit(
            "library.loan.marked_lost",
            json!({
                "loan_id": loan.id.to_string(),
                "member_id": loan.member_id.to_string(),
                "user_id": loan.user_id,
                "item_id": loan.item_id.to_string(),
                "copy_id": loan.copy_id.map(|id| id.to_string()),
                "reason": input.reason,
                "fine_id": fine.as_ref().map(|value| value.id.to_string()),
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "loan": loan, "fine": fine }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn mark_loan_damaged(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MarkLoanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut loan = load_loan(&state, &path.into_inner())?;
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::conflict(format!(
                "Only an active loan can be marked damaged (status: {})",
                loan.status
            )));
        }
        let now = Utc::now();
        let member = load_member(&state, &loan.member_id.to_string())?;
        let item = load_item(&state, &loan.item_id.to_string())?;
        let policy = resolve_policy(&state, &member.member_type, &item.item_type)?;
        loan.status = LOAN_DAMAGED.to_string();
        loan.returned_at = Some(now);
        loan.updated_at = now;
        state.loans.insert(loan.id, loan.clone());
        if let Some(copy_uuid) = loan.copy_id {
            if let Some(mut entry) = state.copies.get_mut(&copy_uuid) {
                entry.value_mut().status = COPY_DAMAGED.to_string();
                entry.value_mut().condition = "damaged".to_string();
                entry.value_mut().updated_at = now;
            }
        }
        let currency = match &input.currency {
            Some(raw) => require_currency(raw)?,
            None => policy.currency.clone(),
        };
        let fine = match input.replacement_amount_minor {
            Some(amount) => {
                let checked = require_non_negative(amount, "replacement_amount_minor")?;
                create_fine_for_loan(&state, &loan, checked, &currency, "damaged_replacement")
            }
            None => None,
        };
        state.emit(
            "library.loan.marked_damaged",
            json!({
                "loan_id": loan.id.to_string(),
                "member_id": loan.member_id.to_string(),
                "user_id": loan.user_id,
                "item_id": loan.item_id.to_string(),
                "copy_id": loan.copy_id.map(|id| id.to_string()),
                "reason": input.reason,
                "fine_id": fine.as_ref().map(|value| value.id.to_string()),
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "loan": loan, "fine": fine }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_overdue_loans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let params = query.into_inner();
        let now = Utc::now();
        let mut items: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| {
                loan.status == LOAN_OVERDUE
                    || ((loan.status == LOAN_ACTIVE) && loan.due_at < now)
            })
            .collect();
        items.sort_by_key(|loan| loan.due_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn scan_overdue_loans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let now = Utc::now();
        let mut marked = 0i64;
        let mut fines_raised = 0i64;
        let mut notified = 0i64;
        let candidates: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| loan.status == LOAN_ACTIVE && loan.due_at < now)
            .collect();
        for mut loan in candidates {
            let member = match state.members.get(&loan.member_id) {
                Some(entry) => entry.value().clone(),
                None => continue,
            };
            let item = match state.items.get(&loan.item_id) {
                Some(entry) => entry.value().clone(),
                None => continue,
            };
            let policy = match resolve_policy(&state, &member.member_type, &item.item_type) {
                Ok(value) => value,
                Err(_) => continue,
            };
            loan.status = LOAN_OVERDUE.to_string();
            loan.last_overdue_noticed_at = Some(now);
            loan.updated_at = now;
            state.loans.insert(loan.id, loan.clone());
            marked += 1;
            let amount = overdue_fine_minor(&policy, loan.due_at, now);
            if create_fine_for_loan(&state, &loan, amount, &policy.currency, "overdue").is_some() {
                fines_raised += 1;
            }
            state.emit(
                "library.loan.overdue",
                json!({
                    "loan_id": loan.id.to_string(),
                    "member_id": loan.member_id.to_string(),
                    "user_id": loan.user_id,
                    "item_id": loan.item_id.to_string(),
                    "due_at": loan.due_at.to_rfc3339(),
                    "actor": caller.actor,
                }),
            );
            state.notify(
                "library.overdue",
                &loan.user_id,
                "Overdue library loan",
                &format!(
                    "The item \"{}\" was due on {} and is now overdue.",
                    item.title,
                    loan.due_at.format("%Y-%m-%d")
                ),
                Some(format!("/library/loans/{}", loan.id)),
            );
            notified += 1;
        }
        Ok(json!({
            "marked_overdue": marked,
            "fines_raised": fines_raised,
            "members_notified": notified,
            "scanned_at": now.to_rfc3339(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Hold handlers
// ---------------------------------------------------------------------------

async fn create_hold(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateHoldRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryHold> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let input = body.into_inner();
        let member = load_member(&state, &input.member_id)?;
        if member.status != MEMBER_ACTIVE {
            return Err(UnifiedError::conflict(format!(
                "Member \"{}\" is not active (status: {})",
                member.id, member.status
            )));
        }
        let item = load_item(&state, &input.item_id)?;
        if item.status != ITEM_ACTIVE {
            return Err(UnifiedError::conflict("The item is withdrawn and cannot be reserved"));
        }
        let duplicate = state.holds.iter().any(|entry| {
            entry.value().member_id == member.id
                && entry.value().item_id == item.id
                && (entry.value().status == HOLD_QUEUED || entry.value().status == HOLD_READY)
        });
        if duplicate {
            return Err(UnifiedError::conflict(
                "The member already has an active hold for this item",
            ));
        }
        let already_has_item = state.loans.iter().any(|entry| {
            entry.value().member_id == member.id
                && entry.value().item_id == item.id
                && (entry.value().status == LOAN_ACTIVE || entry.value().status == LOAN_OVERDUE)
        });
        if already_has_item {
            return Err(UnifiedError::conflict(
                "The member already has this item on loan; a hold is not needed",
            ));
        }
        let available = state.copies.iter().any(|entry| {
            entry.value().item_id == item.id && entry.value().status == COPY_AVAILABLE
        });
        let is_digital = item.digital_ref.is_some()
            || item.document_ref.is_some()
            || item.storage_ref.is_some();
        if available && !is_digital {
            return Err(UnifiedError::conflict(
                "A copy is available; check the item out instead of reserving it",
            ));
        }
        let now = Utc::now();
        let hold = LibraryHold {
            id: Uuid::new_v4(),
            member_id: member.id,
            user_id: member.user_id.clone(),
            item_id: item.id,
            copy_id: None,
            status: HOLD_QUEUED.to_string(),
            position: next_hold_position(&state, item.id),
            ready_at: None,
            expires_at: None,
            company_id: input.company_id,
            created_at: now,
            updated_at: now,
        };
        state.holds.insert(hold.id, hold.clone());
        state.emit(
            "library.hold.created",
            json!({
                "hold_id": hold.id.to_string(),
                "member_id": member.id.to_string(),
                "user_id": member.user_id,
                "item_id": item.id.to_string(),
                "position": hold.position,
                "actor": caller.actor,
            }),
        );
        Ok(hold)
    })();
    match result {
        Ok(hold) => created(hold),
        Err(error) => err(error, debug),
    }
}

async fn list_holds(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let is_manager = check_capability(&ctx, &caller.role, PERM_LIBRARY_MANAGE)
            || check_capability(&ctx, &caller.role, PERM_LIBRARY_CIRCULATE);
        let mut items: Vec<LibraryHold> = state
            .holds
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if !is_manager {
            items.retain(|hold| hold.user_id == caller.actor);
        }
        if let Some(member_id) = filters.get("member_id") {
            items.retain(|hold| hold.member_id.to_string() == *member_id);
        }
        if let Some(item_id) = filters.get("item_id") {
            items.retain(|hold| hold.item_id.to_string() == *item_id);
        }
        if let Some(status) = filters.get("status") {
            items.retain(|hold| &hold.status == status);
        }
        items.sort_by_key(|hold| (hold.item_id, hold.position));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_hold(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryHold> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let hold = load_hold(&state, &path.into_inner())?;
        if hold.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        Ok(hold)
    })();
    match result {
        Ok(hold) => ok(hold),
        Err(error) => err(error, debug),
    }
}

async fn cancel_hold(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryHold> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let mut hold = load_hold(&state, &path.into_inner())?;
        if hold.user_id != caller.actor {
            require_circulate(&ctx, &caller.role)?;
        }
        if hold.status != HOLD_QUEUED && hold.status != HOLD_READY {
            return Err(UnifiedError::conflict(format!(
                "Only a queued or ready hold can be cancelled (status: {})",
                hold.status
            )));
        }
        hold.status = HOLD_CANCELLED.to_string();
        hold.updated_at = Utc::now();
        state.holds.insert(hold.id, hold.clone());
        state.emit(
            "library.hold.cancelled",
            json!({
                "hold_id": hold.id.to_string(),
                "member_id": hold.member_id.to_string(),
                "user_id": hold.user_id,
                "item_id": hold.item_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(hold)
    })();
    match result {
        Ok(hold) => ok(hold),
        Err(error) => err(error, debug),
    }
}

async fn fulfill_hold(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryHold> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let mut hold = load_hold(&state, &path.into_inner())?;
        if hold.status != HOLD_QUEUED {
            return Err(UnifiedError::conflict(format!(
                "Only a queued hold can be marked ready (status: {})",
                hold.status
            )));
        }
        let item = load_item(&state, &hold.item_id.to_string())?;
        let settings = state.settings();
        let now = Utc::now();
        hold.status = HOLD_READY.to_string();
        hold.ready_at = Some(now);
        hold.expires_at = Some(now + chrono::Duration::days(settings.hold_expiry_days));
        hold.updated_at = now;
        state.holds.insert(hold.id, hold.clone());
        state.emit(
            "library.hold.fulfilled",
            json!({
                "hold_id": hold.id.to_string(),
                "member_id": hold.member_id.to_string(),
                "user_id": hold.user_id,
                "item_id": hold.item_id.to_string(),
                "ready_at": now.to_rfc3339(),
                "expires_at": hold.expires_at.map(|value| value.to_rfc3339()),
                "actor": caller.actor,
            }),
        );
        state.notify(
            "library.hold_ready",
            &hold.user_id,
            "Reserved item is ready",
            &format!(
                "The item \"{}\" you reserved is ready for pickup.",
                item.title
            ),
            Some(format!("/library/items/{}", item.id)),
        );
        Ok(hold)
    })();
    match result {
        Ok(hold) => ok(hold),
        Err(error) => err(error, debug),
    }
}

async fn expire_hold(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryHold> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let mut hold = load_hold(&state, &path.into_inner())?;
        if hold.status != HOLD_READY && hold.status != HOLD_QUEUED {
            return Err(UnifiedError::conflict(format!(
                "Only a ready or queued hold can expire (status: {})",
                hold.status
            )));
        }
        let item = load_item(&state, &hold.item_id.to_string())?;
        hold.status = HOLD_EXPIRED.to_string();
        hold.updated_at = Utc::now();
        state.holds.insert(hold.id, hold.clone());
        state.emit(
            "library.hold.expired",
            json!({
                "hold_id": hold.id.to_string(),
                "member_id": hold.member_id.to_string(),
                "user_id": hold.user_id,
                "item_id": hold.item_id.to_string(),
                "actor": caller.actor,
            }),
        );
        // Offer the item to the next hold in the FIFO queue.
        offer_to_next_hold(&state, &item);
        Ok(hold)
    })();
    match result {
        Ok(hold) => ok(hold),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Fine handlers
// ---------------------------------------------------------------------------

async fn list_fines(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let is_manager = check_capability(&ctx, &caller.role, PERM_LIBRARY_MANAGE)
            || check_capability(&ctx, &caller.role, PERM_LIBRARY_CIRCULATE);
        let mut items: Vec<LibraryFine> = state
            .fines
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if !is_manager {
            items.retain(|fine| fine.user_id == caller.actor);
        }
        if let Some(member_id) = filters.get("member_id") {
            items.retain(|fine| fine.member_id.to_string() == *member_id);
        }
        if let Some(loan_id) = filters.get("loan_id") {
            items.retain(|fine| fine.loan_id.to_string() == *loan_id);
        }
        if let Some(status) = filters.get("status") {
            items.retain(|fine| &fine.status == status);
        }
        if let Some(reason) = filters.get("reason") {
            items.retain(|fine| &fine.reason == reason);
        }
        items.sort_by_key(|fine| std::cmp::Reverse(fine.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_fine(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryFine> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let fine = load_fine(&state, &path.into_inner())?;
        if fine.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        Ok(fine)
    })();
    match result {
        Ok(fine) => ok(fine),
        Err(error) => err(error, debug),
    }
}

async fn waive_fine(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibraryFine> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let mut fine = load_fine(&state, &path.into_inner())?;
        if fine.status != FINE_PENDING {
            return Err(UnifiedError::conflict(format!(
                "Only a pending fine can be waived (status: {})",
                fine.status
            )));
        }
        fine.status = FINE_WAIVED.to_string();
        fine.waived_by = Some(caller.actor.clone());
        fine.updated_at = Utc::now();
        state.fines.insert(fine.id, fine.clone());
        state.emit(
            "library.fine.waived",
            json!({
                "fine_id": fine.id.to_string(),
                "loan_id": fine.loan_id.to_string(),
                "member_id": fine.member_id.to_string(),
                "user_id": fine.user_id,
                "amount_minor": fine.amount_minor,
                "currency": fine.currency,
                "actor": caller.actor,
            }),
        );
        Ok(fine)
    })();
    match result {
        Ok(fine) => ok(fine),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Loan policy handlers
// ---------------------------------------------------------------------------

async fn list_policies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<LoanPolicy> = state
            .policies
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(member_type) = filters.get("member_type") {
            items.retain(|policy| &policy.member_type == member_type);
        }
        if let Some(item_type) = filters.get("item_type") {
            items.retain(|policy| &policy.item_type == item_type);
        }
        if let Some(active) = filters.get("active") {
            let wanted = active == "true";
            items.retain(|policy| policy.active == wanted);
        }
        items.sort_by_key(|policy| policy.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreatePolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoanPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let name = require_text(&input.name, "name", MAX_TITLE_LEN)?;
        let member_type = if input.member_type == "default" {
            "default".to_string()
        } else {
            require_in_list(&input.member_type, "member_type", &settings.member_types)?
        };
        let item_type = if input.item_type == "all" {
            "all".to_string()
        } else {
            require_in_list(&input.item_type, "item_type", &settings.item_types)?
        };
        let currency = match &input.currency {
            Some(raw) => require_currency(raw)?,
            None => settings.default_currency.clone(),
        };
        let duplicate = state.policies.iter().any(|entry| {
            entry.value().member_type == member_type
                && entry.value().item_type == item_type
                && entry.value().active
        });
        if duplicate {
            return Err(UnifiedError::conflict(format!(
                "An active policy already covers member type \"{member_type}\" and item type \"{item_type}\""
            )));
        }
        let now = Utc::now();
        let policy = LoanPolicy {
            id: Uuid::new_v4(),
            name,
            member_type,
            item_type,
            loan_days: require_positive(input.loan_days, "loan_days")?,
            max_concurrent: require_positive(input.max_concurrent, "max_concurrent")?,
            max_renewals: require_non_negative(input.max_renewals, "max_renewals")?,
            fine_per_day_minor: require_non_negative(
                input.fine_per_day_minor,
                "fine_per_day_minor",
            )?,
            currency,
            grace_days: require_non_negative(input.grace_days, "grace_days")?,
            active: true,
            company_id: input.company_id,
            created_at: now,
            updated_at: now,
        };
        state.policies.insert(policy.id, policy.clone());
        state.emit(
            "library.policy.created",
            json!({
                "policy_id": policy.id.to_string(),
                "member_type": policy.member_type,
                "item_type": policy.item_type,
                "actor": caller.actor,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}

async fn get_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoanPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        load_policy(&state, &path.into_inner())
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn update_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LoanPolicy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let input = body.into_inner();
        let mut policy = load_policy(&state, &path.into_inner())?;
        let settings = state.settings();
        if let Some(name) = &input.name {
            policy.name = require_text(name, "name", MAX_TITLE_LEN)?;
        }
        if let Some(member_type) = &input.member_type {
            policy.member_type = if member_type == "default" {
                "default".to_string()
            } else {
                require_in_list(member_type, "member_type", &settings.member_types)?
            };
        }
        if let Some(item_type) = &input.item_type {
            policy.item_type = if item_type == "all" {
                "all".to_string()
            } else {
                require_in_list(item_type, "item_type", &settings.item_types)?
            };
        }
        if let Some(loan_days) = input.loan_days {
            policy.loan_days = require_positive(loan_days, "loan_days")?;
        }
        if let Some(max_concurrent) = input.max_concurrent {
            policy.max_concurrent = require_positive(max_concurrent, "max_concurrent")?;
        }
        if let Some(max_renewals) = input.max_renewals {
            policy.max_renewals = require_non_negative(max_renewals, "max_renewals")?;
        }
        if let Some(fine) = input.fine_per_day_minor {
            policy.fine_per_day_minor = require_non_negative(fine, "fine_per_day_minor")?;
        }
        if let Some(currency) = &input.currency {
            policy.currency = require_currency(currency)?;
        }
        if let Some(grace_days) = input.grace_days {
            policy.grace_days = require_non_negative(grace_days, "grace_days")?;
        }
        if let Some(active) = input.active {
            policy.active = active;
        }
        if policy.active {
            let duplicate = state.policies.iter().any(|entry| {
                entry.value().id != policy.id
                    && entry.value().member_type == policy.member_type
                    && entry.value().item_type == policy.item_type
                    && entry.value().active
            });
            if duplicate {
                return Err(UnifiedError::conflict(format!(
                    "An active policy already covers member type \"{}\" and item type \"{}\"",
                    policy.member_type, policy.item_type
                )));
            }
        }
        policy.updated_at = Utc::now();
        state.policies.insert(policy.id, policy.clone());
        state.emit(
            "library.policy.updated",
            json!({
                "policy_id": policy.id.to_string(),
                "member_type": policy.member_type,
                "item_type": policy.item_type,
                "actor": caller.actor,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn delete_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let policy = load_policy(&state, &path.into_inner())?;
        state.policies.remove(&policy.id);
        state.emit(
            "library.policy.deleted",
            json!({
                "policy_id": policy.id.to_string(),
                "member_type": policy.member_type,
                "item_type": policy.item_type,
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "policy_id": policy.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Digital access handlers
// ---------------------------------------------------------------------------

async fn issue_digital_access(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DigitalAccessRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let item = load_item(&state, &path.into_inner())?;
        let input = body.into_inner();
        let loan = load_loan(&state, &input.loan_id)?;
        if loan.item_id != item.id {
            return Err(UnifiedError::bad_request(
                "The loan does not belong to the requested item",
            ));
        }
        if loan.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::conflict(format!(
                "Digital access needs an active loan (status: {})",
                loan.status
            )));
        }
        if item.digital_ref.is_none()
            && item.document_ref.is_none()
            && item.storage_ref.is_none()
        {
            return Err(UnifiedError::conflict(
                "The item has no digital reference; digital access is not available",
            ));
        }
        let settings = state.settings();
        let ttl = match input.expires_secs {
            Some(value) => {
                if value == 0 || value > settings.digital_link_ttl_secs * 24 {
                    return Err(UnifiedError::bad_request(
                        "expires_secs is outside the allowed range for digital access",
                    ));
                }
                value
            }
            None => settings.digital_link_ttl_secs,
        };
        let (token, expires_at) = state.issue_signed_token(&loan.id.to_string(), ttl);
        let now = Utc::now();
        let grant = DigitalAccess {
            id: Uuid::new_v4(),
            loan_id: loan.id,
            item_id: item.id,
            member_id: loan.member_id,
            user_id: loan.user_id.clone(),
            expires_at,
            revoked: false,
            redeemed_at: None,
            company_id: loan.company_id.clone(),
            created_at: now,
            updated_at: now,
        };
        state.digital_access.insert(grant.id, grant.clone());
        state.emit(
            "library.digital_access.issued",
            json!({
                "access_id": grant.id.to_string(),
                "loan_id": loan.id.to_string(),
                "item_id": item.id.to_string(),
                "user_id": loan.user_id,
                "expires_at": expires_at.to_rfc3339(),
                "actor": caller.actor,
            }),
        );
        Ok(json!({
            "access_id": grant.id.to_string(),
            "token": token,
            "expires_at": expires_at.to_rfc3339(),
            "item_id": item.id.to_string(),
            "loan_id": loan.id.to_string(),
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn redeem_digital_access(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let token = path.into_inner();
        let loan_id = state.verify_signed_token(&token)?;
        let loan_uuid = parse_uuid(&loan_id, "loan")?;
        let loan = state
            .loans
            .get(&loan_uuid)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Loan for this access token not found"))?;
        if loan.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        }
        if loan.status != LOAN_ACTIVE && loan.status != LOAN_OVERDUE {
            return Err(UnifiedError::forbidden(
                "Digital access is no longer valid for this loan",
            ));
        }
        let grant = state
            .digital_access
            .iter()
            .map(|entry| entry.value().clone())
            .find(|entry| entry.loan_id == loan_uuid && !entry.revoked)
            .ok_or_else(|| UnifiedError::forbidden("Digital access has been revoked"))?;
        if grant.expires_at <= Utc::now() {
            return Err(UnifiedError::forbidden("Digital access has expired"));
        }
        let item = load_item(&state, &loan.item_id.to_string())?;
        let now = Utc::now();
        if let Some(mut entry) = state.digital_access.get_mut(&grant.id) {
            entry.value_mut().redeemed_at = Some(now);
            entry.value_mut().updated_at = now;
        }
        state.emit(
            "library.digital_access.redeemed",
            json!({
                "access_id": grant.id.to_string(),
                "loan_id": loan.id.to_string(),
                "item_id": item.id.to_string(),
                "user_id": loan.user_id,
                "actor": caller.actor,
            }),
        );
        // References only: the caller hands these to the documents /
        // storage modules; no bytes are ever returned here.
        Ok(json!({
            "item_id": item.id.to_string(),
            "title": item.title,
            "digital_ref": item.digital_ref,
            "document_ref": item.document_ref,
            "storage_ref": item.storage_ref,
            "expires_at": grant.expires_at.to_rfc3339(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_item_digital(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let item = load_item(&state, &path.into_inner())?;
        Ok(json!({
            "item_id": item.id.to_string(),
            "title": item.title,
            "digital_ref": item.digital_ref,
            "document_ref": item.document_ref,
            "storage_ref": item.storage_ref,
            "has_digital": item.digital_ref.is_some() || item.document_ref.is_some() || item.storage_ref.is_some(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Inventory audit handlers
// ---------------------------------------------------------------------------

async fn create_audit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<CreateAuditRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InventoryAudit> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_TITLE_LEN)?;
        let now = Utc::now();
        let audit = InventoryAudit {
            id: Uuid::new_v4(),
            name,
            scope_location: optional_text(&input.scope_location, "scope_location", MAX_TITLE_LEN)?,
            scope_shelf: optional_text(&input.scope_shelf, "scope_shelf", MAX_SLUG_LEN)?,
            status: AUDIT_OPEN.to_string(),
            started_by: caller.actor.clone(),
            completed_at: None,
            items_checked: 0,
            discrepancies_found: 0,
            company_id: input.company_id,
            created_at: now,
            updated_at: now,
        };
        state.audits.insert(audit.id, audit.clone());
        Ok(audit)
    })();
    match result {
        Ok(audit) => created(audit),
        Err(error) => err(error, debug),
    }
}

async fn list_audits(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let params = query.into_inner();
        let pagination = params.normalized();
        let filters = pagination.filters.clone();
        let mut items: Vec<InventoryAudit> = state
            .audits
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(status) = filters.get("status") {
            items.retain(|audit| &audit.status == status);
        }
        items.sort_by_key(|audit| std::cmp::Reverse(audit.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_audit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InventoryAudit> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        load_audit(&state, &path.into_inner())
    })();
    match result {
        Ok(audit) => ok(audit),
        Err(error) => err(error, debug),
    }
}

async fn complete_audit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let mut audit = load_audit(&state, &path.into_inner())?;
        if audit.status != AUDIT_OPEN {
            return Err(UnifiedError::conflict(format!(
                "Only an open audit can be completed (status: {})",
                audit.status
            )));
        }
        let discrepancies: Vec<InventoryDiscrepancy> = state
            .discrepancies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.audit_id == audit.id)
            .collect();
        let unresolved = discrepancies
            .iter()
            .filter(|entry| entry.status == DISCREPANCY_PENDING)
            .count() as i64;
        if unresolved > 0 {
            return Err(UnifiedError::conflict(format!(
                "The audit has {unresolved} unresolved discrepancies; resolve them first"
            )));
        }
        let now = Utc::now();
        audit.status = AUDIT_COMPLETED.to_string();
        audit.completed_at = Some(now);
        audit.discrepancies_found = discrepancies.len() as i64;
        audit.updated_at = now;
        state.audits.insert(audit.id, audit.clone());
        state.emit(
            "library.inventory.completed",
            json!({
                "audit_id": audit.id.to_string(),
                "name": audit.name,
                "scope_location": audit.scope_location,
                "scope_shelf": audit.scope_shelf,
                "items_checked": audit.items_checked,
                "discrepancies_found": audit.discrepancies_found,
                "discrepancies": discrepancies.iter().map(|entry| json!({
                    "discrepancy_id": entry.id.to_string(),
                    "copy_id": entry.copy_id.to_string(),
                    "kind": entry.kind,
                    "status": entry.status,
                })).collect::<Vec<Value>>(),
                "actor": caller.actor,
            }),
        );
        Ok(json!({ "audit": audit, "discrepancies": discrepancies }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_discrepancy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateDiscrepancyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InventoryDiscrepancy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let audit = load_audit(&state, &path.into_inner())?;
        if audit.status != AUDIT_OPEN {
            return Err(UnifiedError::conflict(
                "Discrepancies can only be recorded on an open audit",
            ));
        }
        let input = body.into_inner();
        let settings = state.settings();
        let kind = require_in_list(&input.kind, "kind", &settings.discrepancy_kinds)?;
        let copy = load_copy(&state, &input.copy_id)?;
        if let Some(scope) = &audit.scope_location {
            if copy.location.as_deref() != Some(scope.as_str()) {
                return Err(UnifiedError::bad_request(
                    "The copy is outside the audit scope location",
                ));
            }
        }
        if let Some(shelf) = &audit.scope_shelf {
            if copy.shelf.as_deref() != Some(shelf.as_str()) {
                return Err(UnifiedError::bad_request(
                    "The copy is outside the audit scope shelf",
                ));
            }
        }
        let now = Utc::now();
        let discrepancy = InventoryDiscrepancy {
            id: Uuid::new_v4(),
            audit_id: audit.id,
            copy_id: copy.id,
            item_id: copy.item_id,
            kind: kind.clone(),
            note: optional_text(&input.note, "note", MAX_NOTE_LEN)?,
            status: DISCREPANCY_PENDING.to_string(),
            resolved_by: None,
            company_id: audit.company_id.clone(),
            created_at: now,
            updated_at: now,
        };
        state.discrepancies.insert(discrepancy.id, discrepancy.clone());
        let mut updated_audit = audit.clone();
        updated_audit.items_checked += 1;
        updated_audit.discrepancies_found += 1;
        updated_audit.updated_at = now;
        state.audits.insert(updated_audit.id, updated_audit);
        if kind == DISCREPANCY_MISSING {
            if let Some(mut entry) = state.copies.get_mut(&copy.id) {
                entry.value_mut().status = COPY_LOST.to_string();
                entry.value_mut().updated_at = now;
            }
        }
        if kind == DISCREPANCY_DAMAGED {
            if let Some(mut entry) = state.copies.get_mut(&copy.id) {
                entry.value_mut().status = COPY_DAMAGED.to_string();
                entry.value_mut().condition = "damaged".to_string();
                entry.value_mut().updated_at = now;
            }
        }
        state.emit(
            "library.inventory.discrepancy.recorded",
            json!({
                "discrepancy_id": discrepancy.id.to_string(),
                "audit_id": audit.id.to_string(),
                "copy_id": copy.id.to_string(),
                "item_id": copy.item_id.to_string(),
                "kind": kind,
                "actor": caller.actor,
            }),
        );
        Ok(discrepancy)
    })();
    match result {
        Ok(discrepancy) => created(discrepancy),
        Err(error) => err(error, debug),
    }
}

async fn resolve_discrepancy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<ResolveDiscrepancyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InventoryDiscrepancy> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let (audit_id, discrepancy_id) = path.into_inner();
        let audit = load_audit(&state, &audit_id)?;
        let input = body.into_inner();
        let resolution = require_text(&input.resolution, "resolution", MAX_SLUG_LEN)?;
        if ![DISCREPANCY_RESOLVED, DISCREPANCY_WRITTEN_OFF].contains(&resolution.as_str()) {
            return Err(UnifiedError::bad_request(
                "resolution must be one of: resolved, written_off",
            ));
        }
        if resolution == DISCREPANCY_WRITTEN_OFF && input.approved != Some(true) {
            return Err(UnifiedError::bad_request(
                "A write-off needs explicit approval (approved: true)",
            ));
        }
        let uuid = parse_uuid(&discrepancy_id, "discrepancy")?;
        let mut discrepancy = state
            .discrepancies
            .get(&uuid)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Discrepancy \"{discrepancy_id}\" not found"))
            })?;
        if discrepancy.audit_id != audit.id {
            return Err(UnifiedError::not_found(format!(
                "Discrepancy \"{discrepancy_id}\" does not belong to this audit"
            )));
        }
        if discrepancy.status != DISCREPANCY_PENDING {
            return Err(UnifiedError::conflict(format!(
                "Only a pending discrepancy can be resolved (status: {})",
                discrepancy.status
            )));
        }
        let now = Utc::now();
        discrepancy.status = resolution.clone();
        discrepancy.resolved_by = Some(caller.actor.clone());
        if let Some(note) = &input.note {
            discrepancy.note = Some(require_text(note, "note", MAX_NOTE_LEN)?);
        }
        discrepancy.updated_at = now;
        state.discrepancies.insert(discrepancy.id, discrepancy.clone());
        if resolution == DISCREPANCY_WRITTEN_OFF {
            if let Some(mut entry) = state.copies.get_mut(&discrepancy.copy_id) {
                entry.value_mut().status = COPY_WITHDRAWN.to_string();
                entry.value_mut().updated_at = now;
            }
        }
        if resolution == DISCREPANCY_RESOLVED && discrepancy.kind == DISCREPANCY_FOUND {
            if let Some(mut entry) = state.copies.get_mut(&discrepancy.copy_id) {
                if entry.value().status == COPY_LOST {
                    entry.value_mut().status = COPY_AVAILABLE.to_string();
                    entry.value_mut().updated_at = now;
                }
            }
        }
        state.emit(
            "library.inventory.discrepancy.resolved",
            json!({
                "discrepancy_id": discrepancy.id.to_string(),
                "audit_id": audit.id.to_string(),
                "copy_id": discrepancy.copy_id.to_string(),
                "resolution": resolution,
                "actor": caller.actor,
            }),
        );
        Ok(discrepancy)
    })();
    match result {
        Ok(discrepancy) => ok(discrepancy),
        Err(error) => err(error, debug),
    }
}

async fn audit_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        let audit = load_audit(&state, &path.into_inner())?;
        let mut discrepancies: Vec<InventoryDiscrepancy> = state
            .discrepancies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.audit_id == audit.id)
            .collect();
        discrepancies.sort_by_key(|entry| entry.created_at);
        let by_kind: HashMap<String, i64> =
            discrepancies
                .iter()
                .fold(HashMap::new(), |mut acc, entry| {
                    *acc.entry(entry.kind.clone()).or_insert(0) += 1;
                    acc
                });
        let by_status: HashMap<String, i64> =
            discrepancies
                .iter()
                .fold(HashMap::new(), |mut acc, entry| {
                    *acc.entry(entry.status.clone()).or_insert(0) += 1;
                    acc
                });
        Ok(json!({
            "audit": audit,
            "discrepancies": discrepancies,
            "total_discrepancies": discrepancies.len(),
            "by_kind": by_kind,
            "by_status": by_status,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Reminder handlers
// ---------------------------------------------------------------------------

async fn send_due_soon_reminders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let settings = state.settings();
        let now = Utc::now();
        let window_end = now + chrono::Duration::days(settings.reminder_days_before_due);
        let mut notified = 0i64;
        let candidates: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| {
                loan.status == LOAN_ACTIVE
                    && loan.due_at > now
                    && loan.due_at <= window_end
                    && loan
                        .last_reminded_at
                        .map(|reminded| reminded < now - chrono::Duration::days(1))
                        .unwrap_or(true)
            })
            .collect();
        for mut loan in candidates {
            let item = match state.items.get(&loan.item_id) {
                Some(entry) => entry.value().clone(),
                None => continue,
            };
            state.notify(
                "library.due_soon",
                &loan.user_id,
                "Library loan due soon",
                &format!(
                    "The item \"{}\" is due on {}.",
                    item.title,
                    loan.due_at.format("%Y-%m-%d")
                ),
                Some(format!("/library/loans/{}", loan.id)),
            );
            loan.last_reminded_at = Some(now);
            loan.updated_at = now;
            state.loans.insert(loan.id, loan);
            notified += 1;
        }
        Ok(json!({
            "members_notified": notified,
            "window_days": settings.reminder_days_before_due,
            "sent_at": now.to_rfc3339(),
            "actor": caller.actor,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn send_overdue_reminders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_circulate(&ctx, &caller.role)?;
        let now = Utc::now();
        let mut notified = 0i64;
        let candidates: Vec<LibraryLoan> = state
            .loans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|loan| {
                (loan.status == LOAN_OVERDUE
                    || (loan.status == LOAN_ACTIVE && loan.due_at < now))
                    && loan
                        .last_overdue_noticed_at
                        .map(|noticed| noticed < now - chrono::Duration::days(1))
                        .unwrap_or(true)
            })
            .collect();
        for mut loan in candidates {
            let item = match state.items.get(&loan.item_id) {
                Some(entry) => entry.value().clone(),
                None => continue,
            };
            state.notify(
                "library.overdue",
                &loan.user_id,
                "Overdue library loan",
                &format!(
                    "The item \"{}\" was due on {} and is now overdue.",
                    item.title,
                    loan.due_at.format("%Y-%m-%d")
                ),
                Some(format!("/library/loans/{}", loan.id)),
            );
            loan.last_overdue_noticed_at = Some(now);
            loan.updated_at = now;
            state.loans.insert(loan.id, loan);
            notified += 1;
        }
        Ok(json!({
            "members_notified": notified,
            "sent_at": now.to_rfc3339(),
            "actor": caller.actor,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Settings handlers
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibrarySettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LibrarySettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        let mut validate_list =
            |values: &Option<Vec<String>>, field: &str| -> UnifiedResult<Option<Vec<String>>> {
                match values {
                    Some(list) => {
                        if list.is_empty() {
                            return Err(UnifiedError::bad_request(format!(
                                "{field} must not be empty"
                            )));
                        }
                        let mut checked = Vec::with_capacity(list.len());
                        for value in list {
                            checked.push(require_text(value, field, MAX_SLUG_LEN)?);
                        }
                        Ok(Some(checked))
                    }
                    None => Ok(None),
                }
            };
        if let Some(categories) = validate_list(&input.categories, "categories")? {
            settings.categories = categories;
        }
        if let Some(languages) = validate_list(&input.languages, "languages")? {
            settings.languages = languages;
        }
        if let Some(member_types) = validate_list(&input.member_types, "member_types")? {
            settings.member_types = member_types;
        }
        if let Some(item_types) = validate_list(&input.item_types, "item_types")? {
            settings.item_types = item_types;
        }
        if let Some(item_statuses) = validate_list(&input.item_statuses, "item_statuses")? {
            settings.item_statuses = item_statuses;
        }
        if let Some(copy_statuses) = validate_list(&input.copy_statuses, "copy_statuses")? {
            settings.copy_statuses = copy_statuses;
        }
        if let Some(copy_conditions) = validate_list(&input.copy_conditions, "copy_conditions")? {
            settings.copy_conditions = copy_conditions;
        }
        if let Some(loan_statuses) = validate_list(&input.loan_statuses, "loan_statuses")? {
            settings.loan_statuses = loan_statuses;
        }
        if let Some(hold_statuses) = validate_list(&input.hold_statuses, "hold_statuses")? {
            settings.hold_statuses = hold_statuses;
        }
        if let Some(fine_statuses) = validate_list(&input.fine_statuses, "fine_statuses")? {
            settings.fine_statuses = fine_statuses;
        }
        if let Some(kinds) = validate_list(&input.discrepancy_kinds, "discrepancy_kinds")? {
            settings.discrepancy_kinds = kinds;
        }
        if let Some(currency) = &input.default_currency {
            settings.default_currency = require_currency(currency)?;
        }
        if let Some(days) = input.hold_expiry_days {
            settings.hold_expiry_days = require_positive(days, "hold_expiry_days")?;
        }
        if let Some(ttl) = input.digital_link_ttl_secs {
            if ttl == 0 || ttl > 604_800 {
                return Err(UnifiedError::bad_request(
                    "digital_link_ttl_secs must be between 1 and 604800",
                ));
            }
            settings.digital_link_ttl_secs = ttl;
        }
        if let Some(limit) = input.public_search_rate_limit_per_minute {
            if limit > 10_000 {
                return Err(UnifiedError::bad_request(
                    "public_search_rate_limit_per_minute must be at most 10000",
                ));
            }
            settings.public_search_rate_limit_per_minute = limit;
        }
        if let Some(days) = input.reminder_days_before_due {
            settings.reminder_days_before_due = require_non_negative(days, "reminder_days_before_due")?;
        }
        if let Some(block) = input.block_checkout_when_overdue {
            settings.block_checkout_when_overdue = block;
        }
        if let Some(allow) = input.allow_renew_when_overdue {
            settings.allow_renew_when_overdue = allow;
        }
        if let Some(max) = input.max_grounding_matches {
            if max == 0 || max > 50 {
                return Err(UnifiedError::bad_request(
                    "max_grounding_matches must be between 1 and 50",
                ));
            }
            settings.max_grounding_matches = max;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            "library.settings.updated",
            json!({
                "member_types": settings.member_types,
                "item_types": settings.item_types,
                "default_currency": settings.default_currency,
                "actor": caller.actor,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// AI handlers
// ---------------------------------------------------------------------------

async fn ai_search_assist(
    ctx: web::Data<Arc<AppContext>>,
    _state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let _tenant = require_tenant(&req)?;
    let caller = match caller(&req) {
        Ok(value) => value,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW) {
        return err(error, debug);
    }
    let company_id = resolve_company_id(&req);
    // The AI gate runs inside the helper; this handler never calls
    // a provider directly and never bypasses the gate.
    // The AI module's Caller is the type `run_ai_capability`
    // takes; it carries the same actor and role as this module's
    // local caller.
    let ai_caller = crate::modules::ai::Caller {
        actor: caller.actor.clone(),
        role: caller.role.clone(),
    };
    let delegated = crate::modules::ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "library",
        "search_assist",
        input,
    )
    .await;
    match delegated {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn ai_recommendations(
    ctx: web::Data<Arc<AppContext>>,
    _state: web::Data<LibraryState>,
    req: HttpRequest,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let _tenant = require_tenant(&req)?;
    let caller = match caller(&req) {
        Ok(value) => value,
        Err(error) => return err(error, debug),
    };
    let gate: UnifiedResult<()> = (|| {
        require_permission(&ctx, &caller.role, PERM_LIBRARY_VIEW)?;
        if let Some(user_id) = opt_string(&input, "user_id") {
            if user_id != caller.actor {
                require_permission(&ctx, &caller.role, PERM_LIBRARY_MANAGE)?;
            }
        }
        Ok(())
    })();
    if let Err(error) = gate {
        return err(error, debug);
    }
    let company_id = resolve_company_id(&req);
    // The AI module's Caller is the type `run_ai_capability`
    // takes; it carries the same actor and role as this module's
    // local caller.
    let ai_caller = crate::modules::ai::Caller {
        actor: caller.actor.clone(),
        role: caller.role.clone(),
    };
    let delegated = crate::modules::ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "library",
        "recommendations",
        input,
    )
    .await;
    match delegated {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Grounding for `library.ai.search_assist_requested`: add catalog
/// matches for the query. Catalog data only — no member data.
fn on_search_assist_grounding(state: LibraryState, ctx: &mut HookContext) {
    let query = ctx
        .get("query")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("input")
                .and_then(|value| value.get("query"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    if query.trim().is_empty() {
        return;
    }
    let settings = state.settings();
    let limit = settings.max_grounding_matches.min(MAX_GROUNDING_MATCHES).max(1);
    let mut matches: Vec<LibraryItem> = state
        .items
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.status == ITEM_ACTIVE && item_matches_query(item, &query))
        .collect();
    matches.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
    matches.truncate(limit);
    let grounded: Vec<Value> = matches
        .iter()
        .map(|item| {
            let available = state
                .copies
                .iter()
                .filter(|entry| {
                    entry.value().item_id == item.id && entry.value().status == COPY_AVAILABLE
                })
                .count();
            json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "authors": item.authors,
                "isbn": item.isbn_normalized,
                "category_name": item.category_name,
                "item_type": item.item_type,
                "available_copies": available,
            })
        })
        .collect();
    ctx.add_result(json!({
        "source": "library.catalog",
        "query": query,
        "matches": grounded,
    }));
}

/// Grounding for `library.ai.recommendations_requested`: add the
/// member's anonymised loan-history category profile plus available
/// items in those categories. Category counts only — never raw
/// member personal data, never loan titles.
fn on_recommendations_grounding(state: LibraryState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("input")
                .and_then(|value| value.get("user_id"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    if user_id.is_empty() {
        return;
    }
    let member = state
        .members
        .iter()
        .map(|entry| entry.value().clone())
        .find(|member| member.user_id == user_id);
    let Some(member) = member else {
        return;
    };
    let mut category_counts: HashMap<String, i64> = HashMap::new();
    let mut on_loan_items: Vec<Uuid> = Vec::new();
    for loan in state.loans.iter() {
        let loan = loan.value();
        if loan.member_id != member.id {
            continue;
        }
        if loan.status == LOAN_ACTIVE || loan.status == LOAN_OVERDUE {
            on_loan_items.push(loan.item_id);
        }
        if let Some(item) = state.items.get(&loan.item_id) {
            if let Some(category) = &item.value().category_name {
                *category_counts.entry(category.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut ranked: Vec<(String, i64)> = category_counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(MAX_GROUNDING_CATEGORIES);
    let top_categories: Vec<String> = ranked.iter().map(|entry| entry.0.clone()).collect();
    let settings = state.settings();
    let limit = settings.max_grounding_matches.min(MAX_GROUNDING_MATCHES).max(1);
    let mut suggestions: Vec<Value> = Vec::new();
    for category in &top_categories {
        let mut items: Vec<LibraryItem> = state
            .items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                item.status == ITEM_ACTIVE
                    && item.category_name.as_deref() == Some(category.as_str())
                    && !on_loan_items.contains(&item.id)
            })
            .collect();
        items.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
        for item in items {
            if suggestions.len() >= limit {
                break;
            }
            suggestions.push(json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "authors": item.authors,
                "category_name": item.category_name,
                "item_type": item.item_type,
            }));
        }
        if suggestions.len() >= limit {
            break;
        }
    }
    ctx.add_result(json!({
        "source": "library.loan_history_profile",
        "category_counts": ranked.iter().map(|entry| json!({
            "category": entry.0,
            "loans": entry.1,
        })).collect::<Vec<Value>>(),
        "suggestions": suggestions,
    }));
}

/// A billing payment that names a library fine marks it paid.
/// Payload shapes accepted (read defensively): `{"fine_id", ...}`
/// or `{"reference_type": "library.fine", "reference_id",
/// "payment_id" | "reference"}`.
fn on_billing_paid(state: LibraryState, ctx: &mut HookContext) {
    let fine_id = ctx
        .get("fine_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            let is_library_fine = ctx
                .get("reference_type")
                .and_then(|value| value.as_str())
                == Some("library.fine");
            if is_library_fine {
                ctx.get("reference_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            } else {
                None
            }
        });
    let Some(raw) = fine_id else {
        return;
    };
    let Ok(uuid) = Uuid::parse_str(&raw) else {
        return;
    };
    let Some(mut entry) = state.fines.get_mut(&uuid) else {
        return;
    };
    if entry.value().status != FINE_PENDING {
        return;
    }
    let reference = ctx
        .get("payment_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| opt_string(&ctx.data, "reference"));
    entry.value_mut().status = FINE_PAID.to_string();
    entry.value_mut().paid_reference = reference;
    entry.value_mut().updated_at = Utc::now();
    let fine = entry.value().clone();
    drop(entry);
    state.emit(
        "library.fine.paid",
        json!({
            "fine_id": fine.id.to_string(),
            "loan_id": fine.loan_id.to_string(),
            "member_id": fine.member_id.to_string(),
            "user_id": fine.user_id,
            "amount_minor": fine.amount_minor,
            "currency": fine.currency,
            "paid_reference": fine.paid_reference,
        }),
    );
}

/// A deleted user suspends every library membership tied to them.
fn on_user_deleted(state: LibraryState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(user_id) = user_id else {
        return;
    };
    let now = Utc::now();
    for mut entry in state.members.iter_mut() {
        if entry.value().user_id == user_id && entry.value().status == MEMBER_ACTIVE {
            entry.value_mut().status = MEMBER_SUSPENDED.to_string();
            entry.value_mut().updated_at = now;
            let member = entry.value().clone();
            state.emit(
                "library.member.suspended",
                json!({
                    "member_id": member.id.to_string(),
                    "user_id": member.user_id,
                    "reason": "user_deleted",
                }),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

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
            "billing.payment.completed",
            "library",
            Arc::new(move |ctx: &mut HookContext| {
                on_billing_paid(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "billing.invoice.paid",
            "library",
            Arc::new(move |ctx: &mut HookContext| {
                on_billing_paid(state.clone(), ctx)
            }),
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
            RouteInfo { method: "GET", path: "/api/v1/library/items", owner: "library", description: "List catalog items (paginated, filter by status, item_type, language, category, author, ISBN; full-text-ish substring search)." },
            RouteInfo { method: "POST", path: "/api/v1/library/items", owner: "library", description: "Create a catalog item (catalog; ISBN-10/13 validated, unique per edition)." },
            RouteInfo { method: "GET", path: "/api/v1/library/items/{id}", owner: "library", description: "Get one catalog item." },
            RouteInfo { method: "PATCH", path: "/api/v1/library/items/{id}", owner: "library", description: "Update a catalog item (catalog; withdrawn items cannot be updated)." },
            RouteInfo { method: "DELETE", path: "/api/v1/library/items/{id}", owner: "library", description: "Delete a catalog item with no copies and no loan history (catalog; withdraw otherwise)." },
            RouteInfo { method: "POST", path: "/api/v1/library/items/{id}/withdraw", owner: "library", description: "Withdraw a catalog item and its available copies (catalog; blocked while copies are on loan)." },
            RouteInfo { method: "GET", path: "/api/v1/library/items/{id}/copies", owner: "library", description: "List the copies of an item (paginated, filter by status, shelf, location)." },
            RouteInfo { method: "POST", path: "/api/v1/library/items/{id}/copies", owner: "library", description: "Add a copy to an item (catalog; barcode unique across the library)." },
            RouteInfo { method: "GET", path: "/api/v1/library/copies/{id}", owner: "library", description: "Get one copy." },
            RouteInfo { method: "PATCH", path: "/api/v1/library/copies/{id}", owner: "library", description: "Update a copy (catalog; status changes never move a copy on or off loan directly)." },
            RouteInfo { method: "DELETE", path: "/api/v1/library/copies/{id}", owner: "library", description: "Delete a copy with no loan history (catalog; withdraw instead when it has history)." },
            RouteInfo { method: "GET", path: "/api/v1/library/catalog/search", owner: "library", description: "Public catalog search (read-only, rate-limited per caller, filter by item_type, language, category, author, ISBN; includes availability counts)." },
            RouteInfo { method: "GET", path: "/api/v1/library/catalog/{id}", owner: "library", description: "Get one catalog entry with its copies (active items only)." },
            RouteInfo { method: "GET", path: "/api/v1/library/categories", owner: "library", description: "List categories (paginated, filter by parent_id, searchable)." },
            RouteInfo { method: "POST", path: "/api/v1/library/categories", owner: "library", description: "Create a category (catalog; slug unique)." },
            RouteInfo { method: "GET", path: "/api/v1/library/categories/{id}", owner: "library", description: "Get one category." },
            RouteInfo { method: "PATCH", path: "/api/v1/library/categories/{id}", owner: "library", description: "Update a category (catalog)." },
            RouteInfo { method: "DELETE", path: "/api/v1/library/categories/{id}", owner: "library", description: "Delete a category with no children and no items (catalog)." },
            RouteInfo { method: "GET", path: "/api/v1/library/members", owner: "library", description: "List library members (paginated, filter by member_type, status, user_id)." },
            RouteInfo { method: "POST", path: "/api/v1/library/members", owner: "library", description: "Register a library member (circulate; user_id plus member type, one active membership per user)." },
            RouteInfo { method: "GET", path: "/api/v1/library/members/{id}", owner: "library", description: "Get one member (own record or manager)." },
            RouteInfo { method: "PATCH", path: "/api/v1/library/members/{id}", owner: "library", description: "Update a member (circulate; member type, status, expiry, notes)." },
            RouteInfo { method: "DELETE", path: "/api/v1/library/members/{id}", owner: "library", description: "Delete a member with no active loans and no pending fines (manage; suspend otherwise)." },
            RouteInfo { method: "GET", path: "/api/v1/library/members/{id}/loans", owner: "library", description: "List a member's loans (paginated, filter by status; own record or manager)." },
            RouteInfo { method: "GET", path: "/api/v1/library/members/{id}/history", owner: "library", description: "Loan history of a member with totals and pending fines (own record or manager)." },
            RouteInfo { method: "GET", path: "/api/v1/library/my/loans", owner: "library", description: "The caller's loans (paginated, filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/library/loans", owner: "library", description: "Check out a copy or a digital item (circulate; policy limits, hold queue, and overdue blocking apply)." },
            RouteInfo { method: "GET", path: "/api/v1/library/loans", owner: "library", description: "List loans (paginated, filter by member, user, item, copy, status, due date; staff see all, others see only their own)." },
            RouteInfo { method: "GET", path: "/api/v1/library/loans/overdue", owner: "library", description: "List overdue loans (circulate; includes active loans past their due date)." },
            RouteInfo { method: "POST", path: "/api/v1/library/loans/scan-overdue", owner: "library", description: "Sweep active loans past due: mark overdue, raise fines through billing, notify members (circulate)." },
            RouteInfo { method: "GET", path: "/api/v1/library/loans/{id}", owner: "library", description: "Get one loan (own loan or manager)." },
            RouteInfo { method: "POST", path: "/api/v1/library/loans/{id}/renew", owner: "library", description: "Renew a loan (circulate; blocked by a hold queue, by overdue status per settings, and by the policy renewal limit)." },
            RouteInfo { method: "POST", path: "/api/v1/library/loans/{id}/return", owner: "library", description: "Check in a loan (circulate; overdue returns raise a fine through billing; the next hold is offered the item)." },
            RouteInfo { method: "POST", path: "/api/v1/library/loans/{id}/mark-lost", owner: "library", description: "Mark a loan lost (circulate; replacement charge requested through billing when an amount is given)." },
            RouteInfo { method: "POST", path: "/api/v1/library/loans/{id}/mark-damaged", owner: "library", description: "Mark a loan damaged (circulate; replacement charge requested through billing when an amount is given)." },
            RouteInfo { method: "POST", path: "/api/v1/library/holds", owner: "library", description: "Reserve an item (circulate; FIFO queue position assigned)." },
            RouteInfo { method: "GET", path: "/api/v1/library/holds", owner: "library", description: "List holds (paginated, filter by member, item, status; staff see all, others see only their own)." },
            RouteInfo { method: "GET", path: "/api/v1/library/holds/{id}", owner: "library", description: "Get one hold (own hold or manager)." },
            RouteInfo { method: "POST", path: "/api/v1/library/holds/{id}/cancel", owner: "library", description: "Cancel a queued or ready hold (own hold or circulate staff)." },
            RouteInfo { method: "POST", path: "/api/v1/library/holds/{id}/fulfill", owner: "library", description: "Mark a queued hold ready and notify the member (circulate)." },
            RouteInfo { method: "POST", path: "/api/v1/library/holds/{id}/expire", owner: "library", description: "Expire a ready or queued hold and offer the item to the next hold (circulate)." },
            RouteInfo { method: "GET", path: "/api/v1/library/fines", owner: "library", description: "List fines (paginated, filter by member, loan, status, reason; staff see all, others see only their own)." },
            RouteInfo { method: "GET", path: "/api/v1/library/fines/{id}", owner: "library", description: "Get one fine (own fine or manager)." },
            RouteInfo { method: "POST", path: "/api/v1/library/fines/{id}/waive", owner: "library", description: "Waive a pending fine (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/library/policies", owner: "library", description: "List loan policies (paginated, filter by member_type, item_type, active)." },
            RouteInfo { method: "POST", path: "/api/v1/library/policies", owner: "library", description: "Create a loan policy (manage; loan days, concurrent limit, renewal limit, fine rate, grace days)." },
            RouteInfo { method: "GET", path: "/api/v1/library/policies/{id}", owner: "library", description: "Get one loan policy." },
            RouteInfo { method: "PATCH", path: "/api/v1/library/policies/{id}", owner: "library", description: "Update a loan policy (manage; one active policy per member-type and item-type pair)." },
            RouteInfo { method: "DELETE", path: "/api/v1/library/policies/{id}", owner: "library", description: "Delete a loan policy (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/library/items/{id}/digital-access", owner: "library", description: "Issue a signed expiring digital-access token for an active digital loan." },
            RouteInfo { method: "GET", path: "/api/v1/library/digital-access/{token}", owner: "library", description: "Redeem a signed digital-access token (validates signature and expiry; returns document/storage references only, never bytes)." },
            RouteInfo { method: "GET", path: "/api/v1/library/items/{id}/digital", owner: "library", description: "Get the digital references of an item (document/storage references only)." },
            RouteInfo { method: "POST", path: "/api/v1/library/inventory-audits", owner: "library", description: "Open an inventory audit (manage; optional location/shelf scope)." },
            RouteInfo { method: "GET", path: "/api/v1/library/inventory-audits", owner: "library", description: "List inventory audits (paginated, filter by status)." },
            RouteInfo { method: "GET", path: "/api/v1/library/inventory-audits/{id}", owner: "library", description: "Get one inventory audit." },
            RouteInfo { method: "POST", path: "/api/v1/library/inventory-audits/{id}/complete", owner: "library", description: "Complete an open audit and emit the discrepancy report (manage; blocked while discrepancies are unresolved)." },
            RouteInfo { method: "POST", path: "/api/v1/library/inventory-audits/{id}/discrepancies", owner: "library", description: "Record a discrepancy on an open audit (circulate; missing copies are marked lost, damaged copies marked damaged)." },
            RouteInfo { method: "POST", path: "/api/v1/library/inventory-audits/{id}/discrepancies/{discrepancy_id}/resolve", owner: "library", description: "Resolve a discrepancy (manage; write-off needs explicit approval and withdraws the copy)." },
            RouteInfo { method: "GET", path: "/api/v1/library/inventory-audits/{id}/report", owner: "library", description: "Discrepancy report of an audit (counts by kind and by status)." },
            RouteInfo { method: "POST", path: "/api/v1/library/reminders/send-due-soon", owner: "library", description: "Notify members whose loans fall due inside the configured reminder window (circulate)." },
            RouteInfo { method: "POST", path: "/api/v1/library/reminders/send-overdue", owner: "library", description: "Notify members with overdue loans (circulate; skips loans reminded in the last day)." },
            RouteInfo { method: "GET", path: "/api/v1/library/settings", owner: "library", description: "Get the configurable library settings (lists, defaults, limits)." },
            RouteInfo { method: "PUT", path: "/api/v1/library/settings", owner: "library", description: "Update the configurable library settings (manage)." },
            RouteInfo { method: "POST", path: "/api/v1/library/ai/search-assist", owner: "library", description: "AI-assisted catalog search (view; delegates to the AI gate, grounded on catalog matches)." },
            RouteInfo { method: "POST", path: "/api/v1/library/ai/recommendations", owner: "library", description: "AI recommendations from the anonymised loan-history profile (view; own user_id only unless manager; suggestions only)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LibraryModule::new()));
}
