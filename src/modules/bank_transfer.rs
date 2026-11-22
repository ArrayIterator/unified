// Bank Transfer module — manual payment verification.
//
// Bank transfer is a payment method with no provider API: the
// operator configures the bank accounts that receive money, the
// shopper transfers the amount to one of them and uploads a payment
// proof, and an administrator verifies or rejects the transfer.
//
// Flow:
//
//   1. Checkout with provider `bank_transfer` (through
//      `POST /api/v1/store/orders/{id}/pay`) emits
//      `payment.initiated`. This module listens to it and opens a
//      transfer verification in status `awaiting_proof`, keyed by
//      the order id; the store moves the order to
//      `pending_verification` on `bank_transfer.verification.created`
//      and back to `paid` on approval (`payment.captured`) or to
//      `cancelled` on rejection.
//   2. The shopper uploads proof (`POST
//      /api/v1/bank-transfer/verifications/{id}/proof`): a file
//      reference (storage key) plus the transferred amount, the
//      transfer date, and the bank reference number. Only the
//      reference is stored — never raw file bytes. The amount is
//      checked against the expected order total within a
//      configurable tolerance; a mismatch flags the verification
//      instead of blocking it, so an administrator can still
//      approve it with a reason.
//   3. An administrator verifies (`.../verify`) or rejects
//      (`.../reject`, reason required). Approval emits
//      `payment.captured` with provider `bank_transfer` and a
//      transaction id, so the existing store / billing / licensing
//      wiring picks the payment up exactly like a gateway payment.
//      Rejection emits `bank_transfer.rejected` plus
//      `payment.failed` and notifies the customer, so the order can
//      be moved back to awaiting payment. Every step writes an audit
//      entry (action, actor, role, reason, timestamp) and sends a
//      notification (submitted -> admin, verified / rejected ->
//      customer).
//
// Split bills: paying a `bank_transfer` part of a split emits
// `bank_transfer.proof.requested`; this module listens to it and
// opens a verification carrying the split / part ids, so approval
// of that verification completes the part through the same
// `payment.captured` path.
//
// Money is always integer minor units plus an ISO 4217 currency
// code, matching the payments module. Account numbers are masked on
// every read (only the last four characters are ever returned); the
// full number is stored only in the account record and never
// leaves the API, is never logged, and never appears in a hook
// payload. Secrets do not exist here — there is no provider API and
// therefore no credential to store.
//
// Persistence: records are kept in memory (same pattern as the
// provider registry and the store). Durable persistence is a
// follow-up for the integrator — see the migration sketch in the
// coordinator report.
//
// Events emitted: `bank_transfer.verification.created`,
// `bank_transfer.proof.submitted`, `bank_transfer.verified`,
// `bank_transfer.rejected`, `bank_transfer.cancelled`,
// `payment.captured` (on approval), `payment.failed` (on rejection),
// `notification.requested` (at every step). Events listened to:
// `payment.initiated` (provider `bank_transfer`),
// `bank_transfer.proof.requested` (split part payment).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

/// Emitted when a transfer verification is opened.
pub const BANK_TRANSFER_VERIFICATION_CREATED: &str = "bank_transfer.verification.created";
/// Emitted when a customer submits payment proof.
pub const BANK_TRANSFER_PROOF_SUBMITTED: &str = "bank_transfer.proof.submitted";
/// Emitted when an administrator approves a transfer.
pub const BANK_TRANSFER_VERIFIED: &str = "bank_transfer.verified";
/// Emitted when an administrator rejects a transfer.
pub const BANK_TRANSFER_REJECTED: &str = "bank_transfer.rejected";
/// Emitted when a verification is cancelled.
pub const BANK_TRANSFER_CANCELLED: &str = "bank_transfer.cancelled";

/// Verification statuses: `awaiting_proof`, `submitted`, `approved`,
/// `rejected`, `cancelled`.
const STATUS_AWAITING_PROOF: &str = "awaiting_proof";
const STATUS_SUBMITTED: &str = "submitted";
const STATUS_APPROVED: &str = "approved";
const STATUS_REJECTED: &str = "rejected";
const STATUS_CANCELLED: &str = "cancelled";

// ---------------------------------------------------------------------------
// Settings.
// ---------------------------------------------------------------------------

fn default_proof_expiry_days() -> u32 {
    7
}

/// Operator settings for manual verification. Kept in memory; the
/// integrator persists them with the migration in the coordinator
/// report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BankTransferSettings {
    /// Maximum minor-unit difference accepted between the transferred
    /// amount and the expected total before the proof is flagged as
    /// a mismatch. Default 0 (exact match required).
    #[serde(default)]
    pub amount_tolerance_minor: i64,
    /// Days after which an unsubmitted proof request expires.
    /// Informational: expiry itself is enforced by the operator
    /// (verifications do not auto-cancel), and the value is shown on
    /// the verification so the shopper sees the deadline.
    #[serde(default = "default_proof_expiry_days")]
    pub proof_expiry_days: u32,
    /// Whether the bank reference number is required on proof upload.
    #[serde(default)]
    pub require_reference_number: bool,
}

