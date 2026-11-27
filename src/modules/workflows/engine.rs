// Workflows module - condition evaluation and action execution.
//
// Split from `src/modules/workflows.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use uuid::Uuid;

use super::constants::{EVENT_DEPTH_KEY, MAIL_SEND_REQUESTED, MAX_WEBHOOK_ATTEMPTS, MAX_WORKFLOW_CALL_DEPTH, NOTIFICATION_REQUESTED, SUMMARY_STRING_LEN, TASK_CREATE_REQUESTED, TICKET_CREATE_REQUESTED, WEBHOOK_BACKOFF_SECS, WEBHOOK_TIMEOUT_SECS};
use super::runs::{RunStart, begin_run, finish_run_async, finish_run_sync};
use super::state::{WorkflowsState};
use super::types::{ConditionGroup, ConditionLeaf, ConditionNode, LeafEvaluation, RunStatus, Workflow, WorkflowAction};
use super::validation::{host_allowed, is_secret_segment, validate_field_path};

// Payload summary, conditions, templates

/// Redacted summary of a trigger payload for a run row: only
/// top-level keys, values replaced by their type (strings truncated
/// to 128 characters), secret-named keys dropped entirely. The full
/// payload is never stored - that is why a retry needs a
/// caller-supplied payload.
pub(crate) fn summarize_payload(payload: &Value) -> Value {
    match payload {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                if is_secret_segment(key) {
                    continue;
                }
                out.insert(key.clone(), summarize_value(value));
            }
            Value::Object(out)
        }
        other => summarize_value(other),
    }
}

pub(crate) fn summarize_value(value: &Value) -> Value {
    match value {
        Value::String(raw) => {
            let mut text = raw.clone();
            if text.len() > SUMMARY_STRING_LEN {
                text.truncate(SUMMARY_STRING_LEN);
                text.push_str("...");
            }
            json!({ "type": "string", "value": text })
        }
        Value::Number(number) => json!({ "type": "number", "value": number }),
        Value::Bool(flag) => json!({ "type": "boolean", "value": flag }),
        Value::Null => json!({ "type": "null" }),
        Value::Array(items) => json!({ "type": "array", "length": items.len() }),
        Value::Object(map) => {
            let mut keys: Vec<String> = map
                .keys()
                .filter(|key| !is_secret_segment(key))
                .cloned()
                .collect();
            keys.sort();
            keys.truncate(20);
            json!({ "type": "object", "keys": keys })
        }
    }
}

/// Resolve a `payload.a.b.c` field path against the trigger
/// payload. Returns `None` when any segment is missing.
pub(crate) fn resolve_field<'a>(payload: &'a Value, segments: &[String]) -> Option<&'a Value> {
    let mut current = payload;
    for segment in segments {
        current = current.get(segment)?;
    }
    Some(current)
}

pub(crate) fn values_equal(left: &Value, right: &Value) -> bool {
    if let (Some(a), Some(b)) = (left.as_f64(), right.as_f64()) {
        return a == b;
    }
    left == right
}

pub(crate) fn value_as_number(value: &Value) -> Option<f64> {
    if let Some(number) = value.as_f64() {
        return Some(number);
    }
    value
        .as_str()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
}

/// Evaluate one leaf against the trigger payload. A missing field
/// matches nothing (not even `not_equals`): a condition can only
/// pass on data that is actually present.
pub(crate) fn eval_leaf(leaf: &ConditionLeaf, payload: &Value) -> LeafEvaluation {
    let segments = validate_field_path(&leaf.field).unwrap_or_default();
    let secret = segments.iter().any(|segment| is_secret_segment(segment));
    let actual = segments
        .is_empty()
        .then_some(None)
        .unwrap_or_else(|| resolve_field(payload, &segments));
    let matched = match actual {
        None => false,
        Some(actual) => match leaf.operator.as_str() {
            "equals" => values_equal(actual, &leaf.value),
            "not_equals" => !values_equal(actual, &leaf.value),
            "contains" => {
                if let (Some(haystack), Some(needle)) =
                    (actual.as_str(), leaf.value.as_str())
                {
                    haystack.contains(needle)
                } else if let Some(items) = actual.as_array() {
                    items.iter().any(|item| values_equal(item, &leaf.value))
                } else {
                    false
                }
            }
            "greater_than" => matches!(
                (value_as_number(actual), value_as_number(&leaf.value)),
                (Some(a), Some(b)) if a > b
            ),
            "less_than" => matches!(
                (value_as_number(actual), value_as_number(&leaf.value)),
                (Some(a), Some(b)) if a < b
            ),
            "in" => leaf
                .value
                .as_array()
                .is_some_and(|values| values.iter().any(|item| values_equal(actual, item))),
            _ => false,
        },
    };
    LeafEvaluation {
        field: leaf.field.clone(),
        operator: leaf.operator.clone(),
        value: leaf.value.clone(),
        actual: if secret {
            Value::String("[redacted]".to_string())
        } else {
            actual.cloned().unwrap_or(Value::Null)
        },
        matched,
    }
}

