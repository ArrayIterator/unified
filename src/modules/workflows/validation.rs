// Workflows module - validation helpers.
//
// Split from `src/modules/workflows.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use uuid::Uuid;

use super::constants::{MAIL_SEND_REQUESTED, MAX_ACTIONS, MAX_ALLOWED_HOSTS, MAX_BODY_LEN, MAX_CONDITIONS_PER_GROUP, MAX_CONDITION_DEPTH, MAX_CONDITION_VALUE_BYTES, MAX_CRON_LEN, MAX_DESCRIPTION_LEN, MAX_EMAIL_LEN, MAX_EVENT_NAME_LEN, MAX_FIELD_PATH_LEN, MAX_FIELD_SEGMENTS, MAX_HOST_LEN, MAX_IN_VALUES, MAX_NAME_LEN, MAX_TAG_LEN, MAX_TIMEZONE_LEN, MAX_TITLE_LEN, MAX_URL_LEN, MAX_USER_ID_LEN, MAX_WEBHOOK_BODY_BYTES, MAX_WEBHOOK_HEADERS, NOTIFICATION_REQUESTED, PRIORITIES, SECRET_SEGMENTS, TARGET_MODULES, TASK_CREATE_REQUESTED, TICKET_CREATE_REQUESTED, UPDATE_ALLOWLIST};
use super::state::{parse_uuid, require_text};
use super::types::{ConditionGroup, ConditionLeaf, ConditionNode, TriggerKind, WorkflowAction, WorkflowTrigger};

/// A timezone is an IANA name (`"Asia/Jakarta"`, `"UTC"`): letters,
/// digits, `_`, `-`, `+`, and `/` only, no empty segments. Shape
/// only; no conversion is done (all stored times are UTC).
pub(crate) fn valid_timezone(timezone: &str) -> bool {
    if timezone.is_empty() || timezone.len() > MAX_TIMEZONE_LEN {
        return false;
    }
    if timezone == "UTC" {
        return true;
    }
    timezone.contains('/')
        && timezone.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
        })
}

/// A bus event name: lowercase letters, digits, dots, underscores,
/// and hyphens, never leading / trailing with a dot, no empty
/// segments (for example `invoice.overdue`, `ticket.created`).
pub(crate) fn valid_event_name(event: &str) -> bool {
    if event.is_empty() || event.len() > MAX_EVENT_NAME_LEN {
        return false;
    }
    if event.starts_with('.') || event.ends_with('.') || event.contains("..") {
        return false;
    }
    event
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-')
}

/// A 5-field cron expression where each field is `*`, `*/N`, a
/// plain number, or a comma-separated list of numbers. Same shape
/// rule as the scheduler module's `valid_schedule` (an interval in
/// seconds is not accepted here: a schedule trigger is always a
/// cron expression). Every value is range-checked for its field
/// (minute 0-59, hour 0-23, day 1-31, month 1-12, weekday 0-7)
/// and a step must be positive and no larger than the field's
/// maximum, so `99 99 99 99 99`, hour 25, and `*/999999` are all
/// rejected.
pub(crate) fn valid_cron(cron: &str) -> bool {
    let cron = cron.trim();
    if cron.is_empty() || cron.len() > MAX_CRON_LEN {
        return false;
    }
    let fields: Vec<&str> = cron.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    const RANGES: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
    fields
        .iter()
        .zip(RANGES)
        .all(|(field, (min, max))| valid_cron_field(field, min, max))
}

/// One field of a cron expression, range-checked: `*`, a step
/// `*/N` with `1 <= N <= max`, or a comma-separated list of plain
/// numbers each within `min..=max`.
pub(crate) fn valid_cron_field(field: &str, min: u32, max: u32) -> bool {
    if field == "*" {
        return true;
    }
    if let Some(step) = field.strip_prefix("*/") {
        return step.parse::<u32>().is_ok_and(|n| n > 0 && n <= max);
    }
    field.split(',').all(|part| {
        !part.is_empty() && part.parse::<u32>().is_ok_and(|n| (min..=max).contains(&n))
    })
}

