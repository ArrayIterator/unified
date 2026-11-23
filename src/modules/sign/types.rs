//! Sign types: envelopes (signing requests) with multiple
//! documents, signers with hashed signing tokens, signature
//! capture with tamper evidence, reusable templates,
//! data-driven reminder schedules, hash-chained audit entries,
//! and completion certificates.
//!
//! Documents are linked by id only (no foreign key — documents
//! live in the documents module and this module must not assume
//! their tables or migration order). Signature bytes are never
//! stored or sent: the client sends only the SHA-256 hash of the
//! drawn strokes, the rendered typed name, or the uploaded image
//! bytes, plus the SHA-256 hash of the document as displayed, so
//! the stored hashes prove afterwards what was signed. Money
//! does not exist in this module: there are no money fields, so
//! there is no floating point money anywhere here.

use chrono::{DateTime, Duration, Utc};
use rand::{Rng, distr::Alphanumeric};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_MESSAGE_LEN: usize = 5000;

pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_EMAIL_LEN: usize = 254;

pub(crate) const MAX_ROLE_LEN: usize = 128;

pub(crate) const MAX_REASON_LEN: usize = 2000;

pub(crate) const MAX_DOCUMENT_ID_LEN: usize = 200;

pub(crate) const MAX_FIELD_KEY_LEN: usize = 64;

pub(crate) const MAX_FIELD_LABEL_LEN: usize = 200;

/// Maximum signers on one envelope (same bound as the native
/// external e-signature crate).
pub(crate) const MAX_SIGNERS_PER_ENVELOPE: usize = 50;

/// Maximum documents on one envelope.
pub(crate) const MAX_DOCUMENTS_PER_ENVELOPE: usize = 20;

/// Maximum fields on one template.
pub(crate) const MAX_FIELDS_PER_TEMPLATE: usize = 100;

/// Maximum signer roles on one template.
pub(crate) const MAX_ROLES_PER_TEMPLATE: usize = 50;

/// Signing token: 48 random alphanumeric characters
/// (~285 bits), following the contracts module pattern. The raw
/// token is returned exactly once (at send / resend / add) and
/// is never stored — only its SHA-256 hash.
pub(crate) const SIGNING_TOKEN_LEN: usize = 48;

/// Default signing-token lifetime in days.
pub(crate) const DEFAULT_TOKEN_TTL_DAYS: i64 = 30;

/// Default envelope expiry in days when the caller sets none.
pub(crate) const DEFAULT_EXPIRY_DAYS: i64 = 30;

/// Default first-reminder delay in days after sending.
pub(crate) const DEFAULT_REMINDER_START_AFTER_DAYS: i64 = 3;

/// Default reminder interval in days.
pub(crate) const DEFAULT_REMINDER_INTERVAL_DAYS: i64 = 7;

/// Genesis previous-hash for the first audit entry of an
/// envelope: 64 zeroes, the same shape as a SHA-256 hex digest.
pub(crate) const GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_SIGN_VIEW: &str = "sign.view";

pub(crate) const PERM_SIGN_MANAGE: &str = "sign.manage";

/// Hook event names this module emits.
pub(crate) const EVENT_ENVELOPE_CREATED: &str = "sign.envelope.created";

pub(crate) const EVENT_ENVELOPE_COMPLETED: &str = "sign.envelope.completed";

pub(crate) const EVENT_ENVELOPE_DECLINED: &str = "sign.envelope.declined";

pub(crate) const EVENT_ENVELOPE_CANCELLED: &str = "sign.envelope.cancelled";

pub(crate) const EVENT_ENVELOPE_VOIDED: &str = "sign.envelope.voided";

pub(crate) const EVENT_ENVELOPE_EXPIRED: &str = "sign.envelope.expired";

pub(crate) const EVENT_SIGNER_SIGNED: &str = "sign.signer.signed";

pub(crate) const EVENT_SIGNER_RESENT: &str = "sign.signer.resent";

pub(crate) const EVENT_REMINDER_DUE: &str = "sign.reminder.due";

/// Hook event names this module listens to.
pub(crate) const EVENT_DOCUMENT_DELETED: &str = "documents.deleted";

pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

/// Marker written into signer `name` / `email` and envelope
/// `created_by` when the owning user is deleted. Rows are kept
/// (the signing history stays auditable); only the identity is
/// anonymized.
pub(crate) const DELETED_USER_NAME: &str = "Deleted user";

pub(crate) const DELETED_USER_ID: &str = "deleted-user";

/// Signature capture methods. The method names are the
/// module's capture kinds; which of them a tenant allows is
/// data (`SignSettings.signature_types`).
pub(crate) const SIGNATURE_METHOD_DRAWN: &str = "drawn";

pub(crate) const SIGNATURE_METHOD_TYPED: &str = "typed";

