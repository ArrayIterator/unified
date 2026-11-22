// Tasks module.
//
// Task management under `/api/v1/tasks`: tasks with title, description,
// assignee (a user id), reporter, priority, a validated status workflow,
// due date (overdue is computed), optional company linkage and project
// name, tags, comments, checklist items, and time estimate / time spent.
//
// Status workflow:
//
//   todo -> in_progress -> review -> done
//   todo / in_progress / review -> blocked -> todo | in_progress
//   any non-terminal status -> cancelled
//
// `done` and `cancelled` are terminal: no transition out of them is
// accepted. An illegal transition is rejected with HTTP 422 and the
// allowed target statuses in the error metadata.
//
// Persistence: tasks live in an in-memory store (the same approach the
// audit module uses). The Diesel tables for durable persistence do not
// exist in `src/db/schema.rs` yet; the migration SQL and the schema /
// model entries are supplied to the integrator separately and must be
// added before a database-backed implementation can be wired in.
// Nothing in this file references a table that does not exist yet, so
// it compiles against the current schema.
//
// Interconnection happens only through the hook bus. This module emits:
// `task.created`, `task.assigned`, `task.status.changed`,
// `task.completed`, and `task.comment.added`. The records module
// subscribes to these events. The assignee is referenced by user id
// only; this module never calls the users module directly.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Event names emitted by this module.
// ---------------------------------------------------------------------------

pub const TASK_CREATED: &str = "task.created";
pub const TASK_ASSIGNED: &str = "task.assigned";
pub const TASK_STATUS_CHANGED: &str = "task.status.changed";
pub const TASK_COMPLETED: &str = "task.completed";
pub const TASK_COMMENT_ADDED: &str = "task.comment.added";

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    Medium,
    High,
    Urgent,
}

impl TaskPriority {
    fn rank(self) -> u8 {
        match self {
            TaskPriority::Low => 0,
            TaskPriority::Medium => 1,
            TaskPriority::High => 2,
            TaskPriority::Urgent => 3,
        }
    }
}

impl Default for TaskPriority {
    fn default() -> Self {
        TaskPriority::Medium
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Review,
    Done,
    Blocked,
    Cancelled,
}

impl TaskStatus {
    fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Todo => "todo",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Review => "review",
            TaskStatus::Done => "done",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    /// Statuses this status may move to. `done` and `cancelled` are
    /// terminal.
    fn allowed_next(self) -> &'static [TaskStatus] {
        match self {
            TaskStatus::Todo => &[
                TaskStatus::InProgress,
                TaskStatus::Blocked,
                TaskStatus::Cancelled,
            ],
            TaskStatus::InProgress => &[
                TaskStatus::Review,
                TaskStatus::Todo,
                TaskStatus::Blocked,
                TaskStatus::Cancelled,
            ],
            TaskStatus::Review => &[
                TaskStatus::Done,
                TaskStatus::InProgress,
                TaskStatus::Blocked,
                TaskStatus::Cancelled,
            ],
            TaskStatus::Blocked => &[
                TaskStatus::Todo,
                TaskStatus::InProgress,
                TaskStatus::Cancelled,
            ],
            TaskStatus::Done | TaskStatus::Cancelled => &[],
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "todo" => Some(TaskStatus::Todo),
            "in_progress" => Some(TaskStatus::InProgress),
            "review" => Some(TaskStatus::Review),
            "done" => Some(TaskStatus::Done),
            "blocked" => Some(TaskStatus::Blocked),
            "cancelled" => Some(TaskStatus::Cancelled),
            _ => None,
        }
    }
}

