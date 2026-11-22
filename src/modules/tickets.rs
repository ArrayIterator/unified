// Support Tickets module.
//
// Tickets with comments, assignment, validated status transitions, and
// knowledgebase suggestions. Tickets can also be created through the
// `ticket.create_requested` hook (Chat / AI Support). Interconnection
// happens only through the hook bus.
//
// Extended behaviour (all additive; every original route keeps working):
//   - Departments: a ticket carries `department_id`, is routed to a
//     department by the Departments module through the
//     `ticket.route_requested` hook, and list/detail responses can be
//     filtered by department. Agent scoping in the list handler uses
//     the `department.membership_check` hook plus the `role.check`
//     hook (`tickets:view_all` capability).
//   - SLA timers: first-response and resolution due dates are computed
//     at creation from the department SLA policy
//     (`department.sla_requested`) or from priority-based defaults.
//     `POST /api/v1/tickets/sla/check` (and the
//     `ticket.sla.check_requested` hook) scans for breaches, emits
//     `ticket.sla.breached`, notifies the assignee or department
//     agents through `notification.requested`, and applies escalation
//     rules.
//   - Replies vs internal notes: customer/agent replies live in the
//     public thread (`comments`); staff-only notes live in
//     `internal_notes` and are stripped from the public view returned
//     to customers.
//   - Attachments reference objects already uploaded through the
//     Storage module (`POST /api/v1/storage/targets/{name}/objects`);
//     this module stores only the target/key reference, never bytes.
//   - Canned responses, escalation rules, ticket merging, and links
//     to orders, licenses, and servers (stored as id + type; a license
//     link is validated through the existing
//     `license.validate_requested` hook, sent without an instance id
//     so linking never consumes an activation seat; no lookup hook
//     exists for orders or servers yet, so those links are stored
//     unverified).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