pub(crate) fn valid_email(email: &str) -> bool {
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

/// A webhook host allowlist entry: a bare hostname
/// (`hooks.example.com`) or a wildcard (`*.example.com`).
pub(crate) fn valid_allowed_host(host: &str) -> bool {
    if host.is_empty() || host.len() > MAX_HOST_LEN {
        return false;
    }
    let host = host.strip_prefix("*.").unwrap_or(host);
    if host.starts_with('.') || host.ends_with('.') || host.contains("..") {
        return false;
    }
    host.split('.').all(|segment| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !segment.starts_with('-')
            && !segment.ends_with('-')
    })
}

pub(crate) fn host_allowed(host: &str, allowlist: &[String]) -> bool {
    let host = host.to_lowercase();
    allowlist.iter().any(|entry| {
        let entry = entry.to_lowercase();
        if let Some(suffix) = entry.strip_prefix("*.") {
            host != suffix && host.ends_with(&format!(".{suffix}"))
        } else {
            host == entry
        }
    })
}

pub(crate) fn is_secret_segment(segment: &str) -> bool {
    let lower = segment.to_lowercase();
    SECRET_SEGMENTS
        .iter()
        .any(|secret| lower.contains(secret))
}

/// Generate a fresh webhook token: 32 random bytes, hex-encoded
/// (64 characters). The raw token is returned once to the caller;
/// only its SHA-256 hash is stored.
pub(crate) fn generate_webhook_token() -> (String, String) {
    let bytes: [u8; 32] = rand::random();
    let token: String = {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for &byte in bytes.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    };
    let hash = token_hash(&token);
    (token, hash)
}

pub(crate) fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

/// Constant-time comparison of two hash strings, so token lookup
/// does not leak the expected value through timing.
pub(crate) fn hashes_equal(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Validation (save time)
// ---------------------------------------------------------------------------

pub(crate) fn validate_trigger(trigger: &WorkflowTrigger) -> UnifiedResult<()> {
    match trigger.kind {
        TriggerKind::Event => {
            let event = trigger.event.as_deref().unwrap_or("");
            if !valid_event_name(event) {
                return Err(UnifiedError::bad_request(
                    "trigger.event must be a valid bus event name such as \"invoice.overdue\"",
                ));
            }
        }
        TriggerKind::Schedule => {
            let cron = trigger.cron.as_deref().unwrap_or("");
            if !valid_cron(cron) {
                return Err(UnifiedError::bad_request(
                    "trigger.cron must be a 5-field cron expression such as \"0 9 * * *\"",
                ));
            }
            let timezone = trigger.timezone.as_deref().unwrap_or("UTC");
            if !valid_timezone(timezone) {
                return Err(UnifiedError::bad_request(
                    "trigger.timezone must be a valid IANA timezone name such as \"Asia/Jakarta\"",
                ));
            }
        }
        TriggerKind::Webhook | TriggerKind::Manual => {}
    }
    Ok(())
}

/// Validate a condition field path: `payload.` plus 1..=8
/// segments of letters, digits, or underscores.
pub(crate) fn validate_field_path(field: &str) -> UnifiedResult<Vec<String>> {
    if field.len() > MAX_FIELD_PATH_LEN {
        return Err(UnifiedError::bad_request(
            "condition field must be at most 256 characters",
        ));
    }
    let Some(rest) = field.strip_prefix("payload.") else {
        return Err(UnifiedError::bad_request(
            "condition field must start with \"payload.\" (for example \"payload.order.total\")",
        ));
    };
    if rest.is_empty() {
        return Err(UnifiedError::bad_request(
            "condition field must name a field under \"payload.\"",
        ));
    }
    let segments: Vec<String> = rest.split('.').map(str::to_string).collect();
    if segments.len() > MAX_FIELD_SEGMENTS {
        return Err(UnifiedError::bad_request(
            "condition field must have at most 8 segments",
        ));
    }
    for segment in &segments {
        if segment.is_empty()
            || !segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(UnifiedError::bad_request(format!(
                "condition field has an invalid segment: \"{segment}\""
            )));
        }
    }
    Ok(segments)
}

