// data_tools — In-memory state and the resolver.
//
// The DashMap stores are the runtime authority (same convention as
// the other batch modules); the tables in migration 000125 are the
// persistence targets for the C11 write-through. Nothing here touches
// another module's tables: hook targets (`document_upload_sessions`,
// `auth_revoked_tokens`, `document_downloads`, `spool_temps`,
// `cross_check`) are reached ONLY through the
// `data_tools.cleanup.<target>` hook event, and a target with no
// answering owner reports zero with a note instead of crashing.
// The one owned target is `document_shares` (see the migration
// header: no sharing feature exists yet, so Data Tools owns the
// minimal table until the Documents module grows real sharing).

use super::events::cleanup_event_for;
use super::*;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, RwLock, TryLockError};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct DataToolsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    /// Rules keyed by target machine name.
    pub(crate) rules: DashMap<String, CleanupRule>,
    /// Runs keyed by run id.
    pub(crate) runs: DashMap<String, CleanupRun>,
    /// Share rows keyed by share id. The runtime authority for the
    /// `document_shares` table until real sharing lands in the
    /// Documents module (see the migration header).
    pub(crate) shares: DashMap<String, DocumentShareRecord>,
    /// Serializes runs: one cleanup at a time. `try_lock` is used on
    /// purpose — a second concurrent run gets 409 instead of waiting,
    /// and a hook handler that re-enters a run can never deadlock on
    /// this lock. The guard is only ever held across synchronous map
    /// work and hook emission, never across an await.
    pub(crate) run_lock: Mutex<()>,
}