pub(crate) const SIGNATURE_METHOD_UPLOADED: &str = "uploaded";

// ---------------------------------------------------------------------------
// Pure domain functions (unit-tested below)
// ---------------------------------------------------------------------------

/// SHA-256 hex digest. Used for signing-token storage, signature
/// hashes, document hashes, and IP / user-agent hashing — the
/// raw value is never stored or logged.
pub fn hash_value(value: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

/// One unguessable signing token (48 random alphanumeric
/// characters). Only its SHA-256 hash is ever stored.
pub fn generate_signing_token() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(SIGNING_TOKEN_LEN)
        .map(char::from)
        .collect()
}

/// Email validation: one `@`, non-empty local and domain parts,
/// a dot in the domain, no spaces.
pub fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// True when `value` is a 64-character lowercase-or-mixed hex
/// SHA-256 digest.
pub fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// True when the signing order value is one of the two orders
/// this module understands. The order itself is data on the
/// envelope; an unknown value is rejected rather than guessed.
pub fn valid_signing_order(value: &str) -> bool {
    matches!(value, "sequential" | "parallel")
}

/// True when the signature method is one of the three capture
/// kinds this module records.
pub fn valid_signature_method(value: &str) -> bool {
    matches!(
        value,
        SIGNATURE_METHOD_DRAWN | SIGNATURE_METHOD_TYPED | SIGNATURE_METHOD_UPLOADED
    )
}

/// Envelope status machine. Allowed transitions:
/// draft -> sent, cancelled, voided;
/// sent -> partially_signed, completed, declined, expired,
/// cancelled, voided;
/// partially_signed -> completed, declined, expired, cancelled,
/// voided. Completed, declined, expired, cancelled, and voided
/// are terminal. Anything else is rejected.
pub fn envelope_can_transition(from: EnvelopeStatus, to: EnvelopeStatus) -> bool {
    use EnvelopeStatus::{
        Cancelled, Completed, Declined, Draft, Expired, PartiallySigned, Sent, Voided,
    };
    matches!(
        (from, to),
        (Draft, Sent)
            | (Draft, Cancelled)
            | (Draft, Voided)
            | (Sent, PartiallySigned)
            | (Sent, Completed)
            | (Sent, Declined)
            | (Sent, Expired)
            | (Sent, Cancelled)
            | (Sent, Voided)
            | (PartiallySigned, Completed)
            | (PartiallySigned, Declined)
            | (PartiallySigned, Expired)
            | (PartiallySigned, Cancelled)
            | (PartiallySigned, Voided)
    )
}

/// Signer status machine. Allowed transitions:
/// pending -> sent, viewed, signed, declined;
/// sent -> viewed, signed, declined;
/// viewed -> signed, declined. Signed and declined are
/// terminal. A resend moves a sent or viewed signer back to
/// sent with a fresh token.
pub fn signer_can_transition(from: SignerStatus, to: SignerStatus) -> bool {
    use SignerStatus::{Declined, Pending, Sent, Signed, Viewed};
    matches!(
        (from, to),
        (Pending, Sent)
            | (Pending, Viewed)
            | (Pending, Signed)
            | (Pending, Declined)
            | (Sent, Viewed)
            | (Sent, Signed)
            | (Sent, Declined)
            | (Sent, Sent)
            | (Viewed, Signed)
            | (Viewed, Declined)
            | (Viewed, Sent)
    )
}