/// Evaluate a condition group, collecting every leaf evaluation
/// (used by dry runs and tests so the caller sees exactly which
/// leaves passed).
pub(crate) fn eval_group(group: &ConditionGroup, payload: &Value) -> (bool, Vec<LeafEvaluation>) {
    let mut leaves = Vec::new();
    let mut child_results = Vec::new();
    for node in &group.conditions {
        match node {
            ConditionNode::Group(nested) => {
                let (matched, mut nested_leaves) = eval_group(nested, payload);
                leaves.append(&mut nested_leaves);
                child_results.push(matched);
            }
            ConditionNode::Leaf(leaf) => {
                let evaluation = eval_leaf(leaf, payload);
                child_results.push(evaluation.matched);
                leaves.push(evaluation);
            }
        }
    }
    let matched = match group.operator.as_str() {
        "and" => child_results.iter().all(|value| *value),
        "or" => child_results.iter().any(|value| *value),
        _ => false,
    };
    (matched, leaves)
}

/// Render `{payload.a.b}` placeholders inside a template string.
/// `{workflow.id}` and `{workflow.name}` resolve against the
/// workflow. Unknown or missing fields render as an empty string;
/// secret-named fields render as `[redacted]`.
pub(crate) fn render_template(template: &str, payload: &Value, workflow: &Workflow) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let (before, after_open) = rest.split_at(start);
        out.push_str(before);
        let Some(end) = after_open.find('}') else {
            out.push_str(after_open);
            break;
        };
        let placeholder = &after_open[1..end];
        let replacement = if placeholder == "workflow.id" {
            workflow.id.to_string()
        } else if placeholder == "workflow.name" {
            workflow.name.clone()
        } else if let Some(path) = placeholder.strip_prefix("payload.") {
            let segments: Vec<String> = path.split('.').map(str::to_string).collect();
            if segments.iter().any(|segment| is_secret_segment(segment)) {
                "[redacted]".to_string()
            } else {
                match resolve_field(payload, &segments) {
                    Some(Value::String(raw)) => raw.clone(),
                    Some(Value::Number(number)) => number.to_string(),
                    Some(Value::Bool(flag)) => flag.to_string(),
                    Some(Value::Null) | None => String::new(),
                    Some(other) => serde_json::to_string(other).unwrap_or_default(),
                }
            }
        } else {
            format!("{{{placeholder}}}")
        };
        out.push_str(&replacement);
        rest = &after_open[end + 1..];
    }
    out
}

/// Render templates inside a config value (strings only; objects
/// and arrays are walked recursively).
pub(crate) fn render_config_value(value: &Value, payload: &Value, workflow: &Workflow) -> Value {
    match value {
        Value::String(raw) => Value::String(render_template(raw, payload, workflow)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| render_config_value(item, payload, workflow))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                out.insert(
                    key.clone(),
                    render_config_value(item, payload, workflow),
                );
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

// Outbound webhook (SSRF-safe)

/// True when an IP address is safe to connect to from a workflow:
/// public only. Private, loopback, link-local, multicast,
/// unspecified, documentation, benchmarking, AS112, and
/// carrier-grade NAT ranges are all rejected, and a 6to4 address
/// (2002::/16) is judged by the IPv4 address embedded in it.
pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            let octets = ipv4.octets();
            if ipv4.is_private()
                || ipv4.is_loopback()
                || ipv4.is_link_local()
                || ipv4.is_multicast()
                || ipv4.is_unspecified()
                || ipv4.is_broadcast()
            {
                return false;
            }
            // 0.0.0.0/8 ("this network"), 100.64.0.0/10 (CGNAT),
            // 192.0.0.0/24 (IETF assignments), documentation
            // ranges, 198.18.0.0/15 (benchmarking), 240.0.0.0/4,
            // 192.31.196.0/24 and 192.175.48.0/24 (AS112).
            if octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                || (octets[0] == 192 && octets[1] == 31 && octets[2] == 196)
                || (octets[0] == 192 && octets[1] == 175 && octets[2] == 48)
                || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
                || (octets[0] == 198 && (18..=19).contains(&octets[1]))
                || octets[0] >= 240
            {
                return false;
            }
            true
        }
        IpAddr::V6(ipv6) => {
            if ipv6.is_loopback()
                || ipv6.is_multicast()
                || ipv6.is_unspecified()
                || ipv6.is_unique_local()
            {
                return false;
            }
            let segments = ipv6.segments();
            // fe80::/10 link-local, ::/128 handled above,
            // 2001:db8::/32 documentation.
            if (0xfe80..=0xfebf).contains(&segments[0]) {
                return false;
            }
            if segments[0] == 0x2001 && segments[1] == 0x0db8 {
                return false;
            }
            // ::ffff:a.b.c.d mapped IPv4: judge the mapped address.
            if let Some(mapped) = ipv6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(mapped));
            }
            // 2002::/16 6to4: the embedded IPv4 address lives in
            // segments 1-2; judge it like a mapped address.
            if segments[0] == 0x2002 {
                let embedded = Ipv4Addr::new(
                    (segments[1] >> 8) as u8,
                    (segments[1] & 0xff) as u8,
                    (segments[2] >> 8) as u8,
                    (segments[2] & 0xff) as u8,
                );
                return is_public_ip(IpAddr::V4(embedded));
            }
            true
        }
    }
}