fn default_comment_kind() -> String {
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
    fn public_view(&self) -> Ticket {
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
/// scoping. This codebase has no auth extractor on handlers, so the
/// caller passes the already-authenticated viewer; without a viewer
/// the list/detail behave exactly as before (unscoped, full view).
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

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct TicketsState {
    hooks: RwLock<Arc<HookBus>>,
    tickets: DashMap<String, Ticket>,
    canned_responses: DashMap<String, CannedResponse>,
    escalation_rules: DashMap<String, EscalationRule>,
}

impl TicketsState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            tickets: DashMap::new(),
            canned_responses: DashMap::new(),
            escalation_rules: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Send a ticket notification through the Notifications module.
    /// The existing `notification.requested` event is the only channel
    /// the notifications module listens to for ad-hoc messages.
    fn notify(&self, user_id: &str, title: String, body: String) {
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
    fn notify_department_agents(&self, department_id: &str, title: String, body: String) {
        for agent_id in self.agents_for_department(department_id) {
            self.notify(&agent_id, title.clone(), body.clone());
        }
    }

    /// Ask the Departments module which department should receive a
    /// ticket (`ticket.route_requested`). Returns `None` when no rule
    /// matches and no default department exists, or when the
    /// Departments module is not registered.
    fn route_ticket(
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
    fn sla_for_department(&self, department_id: &str) -> Option<(u32, u32)> {
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
    fn sla_due_dates(
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
    fn apply_sla(&self, ticket: &mut Ticket) {
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
    fn reapply_sla(&self, ticket: &mut Ticket) {
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
    fn check_capability(&self, role: &str, permission: &str) -> bool {
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
    fn department_ids_for_user(&self, user_id: &str) -> Option<Vec<String>> {
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
    fn agents_for_department(&self, department_id: &str) -> Vec<String> {
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
    fn validate_license_link(&self, license_key: &str) -> bool {
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

    fn create_ticket(&self, input: CreateTicket) -> UnifiedResult<Ticket> {
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
    fn apply_escalation_rules(&self, ticket: &mut Ticket, trigger: &str) -> bool {
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
    /// again after another full threshold of silence.
    fn run_sla_scan(&self) -> SlaScanReport {
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
            let mut breached_kind: Option<&str> = None;
            if ticket.first_response_at.is_none() {
                if let Some(due) = ticket.sla_first_response_due_at {
                    if current > due && !ticket.sla_first_response_breached {
                        ticket.sla_first_response_breached = true;
                        breached_kind = Some("first_response");
                        report.first_response_breaches += 1;
                    }
                }
            }
            if let Some(due) = ticket.sla_resolution_due_at {
                if current > due && !ticket.sla_resolution_breached {
                    ticket.sla_resolution_breached = true;
                    if breached_kind.is_none() {
                        breached_kind = Some("resolution");
                    }
                    report.resolution_breaches += 1;
                }
            }
            if let Some(kind) = breached_kind {
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn valid_priority(priority: &str) -> bool {
    matches!(priority, "low" | "normal" | "high" | "urgent")
}

/// Default SLA targets in minutes (first response, resolution) when
/// no department policy answers.
fn default_sla_minutes(priority: &str) -> (u32, u32) {
    match priority {
        "urgent" => (60, 240),
        "high" => (240, 720),
        "low" => (1440, 4320),
        _ => (480, 1440),
    }
}

/// Canonical status values are `open`, `in_progress`, `waiting`,
/// `resolved`, `closed`. Human spellings are accepted as aliases so
/// API clients can use `in-progress` and `waiting-for-customer`.
fn normalize_status(input: &str) -> String {
    match input {
        "in-progress" => "in_progress".to_string(),
        "waiting-for-customer" | "waiting_for_customer" => "waiting".to_string(),
        other => other.to_string(),
    }
}

fn valid_status(status: &str) -> bool {
    matches!(
        status,
        "open" | "in_progress" | "waiting" | "resolved" | "closed"
    )
}

/// Allowed status transitions. `closed -> open` is a reopen.
fn allowed_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("open", "in_progress")
            | ("open", "closed")
            | ("in_progress", "waiting")
            | ("in_progress", "resolved")
            | ("in_progress", "closed")
            | ("waiting", "in_progress")
            | ("waiting", "resolved")
            | ("waiting", "closed")
            | ("resolved", "closed")
            | ("resolved", "in_progress")
            | ("closed", "open")
    )
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Resolve the requester's scope from the viewer query parameters.
/// `None` means unscoped (no viewer supplied, the historical
/// behaviour). `Some` carries (viewer id, may view everything,
/// departments the viewer agents for).
fn viewer_scope(
    state: &TicketsState,
    params: &PaginationParams,
) -> Option<(String, bool, Vec<String>)> {
    let viewer_id = params.filters.get("viewer_id")?.clone();
    if viewer_id.trim().is_empty() {
        return None;
    }
    let viewer_id = viewer_id.trim().to_string();
    let view_all = match params.filters.get("viewer_role") {
        Some(role) => state.check_capability(role, "tickets:view_all"),
        None => false,
    };
    let departments = if view_all {
        Vec::new()
    } else {
        state
            .department_ids_for_user(&viewer_id)
            .unwrap_or_default()
    };
    Some((viewer_id, view_all, departments))
}

fn ticket_visible_to(ticket: &Ticket, scope: &(String, bool, Vec<String>)) -> bool {
    let (viewer_id, view_all, departments) = scope;
    *view_all
        || ticket.user_id == *viewer_id
        || ticket
            .department_id
            .as_ref()
            .is_some_and(|department_id| departments.iter().any(|id| id == department_id))
}

fn ticket_staff_view(ticket: &Ticket, scope: &(String, bool, Vec<String>)) -> bool {
    let (_viewer_id, view_all, departments) = scope;
    *view_all
        || ticket
            .department_id
            .as_ref()
            .is_some_and(|department_id| departments.iter().any(|id| id == department_id))
}

async fn list_tickets(
    state: web::Data<Arc<TicketsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let scope = viewer_scope(&state, &params);
    let sort = params.allowed_sort(&["created_at", "priority", "updated_at"], "created_at");
    let mut items: Vec<Ticket> = state
        .tickets
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|ticket| {
            matches_search(&format!("{} {} {}", ticket.title, ticket.body, ticket.category), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| ticket.status == *status)
                && params
                    .filters
                    .get("priority")
                    .is_none_or(|priority| ticket.priority == *priority)
                && params
                    .filters
                    .get("category")
                    .is_none_or(|category| ticket.category == *category)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| ticket.user_id == *user_id)
                && params.filters.get("company_id").is_none_or(|company_id| {
                    ticket.company_id.as_deref() == Some(company_id.as_str())
                })
                && params.filters.get("assigned_to").is_none_or(|agent| {
                    ticket.assigned_to.as_deref() == Some(agent.as_str())
                })
                && params.filters.get("department_id").is_none_or(|department| {
                    ticket.department_id.as_deref() == Some(department.as_str())
                })
                && params.filters.get("product").is_none_or(|product| {
                    ticket.product.as_deref() == Some(product.as_str())
                })
                && params
                    .filters
                    .get("sla_breached")
                    .is_none_or(|flag| {
                        let wanted = flag == "true";
                        (ticket.sla_first_response_breached || ticket.sla_resolution_breached)
                            == wanted
                    })
                && scope
                    .as_ref()
                    .is_none_or(|scope| ticket_visible_to(ticket, scope))
        })
        .map(|ticket| match &scope {
            // A scoped viewer sees the public view of tickets outside
            // their own departments; staff see the full record.
            Some(scope) if !ticket_staff_view(&ticket, scope) => ticket.public_view(),
            _ => ticket,
        })
        .collect();
    match sort.as_str() {
        "priority" => {
            let rank = |priority: &str| match priority {
                "urgent" => 3,
                "high" => 2,
                "low" => 0,
                _ => 1,
            };
            items.sort_by_key(|ticket| rank(&ticket.priority));
        }
        "updated_at" => items.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_ticket(
    state: web::Data<Arc<TicketsState>>,
    body: web::Json<CreateTicket>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let ticket = state.create_ticket(body.into_inner())?;
        state.emit(
            "ticket.created",
            serde_json::to_value(&ticket).unwrap_or(Value::Null),
        );
        state.emit(
            "notification.requested",
            json!({
                "user_id": &ticket.user_id,
                "title": format!("Ticket created: {}", ticket.title),
                "body": &ticket.body,
                "kind": "ticket",
            }),
        );
        Ok(ticket)
    })();
    respond_created(result)
}

async fn get_ticket(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    query: web::Query<TicketViewQuery>,
) -> HttpResponse {
    let view = query.into_inner();
    let result: UnifiedResult<Ticket> = (|| {
        let ticket = state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        match (&view.viewer_id, &view.viewer_role) {
            (Some(viewer_id), viewer_role) if !viewer_id.trim().is_empty() => {
                let viewer_id = viewer_id.trim().to_string();
                let view_all = match viewer_role {
                    Some(role) => state.check_capability(role, "tickets:view_all"),
                    None => false,
                };
                let departments = if view_all {
                    Vec::new()
                } else {
                    state
                        .department_ids_for_user(&viewer_id)
                        .unwrap_or_default()
                };
                let scope = (viewer_id, view_all, departments);
                // A viewer who may neither see the ticket as staff nor
                // owns it gets a 404, so ticket existence does not leak.
                if !ticket_visible_to(&ticket, &scope) {
                    return Err(UnifiedError::not_found("Ticket not found"));
                }
                if ticket_staff_view(&ticket, &scope) {
                    Ok(ticket)
                } else {
                    Ok(ticket.public_view())
                }
            }
            _ => Ok(ticket),
        }
    })();
    json::respond(result, false)
}

async fn update_ticket(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateTicket>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        // The new department's SLA is resolved before locking the
        // ticket so no hook runs while the ticket is locked.
        let new_department_sla = match &input.department_id {
            Some(department_id) if !department_id.trim().is_empty() => {
                Some(state.sla_for_department(department_id.trim()))
            }
            Some(_) => {
                return Err(UnifiedError::bad_request("department_id must not be empty"));
            }
            None => None,
        };
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Ticket title must not be empty"));
            }
            ticket.title = title.trim().to_string();
        }
        if let Some(body_text) = input.body {
            if body_text.trim().is_empty() {
                return Err(UnifiedError::bad_request("Ticket body must not be empty"));
            }
            ticket.body = body_text.trim().to_string();
        }
        if let Some(priority) = input.priority {
            if !valid_priority(&priority) {
                return Err(UnifiedError::bad_request(
                    "Priority must be one of: low, normal, high, urgent",
                ));
            }
            ticket.priority = priority;
        }
        if let Some(category) = input.category {
            ticket.category = category;
        }
        if let Some(department_id) = input.department_id {
            let department_id = department_id.trim().to_string();
            ticket.department_id = Some(department_id);
            // Restart the SLA clock under the new department.
            let (first_minutes, resolution_minutes) = new_department_sla
                .flatten()
                .unwrap_or_else(|| default_sla_minutes(&ticket.priority));
            let base = now();
            ticket.sla_first_response_due_at =
                Some(base + chrono::Duration::minutes(first_minutes as i64));
            ticket.sla_resolution_due_at =
                Some(base + chrono::Duration::minutes(resolution_minutes as i64));
            ticket.sla_first_response_breached = false;
            ticket.sla_resolution_breached = false;
        }
        if let Some(status) = input.status {
            let status = normalize_status(&status);
            if !valid_status(&status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown ticket status: {status}"
                )));
            }
            if !allowed_transition(&ticket.status, &status) {
                return Err(UnifiedError::unprocessable(format!(
                    "Cannot move ticket from {} to {}",
                    ticket.status, status
                )));
            }
            if status == "resolved" && ticket.status != "resolved" {
                ticket.resolved_at = Some(now());
            }
            if status == "open" {
                // A reopen starts a fresh resolution clock.
                ticket.resolved_at = None;
            }
            ticket.status = status;
        }
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let updated = ticket.clone();
        drop(ticket);
        state.emit("ticket.updated", ticket_value);
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_ticket(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .tickets
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))
    })();
    if result.is_ok() {
        state.emit(
            "ticket.deleted",
            json!({ "ticket_id": path.as_str() }),
        );
    }
    json::respond(result, false)
}

async fn add_comment(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<AddComment>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if input.author_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("author_id must not be empty"));
        }
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Comment body must not be empty"));
        }
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        ticket.comments.push(TicketComment {
            id: new_id(),
            author_id: input.author_id.trim().to_string(),
            body: input.body.trim().to_string(),
            kind: default_comment_kind(),
            created_at: now(),
        });
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        drop(ticket);
        state.emit("ticket.commented", ticket_value);
        Ok(state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("ticket still present"))
    })();
    respond_created(result)
}