impl Default for TaskStatus {
    fn default() -> Self {
        TaskStatus::Todo
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskComment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: Option<Uuid>,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub id: Uuid,
    pub task_id: Uuid,
    pub title: String,
    pub done: bool,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Stored task record. `overdue` is computed, never stored.
#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub assignee_id: Option<Uuid>,
    pub reporter_id: Option<Uuid>,
    pub priority: TaskPriority,
    pub status: TaskStatus,
    pub due_date: Option<DateTime<Utc>>,
    pub company_id: Option<Uuid>,
    pub project_name: Option<String>,
    pub tags: Vec<String>,
    pub comments: Vec<TaskComment>,
    pub checklist: Vec<ChecklistItem>,
    /// Estimated effort in minutes.
    pub estimate_minutes: Option<i32>,
    /// Time actually spent, in minutes.
    pub spent_minutes: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    /// True when the due date has passed and the task is not in a
    /// terminal status.
    pub fn is_overdue(&self, now: DateTime<Utc>) -> bool {
        match self.due_date {
            Some(due) => {
                due < now
                    && self.status != TaskStatus::Done
                    && self.status != TaskStatus::Cancelled
            }
            None => false,
        }
    }
}

/// Public task representation, with the computed `overdue` flag.
#[derive(Debug, Clone, Serialize)]
pub struct TaskResponse {
    #[serde(flatten)]
    pub task: Task,
    pub overdue: bool,
}

impl From<&Task> for TaskResponse {
    fn from(task: &Task) -> Self {
        let overdue = task.is_overdue(Utc::now());
        Self {
            task: task.clone(),
            overdue,
        }
    }
}

pub type TasksStore = Arc<DashMap<Uuid, Task>>;

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaskInput {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub assignee_id: Option<Uuid>,
    #[serde(default)]
    pub reporter_id: Option<Uuid>,
    #[serde(default)]
    pub priority: Option<TaskPriority>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub company_id: Option<Uuid>,
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub estimate_minutes: Option<i32>,
    #[serde(default)]
    pub spent_minutes: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaskInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// `Some(Some(id))` sets the assignee, `Some(None)` clears it,
    /// `None` (field absent) leaves it unchanged.
    #[serde(default)]
    pub assignee_id: Option<Option<Uuid>>,
    #[serde(default)]
    pub reporter_id: Option<Option<Uuid>>,
    #[serde(default)]
    pub priority: Option<TaskPriority>,
    #[serde(default)]
    pub due_date: Option<Option<DateTime<Utc>>>,
    #[serde(default)]
    pub company_id: Option<Option<Uuid>>,
    #[serde(default)]
    pub project_name: Option<Option<String>>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub estimate_minutes: Option<Option<i32>>,
    #[serde(default)]
    pub spent_minutes: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChangeStatusInput {
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssignInput {
    /// Null unassigns the task.
    pub assignee_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddCommentInput {
    #[serde(default)]
    pub author_id: Option<Uuid>,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddChecklistInput {
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogTimeInput {
    /// Minutes to add to `spent_minutes`. Must be positive.
    pub minutes: i32,
}

// ---------------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------------

fn validate_title(title: &str) -> UnifiedResult<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(UnifiedError::bad_request("Title must not be empty"));
    }
    if title.chars().count() > 255 {
        return Err(UnifiedError::bad_request(
            "Title must be at most 255 characters long",
        ));
    }
    Ok(title.to_string())
}

fn validate_description(description: &str) -> UnifiedResult<()> {
    if description.chars().count() > 10_000 {
        return Err(UnifiedError::bad_request(
            "Description must be at most 10000 characters long",
        ));
    }
    Ok(())
}

fn validate_tags(tags: &[String]) -> UnifiedResult<Vec<String>> {
    if tags.len() > 20 {
        return Err(UnifiedError::bad_request("At most 20 tags are allowed"));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim().to_lowercase();
        if tag.is_empty() {
            return Err(UnifiedError::bad_request("Tags must not be empty"));
        }
        if tag.chars().count() > 64 {
            return Err(UnifiedError::bad_request(
                "Each tag must be at most 64 characters long",
            ));
        }
        if !cleaned.contains(&tag) {
            cleaned.push(tag);
        }
    }
    Ok(cleaned)
}

fn validate_minutes(value: i32, field: &str) -> UnifiedResult<i32> {
    if value < 0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be negative"
        )));
    }
    Ok(value)
}

