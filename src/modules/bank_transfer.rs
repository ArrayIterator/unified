// Bank Transfer module - manual payment verification.
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
//      reference is stored - never raw file bytes. The amount is
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
// payload. Secrets do not exist here - there is no provider API and
// therefore no credential to store.
//
// Persistence: records are kept in memory (same pattern as the
// provider registry and the store). Durable persistence is a
// follow-up for the integrator - see the migration sketch in the
// coordinator report.
//
// Events emitted: `bank_transfer.verification.created`,
// `bank_transfer.proof.submitted`, `bank_transfer.verified`,
// `bank_transfer.rejected`, `bank_transfer.cancelled`,
// `payment.captured` (on approval), `payment.failed` (on rejection),
// `notification.requested` (at every step). Events listened to:
// `payment.initiated` (provider `bank_transfer`),
// `bank_transfer.proof.requested` (split part payment).

pub mod ai_capabilities;
mod handlers;
pub mod module;
mod service;
pub mod state;
pub mod types;
mod validation;

pub(crate) use self::handlers::cancel_verification;
pub(crate) use self::handlers::create_account;
pub(crate) use self::handlers::create_verification;
pub(crate) use self::handlers::delete_account;
pub(crate) use self::handlers::get_account;
pub(crate) use self::handlers::get_settings;
pub(crate) use self::handlers::get_status;
pub(crate) use self::handlers::get_verification;
pub(crate) use self::handlers::list_accounts;
pub(crate) use self::handlers::list_accounts_for_currency;
pub(crate) use self::handlers::list_audit;
pub(crate) use self::handlers::list_verifications;
pub(crate) use self::handlers::reject_transfer;
pub(crate) use self::handlers::submit_proof;
pub(crate) use self::handlers::update_account;
pub(crate) use self::handlers::update_settings;
pub(crate) use self::handlers::verify_transfer;
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::BankTransferModule;
pub use self::module::register;
pub(crate) use self::service::actor_of;
pub(crate) use self::service::audit;
pub(crate) use self::service::caller_owns;
pub(crate) use self::service::find_account;
pub(crate) use self::service::find_verification;
pub(crate) use self::service::forbid_self_approval;
pub(crate) use self::service::insert_verification;
pub(crate) use self::service::matches_search;
pub(crate) use self::service::notify;
pub(crate) use self::service::on_payment_initiated;
pub(crate) use self::service::on_proof_requested;
pub(crate) use self::service::paginate;
pub(crate) use self::service::require_verification_owner;
pub(crate) use self::service::verification_detail;
pub use self::state::BankTransferState;
pub(crate) use self::state::accounts_store;
pub(crate) use self::state::audit_store;
pub(crate) use self::state::settings;
pub(crate) use self::state::settings_store;
pub(crate) use self::state::verifications_store;
pub use self::types::BANK_TRANSFER_CANCELLED;
pub use self::types::BANK_TRANSFER_PROOF_SUBMITTED;
pub use self::types::BANK_TRANSFER_REJECTED;
pub use self::types::BANK_TRANSFER_VERIFICATION_CREATED;
pub use self::types::BANK_TRANSFER_VERIFIED;
pub use self::types::BankAccount;
pub use self::types::BankAccountView;
pub use self::types::BankTransferSettings;
pub use self::types::CancelVerificationRequest;
pub use self::types::CreateBankAccountRequest;
pub use self::types::CreateVerificationRequest;
pub(crate) use self::types::NewVerification;
pub use self::types::RejectRequest;
pub(crate) use self::types::STATUS_APPROVED;
pub(crate) use self::types::STATUS_AWAITING_PROOF;
pub(crate) use self::types::STATUS_CANCELLED;
pub(crate) use self::types::STATUS_REJECTED;
pub(crate) use self::types::STATUS_SUBMITTED;
pub use self::types::SubmitProofRequest;
pub use self::types::TransferProof;
pub use self::types::TransferVerification;
pub use self::types::UpdateBankAccountRequest;
pub use self::types::UpdateBankTransferSettings;
pub use self::types::VerificationAuditEntry;
pub use self::types::VerificationDetailView;
pub use self::types::VerifyRequest;
pub use self::types::mask_account_number;
pub(crate) use self::validation::major_to_minor;
pub(crate) use self::validation::normalize_account_number;
pub(crate) use self::validation::validate_account_number;
pub(crate) use self::validation::validate_currency;
pub(crate) use self::validation::validate_storage_key;
pub(crate) use self::validation::validate_swift_bic;
pub(crate) use self::validation::validate_transfer_date;
