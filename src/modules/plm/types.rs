//! PLM types: data-driven lifecycle stages, product versions
//! with revision math, engineering change orders with a status
//! machine, versioned BOM snapshots, data-driven approval
//! steps, and a full audit trail.
//!
//! Nothing about a lifecycle is hard-coded here: stages are
//! rows (an admin defines them, their order, their allowed
//! transitions, and how many approvals entering a stage
//! requires), ECO types live in per-tenant settings, and
//! approval steps are data stored on the ECO itself. The only
//! fixed lists are the ones that are pure state machines with
//! no tenant meaning of their own: the ECO status machine and
//! the approval decision values. Money is always integer minor
//! units; this module carries no money fields at all — cost
//! impact on an ECO is recorded as a free-text assessment plus
//! an impact level, never as a price.
//!
//! References to other modules are by id only, with no foreign
//! key: `product_id` points at a product in the products
//! module, `bom_id` at a BOM in the manufacturing module, and
//! `document_ids` at documents in the documents module.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_SLUG_LEN: usize = 128;

pub(crate) const MAX_SUMMARY_LEN: usize = 5000;

pub(crate) const MAX_CODE_LEN: usize = 64;

pub(crate) const MAX_COMMENT_LEN: usize = 2000;

pub(crate) const MAX_IMPACT_LEN: usize = 5000;

/// Maximum ids in one reference list (affected products,
/// affected versions, linked documents).
pub(crate) const MAX_IDS: usize = 100;

/// Maximum approval steps on one ECO.
pub(crate) const MAX_STEPS: usize = 20;

/// Maximum ECO types in per-tenant settings.
pub(crate) const MAX_ECO_TYPES: usize = 50;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_PLM_VIEW: &str = "plm.view";

pub(crate) const PERM_PLM_MANAGE: &str = "plm.manage";

/// Hook event names this module listens to.
pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

pub(crate) const EVENT_PRODUCT_DELETED: &str = "product.deleted";

pub(crate) const EVENT_BOM_VERSION_CREATED: &str = "manufacturing.bom.version.created";

/// Events this module emits.
pub(crate) const EVENT_STAGE_CHANGED: &str = "plm.stage.changed";

pub(crate) const EVENT_VERSION_CREATED: &str = "plm.version.created";

pub(crate) const EVENT_ECO_CREATED: &str = "plm.eco.created";

pub(crate) const EVENT_ECO_APPROVED: &str = "plm.eco.approved";

pub(crate) const EVENT_ECO_IMPLEMENTED: &str = "plm.eco.implemented";

pub(crate) const EVENT_BOM_VERSIONED: &str = "plm.bom.versioned";

/// Marker written into actor fields when the owning user is
/// deleted. Rows are kept (versions, ECOs, and approvals stay
/// auditable); only the identity is anonymized.
pub(crate) const DELETED_USER_ID: &str = "deleted-user";

/// Default ECO types seeded into per-tenant settings. They are
/// data: an admin extends or replaces the list through the
/// settings endpoints, and ECO creation validates against the
/// stored list.
pub(crate) const DEFAULT_ECO_TYPES: &[&str] = &[
    "design_change",
    "material_change",
    "process_change",
    "cost_reduction",
    "compliance",
    "supplier_change",
];

/// Impact levels for an ECO impact assessment. Data values,
/// validated on write so a typo never becomes a level.
pub(crate) const IMPACT_LEVELS: &[&str] = &["low", "medium", "high", "critical"];

// ---------------------------------------------------------------------------
// Pure domain functions (unit-tested below)
// ---------------------------------------------------------------------------

/// One default lifecycle stage definition. The default set is
/// seed data, not an enum: an admin may rename, reorder,
/// rewire, or delete any of these after seeding, and may add
/// stages of their own.
#[derive(Debug, Clone)]
pub struct StageDefinition {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub sort_order: i64,
    pub is_initial: bool,
    pub is_terminal: bool,
    pub allowed_transitions: &'static [&'static str],
    pub required_approvals: i64,
}

