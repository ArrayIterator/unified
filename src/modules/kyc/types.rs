//! KYC types: verification cases with a status machine,
//! linked identity documents (references only), AI verdicts,
//! verification contexts (the default context) plus versioned
//! extra instructions, risk levels as data, subject and
//! document types as data, per-tenant settings, and a
//! hash-chained audit trail.
//!
//! Nothing about verification behaviour is hard-coded here:
//! which document kinds exist, which subject kinds exist and
//! which documents each requires, which checks the AI runs,
//! which fields the AI may see, the extra instructions, and
//! the risk bands are all rows an admin creates through the
//! API. The only values fixed in code are the status machine,
//! the verdict outcome vocabulary, and the documented risk
//! score formula below — the score thresholds themselves are
//! data (risk-level rows).
//!
//! Money: this module carries no money fields, so there is no
//! floating point money anywhere in it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_LABEL_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_CODE_LEN: usize = 128;

pub(crate) const MAX_REASON_LEN: usize = 2000;

pub(crate) const MAX_INSTRUCTION_LEN: usize = 10000;

pub(crate) const MAX_SUBJECT_ID_LEN: usize = 200;

/// Default case lifetime in days when the tenant has no
/// settings row yet.
pub(crate) const DEFAULT_EXPIRY_DAYS: i64 = 30;

/// Default maximum linked documents per case.
pub(crate) const DEFAULT_MAX_DOCUMENTS_PER_CASE: i64 = 10;

/// Default risk-level code the auto-approve policy applies to.
pub(crate) const DEFAULT_AUTO_APPROVE_RISK_LEVEL: &str = "low";

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_KYC_VIEW: &str = "kyc.view";

pub(crate) const PERM_KYC_MANAGE: &str = "kyc.manage";

/// Hook event names this module emits.
pub(crate) const EVENT_CASE_SUBMITTED: &str = "kyc.case.submitted";

pub(crate) const EVENT_CASE_VERIFIED: &str = "kyc.case.verified";

pub(crate) const EVENT_CASE_APPROVED: &str = "kyc.case.approved";

pub(crate) const EVENT_CASE_REJECTED: &str = "kyc.case.rejected";

pub(crate) const EVENT_CASE_MORE_INFO_REQUESTED: &str = "kyc.case.more_info_requested";

pub(crate) const EVENT_CASE_EXPIRED: &str = "kyc.case.expired";

pub(crate) const EVENT_CASE_ESCALATED: &str = "kyc.case.escalated";

/// Hook event names this module listens to.
pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

/// The MCP context source this module answers, through the MCP
/// module's `mcp.context.<source>` hook contract.
pub(crate) const EVENT_MCP_CONTEXT_KYC: &str = "mcp.context.kyc";

/// The AI grounding hook the AI module's runner emits for this
/// module's verification capability
/// (`<module>.ai.<capability>_requested`).
pub(crate) const EVENT_AI_VERIFY_REQUESTED: &str = "kyc.ai.verify_identity_documents_requested";

/// Marker written into `subject_id` / `submitted_by` /
/// `reviewed_by` / `uploaded_by` / audit `actor_id` when the
/// owning user is deleted. Rows are kept (cases stay
/// auditable); only the identity is anonymized.
pub(crate) const DELETED_USER_ID: &str = "deleted-user";

/// Genesis hash the audit chain starts from (64 zeroes).
pub(crate) const AUDIT_GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

// ---------------------------------------------------------------------------
// Pure domain functions (unit-tested below)
// ---------------------------------------------------------------------------

