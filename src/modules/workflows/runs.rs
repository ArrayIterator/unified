// Workflows module - workflow run lifecycle.
//
// Split from `src/modules/workflows.rs`; the module root re-exports the public API of this module.

use unified_core::hooks::HookContext;
use unified_core::json;
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::constants::{EVENT_DEPTH_KEY, JOB_KIND_WORKFLOWS_RUN, MAX_RUNS_PER_MINUTE, MAX_WORKFLOW_CALL_DEPTH, RATE_WINDOW_SECS, SCHEDULER_ENQUEUE};
use super::engine::{StepOutcome, eval_group, execute_action, execute_action_sync, step_skipped, summarize_payload};
use super::state::{WorkflowsState};
use super::types::{LeafEvaluation, RunStatus, TriggerKind, Workflow, WorkflowRun, WorkflowRunStep};

/// Outcome of starting a run: either it started, or the rate
/// limit dropped it (the dropped run row is the log entry).
pub(crate) enum RunStart {
    Started(WorkflowRun),
    Dropped(WorkflowRun),
}

/// Record a run row that never started: insert it as `Dropped`,
/// bump the workflow's dropped counter, and emit
/// `workflows.run.failed` with the given reason. Shared by the
/// rate-limit drop in `begin_run` and the event-chain depth drop
/// in `on_bus_event`.
fn insert_dropped_run(
    state: &Arc<WorkflowsState>,
    workflow: &Workflow,
    trigger_kind: &str,
    trigger_name: Option<&str>,
    payload: &Value,
    depth: u8,
    reason: &str,
    error: String,
) -> WorkflowRun {
    let now = Utc::now();
    let run = WorkflowRun {
        id: Uuid::now_v7(),
        workflow_id: workflow.id,
        trigger_kind: trigger_kind.to_string(),
        trigger_name: trigger_name.map(str::to_string),
        status: RunStatus::Dropped,
        payload_summary: summarize_payload(payload),
        dry_run: false,
        depth,
        condition_matched: false,
        skipped_reason: None,
        retried_from_run_id: None,
        duration_ms: None,
        error: Some(error),
        started_at: now,
        finished_at: Some(now),
        created_at: now,
        updated_at: now,
    };
    state.runs.insert(run.id, run.clone());
    if let Some(mut stored) = state.workflows.get_mut(&workflow.id) {
        stored.dropped_count += 1;
        stored.last_run_at = Some(now);
        stored.last_run_status = Some(RunStatus::Dropped.as_str().to_string());
        stored.updated_at = now;
    }
    state.emit(
        "workflows.run.failed",
        json!({
            "workflow_id": workflow.id.to_string(),
            "run_id": run.id.to_string(),
            "status": "dropped",
            "reason": reason,
            "trigger_kind": trigger_kind,
        }),
    );
    run
}

/// Create a run row and enforce the guards that apply before any
/// work starts: the per-minute rate limit (max 60 runs per
/// workflow per minute; dry runs are exempt and never counted).
/// Emits `workflows.run.started` for real starts and
/// `workflows.run.failed` (with reason `rate_limited`) for drops.
pub(crate) fn begin_run(
    state: &Arc<WorkflowsState>,
    workflow: &Workflow,
    trigger_kind: &str,
    trigger_name: Option<&str>,
    payload: &Value,
    dry_run: bool,
    depth: u8,
) -> RunStart {
    let now = Utc::now();
    let mut dropped = false;
    if !dry_run {
        let mut starts = state
            .run_starts
            .entry(workflow.id)
            .or_insert_with(Vec::new);
        starts.retain(|started| (*started - now).num_seconds().abs() < RATE_WINDOW_SECS && *started <= now);
        // `retain` above keeps starts within the last 60 seconds;
        // entries from the future (clock skew) are dropped too.
        if starts.len() >= MAX_RUNS_PER_MINUTE {
            dropped = true;
        } else {
            starts.push(now);
        }
    }
    if dropped {
        return RunStart::Dropped(insert_dropped_run(
            state,
            workflow,
            trigger_kind,
            trigger_name,
            payload,
            depth,
            "rate_limited",
            format!(
                "Rate limit exceeded: at most {MAX_RUNS_PER_MINUTE} runs per workflow per minute; this run was dropped"
            ),
        ));
    }
    let run = WorkflowRun {
        id: Uuid::now_v7(),
        workflow_id: workflow.id,
        trigger_kind: trigger_kind.to_string(),
        trigger_name: trigger_name.map(str::to_string),
        status: RunStatus::Running,
        payload_summary: summarize_payload(payload),
        dry_run,
        depth,
        condition_matched: false,
        skipped_reason: None,
        retried_from_run_id: None,
        duration_ms: None,
        error: None,
        started_at: now,
        finished_at: None,
        created_at: now,
        updated_at: now,
    };
    state.runs.insert(run.id, run.clone());
    state.emit(
        "workflows.run.started",
        json!({
            "workflow_id": workflow.id.to_string(),
            "run_id": run.id.to_string(),
            "trigger_kind": trigger_kind,
            "trigger_name": trigger_name,
            "dry_run": dry_run,
            "depth": depth,
        }),
    );
    RunStart::Started(run)
}