impl Default for BankTransferSettings {
    fn default() -> Self {
        Self {
            amount_tolerance_minor: 0,
            proof_expiry_days: 7,
            require_reference_number: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBankTransferSettings {
    #[serde(default)]
    pub amount_tolerance_minor: Option<i64>,
    #[serde(default)]
    pub proof_expiry_days: Option<u32>,
    #[serde(default)]
    pub require_reference_number: Option<bool>,
}

// ---------------------------------------------------------------------------
// Bank accounts.
// ---------------------------------------------------------------------------

/// One bank account that can receive transfers. The full account
/// number is stored on the record but is never serialized: every
/// read goes through `BankAccountView`, which only exposes the
/// masked number and the last four characters.
#[derive(Debug, Clone)]
pub struct BankAccount {
    pub id: Uuid,
    pub bank_name: String,
    pub account_holder: String,
    pub account_number: String,
    pub swift_bic: Option<String>,
    pub currency: String,
    pub instructions: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Masked read view of a bank account. The account number is never
/// returned in full: all but the last four characters are replaced
/// with `*`.
#[derive(Debug, Clone, Serialize)]
pub struct BankAccountView {
    pub id: Uuid,
    pub bank_name: String,
    pub account_holder: String,
    pub account_number_masked: String,
    pub account_number_last4: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swift_bic: Option<String>,
    pub currency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Mask an account number / IBAN: keep only the last four
/// characters, replace the rest with `*`. Whitespace is stripped
/// first so grouped IBANs mask consistently.
pub fn mask_account_number(value: &str) -> String {
    let compact: Vec<char> = value.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() <= 4 {
        return "*".repeat(compact.len().max(1));
    }
    let mut masked = "*".repeat(compact.len() - 4);
    for c in &compact[compact.len() - 4..] {
        masked.push(*c);
    }
    masked
}

fn last4(value: &str) -> String {
    let compact: Vec<char> = value.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() <= 4 {
        return compact.into_iter().collect();
    }
    compact[compact.len() - 4..].iter().collect()
}

impl BankAccount {
    fn view(&self) -> BankAccountView {
        BankAccountView {
            id: self.id,
            bank_name: self.bank_name.clone(),
            account_holder: self.account_holder.clone(),
            account_number_masked: mask_account_number(&self.account_number),
            account_number_last4: last4(&self.account_number),
            swift_bic: self.swift_bic.clone(),
            currency: self.currency.clone(),
            instructions: self.instructions.clone(),
            active: self.active,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateBankAccountRequest {
    pub bank_name: String,
    pub account_holder: String,
    pub account_number: String,
    #[serde(default)]
    pub swift_bic: Option<String>,
    pub currency: String,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default = "default_active")]
    pub active: bool,
}

fn default_active() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBankAccountRequest {
    #[serde(default)]
    pub bank_name: Option<String>,
    #[serde(default)]
    pub account_holder: Option<String>,
    #[serde(default)]
    pub account_number: Option<String>,
    #[serde(default)]
    pub swift_bic: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
}

// ---------------------------------------------------------------------------
// Verifications, proofs, audit trail.
// ---------------------------------------------------------------------------

/// Payment proof uploaded by the shopper. Only a reference to the
/// uploaded file is stored (storage key plus the uploader-provided
/// checksum) — never raw file bytes. The checksum is the content
/// hash supplied with the upload; this module never sees the file
/// bytes, so it cannot recompute it, it only records it for the
/// administrator to compare against the stored file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferProof {
    /// Storage key of the uploaded proof file (for example
    /// `proofs/<order-id>/<file>`), as returned by the file store.
    pub storage_key: String,
    /// Content checksum supplied with the upload (hex digest, for
    /// example SHA-256). Absent when the uploader did not supply one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    /// Amount the shopper says they transferred, in minor units.
    pub amount_minor: i64,
    pub currency: String,
    /// Date of the transfer, `YYYY-MM-DD`.
    pub transfer_date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub submitted_by: String,
    pub submitted_at: DateTime<Utc>,
}

/// One manual transfer verification, keyed by order (and, for split
/// bills, by split part). It mirrors the store order's
/// `pending_verification` state while the transfer waits for an
/// administrator decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferVerification {
    pub id: Uuid,
    pub order_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoice_id: Option<String>,
    pub customer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank_account_id: Option<Uuid>,
    /// `awaiting_proof`, `submitted`, `approved`, `rejected`, or
    /// `cancelled`.
    pub status: String,
    /// Expected total in minor units, when the order total is known
    /// (from the checkout handoff or the split part). Absent when
    /// the total is unknown: the amount check is then skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_amount_minor: Option<i64>,
    pub currency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<TransferProof>,
    /// `not_checked` (no proof yet), `matched`, `mismatch`, or
    /// `skipped_no_expected_amount`.
    pub amount_check: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_difference_minor: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Detail view of a verification: the record, the masked bank
/// account it names, and the full audit trail.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationDetailView {
    pub verification: TransferVerification,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank_account: Option<BankAccountView>,
    pub audit: Vec<VerificationAuditEntry>,
}

/// One audit entry: every step of the manual checking workflow
/// (created, proof submitted, verified, rejected, cancelled) with
/// the actor, the actor role, an optional reason, and a timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationAuditEntry {
    pub id: Uuid,
    pub verification_id: Uuid,
    /// `created`, `proof_requested`, `proof_submitted`, `verified`,
    /// `rejected`, or `cancelled`.
    pub action: String,
    pub actor: String,
    /// `customer`, `admin`, or `system`.
    pub actor_role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateVerificationRequest {
    pub order_id: String,
    pub customer_id: String,
    #[serde(default)]
    pub invoice_id: Option<String>,
    #[serde(default)]
    pub split_id: Option<Uuid>,
    #[serde(default)]
    pub part_id: Option<Uuid>,
    #[serde(default)]
    pub bank_account_id: Option<Uuid>,
    #[serde(default)]
    pub expected_amount_minor: Option<i64>,
    pub currency: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubmitProofRequest {
    #[serde(default)]
    pub bank_account_id: Option<Uuid>,
    pub storage_key: String,
    #[serde(default)]
    pub checksum: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    /// Date of the transfer, `YYYY-MM-DD`.
    pub transfer_date: String,
    #[serde(default)]
    pub reference_number: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub actor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerifyRequest {
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub transaction_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RejectRequest {
    #[serde(default)]
    pub actor: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CancelVerificationRequest {
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// State and in-memory stores.
// ---------------------------------------------------------------------------

pub struct BankTransferState {
    hooks: RwLock<Arc<HookBus>>,
}

impl BankTransferState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }
}

static ACCOUNTS: OnceLock<DashMap<Uuid, BankAccount>> = OnceLock::new();

fn accounts_store() -> &'static DashMap<Uuid, BankAccount> {
    ACCOUNTS.get_or_init(DashMap::new)
}

static VERIFICATIONS: OnceLock<DashMap<Uuid, TransferVerification>> = OnceLock::new();

fn verifications_store() -> &'static DashMap<Uuid, TransferVerification> {
    VERIFICATIONS.get_or_init(DashMap::new)
}

static AUDIT: OnceLock<DashMap<Uuid, VerificationAuditEntry>> = OnceLock::new();

fn audit_store() -> &'static DashMap<Uuid, VerificationAuditEntry> {
    AUDIT.get_or_init(DashMap::new)
}

static SETTINGS: OnceLock<RwLock<BankTransferSettings>> = OnceLock::new();

fn settings_store() -> &'static RwLock<BankTransferSettings> {
    SETTINGS.get_or_init(|| RwLock::new(BankTransferSettings::default()))
}

fn settings() -> BankTransferSettings {
    settings_store()
        .read()
        .expect("settings lock poisoned")
        .clone()
}

// ---------------------------------------------------------------------------
// Validation helpers.
// ---------------------------------------------------------------------------

/// Validate an ISO 4217 currency code (three uppercase letters).
fn validate_currency(currency: &str) -> UnifiedResult<String> {
    let code = currency.trim().to_uppercase();
    if code.len() != 3 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(format!(
            "Currency \"{currency}\" is not a three-letter ISO 4217 code"
        )));
    }
    Ok(code)
}

/// Normalize an account number for duplicate checks: strip
/// whitespace and uppercase.
fn normalize_account_number(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_uppercase())
        .collect()
}

