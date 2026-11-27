//! Referral types: job eligibility, shareable referral
//! links, referrals with a data-driven pipeline, reward
//! rules and rewards, anti-gaming rules, and the audit
//! trail, plus the pure transition / duplicate / reward /
//! token math.
//!
//! Nothing about the referral content is hard-coded: the
//! pipeline stages (with their allowed transitions), the
//! reward rules (amount, payout trigger stage, retention
//! days, eligibility conditions), and the anti-gaming rules
//! (cooldowns, per-period caps, active-employee and
//! self-referral checks) are all data rows an administrator
//! enters. The only fixed vocabulary is the reward status
//! (`pending`, `eligible`, `paid`, `voided`), the four
//! anti-gaming rule kinds, and the two link/reward
//! enforcement outcomes - those are the shape of the
//! module, not its content.
//!
//! Money is integer minor units (`amount_minor`, e.g. cents)
//! with an ISO currency code; `f64` is never used for a
//! stored or returned amount anywhere in this module.

use chrono::{DateTime, Utc};
use rand::{Rng, distr::Alphanumeric};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_EMAIL_LEN: usize = 254;

pub(crate) const MAX_PHONE_LEN: usize = 64;

pub(crate) const MAX_NOTE_LEN: usize = 5000;

pub(crate) const MAX_SLUG_LEN: usize = 64;

pub(crate) const MAX_CURRENCY_LEN: usize = 3;

pub(crate) const MAX_REASON_LEN: usize = 1000;

/// Maximum eligibility conditions on one reward rule.
pub(crate) const MAX_CONDITIONS: usize = 50;

/// Maximum allowed next stages on one pipeline stage.
pub(crate) const MAX_NEXT_STAGES: usize = 50;

/// Length of a raw referral link token (alphanumeric).
pub(crate) const LINK_TOKEN_LEN: usize = 48;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_REFERRAL_VIEW: &str = "referral.view";

pub(crate) const PERM_REFERRAL_MANAGE: &str = "referral.manage";

/// Hook event names this module emits.
pub(crate) const EVENT_LINK_CREATED: &str = "referral.link.created";

pub(crate) const EVENT_SUBMITTED: &str = "referral.submitted";

pub(crate) const EVENT_STAGE_CHANGED: &str = "referral.stage_changed";

pub(crate) const EVENT_HIRED: &str = "referral.hired";

pub(crate) const EVENT_REWARD_PAYABLE: &str = "referral.reward.payable";

pub(crate) const EVENT_REWARD_PAID: &str = "referral.reward.paid";

pub(crate) const EVENT_REWARD_VOIDED: &str = "referral.reward.voided";

pub(crate) const EVENT_ELIGIBILITY_SET: &str = "referral.job.eligibility_set";

/// Hook event names this module listens to.
pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

pub(crate) const EVENT_APPLICANT_MOVED: &str = "recruitment.applicant.moved";

pub(crate) const EVENT_RECRUITMENT_HIRED: &str = "recruitment.hired";

pub(crate) const EVENT_HR_EMPLOYEE_CREATED: &str = "hr.employee_created";

/// Marker written into `referrer_user_id` / `created_by` /
/// audit actors when the owning user is deleted. Rows are
/// kept (the referral history stays auditable); only the
/// identity is anonymized.
pub(crate) const DELETED_USER_ID: &str = "deleted-user";

/// Marker written into a candidate's name when the candidate
/// is anonymized through `user.deleted` (the payload names a
/// user id; when that id is the referrer, the candidate rows
/// that referrer submitted are anonymized too only when the
/// payload also names the candidate email - see `hooks.rs`).
pub(crate) const DELETED_CANDIDATE_NAME: &str = "Deleted candidate";

/// Marker written into a candidate's email on anonymization.
/// Deliberately not a deliverable address.
pub(crate) const DELETED_CANDIDATE_EMAIL: &str = "deleted-candidate";

/// Verification outcome recorded on a referral when the
/// referrer's active-employee status could not be confirmed:
/// no HR existence hook exists in this tree (verified from
/// source - the HR module answers only `user.manager.query`
/// and emits `hr.employee_created` with no tenant), so
/// absence from the local registry proves nothing and the
/// referral is flagged, never silently treated as verified.
pub(crate) const VERIFICATION_UNVERIFIED: &str = "unverified";

pub(crate) const VERIFICATION_VERIFIED: &str = "verified";

