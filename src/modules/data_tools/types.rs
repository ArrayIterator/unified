// data_tools - Types: rules, runs, scan reports, and the minimal
// document share record.
//
// Retention lives in `cleanup_rules` (migration 000125) as data; the
// defaults below are configurable defaults only, never hard-coded
// retention in the resolver. A rule's `dry_run_default` is the
// dry-run flag a run uses when the caller does not pass one
// explicitly; `cross_check` defaults to dry-run because it is
// report-only and never deletes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Read capability: list rules, scan previews, run history.
pub(crate) const PERM_READ: &str = "data_tools:read";
/// Admin capability: change rules, run cleanups, open the admin page.
pub(crate) const PERM_ADMIN: &str = "data_tools:admin";

/// One cleanup target the v1 resolver knows. The `target` string is
/// the stable machine name used in the `cleanup_rules` table, in the
/// `data_tools.cleanup.<target>` hook event, and in scan/run reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TargetDef {
    /// Stable machine name, e.g. `document_shares`.
    pub target: &'static str,
    /// Grace period (days after expiry / revocation / event) before a
    /// row becomes purgeable. 0 = purge as soon as it is expired.
    pub retention_days: i32,
    /// The dry-run flag a run uses when the caller passes none.
    pub dry_run_default: bool,
    /// Short human description, shown on the admin page.
    pub description: &'static str,
    /// True when Data Tools itself counts / deletes for this target
    /// (the in-memory share map). False targets are answered by their
    /// owning module through the `data_tools.cleanup.<target>` hook.
    pub owned: bool,
}

/// The v1 targets, in scan order. `cross_check` is last and
/// report-only: it counts orphans but never deletes.
pub const TARGETS: &[TargetDef] = &[
    TargetDef {
        target: "document_shares",
        retention_days: 30,
        dry_run_default: false,
        description: "Document shares expired or revoked beyond grace.",
        owned: true,
    },
    TargetDef {
        target: "document_upload_sessions",
        retention_days: 7,
        dry_run_default: false,
        description: "Expired chunked-upload sessions and their spool dirs.",
        owned: false,
    },
    TargetDef {
        target: "auth_revoked_tokens",
        retention_days: 30,
        dry_run_default: false,
        description: "Revoked auth tokens whose expiry passed.",
        owned: false,
    },
    TargetDef {
        target: "document_downloads",
        retention_days: 90,
        dry_run_default: false,
        description: "Download log rows older than retention.",
        owned: false,
    },
    TargetDef {
        target: "spool_temps",
        retention_days: 1,
        dry_run_default: false,
        description: "Temporary / spool files older than 24h.",
        owned: false,
    },
    TargetDef {
        target: "cross_check",
        retention_days: 0,
        dry_run_default: true,
        description: "Files on disk with no DB row / DB rows with no file. Report-only: never deletes.",
        owned: false,
    },
];

/// One retention rule (a row of `cleanup_rules`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupRule {
    pub id: String,
    pub target: String,
    pub retention_days: i32,
    pub enabled: bool,
    pub dry_run_default: bool,
    /// `tenant` or `platform`. v1 ships platform rules only;
    /// per-tenant rules are follow-up work.
    pub scope: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The per-target outcome of one scan or run. `counted` is how many
/// rows / objects matched the rule; `removed` is how many were
/// actually deleted (always 0 on a dry run, and always 0 for
/// `cross_check`). `note` explains a zero honestly, for example when
/// no owning module answered the hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetOutcome {
    pub target: String,
    pub counted: u64,
    pub removed: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The preview a scan returns: counts per target, zero deletes.
#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub dry_run: bool,
    pub scanned_at: DateTime<Utc>,
    pub targets: Vec<TargetOutcome>,
}

/// One cleanup run (a row of `cleanup_runs`). This record IS the audit
/// trail for the run: every delete was counted first (see the scan)
/// and the counts + errors land here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupRun {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub dry_run: bool,
    /// `running`, `completed`, or `failed`.
    pub status: String,
    pub counts: Vec<TargetOutcome>,
    #[serde(default)]
    pub errors: Vec<String>,
    /// `manual`, `scheduler`, or `api`.
    pub triggered_by: String,
}

/// Minimal document share row (migration 000125). Signed download
/// URLs are stateless HMAC today and no sharing feature exists yet;
/// Data Tools owns this table until the Documents module grows real
/// sharing. The cleaner deletes only the share row - never the
/// document, never an audit event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentShareRecord {
    pub id: String,
    pub document_id: String,
    /// `owner`, `company`, `module`, or `public`.
    pub scope: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked: bool,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// Request body for `POST /api/v1/data-tools/run`. `dry_run` is
/// optional: when omitted, each target uses its rule's
/// `dry_run_default`; when present, it overrides every rule for
/// this run. The safe default for a manual run is therefore whatever
/// the rules say (real deletes for the shipped defaults, except the
/// report-only `cross_check`).
#[derive(Debug, Clone, Deserialize)]
pub struct RunRequest {
    pub dry_run: Option<bool>,
}

/// Request body for `PATCH /api/v1/data-tools/rules/{id}`. Every
/// field is optional; omitted fields keep their value. `target` and
/// `scope` are immutable through this endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct RulePatch {
    pub retention_days: Option<i32>,
    pub enabled: Option<bool>,
    pub dry_run_default: Option<bool>,
}