/// Resolve a hostname and reject it unless every resolved address
/// is public. DNS is resolved again by the HTTP client when it
/// connects, so this check cannot fully close a rebinding window
/// (see the honesty note); the host allowlist is the primary
/// defence.
pub(crate) async fn resolve_host_is_public(host: &str, port: u16) -> UnifiedResult<()> {
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| UnifiedError::bad_request(format!("Cannot resolve webhook host: {host}")))?;
    let mut saw_any = false;
    for address in addresses {
        saw_any = true;
        if !is_public_ip(address.ip()) {
            return Err(UnifiedError::bad_request(format!(
                "Webhook host \"{host}\" resolves to a non-public address ({})",
                address.ip()
            )));
        }
    }
    if !saw_any {
        return Err(UnifiedError::bad_request(format!(
            "Webhook host \"{host}\" did not resolve"
        )));
    }
    Ok(())
}

/// Outcome of one executed action step.
pub(crate) struct StepOutcome {
    pub(crate) status: &'static str,
    pub(crate) attempts: u32,
    pub(crate) error: Option<String>,
    pub(crate) detail: Value,
}

pub(crate) fn step_success(detail: Value) -> StepOutcome {
    StepOutcome {
        status: "success",
        attempts: 1,
        error: None,
        detail,
    }
}

pub(crate) fn step_failed(error: String, detail: Value) -> StepOutcome {
    StepOutcome {
        status: "failed",
        attempts: 1,
        error: Some(error),
        detail,
    }
}

pub(crate) fn step_skipped(reason: &str, detail: Value) -> StepOutcome {
    StepOutcome {
        status: "skipped",
        attempts: 0,
        error: Some(reason.to_string()),
        detail,
    }
}

/// Emit an event and interpret the answers: no registered handler
/// means the action was not delivered (`skipped`); a result
/// carrying an `error` key means the consumer refused it
/// (`failed`); anything else is `success`. The created record's id
/// is captured from the first result when the consumer returns one.
/// `depth` is the emitting run's depth: the payload is stamped
/// with the depth a downstream event-triggered run should start at
/// (`depth + 1`), so an event chain terminates at
/// `MAX_WORKFLOW_CALL_DEPTH` instead of looping forever.
pub(crate) fn emit_action(
    state: &WorkflowsState,
    event: &str,
    payload: Value,
    depth: u8,
) -> StepOutcome {
    let Some(bus) = state.bus() else {
        return step_skipped(
            "no hook bus is attached; the action was not delivered",
            json!({ "event": event }),
        );
    };
    let handler_count = bus
        .registrations()
        .iter()
        .filter(|(name, _)| name == event)
        .count();
    if handler_count == 0 {
        return step_skipped(
            &format!("no handler is registered for \"{event}\"; the action was not delivered"),
            json!({ "event": event, "handlers": 0 }),
        );
    }
    let mut payload = payload;
    if let Value::Object(map) = &mut payload {
        map.insert(
            EVENT_DEPTH_KEY.to_string(),
            json!(depth.saturating_add(1)),
        );
    }
    let context = bus.emit(event, payload);
    let mut result_id: Option<String> = None;
    for result in &context.results {
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            return step_failed(
                format!("\"{event}\" was refused: {error}"),
                json!({ "event": event, "handlers": handler_count }),
            );
        }
        if result_id.is_none() {
            result_id = result.get("id").and_then(Value::as_str).map(str::to_string);
        }
    }
    step_success(json!({ "event": event, "handlers": handler_count, "result_id": result_id }))
}