/// Effective envelope status at `now`: a sent or
/// partially-signed envelope whose expiry has passed reads as
/// expired. Every other status reads as itself, and an envelope
/// with no expiry never expires.
pub fn effective_envelope_status(
    status: EnvelopeStatus,
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> EnvelopeStatus {
    match status {
        EnvelopeStatus::Sent | EnvelopeStatus::PartiallySigned => {
            if let Some(expires_at) = expires_at {
                if now > expires_at {
                    return EnvelopeStatus::Expired;
                }
            }
            status
        }
        other => other,
    }
}

/// True when every signer in the list has signed. An envelope
/// with no signers never counts as complete.
pub fn all_signers_signed(signers: &[SignSigner]) -> bool {
    !signers.is_empty()
        && signers
            .iter()
            .all(|signer| signer.status == SignerStatus::Signed)
}

/// True when `signer` may sign now. In parallel order any
/// pending signer may sign; in sequential order every signer
/// with a lower `order_index` must already have signed. A
/// signer who already signed or declined may never sign again.
pub fn signer_may_sign(signers: &[SignSigner], signer: &SignSigner, signing_order: &str) -> bool {
    if !matches!(
        signer.status,
        SignerStatus::Pending | SignerStatus::Sent | SignerStatus::Viewed
    ) {
        return false;
    }
    if signing_order == "sequential" {
        return signers.iter().all(|other| {
            other.order_index >= signer.order_index || other.status == SignerStatus::Signed
        });
    }
    true
}

/// Next reminder time for an envelope, or `None` when reminders
/// are disabled for it (interval below one day, or the maximum
/// reminder count already reached). The first reminder goes out
/// `start_after_days` after sending; later ones go out
/// `interval_days` after the previous reminder.
pub fn next_reminder_at(
    schedule: &ReminderSchedule,
    sent_at: DateTime<Utc>,
    last_reminder_at: Option<DateTime<Utc>>,
    reminders_sent: i64,
) -> Option<DateTime<Utc>> {
    if schedule.interval_days < 1 {
        return None;
    }
    if let Some(max) = schedule.max_reminders {
        if reminders_sent >= max {
            return None;
        }
    }
    match last_reminder_at {
        Some(last) => Some(last + Duration::days(schedule.interval_days)),
        None => Some(sent_at + Duration::days(schedule.start_after_days.max(0))),
    }
}

/// Reminders still available under the schedule, or `None` when
/// the schedule is unlimited. Zero means the cap is reached.
pub fn reminders_remaining(schedule: &ReminderSchedule, reminders_sent: i64) -> Option<i64> {
    schedule
        .max_reminders
        .map(|max| (max - reminders_sent).max(0))
}

/// True when a reminder is due at `now`: the envelope was sent,
/// reminders are enabled, the next reminder time has passed,
/// and the cap (when set) has not been reached.
pub fn reminder_due(
    schedule: &ReminderSchedule,
    sent_at: DateTime<Utc>,
    last_reminder_at: Option<DateTime<Utc>>,
    reminders_sent: i64,
    now: DateTime<Utc>,
) -> bool {
    match next_reminder_at(schedule, sent_at, last_reminder_at, reminders_sent) {
        Some(next) => next <= now,
        None => false,
    }
}

/// Canonical input to one audit-entry hash: the previous entry
/// hash plus this entry's identity, action, actor, time, and
/// detail, joined with `|`.
pub fn audit_hash_input(
    prev_hash: &str,
    id: Uuid,
    envelope_id: Uuid,
    action: &str,
    actor: &str,
    created_at: DateTime<Utc>,
    detail: &str,
) -> String {
    format!(
        "{prev_hash}|{}|{}|{action}|{actor}|{}|{detail}",
        id,
        envelope_id,
        created_at.to_rfc3339(),
    )
}

/// SHA-256 hex of the canonical audit input.
pub fn compute_audit_hash(
    prev_hash: &str,
    id: Uuid,
    envelope_id: Uuid,
    action: &str,
    actor: &str,
    created_at: DateTime<Utc>,
    detail: &str,
) -> String {
    hash_value(&audit_hash_input(
        prev_hash,
        id,
        envelope_id,
        action,
        actor,
        created_at,
        detail,
    ))
}

/// Verify a hash-chained audit trail in insertion order.
/// Returns `(valid, broken_index)`: the index of the first
/// entry whose stored hash does not recompute (or whose
/// `prev_hash` does not match the previous entry's hash), or
/// `None` when the whole chain verifies.
pub fn verify_audit_chain(entries: &[SignAuditEntry]) -> (bool, Option<usize>) {
    let mut prev_hash = GENESIS_HASH.to_string();
    for (index, entry) in entries.iter().enumerate() {
        if entry.prev_hash != prev_hash {
            return (false, Some(index));
        }
        let recomputed = compute_audit_hash(
            &entry.prev_hash,
            entry.id,
            entry.envelope_id,
            &entry.action,
            &entry.actor_id,
            entry.created_at,
            entry.detail.as_deref().unwrap_or(""),
        );
        if recomputed != entry.hash {
            return (false, Some(index));
        }
        prev_hash = entry.hash.clone();
    }
    (true, None)
}

/// Certificate hash: SHA-256 over the envelope id, the sorted
/// document hashes, one line per signer (`signer id | email |
/// signature hash | signed at`), and the completion time.
/// Recomputing it from the stored rows proves the certificate
/// still matches the signing record.
pub fn compute_certificate_hash(
    envelope_id: Uuid,
    document_hashes: &[String],
    signers: &[SignSigner],
    completed_at: DateTime<Utc>,
) -> String {
    let mut sorted_hashes = document_hashes.to_vec();
    sorted_hashes.sort();
    let mut input = format!(
        "sign-certificate|{envelope_id}|{}|",
        sorted_hashes.join(",")
    );
    let mut signed: Vec<&SignSigner> = signers
        .iter()
        .filter(|signer| signer.status == SignerStatus::Signed)
        .collect();
    signed.sort_by_key(|signer| signer.order_index);
    for signer in signed {
        let signature_hash = signer
            .signature
            .as_ref()
            .map(|capture| capture.signature_hash.as_str())
            .unwrap_or("");
        let signed_at = signer
            .signed_at
            .map(|at| at.to_rfc3339())
            .unwrap_or_default();
        input.push_str(&format!(
            "{}|{}|{signature_hash}|{signed_at};",
            signer.id, signer.email,
        ));
    }
    input.push_str(&completed_at.to_rfc3339());
    hash_value(&input)
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Envelope (signing request) status. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeStatus {
    Draft,
    Sent,
    PartiallySigned,
    Completed,
    Declined,
    Expired,
    Cancelled,
    Voided,
}

impl Default for EnvelopeStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl EnvelopeStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Sent => "sent",
            Self::PartiallySigned => "partially_signed",
            Self::Completed => "completed",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
            Self::Voided => "voided",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "sent" => Some(Self::Sent),
            "partially_signed" => Some(Self::PartiallySigned),
            "completed" => Some(Self::Completed),
            "declined" => Some(Self::Declined),
            "expired" => Some(Self::Expired),
            "cancelled" => Some(Self::Cancelled),
            "voided" => Some(Self::Voided),
            _ => None,
        }
    }

    /// True for statuses no further transition leaves.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Declined | Self::Expired | Self::Cancelled | Self::Voided
        )
    }
}