/// A reply in the public thread. An agent reply records the first
/// response and moves an open ticket to in progress; a customer reply
/// moves a waiting ticket back to in progress.
async fn add_reply(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<AddReply>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if input.author_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("author_id must not be empty"));
        }
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Reply body must not be empty"));
        }
        let author_id = input.author_id.trim().to_string();
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        let author_kind = match input.author_kind.as_deref() {
            Some("customer") => "customer",
            Some("agent") => "agent",
            Some(other) => {
                return Err(UnifiedError::bad_request(format!(
                    "author_kind must be customer or agent, got {other}"
                )));
            }
            None => {
                if author_id == ticket.user_id {
                    "customer"
                } else {
                    "agent"
                }
            }
        };
        ticket.comments.push(TicketComment {
            id: new_id(),
            author_id: author_id.clone(),
            body: input.body.trim().to_string(),
            kind: if author_kind == "customer" {
                "customer_reply".to_string()
            } else {
                "agent_reply".to_string()
            },
            created_at: now(),
        });
        if author_kind == "agent" {
            if ticket.first_response_at.is_none() {
                ticket.first_response_at = Some(now());
            }
            if ticket.status == "open" {
                ticket.status = "in_progress".to_string();
            }
        } else if ticket.status == "waiting" {
            ticket.status = "in_progress".to_string();
        }
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let notify_user = ticket.user_id.clone();
        let notify_agent = ticket.assigned_to.clone();
        let title = ticket.title.clone();
        let ticket_id = ticket.id.clone();
        drop(ticket);
        state.emit(
            "ticket.replied",
            json!({ "ticket_id": &ticket_id, "author_kind": author_kind, "ticket": ticket_value }),
        );
        if author_kind == "agent" {
            state.notify(
                &notify_user,
                format!("Reply on your ticket: {title}"),
                format!("An agent replied to ticket {ticket_id}."),
            );
        } else if let Some(agent_id) = notify_agent {
            state.notify(
                &agent_id,
                format!("Customer replied: {title}"),
                format!("The customer replied to ticket {ticket_id}."),
            );
        }
        Ok(state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("ticket still present"))
    })();
    respond_created(result)
}