/// Finish a run: update counters on the workflow, stamp the run,
/// and emit `workflows.run.finished` / `workflows.run.failed`.
/// Dry runs never touch counters and never emit lifecycle events.
pub(crate) fn finalize_run(
    state: &Arc<WorkflowsState>,
    run: &mut WorkflowRun,
    status: RunStatus,
    error: Option<String>,
) {
    let now = Utc::now();
    run.status = status;
    run.error = error.clone();
    run.finished_at = Some(now);
    run.updated_at = now;
    run.duration_ms = Some(now.signed_duration_since(run.started_at).num_milliseconds().max(0));
    state.runs.insert(run.id, run.clone());
    if run.dry_run {
        return;
    }
    if let Some(mut stored) = state.workflows.get_mut(&run.workflow_id) {
        stored.run_count += 1;
        match status {
            RunStatus::Finished => stored.success_count += 1,
            RunStatus::Failed => stored.failure_count += 1,
            _ => {}
        }
        stored.last_run_at = Some(now);
        stored.last_run_status = Some(status.as_str().to_string());
        stored.updated_at = now;
    }
    let event = if status == RunStatus::Finished {
        "workflows.run.finished"
    } else {
        "workflows.run.failed"
    };
    state.emit(
        event,
        json!({
            "workflow_id": run.workflow_id.to_string(),
            "run_id": run.id.to_string(),
            "status": status.as_str(),
            "trigger_kind": run.trigger_kind,
            "condition_matched": run.condition_matched,
            "duration_ms": run.duration_ms,
            "error": error,
        }),
    );
}

/// Record one step row for a run.
pub(crate) fn record_step(
    state: &Arc<WorkflowsState>,
    run: &WorkflowRun,
    step_index: usize,
    action_kind: &str,
    outcome: &StepOutcome,
    duration_ms: i64,
) -> WorkflowRunStep {
    let now = Utc::now();
    let step = WorkflowRunStep {
        id: Uuid::now_v7(),
        run_id: run.id,
        workflow_id: run.workflow_id,
        step_index,
        action_kind: action_kind.to_string(),
        status: outcome.status.to_string(),
        attempts: outcome.attempts,
        error: outcome.error.clone(),
        detail: outcome.detail.clone(),
        duration_ms: Some(duration_ms),
        created_at: now,
        updated_at: now,
    };
    state.steps.insert(step.id, step.clone());
    step
}

/// Shared condition evaluation for both finish paths. Returns
/// `(matched, leaf evaluations)`. No conditions means "always
/// run".
pub(crate) fn evaluate_conditions(workflow: &Workflow, payload: &Value) -> (bool, Vec<LeafEvaluation>) {
    match &workflow.conditions {
        None => (true, Vec::new()),
        Some(group) => eval_group(group, payload),
    }
}

/// Decide the terminal status of a run from its step outcomes. A
/// run must never report success when an action never happened or
/// never finished: any undelivered (`skipped`) or `deferred` step
/// fails the run, so it is counted as a failure (not a success)
/// and stays retryable through `POST /runs/{run_id}/retry`. A real
/// failure wins over deferred / skipped in the error message.
fn terminal_outcome(
    failed: Option<String>,
    deferred: usize,
    skipped: usize,
    workflow_id: Uuid,
) -> (RunStatus, Option<String>) {
    if let Some(error) = failed {
        (RunStatus::Failed, Some(error))
    } else if deferred > 0 {
        (
            RunStatus::Failed,
            Some(format!(
                "{deferred} webhook step(s) were deferred; retry the run from POST /api/v1/workflows/{workflow_id}/run"
            )),
        )
    } else if skipped > 0 {
        (
            RunStatus::Failed,
            Some(format!(
                "{skipped} action step(s) were not delivered (no handler is registered for the action's event); the action never happened"
            )),
        )
    } else {
        (RunStatus::Finished, None)
    }
}