/// Validate an account number / IBAN: 4-34 alphanumeric characters
/// after whitespace is stripped.
fn validate_account_number(value: &str) -> UnifiedResult<String> {
    let compact = normalize_account_number(value);
    if compact.len() < 4 || compact.len() > 34 {
        return Err(UnifiedError::bad_request(
            "Account number / IBAN must be 4 to 34 characters",
        ));
    }
    if !compact.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(UnifiedError::bad_request(
            "Account number / IBAN must contain only letters and digits",
        ));
    }
    Ok(compact)
}

/// Validate a SWIFT / BIC code: 8 or 11 alphanumeric characters.
fn validate_swift_bic(value: &str) -> UnifiedResult<String> {
    let code = value.trim().to_uppercase();
    if !(code.len() == 8 || code.len() == 11)
        || !code.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(UnifiedError::bad_request(
            "SWIFT / BIC must be 8 or 11 alphanumeric characters",
        ));
    }
    Ok(code)
}

/// Validate a proof storage key: a relative key, never an absolute
/// path and never containing a parent-directory traversal. Only the
/// reference is stored; the file itself lives in the file store.
fn validate_storage_key(value: &str) -> UnifiedResult<String> {
    let key = value.trim().to_string();
    if key.is_empty() {
        return Err(UnifiedError::bad_request(
            "Proof storage_key is required (a reference to the uploaded proof file)",
        ));
    }
    if key.len() > 512 {
        return Err(UnifiedError::bad_request(
            "Proof storage_key must be at most 512 characters",
        ));
    }
    if key.starts_with('/') || key.contains('\\') || key.split('/').any(|part| part == "..") {
        return Err(UnifiedError::bad_request(
            "Proof storage_key must be a relative key without parent-directory traversal",
        ));
    }
    if key.chars().any(|c| c.is_control()) {
        return Err(UnifiedError::bad_request(
            "Proof storage_key must not contain control characters",
        ));
    }
    Ok(key)
}

/// Validate a transfer date (`YYYY-MM-DD`): well-formed and not in
/// the future.
fn validate_transfer_date(value: &str) -> UnifiedResult<String> {
    let date = NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").map_err(|_| {
        UnifiedError::bad_request(format!(
            "Transfer date \"{value}\" must be YYYY-MM-DD"
        ))
    })?;
    if date > Utc::now().date_naive() {
        return Err(UnifiedError::bad_request(
            "Transfer date must not be in the future",
        ));
    }
    Ok(date.format("%Y-%m-%d").to_string())
}

/// True for zero-decimal currencies (no minor units).
fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency,
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "XPF"
    )
}

/// Convert a major-unit amount (as the store reports order totals)
/// to integer minor units.
fn major_to_minor(amount_major: f64, currency: &str) -> Option<i64> {
    if !amount_major.is_finite() || amount_major < 0.0 {
        return None;
    }
    if is_zero_decimal_currency(currency) {
        Some(amount_major.round() as i64)
    } else {
        Some((amount_major * 100.0).round() as i64)
    }
}

/// Actor identity: an explicit body actor wins, then the `x-user-id`
/// / `x-session-id` headers, then `unknown`. The value is recorded
/// verbatim in the audit trail.
fn actor_of(req: &HttpRequest, body_actor: Option<String>) -> String {
    if let Some(actor) = body_actor
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return actor;
    }
    for header in ["x-user-id", "x-session-id"] {
        if let Some(value) = req
            .headers()
            .get(header)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.trim().is_empty())
        {
            return value.trim().to_string();
        }
    }
    "unknown".to_string()
}

/// Caller identities from the request headers (`x-user-id` and
/// `x-session-id`). Unlike `actor_of`, the body-supplied actor is
/// never trusted here: the body value is self-asserted, so only the
/// headers count for ownership checks. See the store module: this is
/// an ownership check on self-asserted header identity, not
/// authentication.
fn caller_identities(req: &HttpRequest) -> Vec<String> {
    ["x-user-id", "x-session-id"]
        .into_iter()
        .filter_map(|header| {
            req.headers()
                .get(header)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .collect()
}

/// True when the caller may act on a record owned by `owner`: one of
/// the caller identities matches the owner, or there are no identity
/// headers and the owner is `guest`.
fn caller_owns(req: &HttpRequest, owner: &str) -> bool {
    let identities = caller_identities(req);
    if identities.is_empty() {
        return owner == "guest";
    }
    identities.iter().any(|identity| identity == owner)
}

/// Reject a customer action on a verification the caller does not
/// own. Proof upload and cancellation are customer actions, so the
/// caller must be the verification's customer.
fn require_verification_owner(
    req: &HttpRequest,
    verification: &TransferVerification,
) -> UnifiedResult<()> {
    if caller_owns(req, &verification.customer_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Verification belongs to a different customer",
        ))
    }
}

/// Reject an administrator action performed by the customer themself.
/// Verification and rejection are administrator actions and there is
/// no administrator identity in this module yet, so the one abuse
/// that can be checked here is self-approval: when the caller
/// identifies as the verification's customer, the call is refused.
/// A caller with no identity header is allowed through and the gap
/// is recorded for the API-management auth layer.
fn forbid_self_approval(
    req: &HttpRequest,
    verification: &TransferVerification,
) -> UnifiedResult<()> {
    let identities = caller_identities(req);
    if !identities.is_empty()
        && identities
            .iter()
            .any(|identity| identity == &verification.customer_id)
    {
        return Err(UnifiedError::forbidden(
            "The customer cannot verify or reject their own transfer",
        ));
    }
    Ok(())
}

/// Paginate an already-sorted, already-filtered list.
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
        None => true,
        Some(needle) => haystack.to_lowercase().contains(&needle.to_lowercase()),
    }
}

/// Request an in-app notification through the hook bus (the
/// notifications module listens to `notification.requested`).
fn notify(state: &BankTransferState, user_id: &str, kind: &str, title: &str, body: &str) {
    state.emit(
        "notification.requested",
        json!({
            "user_id": user_id,
            "kind": kind,
            "title": title,
            "body": body,
        }),
    );
}

/// Append one audit entry for a verification.
fn audit(
    verification_id: Uuid,
    action: &str,
    actor: &str,
    actor_role: &str,
    reason: Option<String>,
) {
    let entry = VerificationAuditEntry {
        id: Uuid::now_v7(),
        verification_id,
        action: action.to_string(),
        actor: actor.to_string(),
        actor_role: actor_role.to_string(),
        reason,
        created_at: Utc::now(),
    };
    audit_store().insert(entry.id, entry);
}