async fn list_notes(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<TicketNote>> = (|| {
        let ticket = state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        let mut notes = ticket.internal_notes.clone();
        notes.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            notes.reverse();
        }
        Ok(paginate(notes, &params))
    })();
    json::respond(result, false)
}

/// Staff-only internal note. Notes are never part of the customer
/// view and are only reachable through this staff route.
async fn add_note(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<AddNote>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if input.author_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("author_id must not be empty"));
        }
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Note body must not be empty"));
        }
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        ticket.internal_notes.push(TicketNote {
            id: new_id(),
            author_id: input.author_id.trim().to_string(),
            body: input.body.trim().to_string(),
            created_at: now(),
        });
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let updated = ticket.clone();
        drop(ticket);
        state.emit("ticket.note_added", ticket_value);
        Ok(updated)
    })();
    respond_created(result)
}

async fn list_attachments(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<TicketAttachment>> = (|| {
        let ticket = state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        let mut attachments = ticket.attachments.clone();
        attachments.sort_by(|a, b| a.uploaded_at.cmp(&b.uploaded_at));
        if params.normalized().order == "desc" {
            attachments.reverse();
        }
        Ok(paginate(attachments, &params))
    })();
    json::respond(result, false)
}

/// Attach a file that was already uploaded through the Storage
/// module. Only the storage reference is stored here.
async fn add_attachment(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<AddAttachment>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if input.target.trim().is_empty() {
            return Err(UnifiedError::bad_request("target must not be empty"));
        }
        if input.key.trim().is_empty() {
            return Err(UnifiedError::bad_request("key must not be empty"));
        }
        if input.uploaded_by.trim().is_empty() {
            return Err(UnifiedError::bad_request("uploaded_by must not be empty"));
        }
        let key = input.key.trim().to_string();
        let filename = input.filename.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| {
            key.rsplit('/').next().unwrap_or(&key).to_string()
        });
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        ticket.attachments.push(TicketAttachment {
            id: new_id(),
            target: input.target.trim().to_string(),
            stored_key: input
                .stored_key
                .filter(|stored| !stored.trim().is_empty())
                .unwrap_or_else(|| key.clone()),
            key,
            filename,
            content_type: input.content_type,
            size_bytes: input.size_bytes,
            uploaded_by: input.uploaded_by.trim().to_string(),
            uploaded_at: now(),
        });
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let updated = ticket.clone();
        drop(ticket);
        state.emit("ticket.attachment_added", ticket_value);
        Ok(updated)
    })();
    respond_created(result)
}

async fn add_link(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<AddLink>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if !matches!(input.target_type.as_str(), "order" | "license" | "server") {
            return Err(UnifiedError::bad_request(
                "target_type must be one of: order, license, server",
            ));
        }
        if input.target_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("target_id must not be empty"));
        }
        let target_id = input.target_id.trim().to_string();
        // License links are the only ones with a lookup hook today.
        // Orders and servers have no lookup hook, so those links are
        // stored unverified until one exists.
        let verified = if input.target_type == "license" {
            state.validate_license_link(&target_id)
        } else {
            false
        };
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        if ticket
            .links
            .iter()
            .any(|link| link.target_type == input.target_type && link.target_id == target_id)
        {
            return Err(UnifiedError::conflict(format!(
                "Ticket is already linked to {} {target_id}",
                input.target_type
            )));
        }
        ticket.links.push(TicketLink {
            target_type: input.target_type.clone(),
            target_id,
            verified,
            linked_at: now(),
        });
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let updated = ticket.clone();
        drop(ticket);
        state.emit("ticket.linked", ticket_value);
        Ok(updated)
    })();
    respond_created(result)
}

async fn remove_link(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    query: web::Query<RemoveLinkQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let result: UnifiedResult<Ticket> = (|| {
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        let before = ticket.links.len();
        ticket.links.retain(|link| {
            !(link.target_type == query.target_type && link.target_id == query.target_id)
        });
        if ticket.links.len() == before {
            return Err(UnifiedError::not_found(format!(
                "Ticket is not linked to {} {}",
                query.target_type, query.target_id
            )));
        }
        ticket.updated_at = now();
        let updated = ticket.clone();
        drop(ticket);
        Ok(updated)
    })();
    json::respond(result, false)
}

