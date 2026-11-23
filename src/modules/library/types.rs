// Library - Catalog, member, loan, hold, fine, audit and settings types.

use crate::modules::ai::AiCapability;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 300;

pub(crate) const MAX_TEXT_LEN: usize = 5000;

pub(crate) const MAX_SLUG_LEN: usize = 64;

pub(crate) const MAX_REF_LEN: usize = 500;

pub(crate) const MAX_CODE_LEN: usize = 64;

pub(crate) const MAX_NOTE_LEN: usize = 2000;

pub(crate) const MAX_AUTHORS: usize = 20;

pub(crate) const MAX_TAGS: usize = 30;

pub(crate) const MAX_GROUNDING_MATCHES: usize = 10;

pub(crate) const MAX_GROUNDING_CATEGORIES: usize = 10;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_LIBRARY_VIEW: &str = "library.view";

pub(crate) const PERM_LIBRARY_MANAGE: &str = "library.manage";

pub(crate) const PERM_LIBRARY_CIRCULATE: &str = "library.circulate";

pub(crate) const PERM_LIBRARY_CATALOG: &str = "library.catalog";

/// Copy statuses. Stored values; the live list in settings is the
/// authority and these constants only name the values the loan flow
/// transitions between.
pub(crate) const COPY_AVAILABLE: &str = "available";

pub(crate) const COPY_ON_LOAN: &str = "on-loan";

pub(crate) const COPY_RESERVED: &str = "reserved";

pub(crate) const COPY_LOST: &str = "lost";

pub(crate) const COPY_DAMAGED: &str = "damaged";

pub(crate) const COPY_WITHDRAWN: &str = "withdrawn";

/// Loan statuses.
pub(crate) const LOAN_ACTIVE: &str = "active";

pub(crate) const LOAN_OVERDUE: &str = "overdue";

pub(crate) const LOAN_RETURNED: &str = "returned";

pub(crate) const LOAN_LOST: &str = "lost";

pub(crate) const LOAN_DAMAGED: &str = "damaged";

/// Hold statuses.
pub(crate) const HOLD_QUEUED: &str = "queued";

pub(crate) const HOLD_READY: &str = "ready";

pub(crate) const HOLD_FULFILLED: &str = "fulfilled";

pub(crate) const HOLD_CANCELLED: &str = "cancelled";

pub(crate) const HOLD_EXPIRED: &str = "expired";

/// Fine statuses.
pub(crate) const FINE_PENDING: &str = "pending";

pub(crate) const FINE_PAID: &str = "paid";

pub(crate) const FINE_WAIVED: &str = "waived";

/// Member statuses.
pub(crate) const MEMBER_ACTIVE: &str = "active";

pub(crate) const MEMBER_SUSPENDED: &str = "suspended";

pub(crate) const MEMBER_EXPIRED: &str = "expired";

/// Item statuses.
pub(crate) const ITEM_ACTIVE: &str = "active";

pub(crate) const ITEM_WITHDRAWN: &str = "withdrawn";

/// Audit statuses.
pub(crate) const AUDIT_OPEN: &str = "open";

pub(crate) const AUDIT_COMPLETED: &str = "completed";

/// Discrepancy kinds and resolutions. The kind list is validated
/// against settings; these constants name the values the audit
/// flow acts on.
pub(crate) const DISCREPANCY_MISSING: &str = "missing";

pub(crate) const DISCREPANCY_DAMAGED: &str = "damaged";

pub(crate) const DISCREPANCY_FOUND: &str = "found";

pub(crate) const DISCREPANCY_PENDING: &str = "pending";

pub(crate) const DISCREPANCY_RESOLVED: &str = "resolved";

pub(crate) const DISCREPANCY_WRITTEN_OFF: &str = "written_off";

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