fn find_account(id: Uuid) -> UnifiedResult<BankAccount> {
    accounts_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Bank account \"{id}\" not found")))
}

fn find_verification(id: Uuid) -> UnifiedResult<TransferVerification> {
    verifications_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!("Transfer verification \"{id}\" not found"))
        })
}

/// Build the detail view of a verification: the record, the masked
/// bank account it names, and the full audit trail in time order.
fn verification_detail(verification: &TransferVerification) -> VerificationDetailView {
    let bank_account = verification
        .bank_account_id
        .and_then(|id| accounts_store().get(&id).map(|entry| entry.value().view()));
    let mut entries: Vec<VerificationAuditEntry> = audit_store()
        .iter()
        .filter(|entry| entry.value().verification_id == verification.id)
        .map(|entry| entry.value().clone())
        .collect();
    entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    VerificationDetailView {
        verification: verification.clone(),
        bank_account,
        audit: entries,
    }
}

// ---------------------------------------------------------------------------
// Verification creation (shared by the HTTP handler and the hook
// listeners).
// ---------------------------------------------------------------------------

struct NewVerification {
    order_id: String,
    invoice_id: Option<String>,
    customer_id: String,
    split_id: Option<Uuid>,
    part_id: Option<Uuid>,
    bank_account_id: Option<Uuid>,
    expected_amount_minor: Option<i64>,
    currency: String,
}

/// Open a verification in status `awaiting_proof`. A second open
/// verification for the same order (and the same split part, when
/// one is given) is refused with a 409, so a checkout or a split
/// payment can be retried safely without opening duplicates.
fn insert_verification(input: NewVerification, actor: &str) -> UnifiedResult<TransferVerification> {
    let open = verifications_store().iter().any(|entry| {
        let existing = entry.value();
        existing.order_id == input.order_id
            && existing.split_id == input.split_id
            && existing.part_id == input.part_id
            && (existing.status == STATUS_AWAITING_PROOF || existing.status == STATUS_SUBMITTED)
    });
    if open {
        return Err(UnifiedError::conflict(format!(
            "An open transfer verification already exists for order {}",
            input.order_id
        )));
    }
    if let Some(bank_account_id) = input.bank_account_id {
        let account = find_account(bank_account_id)?;
        if !account.active {
            return Err(UnifiedError::unprocessable(
                "The selected bank account is not active",
            ));
        }
    }
    let now = Utc::now();
    let verification = TransferVerification {
        id: Uuid::now_v7(),
        order_id: input.order_id,
        invoice_id: input.invoice_id,
        customer_id: input.customer_id,
        split_id: input.split_id,
        part_id: input.part_id,
        bank_account_id: input.bank_account_id,
        status: STATUS_AWAITING_PROOF.to_string(),
        expected_amount_minor: input.expected_amount_minor,
        currency: input.currency,
        proof: None,
        amount_check: "not_checked".to_string(),
        amount_difference_minor: None,
        transaction_id: None,
        verified_by: None,
        verified_at: None,
        rejection_reason: None,
        created_at: now,
        updated_at: now,
    };
    verifications_store().insert(verification.id, verification.clone());
    audit(
        verification.id,
        "created",
        actor,
        "system",
        None,
    );
    Ok(verification)
}

// ---------------------------------------------------------------------------
// Hook listeners.
// ---------------------------------------------------------------------------

/// The store started a checkout with provider `bank_transfer` and
/// emitted `payment.initiated` with `{ "order", "provider",
/// "amount" }`. Open a verification in status `awaiting_proof` so
/// the shopper can upload proof. The expected amount comes from the
/// order total (major units, converted to minor units); when the
/// total is absent the amount check is skipped on proof upload.
fn on_payment_initiated(state: Arc<BankTransferState>, ctx: &mut HookContext) {
    let provider = ctx
        .get("provider")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if provider != "bank_transfer" && provider != "bank-transfer" {
        return;
    }
    let order = ctx.get("order").cloned().unwrap_or(Value::Null);
    let order_id = order
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "bank_transfer": "rejected",
            "error": "payment.initiated carries no order id",
        }));
        return;
    };
    let customer_id = order
        .get("user_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let currency = order
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_uppercase();
    let amount_major = ctx
        .get("amount")
        .and_then(|value| value.as_f64())
        .or_else(|| order.get("total").and_then(|value| value.as_f64()));
    let expected = amount_major.and_then(|amount| major_to_minor(amount, &currency));
    match insert_verification(
        NewVerification {
            order_id: order_id.clone(),
            invoice_id: None,
            customer_id,
            split_id: None,
            part_id: None,
            bank_account_id: None,
            expected_amount_minor: expected,
            currency,
        },
        "system",
    ) {
        Ok(verification) => {
            // The store moves the order to `pending_verification` on
            // this event, so it is emitted here as well as on the
            // manual create endpoint.
            state.emit(
                BANK_TRANSFER_VERIFICATION_CREATED,
                json!({
                    "verification_id": verification.id,
                    "order_id": verification.order_id.clone(),
                    "customer_id": verification.customer_id.clone(),
                    "status": verification.status.clone(),
                }),
            );
            ctx.add_result(json!({
                "bank_transfer": "accepted",
                "order_id": order_id,
                "verification_id": verification.id,
                "status": verification.status,
                "note": "Transfer the amount to a configured bank account, then upload proof with POST /api/v1/bank-transfer/verifications/{id}/proof.",
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "bank_transfer": "rejected",
                "order_id": order_id,
                "error": error.message,
            }));
        }
    }
}