/// Merge this ticket into another one: the source closes and points
/// at the target, and its thread, attachments, and links move over.
async fn merge_ticket(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<MergeTicket>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let into_ticket_id = body.into_inner().into_ticket_id;
        let source_id = path.as_str().to_string();
        if into_ticket_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("into_ticket_id must not be empty"));
        }
        if into_ticket_id == source_id {
            return Err(UnifiedError::bad_request(
                "A ticket cannot be merged into itself",
            ));
        }
        // The target must exist before any ticket is locked: checking
        // it while holding the source guard could deadlock on the
        // map's per-shard lock, and a failed merge must never leave
        // the source pointing nowhere.
        if !state.tickets.contains_key(into_ticket_id.as_str()) {
            return Err(UnifiedError::not_found("Target ticket not found"));
        }
        let mut source = state
            .tickets
            .get_mut(&source_id)
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        if source.merged_into.is_some() {
            return Err(UnifiedError::conflict(
                "Ticket was already merged into another ticket",
            ));
        }
        let source_title = source.title.clone();
        let source_comments = source.comments.clone();
        let source_attachments = source.attachments.clone();
        let source_links = source.links.clone();
        source.status = "closed".to_string();
        source.merged_into = Some(into_ticket_id.clone());
        source.updated_at = now();
        drop(source);
        let mut target = state
            .tickets
            .get_mut(&into_ticket_id)
            .ok_or_else(|| UnifiedError::not_found("Target ticket not found"))?;
        target.comments.extend(source_comments);
        target.attachments.extend(source_attachments);
        for link in source_links {
            if !target.links.iter().any(|existing| {
                existing.target_type == link.target_type && existing.target_id == link.target_id
            }) {
                target.links.push(link);
            }
        }
        target.comments.push(TicketComment {
            id: new_id(),
            author_id: "system".to_string(),
            body: format!("Ticket {source_id} (\"{source_title}\") was merged into this ticket."),
            kind: "system".to_string(),
            created_at: now(),
        });
        target.updated_at = now();
        let target_value = serde_json::to_value(&*target).unwrap_or(Value::Null);
        let target_id = target.id.clone();
        let target_user = target.user_id.clone();
        let target_title = target.title.clone();
        drop(target);
        state.emit(
            "ticket.merged",
            json!({
                "ticket_id": &source_id,
                "into_ticket_id": &into_ticket_id,
                "ticket": target_value,
            }),
        );
        state.notify(
            &target_user,
            format!("Ticket merged: {target_title}"),
            format!("Ticket {source_id} was merged into ticket {target_id}."),
        );
        Ok(state
            .tickets
            .get(&target_id)
            .map(|entry| entry.value().clone())
            .expect("target ticket still present"))
    })();
    json::respond(result, false)
}

async fn escalate_ticket(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<EscalateTicket>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        // Resolve the new department's SLA before locking the ticket.
        let new_department_sla = match &input.to_department_id {
            Some(department_id) if !department_id.trim().is_empty() => {
                Some(state.sla_for_department(department_id.trim()))
            }
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "to_department_id must not be empty",
                ));
            }
            None => None,
        };
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        if ticket.status == "closed" {
            return Err(UnifiedError::unprocessable(
                "A closed ticket cannot be escalated; reopen it first",
            ));
        }
        if let Some(department_id) = &input.to_department_id {
            let department_id = department_id.trim().to_string();
            ticket.department_id = Some(department_id);
            let (first_minutes, resolution_minutes) = new_department_sla
                .flatten()
                .unwrap_or_else(|| default_sla_minutes(&ticket.priority));
            let base = now();
            ticket.sla_first_response_due_at =
                Some(base + chrono::Duration::minutes(first_minutes as i64));
            ticket.sla_resolution_due_at =
                Some(base + chrono::Duration::minutes(resolution_minutes as i64));
            ticket.sla_first_response_breached = false;
            ticket.sla_resolution_breached = false;
        }
        if let Some(assign_to) = &input.assign_to {
            if !assign_to.trim().is_empty() {
                ticket.assigned_to = Some(assign_to.trim().to_string());
            }
        }
        ticket.escalation_level += 1;
        ticket.escalated_at = Some(now());
        ticket.comments.push(TicketComment {
            id: new_id(),
            author_id: "system".to_string(),
            body: match &input.reason {
                Some(reason) if !reason.trim().is_empty() => {
                    format!("Ticket escalated (level {}): {}", ticket.escalation_level, reason.trim())
                }
                _ => format!("Ticket escalated (level {}).", ticket.escalation_level),
            },
            kind: "system".to_string(),
            created_at: now(),
        });
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let notify_agent = ticket.assigned_to.clone();
        let notify_department = ticket.department_id.clone();
        let title = ticket.title.clone();
        let ticket_id = ticket.id.clone();
        let updated = ticket.clone();
        drop(ticket);
        state.emit(
            "ticket.escalated",
            json!({ "ticket_id": &ticket_id, "trigger": "manual", "ticket": ticket_value }),
        );
        let alert_title = format!("Ticket escalated: {title}");
        let alert_body = format!("Ticket {ticket_id} was escalated manually.");
        match &notify_agent {
            Some(agent_id) => state.notify(agent_id, alert_title, alert_body),
            None => {
                if let Some(department_id) = &notify_department {
                    state.notify_department_agents(department_id, alert_title, alert_body);
                }
            }
        }
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn assign_ticket(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<AssignTicket>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let agent_id = body.into_inner().agent_id;
        if agent_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("agent_id must not be empty"));
        }
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        ticket.assigned_to = Some(agent_id.trim().to_string());
        if ticket.status == "open" {
            ticket.status = "in_progress".to_string();
        }
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let notify_agent = ticket.assigned_to.clone();
        let title = ticket.title.clone();
        let ticket_id = ticket.id.clone();
        drop(ticket);
        state.emit("ticket.assigned", ticket_value);
        if let Some(agent_id) = notify_agent {
            state.notify(
                &agent_id,
                format!("Ticket assigned: {title}"),
                format!("Ticket {ticket_id} was assigned to you."),
            );
        }
        Ok(state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("ticket still present"))
    })();
    json::respond(result, false)
}

/// Run the SLA breach scan now. The scheduler (or any module) can
/// trigger the same scan through the `ticket.sla.check_requested`
/// hook instead of calling this endpoint.
async fn sla_check(state: web::Data<Arc<TicketsState>>) -> HttpResponse {
    let report = state.run_sla_scan();
    json::success_response(report, 200)
}