// Pure helpers: tokens, email, slugs, currency

/// SHA-256 hex digest. Used for referral-link token storage:
/// the raw token is returned exactly once at creation /
/// rotation and only its hash is ever stored or logged.
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

/// One unguessable referral link token (48 random
/// alphanumeric characters). Only its SHA-256 hash is ever
/// stored.
pub fn generate_link_token() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(LINK_TOKEN_LEN)
        .map(char::from)
        .collect()
}

/// True when `value` is a 64-character hex SHA-256 digest.
pub fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Email validation: one `@`, non-empty local and domain
/// parts, a dot in the domain, no spaces.
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

/// Canonical candidate email: trimmed and lowercased, so
/// `Jane@Example.com` and `jane@example.com` are the same
/// candidate for duplicate detection.
pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// True when `value` is a usable pipeline-stage slug:
/// lowercase letters, digits, and dashes, starting with a
/// letter or digit.
pub fn valid_slug(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_SLUG_LEN {
        return false;
    }
    if !value
        .bytes()
        .next()
        .map(|b| b.is_ascii_alphanumeric())
        .unwrap_or(false)
    {
        return false;
    }
    value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// True when `value` is a three-letter ISO currency code
/// (`USD`, `EUR`, `IDR`).
pub fn valid_currency(value: &str) -> bool {
    value.len() == MAX_CURRENCY_LEN && value.bytes().all(|b| b.is_ascii_uppercase())
}

// Pure domain math (unit-tested below)

/// True when a stage transition is allowed: the current
/// stage must exist, must not be terminal, must be active,
/// the target stage must exist and be active, and the target
/// must be listed in the current stage's allowed next
/// stages. Moving to the same stage is never a transition.
pub fn transition_allowed(
    stages: &[PipelineStage],
    from_stage_id: Uuid,
    to_stage_id: Uuid,
) -> bool {
    if from_stage_id == to_stage_id {
        return false;
    }
    let Some(from) = stages.iter().find(|stage| stage.id == from_stage_id) else {
        return false;
    };
    if from.is_terminal || !from.active {
        return false;
    }
    let Some(to) = stages.iter().find(|stage| stage.id == to_stage_id) else {
        return false;
    };
    if !to.active {
        return false;
    }
    from.allowed_next_stage_ids.contains(&to_stage_id)
}

/// The initial pipeline stage for a new referral: the
/// active, non-terminal stage with the lowest sort order.
/// `None` when the tenant has defined no usable stage yet -
/// callers refuse the submission with 409 instead of
/// inventing a stage.
pub fn initial_stage(stages: &[PipelineStage]) -> Option<&PipelineStage> {
    stages
        .iter()
        .filter(|stage| stage.active && !stage.is_terminal)
        .min_by_key(|stage| stage.sort_order)
}

/// True when a referral for this (candidate email, job)
/// already exists. `existing` holds the normalized email
/// plus job id of every referral in the tenant; the check is
/// case-insensitive because emails are normalized on write.
pub fn duplicate_exists(existing: &[(String, Uuid)], candidate_email: &str, job_id: Uuid) -> bool {
    let wanted = normalize_email(candidate_email);
    existing
        .iter()
        .any(|(email, job)| *job == job_id && normalize_email(email) == wanted)
}

/// True when a same-candidate cooldown rule is violated:
/// the candidate was already referred for any job within
/// the last `cooldown_days` days. A cooldown of zero days
/// never blocks. `None` for the last referral means the
/// candidate was never referred before.
pub fn cooldown_violated(
    last_referral_at: Option<DateTime<Utc>>,
    cooldown_days: i64,
    now: DateTime<Utc>,
) -> bool {
    if cooldown_days <= 0 {
        return false;
    }
    let Some(last) = last_referral_at else {
        return false;
    };
    let elapsed_days = (now - last).num_days();
    elapsed_days < cooldown_days
}

/// True when a per-period cap is violated: the referrer
/// already has `count_in_window` referrals inside the
/// window and the cap is `limit`. A limit of zero or less
/// never blocks.
pub fn max_referrals_violated(count_in_window: i64, limit: i64) -> bool {
    limit > 0 && count_in_window >= limit
}

/// True when the candidate is the referrer themself: the
/// candidate email equals the referrer's email. `None` for
/// the referrer email means the referrer's email is unknown
/// (no user-email lookup hook exists in this tree), so the
/// check cannot run and never blocks - callers record that
/// outcome as `unverified` instead of pretending the check
/// passed.
pub fn is_self_referral(referrer_email: Option<&str>, candidate_email: &str) -> bool {
    match referrer_email {
        Some(email) => normalize_email(email) == normalize_email(candidate_email),
        None => false,
    }
}

/// The moment a reward becomes payable: the trigger stage
/// was reached at `trigger_reached_at`, plus the rule's
/// retention days (`0` means payable on reaching the stage).
/// `None` when the trigger stage was never reached.
pub fn reward_payable_at(
    trigger_reached_at: Option<DateTime<Utc>>,
    retention_days: i64,
) -> Option<DateTime<Utc>> {
    trigger_reached_at.map(|reached| reached + chrono::Duration::days(retention_days.max(0)))
}

/// True when a reward is eligible right now: the trigger
/// stage was reached and the retention period has elapsed.
pub fn reward_eligible(
    trigger_reached_at: Option<DateTime<Utc>>,
    retention_days: i64,
    now: DateTime<Utc>,
) -> bool {
    match reward_payable_at(trigger_reached_at, retention_days) {
        Some(payable_at) => now >= payable_at,
        None => false,
    }
}

/// Referral counts per pipeline stage, as (stage slug,
/// count) pairs in stage order. Stages with no referrals
/// are included with a zero count so the pipeline shape
/// stays visible.
pub fn counts_by_stage(stages: &[PipelineStage], referrals: &[Referral]) -> Vec<(String, i64)> {
    let mut ordered: Vec<&PipelineStage> = stages.iter().collect();
    ordered.sort_by_key(|stage| stage.sort_order);
    ordered
        .iter()
        .map(|stage| {
            let count = referrals
                .iter()
                .filter(|referral| referral.stage_id == stage.id)
                .count() as i64;
            (stage.slug.clone(), count)
        })
        .collect()
}

// Enumerations (fixed vocabulary - the shape, not the content)

/// Reward payout status. A reward is created `pending` when
/// its referral reaches the rule's trigger stage, becomes
/// `eligible` once the retention period has elapsed, is
/// `paid` by an explicit mark-paid call (billing / HR act on
/// the `referral.reward.payable` event - this module never
/// moves money itself), and is `voided` when the referral is
/// rejected after the reward was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewardStatus {
    Pending,
    Eligible,
    Paid,
    Voided,
}

