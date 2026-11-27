//! eSignature external module.

use super::*;


/// Lifecycle of a signature request. Expiry is computed on read:
/// a `sent` or `partially_signed` request whose `expires_at` has
/// passed reports (and is stored as) `expired`; a request is never
/// silently auto-completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Draft,
    Sent,
    PartiallySigned,
    Completed,
    Declined,
    Expired,
}

/// Lifecycle of one signer on a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignerStatus {
    Pending,
    Signed,
    Declined,
}

/// One signature record: the evidence captured at signing time. The
/// drawn signature itself is never sent to this module; the client
/// sends only the SHA-256 hex of the drawn signature bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureRecord {
    pub typed_name: String,
    pub signature_hash: String,
    pub ip: String,
    pub user_agent: String,
    pub signed_at: DateTime<Utc>,
}

/// One signer on a signature request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signer {
    pub id: String,
    pub name: String,
    pub email: String,
    pub role: String,
    pub order: u32,
    pub status: SignerStatus,
    pub signed_at: Option<DateTime<Utc>>,
    pub decline_reason: Option<String>,
    pub signature: Option<SignatureRecord>,
}

/// A signature request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureRequest {
    pub id: String,
    pub tenant_id: String,
    pub document_id: String,
    pub document_sha256: String,
    pub title: String,
    pub status: RequestStatus,
    /// Id of the contracts-module contract this request belongs to,
    /// when known. Set at creation or linked later from the
    /// `contracts.signed` hook.
    pub contract_id: Option<String>,
    pub signers: Vec<Signer>,
    pub expires_at: DateTime<Utc>,
    pub callback_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub certificate_hash: Option<String>,
}

/// One hash-chained audit event. `hash` is the SHA-256 of
/// `prev_hash|id|request_id|kind|actor|created_at|details`; the
/// chain is verified by recomputing every hash from the genesis
/// hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: String,
    pub request_id: String,
    pub kind: String,
    pub actor: String,
    pub details: String,
    pub prev_hash: String,
    pub hash: String,
    pub created_at: DateTime<Utc>,
}

/// One signer as recorded on a completion certificate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateSigner {
    pub signer_id: String,
    pub name: String,
    pub email: String,
    pub role: String,
    pub order: u32,
    pub signed_at: DateTime<Utc>,
    pub typed_name: String,
    pub signature_hash: String,
}

/// Certificate of completion for a fully signed request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Certificate {
    pub certificate_hash: String,
    pub request_id: String,
    pub tenant_id: String,
    pub document_id: String,
    pub document_sha256: String,
    pub title: String,
    pub signers: Vec<CertificateSigner>,
    pub audit_event_ids: Vec<String>,
    pub audit_head_hash: String,
    pub completed_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
}

/// One issued signing link, kept so the sender can re-list links and
/// so signing attempts can be audited. The token itself is
/// stateless (HMAC); this record is bookkeeping, not the source of
/// truth for validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigningLink {
    pub request_id: String,
    pub signer_id: String,
    pub email: String,
    pub signing_url: String,
    pub expires_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
}

/// Result of verifying an audit chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainVerification {
    pub valid: bool,
    pub events_checked: usize,
    pub broken_index: Option<usize>,
    pub reason: Option<String>,
}

/// Result of verifying a completion certificate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateVerification {
    pub valid: bool,
    pub certificate: Option<Certificate>,
}

// Input DTOs

#[derive(Debug, Clone, Deserialize)]
pub struct SignerInput {
    pub name: String,
    pub email: String,
    pub role: String,
    pub order: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRequestInput {
    pub tenant_id: String,
    pub document_id: String,
    pub document_sha256: String,
    pub title: String,
    pub signers: Vec<SignerInput>,
    pub expires_at: DateTime<Utc>,
    pub callback_url: Option<String>,
    pub contract_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignInput {
    pub typed_name: String,
    pub signature_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeclineInput {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TenantQuery {
    pub(crate) tenant_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListQuery {
    pub(crate) tenant_id: Option<String>,
    pub(crate) status: Option<String>,
}