pub(crate) fn validate_condition_leaf(leaf: &ConditionLeaf) -> UnifiedResult<()> {
    validate_field_path(&leaf.field)?;
    match leaf.operator.as_str() {
        "equals" | "not_equals" | "contains" | "greater_than" | "less_than" | "in" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "unknown condition operator \"{other}\" (expected equals, not_equals, contains, greater_than, less_than, or in)"
            )));
        }
    }
    let serialized = serde_json::to_string(&leaf.value)
        .unwrap_or_default();
    if serialized.len() > MAX_CONDITION_VALUE_BYTES {
        return Err(UnifiedError::bad_request(
            "condition value must be at most 4096 bytes when serialized",
        ));
    }
    if leaf.operator == "in" {
        let values = leaf.value.as_array().ok_or_else(|| {
            UnifiedError::bad_request("condition operator \"in\" requires an array value")
        })?;
        if values.is_empty() || values.len() > MAX_IN_VALUES {
            return Err(UnifiedError::bad_request(
                "condition operator \"in\" requires between 1 and 100 values",
            ));
        }
    }
    if leaf.operator == "contains" && !leaf.value.is_string() && !leaf.value.is_array() {
        return Err(UnifiedError::bad_request(
            "condition operator \"contains\" requires a string or array value",
        ));
    }
    if (leaf.operator == "greater_than" || leaf.operator == "less_than")
        && !value_is_numeric(&leaf.value)
    {
        return Err(UnifiedError::bad_request(format!(
            "condition operator \"{}\" requires a numeric value",
            leaf.operator
        )));
    }
    Ok(())
}

pub(crate) fn value_is_numeric(value: &Value) -> bool {
    if value.is_number() {
        return true;
    }
    value
        .as_str()
        .is_some_and(|raw| raw.trim().parse::<f64>().is_ok())
}

pub(crate) fn validate_condition_group(group: &ConditionGroup, depth: usize) -> UnifiedResult<()> {
    if depth > MAX_CONDITION_DEPTH {
        return Err(UnifiedError::bad_request(
            "conditions nest at most 3 levels deep",
        ));
    }
    if group.operator != "and" && group.operator != "or" {
        return Err(UnifiedError::bad_request(
            "condition group operator must be \"and\" or \"or\"",
        ));
    }
    if group.conditions.is_empty() || group.conditions.len() > MAX_CONDITIONS_PER_GROUP {
        return Err(UnifiedError::bad_request(
            "a condition group must hold between 1 and 50 conditions",
        ));
    }
    for node in &group.conditions {
        match node {
            ConditionNode::Group(nested) => validate_condition_group(nested, depth + 1)?,
            ConditionNode::Leaf(leaf) => validate_condition_leaf(leaf)?,
        }
    }
    Ok(())
}

pub(crate) fn validate_conditions(conditions: &Option<ConditionGroup>) -> UnifiedResult<()> {
    if let Some(group) = conditions {
        validate_condition_group(group, 1)?;
    }
    Ok(())
}

pub(crate) fn config_text(config: &Value, field: &str, max: usize, required: bool) -> UnifiedResult<Option<String>> {
    match config.get(field) {
        None | Some(Value::Null) => {
            if required {
                Err(UnifiedError::bad_request(format!(
                    "action config requires \"{field}\""
                )))
            } else {
                Ok(None)
            }
        }
        Some(Value::String(raw)) => Ok(Some(require_text(raw, field, max)?)),
        Some(_) => Err(UnifiedError::bad_request(format!(
            "action config \"{field}\" must be a string"
        ))),
    }
}