impl RewardStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Eligible => "eligible",
            Self::Paid => "paid",
            Self::Voided => "voided",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "eligible" => Some(Self::Eligible),
            "paid" => Some(Self::Paid),
            "voided" => Some(Self::Voided),
            _ => None,
        }
    }
}

/// Anti-gaming rule kind. The kinds are the shape of the
/// module; the thresholds, windows, and enforcement of each
/// rule are data on the rule row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AntiGamingKind {
    BlockSelfReferral,
    SameCandidateCooldown,
    MaxReferralsPerPeriod,
    RequireActiveEmployee,
}

impl AntiGamingKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::BlockSelfReferral => "block_self_referral",
            Self::SameCandidateCooldown => "same_candidate_cooldown",
            Self::MaxReferralsPerPeriod => "max_referrals_per_period",
            Self::RequireActiveEmployee => "require_active_employee",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "block_self_referral" => Some(Self::BlockSelfReferral),
            "same_candidate_cooldown" => Some(Self::SameCandidateCooldown),
            "max_referrals_per_period" => Some(Self::MaxReferralsPerPeriod),
            "require_active_employee" => Some(Self::RequireActiveEmployee),
            _ => None,
        }
    }
}

/// How a violated anti-gaming rule is enforced: `block`
/// rejects the submission with 409, `flag` records the
/// violation on the referral row and lets it through for a
/// manager to review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    Block,
    Flag,
}

impl Enforcement {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Flag => "flag",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "block" => Some(Self::Block),
            "flag" => Some(Self::Flag),
            _ => None,
        }
    }
}

// Stored types

/// Referral eligibility of one recruitment job (referenced
/// by id only - the recruitment module owns the job, there
/// is no foreign key and no existence check here). A job
/// with no row is eligible by default; a row with
/// `eligible: false` blocks new referrals for it. A row may
/// point at a per-job reward rule; otherwise the tenant's
/// default rule applies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEligibility {
    pub id: Uuid,
    pub tenant_id: String,
    pub job_id: Uuid,
    pub eligible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_rule_id: Option<Uuid>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One shareable referral link for one (employee, job)