/// Case status machine. Allowed transitions:
/// draft -> submitted; submitted -> verifying,
/// more_info_requested, approved, rejected, expired;
/// verifying -> submitted (run failed or re-queued),
/// more_info_requested, approved (auto-approve only),
/// expired; more_info_requested -> submitted, approved,
/// rejected, expired. Approved, rejected, and expired are
/// terminal. Anything else is rejected.
pub fn can_transition(from: KycCaseStatus, to: KycCaseStatus) -> bool {
    use KycCaseStatus::{
        Approved, Draft, Expired, MoreInfoRequested, Rejected, Submitted, Verifying,
    };
    matches!(
        (from, to),
        (Draft, Submitted)
            | (Submitted, Verifying)
            | (Submitted, MoreInfoRequested)
            | (Submitted, Approved)
            | (Submitted, Rejected)
            | (Submitted, Expired)
            | (Verifying, Submitted)
            | (Verifying, MoreInfoRequested)
            | (Verifying, Approved)
            | (Verifying, Expired)
            | (MoreInfoRequested, Submitted)
            | (MoreInfoRequested, Approved)
            | (MoreInfoRequested, Rejected)
            | (MoreInfoRequested, Expired)
    )
}

/// Resolve a 0-100 risk score to the risk level whose
/// `[min_score, max_score]` range contains it. Levels are
/// considered in `min_score` order; the first match wins.
/// `None` when the tenant has no level covering the score
/// (for example when no risk levels exist yet) — the caller
/// stores no level rather than inventing one.
pub fn resolve_risk_level<'a>(levels: &'a [KycRiskLevel], score: i64) -> Option<&'a KycRiskLevel> {
    let mut ordered: Vec<&KycRiskLevel> = levels.iter().filter(|level| level.active).collect();
    ordered.sort_by_key(|level| level.min_score);
    ordered
        .into_iter()
        .find(|level| score >= level.min_score && score <= level.max_score)
}

/// Risk score (0-100) derived from one AI verdict. The formula
/// is the module's documented scoring rule; the thresholds
/// that turn a score into a level are data (risk-level rows).
/// A rejected verdict is always maximum risk. A verified
/// verdict scores on the confidence gap, halved — a confident
/// verification is low risk. An uncertain verdict scores on
/// the full confidence gap. Every failed check adds 5 points.
/// The result is clamped to 0-100.
pub fn compute_risk_score(
    outcome: KycVerdictOutcome,
    confidence: i64,
    failed_check_count: i64,
) -> i64 {
    let confidence = confidence.clamp(0, 100);
    let failed = failed_check_count.max(0) * 5;
    let base = match outcome {
        KycVerdictOutcome::Rejected => return 100,
        KycVerdictOutcome::Verified => (100 - confidence) / 2,
        KycVerdictOutcome::Uncertain => 100 - confidence,
    };
    (base + failed).clamp(0, 100)
}

/// The resolved verification context for one verification run:
/// the active default context merged with every active extra
/// instruction that applies to it (plus the tenant-global ones,
/// which carry no context id). Instructions are ordered by
/// `priority` (lowest first), then by creation time, and their
/// texts are applied in that order. The context and instruction
/// versions are recorded so a verdict is traceable to the exact
/// configuration that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVerificationContext {
    pub context_id: Uuid,
    pub context_name: String,
    pub context_version: i64,
    pub visible_fields: Vec<String>,
    pub checks: Vec<String>,
    pub instruction_texts: Vec<String>,
    pub instruction_versions: Vec<i64>,
}

pub fn resolve_verification_context(
    context: &KycVerificationContext,
    instructions: &[KycInstruction],
) -> ResolvedVerificationContext {
    let mut applicable: Vec<&KycInstruction> = instructions
        .iter()
        .filter(|instruction| {
            instruction.active
                && (instruction.context_id.is_none() || instruction.context_id == Some(context.id))
        })
        .collect();
    applicable.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    ResolvedVerificationContext {
        context_id: context.id,
        context_name: context.name.clone(),
        context_version: context.version,
        visible_fields: context.visible_fields.clone(),
        checks: context.checks.clone(),
        instruction_texts: applicable
            .iter()
            .map(|instruction| instruction.text.clone())
            .collect(),
        instruction_versions: applicable
            .iter()
            .map(|instruction| instruction.version)
            .collect(),
    }
}