/// Async finish path: evaluates conditions, executes actions in
/// order (stopping at the first failed step; later steps are
/// recorded as skipped), and finalizes the run. A dry run records
/// every action as a planned step and executes nothing.
pub(crate) async fn finish_run_async(
    state: &Arc<WorkflowsState>,
    mut run: WorkflowRun,
    workflow: Workflow,
    payload: Value,
    depth: u8,
) -> WorkflowRun {
    let (matched, _leaves) = evaluate_conditions(&workflow, &payload);
    run.condition_matched = matched;
    state.runs.insert(run.id, run.clone());
    if !matched {
        run.skipped_reason = Some("Conditions did not match; no actions were executed.".to_string());
        let mut run = run;
        finalize_run(state, &mut run, RunStatus::Finished, None);
        return run;
    }
    if run.dry_run {
        for (index, action) in workflow.actions.iter().enumerate() {
            record_step(
                state,
                &run,
                index,
                &action.kind,
                &StepOutcome {
                    status: "skipped",
                    attempts: 0,
                    error: Some("dry run: planned, not executed".to_string()),
                    detail: json!({ "planned": true, "dry_run": true }),
                },
                0,
            );
        }
        let mut run = run;
        finalize_run(state, &mut run, RunStatus::Finished, None);
        return run;
    }
    let mut failed: Option<String> = None;
    let mut deferred = 0usize;
    let mut skipped = 0usize;
    for (index, action) in workflow.actions.iter().enumerate() {
        if failed.is_some() {
            record_step(
                state,
                &run,
                index,
                &action.kind,
                &step_skipped(
                    "a previous step failed; remaining steps were not executed",
                    json!({ "planned": true }),
                ),
                0,
            );
            skipped += 1;
            continue;
        }
        let started = Utc::now();
        let outcome = execute_action(state, &workflow, action, &payload, depth).await;
        let duration_ms = Utc::now()
            .signed_duration_since(started)
            .num_milliseconds()
            .max(0);
        if outcome.status == "deferred" {
            deferred += 1;
        }
        if outcome.status == "skipped" {
            skipped += 1;
        }
        if outcome.status == "failed" {
            failed = outcome.error.clone();
        }
        record_step(state, &run, index, &action.kind, &outcome, duration_ms);
    }
    let mut run = run;
    let (status, error) = terminal_outcome(failed, deferred, skipped, workflow.id);
    finalize_run(state, &mut run, status, error);
    run
}

/// Sync finish path (bus handlers without an async runtime).
/// Identical to the async path except webhook steps are deferred
/// instead of executed.
pub(crate) fn finish_run_sync(
    state: &Arc<WorkflowsState>,
    mut run: WorkflowRun,
    workflow: Workflow,
    payload: Value,
    depth: u8,
) -> WorkflowRun {
    let (matched, _leaves) = evaluate_conditions(&workflow, &payload);
    run.condition_matched = matched;
    state.runs.insert(run.id, run.clone());
    if !matched {
        run.skipped_reason = Some("Conditions did not match; no actions were executed.".to_string());
        let mut run = run;
        finalize_run(state, &mut run, RunStatus::Finished, None);
        return run;
    }
    if run.dry_run {
        for (index, action) in workflow.actions.iter().enumerate() {
            record_step(
                state,
                &run,
                index,
                &action.kind,
                &StepOutcome {
                    status: "skipped",
                    attempts: 0,
                    error: Some("dry run: planned, not executed".to_string()),
                    detail: json!({ "planned": true, "dry_run": true }),
                },
                0,
            );
        }
        let mut run = run;
        finalize_run(state, &mut run, RunStatus::Finished, None);
        return run;
    }
    let mut failed: Option<String> = None;
    let mut deferred = 0usize;
    let mut skipped = 0usize;
    for (index, action) in workflow.actions.iter().enumerate() {
        if failed.is_some() {
            record_step(
                state,
                &run,
                index,
                &action.kind,
                &step_skipped(
                    "a previous step failed; remaining steps were not executed",
                    json!({ "planned": true }),
                ),
                0,
            );
            skipped += 1;
            continue;
        }
        let started = Utc::now();
        let outcome = execute_action_sync(state, &workflow, action, &payload, depth);
        let duration_ms = Utc::now()
            .signed_duration_since(started)
            .num_milliseconds()
            .max(0);
        if outcome.status == "deferred" {
            deferred += 1;
        }
        if outcome.status == "skipped" {
            skipped += 1;
        }
        if outcome.status == "failed" {
            failed = outcome.error.clone();
        }
        record_step(state, &run, index, &action.kind, &outcome, duration_ms);
    }
    let mut run = run;
    let (status, error) = terminal_outcome(failed, deferred, skipped, workflow.id);
    finalize_run(state, &mut run, status, error);
    run
}