/// pair. Only the SHA-256 hash of the token is stored; the
/// raw token is returned exactly once at creation and on
/// rotation. Clicks are counted only - no IP address, user
/// agent, or fingerprint is stored. One active link per
/// (employee, job); creating another returns 409 and the
/// existing link must be rotated for a fresh token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferralLink {
    pub id: Uuid,
    pub tenant_id: String,
    pub job_id: Uuid,
    pub referrer_user_id: String,
    #[serde(skip_serializing)]
    pub token_hash: String,
    #[serde(default)]
    pub click_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_clicked_at: Option<DateTime<Utc>>,
    #[serde(default = "default_active")]
    pub active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_active() -> bool {
    true
}

/// One pipeline stage, defined as data by an administrator:
/// its name, slug, order, terminal / hired / rejected
/// flags, and the stages it may move to. The pipeline
/// itself is never hard-coded - a tenant with no stage rows
/// simply cannot accept referrals yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStage {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub slug: String,
    pub sort_order: i32,
    #[serde(default)]
    pub is_terminal: bool,
    #[serde(default)]
    pub is_hired_stage: bool,
    #[serde(default)]
    pub is_rejected_stage: bool,
    #[serde(default)]
    pub allowed_next_stage_ids: Vec<Uuid>,
    #[serde(default = "default_active")]
    pub active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One reward rule, defined as data: the amount in integer
/// minor units with its currency, the pipeline stage whose
/// entry triggers the reward, the retention days after that
/// stage before the reward becomes payable (`0` means
/// payable on reaching the stage), and free-text eligibility
/// conditions. At most one rule per tenant is the default;
/// a job with no per-job override uses the default rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardRule {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_stage_id: Option<Uuid>,
    #[serde(default)]
    pub retention_days: i64,
    #[serde(default)]
    pub eligibility_conditions: Vec<String>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default = "default_active")]
    pub active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One referral: who referred whom for which job, through
/// which link, and where the candidate sits in the
/// pipeline. `candidate_email` is stored normalized
/// (lowercase); one referral per (candidate email, job) is
/// enforced by a unique index and checked on write.
/// `referrer_verified` records whether the referrer was
/// confirmed as an active employee through the HR employee
/// registry (built from `hr.employee_created` events);
/// `verification_note` explains the outcome, including the
/// `unverified` case where no HR record answered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Referral {
    pub id: Uuid,
    pub tenant_id: String,
    pub job_id: Uuid,
    pub referrer_user_id: String,
    #[serde(default)]
    pub referrer_verified: bool,
    #[serde(default)]
    pub verification_note: String,
    pub candidate_name: String,
    pub candidate_email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<Uuid>,
    pub stage_id: Uuid,
    pub stage_slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audited pipeline move. Rows are append-only: a move
/// never rewrites the referral's past, it appends a record
/// with the previous stage, the new stage, the actor, and
/// the reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferralStageHistory {
    pub id: Uuid,
    pub tenant_id: String,
    pub referral_id: Uuid,
    pub job_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_stage_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_stage_slug: Option<String>,
    pub to_stage_id: Uuid,
    pub to_stage_slug: String,
    pub moved_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One reward owed for a referral: created when the
/// referral reaches the rule's trigger stage, with the
/// amount and currency snapshotted from the rule (a later
/// rule edit never rewrites an owed reward). Payout is
/// never done here - reaching eligibility emits
/// `referral.reward.payable` for billing / HR to act on,
/// and `mark-paid` records that the payout happened
/// elsewhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reward {
    pub id: Uuid,
    pub tenant_id: String,
    pub referral_id: Uuid,
    pub job_id: Uuid,
    pub referrer_user_id: String,
    pub rule_id: Uuid,
    pub rule_name: String,
    pub amount_minor: i64,
    pub currency: String,
    pub status: RewardStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligible_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub void_reason: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One anti-gaming rule, defined as data: its kind, whether
/// it is enabled, its thresholds (`cooldown_days` for the
/// same-candidate cooldown, `limit_count` plus `window_days`
/// for the per-period cap), and whether a violation blocks
/// the submission or only flags the referral for review.
/// The self-referral and active-employee kinds depend on
/// data this module may not have (no user-email lookup hook
/// exists; the HR registry only knows employees created
/// after this module was installed) - when the check cannot
/// run, the referral records `unverified` instead of a
/// false pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntiGamingRule {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub rule_kind: AntiGamingKind,
    #[serde(default = "default_active")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_days: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_days: Option<i64>,
    pub enforcement: Enforcement,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One known employee, learned from `hr.employee_created`