/// Parse the AI capability output into a verdict. The model is
/// asked to return JSON with `verdict` (`verified`,
/// `uncertain`, or `rejected`), `confidence` (0-100), and
/// `reasons` (a list of short strings); `failed_checks` is
/// optional. The output may carry surrounding prose, so the
/// first `{` … last `}` span is parsed. `None` when the output
/// is not parseable or fails validation — the caller records an
/// `uncertain` verdict with a parse-failure reason instead of
/// failing the run.
pub fn parse_ai_verdict(output: &str) -> Option<KycAiVerdict> {
    let start = output.find('{')?;
    let end = output.rfind('}')?;
    if end <= start {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(&output[start..=end]).ok()?;
    let outcome = KycVerdictOutcome::from_str(parsed.get("verdict")?.as_str()?)?;
    let confidence = parsed.get("confidence")?.as_i64()?;
    if !(0..=100).contains(&confidence) {
        return None;
    }
    let reasons: Vec<String> = parsed
        .get("reasons")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let failed_checks: Vec<String> = parsed
        .get("failed_checks")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let model = parsed
        .get("model")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    Some(KycAiVerdict {
        outcome,
        confidence,
        reasons,
        failed_checks,
        model,
        run_at: Utc::now(),
        context_version: 0,
        instruction_versions: Vec::new(),
    })
}

/// True when `code` is a valid data-row code: lowercase
/// letters, digits, and single hyphens only, never leading or
/// trailing with a hyphen, at most 128 chars.
pub fn valid_code(code: &str) -> bool {
    if code.is_empty() || code.len() > MAX_CODE_LEN {
        return false;
    }
    if code.starts_with('-') || code.ends_with('-') || code.contains("--") {
        return false;
    }
    code.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// True when `hash` is a 64-character lowercase hex string
/// (a SHA-256 digest, as stored for linked documents).
pub fn valid_content_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Case status. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KycCaseStatus {
    Draft,
    Submitted,
    Verifying,
    MoreInfoRequested,
    Approved,
    Rejected,
    Expired,
}

impl Default for KycCaseStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl KycCaseStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Submitted => "submitted",
            Self::Verifying => "verifying",
            Self::MoreInfoRequested => "more_info_requested",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "submitted" => Some(Self::Submitted),
            "verifying" => Some(Self::Verifying),
            "more_info_requested" => Some(Self::MoreInfoRequested),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }

    /// Terminal statuses: no transition leaves them.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Approved | Self::Rejected | Self::Expired)
    }
}

/// AI verdict outcome. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KycVerdictOutcome {
    Verified,
    Uncertain,
    Rejected,
}

impl KycVerdictOutcome {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Uncertain => "uncertain",
            Self::Rejected => "rejected",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "verified" => Some(Self::Verified),
            "uncertain" => Some(Self::Uncertain),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

/// One AI verification verdict, stored on the case. `reasons`
/// and `failed_checks` are data (model-supplied strings), never
/// code. `context_version` and `instruction_versions` record
/// the exact default context and extra instructions the run
/// resolved, so a verdict is reproducible from the stored
/// configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KycAiVerdict {
    pub outcome: KycVerdictOutcome,
    pub confidence: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_checks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub run_at: DateTime<Utc>,
    pub context_version: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instruction_versions: Vec<i64>,
}

/// One identity verification case. `subject_id` is a user id
/// or a company id (plain TEXT, no foreign key).
/// `subject_type` names a subject-type row by code.
/// `risk_score` is NULL until the first AI verdict;
/// `risk_level_id` / `risk_level_code` name the resolved risk
/// level (NULL when no level covers the score). `ai_verdict`
/// is the last AI verdict (NULL until the first run). The AI
/// never approves a case by itself: `reviewed_by` is the human
/// admin who decided, or `system` for the admin-enabled
/// auto-approve path (see `KycSettings`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KycCase {
    pub id: Uuid,
    pub tenant_id: String,
    pub subject_id: String,
    pub subject_type: String,
    pub status: KycCaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_score: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_level_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_level_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_verdict: Option<KycAiVerdict>,
    pub submitted_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<String>,
    pub escalated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One linked identity document on a case. Only the
