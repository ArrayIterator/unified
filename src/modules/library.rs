//! Library module - Library module - catalog, copies, members, loans, holds, fines,
//!
//! Split into submodules by concern; the public API is
//! re-exported below. The full route map and module notes
//! live in `library/module.rs`.

pub mod types;
mod state;
mod auth;
mod validation;
mod helpers;
mod service;
mod handlers_catalog;
mod handlers_circulation;
mod handlers_holds;
mod handlers_admin;
mod events;
pub mod module;

pub use self::types::{CreateAuditRequest, CreateCategoryRequest, CreateCopyRequest,
    CreateDiscrepancyRequest, CreateHoldRequest, CreateItemRequest, CreateLoanRequest,
    CreateMemberRequest, CreatePolicyRequest, DigitalAccessRequest, MarkLoanRequest,
    ResolveDiscrepancyRequest, UpdateCategoryRequest, UpdateCopyRequest, UpdateItemRequest,
    UpdateMemberRequest, UpdatePolicyRequest, UpdateSettingsRequest};
pub(crate) use self::types::{AI_CAPABILITIES, AUDIT_COMPLETED, AUDIT_OPEN, COPY_AVAILABLE,
    COPY_DAMAGED, COPY_LOST, COPY_ON_LOAN, COPY_RESERVED, COPY_WITHDRAWN, DISCREPANCY_DAMAGED,
    DISCREPANCY_FOUND, DISCREPANCY_MISSING, DISCREPANCY_PENDING, DISCREPANCY_RESOLVED,
    DISCREPANCY_WRITTEN_OFF, DigitalAccess, FINE_PAID, FINE_PENDING, FINE_WAIVED, HOLD_CANCELLED,
    HOLD_EXPIRED, HOLD_FULFILLED, HOLD_QUEUED, HOLD_READY, ITEM_ACTIVE, ITEM_WITHDRAWN,
    InventoryAudit, InventoryDiscrepancy, LOAN_ACTIVE, LOAN_DAMAGED, LOAN_LOST, LOAN_OVERDUE,
    LOAN_RETURNED, LibraryCategory, LibraryCopy, LibraryFine, LibraryHold, LibraryItem, LibraryLoan,
    LibraryMember, LibrarySettings, LoanPolicy, MAX_AUTHORS, MAX_CODE_LEN, MAX_GROUNDING_CATEGORIES,
    MAX_GROUNDING_MATCHES, MAX_NOTE_LEN, MAX_REF_LEN, MAX_SLUG_LEN, MAX_TAGS, MAX_TEXT_LEN,
    MAX_TITLE_LEN, MEMBER_ACTIVE, MEMBER_EXPIRED, MEMBER_SUSPENDED, PERM_LIBRARY_CATALOG,
    PERM_LIBRARY_CIRCULATE, PERM_LIBRARY_MANAGE, PERM_LIBRARY_VIEW};
pub(crate) use self::state::{LibraryState};
pub(crate) use self::auth::{Caller, caller, can_catalog, can_circulate, check_capability,
    constant_time_eq, require_catalog, require_circulate, require_permission, require_tenant};
pub(crate) use self::validation::{normalize_isbn, optional_text, parse_datetime,
    parse_optional_uuid, parse_uuid, require_currency, require_in_list, require_non_negative,
    require_positive, require_text, slugify};
pub(crate) use self::helpers::{created, err, ok, opt_string, paginate};
pub(crate) use self::service::{check_search_rate, create_fine_for_loan, first_queued_hold,
    item_matches_query, load_audit, load_category, load_copy, load_fine, load_hold, load_item,
    load_loan, load_member, load_policy, next_hold_position, offer_to_next_hold, overdue_fine_minor,
    resolve_policy};
pub(crate) use self::handlers_catalog::{create_category, create_copy, create_item, delete_category,
    delete_copy, delete_item, get_catalog_item, get_category, get_copy, get_item, list_categories,
    list_item_copies, list_items, search_catalog, update_category, update_copy, update_item,
    withdraw_item};
pub(crate) use self::handlers_circulation::{create_loan, create_member, delete_member, get_loan,
    get_member, list_loans, list_member_loans, list_members, list_my_loans, list_overdue_loans,
    mark_loan_damaged, mark_loan_lost, member_history, renew_loan, return_loan, scan_overdue_loans,
    update_member};
pub(crate) use self::handlers_holds::{cancel_hold, create_hold, create_policy, delete_policy,
    expire_hold, fulfill_hold, get_fine, get_hold, get_policy, list_fines, list_holds,
    list_policies, update_policy, waive_fine};
pub(crate) use self::handlers_admin::{add_discrepancy, ai_recommendations, ai_search_assist,
    audit_report, complete_audit, create_audit, get_audit, get_item_digital, get_settings,
    issue_digital_access, list_audits, redeem_digital_access, resolve_discrepancy,
    send_due_soon_reminders, send_overdue_reminders, update_settings};
pub(crate) use self::events::{on_billing_paid, on_recommendations_grounding,
    on_search_assist_grounding, on_user_deleted};
pub use self::module::{LibraryModule, register};