pub(crate) fn validate_action(action: &WorkflowAction, own_id: Option<Uuid>) -> UnifiedResult<()> {
    let config = &action.config;
    if !config.is_null() && !config.is_object() {
        return Err(UnifiedError::bad_request(
            "action config must be a JSON object",
        ));
    }
    match action.kind.as_str() {
        "send_notification" => {
            config_text(config, "title", MAX_TITLE_LEN, true)?;
            config_text(config, "body", MAX_BODY_LEN, true)?;
            if let Some(kind) = config_text(config, "kind", 128, false)? {
                if kind.trim().is_empty() {
                    return Err(UnifiedError::bad_request(
                        "action config \"kind\" must not be empty",
                    ));
                }
            }
            if let Some(priority) = config.get("priority").and_then(Value::as_str) {
                if !["low", "normal", "high", "urgent"].contains(&priority) {
                    return Err(UnifiedError::bad_request(
                        "action config \"priority\" must be low, normal, high, or urgent",
                    ));
                }
            }
            let has_user = config.get("user_id").and_then(Value::as_str).is_some_and(|v| !v.trim().is_empty())
                || config.get("user_ids").and_then(Value::as_array).is_some_and(|v| !v.is_empty());
            if !has_user {
                return Err(UnifiedError::bad_request(
                    "send_notification requires \"user_id\" or a non-empty \"user_ids\" list",
                ));
            }
        }
        "send_email" => {
            let to = config_text(config, "to", MAX_EMAIL_LEN, true)?.unwrap_or_default();
            if !valid_email(&to) {
                return Err(UnifiedError::bad_request(format!("Invalid email address: {to}")));
            }
            let subject = config_text(config, "subject", MAX_TITLE_LEN, false)?;
            let template_id = config_text(config, "template_id", 128, false)?;
            if subject.is_none() && template_id.is_none() {
                return Err(UnifiedError::bad_request(
                    "send_email requires \"subject\" or \"template_id\"",
                ));
            }
            config_text(config, "body_text", MAX_BODY_LEN, false)?;
            config_text(config, "body_html", MAX_BODY_LEN, false)?;
        }
        "create_task" => {
            config_text(config, "title", MAX_NAME_LEN, true)?;
            config_text(config, "description", MAX_DESCRIPTION_LEN, false)?;
            if let Some(priority) = config.get("priority").and_then(Value::as_str) {
                if !PRIORITIES.contains(&priority) {
                    return Err(UnifiedError::bad_request(format!(
                        "action config \"priority\" must be one of: {}",
                        PRIORITIES.join(", ")
                    )));
                }
            }
            if let Some(assignee) = config.get("assignee_id").and_then(Value::as_str) {
                require_text(assignee, "assignee_id", MAX_USER_ID_LEN)?;
            }
        }
        "create_ticket" => {
            config_text(config, "title", MAX_NAME_LEN, true)?;
            config_text(config, "body", MAX_DESCRIPTION_LEN, false)?;
            if let Some(priority) = config.get("priority").and_then(Value::as_str) {
                if !PRIORITIES.contains(&priority) {
                    return Err(UnifiedError::bad_request(format!(
                        "action config \"priority\" must be one of: {}",
                        PRIORITIES.join(", ")
                    )));
                }
            }
            for field in ["category", "product", "department_id"] {
                config_text(config, field, 128, false)?;
            }
        }
        "update_record" => {
            let module = config_text(config, "target_module", 64, true)?.unwrap_or_default();
            let field = config_text(config, "field", 64, true)?.unwrap_or_default();
            let allowed = UPDATE_ALLOWLIST
                .iter()
                .find(|(name, _)| *name == module)
                .map(|(_, fields)| *fields);
            let Some(fields) = allowed else {
                return Err(UnifiedError::bad_request(format!(
                    "update_record target_module \"{module}\" is not allowlisted (allowed: tasks, tickets, projects)"
                )));
            };
            if !fields.contains(&field.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "update_record field \"{field}\" is not allowlisted for module \"{module}\" (allowed: {})",
                    fields.join(", ")
                )));
            }
            let target_id = config_text(config, "target_id", 128, true)?.unwrap_or_default();
            parse_uuid(&target_id, "update_record target")?;
            if config.get("value").is_none() || config.get("value") == Some(&Value::Null) {
                return Err(UnifiedError::bad_request(
                    "update_record requires a \"value\"",
                ));
            }
            if let Some(value) = config.get("value").and_then(Value::as_str) {
                if value.len() > 256 {
                    return Err(UnifiedError::bad_request(
                        "update_record \"value\" must be at most 256 characters",
                    ));
                }
            }
        }
        "call_webhook" => {
            let url = config_text(config, "url", MAX_URL_LEN, true)?.unwrap_or_default();
            validate_webhook_url_shape(&url)?;
            let method = config
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("POST");
            if !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&method) {
                return Err(UnifiedError::bad_request(
                    "call_webhook \"method\" must be GET, POST, PUT, PATCH, or DELETE",
                ));
            }
            if let Some(headers) = config.get("headers") {
                let map = headers.as_object().ok_or_else(|| {
                    UnifiedError::bad_request("call_webhook \"headers\" must be an object")
                })?;
                if map.len() > MAX_WEBHOOK_HEADERS {
                    return Err(UnifiedError::bad_request(
                        "call_webhook allows at most 20 headers",
                    ));
                }
                for (name, value) in map {
                    validate_header_name(name)?;
                    let raw = value.as_str().ok_or_else(|| {
                        UnifiedError::bad_request("call_webhook header values must be strings")
                    })?;
                    if raw.len() > 4096 {
                        return Err(UnifiedError::bad_request(
                            "call_webhook header values must be at most 4096 characters",
                        ));
                    }
                }
            }
            if let Some(body) = config.get("body") {
                if serde_json::to_string(body).unwrap_or_default().len() > MAX_WEBHOOK_BODY_BYTES {
                    return Err(UnifiedError::bad_request(
                        "call_webhook \"body\" must be at most 65536 bytes when serialized",
                    ));
                }
            }
        }
        "assign_user" => {
            let module = config_text(config, "target_module", 64, true)?.unwrap_or_default();
            if !TARGET_MODULES.contains(&module.as_str()) {
                return Err(UnifiedError::bad_request(
                    "assign_user target_module must be \"tasks\" or \"tickets\"",
                ));
            }
            let target_id = config_text(config, "target_id", 128, true)?.unwrap_or_default();
            parse_uuid(&target_id, "assign_user target")?;
            config_text(config, "user_id", MAX_USER_ID_LEN, true)?;
        }
        "add_tag" => {
            let module = config_text(config, "target_module", 64, true)?.unwrap_or_default();
            if !TARGET_MODULES.contains(&module.as_str()) {
                return Err(UnifiedError::bad_request(
                    "add_tag target_module must be \"tasks\" or \"tickets\"",
                ));
            }
            let target_id = config_text(config, "target_id", 128, true)?.unwrap_or_default();
            parse_uuid(&target_id, "add_tag target")?;
            config_text(config, "tag", MAX_TAG_LEN, true)?;
        }
        "run_workflow" => {
            let target = config_text(config, "workflow_id", 128, true)?.unwrap_or_default();
            let target_id = parse_uuid(&target, "run_workflow target")?;
            if Some(target_id) == own_id {
                return Err(UnifiedError::bad_request(
                    "a workflow cannot run itself (self-trigger is rejected)",
                ));
            }
        }
        other => {
            return Err(UnifiedError::bad_request(format!(
                "unknown action kind \"{other}\" (expected send_notification, send_email, create_task, create_ticket, update_record, call_webhook, assign_user, add_tag, or run_workflow)"
            )));
        }
    }
    Ok(())
}

