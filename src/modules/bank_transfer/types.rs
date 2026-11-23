// bank_transfer — Shared types: constants, enums, records, and request/response shapes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
pub(crate) const STATUS_AWAITING_PROOF: &str = "awaiting_proof";

pub(crate) const STATUS_SUBMITTED: &str = "submitted";

pub(crate) const STATUS_APPROVED: &str = "approved";

pub(crate) const STATUS_REJECTED: &str = "rejected";

pub(crate) const STATUS_CANCELLED: &str = "cancelled";

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

/// Last four characters of an account number / IBAN, for display.
/// A number of four characters or fewer has no characters to hide
/// behind a mask, so the full value is never returned: the masked
/// placeholder is shown instead (validation accepts 4-character
/// numbers, so returning the value itself would leak it in full).
fn last4(value: &str) -> String {
    let compact: Vec<char> = value.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() <= 4 {
        return "****".to_string();
    }
    compact[compact.len() - 4..].iter().collect()
}

impl BankAccount {
    pub(crate) fn view(&self) -> BankAccountView {
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
// Verification creation (shared by the HTTP handler and the hook
// listeners).
// ---------------------------------------------------------------------------

pub(crate) struct NewVerification {
    pub(crate) order_id: String,
    pub(crate) invoice_id: Option<String>,
    pub(crate) customer_id: String,
    pub(crate) split_id: Option<Uuid>,
    pub(crate) part_id: Option<Uuid>,
    pub(crate) bank_account_id: Option<Uuid>,
    pub(crate) expected_amount_minor: Option<i64>,
    pub(crate) currency: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_account_number_and_last4() {
        assert_eq!(mask_account_number("12345678"), "****5678");
        assert_eq!(mask_account_number("DE89 3704"), "****3704");
        assert_eq!(mask_account_number("123"), "***");
        assert_eq!(mask_account_number(""), "*");
        assert_eq!(last4("12345678"), "5678");
        // Four characters or fewer: the full number must never be
        // returned (validation accepts 4-character numbers).
        assert_eq!(last4("1234"), "****");
        assert_eq!(last4("12"), "****");
        assert_eq!(last4(""), "****");
        assert_eq!(last4("12345"), "2345");
    }

    #[test]
    fn bank_account_view_masks_number() {
        let now = Utc::now();
        let account = BankAccount {
            id: Uuid::new_v4(),
            bank_name: "Example Bank".to_string(),
            account_holder: "Ada Lovelace".to_string(),
            account_number: "12345678".to_string(),
            swift_bic: Some("EXBKDEFF".to_string()),
            currency: "USD".to_string(),
            instructions: None,
            active: true,
            created_at: now,
            updated_at: now,
        };
        let view = account.view();
        assert_eq!(view.account_number_masked, "****5678");
        assert_eq!(view.account_number_last4, "5678");
        assert_eq!(view.bank_name, "Example Bank");
        assert!(view.active);
    }

    #[test]
    fn bank_account_view_masks_a_four_character_number() {
        let now = Utc::now();
        let account = BankAccount {
            id: Uuid::new_v4(),
            bank_name: "Example Bank".to_string(),
            account_holder: "Ada Lovelace".to_string(),
            account_number: "1234".to_string(),
            swift_bic: None,
            currency: "USD".to_string(),
            instructions: None,
            active: true,
            created_at: now,
            updated_at: now,
        };
        let view = account.view();
        assert_eq!(view.account_number_masked, "****");
        assert_eq!(view.account_number_last4, "****");
    }
}