impl DataToolsState {
    pub fn new() -> Self {
        let state = Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            rules: DashMap::new(),
            runs: DashMap::new(),
            shares: DashMap::new(),
            run_lock: Mutex::new(()),
        };
        state.seed_default_rules();
        state
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Seed the default rules from `TARGETS` (idempotent: an
    /// existing rule keeps its admin-tuned values).
    fn seed_default_rules(&self) {
        let now = now();
        for def in TARGETS {
            self.rules
                .entry(def.target.to_string())
                .or_insert_with(|| CleanupRule {
                    id: new_id(),
                    target: def.target.to_string(),
                    retention_days: def.retention_days,
                    enabled: true,
                    dry_run_default: def.dry_run_default,
                    scope: "platform".to_string(),
                    description: def.description.to_string(),
                    created_at: now,
                    updated_at: now,
                });
        }
    }

    /// All rules, sorted by target name.
    pub fn list_rules(&self) -> Vec<CleanupRule> {
        let mut rules: Vec<CleanupRule> = self
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        rules.sort_by(|a, b| a.target.cmp(&b.target));
        rules
    }

    /// One rule by its id (the `PATCH /rules/{id}` path key).
    pub fn rule_by_id(&self, id: &str) -> Option<CleanupRule> {
        self.rules
            .iter()
            .map(|entry| entry.value().clone())
            .find(|rule| rule.id == id)
    }

    /// The rule for one target, if the target is known and has a rule.
    pub fn rule_for(&self, target: &str) -> Option<CleanupRule> {
        self.rules.get(target).map(|entry| entry.value().clone())
    }

    /// Apply a rule patch. `retention_days` is validated to
    /// 0..=3650 (0 = purge as soon as expired); `target` and `scope`
    /// are immutable through this endpoint.
    pub fn update_rule(&self, id: &str, patch: &RulePatch) -> UnifiedResult<CleanupRule> {
        if let Some(retention_days) = patch.retention_days {
            if !(0..=3650).contains(&retention_days) {
                return Err(UnifiedError::bad_request("retention_days must be 0..=3650"));
            }
        }
        let mut rule = self.rule_by_id(id).ok_or_else(|| {
            UnifiedError::not_found(format!("Cleanup rule \"{id}\" does not exist"))
        })?;
        if let Some(retention_days) = patch.retention_days {
            rule.retention_days = retention_days;
        }
        if let Some(enabled) = patch.enabled {
            rule.enabled = enabled;
        }
        if let Some(dry_run_default) = patch.dry_run_default {
            rule.dry_run_default = dry_run_default;
        }
        rule.updated_at = now();
        self.rules.insert(rule.target.clone(), rule.clone());
        Ok(rule)
    }

    /// Insert one share row as-is (used by tests and, later, by the
    /// sharing feature when it takes over this table).
    pub(crate) fn insert_share(&self, share: DocumentShareRecord) {
        self.shares.insert(share.id.clone(), share);
    }

    /// A share is purgeable when it expired beyond the rule's grace
    /// period, or was revoked beyond grace. A revoked share with no
    /// `revoked_at` is purgeable immediately: revocation is the
    /// operator's explicit decision and the row grants nothing any
    /// more, so there is no grace left to measure.
    fn share_is_purgeable(
        share: &DocumentShareRecord,
        retention_days: i32,
        at: DateTime<Utc>,
    ) -> bool {
        let grace = Duration::days(i64::from(retention_days));
        if let Some(expires_at) = share.expires_at {
            if expires_at + grace < at {
                return true;
            }
        }
        if share.revoked {
            return match share.revoked_at {
                Some(revoked_at) => revoked_at + grace < at,
                None => true,
            };
        }
        false
    }

    /// Count purgeable shares for the owned target (no deletes).
    fn count_purgeable_shares(&self, retention_days: i32, at: DateTime<Utc>) -> u64 {
        self.shares
            .iter()
            .filter(|entry| Self::share_is_purgeable(entry.value(), retention_days, at))
            .count() as u64
    }

    /// Delete purgeable shares for the owned target. The ids are
    /// collected first, then removed: every delete was counted
    /// before it happened.
    fn delete_purgeable_shares(&self, retention_days: i32, at: DateTime<Utc>) -> u64 {
        let ids: Vec<String> = self
            .shares
            .iter()
            .filter(|entry| Self::share_is_purgeable(entry.value(), retention_days, at))
            .map(|entry| entry.key().clone())
            .collect();
        let removed = ids.len() as u64;
        for id in ids {
            self.shares.remove(&id);
        }
        removed
    }

    /// Ask the owning module for one hook target and aggregate its
    /// answers. A target with no answering owner reports zero with a
    /// note; a malformed answer is recorded as an error, never a
    /// panic. `dry_run` is the flag the owner must honor (count only,
    /// zero deletes).
    fn ask_owner(
        &self,
        target: &str,
        retention_days: i32,
        dry_run: bool,
        run_id: &str,
    ) -> TargetOutcome {
        let context = self.bus().emit(
            cleanup_event_for(target),
            json!({
                "target": target,
                "dry_run": dry_run,
                "retention_days": retention_days,
                "run_id": run_id,
            }),
        );
        if context.results.is_empty() {
            return TargetOutcome {
                target: target.to_string(),
                counted: 0,
                removed: 0,
                errors: Vec::new(),
                note: Some(
                    "No owner module answered the cleanup hook for this target.".to_string(),
                ),
            };
        }
        let mut counted = 0u64;
        let mut removed = 0u64;
        let mut errors: Vec<String> = Vec::new();
        for answer in &context.results {
            let Some(object) = answer.as_object() else {
                errors.push("Owner answered with an unusable payload.".to_string());
                continue;
            };
            counted += object
                .get("counted")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            removed += object
                .get("removed")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            if let Some(list) = object.get("errors").and_then(|value| value.as_array()) {
                for error in list {
                    if let Some(text) = error.as_str() {
                        errors.push(text.to_string());
                    }
                }
            }
            if let Some(error) = object.get("error").and_then(|value| value.as_str()) {
                errors.push(error.to_string());
            }
        }
        TargetOutcome {
            target: target.to_string(),
            counted,
            removed,
            errors,
            note: None,
        }
    }

    /// Resolve one target: count first, then delete when the run is
    /// real. Disabled rules are skipped (reported with a note).
    /// `cross_check` is report-only: it is always asked with
    /// `dry_run: true`, whatever the run asked for.
    fn resolve_target(
        &self,
        rule: &CleanupRule,
        dry_run: bool,
        run_id: &str,
        at: DateTime<Utc>,
    ) -> TargetOutcome {
        if !rule.enabled {
            return TargetOutcome {
                target: rule.target.clone(),
                counted: 0,
                removed: 0,
                errors: Vec::new(),
                note: Some("Rule is disabled.".to_string()),
            };
        }
        if rule.target == "document_shares" {
            let counted = self.count_purgeable_shares(rule.retention_days, at);
            let removed = if dry_run {
                0
            } else {
                self.delete_purgeable_shares(rule.retention_days, at)
            };
            return TargetOutcome {
                target: rule.target.clone(),
                counted,
                removed,
                errors: Vec::new(),
                note: None,
            };
        }
        let effective_dry_run = if rule.target == "cross_check" {
            true
        } else {
            dry_run
        };
        self.ask_owner(&rule.target, rule.retention_days, effective_dry_run, run_id)
    }

    /// Dry-run preview: counts per target, zero deletes, guaranteed
    /// (the scan path never calls a delete, for owned targets or
    /// through the hook — owners receive `dry_run: true`).
    pub fn scan(&self) -> ScanReport {
        let at = now();
        let run_id = format!("scan-{}", new_id());
        let targets = TARGETS
            .iter()
            .filter_map(|def| self.rule_for(def.target))
            .map(|rule| self.resolve_target(&rule, true, &run_id, at))
            .collect();
        let report = ScanReport {
            dry_run: true,
            scanned_at: at,
            targets,
        };
        self.emit(
            events::SCAN_COMPLETED,
            serde_json::to_value(&report).unwrap_or(Value::Null),
        );
        report
    }

    /// All runs, newest first.
    pub fn list_runs(&self) -> Vec<CleanupRun> {
        let mut runs: Vec<CleanupRun> = self
            .runs
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        runs
    }

    /// Run a cleanup. `dry_run` of `None` means each target uses its
    /// rule's `dry_run_default`; `Some(flag)` overrides every rule
    /// for this run (`cross_check` stays dry-run regardless). A
    /// second concurrent run is refused with 409. The run row is
    /// created first (status `running`), then finished with the
    /// per-target counts, and `data_tools.run.completed` is emitted
    /// so the run is audited.
    pub fn run_cleanup(
        &self,
        triggered_by: &str,
        dry_run: Option<bool>,
    ) -> UnifiedResult<CleanupRun> {
        // The guard lives to the end of this function, so the run
        // lock is held for the whole run (hook emission included).
        let _guard = match self.run_lock.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => {
                return Err(UnifiedError::conflict(
                    "A cleanup run is already in progress",
                ));
            }
            Err(TryLockError::Poisoned(_)) => {
                return Err(UnifiedError::internal("Cleanup run lock is poisoned"));
            }
        };
        let started_at = now();
        let run_id = new_id();
        let mut run = CleanupRun {
            id: run_id.clone(),
            started_at,
            finished_at: None,
            dry_run: dry_run.unwrap_or(false),
            status: "running".to_string(),
            counts: Vec::new(),
            errors: Vec::new(),
            triggered_by: triggered_by.to_string(),
        };
        self.runs.insert(run_id.clone(), run.clone());
        self.emit(
            events::RUN_STARTED,
            json!({
                "run_id": run_id,
                "triggered_by": triggered_by,
                "dry_run": run.dry_run,
                "started_at": started_at.to_rfc3339(),
            }),
        );
        let mut outcomes: Vec<TargetOutcome> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for def in TARGETS {
            let Some(rule) = self.rule_for(def.target) else {
                continue;
            };
            let effective_dry_run = match dry_run {
                Some(flag) => flag,
                None => rule.dry_run_default,
            };
            let outcome = self.resolve_target(&rule, effective_dry_run, &run_id, started_at);
            if !outcome.errors.is_empty() {
                errors.push(format!("{}: {}", outcome.target, outcome.errors.join("; ")));
            }
            outcomes.push(outcome);
        }
        run.counts = outcomes;
        run.errors = errors;
        run.finished_at = Some(now());
        run.status = if run.errors.is_empty() {
            "completed".to_string()
        } else {
            "failed".to_string()
        };
        // `run.dry_run` keeps the requested flag (None at request
        // time means "run for real per rule defaults", recorded as
        // false); the per-target outcomes carry the actual deletes.
        self.runs.insert(run_id.clone(), run.clone());
        self.emit(
            events::RUN_COMPLETED,
            serde_json::to_value(&run).unwrap_or(Value::Null),
        );
        Ok(run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use std::sync::Mutex as StdMutex;
    use unified_core::hooks::HookContext;

    fn test_share(
        id: &str,
        expires_at: Option<DateTime<Utc>>,
        revoked: bool,
        revoked_at: Option<DateTime<Utc>>,
    ) -> DocumentShareRecord {
        DocumentShareRecord {
            id: id.to_string(),
            document_id: format!("doc-{id}"),
            scope: "owner".to_string(),
            expires_at,
            revoked,
            revoked_at,
            created_by: "tester".to_string(),
            created_at: Utc::now(),
        }
    }

    /// Four shares: expired beyond the 30-day grace (purgeable),
    /// expired within grace (kept), revoked beyond grace (purgeable),
    /// and live (kept).
    fn seeded_state() -> Arc<DataToolsState> {
        let state = Arc::new(DataToolsState::new());
        let now = Utc::now();
        state.insert_share(test_share(
            "share-expired-beyond-grace",
            Some(now - ChronoDuration::days(45)),
            false,
            None,
        ));
        state.insert_share(test_share(
            "share-expired-within-grace",
            Some(now - ChronoDuration::days(10)),
            false,
            None,
        ));
        state.insert_share(test_share(
            "share-revoked-beyond-grace",
            Some(now + ChronoDuration::days(30)),
            true,
            Some(now - ChronoDuration::days(40)),
        ));
        state.insert_share(test_share(
            "share-live",
            Some(now + ChronoDuration::days(30)),
            false,
            None,
        ));
        state
    }

    #[test]
    fn new_state_seeds_one_rule_per_target() {
        let state = DataToolsState::new();
        let rules = state.list_rules();
        assert_eq!(rules.len(), TARGETS.len());
        for def in TARGETS {
            let rule = state.rule_for(def.target).expect("rule must exist");
            assert_eq!(rule.retention_days, def.retention_days);
            assert_eq!(rule.dry_run_default, def.dry_run_default);
            assert!(rule.enabled);
            assert_eq!(rule.scope, "platform");
        }
    }

    #[test]
    fn scan_dry_run_counts_without_deleting() {
        let state = seeded_state();
        let report = state.scan();
        assert!(report.dry_run);
        let shares = report
            .targets
            .iter()
            .find(|outcome| outcome.target == "document_shares")
            .expect("shares target must be reported");
        assert_eq!(shares.counted, 2);
        assert_eq!(shares.removed, 0);
        // Zero deletes: every share is still there.
        assert_eq!(state.shares.len(), 4);
    }

    #[test]
    fn scan_on_empty_state_reports_zeros_without_crashing() {
        let state = DataToolsState::new();
        let report = state.scan();
        assert_eq!(report.targets.len(), TARGETS.len());
        for outcome in &report.targets {
            assert_eq!(outcome.counted, 0);
            assert_eq!(outcome.removed, 0);
        }
        assert_eq!(state.shares.len(), 0);
    }

    #[test]
    fn run_deletes_purgeable_shares_and_counts_them() {
        let state = seeded_state();
        let run = state
            .run_cleanup("manual", Some(false))
            .expect("run must succeed");
        assert_eq!(run.status, "completed");
        assert!(run.finished_at.is_some());
        let shares = run
            .counts
            .iter()
            .find(|outcome| outcome.target == "document_shares")
            .expect("shares target must be reported");
        assert_eq!(shares.counted, 2);
        assert_eq!(shares.removed, 2);
        // Only the within-grace and live shares survive.
        assert_eq!(state.shares.len(), 2);
        assert!(state.shares.contains_key("share-expired-within-grace"));
        assert!(state.shares.contains_key("share-live"));
    }

    #[test]
    fn run_with_dry_run_true_deletes_nothing() {
        let state = seeded_state();
        let run = state
            .run_cleanup("manual", Some(true))
            .expect("run must succeed");
        assert_eq!(run.status, "completed");
        assert!(run.dry_run);
        for outcome in &run.counts {
            assert_eq!(outcome.removed, 0);
        }
        assert_eq!(state.shares.len(), 4);
    }

    #[test]
    fn run_without_dry_run_flag_uses_rule_defaults() {
        let state = seeded_state();
        let run = state.run_cleanup("manual", None).expect("run must succeed");
        let shares = run
            .counts
            .iter()
            .find(|outcome| outcome.target == "document_shares")
            .expect("shares target must be reported");
        // The shipped shares rule defaults to a real run.
        assert_eq!(shares.removed, 2);
    }

    #[test]
    fn run_is_recorded_and_audited() {
        let state = seeded_state();
        let seen: Arc<StdMutex<Vec<Value>>> = Arc::new(StdMutex::new(Vec::new()));
        let seen_clone = seen.clone();
        state.bus().register(
            events::RUN_COMPLETED,
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                seen_clone
                    .lock()
                    .expect("seen lock poisoned")
                    .push(ctx.data.clone());
            }),
        );
        let run = state
            .run_cleanup("manual", Some(false))
            .expect("run must succeed");
        // The run row is the audit record: counts + finish time.
        let recorded = state
            .runs
            .get(&run.id)
            .expect("run must be recorded")
            .value()
            .clone();
        assert_eq!(recorded.status, "completed");
        assert!(recorded.finished_at.is_some());
        assert_eq!(recorded.counts.len(), TARGETS.len());
        // The completion event was emitted for the Audit module.
        let events = seen.lock().expect("seen lock poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("id").and_then(|value| value.as_str()),
            Some(run.id.as_str())
        );
    }