async fn ticket_suggestions(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let ticket = state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        // Ask the knowledgebase (via hook) for articles matching the title.
        let context = state.bus().emit(
            "knowledgebase.search",
            json!({ "query": &ticket.title, "ticket_id": &ticket.id }),
        );
        let mut articles: Vec<Value> = Vec::new();
        for result in context.results {
            match result {
                Value::Array(items) => articles.extend(items),
                Value::Object(map) => {
                    if let Some(Value::Array(items)) = map.get("articles") {
                        articles.extend(items.iter().cloned());
                    } else {
                        articles.push(Value::Object(map));
                    }
                }
                other => articles.push(other),
            }
        }
        Ok(json!({ "ticket_id": &ticket.id, "query": &ticket.title, "articles": articles }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Canned responses
// ---------------------------------------------------------------------------

async fn list_canned_responses(
    state: web::Data<Arc<TicketsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<CannedResponse> = state
        .canned_responses
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|response| {
            matches_search(&format!("{} {} {}", response.title, response.body, response.category), &params)
                && params
                    .filters
                    .get("category")
                    .is_none_or(|category| response.category == *category)
                && params
                    .filters
                    .get("department_id")
                    .is_none_or(|department| {
                        response.department_id.as_deref() == Some(department.as_str())
                    })
        })
        .collect();
    items.sort_by(|a, b| a.title.cmp(&b.title));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_canned_response(
    state: web::Data<Arc<TicketsState>>,
    body: web::Json<CreateCannedResponse>,
) -> HttpResponse {
    let result: UnifiedResult<CannedResponse> = (|| {
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Canned response title must not be empty",
            ));
        }
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Canned response body must not be empty"));
        }
        let response = CannedResponse {
            id: new_id(),
            title: input.title.trim().to_string(),
            body: input.body.trim().to_string(),
            category: input
                .category
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "general".to_string()),
            department_id: input
                .department_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            created_at: now(),
            updated_at: now(),
        };
        state
            .canned_responses
            .insert(response.id.clone(), response.clone());
        Ok(response)
    })();
    respond_created(result)
}

async fn get_canned_response(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<CannedResponse> = state
        .canned_responses
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Canned response not found"));
    json::respond(result, false)
}

async fn update_canned_response(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateCannedResponse>,
) -> HttpResponse {
    let result: UnifiedResult<CannedResponse> = (|| {
        let input = body.into_inner();
        let mut response = state
            .canned_responses
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Canned response not found"))?;
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "Canned response title must not be empty",
                ));
            }
            response.title = title.trim().to_string();
        }
        if let Some(body_text) = input.body {
            if body_text.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "Canned response body must not be empty",
                ));
            }
            response.body = body_text.trim().to_string();
        }
        if let Some(category) = input.category {
            response.category = category;
        }
        if let Some(department_id) = input.department_id {
            response.department_id = Some(department_id);
        }
        response.updated_at = now();
        Ok(response.clone())
    })();
    json::respond(result, false)
}

async fn delete_canned_response(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .canned_responses
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Canned response not found"))
    })();
    json::respond(result, false)
}