/// Signer status. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignerStatus {
    Pending,
    Sent,
    Viewed,
    Signed,
    Declined,
}

impl Default for SignerStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl SignerStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Viewed => "viewed",
            Self::Signed => "signed",
            Self::Declined => "declined",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "sent" => Some(Self::Sent),
            "viewed" => Some(Self::Viewed),
            "signed" => Some(Self::Signed),
            "declined" => Some(Self::Declined),
            _ => None,
        }
    }
}

/// Reminder schedule, stored as data on the envelope (and as
/// the tenant default in settings). `interval_days` is the gap
/// between reminders; `max_reminders` caps the total when set
/// (`None` is unlimited); `start_after_days` is the delay
/// between sending and the first reminder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderSchedule {
    #[serde(default = "default_reminder_interval")]
    pub interval_days: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_reminders: Option<i64>,
    #[serde(default = "default_reminder_start_after")]
    pub start_after_days: i64,
}

fn default_reminder_interval() -> i64 {
    DEFAULT_REMINDER_INTERVAL_DAYS
}

fn default_reminder_start_after() -> i64 {
    DEFAULT_REMINDER_START_AFTER_DAYS
}

impl Default for ReminderSchedule {
    fn default() -> Self {
        Self {
            interval_days: DEFAULT_REMINDER_INTERVAL_DAYS,
            max_reminders: None,
            start_after_days: DEFAULT_REMINDER_START_AFTER_DAYS,
        }
    }
}

/// One envelope (signing request). `document_ids` reference
/// documents-module documents by id only (no foreign key);
/// `document_hashes` holds the SHA-256 of each document as
/// recorded at send time, aligned with `document_ids` by
/// position. `signing_order` is `sequential` or `parallel`.
/// `template_id` records the template this envelope was built
/// from, when there is one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignEnvelope {
    pub id: Uuid,
    pub tenant_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_hashes: Vec<String>,
    pub signing_order: String,
    pub status: EnvelopeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_hash: Option<String>,
    pub reminder_schedule: ReminderSchedule,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Evidence captured at signing time. Only hashes are stored:
/// `signature_hash` is the SHA-256 of the drawn strokes, the
/// rendered typed name, or the uploaded image bytes (the raw
/// bytes never reach this module); `document_hash` is the
/// SHA-256 of the document as the signer saw it, and must match
/// the hash recorded for that document on the envelope.
/// `ip_hash` / `user_agent_hash` are hashes, never raw values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureCapture {
    pub method: String,
    pub signature_hash: String,
    pub document_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typed_name: Option<String>,
    pub ip_hash: String,
    pub user_agent_hash: String,
    pub signed_at: DateTime<Utc>,
}

/// One signer on an envelope. `token_hash` holds the SHA-256 of
/// the current signing token (the raw token is returned once at
/// send / resend / add and never stored); it is empty while the
/// envelope is a draft. `document_id` on the capture names the
/// document the hash belongs to when several were signed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignSigner {
    pub id: Uuid,
    pub tenant_id: String,
    pub envelope_id: Uuid,
    pub name: String,
    pub email: String,
    pub role: String,
    pub order_index: i64,
    pub status: SignerStatus,
    #[serde(skip_serializing)]
    pub token_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declined_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decline_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SignatureCapture>,
    pub reminders_sent: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reminder_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One hash-chained audit entry. `hash` is the SHA-256 of
