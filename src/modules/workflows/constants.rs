// Workflows module - event names, permissions, and limits.
//
// Split from `src/modules/workflows.rs`; the module root re-exports the public API of this module.

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;
pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;
pub(crate) const MAX_EVENT_NAME_LEN: usize = 128;
pub(crate) const MAX_TIMEZONE_LEN: usize = 64;
pub(crate) const MAX_CRON_LEN: usize = 64;
pub(crate) const MAX_HOST_LEN: usize = 253;
pub(crate) const MAX_URL_LEN: usize = 2048;
pub(crate) const MAX_FIELD_PATH_LEN: usize = 256;
pub(crate) const MAX_TAG_LEN: usize = 64;
pub(crate) const MAX_USER_ID_LEN: usize = 128;
pub(crate) const MAX_TITLE_LEN: usize = 500;
pub(crate) const MAX_BODY_LEN: usize = 20_000;
pub(crate) const MAX_EMAIL_LEN: usize = 254;
/// Maximum number of actions on one workflow.
pub(crate) const MAX_ACTIONS: usize = 50;
/// Maximum nesting depth of condition groups (a group inside a
/// group inside a group is the deepest allowed shape).
pub(crate) const MAX_CONDITION_DEPTH: usize = 3;
/// Maximum number of leaf conditions inside one group.
pub(crate) const MAX_CONDITIONS_PER_GROUP: usize = 50;
/// Maximum field-path segments (`payload.a.b.c`).
pub(crate) const MAX_FIELD_SEGMENTS: usize = 8;
/// Maximum values in an `in` operator list.
pub(crate) const MAX_IN_VALUES: usize = 100;
/// Maximum webhook hosts on one workflow's allowlist.
pub(crate) const MAX_ALLOWED_HOSTS: usize = 20;
/// Maximum custom headers on one webhook action.
pub(crate) const MAX_WEBHOOK_HEADERS: usize = 20;
/// Maximum serialized bytes of a webhook action body.
pub(crate) const MAX_WEBHOOK_BODY_BYTES: usize = 65_536;
/// Maximum serialized bytes of one condition value.
pub(crate) const MAX_CONDITION_VALUE_BYTES: usize = 4096;
/// Loop / recursion guards.
pub(crate) const MAX_RUNS_PER_MINUTE: usize = 60;
pub(crate) const MAX_WORKFLOW_CALL_DEPTH: u8 = 3;
/// Payload key carrying the event-chain depth: a run that emits an
/// event stamps the depth its downstream run should start at, so an
/// event-triggered chain terminates at `MAX_WORKFLOW_CALL_DEPTH`
/// instead of looping forever (see `on_bus_event` in `runs.rs`).
pub(crate) const EVENT_DEPTH_KEY: &str = "workflow_event_depth";
/// Retries for outbound webhook steps: 3 attempts total, with
/// exponential backoff (1s, then 2s, then 4s between attempts).
pub(crate) const MAX_WEBHOOK_ATTEMPTS: u32 = 3;
pub(crate) const WEBHOOK_BACKOFF_SECS: [u64; 2] = [1, 2];
/// Outbound webhook timeout in seconds.
pub(crate) const WEBHOOK_TIMEOUT_SECS: u64 = 10;
/// Rate-limit window in seconds.
pub(crate) const RATE_WINDOW_SECS: i64 = 60;
/// Payload summary: strings longer than this are truncated.
pub(crate) const SUMMARY_STRING_LEN: usize = 128;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_WORKFLOWS_VIEW: &str = "workflows.view";
pub(crate) const PERM_WORKFLOWS_MANAGE: &str = "workflows.manage";

/// Hook event names.
pub(crate) const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";
pub(crate) const JOB_KIND_WORKFLOWS_RUN: &str = "workflows.run";
pub(crate) const WORKFLOWS_RUN_REQUESTED: &str = "workflows.run_requested";
pub(crate) const NOTIFICATION_REQUESTED: &str = "notification.requested";
pub(crate) const MAIL_SEND_REQUESTED: &str = "mail.send_requested";
pub(crate) const TASK_CREATE_REQUESTED: &str = "task.create_requested";
pub(crate) const TICKET_CREATE_REQUESTED: &str = "ticket.create_requested";

/// Field-name segments that mark a value as a secret: such values
/// are never stored in a run's payload summary, never echoed in a
/// condition evaluation, and never written to a step detail.
pub(crate) const SECRET_SEGMENTS: [&str; 10] = [
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "authorization",
    "auth",
    "private_key",
    "credit_card",
];

/// (module, field) pairs the `update_record` action may write.
/// Anything else is rejected at save time - never an arbitrary
/// table write. The owning module must consume the
/// `{module}.update_requested` event; today no module does (see
/// the honesty note), so the step records `skipped` until one
/// does.
pub(crate) const UPDATE_ALLOWLIST: [(&str, &[&str]); 3] = [
    ("tasks", &["status", "priority", "assignee_id"]),
    ("tickets", &["status", "priority", "assignee_id"]),
    ("projects", &["status"]),
];

/// Modules the `assign_user` / `add_tag` actions may target.
pub(crate) const TARGET_MODULES: [&str; 2] = ["tasks", "tickets"];

/// Accepted priority values for task / ticket actions.
pub(crate) const PRIORITIES: [&str; 6] = ["low", "medium", "normal", "high", "urgent", "critical"];