/// Shape checks for a webhook URL that need no network: http(s)
/// only, no embedded credentials, host present. The SSRF checks
/// (allowlist + DNS resolution) run at execution time.
pub(crate) fn validate_webhook_url_shape(url: &str) -> UnifiedResult<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(UnifiedError::bad_request(
            "call_webhook \"url\" must start with http:// or https://",
        ));
    }
    if url.contains('@') {
        // Reject userinfo (`http://user:pass@host/...`) outright:
        // credentials never belong in a workflow action URL.
        let after_scheme = url.split("://").nth(1).unwrap_or("");
        let authority = after_scheme.split('/').next().unwrap_or("");
        if authority.contains('@') {
            return Err(UnifiedError::bad_request(
                "call_webhook \"url\" must not contain credentials",
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_header_name(name: &str) -> UnifiedResult<()> {
    let lower = name.to_lowercase();
    if ["host", "content-length", "connection", "transfer-encoding"].contains(&lower.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "call_webhook header \"{name}\" is not allowed"
        )));
    }
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(format!(
            "call_webhook header name \"{name}\" is invalid"
        )));
    }
    Ok(())
}

pub(crate) fn validate_actions(actions: &[WorkflowAction], own_id: Option<Uuid>) -> UnifiedResult<()> {
    if actions.len() > MAX_ACTIONS {
        return Err(UnifiedError::bad_request(format!(
            "a workflow holds at most {MAX_ACTIONS} actions"
        )));
    }
    for action in actions {
        validate_action(action, own_id)?;
    }
    Ok(())
}

/// The bus event one action emits, if any. `run_workflow` runs
/// another workflow directly (no bus event) and `call_webhook`
/// calls out over HTTP, so neither can re-trigger this workflow
/// through the bus.
pub(crate) fn action_emitted_event(action: &WorkflowAction) -> Option<String> {
    match action.kind.as_str() {
        "send_notification" => Some(NOTIFICATION_REQUESTED.to_string()),
        "send_email" => Some(MAIL_SEND_REQUESTED.to_string()),
        "create_task" => Some(TASK_CREATE_REQUESTED.to_string()),
        "create_ticket" => Some(TICKET_CREATE_REQUESTED.to_string()),
        "update_record" => action
            .config
            .get("target_module")
            .and_then(Value::as_str)
            .filter(|module| !module.is_empty())
            .map(|module| format!("{module}.update_requested")),
        "assign_user" => action
            .config
            .get("target_module")
            .and_then(Value::as_str)
            .filter(|module| !module.is_empty())
            .map(|module| format!("{module}.assign_requested")),
        "add_tag" => action
            .config
            .get("target_module")
            .and_then(Value::as_str)
            .filter(|module| !module.is_empty())
            .map(|module| format!("{module}.tag_requested")),
        _ => None,
    }
}