/// events (`{employee_id, user_id, department_id}` - the
/// event carries no tenant, so the registry is keyed by
/// user id and `tenant_id` is empty until an event carries
/// one). Used only for the active-employee check: presence
/// means the user is a known employee; absence means
/// unknown, never "not an employee".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownEmployee {
    pub employee_id: String,
    pub user_id: String,
    pub tenant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One audit row: who changed what, when, with the before
/// and after values as JSON. Written for referral creates /
/// updates / deletes / stage moves, reward mark-paid /
/// void, and link rotation / revocation. Audit rows are
/// never updated or deleted through the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub id: Uuid,
    pub tenant_id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub action: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

// Response views (computed, not stored)

/// The create / rotate link response: the link row plus the
/// raw token, returned exactly once. The token hash is
/// never serialized (see `ReferralLink`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkCreated {
    pub link: ReferralLink,
    pub token: String,
}

/// Aggregate counts for one job: referrals by pipeline
/// stage plus reward totals. Built from stored rows only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobReferralSummary {
    pub job_id: String,
    pub referral_count: i64,
    pub by_stage: Vec<StageCount>,
    pub rewards_pending: i64,
    pub rewards_eligible: i64,
    pub rewards_paid: i64,
    pub rewards_voided: i64,
    pub rewards_paid_minor: i64,
}

/// One stage slice of a job summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageCount {
    pub stage_slug: String,
    pub count: i64,
}

// Request bodies