/// Default lifecycle stages, seeded on request through
/// `POST /stages/seed`. The graph is linear with a step back
/// from design and prototype so rework is possible; production
/// and phase-out both require one recorded approval before a
/// version may enter them.
pub fn default_stage_definitions() -> Vec<StageDefinition> {
    vec![
        StageDefinition {
            slug: "concept",
            name: "Concept",
            description: "Idea and feasibility: the product is being scoped, not yet designed.",
            sort_order: 10,
            is_initial: true,
            is_terminal: false,
            allowed_transitions: &["design"],
            required_approvals: 0,
        },
        StageDefinition {
            slug: "design",
            name: "Design",
            description: "Detailed design: specifications, drawings, and materials are defined.",
            sort_order: 20,
            is_initial: false,
            is_terminal: false,
            allowed_transitions: &["prototype", "concept"],
            required_approvals: 0,
        },
        StageDefinition {
            slug: "prototype",
            name: "Prototype",
            description: "Prototype: physical or pilot builds are tested and iterated.",
            sort_order: 30,
            is_initial: false,
            is_terminal: false,
            allowed_transitions: &["production", "design"],
            required_approvals: 0,
        },
        StageDefinition {
            slug: "production",
            name: "Production",
            description: "Production: the product is released for manufacturing and sale.",
            sort_order: 40,
            is_initial: false,
            is_terminal: false,
            allowed_transitions: &["phase_out"],
            required_approvals: 1,
        },
        StageDefinition {
            slug: "phase_out",
            name: "Phase-Out",
            description: "Phase-out: the product is being retired; no new production runs.",
            sort_order: 50,
            is_initial: false,
            is_terminal: true,
            allowed_transitions: &[],
            required_approvals: 1,
        },
    ]
}

/// True when the stage definition allows a transition to
/// `target_slug`. The graph is data (stored on the stage row),
/// so this checks the row, not a hard-coded list.
pub fn stage_transition_allowed(stage: &PlmStage, target_slug: &str) -> bool {
    stage
        .allowed_transitions
        .iter()
        .any(|slug| slug == target_slug)
}

/// Parse a `major.minor.patch` revision label. Returns `None`
/// for anything that is not three non-negative integers.
pub fn parse_revision(label: &str) -> Option<(i64, i64, i64)> {
    let parts: Vec<&str> = label.trim().split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut numbers = [0i64; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        numbers[index] = part.parse::<i64>().ok()?;
    }
    Some((numbers[0], numbers[1], numbers[2]))
}

/// Render a revision label from its parts.
pub fn revision_label(major: i64, minor: i64, patch: i64) -> String {
    format!("{major}.{minor}.{patch}")
}

/// Next revision after `bump` (`major`, `minor`, or `patch`).
/// A major bump resets minor and patch to zero; a minor bump
/// resets patch to zero. Returns `None` for an unknown bump
/// kind or a non-positive base part, so the caller rejects the
/// request instead of inventing a revision.
pub fn next_revision(major: i64, minor: i64, patch: i64, bump: &str) -> Option<(i64, i64, i64)> {
    if major < 0 || minor < 0 || patch < 0 {
        return None;
    }
    match bump {
        "major" => Some((major + 1, 0, 0)),
        "minor" => Some((major, minor + 1, 0)),
        "patch" => Some((major, minor, patch + 1)),
        _ => None,
    }
}

/// Compare two revisions: negative when `a` is older than `b`,
/// zero when equal, positive when `a` is newer.
pub fn compare_revisions(a: (i64, i64, i64), b: (i64, i64, i64)) -> i64 {
    if a.0 != b.0 {
        return a.0 - b.0;
    }
    if a.1 != b.1 {
        return a.1 - b.1;
    }
    a.2 - b.2
}

/// ECO status machine. Allowed transitions:
/// draft -> review; review -> approved, rejected;
/// approved -> implemented, rejected; implemented -> closed;
/// rejected -> draft (rework). Closed is terminal. Anything
/// else is rejected.
pub fn eco_can_transition(from: EcoStatus, to: EcoStatus) -> bool {
    use EcoStatus::{Approved, Closed, Draft, Implemented, Rejected, Review};
    matches!(
        (from, to),
        (Draft, Review)
            | (Review, Approved)
            | (Review, Rejected)
            | (Approved, Implemented)
            | (Approved, Rejected)
            | (Implemented, Closed)
            | (Rejected, Draft)
    )
}

/// True when every required approval step on the ECO has an
/// approved decision. Steps marked not required never block.
/// `decisions` is `(step_index, decision)` pairs.
pub fn eco_approvals_complete(
    steps: &[ApprovalStep],
    decisions: &[(i64, ApprovalDecision)],
) -> bool {
    steps.iter().enumerate().all(|(index, step)| {
        if !step.required {
            return true;
        }
        decisions.iter().any(|(step_index, decision)| {
            *step_index as usize == index && *decision == ApprovalDecision::Approved
        })
    })
}

