// Departments module (CORE).
//
// Departments own support agents, ticket routing rules, per-department
// SLA policies, working hours, and auto-replies. Interconnection
// happens only through the hook bus — no module calls another module
// directly.
//
// What this module answers for others:
//   - `ticket.route_requested` — which department receives a ticket,
//     by category, product, or keyword rule, falling back to the
//     default department.
//   - `department.sla_requested` — a department's first-response and
//     resolution targets in minutes.
//   - `department.membership_check` — which departments a user agents
//     for (used by the tickets list handler for role-based access).
//   - `department.agents_requested` — the agent user ids of a
//     department (used for SLA breach and escalation alerts).
//   - `ticket.created` — sends the department auto-reply to the
//     ticket owner when the ticket arrives outside working hours.
//
// Agents are stored as user ids only. Names are resolved by callers
// through the Users module (`GET /api/v1/users/{id}`); no user lookup
// hook exists, so none is invented here. An agent's role, when one is
// supplied, is validated through the existing `role.check` hook.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Datelike, Timelike, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Working hours for one department. `days` uses 0 = Sunday through
/// 6 = Saturday. `timezone` is a display name; `utc_offset_minutes`
/// is what the hours are evaluated against, because no timezone
/// database is available in this crate set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingHours {
    pub days: Vec<u8>,
    pub start: String,
    pub end: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub utc_offset_minutes: i32,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

impl Default for WorkingHours {
    fn default() -> Self {
        Self {
            days: vec![1, 2, 3, 4, 5],
            start: "09:00".to_string(),
            end: "17:00".to_string(),
            timezone: "UTC".to_string(),
            utc_offset_minutes: 0,
        }
    }
}

/// Auto-reply sent to the ticket owner when a ticket arrives outside
/// working hours. Sent through `notification.requested`, never by
/// email directly from here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoReply {
    pub enabled: bool,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