// ---------------------------------------------------------------------------
// Trigger subscriptions + scheduler
// ---------------------------------------------------------------------------

/// Subscribe one workflow's event trigger: register a bus handler
/// for the workflow's event name. The handler is registered per
/// workflow, so several workflows can listen to the same event.
/// Called on create / enable / trigger change.
pub(crate) fn subscribe_workflow(state: &Arc<WorkflowsState>, workflow: &Workflow) {
    if workflow.trigger.kind != TriggerKind::Event || !workflow.is_active {
        return;
    }
    let Some(bus) = state.bus() else {
        return;
    };
    let Some(event_name) = workflow.trigger.event.clone() else {
        return;
    };
    let workflow_id = workflow.id;
    let handler_state = Arc::clone(state);
    let handler_event = event_name.clone();
    let handler_id = bus.register(
        &event_name,
        "workflows",
        Arc::new(move |ctx: &mut HookContext| {
            on_bus_event(
                Arc::clone(&handler_state),
                workflow_id,
                &handler_event,
                ctx.data.clone(),
            );
        }),
    );
    state
        .subscriptions
        .entry(workflow_id)
        .or_insert_with(Vec::new)
        .push((event_name, handler_id));
}

/// Remove every bus subscription of one workflow. Called on
/// disable / delete / trigger change.
pub(crate) fn unsubscribe_workflow(state: &Arc<WorkflowsState>, workflow_id: Uuid) {
    let Some(bus) = state.bus() else {
        state.subscriptions.remove(&workflow_id);
        return;
    };
    if let Some((_, entries)) = state.subscriptions.remove(&workflow_id) {
        for (event_name, handler_id) in entries {
            bus.unregister_by_id(&event_name, handler_id);
        }
    }
}