/// Insert a canned response as an agent reply on a ticket.
async fn apply_canned_response(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<ApplyCannedResponse>,
) -> HttpResponse {
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        if input.author_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("author_id must not be empty"));
        }
        let canned = state
            .canned_responses
            .get(input.canned_response_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Canned response not found"))?;
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        ticket.comments.push(TicketComment {
            id: new_id(),
            author_id: input.author_id.trim().to_string(),
            body: canned.body.clone(),
            kind: "agent_reply".to_string(),
            created_at: now(),
        });
        if ticket.first_response_at.is_none() {
            ticket.first_response_at = Some(now());
        }
        if ticket.status == "open" {
            ticket.status = "in_progress".to_string();
        }
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let notify_user = ticket.user_id.clone();
        let title = ticket.title.clone();
        let ticket_id = ticket.id.clone();
        drop(ticket);
        state.emit(
            "ticket.replied",
            json!({ "ticket_id": &ticket_id, "author_kind": "agent", "ticket": ticket_value }),
        );
        state.notify(
            &notify_user,
            format!("Reply on your ticket: {title}"),
            format!("An agent replied to ticket {ticket_id}."),
        );
        Ok(state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("ticket still present"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Escalation rules
// ---------------------------------------------------------------------------

fn validate_escalation_trigger(trigger: &str) -> UnifiedResult<()> {
    if !matches!(trigger, "sla_breach" | "no_response") {
        return Err(UnifiedError::bad_request(
            "trigger must be one of: sla_breach, no_response",
        ));
    }
    Ok(())
}

async fn list_escalation_rules(
    state: web::Data<Arc<TicketsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<EscalationRule> = state
        .escalation_rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            matches_search(&format!("{} {}", rule.name, rule.trigger), &params)
                && params.filters.get("trigger").is_none_or(|trigger| {
                    rule.trigger == *trigger
                })
                && params.filters.get("department_id").is_none_or(|department| {
                    rule.department_id.as_deref() == Some(department.as_str())
                })
                && params
                    .filters
                    .get("enabled")
                    .is_none_or(|flag| rule.enabled == (flag == "true"))
        })
        .collect();
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then(a.created_at.cmp(&b.created_at))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_escalation_rule(
    state: web::Data<Arc<TicketsState>>,
    body: web::Json<CreateEscalationRule>,
) -> HttpResponse {
    let result: UnifiedResult<EscalationRule> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Rule name must not be empty"));
        }
        validate_escalation_trigger(&input.trigger)?;
        if input.trigger == "no_response" && input.threshold_minutes.is_none() {
            return Err(UnifiedError::bad_request(
                "threshold_minutes is required for the no_response trigger",
            ));
        }
        if let Some(to_priority) = &input.to_priority {
            if !valid_priority(to_priority) {
                return Err(UnifiedError::bad_request(
                    "to_priority must be one of: low, normal, high, urgent",
                ));
            }
        }
        if let Some(from_priority) = &input.from_priority {
            if !valid_priority(from_priority) {
                return Err(UnifiedError::bad_request(
                    "from_priority must be one of: low, normal, high, urgent",
                ));
            }
        }
        let rule = EscalationRule {
            id: new_id(),
            name: input.name.trim().to_string(),
            department_id: input
                .department_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            trigger: input.trigger,
            sort_order: input.sort_order.unwrap_or(0),
            threshold_minutes: input.threshold_minutes,
            from_priority: input.from_priority,
            to_priority: input.to_priority,
            assign_to: input
                .assign_to
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            escalate_to_department_id: input
                .escalate_to_department_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            enabled: input.enabled.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state
            .escalation_rules
            .insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    respond_created(result)
}

async fn get_escalation_rule(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<EscalationRule> = state
        .escalation_rules
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Escalation rule not found"));
    json::respond(result, false)
}

async fn update_escalation_rule(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateEscalationRule>,
) -> HttpResponse {
    let result: UnifiedResult<EscalationRule> = (|| {
        let input = body.into_inner();
        let mut rule = state
            .escalation_rules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Escalation rule not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Rule name must not be empty"));
            }
            rule.name = name.trim().to_string();
        }
        if let Some(trigger) = input.trigger {
            validate_escalation_trigger(&trigger)?;
            rule.trigger = trigger;
        }
        if let Some(sort_order) = input.sort_order {
            rule.sort_order = sort_order;
        }
        if let Some(threshold) = input.threshold_minutes {
            rule.threshold_minutes = Some(threshold);
        }
        if let Some(from_priority) = input.from_priority {
            if !valid_priority(&from_priority) {
                return Err(UnifiedError::bad_request(
                    "from_priority must be one of: low, normal, high, urgent",
                ));
            }
            rule.from_priority = Some(from_priority);
        }
        if let Some(to_priority) = input.to_priority {
            if !valid_priority(&to_priority) {
                return Err(UnifiedError::bad_request(
                    "to_priority must be one of: low, normal, high, urgent",
                ));
            }
            rule.to_priority = Some(to_priority);
        }
        if let Some(assign_to) = input.assign_to {
            rule.assign_to = Some(assign_to);
        }
        if let Some(department_id) = input.escalate_to_department_id {
            rule.escalate_to_department_id = Some(department_id);
        }
        if let Some(department_id) = input.department_id {
            rule.department_id = Some(department_id);
        }
        if let Some(enabled) = input.enabled {
            rule.enabled = enabled;
        }
        rule.updated_at = now();
        Ok(rule.clone())
    })();
    json::respond(result, false)
}