/// Build the header map for an outbound webhook call. A
/// configured header whose name or value is not a valid HTTP
/// header fails the step: a bad header is never silently dropped
/// (a dropped `Authorization` header would send an unauthenticated
/// request that looks like success). The offending value is never
/// included in the error: header values can carry credentials.
pub(crate) fn build_webhook_headers(
    rendered: &Value,
) -> Result<reqwest::header::HeaderMap, String> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(map) = rendered.get("headers").and_then(Value::as_object) {
        for (name, value) in map {
            let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| format!("webhook header \"{name}\" is not a valid HTTP header name"))?;
            let header_value =
                reqwest::header::HeaderValue::from_str(value.as_str().unwrap_or("")).map_err(
                    |_| {
                        format!(
                            "webhook header \"{name}\" has a value that is not a valid HTTP header value"
                        )
                    },
                )?;
            headers.insert(header_name, header_value);
        }
    }
    Ok(headers)
}

/// Execute one `call_webhook` action: allowlist + SSRF checks,
/// then the HTTP call with up to 3 attempts and exponential
/// backoff. Only 2xx counts as success.
pub(crate) async fn execute_webhook_action(
    workflow: &Workflow,
    config: &Value,
    payload: &Value,
) -> StepOutcome {
    let rendered = render_config_value(config, payload, workflow);
    let url_raw = rendered
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let method = rendered
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("POST")
        .to_string();
    let parsed = match url::Url::parse(&url_raw) {
        Ok(url) => url,
        Err(_) => {
            return step_failed(
                format!("Invalid webhook URL: {url_raw}"),
                json!({ "method": method }),
            )
        }
    };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return step_failed(
            "webhook URL must use http or https".to_string(),
            json!({ "method": method }),
        );
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return step_failed(
            "webhook URL must not contain credentials".to_string(),
            json!({ "method": method }),
        );
    }
    let Some(host) = parsed.host_str().map(str::to_string) else {
        return step_failed(
            "webhook URL has no host".to_string(),
            json!({ "method": method }),
        );
    };
    if !host_allowed(&host, &workflow.webhook_allowed_hosts) {
        return step_failed(
            format!("webhook host \"{host}\" is not on this workflow's allowlist"),
            json!({ "method": method, "host": host }),
        );
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    if let Err(error) = resolve_host_is_public(&host, port).await {
        return step_failed(error.message, json!({ "method": method, "host": host }));
    }
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(WEBHOOK_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Unified-Workflows/1.0")
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return step_failed(
                format!("cannot build webhook client: {error}"),
                json!({ "method": method, "host": host }),
            )
        }
    };
    let headers = match build_webhook_headers(&rendered) {
        Ok(headers) => headers,
        Err(error) => {
            return step_failed(error, json!({ "method": method, "host": host }));
        }
    };
    let body_text: Option<String> = match rendered.get("body") {
        None | Some(Value::Null) => None,
        Some(Value::String(raw)) => Some(raw.clone()),
        Some(other) => Some(serde_json::to_string(other).unwrap_or_default()),
    };
    let mut last_error = String::from("webhook call failed");
    let mut attempts = 0u32;
    while attempts < MAX_WEBHOOK_ATTEMPTS {
        attempts += 1;
        let request = client
            .request(
                reqwest::Method::from_bytes(method.as_bytes())
                    .unwrap_or(reqwest::Method::POST),
                url_raw.clone(),
            )
            .headers(headers.clone());
        let request = match &body_text {
            Some(body) if method != "GET" => request.body(body.clone()),
            _ => request,
        };
        match request.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                if response.status().is_success() {
                    return StepOutcome {
                        status: "success",
                        attempts,
                        error: None,
                        detail: json!({ "method": method, "host": host, "status": status }),
                    };
                }
                last_error = format!("webhook answered with HTTP {status}");
            }
            Err(error) => {
                last_error = format!("webhook call failed: {error}");
            }
        }
        if attempts < MAX_WEBHOOK_ATTEMPTS {
            let backoff = WEBHOOK_BACKOFF_SECS[(attempts - 1) as usize];
            tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
        }
    }
    StepOutcome {
        status: "failed",
        attempts,
        error: Some(last_error),
        detail: json!({ "method": method, "host": host }),
    }
}

// Execution engine