/// A split part is being paid with bank transfer (emitted by the
/// payments module as `bank_transfer.proof.requested` with the split
/// / part ids, the payer, and the part amount). Open a verification
/// for that part so the payer can upload proof; approval then
/// completes the part through `payment.captured`.
fn on_proof_requested(state: Arc<BankTransferState>, ctx: &mut HookContext) {
    let split_id = ctx
        .get("split_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok());
    let part_id = ctx
        .get("part_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok());
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    if order_id.is_empty() {
        ctx.add_result(json!({
            "bank_transfer": "rejected",
            "error": "bank_transfer.proof.requested carries no order_id",
        }));
        return;
    }
    let customer_id = ctx
        .get("customer_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_uppercase();
    let expected = ctx
        .get("amount_minor")
        .and_then(|value| value.as_i64());
    let invoice_id = ctx
        .get("invoice_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    match insert_verification(
        NewVerification {
            order_id: order_id.clone(),
            invoice_id,
            customer_id,
            split_id,
            part_id,
            bank_account_id: None,
            expected_amount_minor: expected,
            currency,
        },
        "system",
    ) {
        Ok(verification) => {
            audit(
                verification.id,
                "proof_requested",
                "system",
                "system",
                None,
            );
            state.emit(
                BANK_TRANSFER_VERIFICATION_CREATED,
                json!({
                    "verification_id": verification.id,
                    "order_id": verification.order_id.clone(),
                    "customer_id": verification.customer_id.clone(),
                    "status": verification.status.clone(),
                    "split_id": verification.split_id,
                    "part_id": verification.part_id,
                }),
            );
            ctx.add_result(json!({
                "bank_transfer": "accepted",
                "order_id": order_id,
                "verification_id": verification.id,
                "status": verification.status,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "bank_transfer": "rejected",
                "order_id": order_id,
                "error": error.message,
            }));
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers: status, settings.
// ---------------------------------------------------------------------------

async fn get_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _ = state;
    let result: UnifiedResult<Value> = (|| {
        let accounts_total = accounts_store().len() as u64;
        let accounts_active = accounts_store()
            .iter()
            .filter(|entry| entry.value().active)
            .count() as u64;
        let mut by_status: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for entry in verifications_store().iter() {
            *by_status
                .entry(entry.value().status.clone())
                .or_insert(0) += 1;
        }
        let pending = by_status.get(STATUS_AWAITING_PROOF).copied().unwrap_or(0)
            + by_status.get(STATUS_SUBMITTED).copied().unwrap_or(0);
        Ok(json!({
            "module": "bank_transfer",
            "accounts_total": accounts_total,
            "accounts_active": accounts_active,
            "verifications_by_status": by_status,
            "verifications_pending": pending,
        }))
    })();
    json::respond(result, debug)
}

async fn get_settings(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BankTransferSettings> = Ok(settings());
    json::respond(result, debug)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<UpdateBankTransferSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BankTransferSettings> = (|| {
        let input = body.into_inner();
        if let Some(tolerance) = input.amount_tolerance_minor {
            if tolerance < 0 {
                return Err(UnifiedError::bad_request(
                    "amount_tolerance_minor must be zero or greater",
                ));
            }
        }
        if let Some(days) = input.proof_expiry_days {
            if days == 0 || days > 365 {
                return Err(UnifiedError::bad_request(
                    "proof_expiry_days must be between 1 and 365",
                ));
            }
        }
        let mut current = settings_store()
            .write()
            .expect("settings lock poisoned");
        if let Some(tolerance) = input.amount_tolerance_minor {
            current.amount_tolerance_minor = tolerance;
        }
        if let Some(days) = input.proof_expiry_days {
            current.proof_expiry_days = days;
        }
        if let Some(required) = input.require_reference_number {
            current.require_reference_number = required;
        }
        Ok(current.clone())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: bank accounts.
// ---------------------------------------------------------------------------

async fn list_accounts(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<BankAccountView>> = (|| {
        let pagination = params.normalized();
        let mut items: Vec<BankAccountView> = accounts_store()
            .iter()
            .map(|entry| entry.value().view())
            .filter(|account| {
                matches_search(
                    &format!("{} {}", account.bank_name, account.account_holder),
                    &params,
                ) && params.filters.get("currency").is_none_or(|value| {
                    account.currency.eq_ignore_ascii_case(value)
                }) && params.filters.get("active").is_none_or(|value| {
                    account.active == (*value == "true" || *value == "1")
                }) && params.filters.get("bank_name").is_none_or(|value| {
                    account.bank_name == *value
                })
            })
            .collect();
        let sort = params.allowed_sort(&["bank_name", "created_at"], "created_at");
        match sort.as_str() {
            "bank_name" => items.sort_by(|a, b| a.bank_name.cmp(&b.bank_name)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if pagination.order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

/// Active accounts for one currency, masked — the list a shopper
/// picks from at checkout.
async fn list_accounts_for_currency(
    ctx: web::Data<Arc<AppContext>>,
    currency: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<BankAccountView>> = (|| {
        let currency = validate_currency(&currency)?;
        let mut items: Vec<BankAccountView> = accounts_store()
            .iter()
            .map(|entry| entry.value().view())
            .filter(|account| account.active && account.currency == currency)
            .collect();
        items.sort_by(|a, b| a.bank_name.cmp(&b.bank_name));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_account(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateBankAccountRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BankAccountView> = (|| {
        let input = body.into_inner();
        if input.bank_name.trim().is_empty() {
            return Err(UnifiedError::bad_request("bank_name is required"));
        }
        if input.account_holder.trim().is_empty() {
            return Err(UnifiedError::bad_request("account_holder is required"));
        }
        let account_number = validate_account_number(&input.account_number)?;
        let swift_bic = match input.swift_bic {
            Some(value) if !value.trim().is_empty() => Some(validate_swift_bic(&value)?),
            _ => None,
        };
        let currency = validate_currency(&input.currency)?;
        let instructions = input
            .instructions
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let duplicate = accounts_store().iter().any(|entry| {
            normalize_account_number(&entry.value().account_number) == account_number
                && entry.value().currency == currency
        });
        if duplicate {
            return Err(UnifiedError::conflict(
                "A bank account with this account number already exists for this currency",
            ));
        }
        let now = Utc::now();
        let account = BankAccount {
            id: Uuid::now_v7(),
            bank_name: input.bank_name.trim().to_string(),
            account_holder: input.account_holder.trim().to_string(),
            account_number,
            swift_bic,
            currency,
            instructions,
            active: input.active,
            created_at: now,
            updated_at: now,
        };
        accounts_store().insert(account.id, account.clone());
        Ok(account.view())
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_account(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BankAccountView> =
        find_account(id.into_inner()).map(|account| account.view());
    json::respond(result, debug)
}

async fn update_account(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateBankAccountRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BankAccountView> = (|| {
        let id = id.into_inner();
        let input = body.into_inner();
        let mut account = find_account(id)?;
        if let Some(bank_name) = input.bank_name {
            if bank_name.trim().is_empty() {
                return Err(UnifiedError::bad_request("bank_name must not be empty"));
            }
            account.bank_name = bank_name.trim().to_string();
        }
        if let Some(holder) = input.account_holder {
            if holder.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "account_holder must not be empty",
                ));
            }
            account.account_holder = holder.trim().to_string();
        }
        if let Some(number) = input.account_number {
            let normalized = validate_account_number(&number)?;
            let duplicate = accounts_store().iter().any(|entry| {
                entry.value().id != id
                    && normalize_account_number(&entry.value().account_number) == normalized
                    && entry.value().currency == account.currency
            });
            if duplicate {
                return Err(UnifiedError::conflict(
                    "A bank account with this account number already exists for this currency",
                ));
            }
            account.account_number = normalized;
        }
        if let Some(swift_bic) = input.swift_bic {
            account.swift_bic = if swift_bic.trim().is_empty() {
                None
            } else {
                Some(validate_swift_bic(&swift_bic)?)
            };
        }
        if let Some(instructions) = input.instructions {
            account.instructions = if instructions.trim().is_empty() {
                None
            } else {
                Some(instructions.trim().to_string())
            };
        }
        if let Some(active) = input.active {
            account.active = active;
        }
        account.updated_at = Utc::now();
        accounts_store().insert(id, account.clone());
        Ok(account.view())
    })();
    json::respond(result, debug)
}

async fn delete_account(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let _account = find_account(id)?;
        let referenced = verifications_store().iter().any(|entry| {
            entry.value().bank_account_id == Some(id)
                && (entry.value().status == STATUS_AWAITING_PROOF
                    || entry.value().status == STATUS_SUBMITTED)
        });
        if referenced {
            return Err(UnifiedError::conflict(
                "Bank account cannot be deleted while open transfer verifications reference it; deactivate it instead",
            ));
        }
        accounts_store().remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: transfer verifications.
// ---------------------------------------------------------------------------

async fn list_verifications(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TransferVerification>> = (|| {
        let pagination = params.normalized();
        let mut items: Vec<TransferVerification> = verifications_store()
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|verification| {
                matches_search(
                    &format!(
                        "{} {} {}",
                        verification.order_id,
                        verification.customer_id,
                        verification
                            .proof
                            .as_ref()
                            .and_then(|proof| proof.reference_number.clone())
                            .unwrap_or_default()
                    ),
                    &params,
                ) && params.filters.get("status").is_none_or(|value| {
                    verification.status == *value
                }) && params.filters.get("order_id").is_none_or(|value| {
                    verification.order_id == *value
                }) && params.filters.get("customer_id").is_none_or(|value| {
                    verification.customer_id == *value
                }) && params.filters.get("bank_account_id").is_none_or(|value| {
                    verification
                        .bank_account_id
                        .map(|id| id.to_string())
                        .as_deref()
                        == Some(value.as_str())
                }) && params.filters.get("split_id").is_none_or(|value| {
                    verification.split_id.map(|id| id.to_string()).as_deref()
                        == Some(value.as_str())
                }) && params.filters.get("currency").is_none_or(|value| {
                    verification.currency.eq_ignore_ascii_case(value)
                })
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "status"], "created_at");
        match sort.as_str() {
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if pagination.order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_verification(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
    body: web::Json<CreateVerificationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let input = body.into_inner();
        if input.order_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("order_id is required"));
        }
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        let currency = validate_currency(&input.currency)?;
        if let Some(expected) = input.expected_amount_minor {
            if expected <= 0 {
                return Err(UnifiedError::bad_request(
                    "expected_amount_minor must be greater than zero",
                ));
            }
        }
        let verification = insert_verification(
            NewVerification {
                order_id: input.order_id.trim().to_string(),
                invoice_id: input
                    .invoice_id
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                customer_id: input.customer_id.trim().to_string(),
                split_id: input.split_id,
                part_id: input.part_id,
                bank_account_id: input.bank_account_id,
                expected_amount_minor: input.expected_amount_minor,
                currency,
            },
            "system",
        )?;
        state.emit(
            BANK_TRANSFER_VERIFICATION_CREATED,
            json!({
                "verification_id": verification.id,
                "order_id": verification.order_id.clone(),
                "customer_id": verification.customer_id.clone(),
                "status": verification.status.clone(),
            }),
        );
        Ok(verification_detail(&verification))
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_verification(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let verification = find_verification(id.into_inner())?;
        if !caller_owns(&req, &verification.customer_id) {
            return Err(UnifiedError::forbidden(
                "Verification belongs to a different customer",
            ));
        }
        Ok(verification_detail(&verification))
    })();
    json::respond(result, debug)
}

async fn submit_proof(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<SubmitProofRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let id = id.into_inner();
        let input = body.into_inner();
        let mut verification = find_verification(id)?;
        if verification.status != STATUS_AWAITING_PROOF {
            return Err(UnifiedError::conflict(format!(
                "Verification is {} and cannot accept proof",
                verification.status
            )));
        }
        // Only the customer who owns this verification may upload
        // its proof: otherwise anyone could attach a proof to
        // another customer's order.
        require_verification_owner(&req, &verification)?;
        let actor = actor_of(&req, input.actor.clone());
        let storage_key = validate_storage_key(&input.storage_key)?;
        let checksum = match input.checksum {
            Some(value) if !value.trim().is_empty() => {
                let checksum = value.trim().to_string();
                if checksum.len() < 32
                    || checksum.len() > 128
                    || !checksum.chars().all(|c| c.is_ascii_hexdigit())
                {
                    return Err(UnifiedError::bad_request(
                        "Proof checksum must be a hex digest of 32 to 128 characters (for example SHA-256)",
                    ));
                }
                Some(checksum)
            }
            _ => None,
        };
        if input.amount_minor <= 0 {
            return Err(UnifiedError::bad_request(
                "Proof amount must be greater than zero (integer minor units)",
            ));
        }
        let currency = validate_currency(&input.currency)?;
        if currency != verification.currency {
            return Err(UnifiedError::unprocessable(format!(
                "Proof currency {currency} does not match the verification currency {}",
                verification.currency
            )));
        }
        let transfer_date = validate_transfer_date(&input.transfer_date)?;
        let settings = settings();
        let reference_number = input
            .reference_number
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if settings.require_reference_number && reference_number.is_none() {
            return Err(UnifiedError::bad_request(
                "A bank reference number is required for the payment proof",
            ));
        }
        if let Some(reference) = &reference_number {
            if reference.len() > 64 {
                return Err(UnifiedError::bad_request(
                    "Reference number must be at most 64 characters",
                ));
            }
        }
        let bank_account_id = input.bank_account_id.or(verification.bank_account_id);
        let bank_account_id = bank_account_id.ok_or_else(|| {
            UnifiedError::bad_request(
                "bank_account_id is required: the account the money was transferred to",
            )
        })?;
        let account = find_account(bank_account_id)?;
        if !account.active {
            return Err(UnifiedError::unprocessable(
                "The selected bank account is not active",
            ));
        }
        // Amount check against the expected total, within the
        // configured tolerance. A mismatch flags the verification
        // instead of blocking it: the administrator can still
        // approve it with a reason.
        let (amount_check, amount_difference) = match verification.expected_amount_minor {
            Some(expected) => {
                let difference = input.amount_minor - expected;
                if difference.abs() <= settings.amount_tolerance_minor {
                    ("matched".to_string(), Some(difference))
                } else {
                    ("mismatch".to_string(), Some(difference))
                }
            }
            None => ("skipped_no_expected_amount".to_string(), None),
        };
        let proof = TransferProof {
            storage_key: storage_key.clone(),
            checksum,
            amount_minor: input.amount_minor,
            currency: currency.clone(),
            transfer_date,
            reference_number,
            note: input
                .note
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            submitted_by: actor.clone(),
            submitted_at: Utc::now(),
        };
        verification.proof = Some(proof);
        verification.bank_account_id = Some(bank_account_id);
        verification.status = STATUS_SUBMITTED.to_string();
        verification.amount_check = amount_check.clone();
        verification.amount_difference_minor = amount_difference;
        verification.updated_at = Utc::now();
        verifications_store().insert(id, verification.clone());
        audit(
            id,
            "proof_submitted",
            &actor,
            "customer",
            None,
        );
        state.emit(
            BANK_TRANSFER_PROOF_SUBMITTED,
            json!({
                "verification_id": id,
                "order_id": verification.order_id,
                "customer_id": verification.customer_id,
                "bank_account_id": bank_account_id,
                "amount_minor": input.amount_minor,
                "currency": currency.clone(),
                "amount_check": amount_check,
                "amount_difference_minor": amount_difference,
                "split_id": verification.split_id,
                "part_id": verification.part_id,
            }),
        );
        notify(
            &state,
            "admin",
            "bank_transfer_proof_submitted",
            "Payment proof submitted",
            &format!(
                "Order {} has a bank transfer proof of {} {} to verify (amount check: {amount_check}).",
                verification.order_id, input.amount_minor, currency
            ),
        );
        Ok(verification_detail(&verification))
    })();
    json::respond(result, debug)
}

async fn verify_transfer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<VerifyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let id = id.into_inner();
        let input = body.into_inner();
        let mut verification = find_verification(id)?;
        if verification.status != STATUS_SUBMITTED {
            return Err(UnifiedError::conflict(format!(
                "Only a submitted verification can be verified (status is \"{}\")",
                verification.status
            )));
        }
        // The customer must never approve their own transfer.
        forbid_self_approval(&req, &verification)?;
        let actor = actor_of(&req, input.actor.clone());
        let reason = input
            .reason
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let proof = verification.proof.clone().ok_or_else(|| {
            UnifiedError::internal("Submitted verification has no proof attached")
        })?;
        // The administrator approval IS the capture: there is no
        // provider API to call. The transaction id is the record the
        // store, billing, and licensing wiring keys on.
        let transaction_id = input
            .transaction_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("bt-{}", Uuid::now_v7()));
        verification.status = STATUS_APPROVED.to_string();
        verification.transaction_id = Some(transaction_id.clone());
        verification.verified_by = Some(actor.clone());
        verification.verified_at = Some(Utc::now());
        verification.updated_at = Utc::now();
        verifications_store().insert(id, verification.clone());
        audit(
            id,
            "verified",
            &actor,
            "admin",
            reason.clone(),
        );
        state.emit(
            BANK_TRANSFER_VERIFIED,
            json!({
                "verification_id": id,
                "order_id": verification.order_id.clone(),
                "customer_id": verification.customer_id.clone(),
                "transaction_id": transaction_id.clone(),
                "amount_minor": proof.amount_minor,
                "currency": proof.currency.clone(),
                "amount_check": verification.amount_check.clone(),
                "split_id": verification.split_id,
                "part_id": verification.part_id,
            }),
        );
        // Capture event: same shape the gateway emits, so the
        // store / billing / licensing wiring picks the payment up
        // exactly like a gateway payment.
        state.emit(
            "payment.captured",
            json!({
                "transaction_id": transaction_id.clone(),
                "provider_transaction_id": transaction_id,
                "provider": "bank_transfer",
                "customer_id": verification.customer_id.clone(),
                "order_id": verification.order_id.clone(),
                "invoice_id": verification.invoice_id.clone(),
                "amount_minor": proof.amount_minor,
                "currency": proof.currency.clone(),
                "fee_minor": Value::Null,
                "status": "captured",
                "verification_id": id,
                "split_id": verification.split_id,
                "part_id": verification.part_id,
                "amount_mismatch": verification.amount_check == "mismatch",
            }),
        );
        notify(
            &state,
            &verification.customer_id,
            "bank_transfer_verified",
            "Bank transfer verified",
            &format!(
                "Your bank transfer of {} {} for order {} has been verified.",
                proof.amount_minor, proof.currency, verification.order_id
            ),
        );
        Ok(verification_detail(&verification))
    })();
    json::respond(result, debug)
}

async fn reject_transfer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<RejectRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let id = id.into_inner();
        let input = body.into_inner();
        let mut verification = find_verification(id)?;
        if verification.status != STATUS_SUBMITTED {
            return Err(UnifiedError::conflict(format!(
                "Only a submitted verification can be rejected (status is \"{}\")",
                verification.status
            )));
        }
        // The customer must never reject their own transfer either:
        // rejection cancels the store order.
        forbid_self_approval(&req, &verification)?;
        let actor = actor_of(&req, input.actor.clone());
        let reason = input.reason.trim().to_string();
        if reason.is_empty() {
            return Err(UnifiedError::bad_request(
                "A rejection reason is required",
            ));
        }
        verification.status = STATUS_REJECTED.to_string();
        verification.rejection_reason = Some(reason.clone());
        verification.updated_at = Utc::now();
        verifications_store().insert(id, verification.clone());
        audit(
            id,
            "rejected",
            &actor,
            "admin",
            Some(reason.clone()),
        );
        state.emit(
            BANK_TRANSFER_REJECTED,
            json!({
                "verification_id": id,
                "order_id": verification.order_id.clone(),
                "customer_id": verification.customer_id.clone(),
                "reason": reason.clone(),
                "split_id": verification.split_id,
                "part_id": verification.part_id,
            }),
        );
        // Failure event: the store cancels the order on rejection
        // (the `pending_verification` to `pending` transition stays
        // available in the store's status machine for an administrator
        // who wants to put the order back to awaiting payment).
        state.emit(
            "payment.failed",
            json!({
                "provider": "bank_transfer",
                "customer_id": verification.customer_id.clone(),
                "order_id": verification.order_id.clone(),
                "invoice_id": verification.invoice_id.clone(),
                "reason": reason,
                "verification_id": id,
                "split_id": verification.split_id,
                "part_id": verification.part_id,
            }),
        );
        notify(
            &state,
            &verification.customer_id,
            "bank_transfer_rejected",
            "Bank transfer rejected",
            &format!(
                "Your bank transfer for order {} was rejected: {reason}. Please check the transfer details and try again.",
                verification.order_id
            ),
        );
        Ok(verification_detail(&verification))
    })();
    json::respond(result, debug)
}

async fn cancel_verification(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BankTransferState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: Option<web::Json<CancelVerificationRequest>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerificationDetailView> = (|| {
        let id = id.into_inner();
        let input = body.and_then(|body| body.into_inner());
        let mut verification = find_verification(id)?;
        if verification.status != STATUS_AWAITING_PROOF
            && verification.status != STATUS_SUBMITTED
        {
            return Err(UnifiedError::conflict(format!(
                "Verification is {} and cannot be cancelled",
                verification.status
            )));
        }
        // Only the owning customer may cancel their verification.
        require_verification_owner(&req, &verification)?;
        let actor = actor_of(
            &req,
            input.as_ref().and_then(|input| input.actor.clone()),
        );
        let reason = input
            .and_then(|input| input.reason)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        verification.status = STATUS_CANCELLED.to_string();
        verification.updated_at = Utc::now();
        verifications_store().insert(id, verification.clone());
        audit(id, "cancelled", &actor, "customer", reason.clone());
        state.emit(
            BANK_TRANSFER_CANCELLED,
            json!({
                "verification_id": id,
                "order_id": verification.order_id.clone(),
                "customer_id": verification.customer_id.clone(),
                "reason": reason,
            }),
        );
        notify(
            &state,
            &verification.customer_id,
            "bank_transfer_cancelled",
            "Bank transfer cancelled",
            &format!(
                "The bank transfer verification for order {} was cancelled.",
                verification.order_id
            ),
        );
        Ok(verification_detail(&verification))
    })();
    json::respond(result, debug)
}

async fn list_audit(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<VerificationAuditEntry>> = (|| {
        let id = id.into_inner();
        let _verification = find_verification(id)?;
        let pagination = params.normalized();
        let mut items: Vec<VerificationAuditEntry> = audit_store()
            .iter()
            .filter(|entry| entry.value().verification_id == id)
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                matches_search(&format!("{} {}", entry.action, entry.actor), &params)
                    && params
                        .filters
                        .get("action")
                        .is_none_or(|value| entry.action == *value)
                    && params.filters.get("actor_role").is_none_or(|value| {
                        entry.actor_role == *value
                    })
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if pagination.order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct BankTransferModule {
    state: Arc<BankTransferState>,
}

impl BankTransferModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(BankTransferState::new()),
        }
    }
}

impl Default for BankTransferModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for BankTransferModule {
    fn id(&self) -> &'static str {
        "bank_transfer"
    }

    fn name(&self) -> &'static str {
        "Bank Transfer"
    }

    fn description(&self) -> &'static str {
        "Manual bank transfer payments: admin-configured bank accounts per currency, customer proof upload with amount matching, administrator verification with a full audit trail, and split-bill part verification. Approval emits payment.captured with provider bank_transfer."
    }

    fn requires(&self) -> &[&'static str] {
        &["payments", "store", "billing"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["serde", "serde_json", "chrono", "uuid", "dashmap"]
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
            "payment.initiated",
            "bank_transfer",
            Arc::new(move |ctx: &mut HookContext| on_payment_initiated(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "bank_transfer.proof.requested",
            "bank_transfer",
            Arc::new(move |ctx: &mut HookContext| on_proof_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/bank-transfer")
                .route("", web::get().to(get_status))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/accounts", web::get().to(list_accounts))
                .route("/accounts", web::post().to(create_account))
                .route("/accounts/currency/{currency}", web::get().to(list_accounts_for_currency))
                .route("/accounts/{id}", web::get().to(get_account))
                .route("/accounts/{id}", web::put().to(update_account))
                .route("/accounts/{id}", web::delete().to(delete_account))
                .route("/verifications", web::get().to(list_verifications))
                .route("/verifications", web::post().to(create_verification))
                .route("/verifications/{id}", web::get().to(get_verification))
                .route("/verifications/{id}/proof", web::post().to(submit_proof))
                .route("/verifications/{id}/verify", web::post().to(verify_transfer))
                .route("/verifications/{id}/reject", web::post().to(reject_transfer))
                .route("/verifications/{id}/cancel", web::post().to(cancel_verification))
                .route("/verifications/{id}/audit", web::get().to(list_audit)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer", owner: "bank_transfer", description: "Module status: account counts and verifications by status" },
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/settings", owner: "bank_transfer", description: "Read manual verification settings (amount tolerance, proof expiry, reference requirement)" },
            RouteInfo { method: "PUT", path: "/api/v1/bank-transfer/settings", owner: "bank_transfer", description: "Update manual verification settings" },
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/accounts", owner: "bank_transfer", description: "List bank accounts (numbers masked)" },
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/accounts", owner: "bank_transfer", description: "Create a bank account for one currency" },
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/accounts/currency/{currency}", owner: "bank_transfer", description: "List active bank accounts for one currency (checkout picker)" },
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/accounts/{id}", owner: "bank_transfer", description: "Get one bank account (number masked)" },
            RouteInfo { method: "PUT", path: "/api/v1/bank-transfer/accounts/{id}", owner: "bank_transfer", description: "Update a bank account" },
            RouteInfo { method: "DELETE", path: "/api/v1/bank-transfer/accounts/{id}", owner: "bank_transfer", description: "Delete a bank account (refused while open verifications reference it)" },
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/verifications", owner: "bank_transfer", description: "List transfer verifications with pagination and filters" },
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/verifications", owner: "bank_transfer", description: "Open a transfer verification for an order (or a split part)" },
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/verifications/{id}", owner: "bank_transfer", description: "Get one verification with masked account and full audit trail" },
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/verifications/{id}/proof", owner: "bank_transfer", description: "Submit payment proof (file reference, amount, transfer date, reference number)" },
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/verifications/{id}/verify", owner: "bank_transfer", description: "Approve a transfer (emits payment.captured with provider bank_transfer)" },
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/verifications/{id}/reject", owner: "bank_transfer", description: "Reject a transfer with a required reason (emits payment.failed, notifies the customer)" },
            RouteInfo { method: "POST", path: "/api/v1/bank-transfer/verifications/{id}/cancel", owner: "bank_transfer", description: "Cancel an open verification" },
            RouteInfo { method: "GET", path: "/api/v1/bank-transfer/verifications/{id}/audit", owner: "bank_transfer", description: "List the audit trail of one verification" },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(BankTransferModule::new()));
}