async fn delete_escalation_rule(
    state: web::Data<Arc<TicketsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .escalation_rules
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Escalation rule not found"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

fn on_ticket_create_requested(state: Arc<TicketsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let input = CreateTicket {
        title: data
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        body: data
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        user_id: data
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        company_id: data
            .get("company_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        priority: data
            .get("priority")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        category: data
            .get("category")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        product: data
            .get("product")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        department_id: data
            .get("department_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    };
    match state.create_ticket(input) {
        Ok(ticket) => {
            let value = serde_json::to_value(&ticket).unwrap_or(Value::Null);
            ctx.add_result(value.clone());
            state.emit("ticket.created", value);
        }
        Err(error) => {
            ctx.add_result(json!({ "error": error.message }));
        }
    }
}

/// Run the SLA scan when another module asks for it through the bus
/// (`ticket.sla.check_requested`).
fn on_ticket_sla_check_requested(state: Arc<TicketsState>, ctx: &mut HookContext) {
    let report = state.run_sla_scan();
    ctx.add_result(serde_json::to_value(&report).unwrap_or(Value::Null));
}

/// Answer the Departments module's delete guard
/// (`department.delete_check`) with how many tickets still belong to
/// the department.
fn on_department_delete_check(state: Arc<TicketsState>, ctx: &mut HookContext) {
    let Some(department_id) = ctx.get("department_id").and_then(|v| v.as_str()) else {
        return;
    };
    let ticket_count = state
        .tickets
        .iter()
        .filter(|entry| entry.value().department_id.as_deref() == Some(department_id))
        .count() as u64;
    ctx.add_result(json!({
        "department_id": department_id,
        "ticket_count": ticket_count,
    }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct TicketsModule {
    state: Arc<TicketsState>,
}

impl TicketsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(TicketsState::new()),
        }
    }
}

impl Default for TicketsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TicketsModule {
    fn id(&self) -> &'static str {
        "tickets"
    }

    fn name(&self) -> &'static str {
        "Support Tickets"
    }

    fn description(&self) -> &'static str {
        "Support tickets with comments, assignment, validated status transitions, knowledgebase suggestions, departments, SLA timers, escalation rules, canned responses, internal notes, attachments, merging, and links to orders, licenses, and servers."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["knowledgebase", "notifications", "departments"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_ticket_create_requested(state.clone(), ctx));
        bus.register("ticket.create_requested", "tickets", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_ticket_sla_check_requested(state.clone(), ctx)
        });
        bus.register("ticket.sla.check_requested", "tickets", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_department_delete_check(state.clone(), ctx)
        });
        bus.register("department.delete_check", "tickets", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/tickets")
                // Literal segments first: they must win over `/{id}`.
                .route("/canned-responses", web::get().to(list_canned_responses))
                .route("/canned-responses", web::post().to(create_canned_response))
                .route(
                    "/canned-responses/{id}",
                    web::get().to(get_canned_response),
                )
                .route(
                    "/canned-responses/{id}",
                    web::put().to(update_canned_response),
                )
                .route(
                    "/canned-responses/{id}",
                    web::delete().to(delete_canned_response),
                )
                .route("/escalations/rules", web::get().to(list_escalation_rules))
                .route("/escalations/rules", web::post().to(create_escalation_rule))
                .route(
                    "/escalations/rules/{id}",
                    web::get().to(get_escalation_rule),
                )
                .route(
                    "/escalations/rules/{id}",
                    web::put().to(update_escalation_rule),
                )
                .route(
                    "/escalations/rules/{id}",
                    web::delete().to(delete_escalation_rule),
                )
                .route("/sla/check", web::post().to(sla_check))
                .route("", web::get().to(list_tickets))
                .route("", web::post().to(create_ticket))
                .route("/{id}", web::get().to(get_ticket))
                .route("/{id}", web::put().to(update_ticket))
                .route("/{id}", web::delete().to(delete_ticket))
                .route("/{id}/comments", web::post().to(add_comment))
                .route("/{id}/replies", web::post().to(add_reply))
                .route("/{id}/notes", web::get().to(list_notes))
                .route("/{id}/notes", web::post().to(add_note))
                .route("/{id}/attachments", web::get().to(list_attachments))
                .route("/{id}/attachments", web::post().to(add_attachment))
                .route("/{id}/links", web::post().to(add_link))
                .route("/{id}/links", web::delete().to(remove_link))
                .route("/{id}/merge", web::post().to(merge_ticket))
                .route("/{id}/escalate", web::post().to(escalate_ticket))
                .route("/{id}/apply-canned", web::post().to(apply_canned_response))
                .route("/{id}/assign", web::post().to(assign_ticket))
                .route("/{id}/suggestions", web::get().to(ticket_suggestions)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/tickets", owner: "tickets", description: "List tickets (paginated; filter by status, priority, category, product, department, assignee, SLA breach; viewer-scoped)." },
            RouteInfo { method: "POST", path: "/api/v1/tickets", owner: "tickets", description: "Create a ticket (routed to a department, SLA timers set)." },
            RouteInfo { method: "GET", path: "/api/v1/tickets/{id}", owner: "tickets", description: "Get one ticket (public view for non-staff viewers)." },
            RouteInfo { method: "PUT", path: "/api/v1/tickets/{id}", owner: "tickets", description: "Update a ticket (validated status transitions, department move recomputes SLA)." },
            RouteInfo { method: "DELETE", path: "/api/v1/tickets/{id}", owner: "tickets", description: "Delete a ticket." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/comments", owner: "tickets", description: "Add a plain comment to a ticket." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/replies", owner: "tickets", description: "Add a customer or agent reply (drives first response and waiting state)." },
            RouteInfo { method: "GET", path: "/api/v1/tickets/{id}/notes", owner: "tickets", description: "List staff-only internal notes." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/notes", owner: "tickets", description: "Add a staff-only internal note." },
            RouteInfo { method: "GET", path: "/api/v1/tickets/{id}/attachments", owner: "tickets", description: "List ticket attachments (storage references)." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/attachments", owner: "tickets", description: "Attach a file already uploaded through the Storage module." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/links", owner: "tickets", description: "Link a ticket to an order, license, or server." },
            RouteInfo { method: "DELETE", path: "/api/v1/tickets/{id}/links", owner: "tickets", description: "Remove a ticket link (?target_type=&target_id=)." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/merge", owner: "tickets", description: "Merge a ticket into another ticket." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/escalate", owner: "tickets", description: "Escalate a ticket manually (level, optional department move and reassignment)." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/apply-canned", owner: "tickets", description: "Insert a canned response as an agent reply." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/{id}/assign", owner: "tickets", description: "Assign a ticket to an agent." },
            RouteInfo { method: "GET", path: "/api/v1/tickets/{id}/suggestions", owner: "tickets", description: "Knowledgebase suggestions for a ticket." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/sla/check", owner: "tickets", description: "Run the SLA breach and escalation scan now." },
            RouteInfo { method: "GET", path: "/api/v1/tickets/canned-responses", owner: "tickets", description: "List canned responses (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/canned-responses", owner: "tickets", description: "Create a canned response." },
            RouteInfo { method: "GET", path: "/api/v1/tickets/canned-responses/{id}", owner: "tickets", description: "Get one canned response." },
            RouteInfo { method: "PUT", path: "/api/v1/tickets/canned-responses/{id}", owner: "tickets", description: "Update a canned response." },
            RouteInfo { method: "DELETE", path: "/api/v1/tickets/canned-responses/{id}", owner: "tickets", description: "Delete a canned response." },
            RouteInfo { method: "GET", path: "/api/v1/tickets/escalations/rules", owner: "tickets", description: "List escalation rules (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/tickets/escalations/rules", owner: "tickets", description: "Create an escalation rule." },
            RouteInfo { method: "GET", path: "/api/v1/tickets/escalations/rules/{id}", owner: "tickets", description: "Get one escalation rule." },
            RouteInfo { method: "PUT", path: "/api/v1/tickets/escalations/rules/{id}", owner: "tickets", description: "Update an escalation rule." },
            RouteInfo { method: "DELETE", path: "/api/v1/tickets/escalations/rules/{id}", owner: "tickets", description: "Delete an escalation rule." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TicketsModule::new()));
}