/// Field-by-field diff metadata between two versions of the
/// same product. Each entry names the field and the value on
/// each side, so a caller can render a change summary without
/// fetching both records again. Identical fields are omitted.
pub fn diff_versions(a: &PlmVersion, b: &PlmVersion) -> Vec<Value> {
    let mut changes: Vec<Value> = Vec::new();
    let mut push = |field: &str, from: Value, to: Value| {
        if from != to {
            changes.push(serde_json::json!({
                "field": field,
                "from": from,
                "to": to,
            }));
        }
    };
    push(
        "revision",
        Value::String(a.revision()),
        Value::String(b.revision()),
    );
    push(
        "status",
        Value::String(a.status.clone()),
        Value::String(b.status.clone()),
    );
    push(
        "change_summary",
        Value::String(a.change_summary.clone()),
        Value::String(b.change_summary.clone()),
    );
    push(
        "effective_from",
        a.effective_from
            .map(|value| Value::String(value.to_rfc3339()))
            .unwrap_or(Value::Null),
        b.effective_from
            .map(|value| Value::String(value.to_rfc3339()))
            .unwrap_or(Value::Null),
    );
    push(
        "effective_to",
        a.effective_to
            .map(|value| Value::String(value.to_rfc3339()))
            .unwrap_or(Value::Null),
        b.effective_to
            .map(|value| Value::String(value.to_rfc3339()))
            .unwrap_or(Value::Null),
    );
    push(
        "is_current",
        Value::Bool(a.is_current),
        Value::Bool(b.is_current),
    );
    changes
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One lifecycle stage, defined as data. `allowed_transitions`
/// names the stage slugs a version may move to from here;
/// `required_approvals` is how many approved version
/// approvals must exist before a version may enter this stage
/// (0 means no approval gate). `is_initial` marks the stage
/// new versions start in; `is_terminal` marks a stage with no
/// way out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlmStage {
    pub id: Uuid,
    pub tenant_id: String,
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub sort_order: i64,
    pub is_initial: bool,
    pub is_terminal: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_transitions: Vec<String>,
    pub required_approvals: i64,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One product version (revision). `product_id` references a
/// product in the products module by id only (no foreign key).
/// The revision is three integers rendered as
/// `major.minor.patch`; `status` is the slug of the lifecycle
/// stage this version is in (data, not an enum — it must match
/// an existing stage of the tenant). `supersedes_version_id`
/// links to the previous revision of the same product.
/// `is_current` marks the revision customers and manufacturing
/// should use; only one version per product is current at a
/// time. `product_deleted` is set by the `product.deleted`
/// listener so a deleted product's history stays visible and
/// auditable instead of vanishing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlmVersion {
    pub id: Uuid,
    pub tenant_id: String,
    pub product_id: String,
    pub major: i64,
    pub minor: i64,
    pub patch: i64,
    pub change_summary: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_version_id: Option<Uuid>,
    pub is_current: bool,
    pub product_deleted: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PlmVersion {
    pub(crate) fn revision(&self) -> String {
        revision_label(self.major, self.minor, self.patch)
    }
}

/// ECO status. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EcoStatus {
    Draft,
    Review,
    Approved,
    Implemented,
    Closed,
    Rejected,
}

impl Default for EcoStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl EcoStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Review => "review",
            Self::Approved => "approved",
            Self::Implemented => "implemented",
            Self::Closed => "closed",
            Self::Rejected => "rejected",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "review" => Some(Self::Review),
            "approved" => Some(Self::Approved),
            "implemented" => Some(Self::Implemented),
            "closed" => Some(Self::Closed),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

/// One approval step definition, stored as data on the ECO.
/// `approver_role` is a role slug (for example `engineering`
/// or `quality`); the step is decided by a caller holding that
/// role. `required` steps must be approved before the ECO may
/// move from review to approved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalStep {
    pub name: String,
    pub approver_role: String,
    #[serde(default = "default_required_true")]
    pub required: bool,
}

fn default_required_true() -> bool {
    true
}

/// One engineering change order. `code` is unique per tenant
/// (for example `ECO-0001`). `eco_type` is a value from the
/// tenant's settings list, not a hard-coded enum.
/// `affected_product_ids` and `affected_version_ids` are
/// references by id only; `linked_document_ids` are document
/// references by id only. `impact_level` is one of `low`,
/// `medium`, `high`, `critical`; `impact_assessment` is the
/// free-text analysis. `approval_steps` is the ordered,
/// data-driven approval workflow for this ECO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlmEco {
    pub id: Uuid,
    pub tenant_id: String,
    pub code: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub eco_type: String,
    pub status: EcoStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_product_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_version_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_document_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_assessment: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approval_steps: Vec<ApprovalStep>,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Approval decision. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Pending,
    Approved,
    Rejected,
}