#[derive(Debug, Deserialize)]
pub struct SetEligibilityRequest {
    pub eligible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_rule_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateLinkRequest {
    pub job_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateReferralRequest {
    pub job_id: String,
    pub candidate_name: String,
    pub candidate_email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateReferralRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_phone: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct TransitionReferralRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_stage_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_stage_slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRewardRuleRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub amount_minor: i64,
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_stage_id: Option<String>,
    #[serde(default)]
    pub retention_days: i64,
    #[serde(default)]
    pub eligibility_conditions: Vec<String>,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRewardRuleRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_minor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_stage_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligibility_conditions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateStageRequest {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default)]
    pub is_terminal: bool,
    #[serde(default)]
    pub is_hired_stage: bool,
    #[serde(default)]
    pub is_rejected_stage: bool,
    #[serde(default)]
    pub allowed_next_stage_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateStageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_terminal: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_hired_stage: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_rejected_stage: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_next_stage_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAntiGamingRuleRequest {
    pub name: String,
    pub rule_kind: String,
    #[serde(default = "default_rule_enabled")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_days: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_days: Option<i64>,
    pub enforcement: String,
}

fn default_rule_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct UpdateAntiGamingRuleRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_count: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_days: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_days: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MarkPaidRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VoidRewardRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(
        id: Uuid,
        slug: &str,
        sort_order: i32,
        terminal: bool,
        next: Vec<Uuid>,
    ) -> PipelineStage {
        let now = Utc::now();
        PipelineStage {
            id,
            tenant_id: "tenant-a".to_string(),
            name: slug.to_string(),
            slug: slug.to_string(),
            sort_order,
            is_terminal: terminal,
            is_hired_stage: false,
            is_rejected_stage: false,
            allowed_next_stage_ids: next,
            active: true,
            created_by: "user-1".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn referral(id: Uuid, stage_id: Uuid, slug: &str) -> Referral {
        let now = Utc::now();
        Referral {
            id,
            tenant_id: "tenant-a".to_string(),
            job_id: Uuid::new_v4(),
            referrer_user_id: "user-1".to_string(),
            referrer_verified: false,
            verification_note: "unverified".to_string(),
            candidate_name: "Jane".to_string(),
            candidate_email: "jane@example.com".to_string(),
            candidate_phone: None,
            link_id: None,
            stage_id,
            stage_slug: slug.to_string(),
            source: Some("manual".to_string()),
            notes: None,
            created_by: "user-1".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn pipeline_transition_validation() {
        let submitted = Uuid::new_v4();
        let screening = Uuid::new_v4();
        let interview = Uuid::new_v4();
        let hired = Uuid::new_v4();
        let rejected = Uuid::new_v4();
        let stages = vec![
            stage(submitted, "submitted", 0, false, vec![screening, rejected]),
            stage(screening, "screening", 1, false, vec![interview, rejected]),
            stage(interview, "interview", 2, false, vec![hired, rejected]),
            stage(hired, "hired", 3, true, vec![]),
            stage(rejected, "rejected", 4, true, vec![]),
        ];
        assert!(transition_allowed(&stages, submitted, screening));
        assert!(transition_allowed(&stages, submitted, rejected));
        assert!(transition_allowed(&stages, screening, interview));
        // No skipping ahead: submitted cannot jump to interview.
        assert!(!transition_allowed(&stages, submitted, interview));
        // No backwards moves.
        assert!(!transition_allowed(&stages, screening, submitted));
        // Terminal stages have no outgoing transitions.
        assert!(!transition_allowed(&stages, hired, rejected));
        assert!(!transition_allowed(&stages, rejected, submitted));
        // Same stage is not a transition.
        assert!(!transition_allowed(&stages, submitted, submitted));
        // Unknown stages never transition.
        assert!(!transition_allowed(&stages, Uuid::new_v4(), screening));
        assert!(!transition_allowed(&stages, submitted, Uuid::new_v4()));
        // The initial stage is the lowest-order active one.
        assert_eq!(initial_stage(&stages).unwrap().slug, "submitted");
        assert!(initial_stage(&[]).is_none());
    }

    #[test]
    fn inactive_stage_blocks_transitions() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut stages = vec![
            stage(a, "submitted", 0, false, vec![b]),
            stage(b, "screening", 1, false, vec![]),
        ];
        assert!(transition_allowed(&stages, a, b));
        stages[1].active = false;
        assert!(!transition_allowed(&stages, a, b));
        stages[1].active = true;
        stages[0].active = false;
        assert!(!transition_allowed(&stages, a, b));
        assert!(initial_stage(&stages).is_none());
    }

    #[test]
    fn duplicate_detection_is_case_insensitive() {
        let job = Uuid::new_v4();
        let other_job = Uuid::new_v4();
        let existing = vec![("jane@example.com".to_string(), job)];
        assert!(duplicate_exists(&existing, "jane@example.com", job));
        assert!(duplicate_exists(&existing, "Jane@Example.COM", job));
        assert!(duplicate_exists(&existing, "  jane@example.com  ", job));
        assert!(!duplicate_exists(&existing, "john@example.com", job));
        // Same candidate for a different job is not a duplicate.
        assert!(!duplicate_exists(&existing, "jane@example.com", other_job));
        assert!(!duplicate_exists(&[], "jane@example.com", job));
    }

    #[test]
    fn cooldown_and_cap_math() {
        let now = Utc::now();
        let yesterday = now - chrono::Duration::days(1);
        let ten_days_ago = now - chrono::Duration::days(10);
        assert!(cooldown_violated(Some(yesterday), 30, now));
        assert!(!cooldown_violated(Some(ten_days_ago), 7, now));
        assert!(!cooldown_violated(Some(yesterday), 1, now));
        assert!(!cooldown_violated(Some(yesterday), 0, now));
        assert!(!cooldown_violated(None, 30, now));
        assert!(max_referrals_violated(5, 5));
        assert!(max_referrals_violated(6, 5));
        assert!(!max_referrals_violated(4, 5));
        assert!(!max_referrals_violated(100, 0));
    }

    #[test]
    fn reward_eligibility_math() {
        let now = Utc::now();
        let hired_at = now - chrono::Duration::days(100);
        // Zero retention: payable the moment the stage is reached.
        assert!(reward_eligible(Some(hired_at), 0, now));
        assert_eq!(reward_payable_at(Some(hired_at), 0), Some(hired_at));
        // 90-day retention: not eligible at 30 days, eligible at 100.
        assert!(!reward_eligible(
            Some(now - chrono::Duration::days(30)),
            90,
            now
        ));
        assert!(reward_eligible(Some(hired_at), 90, now));
        assert_eq!(
            reward_payable_at(Some(hired_at), 90),
            Some(hired_at + chrono::Duration::days(90))
        );
        // Trigger stage never reached: never eligible.
        assert!(!reward_eligible(None, 0, now));
        assert!(reward_payable_at(None, 0).is_none());
        // Negative retention is treated as zero.
        assert!(reward_eligible(Some(hired_at), -5, now));
    }

    #[test]
    fn self_referral_needs_a_known_email() {
        assert!(is_self_referral(
            Some("jane@example.com"),
            "Jane@Example.com"
        ));
        assert!(!is_self_referral(
            Some("jane@example.com"),
            "john@example.com"
        ));
        // Unknown referrer email: the check cannot run and never blocks.
        assert!(!is_self_referral(None, "jane@example.com"));
    }

    #[test]
    fn link_token_handling() {
        let token = generate_link_token();
        assert_eq!(token.len(), LINK_TOKEN_LEN);
        assert!(token.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_ne!(generate_link_token(), token);
        let hash = hash_value(&token);
        assert_eq!(hash.len(), 64);
        assert!(valid_sha256_hex(&hash));
        assert_eq!(hash_value(&token), hash);
        assert_ne!(hash_value("other-token"), hash);
        assert!(!valid_sha256_hex("xyz"));
        assert!(!valid_sha256_hex(&"a".repeat(63)));
        // The hash never contains the raw token.
        assert!(!hash.contains(&token));
    }

    #[test]
    fn email_slug_and_currency_validation() {
        assert!(valid_email("jane@example.com"));
        assert!(!valid_email("jane@example"));
        assert!(!valid_email("jane example.com"));
        assert!(!valid_email("jane@@example.com"));
        assert!(!valid_email("@example.com"));
        assert!(!valid_email("jane@.example.com"));
        assert_eq!(normalize_email("  Jane@Example.COM "), "jane@example.com");
        assert!(valid_slug("screening"));
        assert!(valid_slug("phone-screen-2"));
        assert!(!valid_slug("Screening"));
        assert!(!valid_slug("phone screen"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("-screening"));
        assert!(valid_currency("USD"));
        assert!(valid_currency("IDR"));
        assert!(!valid_currency("usd"));
        assert!(!valid_currency("US"));
        assert!(!valid_currency("USDD"));
    }

    #[test]
    fn counts_by_stage_include_empty_stages() {
        let submitted = Uuid::new_v4();
        let screening = Uuid::new_v4();
        let hired = Uuid::new_v4();
        let stages = vec![
            stage(submitted, "submitted", 0, false, vec![screening]),
            stage(screening, "screening", 1, false, vec![hired]),
            stage(hired, "hired", 2, true, vec![]),
        ];
        let referrals = vec![
            referral(Uuid::new_v4(), submitted, "submitted"),
            referral(Uuid::new_v4(), submitted, "submitted"),
            referral(Uuid::new_v4(), hired, "hired"),
        ];
        let counts = counts_by_stage(&stages, &referrals);
        assert_eq!(
            counts,
            vec![
                ("submitted".to_string(), 2),
                ("screening".to_string(), 0),
                ("hired".to_string(), 1),
            ]
        );
    }

    #[test]
    fn vocabulary_parsing() {
        assert_eq!(
            RewardStatus::from_str("pending"),
            Some(RewardStatus::Pending)
        );
        assert_eq!(
            RewardStatus::from_str("eligible"),
            Some(RewardStatus::Eligible)
        );
        assert_eq!(RewardStatus::from_str("paid"), Some(RewardStatus::Paid));
        assert_eq!(RewardStatus::from_str("voided"), Some(RewardStatus::Voided));
        assert_eq!(RewardStatus::from_str("sent"), None);
        assert_eq!(
            AntiGamingKind::from_str("block_self_referral"),
            Some(AntiGamingKind::BlockSelfReferral)
        );
        assert_eq!(
            AntiGamingKind::from_str("same_candidate_cooldown"),
            Some(AntiGamingKind::SameCandidateCooldown)
        );
        assert_eq!(
            AntiGamingKind::from_str("max_referrals_per_period"),
            Some(AntiGamingKind::MaxReferralsPerPeriod)
        );
        assert_eq!(
            AntiGamingKind::from_str("require_active_employee"),
            Some(AntiGamingKind::RequireActiveEmployee)
        );
        assert_eq!(AntiGamingKind::from_str("block_everything"), None);
        assert_eq!(Enforcement::from_str("block"), Some(Enforcement::Block));
        assert_eq!(Enforcement::from_str("flag"), Some(Enforcement::Flag));
        assert_eq!(Enforcement::from_str("warn"), None);
    }
}