impl Default for AutoReply {
    fn default() -> Self {
        Self {
            enabled: false,
            subject: String::new(),
            body: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Department {
    pub id: String,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub email: Option<String>,
    /// `active` or `inactive`. Inactive departments never receive
    /// routed tickets.
    pub status: String,
    /// The fallback department when no routing rule matches.
    pub is_default: bool,
    /// First-response target in minutes.
    pub sla_first_response_minutes: u32,
    /// Resolution target in minutes.
    pub sla_resolution_minutes: u32,
    pub working_hours: WorkingHours,
    pub auto_reply: AutoReply,
    /// Agent user ids. Names live in the Users module.
    #[serde(default)]
    pub agents: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateDepartment {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub email: Option<String>,
    pub status: Option<String>,
    pub is_default: Option<bool>,
    pub sla_first_response_minutes: Option<u32>,
    pub sla_resolution_minutes: Option<u32>,
    pub working_hours: Option<WorkingHours>,
    pub auto_reply: Option<AutoReply>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateDepartment {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub email: Option<String>,
    pub status: Option<String>,
    pub is_default: Option<bool>,
    pub sla_first_response_minutes: Option<u32>,
    pub sla_resolution_minutes: Option<u32>,
    pub working_hours: Option<WorkingHours>,
    pub auto_reply: Option<AutoReply>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddAgent {
    pub user_id: String,
    /// Optional role the agent must hold; validated through the
    /// `role.check` hook when supplied.
    pub role_id: Option<String>,
}

/// One routing rule: which department receives a ticket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    pub id: String,
    pub department_id: String,
    /// `category`, `product`, or `keyword`.
    pub match_type: String,
    /// Category slug, product name, or keyword substring.
    pub match_value: String,
    /// Evaluation order; lowest first, ties broken by creation time.
    pub sort_order: u32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRoutingRule {
    pub match_type: String,
    pub match_value: String,
    pub sort_order: Option<u32>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRoutingRule {
    pub match_type: Option<String>,
    pub match_value: Option<String>,
    pub sort_order: Option<u32>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutePreview {
    pub title: Option<String>,
    pub body: Option<String>,
    pub category: Option<String>,
    pub product: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct DepartmentsState {
    hooks: RwLock<Arc<HookBus>>,
    departments: DashMap<String, Department>,
    rules: DashMap<String, RoutingRule>,
}

impl DepartmentsState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            departments: DashMap::new(),
            rules: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Ask the Roles module whether a role exists (`role.check` with
    /// a `role_id` payload). `false` when no handler answers.
    fn role_exists(&self, role_id: &str) -> bool {
        let context = self.bus().emit(
            "role.check",
            json!({ "role_id": role_id, "permission": "*", "allowed": true }),
        );
        context
            .get("allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Ask the Tickets module how many tickets still belong to a
    /// department (`department.delete_check`). `None` when nothing
    /// answers.
    fn ticket_count_for(&self, department_id: &str) -> Option<u64> {
        let context = self.bus().emit(
            "department.delete_check",
            json!({ "department_id": department_id }),
        );
        for result in &context.results {
            if let Some(count) = result.get("ticket_count").and_then(|v| v.as_u64()) {
                return Some(count);
            }
        }
        None
    }

    fn sorted_rules(&self) -> Vec<RoutingRule> {
        let mut rules: Vec<RoutingRule> = self
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        rules.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at))
        });
        rules
    }

    /// The department a ticket routes to, plus the rule that matched
    /// (if any). Rules run first; the default department is the
    /// fallback; inactive departments never receive tickets.
    fn route_for(
        &self,
        title: &str,
        body: &str,
        category: &str,
        product: Option<&str>,
    ) -> (Option<String>, Option<String>) {
        let title_lower = title.to_lowercase();
        let body_lower = body.to_lowercase();
        let category_lower = category.to_lowercase();
        let product_lower = product.map(str::to_lowercase);
        for rule in self.sorted_rules() {
            if !rule.enabled {
                continue;
            }
            let Some(department) = self.departments.get(&rule.department_id) else {
                continue;
            };
            if department.status != "active" {
                continue;
            }
            let matched = match rule.match_type.as_str() {
                "category" => category_lower == rule.match_value.to_lowercase(),
                "product" => product_lower
                    .as_ref()
                    .is_some_and(|product| *product == rule.match_value.to_lowercase()),
                "keyword" => {
                    let needle = rule.match_value.to_lowercase();
                    !needle.is_empty()
                        && (title_lower.contains(&needle) || body_lower.contains(&needle))
                }
                _ => false,
            };
            if matched {
                return (Some(rule.department_id.clone()), Some(rule.id.clone()));
            }
        }
        let default = self
            .departments
            .iter()
            .find(|entry| entry.value().is_default && entry.value().status == "active")
            .map(|entry| entry.key().clone());
        (default, None)
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

/// `My Support Team` -> `my-support-team`.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn valid_email(email: &str) -> bool {
    let mut parts = email.split('@');
    matches!((parts.next(), parts.next(), parts.next()), (Some(local), Some(domain), None)
        if !local.is_empty() && !domain.is_empty() && domain.contains('.'))
}

/// `09:30` -> minutes since midnight. `None` when malformed.
fn parse_hhmm(value: &str) -> Option<u32> {
    let (hour, minute) = value.split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(hour * 60 + minute)
}

fn valid_working_hours(hours: &WorkingHours) -> UnifiedResult<()> {
    if hours.days.is_empty() {
        return Err(UnifiedError::bad_request(
            "working_hours.days must list at least one day",
        ));
    }
    if hours.days.iter().any(|day| *day > 6) {
        return Err(UnifiedError::bad_request(
            "working_hours.days entries must be 0 (Sunday) through 6 (Saturday)",
        ));
    }
    let start = parse_hhmm(&hours.start)
        .ok_or_else(|| UnifiedError::bad_request("working_hours.start must be HH:MM"))?;
    let end = parse_hhmm(&hours.end)
        .ok_or_else(|| UnifiedError::bad_request("working_hours.end must be HH:MM"))?;
    if start >= end {
        return Err(UnifiedError::bad_request(
            "working_hours.start must be before working_hours.end (overnight shifts are not supported)",
        ));
    }
    Ok(())
}

/// True when `moment` falls inside the department's working hours.
fn within_working_hours(hours: &WorkingHours, moment: DateTime<Utc>) -> bool {
    let local = moment + chrono::Duration::minutes(hours.utc_offset_minutes as i64);
    let weekday = local.weekday().num_days_from_sunday() as u8;
    if !hours.days.contains(&weekday) {
        return false;
    }
    let (Some(start), Some(end)) = (parse_hhmm(&hours.start), parse_hhmm(&hours.end)) else {
        return false;
    };
    let minute_of_day = local.hour() * 60 + local.minute();
    minute_of_day >= start && minute_of_day < end
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// Handlers: departments
// ---------------------------------------------------------------------------

async fn list_departments(
    state: web::Data<Arc<DepartmentsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Department> = state
        .departments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|department| {
            matches_search(
                &format!("{} {} {}", department.name, department.slug, department.description),
                &params,
            ) && params
                .filters
                .get("status")
                .is_none_or(|status| department.status == *status)
                && params
                    .filters
                    .get("is_default")
                    .is_none_or(|flag| department.is_default == (flag == "true"))
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_department(
    state: web::Data<Arc<DepartmentsState>>,
    body: web::Json<CreateDepartment>,
) -> HttpResponse {
    let result: UnifiedResult<Department> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Department name must not be empty"));
        }
        let slug = match input.slug {
            Some(slug) if !slug.trim().is_empty() => slug.trim().to_lowercase(),
            _ => slugify(&input.name),
        };
        if !valid_slug(&slug) {
            return Err(UnifiedError::bad_request(
                "slug must be lowercase letters, digits, and hyphens",
            ));
        }
        if state
            .departments
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "A department with slug \"{slug}\" already exists"
            )));
        }
        let email = input
            .email
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(email) = &email {
            if !valid_email(email) {
                return Err(UnifiedError::bad_request("email must be a valid email address"));
            }
        }
        let status = input.status.unwrap_or_else(|| "active".to_string());
        if !matches!(status.as_str(), "active" | "inactive") {
            return Err(UnifiedError::bad_request(
                "status must be one of: active, inactive",
            ));
        }
        let first_response = input.sla_first_response_minutes.unwrap_or(240);
        let resolution = input.sla_resolution_minutes.unwrap_or(1440);
        if first_response == 0 || resolution == 0 {
            return Err(UnifiedError::bad_request(
                "SLA targets must be greater than zero minutes",
            ));
        }
        let working_hours = input.working_hours.unwrap_or_default();
        valid_working_hours(&working_hours)?;
        let is_default = input.is_default.unwrap_or(false);
        if is_default {
            for mut entry in state.departments.iter_mut() {
                entry.value_mut().is_default = false;
            }
        }
        let department = Department {
            id: new_id(),
            name: input.name.trim().to_string(),
            slug,
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            email,
            status,
            is_default,
            sla_first_response_minutes: first_response,
            sla_resolution_minutes: resolution,
            working_hours,
            auto_reply: input.auto_reply.unwrap_or_default(),
            agents: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .departments
            .insert(department.id.clone(), department.clone());
        let value = serde_json::to_value(&department).unwrap_or(Value::Null);
        state.emit("department.created", value);
        Ok(department)
    })();
    respond_created(result)
}

async fn get_department(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Department> = state
        .departments
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Department not found"));
    json::respond(result, false)
}

async fn update_department(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateDepartment>,
) -> HttpResponse {
    let result: UnifiedResult<Department> = (|| {
        let input = body.into_inner();
        if let Some(slug) = &input.slug {
            if !slug.trim().is_empty() && !valid_slug(&slug.trim().to_lowercase()) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and hyphens",
                ));
            }
            if !slug.trim().is_empty()
                && state.departments.iter().any(|entry| {
                    entry.key() != path.as_str() && entry.value().slug == slug.trim().to_lowercase()
                })
            {
                return Err(UnifiedError::conflict(format!(
                    "A department with slug \"{}\" already exists",
                    slug.trim().to_lowercase()
                )));
            }
        }
        if let Some(email) = &input.email {
            if !email.trim().is_empty() && !valid_email(email.trim()) {
                return Err(UnifiedError::bad_request("email must be a valid email address"));
            }
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "inactive") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, inactive",
                ));
            }
        }
        if let Some(hours) = &input.working_hours {
            valid_working_hours(hours)?;
        }
        if let Some(true) = input.is_default {
            for mut entry in state.departments.iter_mut() {
                if entry.key() != path.as_str() {
                    entry.value_mut().is_default = false;
                }
            }
        }
        let mut department = state
            .departments
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Department not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Department name must not be empty"));
            }
            department.name = name.trim().to_string();
        }
        if let Some(slug) = input.slug {
            if !slug.trim().is_empty() {
                department.slug = slug.trim().to_lowercase();
            }
        }
        if let Some(description) = input.description {
            department.description = description;
        }
        if let Some(email) = input.email {
            department.email = if email.trim().is_empty() {
                None
            } else {
                Some(email.trim().to_string())
            };
        }
        if let Some(status) = input.status {
            department.status = status;
        }
        if let Some(is_default) = input.is_default {
            department.is_default = is_default;
        }
        if let Some(minutes) = input.sla_first_response_minutes {
            if minutes == 0 {
                return Err(UnifiedError::bad_request(
                    "sla_first_response_minutes must be greater than zero",
                ));
            }
            department.sla_first_response_minutes = minutes;
        }
        if let Some(minutes) = input.sla_resolution_minutes {
            if minutes == 0 {
                return Err(UnifiedError::bad_request(
                    "sla_resolution_minutes must be greater than zero",
                ));
            }
            department.sla_resolution_minutes = minutes;
        }
        if let Some(hours) = input.working_hours {
            department.working_hours = hours;
        }
        if let Some(auto_reply) = input.auto_reply {
            department.auto_reply = auto_reply;
        }
        department.updated_at = now();
        let value = serde_json::to_value(&*department).unwrap_or(Value::Null);
        let updated = department.clone();
        drop(department);
        state.emit("department.updated", value);
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_department(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let department_id = path.as_str().to_string();
    // The Tickets module answers `department.delete_check`; a
    // department that still owns tickets cannot be deleted.
    if let Some(ticket_count) = state.ticket_count_for(&department_id) {
        if ticket_count > 0 {
            return json::error_response(
                &UnifiedError::conflict(format!(
                    "Department still owns {ticket_count} ticket(s); move or close them first"
                ))
                .with_metadata("ticket_count", json!(ticket_count)),
                false,
            );
        }
    }
    let result: UnifiedResult<Value> = (|| {
        state
            .departments
            .remove(&department_id)
            .map(|_| json!({ "deleted": true, "id": &department_id }))
            .ok_or_else(|| UnifiedError::not_found("Department not found"))
    })();
    if result.is_ok() {
        // Routing rules belong to the department; they go with it.
        state
            .rules
            .retain(|_, rule| rule.department_id != department_id);
        state.emit(
            "department.deleted",
            json!({ "department_id": department_id }),
        );
    }
    json::respond(result, false)
}

