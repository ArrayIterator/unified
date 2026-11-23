// data_tools — Hook events and the scheduler wiring.
//
// Ownership boundary (hard): Data Tools reaches other modules'
// tables ONLY through these events, never directly.
//
//   Scheduler owns WHEN: a `cleanup` scheduler job dispatches the
//   `cleanup` kind, the Scheduler emits `cleanup.requested`, and this
//   module answers it by running a real cleanup (per-rule
//   `dry_run_default` when the payload carries no `dry_run`).
//
//   Data Tools owns WHAT + the rules: for every hook target it emits
//   `data_tools.cleanup.<target>` with
//   `{"target", "dry_run", "retention_days", "run_id"}` and the
//   owning module answers by adding ONE result:
//   `{"target", "counted", "removed", "errors": [], "note"}`.
//   `dry_run: true` means count only — zero deletes, always.
//   `cross_check` is always asked with `dry_run: true` (report-only).
//   A target with no answering owner reports zero with a note; the
//   resolver never crashes on an unanswered target.
//
//   Audit: every run emits `data_tools.run.started` and
//   `data_tools.run.completed` (the run row in `cleanup_runs` is the
//   audit record; the Audit module subscribes to these events —
//   follow-up: add them to `AUDITED_EVENTS` in the Audit module).
//   Audit events are NEVER deleted by the cleaner — only the share
//   row is.

use super::*;
use serde_json::json;
use std::sync::Arc;
use unified_core::hooks::HookContext;

/// Emitted by the Scheduler when a `cleanup` job fires.
pub const CLEANUP_REQUESTED: &str = "cleanup.requested";

/// Emitted when a cleanup run starts.
pub const RUN_STARTED: &str = "data_tools.run.started";

/// Emitted when a cleanup run finishes (the audit event for a run).
pub const RUN_COMPLETED: &str = "data_tools.run.completed";

/// Emitted when a dry-run scan finishes.
pub const SCAN_COMPLETED: &str = "data_tools.scan.completed";

/// The per-target cleanup event for one target machine name.
pub fn cleanup_event_for(target: &str) -> String {
    format!("data_tools.cleanup.{target}")
}

/// Answer the scheduler's `cleanup.requested`: run a cleanup with
/// the run recorded as `triggered_by: scheduler`. A `dry_run` flag
/// in the payload is honored; without one, each rule's
/// `dry_run_default` applies. The outcome is added as a result so
/// the scheduler's dispatch summary can report it.
pub(crate) fn on_cleanup_requested(state: Arc<DataToolsState>, ctx: &mut HookContext) {
    let dry_run = ctx.get("dry_run").and_then(|value| value.as_bool());
    match state.run_cleanup("scheduler", dry_run) {
        Ok(run) => {
            ctx.add_result(json!({
                "cleaned": true,
                "run_id": run.id,
                "status": run.status,
                "dry_run": run.dry_run,
                "counts": run.counts,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "cleaned": false,
                "error": error.to_string(),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_event_names_are_stable() {
        assert_eq!(
            cleanup_event_for("document_downloads"),
            "data_tools.cleanup.document_downloads"
        );
        assert_eq!(
            cleanup_event_for("cross_check"),
            "data_tools.cleanup.cross_check"
        );
        assert_eq!(CLEANUP_REQUESTED, "cleanup.requested");
    }

    #[test]
    fn scheduler_hook_records_a_run() {
        let state = Arc::new(DataToolsState::new());
        let mut ctx = HookContext::new(CLEANUP_REQUESTED, json!({}));
        on_cleanup_requested(state.clone(), &mut ctx);
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(
            ctx.results[0]
                .get("cleaned")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        let runs = state.list_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].triggered_by, "scheduler");
        assert_eq!(runs[0].status, "completed");
    }

    #[test]
    fn scheduler_hook_honors_dry_run_payload() {
        let state = Arc::new(DataToolsState::new());
        let mut ctx = HookContext::new(CLEANUP_REQUESTED, json!({ "dry_run": true }));
        on_cleanup_requested(state.clone(), &mut ctx);
        let runs = state.list_runs();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].dry_run);
        for outcome in &runs[0].counts {
            assert_eq!(outcome.removed, 0);
        }
    }
}