fn client_ip(req: &HttpRequest) -> Option<String> {
    req.connection_info()
        .realip_remote_addr()
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn task_event_payload(task: &Task, ip: Option<&str>) -> Value {
    let mut payload = serde_json::json!({
        "id": task.id,
        "task_id": task.id,
        "title": task.title,
        "status": task.status.as_str(),
        "priority": task.priority,
        "assignee_id": task.assignee_id,
        "reporter_id": task.reporter_id,
        "company_id": task.company_id,
        "project_name": task.project_name,
        "entity_type": "task",
        "entity_id": task.id,
        "resource": format!("task:{}", task.id),
    });
    if let Some(ip) = ip {
        if let Value::Object(map) = &mut payload {
            map.insert("ip".to_string(), Value::String(ip.to_string()));
        }
    }
    if let Some(actor) = task.reporter_id {
        if let Value::Object(map) = &mut payload {
            map.insert("actor_id".to_string(), Value::String(actor.to_string()));
        }
    }
    payload
}

fn parse_date_filter(
    params: &PaginationParams,
    key: &str,
) -> UnifiedResult<Option<DateTime<Utc>>> {
    match params.filters.get(key) {
        Some(raw) => raw
            .parse::<DateTime<Utc>>()
            .map(Some)
            .map_err(|_| {
                UnifiedError::bad_request(format!(
                    "Filter \"{key}\" must be an RFC 3339 date-time, got \"{raw}\""
                ))
            }),
        None => Ok(None),
    }
}

fn parse_uuid_filter(params: &PaginationParams, key: &str) -> UnifiedResult<Option<Uuid>> {
    match params.filters.get(key) {
        Some(raw) => raw.parse::<Uuid>().map(Some).map_err(|_| {
            UnifiedError::bad_request(format!("Filter \"{key}\" must be a UUID, got \"{raw}\""))
        }),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn list_tasks(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let pagination = params.normalized();
    let sort = params.allowed_sort(
        &["created_at", "due_date", "priority", "title", "updated_at"],
        "created_at",
    );
    let descending = pagination.order == "desc";
    let search = pagination
        .search
        .clone()
        .map(|value| value.to_lowercase());

    let status_filter = match params.filters.get("status").map(|value| {
        TaskStatus::parse(value).ok_or_else(|| {
            UnifiedError::bad_request(format!("Unknown task status: \"{value}\""))
        })
    }) {
        Some(Ok(status)) => Some(status),
        Some(Err(error)) => return error_response(&error, debug),
        None => None,
    };
    let priority_filter = match params.filters.get("priority").map(|value| {
        serde_json::from_value::<TaskPriority>(Value::String(value.clone())).map_err(|_| {
            UnifiedError::bad_request(format!("Unknown task priority: \"{value}\""))
        })
    }) {
        Some(Ok(priority)) => Some(priority),
        Some(Err(error)) => return error_response(&error, debug),
        None => None,
    };
    let assignee_filter = match parse_uuid_filter(&params, "assignee_id") {
        Ok(value) => value,
        Err(error) => return error_response(&error, debug),
    };
    let company_filter = match parse_uuid_filter(&params, "company_id") {
        Ok(value) => value,
        Err(error) => return error_response(&error, debug),
    };
    let due_before = match parse_date_filter(&params, "due_before") {
        Ok(value) => value,
        Err(error) => return error_response(&error, debug),
    };
    let due_after = match parse_date_filter(&params, "due_after") {
        Ok(value) => value,
        Err(error) => return error_response(&error, debug),
    };
    let overdue_filter = params
        .filters
        .get("overdue")
        .and_then(|value| value.parse::<bool>().ok());
    let project_filter = params.filters.get("project_name").cloned();
    let now = Utc::now();

    let mut records: Vec<Task> = store
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|task| {
            if let Some(status) = status_filter {
                if task.status != status {
                    return false;
                }
            }
            if let Some(priority) = priority_filter {
                if task.priority != priority {
                    return false;
                }
            }
            if let Some(assignee) = assignee_filter {
                if task.assignee_id != Some(assignee) {
                    return false;
                }
            }
            if let Some(company) = company_filter {
                if task.company_id != Some(company) {
                    return false;
                }
            }
            if let Some(before) = due_before {
                match task.due_date {
                    Some(due) if due <= before => {}
                    _ => return false,
                }
            }
            if let Some(after) = due_after {
                match task.due_date {
                    Some(due) if due >= after => {}
                    _ => return false,
                }
            }
            if let Some(want_overdue) = overdue_filter {
                if task.is_overdue(now) != want_overdue {
                    return false;
                }
            }
            if let Some(project) = &project_filter {
                match &task.project_name {
                    Some(name) if name.eq_ignore_ascii_case(project) => {}
                    _ => return false,
                }
            }
            if let Some(needle) = &search {
                let haystack = format!(
                    "{} {} {} {}",
                    task.title,
                    task.description,
                    task.project_name.clone().unwrap_or_default(),
                    task.tags.join(" ")
                )
                .to_lowercase();
                if !haystack.contains(needle.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();

    records.sort_by(|a, b| match sort.as_str() {
        "due_date" => a.due_date.cmp(&b.due_date),
        "priority" => a.priority.rank().cmp(&b.priority.rank()),
        "title" => a.title.cmp(&b.title),
        "updated_at" => a.updated_at.cmp(&b.updated_at),
        _ => a.created_at.cmp(&b.created_at),
    });
    if descending {
        records.reverse();
    }

    let total = records.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let items: Vec<TaskResponse> = records
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .map(|task| TaskResponse::from(&task))
        .collect();
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
}

async fn create_task(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    body: web::Json<CreateTaskInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let ip = client_ip(&req);
    let result: UnifiedResult<Task> = (|| {
        let title = validate_title(&input.title)?;
        let description = input.description.unwrap_or_default();
        validate_description(&description)?;
        let tags = validate_tags(&input.tags.unwrap_or_default())?;
        let estimate = match input.estimate_minutes {
            Some(minutes) => Some(validate_minutes(minutes, "estimate_minutes")?),
            None => None,
        };
        let spent = validate_minutes(input.spent_minutes.unwrap_or(0), "spent_minutes")?;
        let status = input.status.unwrap_or_default();
        let now = Utc::now();
        let task = Task {
            id: Uuid::new_v7(),
            title,
            description,
            assignee_id: input.assignee_id,
            reporter_id: input.reporter_id,
            priority: input.priority.unwrap_or_default(),
            status,
            due_date: input.due_date,
            company_id: input.company_id,
            project_name: input
                .project_name
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty()),
            tags,
            comments: Vec::new(),
            checklist: Vec::new(),
            estimate_minutes: estimate,
            spent_minutes: spent,
            completed_at: if status == TaskStatus::Done {
                Some(now)
            } else {
                None
            },
            created_at: now,
            updated_at: now,
        };
        store.insert(task.id, task.clone());
        Ok(task)
    })();
    match result {
        Ok(task) => {
            ctx.hooks.emit(TASK_CREATED, task_event_payload(&task, ip.as_deref()));
            if task.assignee_id.is_some() {
                ctx.hooks
                    .emit(TASK_ASSIGNED, task_event_payload(&task, ip.as_deref()));
            }
            if task.status == TaskStatus::Done {
                ctx.hooks
                    .emit(TASK_COMPLETED, task_event_payload(&task, ip.as_deref()));
            }
            success_response(TaskResponse::from(&task), 201)
        }
        Err(error) => error_response(&error, debug),
    }
}

async fn get_task(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let task_id = id.into_inner();
    match store.get(&task_id) {
        Some(record) => success_response(TaskResponse::from(record.value()), 200),
        None => error_response(
            &UnifiedError::not_found(format!("Task {task_id} not found")),
            debug,
        ),
    }
}

async fn update_task(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateTaskInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let task_id = id.into_inner();
    let input = body.into_inner();
    let result: UnifiedResult<Task> = (|| {
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        if let Some(title) = &input.title {
            record.title = validate_title(title)?;
        }
        if let Some(description) = &input.description {
            validate_description(description)?;
            record.description = description.clone();
        }
        if let Some(assignee) = input.assignee_id {
            record.assignee_id = assignee;
        }
        if let Some(reporter) = input.reporter_id {
            record.reporter_id = reporter;
        }
        if let Some(priority) = input.priority {
            record.priority = priority;
        }
        if let Some(due_date) = input.due_date {
            record.due_date = due_date;
        }
        if let Some(company) = input.company_id {
            record.company_id = company;
        }
        if let Some(project) = input.project_name {
            record.project_name = project
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty());
        }
        if let Some(tags) = &input.tags {
            record.tags = validate_tags(tags)?;
        }
        if let Some(estimate) = input.estimate_minutes {
            record.estimate_minutes = match estimate {
                Some(minutes) => Some(validate_minutes(minutes, "estimate_minutes")?),
                None => None,
            };
        }
        if let Some(spent) = input.spent_minutes {
            record.spent_minutes = validate_minutes(spent, "spent_minutes")?;
        }
        record.updated_at = Utc::now();
        Ok(record.clone())
    })();
    match result {
        Ok(task) => success_response(TaskResponse::from(&task), 200),
        Err(error) => error_response(&error, debug),
    }
}

async fn delete_task(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let task_id = id.into_inner();
    match store.remove(&task_id) {
        Some(_) => success_response(
            serde_json::json!({ "deleted": true, "id": task_id }),
            200,
        ),
        None => error_response(
            &UnifiedError::not_found(format!("Task {task_id} not found")),
            debug,
        ),
    }
}

async fn change_task_status(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<ChangeStatusInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let task_id = id.into_inner();
    let target = body.status;
    let ip = client_ip(&req);
    let result: UnifiedResult<(Task, TaskStatus)> = (|| {
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        let from = record.status;
        if from == target {
            return Err(UnifiedError::unprocessable(format!(
                "Task is already in status \"{}\"",
                from.as_str()
            ))
            .with_metadata("from", Value::String(from.as_str().to_string()))
            .with_metadata("to", Value::String(target.as_str().to_string())));
        }
        if !from.allowed_next().contains(&target) {
            let allowed: Vec<&str> = from
                .allowed_next()
                .iter()
                .map(|status| status.as_str())
                .collect();
            return Err(UnifiedError::unprocessable(format!(
                "Cannot move task from \"{}\" to \"{}\"",
                from.as_str(),
                target.as_str()
            ))
            .with_metadata("from", Value::String(from.as_str().to_string()))
            .with_metadata("to", Value::String(target.as_str().to_string()))
            .with_metadata("allowed", Value::from(allowed)));
        }
        record.status = target;
        record.updated_at = Utc::now();
        if target == TaskStatus::Done {
            record.completed_at = Some(record.updated_at);
        } else if from == TaskStatus::Done {
            record.completed_at = None;
        }
        Ok((record.clone(), from))
    })();
    match result {
        Ok((task, from)) => {
            // Emit the status change with the target status and the
            // previous status; the completion event fires only when the
            // task reaches done.
            let mut payload = task_event_payload(&task, ip.as_deref());
            if let Value::Object(map) = &mut payload {
                map.insert(
                    "from_status".to_string(),
                    Value::String(from.as_str().to_string()),
                );
            }
            ctx.hooks.emit(TASK_STATUS_CHANGED, payload);
            if task.status == TaskStatus::Done {
                ctx.hooks
                    .emit(TASK_COMPLETED, task_event_payload(&task, ip.as_deref()));
            }
            success_response(TaskResponse::from(&task), 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

async fn assign_task(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<AssignInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let task_id = id.into_inner();
    let assignee_id = body.assignee_id;
    let ip = client_ip(&req);
    let result: UnifiedResult<Task> = (|| {
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        record.assignee_id = assignee_id;
        record.updated_at = Utc::now();
        Ok(record.clone())
    })();
    match result {
        Ok(task) => {
            ctx.hooks
                .emit(TASK_ASSIGNED, task_event_payload(&task, ip.as_deref()));
            success_response(TaskResponse::from(&task), 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

async fn log_time(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    id: web::Path<Uuid>,
    body: web::Json<LogTimeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let task_id = id.into_inner();
    let minutes = body.minutes;
    let result: UnifiedResult<Task> = (|| {
        if minutes <= 0 {
            return Err(UnifiedError::bad_request(
                "Logged time must be a positive number of minutes",
            ));
        }
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        record.spent_minutes = record
            .spent_minutes
            .checked_add(minutes)
            .ok_or_else(|| UnifiedError::bad_request("Time spent would overflow"))?;
        record.updated_at = Utc::now();
        Ok(record.clone())
    })();
    match result {
        Ok(task) => success_response(TaskResponse::from(&task), 200),
        Err(error) => error_response(&error, debug),
    }
}

async fn list_comments(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    id: web::Path<Uuid>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let task_id = id.into_inner();
    let params = query.into_inner();
    let pagination = params.normalized();
    let descending = pagination.order == "desc";
    let Some(record) = store.get(&task_id) else {
        return error_response(
            &UnifiedError::not_found(format!("Task {task_id} not found")),
            debug,
        );
    };
    let mut comments = record.comments.clone();
    if let Some(needle) = &pagination.search {
        let needle = needle.to_lowercase();
        comments.retain(|comment| comment.body.to_lowercase().contains(&needle));
    }
    comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if descending {
        comments.reverse();
    }
    let total = comments.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let items: Vec<TaskComment> = comments
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
}

async fn add_comment(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<AddCommentInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let task_id = id.into_inner();
    let input = body.into_inner();
    let ip = client_ip(&req);
    let result: UnifiedResult<TaskComment> = (|| {
        let body_text = input.body.trim();
        if body_text.is_empty() {
            return Err(UnifiedError::bad_request("Comment body must not be empty"));
        }
        if body_text.chars().count() > 5_000 {
            return Err(UnifiedError::bad_request(
                "Comment body must be at most 5000 characters long",
            ));
        }
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        let comment = TaskComment {
            id: Uuid::new_v7(),
            task_id,
            author_id: input.author_id,
            body: body_text.to_string(),
            created_at: Utc::now(),
        };
        record.comments.push(comment.clone());
        record.updated_at = Utc::now();
        Ok(comment)
    })();
    match result {
        Ok(comment) => {
            let mut payload = serde_json::json!({
                "id": comment.id,
                "comment_id": comment.id,
                "task_id": task_id,
                "author_id": comment.author_id,
                "entity_type": "task",
                "entity_id": task_id,
                "resource": format!("task:{task_id}"),
                "summary": "Comment added to task",
            });
            if let Some(actor) = comment.author_id {
                if let Value::Object(map) = &mut payload {
                    map.insert("actor_id".to_string(), Value::String(actor.to_string()));
                }
            }
            if let Some(ip) = ip {
                if let Value::Object(map) = &mut payload {
                    map.insert("ip".to_string(), Value::String(ip));
                }
            }
            ctx.hooks.emit(TASK_COMMENT_ADDED, payload);
            success_response(comment, 201)
        }
        Err(error) => error_response(&error, debug),
    }
}

async fn add_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    id: web::Path<Uuid>,
    body: web::Json<AddChecklistInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let task_id = id.into_inner();
    let input = body.into_inner();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let title = input.title.trim();
        if title.is_empty() {
            return Err(UnifiedError::bad_request(
                "Checklist item title must not be empty",
            ));
        }
        if title.chars().count() > 255 {
            return Err(UnifiedError::bad_request(
                "Checklist item title must be at most 255 characters long",
            ));
        }
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        let item = ChecklistItem {
            id: Uuid::new_v7(),
            task_id,
            title: title.to_string(),
            done: false,
            completed_at: None,
            created_at: Utc::now(),
        };
        record.checklist.push(item.clone());
        record.updated_at = Utc::now();
        Ok(item)
    })();
    match result {
        Ok(item) => success_response(item, 201),
        Err(error) => error_response(&error, debug),
    }
}

async fn toggle_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (task_id, item_id) = path.into_inner();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        let item = record
            .checklist
            .iter_mut()
            .find(|item| item.id == item_id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Checklist item {item_id} not found"))
            })?;
        item.done = !item.done;
        item.completed_at = if item.done { Some(Utc::now()) } else { None };
        let updated = item.clone();
        record.updated_at = Utc::now();
        Ok(updated)
    })();
    match result {
        Ok(item) => success_response(item, 200),
        Err(error) => error_response(&error, debug),
    }
}

async fn delete_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<TasksStore>,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (task_id, item_id) = path.into_inner();
    let result: UnifiedResult<()> = (|| {
        let mut record = store.get_mut(&task_id).ok_or_else(|| {
            UnifiedError::not_found(format!("Task {task_id} not found"))
        })?;
        let before = record.checklist.len();
        record.checklist.retain(|item| item.id != item_id);
        if record.checklist.len() == before {
            return Err(UnifiedError::not_found(format!(
                "Checklist item {item_id} not found"
            )));
        }
        record.updated_at = Utc::now();
        Ok(())
    })();
    match result {
        Ok(()) => success_response(
            serde_json::json!({ "deleted": true, "id": item_id }),
            200,
        ),
        Err(error) => error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct TasksModule {
    store: TasksStore,
}

impl TasksModule {
    pub fn new() -> Self {
        Self {
            store: Arc::new(DashMap::new()),
        }
    }
}

impl Default for TasksModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TasksModule {
    fn id(&self) -> &'static str {
        "tasks"
    }

    fn name(&self) -> &'static str {
        "Tasks"
    }

    fn description(&self) -> &'static str {
        "Task management: status workflow, assignees, due dates, tags, comments, checklist items, and time tracking. Persists in memory; the database tables are the persistence target (see the module header)."
    }

    fn requires(&self) -> &[&'static str] {
        &["users"]
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/tasks", owner: "tasks", description: "List tasks with pagination and filters." },
            RouteInfo { method: "POST", path: "/api/v1/tasks", owner: "tasks", description: "Create a task." },
            RouteInfo { method: "GET", path: "/api/v1/tasks/{id}", owner: "tasks", description: "Get one task." },
            RouteInfo { method: "PUT", path: "/api/v1/tasks/{id}", owner: "tasks", description: "Update a task." },
            RouteInfo { method: "DELETE", path: "/api/v1/tasks/{id}", owner: "tasks", description: "Delete a task." },
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/status", owner: "tasks", description: "Change a task status (validated transition)." },
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/assign", owner: "tasks", description: "Assign or unassign a task." },
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/time", owner: "tasks", description: "Log time spent on a task." },
            RouteInfo { method: "GET", path: "/api/v1/tasks/{id}/comments", owner: "tasks", description: "List comments on a task." },
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/comments", owner: "tasks", description: "Add a comment to a task." },
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/checklist", owner: "tasks", description: "Add a checklist item to a task." },
            RouteInfo { method: "POST", path: "/api/v1/tasks/{id}/checklist/{item_id}/toggle", owner: "tasks", description: "Toggle a checklist item." },
            RouteInfo { method: "DELETE", path: "/api/v1/tasks/{id}/checklist/{item_id}", owner: "tasks", description: "Delete a checklist item." },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let store = self.store.clone();
        cfg.service(
            web::scope("/api/v1/tasks")
                .app_data(web::Data::new(store))
                .route("", web::get().to(list_tasks))
                .route("", web::post().to(create_task))
                .route("/{id}", web::get().to(get_task))
                .route("/{id}", web::put().to(update_task))
                .route("/{id}", web::delete().to(delete_task))
                .route("/{id}/status", web::post().to(change_task_status))
                .route("/{id}/assign", web::post().to(assign_task))
                .route("/{id}/time", web::post().to(log_time))
                .route("/{id}/comments", web::get().to(list_comments))
                .route("/{id}/comments", web::post().to(add_comment))
                .route("/{id}/checklist", web::post().to(add_checklist_item))
                .route(
                    "/{id}/checklist/{item_id}/toggle",
                    web::post().to(toggle_checklist_item),
                )
                .route(
                    "/{id}/checklist/{item_id}",
                    web::delete().to(delete_checklist_item),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TasksModule::new()));
}