impl Default for ApprovalDecision {
    fn default() -> Self {
        Self::Pending
    }
}

impl ApprovalDecision {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

/// One approval decision record. `entity_type` is `eco` or
/// `version` and `entity_id` is the ECO or version the decision
/// belongs to. For an ECO, `step_index` points at the ECO's
/// `approval_steps` entry (one decision per step). For a
/// version, `step_index` is 0 and the record counts towards
/// the target stage's `required_approvals` gate. `approver_role`
/// is the role slug the decision was made under; `approver_id`
/// is the deciding user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlmApproval {
    pub id: Uuid,
    pub tenant_id: String,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub step_index: i64,
    pub step_name: String,
    pub approver_role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approver_id: Option<String>,
    pub decision: ApprovalDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One BOM snapshot. `bom_id` references a manufacturing BOM
/// by id only (no foreign key); `version_number` is strictly
/// increasing per BOM. `eco_id` links the snapshot to the ECO
/// that drove the change, when there is one — an ECO must be
/// approved or implemented before it may be linked.
/// `snapshot` is the component/line snapshot as stored at
/// versioning time; `effective_from` is when the snapshot
/// takes effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlmBomVersion {
    pub id: Uuid,
    pub tenant_id: String,
    pub bom_id: String,
    pub version_number: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eco_id: Option<Uuid>,
    #[serde(default)]
    pub snapshot: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// One audit trail entry. Every stage change, version
/// creation, ECO status change, approval decision, and BOM
/// versioning writes one row here, so the full history of a
/// product's lifecycle can be replayed from this table alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlmAuditEntry {
    pub id: Uuid,
    pub tenant_id: String,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub action: String,
    pub actor_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_value: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Per-tenant PLM settings. `eco_types` is the data-driven
/// list ECO creation validates against; `require_eco_for_bom`
/// makes an ECO link mandatory on every BOM version when
/// true.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlmSettings {
    pub eco_types: Vec<String>,
    pub require_eco_for_bom: bool,
}