/// `prev_hash|id|envelope_id|action|actor|created_at|detail`;
/// the chain is verified by recomputing every hash from the
/// genesis hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignAuditEntry {
    pub id: Uuid,
    pub tenant_id: String,
    pub envelope_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_id: Option<Uuid>,
    pub action: String,
    pub actor_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub prev_hash: String,
    pub hash: String,
    pub created_at: DateTime<Utc>,
}

/// One signer as recorded on a completion certificate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateSigner {
    pub signer_id: Uuid,
    pub name: String,
    pub email: String,
    pub role: String,
    pub order_index: i64,
    pub signed_at: DateTime<Utc>,
    pub method: String,
    pub signature_hash: String,
    pub document_hash: String,
}

/// Certificate of completion for a fully signed envelope: the
/// hash-chain summary. `certificate_hash` is the SHA-256 of the
/// envelope id, document hashes, signer evidence, and
/// completion time; `audit_head_hash` is the hash of the last
/// audit entry, so the certificate is bound to the trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignCertificate {
    pub certificate_hash: String,
    pub envelope_id: Uuid,
    pub tenant_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_hashes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signers: Vec<CertificateSigner>,
    pub audit_head_hash: String,
    pub audit_event_count: i64,
    pub completed_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
}

/// One signer role on a template. `name` / `email` are defaults
/// filled in when an envelope is built from the template; when
/// they are empty the caller must supply the signer at use
/// time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateSignerRole {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub order_index: i64,
}

/// One field on a template. `field_type` is data (`text`,
/// `signature`, `date`, `checkbox`, or a tenant-defined kind);
/// `page` names the document page the field sits on, when the
/// template knows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateField {
    pub key: String,
    pub label: String,
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
}

/// A reusable signing template: documents, signer roles, and
/// fields as data, plus the signing order, reminder schedule,
/// and default expiry an envelope built from it inherits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignTemplate {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signer_roles: Vec<TemplateSignerRole>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<TemplateField>,
    pub signing_order: String,
    pub reminder_schedule: ReminderSchedule,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<i64>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-tenant sign settings. `signature_types` names the
/// capture methods this tenant allows (a subset of drawn /
/// typed / uploaded); `provider` names the signing provider
/// integration to use, as data (`internal` is the built-in
/// hash-and-audit provider — no provider behaviour is
/// hard-coded here, the name only selects configuration);
/// `provider_settings` holds that provider's configuration as
/// JSON. `require_view_before_sign` makes the sign page record
/// a view before a signature is accepted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignSettings {
    pub signature_types: Vec<String>,
    pub default_expiry_days: i64,
    pub default_reminder_schedule: ReminderSchedule,
    pub reminder_enabled: bool,
    pub require_view_before_sign: bool,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_settings: Option<serde_json::Value>,
}

impl Default for SignSettings {
    fn default() -> Self {
        Self {
            signature_types: vec![
                SIGNATURE_METHOD_DRAWN.to_string(),
                SIGNATURE_METHOD_TYPED.to_string(),
                SIGNATURE_METHOD_UPLOADED.to_string(),
            ],
            default_expiry_days: DEFAULT_EXPIRY_DAYS,
            default_reminder_schedule: ReminderSchedule::default(),
            reminder_enabled: true,
            require_view_before_sign: false,
            provider: "internal".to_string(),
            provider_settings: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

/// One signer supplied inline when creating an envelope or
/// building one from a template.
#[derive(Debug, Deserialize)]
pub struct SignerInput {
    pub name: String,
    pub email: String,
    #[serde(default = "default_signer_role")]
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_index: Option<i64>,
}

fn default_signer_role() -> String {
    "signer".to_string()
}

#[derive(Debug, Deserialize)]
pub struct CreateEnvelopeRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub document_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_hashes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder_schedule: Option<ReminderSchedule>,
    #[serde(default)]
    pub signers: Vec<SignerInput>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEnvelopeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_hashes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder_schedule: Option<ReminderSchedule>,
}

#[derive(Debug, Deserialize)]
pub struct SignRequest {
    /// `drawn`, `typed`, or `uploaded`.
    pub method: String,
    /// SHA-256 hex of the signature bytes (strokes, rendered
    /// typed name, or uploaded image).
    pub signature_hash: String,
    /// Required when `method` is `typed`: the name as typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typed_name: Option<String>,
    /// The document being signed.
    pub document_id: String,
    /// SHA-256 hex of the document as the signer saw it; must
    /// match the hash recorded for that document on the
    /// envelope.
    pub document_hash: String,
}

#[derive(Debug, Deserialize)]
pub struct DeclineRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTemplateRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub document_ids: Vec<String>,
    #[serde(default)]
    pub signer_roles: Vec<TemplateSignerRole>,
    #[serde(default)]
    pub fields: Vec<TemplateField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder_schedule: Option<ReminderSchedule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTemplateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_roles: Option<Vec<TemplateSignerRole>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<TemplateField>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder_schedule: Option<ReminderSchedule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct UseTemplateRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_hashes: Option<Vec<String>>,
    /// Signers supplied at use time, matched to template roles
    /// by `role`. A role with a default name/email on the
    /// template needs no entry here.
    #[serde(default)]
    pub signers: Vec<SignerInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_expiry_days: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reminder_schedule: Option<ReminderSchedule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_view_before_sign: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_settings: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap()
    }

