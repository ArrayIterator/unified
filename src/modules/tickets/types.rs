// Tickets module - ticket types and shared state.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;

// Types

pub(crate) fn default_comment_kind() -> String {
    "comment".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketComment {
    pub id: String,
    pub author_id: String,
    pub body: String,
    /// `comment`, `customer_reply`, `agent_reply`, or `system`.
    #[serde(default = "default_comment_kind")]
    pub kind: String,
    pub created_at: DateTime<Utc>,
}

/// A staff-only note. Never shown to the customer: list/detail
/// responses use the public view (see `Ticket::public_view`) for
/// non-staff viewers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketNote {
    pub id: String,
    pub author_id: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

/// A link from a ticket to an order, a license, or a server. The target
/// is stored as id + type only; `verified` records whether the target
/// was confirmed to exist through a hook at link time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketLink {
    /// `order`, `license`, or `server`.
    pub target_type: String,
    /// Order id, license key, or server id.
    pub target_id: String,
    pub verified: bool,
    pub linked_at: DateTime<Utc>,
}

/// A reference to a file already uploaded through the Storage module.
/// The bytes live in storage; only the target/key reference is kept
/// here so there is no second upload system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketAttachment {
    pub id: String,
    /// Storage target name, as in `/api/v1/storage/targets/{name}`.
    pub target: String,
    /// Object key supplied at upload time.
    pub key: String,
    /// Key the provider stored the object under.
    pub stored_key: String,
    pub filename: String,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    pub uploaded_by: String,
    pub uploaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    pub title: String,
    pub body: String,
    pub user_id: String,
    pub company_id: Option<String>,
    /// `low`, `normal`, `high`, `urgent`.
    pub priority: String,
    /// `open`, `in_progress`, `waiting`, `resolved`, `closed`.
    pub status: String,
    pub category: String,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub department_id: Option<String>,
    pub assigned_to: Option<String>,
    pub comments: Vec<TicketComment>,
    #[serde(default)]
    pub internal_notes: Vec<TicketNote>,
    #[serde(default)]
    pub links: Vec<TicketLink>,
    #[serde(default)]
    pub attachments: Vec<TicketAttachment>,
    #[serde(default)]
    pub sla_first_response_due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub sla_resolution_due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub first_response_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub sla_first_response_breached: bool,
    #[serde(default)]
    pub sla_resolution_breached: bool,
    #[serde(default)]
    pub escalation_level: u32,
    #[serde(default)]
    pub escalated_at: Option<DateTime<Utc>>,
    /// When this ticket was merged into another one, the target id.
    /// Merged tickets are always `closed`.
    #[serde(default)]
    pub merged_into: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Ticket {
    /// The view a customer (or any non-staff viewer) may see:
    /// identical except internal notes are removed.
    pub(crate) fn public_view(&self) -> Ticket {
        let mut view = self.clone();
        view.internal_notes = Vec::new();
        view
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTicket {
    pub title: String,
    pub body: String,
    pub user_id: String,
    pub company_id: Option<String>,
    pub priority: Option<String>,
    pub category: Option<String>,
    /// Product the ticket is about; used by department routing rules.
    pub product: Option<String>,
    /// Manual department assignment. Wins over routing rules.
    pub department_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTicket {
    pub title: Option<String>,
    pub body: Option<String>,
    pub priority: Option<String>,
    pub category: Option<String>,
    /// Status change; validated against the allowed transitions.
    /// Aliases are accepted: `in-progress` and
    /// `waiting-for-customer` / `waiting_for_customer`.
    pub status: Option<String>,
    /// Move the ticket to another department; recomputes the SLA
    /// timers from the new department's policy.
    pub department_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddComment {
    pub author_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddReply {
    pub author_id: String,
    pub body: String,
    /// `customer` or `agent`. Defaults to `customer` when the author
    /// is the ticket owner, otherwise `agent`.
    pub author_kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddNote {
    pub author_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddAttachment {
    /// Storage target name (see the Storage module).
    pub target: String,
    /// Object key, as uploaded through the Storage module.
    pub key: String,
    /// Key the provider stored the object under; defaults to `key`.
    pub stored_key: Option<String>,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub uploaded_by: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddLink {
    /// `order`, `license`, or `server`.
    pub target_type: String,
    /// Order id, license key, or server id.
    pub target_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoveLinkQuery {
    pub target_type: String,
    pub target_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MergeTicket {
    pub into_ticket_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EscalateTicket {
    pub to_department_id: Option<String>,
    pub assign_to: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssignTicket {
    pub agent_id: String,
}

/// Identity supplied by the fronting API layer for department
/// scoping. Kept for compatibility: the viewer parameters are a
/// display hint only and are ignored. Identity always comes from the
/// authenticated caller (`X-User-Id` / `X-Role` headers), so a
/// caller cannot impersonate another user or role by passing them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TicketViewQuery {
    pub viewer_id: Option<String>,
    pub viewer_role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CannedResponse {
    pub id: String,
    pub title: String,
    pub body: String,
    pub category: String,
    #[serde(default)]
    pub department_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCannedResponse {
    pub title: String,
    pub body: String,
    pub category: Option<String>,
    pub department_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCannedResponse {
    pub title: Option<String>,
    pub body: Option<String>,
    pub category: Option<String>,
    pub department_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyCannedResponse {
    pub canned_response_id: String,
    pub author_id: String,
}

/// One escalation rule. At most one rule applies per ticket per SLA
/// scan (first match in `sort_order`, then creation order).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationRule {
    pub id: String,
    pub name: String,
    /// `None` means the rule applies to every department.
    #[serde(default)]
    pub department_id: Option<String>,
    /// `sla_breach` or `no_response`.
    pub trigger: String,
    /// Evaluation order; lowest first, ties broken by creation time.
    #[serde(default)]
    pub sort_order: u32,
    /// Minutes of silence before a `no_response` rule fires.
    #[serde(default)]
    pub threshold_minutes: Option<u32>,
    /// Only tickets with this priority match; `None` matches all.
    #[serde(default)]
    pub from_priority: Option<String>,
    #[serde(default)]
    pub to_priority: Option<String>,
    #[serde(default)]
    pub assign_to: Option<String>,
    #[serde(default)]
    pub escalate_to_department_id: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateEscalationRule {
    pub name: String,
    pub department_id: Option<String>,
    pub trigger: String,
    pub sort_order: Option<u32>,
    pub threshold_minutes: Option<u32>,
    pub from_priority: Option<String>,
    pub to_priority: Option<String>,
    pub assign_to: Option<String>,
    pub escalate_to_department_id: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateEscalationRule {
    pub name: Option<String>,
    pub department_id: Option<String>,
    pub trigger: Option<String>,
    pub sort_order: Option<u32>,
    pub threshold_minutes: Option<u32>,
    pub from_priority: Option<String>,
    pub to_priority: Option<String>,
    pub assign_to: Option<String>,
    pub escalate_to_department_id: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SlaScanReport {
    pub checked: u64,
    pub first_response_breaches: u64,
    pub resolution_breaches: u64,
    pub escalated: u64,
}

// State

pub struct TicketsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) tickets: DashMap<String, Ticket>,
    pub(crate) canned_responses: DashMap<String, CannedResponse>,
    pub(crate) escalation_rules: DashMap<String, EscalationRule>,
}

impl TicketsState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            tickets: DashMap::new(),
            canned_responses: DashMap::new(),
            escalation_rules: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Send a ticket notification through the Notifications module.
    /// The existing `notification.requested` event is the only channel
    /// the notifications module listens to for ad-hoc messages.
    pub(crate) fn notify(&self, user_id: &str, title: String, body: String) {
        if user_id.trim().is_empty() {
            return;
        }
        self.emit(
            "notification.requested",
            json!({
                "user_id": user_id.trim(),
                "title": title,
                "body": body,
                "kind": "ticket",
            }),
        );
    }

    /// Notify every agent of a department (answers come from the
    /// Departments module through `department.agents_requested`).
    pub(crate) fn notify_department_agents(&self, department_id: &str, title: String, body: String) {
        for agent_id in self.agents_for_department(department_id) {
            self.notify(&agent_id, title.clone(), body.clone());
        }
    }

    /// Ask the Departments module which department should receive a
    /// ticket (`ticket.route_requested`). Returns `None` when no rule
    /// matches and no default department exists, or when the
    /// Departments module is not registered.
    pub(crate) fn route_ticket(
        &self,
        title: &str,
        body: &str,
        category: &str,
        product: Option<&str>,
    ) -> Option<String> {
        let context = self.bus().emit(
            "ticket.route_requested",
            json!({
                "title": title,
                "body": body,
                "category": category,
                "product": product,
            }),
        );
        for result in &context.results {
            if let Some(department_id) = result.get("department_id").and_then(|v| v.as_str()) {
                if !department_id.is_empty() {
                    return Some(department_id.to_string());
                }
            }
        }
        None
    }

    /// Ask the Departments module for a department's SLA policy
    /// (`department.sla_requested`): (first-response minutes,
    /// resolution minutes). `None` when the department is unknown or
    /// the Departments module is not registered.
    pub(crate) fn sla_for_department(&self, department_id: &str) -> Option<(u32, u32)> {
        let context = self.bus().emit(
            "department.sla_requested",
            json!({ "department_id": department_id }),
        );
        for result in &context.results {
            let first = result
                .get("first_response_minutes")
                .and_then(|v| v.as_u64());
            let resolution = result.get("resolution_minutes").and_then(|v| v.as_u64());
            if let (Some(first), Some(resolution)) = (first, resolution) {
                return Some((first as u32, resolution as u32));
            }
        }
        None
    }

    /// (first-response due, resolution due) for a ticket, from the
    /// department policy when one answers, otherwise from the
    /// priority-based defaults.
    pub(crate) fn sla_due_dates(
        &self,
        department_id: Option<&str>,
        priority: &str,
        base: DateTime<Utc>,
    ) -> (DateTime<Utc>, DateTime<Utc>) {
        let (first_minutes, resolution_minutes) = match department_id {
            Some(department_id) => self
                .sla_for_department(department_id)
                .unwrap_or_else(|| default_sla_minutes(priority)),
            None => default_sla_minutes(priority),
        };
        (
            base + chrono::Duration::minutes(first_minutes as i64),
            base + chrono::Duration::minutes(resolution_minutes as i64),
        )
    }

    /// Set a new ticket's SLA due dates from its creation time.
    pub(crate) fn apply_sla(&self, ticket: &mut Ticket) {
        let (first_due, resolution_due) = self.sla_due_dates(
            ticket.department_id.as_deref(),
            &ticket.priority,
            ticket.created_at,
        );
        ticket.sla_first_response_due_at = Some(first_due);
        ticket.sla_resolution_due_at = Some(resolution_due);
    }

    /// Restart the SLA clock when a ticket moves departments: due
    /// dates are recomputed from now and breach flags reset, so the
    /// new department's targets are tracked exactly once.
    pub(crate) fn reapply_sla(&self, ticket: &mut Ticket) {
        let (first_due, resolution_due) = self.sla_due_dates(
            ticket.department_id.as_deref(),
            &ticket.priority,
            now(),
        );
        ticket.sla_first_response_due_at = Some(first_due);
        ticket.sla_resolution_due_at = Some(resolution_due);
        ticket.sla_first_response_breached = false;
        ticket.sla_resolution_breached = false;
    }

    /// Ask the Roles module whether `role` holds `permission`
    /// (`role.check`). Fails closed: no answer means not allowed.
    pub(crate) fn check_capability(&self, role: &str, permission: &str) -> bool {
        let context = self.bus().emit(
            "role.check",
            json!({ "role": role, "permission": permission, "allowed": false }),
        );
        context
            .get("allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Ask the Departments module which departments a user agents
    /// for (`department.membership_check`). `None` when the module
    /// does not answer.
    pub(crate) fn department_ids_for_user(&self, user_id: &str) -> Option<Vec<String>> {
        let context = self.bus().emit(
            "department.membership_check",
            json!({ "user_id": user_id }),
        );
        for result in &context.results {
            if let Some(ids) = result.get("department_ids").and_then(|v| v.as_array()) {
                return Some(
                    ids.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                );
            }
        }
        None
    }

    /// Ask the Departments module for a department's agent user ids
    /// (`department.agents_requested`). Empty when it does not answer.
    pub(crate) fn agents_for_department(&self, department_id: &str) -> Vec<String> {
        let context = self.bus().emit(
            "department.agents_requested",
            json!({ "department_id": department_id }),
        );
        for result in &context.results {
            if let Some(ids) = result.get("agent_ids").and_then(|v| v.as_array()) {
                return ids
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
            }
        }
        Vec::new()
    }

    /// Validate a license link through the existing
    /// `license.validate_requested` hook. No instance id is sent on
    /// purpose: the licensing hook registers an activation seat when
    /// one is supplied, and linking a ticket must never consume a
    /// seat. `false` when no validator is registered.
    pub(crate) fn validate_license_link(&self, license_key: &str) -> bool {
        if !self.bus().has("license.validate_requested") {
            return false;
        }
        let context = self.bus().emit(
            "license.validate_requested",
            json!({ "license_key": license_key }),
        );
        context
            .results
            .iter()
            .any(|result| result.get("valid").and_then(|v| v.as_bool()) == Some(true))
    }

    pub(crate) fn create_ticket(&self, input: CreateTicket) -> UnifiedResult<Ticket> {
        let title = input.title.trim().to_string();
        let body = input.body.trim().to_string();
        let user_id = input.user_id.trim().to_string();
        if title.is_empty() {
            return Err(UnifiedError::bad_request("Ticket title must not be empty"));
        }
        if body.is_empty() {
            return Err(UnifiedError::bad_request("Ticket body must not be empty"));
        }
        if user_id.is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let priority = input.priority.unwrap_or_else(|| "normal".to_string());
        if !valid_priority(&priority) {
            return Err(UnifiedError::bad_request(
                "Priority must be one of: low, normal, high, urgent",
            ));
        }
        let category = input
            .category
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "general".to_string());
        let product = input
            .product
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        // Manual assignment wins; otherwise ask the routing rules.
        let department_id = match input.department_id {
            Some(department_id) if !department_id.trim().is_empty() => {
                Some(department_id.trim().to_string())
            }
            _ => self.route_ticket(&title, &body, &category, product.as_deref()),
        };
        let mut ticket = Ticket {
            id: new_id(),
            title,
            body,
            user_id,
            company_id: input.company_id,
            priority,
            status: "open".to_string(),
            category,
            product,
            department_id,
            assigned_to: None,
            comments: Vec::new(),
            internal_notes: Vec::new(),
            links: Vec::new(),
            attachments: Vec::new(),
            sla_first_response_due_at: None,
            sla_resolution_due_at: None,
            first_response_at: None,
            resolved_at: None,
            sla_first_response_breached: false,
            sla_resolution_breached: false,
            escalation_level: 0,
            escalated_at: None,
            merged_into: None,
            created_at: now(),
            updated_at: now(),
        };
        self.apply_sla(&mut ticket);
        self.tickets.insert(ticket.id.clone(), ticket.clone());
        Ok(ticket)
    }

    /// Apply the first matching enabled escalation rule for a trigger.
    /// Rules are ordered by `sort_order`, then creation time. Returns
    /// true when a rule fired.
    pub(crate) fn apply_escalation_rules(&self, ticket: &mut Ticket, trigger: &str) -> bool {
        let mut rules: Vec<EscalationRule> = self
            .escalation_rules
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        rules.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at))
        });
        let current = now();
        for rule in rules {
            if !rule.enabled || rule.trigger != trigger {
                continue;
            }
            if let Some(department_id) = &rule.department_id {
                if ticket.department_id.as_deref() != Some(department_id.as_str()) {
                    continue;
                }
            }
            if let Some(from_priority) = &rule.from_priority {
                if &ticket.priority != from_priority {
                    continue;
                }
            }
            if trigger == "no_response" {
                let Some(threshold) = rule.threshold_minutes else {
                    continue;
                };
                if current
                    .signed_duration_since(ticket.updated_at)
                    .num_minutes()
                    < threshold as i64
                {
                    continue;
                }
            }
            if let Some(to_priority) = &rule.to_priority {
                if valid_priority(to_priority) {
                    ticket.priority = to_priority.clone();
                }
            }
            if let Some(assign_to) = &rule.assign_to {
                if !assign_to.trim().is_empty() {
                    ticket.assigned_to = Some(assign_to.trim().to_string());
                }
            }
            if let Some(department_id) = &rule.escalate_to_department_id {
                if !department_id.trim().is_empty() {
                    ticket.department_id = Some(department_id.trim().to_string());
                    self.reapply_sla(ticket);
                }
            }
            ticket.escalation_level += 1;
            ticket.escalated_at = Some(current);
            ticket.updated_at = current;
            return true;
        }
        false
    }

    /// Scan every open ticket for SLA breaches and silent tickets.
    /// Breach flags are set exactly once per department assignment,
    /// so a scan never re-alerts for the same breach; escalation
    /// bumps `updated_at`, so a `no_response` rule can only fire
    /// again after another full threshold of silence. When the
    /// first-response and resolution targets breach at once, one
    /// `ticket.sla.breached` event is emitted per breached kind, so
    /// no breach is hidden behind the other; escalation still runs
    /// at most once per ticket per scan.
    pub(crate) fn run_sla_scan(&self) -> SlaScanReport {
        let current = now();
        let mut report = SlaScanReport::default();
        let mut escalated: HashSet<String> = HashSet::new();

        // First pass: flag breaches. Flags are mutated under the map
        // guard; notifications and rule evaluation happen afterwards
        // so no hook runs while a ticket is locked.
        let mut breaches: Vec<(String, String, Option<String>, Option<String>, String, String)> =
            Vec::new();
        let ids: Vec<String> = self.tickets.iter().map(|entry| entry.key().clone()).collect();
        for id in &ids {
            report.checked += 1;
            let Some(mut ticket) = self.tickets.get_mut(id) else {
                continue;
            };
            if ticket.status == "resolved" || ticket.status == "closed" {
                continue;
            }
            let mut breached_kinds: Vec<&str> = Vec::new();
            if ticket.first_response_at.is_none() {
                if let Some(due) = ticket.sla_first_response_due_at {
                    if current > due && !ticket.sla_first_response_breached {
                        ticket.sla_first_response_breached = true;
                        breached_kinds.push("first_response");
                        report.first_response_breaches += 1;
                    }
                }
            }
            if let Some(due) = ticket.sla_resolution_due_at {
                if current > due && !ticket.sla_resolution_breached {
                    ticket.sla_resolution_breached = true;
                    breached_kinds.push("resolution");
                    report.resolution_breaches += 1;
                }
            }
            for kind in breached_kinds {
                breaches.push((
                    ticket.id.clone(),
                    kind.to_string(),
                    ticket.assigned_to.clone(),
                    ticket.department_id.clone(),
                    ticket.user_id.clone(),
                    ticket.title.clone(),
                ));
            }
        }

        // Second pass: alert and escalate for each new breach.
        for (ticket_id, kind, assigned_to, department_id, _user_id, title) in breaches {
            let ticket_value = self
                .tickets
                .get(&ticket_id)
                .map(|entry| serde_json::to_value(entry.value()).unwrap_or(Value::Null))
                .unwrap_or(Value::Null);
            self.emit(
                "ticket.sla.breached",
                json!({
                    "ticket_id": &ticket_id,
                    "kind": &kind,
                    "title": &title,
                    "assigned_to": &assigned_to,
                    "department_id": &department_id,
                    "ticket": ticket_value,
                }),
            );
            // Alert the assignee; when nobody owns the ticket yet,
            // alert the whole department instead.
            let alert_title = format!("SLA breached ({kind}): {title}");
            let alert_body = format!("Ticket {ticket_id} breached its {kind} SLA target.");
            match &assigned_to {
                Some(agent_id) => self.notify(agent_id, alert_title, alert_body),
                None => {
                    if let Some(department_id) = &department_id {
                        self.notify_department_agents(
                            department_id,
                            alert_title,
                            alert_body,
                        );
                    }
                }
            }
            if let Some(mut ticket) = self.tickets.get_mut(&ticket_id) {
                // Escalation runs at most once per ticket per scan,
                // even when several kinds breached at once.
                if escalated.contains(&ticket_id) {
                    continue;
                }
                if self.apply_escalation_rules(&mut ticket, "sla_breach") {
                    let ticket_value =
                        serde_json::to_value(&*ticket).unwrap_or(Value::Null);
                    let escalated_to = ticket.assigned_to.clone();
                    let escalated_department = ticket.department_id.clone();
                    drop(ticket);
                    escalated.insert(ticket_id.clone());
                    report.escalated += 1;
                    self.emit(
                        "ticket.escalated",
                        json!({
                            "ticket_id": &ticket_id,
                            "trigger": "sla_breach",
                            "ticket": ticket_value,
                        }),
                    );
                    let title = format!("Ticket escalated: {title}");
                    let body = format!("Ticket {ticket_id} was escalated after an SLA breach.");
                    match &escalated_to {
                        Some(agent_id) => self.notify(agent_id, title, body),
                        None => {
                            if let Some(department_id) = &escalated_department {
                                self.notify_department_agents(department_id, title, body);
                            }
                        }
                    }
                }
            }
        }

        // Third pass: silent tickets. Escalation bumps `updated_at`,
        // which re-arms the silence threshold.
        for id in &ids {
            if escalated.contains(id) {
                continue;
            }
            let Some(mut ticket) = self.tickets.get_mut(id) else {
                continue;
            };
            if ticket.status == "resolved" || ticket.status == "closed" {
                continue;
            }
            if self.apply_escalation_rules(&mut ticket, "no_response") {
                let ticket_id = ticket.id.clone();
                let title = ticket.title.clone();
                let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
                let escalated_to = ticket.assigned_to.clone();
                let escalated_department = ticket.department_id.clone();
                drop(ticket);
                report.escalated += 1;
                self.emit(
                    "ticket.escalated",
                    json!({
                        "ticket_id": &ticket_id,
                        "trigger": "no_response",
                        "ticket": ticket_value,
                    }),
                );
                let alert_title = format!("Ticket escalated: {title}");
                let alert_body =
                    format!("Ticket {ticket_id} was escalated after no response.");
                match &escalated_to {
                    Some(agent_id) => self.notify(agent_id, alert_title, alert_body),
                    None => {
                        if let Some(department_id) = &escalated_department {
                            self.notify_department_agents(department_id, alert_title, alert_body);
                        }
                    }
                }
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn breached_ticket() -> Ticket {
        let past = now() - chrono::Duration::hours(2);
        Ticket {
            id: new_id(),
            title: "SLA breach".to_string(),
            body: "Both timers expired".to_string(),
            user_id: "user-1".to_string(),
            company_id: None,
            priority: "normal".to_string(),
            status: "open".to_string(),
            category: "general".to_string(),
            product: None,
            department_id: None,
            assigned_to: None,
            comments: Vec::new(),
            internal_notes: Vec::new(),
            links: Vec::new(),
            attachments: Vec::new(),
            sla_first_response_due_at: Some(past),
            sla_resolution_due_at: Some(past),
            first_response_at: None,
            resolved_at: None,
            sla_first_response_breached: false,
            sla_resolution_breached: false,
            escalation_level: 0,
            escalated_at: None,
            merged_into: None,
            created_at: past,
            updated_at: past,
        }
    }

    #[test]
    fn sla_scan_emits_one_breach_event_per_kind() {
        let state = TicketsState::new();
        let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let capture = captured.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            capture
                .lock()
                .expect("capture lock poisoned")
                .push(ctx.data.clone());
        });
        state
            .bus()
            .register("ticket.sla.breached", "tickets-test", handler);
        let ticket = breached_ticket();
        state.tickets.insert(ticket.id.clone(), ticket);
        let report = state.run_sla_scan();
        assert_eq!(report.checked, 1);
        assert_eq!(report.first_response_breaches, 1);
        assert_eq!(report.resolution_breaches, 1);
        assert_eq!(report.escalated, 0);
        let events = captured.lock().expect("capture lock poisoned");
        assert_eq!(events.len(), 2, "one breach event per breached kind");
        let mut kinds: Vec<&str> = events
            .iter()
            .map(|event| event.get("kind").and_then(|v| v.as_str()).unwrap_or(""))
            .collect();
        kinds.sort_unstable();
        assert_eq!(kinds, vec!["first_response", "resolution"]);
    }
}