async fn get_department_sla(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = state
        .departments
        .get(path.as_str())
        .map(|entry| {
            let department = entry.value();
            json!({
                "department_id": &department.id,
                "first_response_minutes": department.sla_first_response_minutes,
                "resolution_minutes": department.sla_resolution_minutes,
            })
        })
        .ok_or_else(|| UnifiedError::not_found("Department not found"));
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Handlers: agents
// ---------------------------------------------------------------------------

async fn list_agents(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = state
        .departments
        .get(path.as_str())
        .map(|entry| {
            json!({
                "department_id": &entry.value().id,
                "agent_ids": &entry.value().agents,
            })
        })
        .ok_or_else(|| UnifiedError::not_found("Department not found"));
    json::respond(result, false)
}

async fn add_agent(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<String>,
    body: web::Json<AddAgent>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        if input.user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let user_id = input.user_id.trim().to_string();
        if let Some(role_id) = &input.role_id {
            if !role_id.trim().is_empty() && !state.role_exists(role_id.trim()) {
                return Err(UnifiedError::not_found(format!(
                    "Role {} not found",
                    role_id.trim()
                )));
            }
        }
        let mut department = state
            .departments
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Department not found"))?;
        if department.agents.iter().any(|agent| *agent == user_id) {
            return Err(UnifiedError::conflict(format!(
                "User {user_id} is already an agent of this department"
            )));
        }
        department.agents.push(user_id.clone());
        department.updated_at = now();
        let value = json!({
            "department_id": &department.id,
            "agent_ids": &department.agents,
        });
        drop(department);
        state.emit(
            "department.agent_added",
            json!({ "department_id": path.as_str(), "user_id": user_id }),
        );
        Ok(value)
    })();
    json::respond(result, false)
}