    fn signer(order: i64, status: SignerStatus) -> SignSigner {
        SignSigner {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            envelope_id: Uuid::new_v4(),
            name: "Alice".to_string(),
            email: "alice@example.com".to_string(),
            role: "signer".to_string(),
            order_index: order,
            status,
            token_hash: String::new(),
            token_expires_at: None,
            viewed_at: None,
            signed_at: None,
            declined_at: None,
            decline_reason: None,
            signature: None,
            reminders_sent: 0,
            last_reminder_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn schedule(interval: i64, max: Option<i64>, start_after: i64) -> ReminderSchedule {
        ReminderSchedule {
            interval_days: interval,
            max_reminders: max,
            start_after_days: start_after,
        }
    }

    #[test]
    fn token_is_48_alphanumeric_and_hash_is_sha256() {
        let token = generate_signing_token();
        assert_eq!(token.len(), SIGNING_TOKEN_LEN);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(generate_signing_token(), token);
        let hash = hash_value(&token);
        assert_eq!(hash.len(), 64);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(hash, hash_value(&token));
        assert_ne!(hash, hash_value("other-token"));
        // Known vector: SHA-256 of the empty string.
        assert_eq!(
            hash_value(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn email_and_sha256_validation() {
        assert!(valid_email("alice@example.com"));
        assert!(!valid_email("not-an-email"));
        assert!(!valid_email("alice@"));
        assert!(!valid_email("@example.com"));
        assert!(!valid_email("a@b@c.com"));
        assert!(!valid_email("alice @example.com"));
        assert!(!valid_email("alice@example"));
        assert!(valid_sha256_hex(&"a".repeat(64)));
        assert!(valid_sha256_hex(&"A".repeat(64)));
        assert!(!valid_sha256_hex("xyz"));
        assert!(!valid_sha256_hex(&"a".repeat(63)));
        assert!(!valid_sha256_hex(&"g".repeat(64)));
    }

    #[test]
    fn signing_order_and_method_validation() {
        assert!(valid_signing_order("sequential"));
        assert!(valid_signing_order("parallel"));
        assert!(!valid_signing_order("random"));
        assert!(valid_signature_method("drawn"));
        assert!(valid_signature_method("typed"));
        assert!(valid_signature_method("uploaded"));
        assert!(!valid_signature_method("voice"));
    }

    #[test]
    fn envelope_status_machine() {
        use EnvelopeStatus::{
            Cancelled, Completed, Declined, Draft, Expired, PartiallySigned, Sent, Voided,
        };
        assert!(envelope_can_transition(Draft, Sent));
        assert!(envelope_can_transition(Draft, Cancelled));
        assert!(envelope_can_transition(Draft, Voided));
        assert!(!envelope_can_transition(Draft, Completed));
        assert!(!envelope_can_transition(Draft, Draft));
        assert!(envelope_can_transition(Sent, PartiallySigned));
        assert!(envelope_can_transition(Sent, Completed));
        assert!(envelope_can_transition(Sent, Declined));
        assert!(envelope_can_transition(Sent, Expired));
        assert!(envelope_can_transition(PartiallySigned, Completed));
        assert!(envelope_can_transition(PartiallySigned, Declined));
        assert!(!envelope_can_transition(PartiallySigned, Sent));
        assert!(!envelope_can_transition(Completed, Cancelled));
        assert!(!envelope_can_transition(Declined, Sent));
        assert!(!envelope_can_transition(Expired, Sent));
        assert!(!envelope_can_transition(Voided, Sent));
        assert!(!envelope_can_transition(Cancelled, Draft));
    }

    #[test]
    fn signer_status_machine() {
        use SignerStatus::{Declined, Pending, Sent, Signed, Viewed};
        assert!(signer_can_transition(Pending, Sent));
        assert!(signer_can_transition(Pending, Signed));
        assert!(signer_can_transition(Sent, Viewed));
        assert!(signer_can_transition(Viewed, Signed));
        assert!(signer_can_transition(Viewed, Declined));
        assert!(signer_can_transition(Sent, Sent));
        assert!(signer_can_transition(Viewed, Sent));
        assert!(!signer_can_transition(Signed, Sent));
        assert!(!signer_can_transition(Declined, Viewed));
        assert!(!signer_can_transition(Pending, Pending));
    }

    #[test]
    fn effective_status_expires_only_live_envelopes() {
        let past = utc(2026, 11, 1, 0);
        let now = utc(2026, 11, 22, 0);
        assert_eq!(
            effective_envelope_status(EnvelopeStatus::Sent, Some(past), now),
            EnvelopeStatus::Expired
        );
        assert_eq!(
            effective_envelope_status(EnvelopeStatus::PartiallySigned, Some(past), now),
            EnvelopeStatus::Expired
        );
        assert_eq!(
            effective_envelope_status(EnvelopeStatus::Sent, Some(utc(2026, 12, 1, 0)), now),
            EnvelopeStatus::Sent
        );
        assert_eq!(
            effective_envelope_status(EnvelopeStatus::Sent, None, now),
            EnvelopeStatus::Sent
        );
        assert_eq!(
            effective_envelope_status(EnvelopeStatus::Draft, Some(past), now),
            EnvelopeStatus::Draft
        );
        assert_eq!(
            effective_envelope_status(EnvelopeStatus::Completed, Some(past), now),
            EnvelopeStatus::Completed
        );
    }

    #[test]
    fn all_signers_signed_requires_at_least_one() {
        assert!(!all_signers_signed(&[]));
        assert!(!all_signers_signed(&[
            signer(0, SignerStatus::Signed),
            signer(1, SignerStatus::Sent),
        ]));
        assert!(all_signers_signed(&[
            signer(0, SignerStatus::Signed),
            signer(1, SignerStatus::Signed),
        ]));
    }

    #[test]
    fn signer_may_sign_enforces_sequence() {
        let first = signer(0, SignerStatus::Sent);
        let mut second = signer(1, SignerStatus::Sent);
        let signers = vec![first.clone(), second.clone()];
        assert!(signer_may_sign(&signers, &first, "sequential"));
        assert!(!signer_may_sign(&signers, &second, "sequential"));
        assert!(signer_may_sign(&signers, &second, "parallel"));
        second.status = SignerStatus::Signed;
        assert!(!signer_may_sign(&signers, &second, "parallel"));
        let mut first_signed = first.clone();
        first_signed.status = SignerStatus::Signed;
        let signers = vec![first_signed, second.clone()];
        assert!(signer_may_sign(&signers, &second, "sequential"));
        let declined = signer(0, SignerStatus::Declined);
        assert!(!signer_may_sign(&[declined.clone()], &declined, "parallel"));
    }

    #[test]
    fn reminder_schedule_math() {
        let sent_at = utc(2026, 11, 1, 10);
        let plan = schedule(7, None, 3);
        // First reminder 3 days after sending.
        assert_eq!(
            next_reminder_at(&plan, sent_at, None, 0),
            Some(utc(2026, 11, 4, 10))
        );
        // Later reminders 7 days after the previous one.
        assert_eq!(
            next_reminder_at(&plan, sent_at, Some(utc(2026, 11, 4, 10)), 1),
            Some(utc(2026, 11, 11, 10))
        );
        // Zero interval disables reminders entirely.
        assert_eq!(
            next_reminder_at(&schedule(0, None, 3), sent_at, None, 0),
            None
        );
        // The cap stops reminders once reached.
        let capped = schedule(7, Some(2), 3);
        assert_eq!(next_reminder_at(&capped, sent_at, None, 2), None);
        assert_eq!(
            next_reminder_at(&capped, sent_at, None, 1),
            Some(utc(2026, 11, 4, 10))
        );
        assert_eq!(reminders_remaining(&capped, 1), Some(1));
        assert_eq!(reminders_remaining(&capped, 5), Some(0));
        assert_eq!(reminders_remaining(&plan, 9), None);
        // Due only once the next reminder time has passed.
        assert!(!reminder_due(&plan, sent_at, None, 0, utc(2026, 11, 4, 9)));
        assert!(reminder_due(&plan, sent_at, None, 0, utc(2026, 11, 4, 10)));
        assert!(reminder_due(&plan, sent_at, None, 0, utc(2026, 11, 5, 10)));
        assert!(!reminder_due(
            &capped,
            sent_at,
            None,
            2,
            utc(2026, 11, 5, 10)
        ));
        assert!(!reminder_due(
            &schedule(0, None, 3),
            sent_at,
            None,
            0,
            utc(2026, 12, 1, 10)
        ));
    }

    #[test]
    fn audit_chain_verifies_and_detects_tampering() {
        let envelope_id = Uuid::new_v4();
        let first_id = Uuid::new_v4();
        let first = SignAuditEntry {
            id: first_id,
            tenant_id: "tenant-1".to_string(),
            envelope_id,
            signer_id: None,
            action: "created".to_string(),
            actor_id: "user-1".to_string(),
            detail: Some("envelope created".to_string()),
            prev_hash: GENESIS_HASH.to_string(),
            hash: compute_audit_hash(
                GENESIS_HASH,
                first_id,
                envelope_id,
                "created",
                "user-1",
                utc(2026, 11, 22, 10),
                "envelope created",
            ),
            created_at: utc(2026, 11, 22, 10),
        };
        let second_id = Uuid::new_v4();
        let second = SignAuditEntry {
            id: second_id,
            tenant_id: "tenant-1".to_string(),
            envelope_id,
            signer_id: None,
            action: "sent".to_string(),
            actor_id: "user-1".to_string(),
            detail: None,
            prev_hash: first.hash.clone(),
            hash: compute_audit_hash(
                &first.hash,
                second_id,
                envelope_id,
                "sent",
                "user-1",
                utc(2026, 11, 22, 11),
                "",
            ),
            created_at: utc(2026, 11, 22, 11),
        };
        let entries = vec![first.clone(), second.clone()];
        assert_eq!(verify_audit_chain(&entries), (true, None));
        assert_eq!(verify_audit_chain(&[]), (true, None));
        let mut tampered = entries.clone();
        tampered[1].detail = Some("tampered".to_string());
        assert_eq!(verify_audit_chain(&tampered), (false, Some(1)));
        let mut broken_link = entries.clone();
        broken_link[1].prev_hash = GENESIS_HASH.to_string();
        assert_eq!(verify_audit_chain(&broken_link), (false, Some(1)));
    }

    #[test]
    fn certificate_hash_is_stable_and_signer_sensitive() {
        let envelope_id = Uuid::new_v4();
        let completed_at = utc(2026, 11, 22, 12);
        let mut first = signer(0, SignerStatus::Signed);
        first.signed_at = Some(completed_at);
        first.signature = Some(SignatureCapture {
            method: "drawn".to_string(),
            signature_hash: "b".repeat(64),
            document_hash: "a".repeat(64),
            typed_name: None,
            ip_hash: "c".repeat(64),
            user_agent_hash: "d".repeat(64),
            signed_at: completed_at,
        });
        let signers = vec![first.clone()];
        let hashes = vec!["a".repeat(64)];
        let hash = compute_certificate_hash(&envelope_id, &hashes, &signers, completed_at);
        assert_eq!(hash.len(), 64);
        assert_eq!(
            compute_certificate_hash(&envelope_id, &hashes, &signers, completed_at),
            hash
        );
        let mut changed = first.clone();
        changed.signature.as_mut().unwrap().signature_hash = "e".repeat(64);
        assert_ne!(
            compute_certificate_hash(&envelope_id, &hashes, &[changed], completed_at),
            hash
        );
        // Document hash order does not matter: the list is sorted.
        let two = vec!["b".repeat(64), "a".repeat(64)];
        assert_eq!(
            compute_certificate_hash(&envelope_id, &two, &signers, completed_at),
            compute_certificate_hash(
                &envelope_id,
                &vec!["a".repeat(64), "b".repeat(64)],
                &signers,
                completed_at
            )
        );
    }

    #[test]
    fn status_round_trips() {
        for (status, text) in [
            (EnvelopeStatus::Draft, "draft"),
            (EnvelopeStatus::Sent, "sent"),
            (EnvelopeStatus::PartiallySigned, "partially_signed"),
            (EnvelopeStatus::Completed, "completed"),
            (EnvelopeStatus::Declined, "declined"),
            (EnvelopeStatus::Expired, "expired"),
            (EnvelopeStatus::Cancelled, "cancelled"),
            (EnvelopeStatus::Voided, "voided"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(EnvelopeStatus::from_str(text), Some(status));
        }
        assert_eq!(EnvelopeStatus::from_str("shipped"), None);
        assert!(EnvelopeStatus::Completed.is_terminal());
        assert!(EnvelopeStatus::Voided.is_terminal());
        assert!(!EnvelopeStatus::Sent.is_terminal());
        assert!(!EnvelopeStatus::Draft.is_terminal());
        for (status, text) in [
            (SignerStatus::Pending, "pending"),
            (SignerStatus::Sent, "sent"),
            (SignerStatus::Viewed, "viewed"),
            (SignerStatus::Signed, "signed"),
            (SignerStatus::Declined, "declined"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(SignerStatus::from_str(text), Some(status));
        }
    }

    #[test]
    fn settings_defaults_allow_all_capture_methods() {
        let settings = SignSettings::default();
        assert_eq!(settings.signature_types.len(), 3);
        assert!(settings.signature_types.contains(&"drawn".to_string()));
        assert!(settings.signature_types.contains(&"typed".to_string()));
        assert!(settings.signature_types.contains(&"uploaded".to_string()));
        assert_eq!(settings.default_expiry_days, DEFAULT_EXPIRY_DAYS);
        assert_eq!(settings.provider, "internal");
        assert!(settings.reminder_enabled);
        assert!(!settings.require_view_before_sign);
    }
}