/// documents-module reference (`document_id`), the SHA-256 of
/// the document bytes (`content_hash`), and file metadata are
/// stored — never the file itself. `document_type_code` names
/// a document-type row by code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KycDocument {
    pub id: Uuid,
    pub tenant_id: String,
    pub case_id: Uuid,
    pub document_type_code: String,
    pub document_id: String,
    pub content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub uploaded_by: String,
    pub created_at: DateTime<Utc>,
}

/// One document kind a tenant accepts, as data.
/// `subject_types` lists the subject-type codes the kind
/// applies to; an empty list means it applies to every
/// subject type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KycDocumentType {
    pub id: Uuid,
    pub tenant_id: String,
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subject_types: Vec<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One subject kind a tenant verifies, as data.
/// `required_document_type_codes` lists the document-type
/// codes a case of this kind must link before it can be
/// submitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KycSubjectType {
    pub id: Uuid,
    pub tenant_id: String,
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_document_type_codes: Vec<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The DEFAULT CONTEXT the AI verification pipeline runs
/// under, as data. One context is active per tenant. The
/// context declares which case fields and document metadata
/// the AI may see (`visible_fields`) and which checks it must
/// run (`checks` — values are data, the module never invents
/// a check name). `version` increments on every update; a
/// verdict records the version it ran under.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KycVerificationContext {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub version: i64,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One ADDITIONAL INSTRUCTION declared on top of a
/// verification context, as data. `context_id` NULL means the
/// instruction applies to every context of the tenant.
/// Instructions are versioned: an update creates a new row
/// with `version` + 1 and deactivates the old row, which keeps
/// `supersedes_id` pointing at its replacement, so the history
/// is never rewritten. `priority` orders instructions (lowest
/// first) when they merge into the resolved context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KycInstruction {
    pub id: Uuid,
    pub tenant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<Uuid>,
    pub text: String,
    pub version: i64,
    pub active: bool,
    pub priority: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_id: Option<Uuid>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One risk band, as data. A case score (0-100) resolves to
/// the level whose `[min_score, max_score]` contains it.
/// Active levels of a tenant must not overlap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KycRiskLevel {
    pub id: Uuid,
    pub tenant_id: String,
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub min_score: i64,
    pub max_score: i64,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-tenant KYC settings. `auto_approve_low_risk` is the
/// only path by which an AI verdict can approve a case without
/// a human, and only for the configured low-risk level code;
/// it defaults to off. `require_ai_verification` gates the
/// verify endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KycSettings {
    pub auto_approve_low_risk: bool,
    pub auto_approve_risk_level_code: String,
    pub require_ai_verification: bool,
    pub verification_expiry_days: i64,
    pub max_documents_per_case: i64,
}

impl Default for KycSettings {
    fn default() -> Self {
        Self {
            auto_approve_low_risk: false,
            auto_approve_risk_level_code: DEFAULT_AUTO_APPROVE_RISK_LEVEL.to_string(),
            require_ai_verification: true,
            verification_expiry_days: DEFAULT_EXPIRY_DAYS,
            max_documents_per_case: DEFAULT_MAX_DOCUMENTS_PER_CASE,
        }
    }
}