/// Queue one scheduled workflow through `scheduler.enqueue` with
/// kind `workflows.run`. The scheduler knows the kind (see
/// `KNOWN_KINDS` in the scheduler module) and its dispatch emits
/// `workflows.run_requested`, which runs the named workflow.
/// `scheduler_status` records the outcome honestly (`queued` /
/// `declined`).
pub(crate) fn schedule_workflow(state: &Arc<WorkflowsState>, workflow: &mut Workflow) {
    if workflow.trigger.kind != TriggerKind::Schedule {
        workflow.scheduler_status = "not_scheduled".to_string();
        workflow.scheduler_job_id = None;
        return;
    }
    let Some(bus) = state.bus() else {
        workflow.scheduler_status = "declined".to_string();
        workflow.scheduler_job_id = None;
        return;
    };
    let answer = bus.emit(
        SCHEDULER_ENQUEUE,
        json!({
            "kind": JOB_KIND_WORKFLOWS_RUN,
            "payload": {
                "workflow_id": workflow.id.to_string(),
                "trigger": "schedule",
                "cron": workflow.trigger.cron,
                "timezone": workflow.trigger.timezone,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            workflow.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            workflow.scheduler_status = "queued".to_string();
        }
        _ => {
            workflow.scheduler_job_id = None;
            workflow.scheduler_status = "declined".to_string();
        }
    }
}

/// Handle a bus event for one workflow: the runtime trigger path.
/// Guards: inactive or deleted workflows ignore the event; a run
/// that would trigger itself is rejected (a `workflows.*` event
/// whose payload names this workflow); and an event chain deeper
/// than `MAX_WORKFLOW_CALL_DEPTH` is dropped (see the depth note
/// below). The run starts on the caller's thread and finishes on
/// a spawned task when a Tokio runtime is available; without one
/// it finishes inline on the sync path (webhook steps are
/// deferred, never silently lost).
pub(crate) fn on_bus_event(
    state: Arc<WorkflowsState>,
    workflow_id: Uuid,
    event_name: &str,
    payload: Value,
) {
    let Some(workflow) = state
        .workflows
        .get(&workflow_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    if !workflow.is_active {
        return;
    }
    if event_name.starts_with("workflows.")
        && payload
            .get("workflow_id")
            .and_then(Value::as_str)
            .is_some_and(|value| value == workflow_id.to_string())
    {
        // A run that triggers itself is rejected.
        return;
    }
    let trigger_name = if workflow.trigger.kind == TriggerKind::Event {
        workflow.trigger.event.clone()
    } else {
        Some(event_name.to_string())
    };
    // Event-chain depth: a run stamps every event it emits with
    // the depth its downstream run should start at
    // (`workflow_event_depth`, set by `emit_action`). A chain
    // deeper than MAX_WORKFLOW_CALL_DEPTH is dropped with a log
    // row instead of starting another run: the per-minute rate
    // limit alone only slows a self-triggering loop, it never
    // stops it. Events from other modules carry no depth key and
    // start at depth 0.
    let chain_depth = payload
        .get(EVENT_DEPTH_KEY)
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if chain_depth > MAX_WORKFLOW_CALL_DEPTH as u64 {
        insert_dropped_run(
            &state,
            &workflow,
            "event",
            trigger_name.as_deref(),
            &payload,
            MAX_WORKFLOW_CALL_DEPTH,
            "event_chain_depth_exceeded",
            format!(
                "Event chain depth limit ({MAX_WORKFLOW_CALL_DEPTH}) exceeded; refusing to start another run for event \"{event_name}\""
            ),
        );
        return;
    }
    let depth = chain_depth as u8;
    match begin_run(
        &state,
        &workflow,
        "event",
        trigger_name.as_deref(),
        &payload,
        false,
        depth,
    ) {
        RunStart::Dropped(_) => {}
        RunStart::Started(run) => {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let task_state = Arc::clone(&state);
                let task_workflow = workflow.clone();
                handle.spawn(async move {
                    finish_run_async(&task_state, run, task_workflow, payload, depth).await;
                });
            } else {
                finish_run_sync(&state, run, workflow, payload, depth);
            }
        }
    }
}

/// Answer `workflows.run_requested`: the event the scheduler emits
/// when a `workflows.run` job fires. Runs the named scheduled
/// workflow; an unknown or inactive workflow id is ignored so a
/// stale job can never run a deleted workflow.
pub(crate) fn on_workflows_run_requested(state: Arc<WorkflowsState>, ctx: &mut HookContext) {
    let workflow_id = ctx
        .data
        .get("workflow_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(workflow_id) = workflow_id else {
        ctx.add_result(json!({ "started": false, "error": "workflows.run_requested requires 'workflow_id'" }));
        return;
    };
    let Some(workflow) = state
        .workflows
        .get(&workflow_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "started": false, "workflow_id": workflow_id.to_string(), "error": "Workflow not found" }));
        return;
    };
    if !workflow.is_active {
        ctx.add_result(json!({ "started": false, "workflow_id": workflow_id.to_string(), "reason": "Workflow is disabled" }));
        return;
    }
    if workflow.trigger.kind != TriggerKind::Schedule {
        ctx.add_result(json!({ "started": false, "workflow_id": workflow_id.to_string(), "reason": "Workflow has no schedule trigger" }));
        return;
    }
    let payload = ctx.data.get("payload").cloned().unwrap_or(Value::Null);
    match begin_run(
        &state,
        &workflow,
        "schedule",
        workflow.trigger.cron.as_deref(),
        &payload,
        false,
        0,
    ) {
        RunStart::Dropped(dropped) => {
            ctx.add_result(json!({ "started": false, "run_id": dropped.id.to_string(), "status": "dropped", "reason": "rate_limited" }));
        }
        RunStart::Started(run) => {
            let run_id = run.id;
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let task_state = Arc::clone(&state);
                handle.spawn(async move {
                    finish_run_async(&task_state, run, workflow, payload, 0).await;
                });
            } else {
                finish_run_sync(&state, run, workflow, payload, 0);
            }
            ctx.add_result(json!({ "started": true, "run_id": run_id.to_string() }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::engine::execute_action_sync;
    use super::types::{WorkflowAction, WorkflowTrigger};
    use unified_core::hooks::HookBus;

    fn test_workflow(trigger: WorkflowTrigger, actions: Vec<WorkflowAction>) -> Workflow {
        let now = Utc::now();
        Workflow {
            id: Uuid::now_v7(),
            name: "test workflow".to_string(),
            description: None,
            trigger,
            conditions: None,
            actions,
            is_active: true,
            owner_id: "tester".to_string(),
            company_id: None,
            webhook_token_hash: None,
            webhook_allowed_hosts: vec![],
            run_count: 0,
            success_count: 0,
            failure_count: 0,
            dropped_count: 0,
            last_run_at: None,
            last_run_status: None,
            scheduler_status: "not_scheduled".to_string(),
            scheduler_job_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn event_trigger(event: &str) -> WorkflowTrigger {
        WorkflowTrigger {
            kind: TriggerKind::Event,
            event: Some(event.to_string()),
            cron: None,
            timezone: None,
        }
    }

    fn notify_action() -> WorkflowAction {
        WorkflowAction {
            kind: "send_notification".to_string(),
            config: json!({"title": "hi", "body": "there", "user_id": "u1"}),
        }
    }

    fn webhook_action() -> WorkflowAction {
        WorkflowAction {
            kind: "call_webhook".to_string(),
            config: json!({"url": "https://hooks.example.com/hook"}),
        }
    }

    fn update_tasks_action() -> WorkflowAction {
        WorkflowAction {
            kind: "update_record".to_string(),
            config: json!({
                "target_module": "tasks",
                "target_id": Uuid::now_v7().to_string(),
                "field": "status",
                "value": "done",
            }),
        }
    }

    /// Start a run through `begin_run` and finish it on the sync
    /// path, as a bus handler without an async runtime would.
    fn run_sync(state: &Arc<WorkflowsState>, workflow: Workflow, dry_run: bool) -> WorkflowRun {
        match begin_run(
            state,
            &workflow,
            "manual",
            Some("manual"),
            &json!({}),
            dry_run,
            0,
        ) {
            RunStart::Started(run) => finish_run_sync(state, run, workflow, json!({}), 0),
            RunStart::Dropped(run) => panic!("run unexpectedly dropped: {:?}", run.error),
        }
    }

    fn counters(state: &WorkflowsState, workflow_id: Uuid) -> (i64, i64, i64) {
        let stored = state.workflows.get(&workflow_id).expect("workflow stored");
        (stored.success_count, stored.failure_count, stored.dropped_count)
    }

    #[test]
    fn terminal_outcome_needs_a_clean_run_to_finish() {
        let id = Uuid::now_v7();
        assert_eq!(terminal_outcome(None, 0, 0, id).0, RunStatus::Finished);
        assert_eq!(terminal_outcome(None, 0, 0, id).1, None);
        let (status, error) = terminal_outcome(None, 0, 1, id);
        assert_eq!(status, RunStatus::Failed);
        assert!(error.unwrap().contains("not delivered"));
        let (status, error) = terminal_outcome(None, 2, 0, id);
        assert_eq!(status, RunStatus::Failed);
        assert!(error.unwrap().contains("deferred"));
        let (status, error) = terminal_outcome(Some("boom".to_string()), 1, 1, id);
        assert_eq!(status, RunStatus::Failed);
        assert_eq!(error.unwrap(), "boom");
    }

    #[test]
    fn skipped_action_fails_the_run_and_counts_as_failure() {
        // No bus is attached, so the notification is never
        // delivered: the step is skipped and the run must fail,
        // not finish as a success.
        let state = Arc::new(WorkflowsState::default());
        let workflow = test_workflow(event_trigger("ticket.created"), vec![notify_action()]);
        state.workflows.insert(workflow.id, workflow.clone());
        let finished = run_sync(&state, workflow.clone(), false);
        assert_eq!(finished.status, RunStatus::Failed);
        assert!(finished.error.unwrap().contains("not delivered"));
        assert_eq!(counters(&state, workflow.id), (0, 1, 0));
    }

    #[test]
    fn deferred_webhook_fails_the_run_and_counts_as_failure() {
        // On the sync path a webhook step is deferred; the run
        // must fail (and stay retryable), not finish with an
        // error attached to a success.
        let state = Arc::new(WorkflowsState::default());
        let workflow = test_workflow(event_trigger("ticket.created"), vec![webhook_action()]);
        state.workflows.insert(workflow.id, workflow.clone());
        let finished = run_sync(&state, workflow.clone(), false);
        assert_eq!(finished.status, RunStatus::Failed);
        assert!(finished.error.unwrap().contains("deferred"));
        assert_eq!(counters(&state, workflow.id), (0, 1, 0));
    }

    #[test]
    fn dry_run_still_finishes_with_planned_steps() {
        let state = Arc::new(WorkflowsState::default());
        let workflow = test_workflow(event_trigger("ticket.created"), vec![notify_action()]);
        state.workflows.insert(workflow.id, workflow.clone());
        let finished = run_sync(&state, workflow.clone(), true);
        assert_eq!(finished.status, RunStatus::Finished);
        assert_eq!(counters(&state, workflow.id), (0, 0, 0));
    }

    #[test]
    fn run_workflow_parent_fails_when_child_is_undelivered() {
        // The child run fails (its notification is never
        // delivered), so the parent's run_workflow step must fail
        // too instead of treating a Finished-with-error child as
        // success.
        let state = Arc::new(WorkflowsState::default());
        let child = test_workflow(event_trigger("ticket.created"), vec![notify_action()]);
        state.workflows.insert(child.id, child.clone());
        let parent = test_workflow(
            event_trigger("ticket.created"),
            vec![WorkflowAction {
                kind: "run_workflow".to_string(),
                config: json!({"workflow_id": child.id.to_string()}),
            }],
        );
        state.workflows.insert(parent.id, parent.clone());
        let action = parent.actions[0].clone();
        let outcome = execute_action_sync(&state, &parent, &action, &json!({}), 0);
        assert_eq!(outcome.status, "failed");
        let error = outcome.error.unwrap();
        assert!(error.contains("ended as failed"), "{error}");
        assert!(error.contains("not delivered"), "{error}");
        let child_run = state
            .runs
            .iter()
            .find(|entry| entry.value().workflow_id == child.id)
            .expect("child run recorded");
        assert_eq!(child_run.value().status, RunStatus::Failed);
    }

    #[test]
    fn over_depth_event_is_dropped_not_run() {
        let state = Arc::new(WorkflowsState::default());
        let workflow = test_workflow(
            event_trigger("tasks.update_requested"),
            vec![update_tasks_action()],
        );
        state.workflows.insert(workflow.id, workflow.clone());
        on_bus_event(
            Arc::clone(&state),
            workflow.id,
            "tasks.update_requested",
            json!({ EVENT_DEPTH_KEY: 4 }),
        );
        assert_eq!(state.runs.len(), 1);
        let run = state.runs.iter().next().expect("one run row");
        assert_eq!(run.value().status, RunStatus::Dropped);
        assert!(run.value().error.as_deref().unwrap().contains("depth limit"));
        assert_eq!(counters(&state, workflow.id), (0, 0, 1));
    }

    #[test]
    fn event_self_trigger_chain_terminates_at_max_depth() {
        // The workflow listens to the exact event its own action
        // emits: without a depth limit this loops forever. The
        // stamped depth must stop the chain after the run at depth
        // MAX_WORKFLOW_CALL_DEPTH, with one dropped run.
        let state = Arc::new(WorkflowsState::default());
        let bus = Arc::new(HookBus::new());
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&bus));
        let workflow = test_workflow(
            event_trigger("tasks.update_requested"),
            vec![update_tasks_action()],
        );
        state.workflows.insert(workflow.id, workflow.clone());
        subscribe_workflow(&state, &workflow);
        on_bus_event(
            Arc::clone(&state),
            workflow.id,
            "tasks.update_requested",
            json!({}),
        );
        let mut depths: Vec<u8> = state
            .runs
            .iter()
            .map(|entry| entry.value().depth)
            .collect();
        depths.sort_unstable();
        assert_eq!(
            depths,
            vec![0, 1, 2, 3, MAX_WORKFLOW_CALL_DEPTH],
            "four runs plus one depth-limit drop, no run deeper than the cap"
        );
        let finished = state
            .runs
            .iter()
            .filter(|entry| entry.value().status == RunStatus::Finished)
            .count();
        assert_eq!(finished, 4);
        let dropped = state
            .runs
            .iter()
            .filter(|entry| entry.value().status == RunStatus::Dropped)
            .count();
        assert_eq!(dropped, 1);
        assert_eq!(counters(&state, workflow.id), (4, 0, 1));
    }
}