/// Execute one action. Emit-based actions run synchronously; the
/// webhook action needs the network and runs async. `depth` is the
/// workflow-call depth of the run this action belongs to.
pub(crate) async fn execute_action(
    state: &Arc<WorkflowsState>,
    workflow: &Workflow,
    action: &WorkflowAction,
    payload: &Value,
    depth: u8,
) -> StepOutcome {
    let config = render_config_value(&action.config, payload, workflow);
    match action.kind.as_str() {
        "send_notification" => {
            let kind = config
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("workflow.notification");
            let title = config
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("");
            let body = config
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or("");
            let link = config
                .get("link")
                .and_then(Value::as_str)
                .map(str::to_string);
            let priority = config
                .get("priority")
                .and_then(Value::as_str)
                .unwrap_or("normal");
            let mut event_payload = json!({
                "kind": kind,
                "title": title,
                "body": body,
                "module": "workflows",
                "category": "workflows",
                "priority": priority,
                "link": link,
                "workflow_id": workflow.id.to_string(),
            });
            if let Some(user_id) = config.get("user_id").and_then(Value::as_str) {
                event_payload["user_id"] = Value::String(user_id.to_string());
            }
            if let Some(user_ids) = config.get("user_ids") {
                event_payload["user_ids"] = user_ids.clone();
            }
            if let Some(channels) = config.get("channels") {
                event_payload["channels"] = channels.clone();
            }
            // The notifications module delivers; this only asks.
            emit_action(state, NOTIFICATION_REQUESTED, event_payload, depth)
        }
        "send_email" => {
            let mut event_payload = json!({
                "to": config.get("to"),
                "subject": config.get("subject"),
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            for field in ["body_text", "body_html", "template_id", "variables"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, MAIL_SEND_REQUESTED, event_payload, depth)
        }
        "create_task" => {
            let mut event_payload = json!({
                "title": config.get("title"),
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            for field in ["description", "assignee_id", "priority", "due_at"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, TASK_CREATE_REQUESTED, event_payload, depth)
        }
        "create_ticket" => {
            let mut event_payload = json!({
                "title": config.get("title"),
                "body": config.get("body"),
                "user_id": workflow.owner_id,
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            if let Some(company_id) = &workflow.company_id {
                event_payload["company_id"] = Value::String(company_id.clone());
            }
            for field in ["priority", "category", "product", "department_id"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, TICKET_CREATE_REQUESTED, event_payload, depth)
        }
        "update_record" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            let event = format!("{module}.update_requested");
            emit_action(
                state,
                &event,
                json!({
                    "id": config.get("target_id"),
                    "field": config.get("field"),
                    "value": config.get("value"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
                depth,
            )
        }
        "assign_user" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            let event = format!("{module}.assign_requested");
            emit_action(
                state,
                &event,
                json!({
                    "id": config.get("target_id"),
                    "user_id": config.get("user_id"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
                depth,
            )
        }
        "add_tag" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            let event = format!("{module}.tag_requested");
            emit_action(
                state,
                &event,
                json!({
                    "id": config.get("target_id"),
                    "tag": config.get("tag"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
                depth,
            )
        }
        "run_workflow" => {
            let target_raw = config
                .get("workflow_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let Ok(target_id) = Uuid::parse_str(target_raw) else {
                return step_failed(
                    format!("Invalid run_workflow target id: {target_raw}"),
                    json!({}),
                );
            };
            if target_id == workflow.id {
                return step_failed(
                    "a workflow cannot run itself".to_string(),
                    json!({ "workflow_id": target_id.to_string() }),
                );
            }
            if depth >= MAX_WORKFLOW_CALL_DEPTH {
                return step_failed(
                    format!(
                        "max workflow-call depth ({MAX_WORKFLOW_CALL_DEPTH}) reached; refusing to run workflow {target_id}"
                    ),
                    json!({ "workflow_id": target_id.to_string(), "depth": depth }),
                );
            }
            let Some(child) = state
                .workflows
                .get(&target_id)
                .map(|entry| entry.value().clone())
            else {
                return step_failed(
                    format!("run_workflow target not found: {target_id}"),
                    json!({ "workflow_id": target_id.to_string() }),
                );
            };
            if !child.is_active {
                return step_failed(
                    format!("run_workflow target is disabled: {target_id}"),
                    json!({ "workflow_id": target_id.to_string() }),
                );
            }
            let child_payload = payload.clone();
            match begin_run(
                state,
                &child,
                "event",
                Some("workflows.run_workflow"),
                &child_payload,
                false,
                depth + 1,
            ) {
                RunStart::Dropped(dropped) => step_failed(
                    format!(
                        "run_workflow target hit its rate limit (run {} dropped)",
                        dropped.id
                    ),
                    json!({ "workflow_id": target_id.to_string(), "run_id": dropped.id.to_string() }),
                ),
                RunStart::Started(child_run) => {
                    let finished =
                        finish_run_async(state, child_run, child, child_payload, depth + 1).await;
                    if finished.status == RunStatus::Finished {
                        step_success(
                            json!({ "workflow_id": target_id.to_string(), "run_id": finished.id.to_string() }),
                        )
                    } else {
                        step_failed(
                            format!(
                                "run_workflow target run {} ended as {}: {}",
                                finished.id,
                                finished.status.as_str(),
                                finished.error.as_deref().unwrap_or("no error recorded")
                            ),
                            json!({ "workflow_id": target_id.to_string(), "run_id": finished.id.to_string() }),
                        )
                    }
                }
            }
        }
        "call_webhook" => execute_webhook_action(workflow, &config, payload).await,
        other => step_failed(
            format!("unknown action kind \"{other}\""),
            json!({}),
        ),
    }
}

/// The sync path for runs started from a bus handler without an
/// async runtime: emit-based actions execute; a webhook step is
/// recorded as `deferred` (the caller can retry the run from
/// `POST /{id}/run`, where the async runtime is available).
pub(crate) fn execute_action_sync(
    state: &Arc<WorkflowsState>,
    workflow: &Workflow,
    action: &WorkflowAction,
    payload: &Value,
    depth: u8,
) -> StepOutcome {
    if action.kind == "call_webhook" {
        return StepOutcome {
            status: "deferred",
            attempts: 0,
            error: Some(
                "outbound webhook needs an async runtime; retry the run from POST /api/v1/workflows/{id}/run"
                    .to_string(),
            ),
            detail: json!({ "action": "call_webhook" }),
        };
    }
    if action.kind == "run_workflow" {
        let config = render_config_value(&action.config, payload, workflow);
        let target_raw = config
            .get("workflow_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let Ok(target_id) = Uuid::parse_str(target_raw) else {
            return step_failed(
                format!("Invalid run_workflow target id: {target_raw}"),
                json!({}),
            );
        };
        if target_id == workflow.id {
            return step_failed(
                "a workflow cannot run itself".to_string(),
                json!({ "workflow_id": target_id.to_string() }),
            );
        }
        if depth >= MAX_WORKFLOW_CALL_DEPTH {
            return step_failed(
                format!(
                    "max workflow-call depth ({MAX_WORKFLOW_CALL_DEPTH}) reached; refusing to run workflow {target_id}"
                ),
                json!({ "workflow_id": target_id.to_string(), "depth": depth }),
            );
        }
        let Some(child) = state
            .workflows
            .get(&target_id)
            .map(|entry| entry.value().clone())
        else {
            return step_failed(
                format!("run_workflow target not found: {target_id}"),
                json!({ "workflow_id": target_id.to_string() }),
            );
        };
        if !child.is_active {
            return step_failed(
                format!("run_workflow target is disabled: {target_id}"),
                json!({ "workflow_id": target_id.to_string() }),
            );
        }
        let child_payload = payload.clone();
        return match begin_run(
            state,
            &child,
            "event",
            Some("workflows.run_workflow"),
            &child_payload,
            false,
            depth + 1,
        ) {
            RunStart::Dropped(dropped) => step_failed(
                format!(
                    "run_workflow target hit its rate limit (run {} dropped)",
                    dropped.id
                ),
                json!({ "workflow_id": target_id.to_string(), "run_id": dropped.id.to_string() }),
            ),
            RunStart::Started(child_run) => {
                let finished =
                    finish_run_sync(state, child_run, child, child_payload, depth + 1);
                if finished.status == RunStatus::Finished {
                    step_success(
                        json!({ "workflow_id": target_id.to_string(), "run_id": finished.id.to_string() }),
                    )
                } else {
                    step_failed(
                        format!(
                            "run_workflow target run {} ended as {}: {}",
                            finished.id,
                            finished.status.as_str(),
                            finished.error.as_deref().unwrap_or("no error recorded")
                        ),
                        json!({ "workflow_id": target_id.to_string(), "run_id": finished.id.to_string() }),
                    )
                }
            }
        };
    }
    // Emit-based actions share the async implementation: build a
    // blocking-free future is impossible here, so reuse the same
    // code through a tiny local runtime-free path - the emits are
    // synchronous, so run them directly via a duplicate of the
    // emit branches.
    let config = render_config_value(&action.config, payload, workflow);
    match action.kind.as_str() {
        "send_notification" => {
            let kind = config
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("workflow.notification");
            let title = config
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("");
            let body = config
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or("");
            let mut event_payload = json!({
                "kind": kind,
                "title": title,
                "body": body,
                "module": "workflows",
                "category": "workflows",
                "priority": config.get("priority").and_then(Value::as_str).unwrap_or("normal"),
                "link": config.get("link"),
                "workflow_id": workflow.id.to_string(),
            });
            if let Some(user_id) = config.get("user_id").and_then(Value::as_str) {
                event_payload["user_id"] = Value::String(user_id.to_string());
            }
            if let Some(user_ids) = config.get("user_ids") {
                event_payload["user_ids"] = user_ids.clone();
            }
            if let Some(channels) = config.get("channels") {
                event_payload["channels"] = channels.clone();
            }
            emit_action(state, NOTIFICATION_REQUESTED, event_payload, depth)
        }
        "send_email" => {
            let mut event_payload = json!({
                "to": config.get("to"),
                "subject": config.get("subject"),
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            for field in ["body_text", "body_html", "template_id", "variables"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, MAIL_SEND_REQUESTED, event_payload, depth)
        }
        "create_task" => {
            let mut event_payload = json!({
                "title": config.get("title"),
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            for field in ["description", "assignee_id", "priority", "due_at"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, TASK_CREATE_REQUESTED, event_payload, depth)
        }
        "create_ticket" => {
            let mut event_payload = json!({
                "title": config.get("title"),
                "body": config.get("body"),
                "user_id": workflow.owner_id,
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            if let Some(company_id) = &workflow.company_id {
                event_payload["company_id"] = Value::String(company_id.clone());
            }
            for field in ["priority", "category", "product", "department_id"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, TICKET_CREATE_REQUESTED, event_payload, depth)
        }
        "update_record" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            emit_action(
                state,
                &format!("{module}.update_requested"),
                json!({
                    "id": config.get("target_id"),
                    "field": config.get("field"),
                    "value": config.get("value"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
                depth,
            )
        }
        "assign_user" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            emit_action(
                state,
                &format!("{module}.assign_requested"),
                json!({
                    "id": config.get("target_id"),
                    "user_id": config.get("user_id"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
                depth,
            )
        }
        "add_tag" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            emit_action(
                state,
                &format!("{module}.tag_requested"),
                json!({
                    "id": config.get("target_id"),
                    "tag": config.get("tag"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
                depth,
            )
        }
        other => step_failed(format!("unknown action kind \"{other}\""), json!({})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(field: &str, operator: &str, value: Value) -> ConditionLeaf {
        ConditionLeaf {
            field: field.to_string(),
            operator: operator.to_string(),
            value,
        }
    }

    #[test]
    fn values_equal_compares_numbers_across_types() {
        assert!(values_equal(&json!(1), &json!(1.0)));
        assert!(values_equal(&json!("a"), &json!("a")));
        assert!(!values_equal(&json!(1), &json!(2)));
        assert!(!values_equal(&json!(true), &json!(1)));
    }

    #[test]
    fn value_as_number_parses_strings() {
        assert_eq!(value_as_number(&json!(3)), Some(3.0));
        assert_eq!(value_as_number(&json!("42.5")), Some(42.5));
        assert_eq!(value_as_number(&json!("x")), None);
        assert_eq!(value_as_number(&json!(null)), None);
    }

    #[test]
    fn resolve_field_walks_nested_payload() {
        let payload = json!({"a": {"b": 5}});
        assert_eq!(
            resolve_field(&payload, &["a".to_string(), "b".to_string()]),
            Some(&json!(5))
        );
        assert_eq!(
            resolve_field(&payload, &["a".to_string(), "c".to_string()]),
            None
        );
        assert_eq!(resolve_field(&payload, &[]), Some(&payload));
    }

    #[test]
    fn summarize_value_shapes() {
        let short = summarize_value(&json!("hello"));
        assert_eq!(short["type"], json!("string"));
        assert_eq!(short["value"], json!("hello"));
        let long = summarize_value(&json!("x".repeat(200)));
        assert_eq!(long["value"].as_str().unwrap().len(), 131);
        assert!(long["value"].as_str().unwrap().ends_with("..."));
        assert_eq!(summarize_value(&json!(7))["type"], json!("number"));
        let array = summarize_value(&json!([1, 2]));
        assert_eq!(array["type"], json!("array"));
        assert_eq!(array["length"], json!(2));
        let object = summarize_value(&json!({"b": 1, "a": 2, "password": "x"}));
        assert_eq!(object["type"], json!("object"));
        assert_eq!(object["keys"], json!(["a", "b"]));
    }

    #[test]
    fn eval_leaf_matches_operators() {
        let payload = json!({"total": 10, "name": "hello world"});
        assert!(eval_leaf(&leaf("payload.total", "equals", json!(10)), &payload).matched);
        assert!(!eval_leaf(&leaf("payload.total", "equals", json!(11)), &payload).matched);
        assert!(eval_leaf(&leaf("payload.total", "not_equals", json!(11)), &payload).matched);
        assert!(eval_leaf(&leaf("payload.total", "greater_than", json!(5)), &payload).matched);
        assert!(!eval_leaf(&leaf("payload.total", "less_than", json!(5)), &payload).matched);
        assert!(
            eval_leaf(&leaf("payload.name", "contains", json!("world")), &payload).matched
        );
        assert!(eval_leaf(&leaf("payload.total", "in", json!([1, 2, 10])), &payload).matched);
        let missing = json!({"other": 1});
        assert!(!eval_leaf(&leaf("payload.total", "equals", json!(10)), &missing).matched);
        assert!(!eval_leaf(&leaf("payload.total", "not_equals", json!(10)), &missing).matched);
    }

    #[test]
    fn eval_group_combines_children() {
        let payload = json!({"total": 10});
        let yes = ConditionNode::Leaf(leaf("payload.total", "equals", json!(10)));
        let no = ConditionNode::Leaf(leaf("payload.total", "equals", json!(11)));
        let and_both = ConditionGroup {
            operator: "and".to_string(),
            conditions: vec![yes.clone(), yes.clone()],
        };
        assert!(eval_group(&and_both, &payload).0);
        let and_mixed = ConditionGroup {
            operator: "and".to_string(),
            conditions: vec![yes.clone(), no.clone()],
        };
        assert!(!eval_group(&and_mixed, &payload).0);
        let or_mixed = ConditionGroup {
            operator: "or".to_string(),
            conditions: vec![no.clone(), yes.clone()],
        };
        assert!(eval_group(&or_mixed, &payload).0);
        let or_none = ConditionGroup {
            operator: "or".to_string(),
            conditions: vec![no.clone(), no.clone()],
        };
        assert!(!eval_group(&or_none, &payload).0);
        let unknown = ConditionGroup {
            operator: "xor".to_string(),
            conditions: vec![yes.clone()],
        };
        assert!(!eval_group(&unknown, &payload).0);
    }

    #[test]
    fn is_public_ip_rejects_non_public() {
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("2001:4860:4860::8888".parse().unwrap()));
        assert!(!is_public_ip("10.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("192.168.1.1".parse().unwrap()));
        assert!(!is_public_ip("100.64.0.1".parse().unwrap()));
        assert!(!is_public_ip("192.0.2.1".parse().unwrap()));
        assert!(!is_public_ip("::1".parse().unwrap()));
        // AS112 ranges are not public.
        assert!(!is_public_ip("192.31.196.1".parse().unwrap()));
        assert!(!is_public_ip("192.31.196.53".parse().unwrap()));
        assert!(!is_public_ip("192.175.48.1".parse().unwrap()));
        assert!(is_public_ip("192.31.197.1".parse().unwrap()));
        assert!(is_public_ip("192.175.49.1".parse().unwrap()));
        // Mapped IPv4 is judged by the mapped address.
        assert!(is_public_ip("::ffff:8.8.8.8".parse().unwrap()));
        assert!(!is_public_ip("::ffff:10.0.0.1".parse().unwrap()));
        // 6to4 (2002::/16) is judged by the embedded IPv4 address.
        assert!(is_public_ip("2002:0808:0808::".parse().unwrap()));
        assert!(!is_public_ip("2002:0a00:0001::".parse().unwrap()));
        assert!(!is_public_ip("2002:c0a8:0101::".parse().unwrap()));
        assert!(!is_public_ip("2002:7f00:0001::".parse().unwrap()));
    }

    #[test]
    fn build_webhook_headers_accepts_valid_headers() {
        let rendered = json!({
            "headers": {
                "Authorization": "Bearer secret-token",
                "X-Custom": "value",
            }
        });
        let headers = build_webhook_headers(&rendered).unwrap();
        assert_eq!(
            headers.get("authorization").unwrap().to_str().unwrap(),
            "Bearer secret-token"
        );
        assert_eq!(headers.get("x-custom").unwrap().to_str().unwrap(), "value");
        assert!(build_webhook_headers(&json!({})).is_ok());
        assert!(build_webhook_headers(&json!({ "headers": {} })).is_ok());
    }

    #[test]
    fn build_webhook_headers_fails_instead_of_dropping() {
        // A header value with a control character must fail the
        // step, never send the request without the header.
        let bad_value = json!({ "headers": { "Authorization": "Bearer a\nb" } });
        let error = build_webhook_headers(&bad_value).unwrap_err();
        assert!(error.contains("Authorization"));
        assert!(!error.contains("Bearer a"));
        // A header name that is not a valid token fails too.
        let bad_name = json!({ "headers": { "Bad Name": "value" } });
        assert!(build_webhook_headers(&bad_name).is_err());
    }
}