async fn remove_agent(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (department_id, user_id) = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let mut department = state
            .departments
            .get_mut(department_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Department not found"))?;
        let before = department.agents.len();
        department.agents.retain(|agent| *agent != user_id);
        if department.agents.len() == before {
            return Err(UnifiedError::not_found(format!(
                "User {user_id} is not an agent of this department"
            )));
        }
        department.updated_at = now();
        let value = json!({
            "department_id": &department.id,
            "agent_ids": &department.agents,
        });
        drop(department);
        state.emit(
            "department.agent_removed",
            json!({ "department_id": department_id, "user_id": user_id }),
        );
        Ok(value)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Handlers: routing rules
// ---------------------------------------------------------------------------

fn valid_match_type(match_type: &str) -> bool {
    matches!(match_type, "category" | "product" | "keyword")
}

async fn list_rules_for_department(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let department_id = path.as_str().to_string();
    if !state.departments.contains_key(department_id.as_str()) {
        return json::error_response(&UnifiedError::not_found("Department not found"), false);
    }
    let mut items: Vec<RoutingRule> = state
        .rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            rule.department_id == department_id
                && params
                    .filters
                    .get("match_type")
                    .is_none_or(|match_type| rule.match_type == *match_type)
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

async fn list_rules(
    state: web::Data<Arc<DepartmentsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<RoutingRule> = state
        .rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            params
                .filters
                .get("department_id")
                .is_none_or(|department| rule.department_id == *department)
                && params
                    .filters
                    .get("match_type")
                    .is_none_or(|match_type| rule.match_type == *match_type)
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

async fn create_rule(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<String>,
    body: web::Json<CreateRoutingRule>,
) -> HttpResponse {
    let result: UnifiedResult<RoutingRule> = (|| {
        let department_id = path.as_str().to_string();
        if !state.departments.contains_key(department_id.as_str()) {
            return Err(UnifiedError::not_found("Department not found"));
        }
        let input = body.into_inner();
        if !valid_match_type(&input.match_type) {
            return Err(UnifiedError::bad_request(
                "match_type must be one of: category, product, keyword",
            ));
        }
        if input.match_value.trim().is_empty() {
            return Err(UnifiedError::bad_request("match_value must not be empty"));
        }
        let rule = RoutingRule {
            id: new_id(),
            department_id,
            match_type: input.match_type,
            match_value: input.match_value.trim().to_string(),
            sort_order: input.sort_order.unwrap_or(0),
            enabled: input.enabled.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.rules.insert(rule.id.clone(), rule.clone());
        let value = serde_json::to_value(&rule).unwrap_or(Value::Null);
        state.emit("department.rule.created", value);
        Ok(rule)
    })();
    respond_created(result)
}

async fn get_rule(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<RoutingRule> = state
        .rules
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Routing rule not found"));
    json::respond(result, false)
}

async fn update_rule(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateRoutingRule>,
) -> HttpResponse {
    let result: UnifiedResult<RoutingRule> = (|| {
        let input = body.into_inner();
        if let Some(match_type) = &input.match_type {
            if !valid_match_type(match_type) {
                return Err(UnifiedError::bad_request(
                    "match_type must be one of: category, product, keyword",
                ));
            }
        }
        let mut rule = state
            .rules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Routing rule not found"))?;
        if let Some(match_type) = input.match_type {
            rule.match_type = match_type;
        }
        if let Some(match_value) = input.match_value {
            if match_value.trim().is_empty() {
                return Err(UnifiedError::bad_request("match_value must not be empty"));
            }
            rule.match_value = match_value.trim().to_string();
        }
        if let Some(sort_order) = input.sort_order {
            rule.sort_order = sort_order;
        }
        if let Some(enabled) = input.enabled {
            rule.enabled = enabled;
        }
        rule.updated_at = now();
        let value = serde_json::to_value(&*rule).unwrap_or(Value::Null);
        let updated = rule.clone();
        drop(rule);
        state.emit("department.rule.updated", value);
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn delete_rule(
    state: web::Data<Arc<DepartmentsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .rules
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Routing rule not found"))
    })();
    if result.is_ok() {
        state.emit(
            "department.rule.deleted",
            json!({ "rule_id": path.as_str() }),
        );
    }
    json::respond(result, false)
}

/// Dry-run routing for a ticket draft: which department would receive
/// it, and through which rule (if any).
async fn route_preview(
    state: web::Data<Arc<DepartmentsState>>,
    body: web::Json<RoutePreview>,
) -> HttpResponse {
    let input = body.into_inner();
    let (department_id, matched_rule_id) = state.route_for(
        input.title.as_deref().unwrap_or(""),
        input.body.as_deref().unwrap_or(""),
        input.category.as_deref().unwrap_or("general"),
        input.product.as_deref(),
    );
    let matched_via = match (&department_id, &matched_rule_id) {
        (Some(_), Some(_)) => "rule",
        (Some(_), None) => "default",
        (None, _) => "none",
    };
    json::success_response(
        json!({
            "department_id": department_id,
            "matched_rule_id": matched_rule_id,
            "matched_via": matched_via,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Answer `ticket.route_requested` with the routed department, when
/// one matches. No result means "no route" and the ticket stays
/// unassigned to a department.
fn on_ticket_route_requested(state: Arc<DepartmentsState>, ctx: &mut HookContext) {
    let title = ctx
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let body = ctx
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let category = ctx
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("general")
        .to_string();
    let product = ctx
        .get("product")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let (department_id, _rule_id) = state.route_for(&title, &body, &category, product.as_deref());
    if let Some(department_id) = department_id {
        ctx.add_result(json!({ "department_id": department_id }));
    }
}

/// Answer `department.sla_requested` with the department's SLA
/// targets in minutes.
fn on_department_sla_requested(state: Arc<DepartmentsState>, ctx: &mut HookContext) {
    let Some(department_id) = ctx.get("department_id").and_then(|v| v.as_str()) else {
        return;
    };
    if let Some(department) = state.departments.get(department_id) {
        ctx.add_result(json!({
            "department_id": &department.id,
            "first_response_minutes": department.sla_first_response_minutes,
            "resolution_minutes": department.sla_resolution_minutes,
        }));
    }
}

/// Answer `department.membership_check` with the departments a user
/// agents for. Only active departments count.
fn on_department_membership_check(state: Arc<DepartmentsState>, ctx: &mut HookContext) {
    let Some(user_id) = ctx.get("user_id").and_then(|v| v.as_str()) else {
        return;
    };
    let department_ids: Vec<String> = state
        .departments
        .iter()
        .filter(|entry| {
            entry.value().status == "active"
                && entry.value().agents.iter().any(|agent| agent == user_id)
        })
        .map(|entry| entry.key().clone())
        .collect();
    ctx.add_result(json!({
        "user_id": user_id,
        "department_ids": department_ids,
    }));
}

/// Answer `department.agents_requested` with a department's agent
/// user ids.
fn on_department_agents_requested(state: Arc<DepartmentsState>, ctx: &mut HookContext) {
    let Some(department_id) = ctx.get("department_id").and_then(|v| v.as_str()) else {
        return;
    };
    if let Some(department) = state.departments.get(department_id) {
        let agent_ids: Vec<String> = department.agents.clone();
        ctx.add_result(json!({
            "department_id": &department.id,
            "agent_ids": agent_ids,
        }));
    }
}

/// On `ticket.created`, send the department auto-reply to the ticket
/// owner when the ticket arrived outside the department's working
/// hours. The reply goes through `notification.requested`, the event
/// the Notifications module already listens to.
fn on_ticket_created(state: Arc<DepartmentsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(department_id) = data.get("department_id").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(user_id) = data.get("user_id").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(department) = state.departments.get(department_id) else {
        return;
    };
    if !department.auto_reply.enabled {
        return;
    }
    if within_working_hours(&department.working_hours, now()) {
        return;
    }
    let subject = if department.auto_reply.subject.trim().is_empty() {
        "We received your ticket".to_string()
    } else {
        department.auto_reply.subject.clone()
    };
    let body = department.auto_reply.body.clone();
    let user_id = user_id.to_string();
    drop(department);
    state.emit(
        "notification.requested",
        json!({
            "user_id": user_id,
            "title": subject,
            "body": body,
            "kind": "ticket",
        }),
    );
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct DepartmentsModule {
    state: Arc<DepartmentsState>,
}

impl DepartmentsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(DepartmentsState::new()),
        }
    }
}

impl Default for DepartmentsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for DepartmentsModule {
    fn id(&self) -> &'static str {
        "departments"
    }

    fn name(&self) -> &'static str {
        "Departments"
    }

    fn description(&self) -> &'static str {
        "Core support departments: agents, ticket routing rules, per-department SLA policies, working hours, and auto-replies."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["roles", "users", "notifications"]
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
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_ticket_route_requested(state.clone(), ctx)
        });
        bus.register("ticket.route_requested", "departments", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_department_sla_requested(state.clone(), ctx)
        });
        bus.register("department.sla_requested", "departments", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_department_membership_check(state.clone(), ctx)
        });
        bus.register("department.membership_check", "departments", handler);
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_department_agents_requested(state.clone(), ctx)
        });
        bus.register("department.agents_requested", "departments", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_ticket_created(state.clone(), ctx));
        bus.register("ticket.created", "departments", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/departments")
                // Literal segments first: they must win over `/{id}`.
                .route("/rules", web::get().to(list_rules))
                .route("/route-preview", web::post().to(route_preview))
                .route("", web::get().to(list_departments))
                .route("", web::post().to(create_department))
                .route("/{id}", web::get().to(get_department))
                .route("/{id}", web::put().to(update_department))
                .route("/{id}", web::delete().to(delete_department))
                .route("/{id}/sla", web::get().to(get_department_sla))
                .route("/{id}/agents", web::get().to(list_agents))
                .route("/{id}/agents", web::post().to(add_agent))
                .route("/{id}/agents/{user_id}", web::delete().to(remove_agent))
                .route("/{id}/rules", web::get().to(list_rules_for_department))
                .route("/{id}/rules", web::post().to(create_rule))
                .route("/rules/{rule_id}", web::get().to(get_rule))
                .route("/rules/{rule_id}", web::put().to(update_rule))
                .route("/rules/{rule_id}", web::delete().to(delete_rule)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/departments", owner: "departments", description: "List departments (paginated; filter by status, default flag)." },
            RouteInfo { method: "POST", path: "/api/v1/departments", owner: "departments", description: "Create a department." },
            RouteInfo { method: "GET", path: "/api/v1/departments/{id}", owner: "departments", description: "Get one department." },
            RouteInfo { method: "PUT", path: "/api/v1/departments/{id}", owner: "departments", description: "Update a department." },
            RouteInfo { method: "DELETE", path: "/api/v1/departments/{id}", owner: "departments", description: "Delete a department (refused while it still owns tickets)." },
            RouteInfo { method: "GET", path: "/api/v1/departments/{id}/sla", owner: "departments", description: "Get a department's SLA policy (first-response and resolution minutes)." },
            RouteInfo { method: "GET", path: "/api/v1/departments/{id}/agents", owner: "departments", description: "List a department's agent user ids." },
            RouteInfo { method: "POST", path: "/api/v1/departments/{id}/agents", owner: "departments", description: "Add an agent to a department (optional role validation)." },
            RouteInfo { method: "DELETE", path: "/api/v1/departments/{id}/agents/{user_id}", owner: "departments", description: "Remove an agent from a department." },
            RouteInfo { method: "GET", path: "/api/v1/departments/{id}/rules", owner: "departments", description: "List a department's routing rules (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/departments/{id}/rules", owner: "departments", description: "Create a routing rule for a department." },
            RouteInfo { method: "GET", path: "/api/v1/departments/rules", owner: "departments", description: "List all routing rules (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/departments/rules/{rule_id}", owner: "departments", description: "Get one routing rule." },
            RouteInfo { method: "PUT", path: "/api/v1/departments/rules/{rule_id}", owner: "departments", description: "Update a routing rule." },
            RouteInfo { method: "DELETE", path: "/api/v1/departments/rules/{rule_id}", owner: "departments", description: "Delete a routing rule." },
            RouteInfo { method: "POST", path: "/api/v1/departments/route-preview", owner: "departments", description: "Dry-run ticket routing for a draft ticket." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DepartmentsModule::new()));
}