    #[test]
    fn rules_crud_round_trip() {
        let state = DataToolsState::new();
        let rule = state
            .rule_for("document_downloads")
            .expect("rule must exist");
        let updated = state
            .update_rule(
                &rule.id,
                &RulePatch {
                    retention_days: Some(180),
                    enabled: Some(false),
                    dry_run_default: Some(true),
                },
            )
            .expect("patch must succeed");
        assert_eq!(updated.retention_days, 180);
        assert!(!updated.enabled);
        assert!(updated.dry_run_default);
        // A disabled rule is skipped by the resolver with a note.
        let report = state.scan();
        let downloads = report
            .targets
            .iter()
            .find(|outcome| outcome.target == "document_downloads")
            .expect("downloads target must be reported");
        assert_eq!(downloads.counted, 0);
        assert_eq!(downloads.note.as_deref(), Some("Rule is disabled."));
    }

    #[test]
    fn rule_patch_rejects_bad_retention_and_unknown_id() {
        let state = DataToolsState::new();
        let rule = state.rule_for("spool_temps").expect("rule must exist");
        let bad = state.update_rule(
            &rule.id,
            &RulePatch {
                retention_days: Some(4000),
                enabled: None,
                dry_run_default: None,
            },
        );
        assert!(bad.is_err());
        let missing = state.update_rule(
            "no-such-rule",
            &RulePatch {
                retention_days: Some(10),
                enabled: None,
                dry_run_default: None,
            },
        );
        assert!(missing.is_err());
    }