/// Reject a workflow whose trigger event is also emitted by one of
/// its own actions: every run would re-trigger the workflow (a
/// direct self-loop). Cross-workflow loops are stopped at runtime
/// by the event-chain depth limit instead (see `on_bus_event`).
pub(crate) fn validate_no_self_trigger_event(
    trigger: &WorkflowTrigger,
    actions: &[WorkflowAction],
) -> UnifiedResult<()> {
    if trigger.kind != TriggerKind::Event {
        return Ok(());
    }
    let Some(event) = trigger.event.as_deref() else {
        return Ok(());
    };
    for action in actions {
        if let Some(emitted) = action_emitted_event(action) {
            if emitted == event {
                return Err(UnifiedError::bad_request(format!(
                    "trigger event \"{event}\" is also emitted by this workflow's \"{}\" action; that would trigger the workflow again on every run",
                    action.kind
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_allowed_hosts(hosts: &[String]) -> UnifiedResult<()> {
    if hosts.len() > MAX_ALLOWED_HOSTS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_ALLOWED_HOSTS} webhook hosts are allowed per workflow"
        )));
    }
    let mut seen = HashSet::new();
    for host in hosts {
        let lower = host.trim().to_lowercase();
        if !valid_allowed_host(&lower) {
            return Err(UnifiedError::bad_request(format!(
                "invalid webhook host in allowlist: \"{host}\""
            )));
        }
        if !seen.insert(lower) {
            return Err(UnifiedError::bad_request(format!(
                "duplicate webhook host in allowlist: \"{host}\""
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(field: &str, operator: &str, value: Value) -> ConditionNode {
        ConditionNode::Leaf(ConditionLeaf {
            field: field.to_string(),
            operator: operator.to_string(),
            value,
        })
    }

    fn group(operator: &str, conditions: Vec<ConditionNode>) -> ConditionGroup {
        ConditionGroup {
            operator: operator.to_string(),
            conditions,
        }
    }

    #[test]
    fn valid_timezone_accepts_iana_names() {
        assert!(valid_timezone("UTC"));
        assert!(valid_timezone("Asia/Jakarta"));
        assert!(valid_timezone("America/New_York"));
        assert!(!valid_timezone(""));
        assert!(!valid_timezone("No/Such/"));
        assert!(!valid_timezone("Asia//Jakarta"));
        assert!(!valid_timezone("Bad Name"));
        assert!(!valid_timezone("a"));
    }

    #[test]
    fn valid_event_name_rules() {
        assert!(valid_event_name("invoice.overdue"));
        assert!(valid_event_name("ticket.created"));
        assert!(valid_event_name("a-b_c.d9"));
        assert!(!valid_event_name(".bad"));
        assert!(!valid_event_name("bad."));
        assert!(!valid_event_name("a..b"));
        assert!(!valid_event_name("BAD"));
        assert!(!valid_event_name(""));
    }

    #[test]
    fn valid_cron_shapes() {
        assert!(valid_cron("0 9 * * *"));
        assert!(valid_cron("* * * * *"));
        assert!(valid_cron("*/15 * * * *"));
        assert!(valid_cron("5,10 9 * * *"));
        assert!(valid_cron("0 0 31 12 7"));
        assert!(valid_cron("*/59 * * * *"));
        assert!(!valid_cron("0 9 * *"));
        assert!(!valid_cron("0 9 * * MON"));
        assert!(!valid_cron(""));
        assert!(!valid_cron("0 9 * * * *"));
    }

    #[test]
    fn valid_cron_rejects_out_of_range_values() {
        // Every field is range-checked: minute 0-59, hour 0-23,
        // day 1-31, month 1-12, weekday 0-7.
        assert!(!valid_cron("99 99 99 99 99"));
        assert!(!valid_cron("60 * * * *"));
        assert!(!valid_cron("0 25 * * *"));
        assert!(!valid_cron("0 24 * * *"));
        assert!(!valid_cron("0 9 0 * *"));
        assert!(!valid_cron("0 9 32 * *"));
        assert!(!valid_cron("0 9 * 0 *"));
        assert!(!valid_cron("0 9 * 13 *"));
        assert!(!valid_cron("0 9 * * 8"));
        assert!(!valid_cron("0 9 * * -1"));
        assert!(!valid_cron("0,60 9 * * *"));
        // Steps must be positive and no larger than the field max.
        assert!(!valid_cron("*/0 * * * *"));
        assert!(!valid_cron("*/60 * * * *"));
        assert!(!valid_cron("*/999999 * * * *"));
        assert!(!valid_cron("* */24 * * *"));
        assert!(!valid_cron("* * */32 * *"));
        assert!(!valid_cron("* * * */13 *"));
        assert!(!valid_cron("* * * * */8"));
        // Boundary values are accepted.
        assert!(valid_cron("59 23 31 12 7"));
        assert!(valid_cron("0 0 1 1 0"));
        assert!(valid_cron("*/59 */23 */31 */12 */7"));
    }

    #[test]
    fn self_trigger_event_is_rejected() {
        let trigger = WorkflowTrigger {
            kind: TriggerKind::Event,
            event: Some("tasks.update_requested".to_string()),
            cron: None,
            timezone: None,
        };
        let looping = WorkflowAction {
            kind: "update_record".to_string(),
            config: json!({
                "target_module": "tasks",
                "target_id": Uuid::now_v7().to_string(),
                "field": "status",
                "value": "done",
            }),
        };
        assert!(validate_no_self_trigger_event(&trigger, &[looping]).is_err());
        let other_event = WorkflowAction {
            kind: "update_record".to_string(),
            config: json!({
                "target_module": "tickets",
                "target_id": Uuid::now_v7().to_string(),
                "field": "status",
                "value": "done",
            }),
        };
        assert!(validate_no_self_trigger_event(&trigger, &[other_event]).is_ok());
        let notifier = WorkflowAction {
            kind: "send_notification".to_string(),
            config: json!({"title": "hi", "body": "there", "user_id": "u1"}),
        };
        assert!(validate_no_self_trigger_event(&trigger, &[notifier]).is_ok());
        let notify_trigger = WorkflowTrigger {
            kind: TriggerKind::Event,
            event: Some("notification.requested".to_string()),
            cron: None,
            timezone: None,
        };
        assert!(validate_no_self_trigger_event(&notify_trigger, &[notifier]).is_err());
        let scheduled = WorkflowTrigger {
            kind: TriggerKind::Schedule,
            event: None,
            cron: Some("0 9 * * *".to_string()),
            timezone: Some("UTC".to_string()),
        };
        assert!(validate_no_self_trigger_event(&scheduled, &[notifier]).is_ok());
    }

    #[test]
    fn valid_email_rules() {
        assert!(valid_email("a@b.com"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a b@c.com"));
        assert!(!valid_email("a@@b.com"));
        assert!(!valid_email(""));
        assert!(!valid_email("a@.b.com"));
        assert!(!valid_email("a@b.com."));
    }

    #[test]
    fn allowed_host_matching() {
        assert!(valid_allowed_host("hooks.example.com"));
        assert!(valid_allowed_host("*.example.com"));
        assert!(!valid_allowed_host("-bad.example.com"));
        assert!(!valid_allowed_host("bad..com"));
        assert!(!valid_allowed_host(""));
        assert!(host_allowed("sub.example.com", &["*.example.com".to_string()]));
        assert!(!host_allowed("example.com", &["*.example.com".to_string()]));
        assert!(host_allowed("api.example.com", &["api.example.com".to_string()]));
        assert!(host_allowed("API.Example.COM", &["api.example.com".to_string()]));
    }

    #[test]
    fn secret_segments_are_detected() {
        assert!(is_secret_segment("api_token"));
        assert!(is_secret_segment("password_hash"));
        assert!(is_secret_segment("credit_card_number"));
        assert!(is_secret_segment("author"));
        assert!(!is_secret_segment("username"));
        assert!(!is_secret_segment("public_key"));
    }

    #[test]
    fn token_hash_is_sha256_hex() {
        assert_eq!(
            token_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(token_hash("abc").len(), 64);
        assert_eq!(
            token_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hashes_equal_compares_exactly() {
        assert!(hashes_equal("abc", "abc"));
        assert!(!hashes_equal("abc", "abd"));
        assert!(!hashes_equal("abc", "abcd"));
    }

    #[test]
    fn validate_field_path_rules() {
        assert_eq!(
            validate_field_path("payload.order.total").unwrap(),
            vec!["order".to_string(), "total".to_string()]
        );
        assert!(validate_field_path("order.total").is_err());
        assert!(validate_field_path("payload.").is_err());
        assert!(validate_field_path("payload.a.b.c.d.e.f.g.h.i").is_err());
        assert!(validate_field_path("payload.a-b").is_err());
        assert!(validate_field_path("payload.a1_b2").is_ok());
    }

    #[test]
    fn validate_condition_leaf_rules() {
        let ok = ConditionLeaf {
            field: "payload.total".to_string(),
            operator: "equals".to_string(),
            value: json!(10),
        };
        assert!(validate_condition_leaf(&ok).is_ok());
        let bad_operator = ConditionLeaf {
            field: "payload.total".to_string(),
            operator: "bogus".to_string(),
            value: json!(10),
        };
        assert!(validate_condition_leaf(&bad_operator).is_err());
        let bad_field = ConditionLeaf {
            field: "total".to_string(),
            operator: "equals".to_string(),
            value: json!(10),
        };
        assert!(validate_condition_leaf(&bad_field).is_err());
        let in_without_array = ConditionLeaf {
            field: "payload.total".to_string(),
            operator: "in".to_string(),
            value: json!(10),
        };
        assert!(validate_condition_leaf(&in_without_array).is_err());
        let in_with_array = ConditionLeaf {
            field: "payload.total".to_string(),
            operator: "in".to_string(),
            value: json!([1, 2]),
        };
        assert!(validate_condition_leaf(&in_with_array).is_ok());
        let contains_number = ConditionLeaf {
            field: "payload.name".to_string(),
            operator: "contains".to_string(),
            value: json!(42),
        };
        assert!(validate_condition_leaf(&contains_number).is_err());
        let greater_than_text = ConditionLeaf {
            field: "payload.total".to_string(),
            operator: "greater_than".to_string(),
            value: json!("abc"),
        };
        assert!(validate_condition_leaf(&greater_than_text).is_err());
    }

    #[test]
    fn validate_condition_group_rules() {
        let one_leaf = group("and", vec![leaf("payload.total", "equals", json!(10))]);
        assert!(validate_condition_group(&one_leaf, 1).is_ok());
        assert!(validate_conditions(&Some(one_leaf)).is_ok());
        assert!(validate_conditions(&None).is_ok());
        let empty = group("and", vec![]);
        assert!(validate_condition_group(&empty, 1).is_err());
        let bad_operator = group("xor", vec![leaf("payload.total", "equals", json!(10))]);
        assert!(validate_condition_group(&bad_operator, 1).is_err());
        let deep_ok = group(
            "and",
            vec![ConditionNode::Group(group(
                "and",
                vec![ConditionNode::Group(group(
                    "and",
                    vec![leaf("payload.total", "equals", json!(10))],
                ))],
            ))],
        );
        assert!(validate_condition_group(&deep_ok, 1).is_ok());
        let too_deep = group(
            "and",
            vec![ConditionNode::Group(group(
                "and",
                vec![ConditionNode::Group(group(
                    "and",
                    vec![ConditionNode::Group(group(
                        "and",
                        vec![leaf("payload.total", "equals", json!(10))],
                    ))],
                ))],
            ))],
        );
        assert!(validate_condition_group(&too_deep, 1).is_err());
    }

    #[test]
    fn value_is_numeric_accepts_numbers_and_numeric_strings() {
        assert!(value_is_numeric(&json!(42)));
        assert!(value_is_numeric(&json!("3.5")));
        assert!(!value_is_numeric(&json!("abc")));
        assert!(!value_is_numeric(&json!(true)));
        assert!(!value_is_numeric(&json!(null)));
    }
    #[test]
    fn token_hash_matches_known_sha256_vector() {
        assert_eq!(
            token_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn generate_webhook_token_is_64_hex_and_hash_matches() {
        let (token, hash) = generate_webhook_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(token_hash(&token), hash);
        assert_eq!(hash.len(), 64);
    }

}
