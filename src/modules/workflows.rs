// Workflow Automation / Autopilot module — event-driven workflows
// ("if this happens, do that"), scheduled workflows, webhook
// triggers, and manual runs, with condition groups, ordered
// actions, per-run logs, dry-run testing, retries, and loop /
// recursion guards.
//
// Routes live under `/api/v1/workflows`:
//
//   Workflows:
//     GET/POST / — list workflows / create a workflow.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/enable, /{id}/disable — activate / deactivate.
//     POST /{id}/duplicate — copy (the copy starts disabled).
//     POST /{id}/run — manual run ({payload, dry_run}).
//     POST /{id}/test — dry-run evaluation: conditions + planned
//       actions, no side effects, no run row.
//   Runs:
//     GET  /{id}/runs — paginated, filter by status, trigger_kind.
//     GET  /runs/{run_id} — one run with its steps.
//     POST /runs/{run_id}/retry — re-run a failed run with a
//       caller-supplied payload (see the honesty note: the original
//       payload is not retained, only a redacted summary).
//   Triggers:
//     GET  /triggers — active bus-event subscriptions
//       (event name -> workflow ids).
//     POST /hooks/{token} — webhook trigger. The token is the
//       secret (see the public-access note below); the body is the
//       trigger payload.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory workflow, run, and run-step management; trigger
// validation (bus-event names, 5-field cron + timezone, webhook
// tokens); condition groups nested up to 3 levels with field
// validation at save time; ordered action execution with template
// substitution from the trigger payload; per-run logs with
// per-step success / failure / skipped / deferred status,
// attempts, and duration; dry-run / test mode that evaluates
// conditions and lists planned actions without side effects;
// retries (max 3 attempts, exponential backoff) for outbound
// webhook steps only — emit-based actions are attempted once,
// because re-emitting them could duplicate a notification, email,
// task, or ticket; loop / recursion guards (max workflow-call
// depth 3, max 60 runs per workflow per minute with excess runs
// dropped and logged, self-trigger rejection at save time and at
// runtime); SSRF-safe outbound webhooks (per-workflow host
// allowlist, DNS resolution checked against private / reserved
// ranges, 10s timeout, redirects disabled).
//
// What is NOT built here:
// - No real delivery of anything: notification / email / task /
//   ticket actions only emit hook-bus events and the owning module
//   does the work. `notification.requested` is consumed by the
//   notifications module; `mail.send_requested` by the mail
//   module; `ticket.create_requested` by the tickets module
//   (which really creates the ticket and answers with the ticket
//   or `{"error": ...}`). `task.create_requested`,
//   `{tasks|tickets}.update_requested`,
//   `{tasks|tickets}.assign_requested`, and
//   `{tasks|tickets}.tag_requested` have NO consumer yet: the
//   tasks module does not listen to the bus at all, and the
//   tickets module only listens to `ticket.create_requested`.
//   When no handler is registered for an action's event the step
//   is recorded as `skipped` (the step stays skipped, but the run
//   now finalizes `Failed`) with the reason in the step detail,
//   so a workflow never pretends an undelivered action happened. The follow-up is for the tasks / tickets
//   modules to add `*_requested` listeners mirroring
//   `tickets.rs` `on_ticket_create_requested`.
// - The `update_record` action never writes to a table: it only
//   emits `{module}.update_requested` for an allowlisted
//   (module, field) pair. Arbitrary table writes are refused.
// - Scheduled triggers queue through `scheduler.enqueue` with
//   kind `workflows.run`, exactly like the events module queues
//   reminders. `workflows.run` is in the scheduler's `KNOWN_KINDS`
//   list (`scheduler/types.rs`) with a dispatch arm that emits
//   `workflows.run_requested` with `{"workflow_id", "trigger":
//   "schedule"}`, so scheduled workflows run on schedule instead
//   of recording `scheduler_status: "declined"`.
// - The trigger payload is never stored in full: only a redacted
//   summary (top-level keys with value types, strings truncated
//   to 128 characters, secret-named keys dropped). That is why a
//   retry needs a caller-supplied payload.
// - DNS-based SSRF checks resolve the host once before the call,
//   but the HTTP client resolves it again when connecting, so a
//   DNS-rebinding (TOCTOU) attack between the check and the
//   connect is not fully closed; the allowlist is the primary
//   defence and the check is the second layer.
//
// Public-access gate: the webhook trigger endpoint
// `POST /api/v1/workflows/hooks/{token}` carries no `X-User-Id` /
// `X-Role` headers, so it has no capability check — the token in
// the URL path IS the credential (64 hex characters, stored only
// as a SHA-256 hash, compared in constant time). Like the events
// module's public routes it relies on the API Manager's
// `ApiAccessRule` gate: an administrator must create the matching
// rule for `POST /api/v1/workflows/hooks/*` or the API Manager
// denies the call (default deny). Rotate a leaked token with
// `{"rotate_webhook_token": true}` on PATCH /{id}; the old token
// stops working immediately. Every other route requires
// `workflows.view` (reads) or `workflows.manage` (writes).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).
//
// Events emitted on the hook bus:
//   - `workflows.created`, `workflows.updated`, `workflows.deleted`
//   - `workflows.run.started`, `workflows.run.finished`,
//     `workflows.run.failed`
//   - `notification.requested` (send_notification action) with the
//     notifications module's payload shape.
//   - `mail.send_requested` (send_email action) with the mail
//     module's payload shape (`to`, `subject`, `body_text` /
//     `body_html` / `template_id`, `variables`).
//   - `task.create_requested` (create_task action; no consumer
//     yet — see the honesty note).
//   - `ticket.create_requested` (create_ticket action; consumed
//     by the tickets module).
//   - `{tasks|tickets}.update_requested`,
//     `{tasks|tickets}.assign_requested`,
//     `{tasks|tickets}.tag_requested` (update_record / assign_user
//     / add_tag actions; no consumer yet).
//   - `scheduler.enqueue` with `{"kind": "workflows.run",
//     "payload": {"workflow_id", "trigger": "schedule"}}` when a
//     workflow with a schedule trigger is created or enabled.
// Events this module listens to:
//   - Nothing by default. Each workflow with an `event` trigger
//     subscribes its own handler for that bus event name at
//     runtime (on create / enable / trigger change) and
//     unsubscribes on disable / delete / trigger change. A
//     handler is registered per workflow, so several workflows
//     can listen to the same event independently. Subscribing is
//     how a trigger "listens": there is no static list of event
//     names in this file — any hook-bus event name is a valid
//     trigger, for example `invoice.overdue`, `ticket.created`,
//     or `order.paid`.
//   - `workflows.run_requested` — the event the scheduler emits
//     when a `workflows.run` job fires, following its naming rule
//     (`backup.run` -> `backup.run_requested`); runs the named
//     scheduled workflow.

pub mod ai_capabilities;
mod constants;
pub mod types;
pub mod state;
mod validation;
mod engine;
mod runs;
mod handlers;
pub mod routes;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::TriggerKind;
pub use self::types::WorkflowTrigger;
pub use self::types::ConditionLeaf;
pub use self::types::ConditionGroup;
pub use self::types::ConditionNode;
pub use self::types::WorkflowAction;
pub use self::types::Workflow;
pub use self::types::RunStatus;
pub use self::types::WorkflowRun;
pub use self::types::WorkflowRunStep;
pub use self::types::LeafEvaluation;
pub use self::types::CreateWorkflowRequest;
pub use self::types::UpdateWorkflowRequest;
pub use self::types::RunWorkflowRequest;
pub use self::types::TestWorkflowRequest;
pub use self::types::RetryRunRequest;
pub use self::state::WorkflowsState;
pub use self::routes::WORKFLOWS_RUN_JOB_KIND;
pub use self::routes::WORKFLOWS_RUN_REQUESTED_EVENT;
pub use self::routes::WorkflowsModule;
pub use self::routes::register;