    #[test]
    fn hook_target_without_owner_reports_zero_with_note() {
        let state = DataToolsState::new();
        let report = state.scan();
        let sessions = report
            .targets
            .iter()
            .find(|outcome| outcome.target == "document_upload_sessions")
            .expect("sessions target must be reported");
        assert_eq!(sessions.counted, 0);
        assert_eq!(sessions.removed, 0);
        assert!(
            sessions
                .note
                .as_deref()
                .unwrap_or("")
                .contains("No owner module answered")
        );
    }

    #[test]
    fn hook_target_with_stub_owner_aggregates_counts() {
        let state = Arc::new(DataToolsState::new());
        let bus = state.bus();
        bus.register(
            "data_tools.cleanup.document_downloads",
            "stub-owner",
            Arc::new(|ctx: &mut HookContext| {
                let dry_run = ctx
                    .get("dry_run")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true);
                ctx.add_result(json!({
                    "target": "document_downloads",
                    "counted": 7,
                    "removed": if dry_run { 0 } else { 7 },
                }));
            }),
        );
        let run = state
            .run_cleanup("manual", Some(false))
            .expect("run must succeed");
        let downloads = run
            .counts
            .iter()
            .find(|outcome| outcome.target == "document_downloads")
            .expect("downloads target must be reported");
        assert_eq!(downloads.counted, 7);
        assert_eq!(downloads.removed, 7);
    }

    #[test]
    fn cross_check_is_always_dry_run_even_in_a_real_run() {
        let state = Arc::new(DataToolsState::new());
        let seen: Arc<StdMutex<Vec<Value>>> = Arc::new(StdMutex::new(Vec::new()));
        let seen_clone = seen.clone();
        state.bus().register(
            "data_tools.cleanup.cross_check",
            "stub-owner",
            Arc::new(move |ctx: &mut HookContext| {
                seen_clone
                    .lock()
                    .expect("seen lock poisoned")
                    .push(ctx.data.clone());
                ctx.add_result(json!({
                    "target": "cross_check",
                    "counted": 3,
                    "removed": 0,
                }));
            }),
        );
        let run = state
            .run_cleanup("manual", Some(false))
            .expect("run must succeed");
        let payloads = seen.lock().expect("seen lock poisoned");
        assert_eq!(payloads.len(), 1);
        assert_eq!(
            payloads[0].get("dry_run").and_then(|value| value.as_bool()),
            Some(true),
            "cross_check must always be asked with dry_run: true"
        );
        let cross = run
            .counts
            .iter()
            .find(|outcome| outcome.target == "cross_check")
            .expect("cross_check target must be reported");
        assert_eq!(cross.counted, 3);
        assert_eq!(cross.removed, 0);
    }

    #[test]
    fn revoked_share_without_timestamp_is_purgeable() {
        let state = Arc::new(DataToolsState::new());
        state.insert_share(test_share("share-revoked-no-stamp", None, true, None));
        let report = state.scan();
        let shares = report
            .targets
            .iter()
            .find(|outcome| outcome.target == "document_shares")
            .expect("shares target must be reported");
        assert_eq!(shares.counted, 1);
    }

    #[test]
    fn second_concurrent_run_is_refused() {
        let state = Arc::new(DataToolsState::new());
        let guard = state.run_lock.lock().expect("lock poisoned");
        let refused = state.run_cleanup("manual", Some(false));
        assert!(refused.is_err());
        drop(guard);
        assert!(state.run_cleanup("manual", Some(true)).is_ok());
    }
}