impl Default for PlmSettings {
    fn default() -> Self {
        Self {
            eco_types: DEFAULT_ECO_TYPES
                .iter()
                .map(|item| item.to_string())
                .collect(),
            require_eco_for_bom: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateStageRequest {
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub is_initial: bool,
    #[serde(default)]
    pub is_terminal: bool,
    #[serde(default)]
    pub allowed_transitions: Vec<String>,
    #[serde(default)]
    pub required_approvals: i64,
    #[serde(default = "default_true")]
    pub active: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct UpdateStageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_initial: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_terminal: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_transitions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_approvals: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateVersionRequest {
    pub product_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub major: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_version_id: Option<Uuid>,
    /// `major`, `minor`, or `patch`: bump the superseded
    /// revision instead of supplying explicit numbers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bump: Option<String>,
    pub change_summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub is_current: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateVersionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<Option<DateTime<Utc>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_to: Option<Option<DateTime<Utc>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_current: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct VersionTransitionRequest {
    pub to_stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateApprovalRequest {
    /// For an ECO: the index into its `approval_steps`.
    /// For a version: 0.
    #[serde(default)]
    pub step_index: i64,
    pub approver_role: String,
    /// `approved` or `rejected`.
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateEcoRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub eco_type: String,
    #[serde(default)]
    pub affected_product_ids: Vec<String>,
    #[serde(default)]
    pub affected_version_ids: Vec<Uuid>,
    #[serde(default)]
    pub linked_document_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_assessment: Option<String>,
    #[serde(default)]
    pub approval_steps: Vec<ApprovalStep>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEcoRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eco_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_product_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_version_ids: Option<Vec<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_document_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_level: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_assessment: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_steps: Option<Vec<ApprovalStep>>,
}

#[derive(Debug, Deserialize)]
pub struct EcoTransitionRequest {
    /// Target status: `review`, `approved`, `implemented`,
    /// `closed`, `rejected`, or `draft`.
    pub to_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBomVersionRequest {
    pub bom_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_number: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eco_id: Option<Uuid>,
    #[serde(default)]
    pub snapshot: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eco_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_eco_for_bom: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage_with_transitions(transitions: &[&str], required: i64) -> PlmStage {
        PlmStage {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            slug: "design".to_string(),
            name: "Design".to_string(),
            description: None,
            sort_order: 20,
            is_initial: false,
            is_terminal: false,
            allowed_transitions: transitions.iter().map(|item| item.to_string()).collect(),
            required_approvals: required,
            active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn version(revision: (i64, i64, i64), status: &str, summary: &str) -> PlmVersion {
        PlmVersion {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            product_id: "product-1".to_string(),
            major: revision.0,
            minor: revision.1,
            patch: revision.2,
            change_summary: summary.to_string(),
            status: status.to_string(),
            effective_from: None,
            effective_to: None,
            supersedes_version_id: None,
            is_current: false,
            product_deleted: false,
            created_by: "user-1".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn default_stages_form_a_connected_graph() {
        let stages = default_stage_definitions();
        assert_eq!(stages.len(), 5);
        let slugs: Vec<&str> = stages.iter().map(|stage| stage.slug).collect();
        assert_eq!(
            slugs,
            vec!["concept", "design", "prototype", "production", "phase_out"]
        );
        // Exactly one initial stage and one terminal stage.
        assert_eq!(stages.iter().filter(|stage| stage.is_initial).count(), 1);
        assert_eq!(stages.iter().filter(|stage| stage.is_terminal).count(), 1);
        // Every transition target is a defined stage.
        for stage in &stages {
            for target in stage.allowed_transitions {
                assert!(slugs.contains(target), "unknown target {target}");
            }
        }
        // Sort orders are strictly increasing.
        let mut orders: Vec<i64> = stages.iter().map(|stage| stage.sort_order).collect();
        let sorted = {
            let mut copy = orders.clone();
            copy.sort_unstable();
            copy
        };
        assert_eq!(orders, sorted);
        orders.dedup();
        assert_eq!(orders.len(), stages.len());
        // Production and phase-out require an approval to enter.
        assert_eq!(stages[3].required_approvals, 1);
        assert_eq!(stages[4].required_approvals, 1);
        assert_eq!(stages[0].required_approvals, 0);
    }

    #[test]
    fn stage_transition_allowed_reads_the_row() {
        let stage = stage_with_transitions(&["prototype", "concept"], 0);
        assert!(stage_transition_allowed(&stage, "prototype"));
        assert!(stage_transition_allowed(&stage, "concept"));
        assert!(!stage_transition_allowed(&stage, "production"));
        let terminal = stage_with_transitions(&[], 1);
        assert!(!stage_transition_allowed(&terminal, "concept"));
    }

    #[test]
    fn revision_parse_and_label_round_trip() {
        assert_eq!(parse_revision("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_revision("0.0.0"), Some((0, 0, 0)));
        assert_eq!(parse_revision("10.20.30"), Some((10, 20, 30)));
        assert_eq!(revision_label(1, 2, 3), "1.2.3");
        assert_eq!(parse_revision("1.2"), None);
        assert_eq!(parse_revision("1.2.3.4"), None);
        assert_eq!(parse_revision("1.x.3"), None);
        assert_eq!(parse_revision("1.-2.3"), None);
        assert_eq!(parse_revision(""), None);
        assert_eq!(parse_revision("v1.2.3"), None);
    }

    #[test]
    fn next_revision_bumps_reset_lower_parts() {
        assert_eq!(next_revision(1, 2, 3, "major"), Some((2, 0, 0)));
        assert_eq!(next_revision(1, 2, 3, "minor"), Some((1, 3, 0)));
        assert_eq!(next_revision(1, 2, 3, "patch"), Some((1, 2, 4)));
        assert_eq!(next_revision(0, 0, 0, "major"), Some((1, 0, 0)));
        assert_eq!(next_revision(1, 2, 3, "hotfix"), None);
        assert_eq!(next_revision(1, 2, 3, ""), None);
        assert_eq!(next_revision(-1, 0, 0, "patch"), None);
        assert_eq!(next_revision(1, -1, 0, "minor"), None);
    }

    #[test]
    fn compare_revisions_orders_semver_parts() {
        assert!(compare_revisions((1, 0, 0), (2, 0, 0)) < 0);
        assert!(compare_revisions((2, 0, 0), (1, 9, 9)) > 0);
        assert!(compare_revisions((1, 2, 0), (1, 1, 9)) > 0);
        assert!(compare_revisions((1, 2, 3), (1, 2, 4)) < 0);
        assert_eq!(compare_revisions((1, 2, 3), (1, 2, 3)), 0);
    }

    #[test]
    fn eco_status_machine_transitions() {
        use EcoStatus::{Approved, Closed, Draft, Implemented, Rejected, Review};
        assert!(eco_can_transition(Draft, Review));
        assert!(!eco_can_transition(Draft, Approved));
        assert!(!eco_can_transition(Draft, Closed));
        assert!(eco_can_transition(Review, Approved));
        assert!(eco_can_transition(Review, Rejected));
        assert!(!eco_can_transition(Review, Implemented));
        assert!(eco_can_transition(Approved, Implemented));
        assert!(eco_can_transition(Approved, Rejected));
        assert!(!eco_can_transition(Approved, Closed));
        assert!(eco_can_transition(Implemented, Closed));
        assert!(!eco_can_transition(Implemented, Approved));
        assert!(eco_can_transition(Rejected, Draft));
        assert!(!eco_can_transition(Rejected, Review));
        assert!(!eco_can_transition(Closed, Draft));
        assert!(!eco_can_transition(Draft, Draft));
    }

    #[test]
    fn eco_approvals_complete_requires_required_steps_only() {
        let steps = vec![
            ApprovalStep {
                name: "Engineering".to_string(),
                approver_role: "engineering".to_string(),
                required: true,
            },
            ApprovalStep {
                name: "Quality".to_string(),
                approver_role: "quality".to_string(),
                required: true,
            },
            ApprovalStep {
                name: "Advisory".to_string(),
                approver_role: "advisor".to_string(),
                required: false,
            },
        ];
        assert!(!eco_approvals_complete(&steps, &[]));
        assert!(!eco_approvals_complete(
            &steps,
            &[(0, ApprovalDecision::Approved)]
        ));
        assert!(eco_approvals_complete(
            &steps,
            &[
                (0, ApprovalDecision::Approved),
                (1, ApprovalDecision::Approved)
            ]
        ));
        // A rejected required step does not count as approved.
        assert!(!eco_approvals_complete(
            &steps,
            &[
                (0, ApprovalDecision::Approved),
                (1, ApprovalDecision::Rejected)
            ]
        ));
        // No steps at all: nothing blocks.
        assert!(eco_approvals_complete(&[], &[]));
    }

    #[test]
    fn version_diff_lists_only_changed_fields() {
        let mut a = version((1, 0, 0), "design", "First draft");
        let mut b = version((1, 1, 0), "prototype", "First draft");
        b.is_current = true;
        let changes = diff_versions(&a, &b);
        let fields: Vec<&str> = changes
            .iter()
            .filter_map(|change| change.get("field")?.as_str())
            .collect();
        assert_eq!(fields, vec!["revision", "status", "is_current"]);
        // Identical versions diff to nothing.
        a.is_current = true;
        let b_same = version((1, 0, 0), "design", "First draft");
        let mut b_same = b_same;
        b_same.is_current = true;
        assert!(diff_versions(&a, &b_same).is_empty());
    }

    #[test]
    fn eco_status_round_trips() {
        for (status, text) in [
            (EcoStatus::Draft, "draft"),
            (EcoStatus::Review, "review"),
            (EcoStatus::Approved, "approved"),
            (EcoStatus::Implemented, "implemented"),
            (EcoStatus::Closed, "closed"),
            (EcoStatus::Rejected, "rejected"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(EcoStatus::from_str(text), Some(status));
        }
        assert_eq!(EcoStatus::from_str("shipped"), None);
    }

    #[test]
    fn approval_decision_round_trips() {
        for (decision, text) in [
            (ApprovalDecision::Pending, "pending"),
            (ApprovalDecision::Approved, "approved"),
            (ApprovalDecision::Rejected, "rejected"),
        ] {
            assert_eq!(decision.as_str(), text);
            assert_eq!(ApprovalDecision::from_str(text), Some(decision));
        }
        assert_eq!(ApprovalDecision::from_str("maybe"), None);
    }

    #[test]
    fn default_eco_types_are_known() {
        let settings = PlmSettings::default();
        assert!(settings.eco_types.contains(&"design_change".to_string()));
        assert!(settings.eco_types.contains(&"compliance".to_string()));
        assert!(!settings.require_eco_for_bom);
        assert_eq!(settings.eco_types.len(), DEFAULT_ECO_TYPES.len());
    }
}