/// One hash-chained audit entry for a case. `hash` is the
/// SHA-256 of
/// `prev_hash|id|case_id|action|actor|created_at|detail`; the
/// first entry of a case chains from the genesis hash (64
/// zeroes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KycAuditEntry {
    pub id: Uuid,
    pub tenant_id: String,
    pub case_id: Uuid,
    pub action: String,
    pub actor_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub prev_hash: String,
    pub hash: String,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateCaseRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    pub subject_type: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCaseRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LinkDocumentRequest {
    pub document_type_code: String,
    pub document_id: String,
    pub content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DecisionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RequestMoreInfoRequest {
    pub reason: String,
    #[serde(default)]
    pub requested_document_type_codes: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDocumentTypeRequest {
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub subject_types: Vec<String>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDocumentTypeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSubjectTypeRequest {
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required_document_type_codes: Vec<String>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSubjectTypeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_document_type_codes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateContextRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub visible_fields: Vec<String>,
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateContextRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_fields: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateInstructionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<Uuid>,
    pub text: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateInstructionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRiskLevelRequest {
    pub code: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub min_score: i64,
    pub max_score: i64,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRiskLevelRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_score: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_score: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_approve_low_risk: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_approve_risk_level_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_ai_verification: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_expiry_days: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_documents_per_case: Option<i64>,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> KycVerificationContext {
        KycVerificationContext {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            name: "default".to_string(),
            description: None,
            version: 3,
            active: true,
            visible_fields: vec!["subject_type".to_string()],
            checks: vec!["example-check".to_string()],
            created_by: "admin-1".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn instruction(
        text: &str,
        priority: i64,
        context_id: Option<Uuid>,
        active: bool,
    ) -> KycInstruction {
        KycInstruction {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            context_id,
            text: text.to_string(),
            version: 1,
            active,
            priority,
            supersedes_id: None,
            created_by: "admin-1".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn risk_level(code: &str, min_score: i64, max_score: i64, active: bool) -> KycRiskLevel {
        KycRiskLevel {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            code: code.to_string(),
            label: code.to_string(),
            description: None,
            min_score,
            max_score,
            active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn status_machine_transitions() {
        use KycCaseStatus::{
            Approved, Draft, Expired, MoreInfoRequested, Rejected, Submitted, Verifying,
        };
        assert!(can_transition(Draft, Submitted));
        assert!(!can_transition(Draft, Approved));
        assert!(!can_transition(Draft, Verifying));
        assert!(can_transition(Submitted, Verifying));
        assert!(can_transition(Submitted, MoreInfoRequested));
        assert!(can_transition(Submitted, Approved));
        assert!(can_transition(Submitted, Rejected));
        assert!(can_transition(Submitted, Expired));
        assert!(can_transition(Verifying, Submitted));
        assert!(can_transition(Verifying, MoreInfoRequested));
        assert!(can_transition(Verifying, Approved));
        assert!(!can_transition(Verifying, Rejected));
        assert!(!can_transition(Verifying, Draft));
        assert!(can_transition(MoreInfoRequested, Submitted));
        assert!(can_transition(MoreInfoRequested, Approved));
        assert!(can_transition(MoreInfoRequested, Rejected));
        assert!(can_transition(MoreInfoRequested, Expired));
        assert!(!can_transition(MoreInfoRequested, Verifying));
        assert!(!can_transition(Approved, Submitted));
        assert!(!can_transition(Rejected, Submitted));
        assert!(!can_transition(Expired, Submitted));
        assert!(!can_transition(Draft, Draft));
        assert!(Approved.is_terminal());
        assert!(Rejected.is_terminal());
        assert!(Expired.is_terminal());
        assert!(!Submitted.is_terminal());
    }

    #[test]
    fn status_round_trips() {
        for (status, text) in [
            (KycCaseStatus::Draft, "draft"),
            (KycCaseStatus::Submitted, "submitted"),
            (KycCaseStatus::Verifying, "verifying"),
            (KycCaseStatus::MoreInfoRequested, "more_info_requested"),
            (KycCaseStatus::Approved, "approved"),
            (KycCaseStatus::Rejected, "rejected"),
            (KycCaseStatus::Expired, "expired"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(KycCaseStatus::from_str(text), Some(status));
        }
        assert_eq!(KycCaseStatus::from_str("pending"), None);
    }

    #[test]
    fn risk_level_resolution() {
        let levels = vec![
            risk_level("low", 0, 29, true),
            risk_level("medium", 30, 69, true),
            risk_level("high", 70, 100, true),
        ];
        assert_eq!(resolve_risk_level(&levels, 0).unwrap().code, "low");
        assert_eq!(resolve_risk_level(&levels, 29).unwrap().code, "low");
        assert_eq!(resolve_risk_level(&levels, 30).unwrap().code, "medium");
        assert_eq!(resolve_risk_level(&levels, 69).unwrap().code, "medium");
        assert_eq!(resolve_risk_level(&levels, 70).unwrap().code, "high");
        assert_eq!(resolve_risk_level(&levels, 100).unwrap().code, "high");
        assert!(resolve_risk_level(&levels, -1).is_none());
        assert!(resolve_risk_level(&levels, 101).is_none());
        assert!(resolve_risk_level(&[], 10).is_none());
        // Inactive levels never resolve, even when they cover the score.
        let inactive = vec![risk_level("low", 0, 100, false)];
        assert!(resolve_risk_level(&inactive, 10).is_none());
        // First match in min_score order wins when ranges overlap.
        let overlapping = vec![
            risk_level("wide", 0, 100, true),
            risk_level("narrow", 40, 60, true),
        ];
        assert_eq!(resolve_risk_level(&overlapping, 50).unwrap().code, "wide");
    }

    #[test]
    fn risk_score_formula() {
        use KycVerdictOutcome::{Rejected, Uncertain, Verified};
        // Rejected is always maximum risk.
        assert_eq!(compute_risk_score(Rejected, 100, 0), 100);
        assert_eq!(compute_risk_score(Rejected, 0, 0), 100);
        // Verified halves the confidence gap.
        assert_eq!(compute_risk_score(Verified, 100, 0), 0);
        assert_eq!(compute_risk_score(Verified, 80, 0), 10);
        assert_eq!(compute_risk_score(Verified, 0, 0), 50);
        // Uncertain keeps the full gap.
        assert_eq!(compute_risk_score(Uncertain, 100, 0), 0);
        assert_eq!(compute_risk_score(Uncertain, 50, 0), 50);
        // Each failed check adds 5, clamped at 100.
        assert_eq!(compute_risk_score(Verified, 100, 2), 10);
        assert_eq!(compute_risk_score(Uncertain, 0, 30), 100);
        // Out-of-range confidence is clamped, negative failures ignored.
        assert_eq!(compute_risk_score(Verified, 150, -1), 0);
        assert_eq!(compute_risk_score(Uncertain, -20, 0), 100);
    }

    #[test]
    fn context_resolution_merges_instructions_in_priority_order() {
        let ctx = context();
        let global = instruction("global note", 5, None, true);
        let scoped_low = instruction("scoped first", 1, Some(ctx.id), true);
        let scoped_high = instruction("scoped second", 9, Some(ctx.id), true);
        let other_context = instruction("other context", 0, Some(Uuid::new_v4()), true);
        let inactive = instruction("inactive", 0, Some(ctx.id), false);
        let resolved = resolve_verification_context(
            &ctx,
            &[global, scoped_low, scoped_high, other_context, inactive],
        );
        assert_eq!(resolved.context_id, ctx.id);
        assert_eq!(resolved.context_name, "default");
        assert_eq!(resolved.context_version, 3);
        assert_eq!(resolved.visible_fields, vec!["subject_type".to_string()]);
        assert_eq!(resolved.checks, vec!["example-check".to_string()]);
        // Priority order: scoped first (1), global note (5), scoped second (9).
        assert_eq!(
            resolved.instruction_texts,
            vec![
                "scoped first".to_string(),
                "global note".to_string(),
                "scoped second".to_string()
            ]
        );
        assert_eq!(resolved.instruction_versions, vec![1, 1, 1]);
    }

    #[test]
    fn context_resolution_without_instructions() {
        let ctx = context();
        let resolved = resolve_verification_context(&ctx, &[]);
        assert!(resolved.instruction_texts.is_empty());
        assert!(resolved.instruction_versions.is_empty());
        assert_eq!(resolved.context_version, 3);
    }

    #[test]
    fn parse_ai_verdict_valid_json() {
        let output = r#"{"verdict": "verified", "confidence": 87, "reasons": ["Document matches"], "failed_checks": []}"#;
        let verdict = parse_ai_verdict(output).unwrap();
        assert_eq!(verdict.outcome, KycVerdictOutcome::Verified);
        assert_eq!(verdict.confidence, 87);
        assert_eq!(verdict.reasons, vec!["Document matches".to_string()]);
        assert!(verdict.failed_checks.is_empty());
    }

    #[test]
    fn parse_ai_verdict_tolerates_surrounding_prose() {
        let output = "Here is the result:\n{\"verdict\": \"uncertain\", \"confidence\": 40, \"reasons\": [\"Blurry photo\"], \"failed_checks\": [\"example-check\"]}\nDone.";
        let verdict = parse_ai_verdict(output).unwrap();
        assert_eq!(verdict.outcome, KycVerdictOutcome::Uncertain);
        assert_eq!(verdict.confidence, 40);
        assert_eq!(verdict.failed_checks, vec!["example-check".to_string()]);
    }

    #[test]
    fn parse_ai_verdict_rejects_bad_input() {
        assert!(parse_ai_verdict("not json at all").is_none());
        assert!(parse_ai_verdict(r#"{"verdict": "maybe", "confidence": 50}"#).is_none());
        assert!(parse_ai_verdict(r#"{"verdict": "verified", "confidence": 101}"#).is_none());
        assert!(parse_ai_verdict(r#"{"verdict": "verified", "confidence": -1}"#).is_none());
        assert!(parse_ai_verdict(r#"{"verdict": "verified"}"#).is_none());
        assert!(parse_ai_verdict(r#"{"confidence": 50}"#).is_none());
        assert!(parse_ai_verdict("{}").is_none());
    }

    #[test]
    fn verdict_outcome_round_trips() {
        for (outcome, text) in [
            (KycVerdictOutcome::Verified, "verified"),
            (KycVerdictOutcome::Uncertain, "uncertain"),
            (KycVerdictOutcome::Rejected, "rejected"),
        ] {
            assert_eq!(outcome.as_str(), text);
            assert_eq!(KycVerdictOutcome::from_str(text), Some(outcome));
        }
        assert_eq!(KycVerdictOutcome::from_str("approved"), None);
    }

    #[test]
    fn code_validation() {
        assert!(valid_code("my-code"));
        assert!(valid_code("example"));
        assert!(valid_code("a1-b2"));
        assert!(!valid_code("My-Code"));
        assert!(!valid_code("a--b"));
        assert!(!valid_code("-a"));
        assert!(!valid_code("a-"));
        assert!(!valid_code(""));
        assert!(!valid_code("a_b"));
    }

    #[test]
    fn content_hash_validation() {
        assert!(valid_content_hash(&"a".repeat(64)));
        assert!(valid_content_hash(&"0123456789abcdef".repeat(4)));
        assert!(!valid_content_hash(&"A".repeat(64)));
        assert!(!valid_content_hash("abc"));
        assert!(!valid_content_hash(&"a".repeat(63)));
        assert!(!valid_content_hash(&"g".repeat(64)));
    }

    #[test]
    fn settings_defaults_are_safe() {
        let settings = KycSettings::default();
        assert!(!settings.auto_approve_low_risk);
        assert_eq!(settings.auto_approve_risk_level_code, "low");
        assert!(settings.require_ai_verification);
        assert_eq!(settings.verification_expiry_days, DEFAULT_EXPIRY_DAYS);
        assert_eq!(
            settings.max_documents_per_case,
            DEFAULT_MAX_DOCUMENTS_PER_CASE
        );
    }
}
