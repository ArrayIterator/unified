// Form Builder module — form definitions with drag-drop field
// order, embed codes, a submissions inbox, server-side validation,
// spam protection, and submission mapping to CRM leads, support
// tickets, or tasks.
//
// Routes live under `/api/v1/forms`:
//
//   Forms (admin):
//     GET/POST / — list forms / create a form (with fields).
//     GET/PATCH/DELETE /{id}.
//     GET  /{id}/embed — embed code (render + submit URLs, script
//       and iframe snippets) for the form.
//   Fields (admin):
//     GET/POST /{id}/fields.
//     GET/PATCH/DELETE /{id}/fields/{field_id}.
//     POST /{id}/fields/reorder — reorder fields by id list.
//   Submissions inbox (admin):
//     GET  /{id}/submissions — paginated, filter by status.
//     GET  /{id}/submissions/export?format=csv|json.
//     GET  /submissions/{submission_id} — one submission with its
//       values.
//     POST /submissions/{submission_id}/read — mark read.
//     POST /submissions/{submission_id}/spam — mark spam.
//     POST /submissions/{submission_id}/remap — retry the mapping.
//   Public (no auth; see the public-access note below):
//     GET  /public/{slug} — public render: the form definition as
//       JSON for embedding.
//     POST /public/{slug}/submit — public submit: honeypot plus
//       per-IP rate limit plus per-form hourly cap, server-side
//       validation with the same rules as defined, then mapping.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory form, field, submission, and submission-value
// management; the full field-type set (text, textarea, email,
// number, select, multi-select, checkbox, radio, date, file,
// rating, consent checkbox); per-field required flags, length /
// value bounds, regex patterns (compiled and rejected when
// invalid), allowed values for choice fields, and conditional
// visibility (`show_when`: field B shows only when field A equals
// X — hidden fields are skipped by validation and never stored);
// server-side validation on submit using exactly the defined
// rules; spam protection (honeypot field, per-IP rate limit of 10
// submissions per form per hour, per-form cap of 200 submissions
// per hour); submission mapping that emits
// `forms.mapping.requested` and records the outcome on the
// submission; and `forms.created`, `forms.updated`,
// `forms.submission.received`, and `forms.submission.mapped`
// events on the hook bus. What is NOT built here: actual CRM lead
// / ticket / task creation — the owning modules (CRM, tickets,
// tasks) must register a `forms.mapping.requested` handler that
// answers `{"created": true, "id": "..."}` for the requested
// target (`crm_lead`, `support_ticket`, or `task`) with the
// payload shape `{"target", "form_id", "submission_id", "title",
// "fields": {target_field: value}}`. Until such a handler exists
// the mapping is recorded as `failed` with the error "no mapping
// handler registered for target ..." — mapping failures are
// logged on the submission, never silent, and can be retried
// through `/submissions/{id}/remap`. File upload itself is NOT
// done here either: a `file` field carries a storage-module key
// reference (the client uploads the file through the storage
// module first and submits the key); only the key is validated
// and stored. And the public render / submit routes are public
// only through the API Manager gate described below.
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler — the
// check lives in the API Manager middleware / `resolve_public`
// path — so the public routes below rely on that gate: an
// administrator must create the matching `ApiAccessRule` rows for
// `GET /api/v1/forms/public/*` and `POST
// /api/v1/forms/public/*/submit`. The routes are registered in
// `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks them up.
// Do NOT treat the absence of a capability check on a public route
// as an open door: without the rule the API Manager denies the
// call.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// Public submit callers are identified only by their IP address,
// for rate limiting; no account is required.
//
// Events emitted on the hook bus:
//   - `forms.created`, `forms.updated`, `forms.deleted`
//   - `forms.submission.received`
//   - `forms.submission.mapped` (only when a mapping succeeds)
//   - `forms.mapping.requested` (payload `{"target", "form_id",
//     "submission_id", "title", "fields"}`; a handler answers
//     `{"created": true, "id": "..."}` or
//     `{"created": false, "error": "..."}`)
// Events this module listens to: none.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_SLUG_LEN: usize = 128;
const MAX_LABEL_LEN: usize = 200;
const MAX_KEY_LEN: usize = 64;
const MAX_PLACEHOLDER_LEN: usize = 200;
const MAX_HELP_LEN: usize = 500;
const MAX_OPTION_LEN: usize = 128;
const MAX_OPTIONS: usize = 50;
const MAX_FIELDS_PER_FORM: usize = 100;
const MAX_PATTERN_LEN: usize = 500;
const MAX_STORAGE_KEY_LEN: usize = 512;
const MAX_TITLE_TEMPLATE_LEN: usize = 500;
const MAX_SUCCESS_MESSAGE_LEN: usize = 1000;
const MAX_BUTTON_LABEL_LEN: usize = 80;
const MAX_HONEYPOT_LEN: usize = 64;
/// Spam protection: at most 10 submissions per IP per form per
/// rolling hour, and at most 200 submissions per form per rolling
/// hour (all IPs together).
const MAX_SUBMISSIONS_PER_IP_PER_HOUR: usize = 10;
const MAX_SUBMISSIONS_PER_FORM_PER_HOUR: usize = 200;

/// Field types a form field may have.
const FIELD_TYPES: [&str; 12] = [
    "text",
    "textarea",
    "email",
    "number",
    "select",
    "multi-select",
    "checkbox",
    "radio",
    "date",
    "file",
    "rating",
    "consent",
];

/// Mapping targets a form may map submissions to.
const MAPPING_TARGETS: [&str; 3] = ["crm_lead", "support_ticket", "task"];

/// Capability names checked through the `role.check` hook.
const PERM_FORMS_VIEW: &str = "forms.view";
const PERM_FORMS_MANAGE: &str = "forms.manage";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle of a form: `draft` -> `published`; `archived` hides
/// the form from public render and submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormStatus {
    Draft,
    Published,
    Archived,
}

impl Default for FormStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl FormStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::Archived => "archived",
        }
    }
}

/// Conditional visibility: this field is shown only when the field
/// named `field_key` currently equals `equals` (compared as
/// strings). Hidden fields are skipped by submit validation and
/// never stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldCondition {
    pub field_key: String,
    pub equals: String,
}

/// Where a submission goes after it is received. `field_map` maps
/// form field keys to target field names; `title_template` may
/// contain `{field_key}` placeholders filled from the submitted
/// values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormMapping {
    pub target: String,
    #[serde(default)]
    pub field_map: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_template: Option<String>,
}

/// One form definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Form {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub slug: String,
    pub status: FormStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub owner_id: String,
    #[serde(default = "default_submit_label")]
    pub submit_button_label: String,
    #[serde(default = "default_success_message")]
    pub success_message: String,
    /// Name of the honeypot field rendered in the form but never
    /// filled by a human. A non-empty value marks the submission
    /// as spam.
    #[serde(default = "default_honeypot_field")]
    pub honeypot_field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping: Option<FormMapping>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_submit_label() -> String {
    "Submit".to_string()
}

fn default_success_message() -> String {
    "Thank you for your submission.".to_string()
}

fn default_honeypot_field() -> String {
    "website".to_string()
}

/// One field of a form. Validation rules live here and are applied
/// verbatim at submit time: `min_length` / `max_length` and
/// `pattern` for text-like fields, `min_value` / `max_value` for
/// numbers, `options` (allowed values) for choice fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormField {
    pub id: Uuid,
    pub form_id: Uuid,
    /// Machine name, `[a-z0-9_]+`, unique per form. Submitted
    /// values are keyed by it.
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub position: i64,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help_text: Option<String>,
    /// Allowed values for `select`, `multi-select`, and `radio`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_value: Option<f64>,
    /// Regex the value must match (text, textarea, email only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_when: Option<FieldCondition>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One received submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormSubmission {
    pub id: Uuid,
    pub form_id: Uuid,
    /// `new`, `read`, or `spam`.
    pub status: String,
    /// `none` (no mapping configured), `mapped`, or `failed`.
    pub mapping_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapped_target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One submitted value. `value` holds the validated value as JSON
/// (string, number, boolean, or array for multi-select).
/// `file_storage_key` is set for `file` fields: the storage-module
/// key reference submitted by the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormSubmissionValue {
    pub id: Uuid,
    pub submission_id: Uuid,
    pub field_id: Uuid,
    pub field_key: String,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_storage_key: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateFieldRequest {
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub position: Option<i64>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help_text: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_when: Option<FieldCondition>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateFieldRequest {
    pub key: Option<String>,
    pub label: Option<String>,
    pub field_type: Option<String>,
    pub position: Option<i64>,
    pub required: Option<bool>,
    pub placeholder: Option<Option<String>>,
    pub help_text: Option<Option<String>>,
    pub options: Option<Vec<String>>,
    pub min_length: Option<Option<i64>>,
    pub max_length: Option<Option<i64>>,
    pub min_value: Option<Option<f64>>,
    pub max_value: Option<Option<f64>>,
    pub pattern: Option<Option<String>>,
    pub show_when: Option<Option<FieldCondition>>,
}

#[derive(Debug, Deserialize)]
pub struct MappingRequest {
    pub target: String,
    #[serde(default)]
    pub field_map: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_template: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateFormRequest {
    pub title: String,
    pub description: Option<String>,
    pub slug: Option<String>,
    pub status: Option<FormStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub submit_button_label: Option<String>,
    pub success_message: Option<String>,
    pub honeypot_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping: Option<MappingRequest>,
    #[serde(default)]
    pub fields: Vec<CreateFieldRequest>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateFormRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub slug: Option<String>,
    pub status: Option<FormStatus>,
    pub company_id: Option<Option<String>>,
    pub submit_button_label: Option<String>,
    pub success_message: Option<String>,
    pub honeypot_field: Option<String>,
    pub mapping: Option<Option<MappingRequest>>,
}

#[derive(Debug, Deserialize)]
pub struct ReorderFieldsRequest {
    pub field_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitFormRequest {
    /// Submitted values keyed by field key. The honeypot field
    /// travels here too, under the form's honeypot field name.
    #[serde(default)]
    pub values: Map<String, Value>,
    /// Optional top-level honeypot value, for clients that send it
    /// outside `values`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub honeypot: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default = "default_export_format")]
    pub format: String,
}

fn default_export_format() -> String {
    "json".to_string()
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for forms. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000047_forms`; until that
/// migration is executed, forms do not survive a restart.
/// `attempts` holds submit timestamps per rate-limit key, pruned to
/// the last hour on every submit.
#[derive(Clone)]
pub struct FormsState {
    pub forms: Arc<DashMap<Uuid, Form>>,
    pub fields: Arc<DashMap<Uuid, FormField>>,
    pub submissions: Arc<DashMap<Uuid, FormSubmission>>,
    pub values: Arc<DashMap<Uuid, FormSubmissionValue>>,
    pub attempts: Arc<DashMap<String, Vec<DateTime<Utc>>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for FormsState {
    fn default() -> Self {
        Self {
            forms: Arc::new(DashMap::new()),
            fields: Arc::new(DashMap::new()),
            submissions: Arc::new(DashMap::new()),
            values: Arc::new(DashMap::new()),
            attempts: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl FormsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
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

/// A slug is lowercase letters, digits, and single hyphens only,
/// never leading or trailing with a hyphen, at most 128 chars.
fn valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in title.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("form");
    }
    out.truncate(MAX_SLUG_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// A field key is lowercase letters, digits, and underscores only,
/// never leading with a digit, at most 64 chars.
fn valid_key(key: &str) -> bool {
    if key.is_empty() || key.len() > MAX_KEY_LEN {
        return false;
    }
    if key.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    key.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > 254 || email.contains(' ') {
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

/// Best-effort client IP: the first `X-Forwarded-For` entry, else
/// the peer address. Used only for rate limiting and submission
/// records, never for authentication.
fn client_ip(req: &HttpRequest) -> Option<String> {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            req.connection_info()
                .realip_remote_addr()
                .map(str::to_string)
        })
}

fn slug_in_use(state: &FormsState, slug: &str, except: Option<Uuid>) -> bool {
    state
        .forms
        .iter()
        .any(|entry| entry.value().slug == slug && Some(entry.value().id) != except)
}

fn load_form(state: &FormsState, raw_id: &str) -> UnifiedResult<Form> {
    let id = parse_uuid(raw_id, "form")?;
    state
        .forms
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Form not found: {id}")))
}

fn load_form_by_slug(state: &FormsState, slug: &str) -> UnifiedResult<Form> {
    state
        .forms
        .iter()
        .map(|entry| entry.value().clone())
        .find(|form| form.slug == slug)
        .ok_or_else(|| UnifiedError::not_found(format!("Form not found: {slug}")))
}

fn fields_for(state: &FormsState, form_id: Uuid) -> Vec<FormField> {
    let mut items: Vec<FormField> = state
        .fields
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|field| field.form_id == form_id)
        .collect();
    items.sort_by_key(|field| (field.position, field.created_at));
    items
}

fn load_field(state: &FormsState, form_id: Uuid, raw_id: &str) -> UnifiedResult<FormField> {
    let id = parse_uuid(raw_id, "field")?;
    state
        .fields
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|field| field.form_id == form_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Field not found: {id}")))
}

fn key_in_use(state: &FormsState, form_id: Uuid, key: &str, except: Option<Uuid>) -> bool {
    state.fields.iter().any(|entry| {
        entry.value().form_id == form_id
            && entry.value().key == key
            && Some(entry.value().id) != except
    })
}

fn validate_mapping(mapping: &MappingRequest) -> UnifiedResult<FormMapping> {
    if !MAPPING_TARGETS.contains(&mapping.target.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "Mapping target must be one of: {}",
            MAPPING_TARGETS.join(", ")
        )));
    }
    let mut field_map: HashMap<String, String> = HashMap::new();
    for (from, to) in &mapping.field_map {
        let from = require_text(from, "mapping.field_map key", MAX_KEY_LEN)?;
        let to = require_text(to, "mapping.field_map value", MAX_KEY_LEN)?;
        if !valid_key(&from) || !valid_key(&to) {
            return Err(UnifiedError::bad_request(
                "mapping.field_map keys and values must be field keys ([a-z0-9_]+, max 64 characters)",
            ));
        }
        field_map.insert(from, to);
    }
    let title_template = match &mapping.title_template {
        Some(raw) => Some(require_text(raw, "mapping.title_template", MAX_TITLE_TEMPLATE_LEN)?),
        None => None,
    };
    Ok(FormMapping {
        target: mapping.target.clone(),
        field_map,
        title_template,
    })
}

/// Validate one field definition before it is stored. `known_keys`
/// holds the keys of the form's other fields (for `show_when`
/// references); a condition may only reference a key in that set.
/// Rules are type-scoped: options only for choice fields, length
/// bounds and patterns only for text-like fields, value bounds
/// only for number fields.
fn validate_field_definition(
    input: &CreateFieldRequest,
    known_keys: &[String],
) -> UnifiedResult<()> {
    if !valid_key(&input.key) {
        return Err(UnifiedError::bad_request(
            "field key must be lowercase letters, digits, and underscores only, never leading with a digit, max 64 characters",
        ));
    }
    if input.label.trim().is_empty() || input.label.len() > MAX_LABEL_LEN {
        return Err(UnifiedError::bad_request(
            "field label must not be empty and must be at most 200 characters",
        ));
    }
    if !FIELD_TYPES.contains(&input.field_type.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "Unknown field type \"{}\" (expected one of: {})",
            input.field_type,
            FIELD_TYPES.join(", ")
        )));
    }
    if let Some(position) = input.position {
        if position < 0 {
            return Err(UnifiedError::bad_request("field position must be >= 0"));
        }
    }
    if let Some(placeholder) = &input.placeholder {
        if placeholder.len() > MAX_PLACEHOLDER_LEN {
            return Err(UnifiedError::bad_request(
                "field placeholder must be at most 200 characters",
            ));
        }
    }
    if let Some(help) = &input.help_text {
        if help.len() > MAX_HELP_LEN {
            return Err(UnifiedError::bad_request(
                "field help_text must be at most 500 characters",
            ));
        }
    }
    let is_choice = matches!(input.field_type.as_str(), "select" | "multi-select" | "radio");
    let is_text_like = matches!(
        input.field_type.as_str(),
        "text" | "textarea" | "email"
    );
    if is_choice {
        if input.options.is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "field type \"{}\" requires at least one option",
                input.field_type
            )));
        }
        if input.options.len() > MAX_OPTIONS {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_OPTIONS} options are allowed per field"
            )));
        }
        let mut seen: Vec<&str> = Vec::new();
        for option in &input.options {
            if option.trim().is_empty() || option.len() > MAX_OPTION_LEN {
                return Err(UnifiedError::bad_request(
                    "field options must not be empty and must be at most 128 characters",
                ));
            }
            if seen.contains(&option.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate field option \"{option}\""
                )));
            }
            seen.push(option.as_str());
        }
    } else if !input.options.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "field type \"{}\" does not take options",
            input.field_type
        )));
    }
    if !is_text_like {
        if input.min_length.is_some() || input.max_length.is_some() {
            return Err(UnifiedError::bad_request(format!(
                "field type \"{}\" does not take min_length / max_length",
                input.field_type
            )));
        }
        if input.pattern.is_some() {
            return Err(UnifiedError::bad_request(format!(
                "field type \"{}\" does not take a regex pattern",
                input.field_type
            )));
        }
    }
    if let (Some(min), Some(max)) = (input.min_length, input.max_length) {
        if min < 0 || max < min {
            return Err(UnifiedError::bad_request(
                "min_length must be >= 0 and max_length must be >= min_length",
            ));
        }
    }
    if input.field_type != "number" {
        if input.min_value.is_some() || input.max_value.is_some() {
            return Err(UnifiedError::bad_request(
                "only number fields take min_value / max_value",
            ));
        }
    }
    if let (Some(min), Some(max)) = (input.min_value, input.max_value) {
        if !min.is_finite() || !max.is_finite() || max < min {
            return Err(UnifiedError::bad_request(
                "min_value and max_value must be finite and max_value must be >= min_value",
            ));
        }
    }
    if let Some(pattern) = &input.pattern {
        if pattern.len() > MAX_PATTERN_LEN {
            return Err(UnifiedError::bad_request(
                "field pattern must be at most 500 characters",
            ));
        }
        if let Err(error) = Regex::new(pattern) {
            return Err(UnifiedError::bad_request(format!(
                "field pattern is not a valid regex: {error}"
            )));
        }
    }
    if let Some(condition) = &input.show_when {
        if condition.field_key == input.key {
            return Err(UnifiedError::bad_request(
                "show_when must reference a different field",
            ));
        }
        if !known_keys.iter().any(|key| key == &condition.field_key) {
            return Err(UnifiedError::bad_request(format!(
                "show_when references unknown field \"{}\"",
                condition.field_key
            )));
        }
        if condition.equals.len() > MAX_OPTION_LEN {
            return Err(UnifiedError::bad_request(
                "show_when.equals must be at most 128 characters",
            ));
        }
    }
    Ok(())
}

fn build_field(state: &FormsState, form_id: Uuid, input: &CreateFieldRequest, now: DateTime<Utc>) -> FormField {
    let position = match input.position {
        Some(position) => position,
        None => fields_for(state, form_id)
            .iter()
            .map(|field| field.position)
            .max()
            .unwrap_or(-1)
            + 1,
    };
    FormField {
        id: Uuid::now_v7(),
        form_id,
        key: input.key.trim().to_string(),
        label: input.label.trim().to_string(),
        field_type: input.field_type.clone(),
        position,
        required: input.required,
        placeholder: input.placeholder.clone(),
        help_text: input.help_text.clone(),
        options: input.options.clone(),
        min_length: input.min_length,
        max_length: input.max_length,
        min_value: input.min_value,
        max_value: input.max_value,
        pattern: input.pattern.clone(),
        show_when: input.show_when.clone(),
        created_at: now,
        updated_at: now,
    }
}

/// Shift every field at or after `position` one step down, so an
/// insert at an explicit position keeps positions unique.
fn shift_fields_for_insert(state: &FormsState, form_id: Uuid, position: i64) {
    for mut entry in state.fields.iter_mut() {
        if entry.value().form_id == form_id && entry.value().position >= position {
            entry.value_mut().position += 1;
            entry.value_mut().updated_at = Utc::now();
        }
    }
}

// ---------------------------------------------------------------------------
// Submission validation
// ---------------------------------------------------------------------------

/// Render a submitted JSON value as a plain string for equality
/// checks (conditional visibility) and CSV export.
fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// True when the field is visible given the submitted values:
/// no condition means visible; otherwise the referenced field's
/// value must equal the condition's `equals` string.
fn field_visible(field: &FormField, values: &Map<String, Value>) -> bool {
    let Some(condition) = &field.show_when else {
        return true;
    };
    match values.get(&condition.field_key) {
        Some(value) => value_to_string(value) == condition.equals,
        None => false,
    }
}

fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// Validate submitted values against the form's field definitions
/// and return the stored values (visible fields only, in field
/// order). Hidden fields are skipped entirely: not validated, not
/// stored. Unknown keys are rejected.
fn validate_submission_values(
    fields: &[FormField],
    values: &Map<String, Value>,
    honeypot_field: &str,
) -> UnifiedResult<Vec<(FormField, Value)>> {
    for key in values.keys() {
        if key == honeypot_field {
            continue;
        }
        if !fields.iter().any(|field| &field.key == key) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown form field \"{key}\""
            )));
        }
    }
    let mut stored: Vec<(FormField, Value)> = Vec::new();
    for field in fields {
        if !field_visible(field, values) {
            continue;
        }
        let value = values.get(&field.key);
        match value {
            None | Some(Value::Null) => {
                if field.required {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{}\" is required",
                        field.label
                    )));
                }
            }
            Some(value) => {
                if field.required && is_empty_value(value) {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{}\" is required",
                        field.label
                    )));
                }
                if !is_empty_value(value) {
                    validate_field_value(field, value)?;
                }
                stored.push((field.clone(), value.clone()));
            }
        }
    }
    Ok(stored)
}

/// Validate one submitted value against its field's rules.
fn validate_field_value(field: &FormField, value: &Value) -> UnifiedResult<()> {
    let label = &field.label;
    match field.field_type.as_str() {
        "text" | "textarea" => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!("Field \"{label}\" must be text"))
            })?;
            check_text_rules(field, text)?;
        }
        "email" => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!("Field \"{label}\" must be text"))
            })?;
            if !valid_email(text) {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a valid email address"
                )));
            }
            check_text_rules(field, text)?;
        }
        "number" => {
            let number = value.as_f64().or_else(|| {
                value
                    .as_str()
                    .and_then(|text| text.trim().parse::<f64>().ok())
            });
            let Some(number) = number else {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a number"
                )));
            };
            if !number.is_finite() {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a finite number"
                )));
            }
            if let Some(min) = field.min_value {
                if number < min {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{label}\" must be >= {min}"
                    )));
                }
            }
            if let Some(max) = field.max_value {
                if number > max {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{label}\" must be <= {max}"
                    )));
                }
            }
        }
        "select" | "radio" => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!("Field \"{label}\" must be text"))
            })?;
            if !field.options.iter().any(|option| option == text) {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be one of: {}",
                    field.options.join(", ")
                )));
            }
        }
        "multi-select" => {
            let items = value.as_array().ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a list of options"
                ))
            })?;
            for item in items {
                let text = item.as_str().ok_or_else(|| {
                    UnifiedError::bad_request(format!(
                        "Field \"{label}\" options must be text"
                    ))
                })?;
                if !field.options.iter().any(|option| option == text) {
                    return Err(UnifiedError::bad_request(format!(
                        "Field \"{label}\" must only contain: {}",
                        field.options.join(", ")
                    )));
                }
            }
        }
        "checkbox" => {
            if !value.is_boolean() {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be true or false"
                )));
            }
        }
        "date" => {
            let text = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!("Field \"{label}\" must be text"))
            })?;
            let parsed = DateTime::parse_from_rfc3339(text).is_ok()
                || NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok();
            if !parsed {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a date (YYYY-MM-DD or RFC 3339)"
                )));
            }
        }
        "file" => {
            let key = value.as_str().ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a storage key reference"
                ))
            })?;
            if key.trim().is_empty()
                || key.len() > MAX_STORAGE_KEY_LEN
                || key.chars().any(char::is_whitespace)
            {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a storage key reference (at most 512 characters, no whitespace)"
                )));
            }
        }
        "rating" => {
            let number = value.as_i64().or_else(|| {
                value
                    .as_str()
                    .and_then(|text| text.trim().parse::<i64>().ok())
            });
            let Some(number) = number else {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a whole number from 1 to 5"
                )));
            };
            if !(1..=5).contains(&number) {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be a whole number from 1 to 5"
                )));
            }
        }
        "consent" => {
            let accepted = value.as_bool().unwrap_or(false);
            if field.required && !accepted {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be accepted"
                )));
            }
            if !value.is_boolean() {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" must be true or false"
                )));
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_text_rules(field: &FormField, text: &str) -> UnifiedResult<()> {
    let label = &field.label;
    if let Some(min) = field.min_length {
        if (text.chars().count() as i64) < min {
            return Err(UnifiedError::bad_request(format!(
                "Field \"{label}\" must be at least {min} characters"
            )));
        }
    }
    if let Some(max) = field.max_length {
        if (text.chars().count() as i64) > max {
            return Err(UnifiedError::bad_request(format!(
                "Field \"{label}\" must be at most {max} characters"
            )));
        }
    }
    if let Some(pattern) = &field.pattern {
        if let Ok(regex) = Regex::new(pattern) {
            if !regex.is_match(text) {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{label}\" does not match the required format"
                )));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Rate limiting + mapping
// ---------------------------------------------------------------------------

/// Enforce the submit rate limits and record the attempt: at most
/// 10 submissions per IP per form per rolling hour, and at most
/// 200 submissions per form per rolling hour. Stale timestamps
/// (older than one hour) are pruned on every call.
fn check_rate_limit(state: &FormsState, form_id: Uuid, ip: &str) -> UnifiedResult<()> {
    let now = Utc::now();
    let hour_ago = now - chrono::Duration::hours(1);
    let ip_key = format!("ip:{form_id}:{ip}");
    let form_key = format!("form:{form_id}");
    let mut ip_attempts = state
        .attempts
        .get(&ip_key)
        .map(|entry| entry.value().clone())
        .unwrap_or_default();
    ip_attempts.retain(|instant| *instant > hour_ago);
    if ip_attempts.len() >= MAX_SUBMISSIONS_PER_IP_PER_HOUR {
        return Err(UnifiedError::new(
            "Too many submissions from this address for this form; try again later",
            429,
        ));
    }
    let mut form_attempts = state
        .attempts
        .get(&form_key)
        .map(|entry| entry.value().clone())
        .unwrap_or_default();
    form_attempts.retain(|instant| *instant > hour_ago);
    if form_attempts.len() >= MAX_SUBMISSIONS_PER_FORM_PER_HOUR {
        return Err(UnifiedError::new(
            "This form has reached its hourly submission limit; try again later",
            429,
        ));
    }
    ip_attempts.push(now);
    form_attempts.push(now);
    state.attempts.insert(ip_key, ip_attempts);
    state.attempts.insert(form_key, form_attempts);
    Ok(())
}

/// Fill a title template's `{field_key}` placeholders from the
/// stored values, then trim. Unknown keys stay untouched.
fn fill_title_template(template: &str, values: &[(FormField, Value)]) -> String {
    let mut out = template.to_string();
    for (field, value) in values {
        let placeholder = format!("{{{}}}", field.key);
        out = out.replace(&placeholder, &value_to_string(value));
    }
    out.trim().to_string()
}

/// Run the form's mapping for one submission: emit
/// `forms.mapping.requested` and read the handler's answer. Returns
/// (mapping_status, mapped_target_id, mapping_error). A missing
/// bus, an empty answer, or an explicit failure are all recorded
/// as `failed` with an error message — never silent.
fn try_mapping(
    state: &FormsState,
    form: &Form,
    submission: &FormSubmission,
    stored: &[(FormField, Value)],
) -> (String, Option<String>, Option<String>) {
    let Some(mapping) = &form.mapping else {
        return ("none".to_string(), None, None);
    };
    let Some(bus) = state.bus() else {
        return (
            "failed".to_string(),
            None,
            Some("mapping bus is unavailable".to_string()),
        );
    };
    let mut fields: Map<String, Value> = Map::new();
    for (field, value) in stored {
        if let Some(target_field) = mapping.field_map.get(&field.key) {
            fields.insert(target_field.clone(), value.clone());
        }
    }
    let title = mapping
        .title_template
        .as_deref()
        .map(|template| fill_title_template(template, stored))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| format!("Form submission: {}", form.title));
    let answer = bus.emit(
        "forms.mapping.requested",
        json!({
            "target": mapping.target,
            "form_id": form.id.to_string(),
            "submission_id": submission.id.to_string(),
            "title": title,
            "fields": fields,
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("created").and_then(Value::as_bool) == Some(true) => {
            let target_id = result
                .get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .or_else(|| {
                    result
                        .get("target_id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                });
            ("mapped".to_string(), target_id, None)
        }
        Some(result) => {
            let error = result
                .get("error")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| "mapping handler declined the submission".to_string());
            ("failed".to_string(), None, Some(error))
        }
        None => (
            "failed".to_string(),
            None,
            Some(format!(
                "no mapping handler registered for target \"{}\"",
                mapping.target
            )),
        ),
    }
}

/// Apply a mapping outcome to the stored submission and emit
/// `forms.submission.mapped` on success.
fn apply_mapping_outcome(
    state: &FormsState,
    form: &Form,
    submission: &mut FormSubmission,
    outcome: (String, Option<String>, Option<String>),
) {
    let (status, target_id, error) = outcome;
    submission.mapping_status = status.clone();
    submission.mapped_target_id = target_id.clone();
    submission.mapping_error = error;
    submission.updated_at = Utc::now();
    state.submissions.insert(submission.id, submission.clone());
    if status == "mapped" {
        state.emit(
            "forms.submission.mapped",
            json!({
                "form_id": form.id.to_string(),
                "submission_id": submission.id.to_string(),
                "target": form.mapping.as_ref().map(|mapping| mapping.target.clone()),
                "target_id": target_id,
            }),
        );
    }
}

fn submission_view(state: &FormsState, submission: &FormSubmission) -> Value {
    let mut values: Vec<FormSubmissionValue> = state
        .values
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|value| value.submission_id == submission.id)
        .collect();
    values.sort_by_key(|value| value.created_at);
    let mut view = serde_json::to_value(submission).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut view {
        map.insert(
            "values".to_string(),
            serde_json::to_value(values).unwrap_or(Value::Null),
        );
    }
    view
}

fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct FormsModule {
    state: FormsState,
}

impl FormsModule {
    pub fn new() -> Self {
        Self {
            state: FormsState::default(),
        }
    }
}

impl Default for FormsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for FormsModule {
    fn id(&self) -> &'static str {
        "forms"
    }

    fn name(&self) -> &'static str {
        "Form Builder"
    }

    fn description(&self) -> &'static str {
        "Form builder: definitions with ordered fields, validation rules and conditional visibility, embed codes, a submissions inbox with spam protection, and submission mapping to CRM leads, support tickets, or tasks."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "crm", "tickets", "tasks", "storage"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "crm", "tickets", "tasks", "storage"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, _bus: &HookBus) {}

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/forms")
                .app_data(state)
                .route("", web::get().to(list_forms))
                .route("", web::post().to(create_form))
                .route("/public/{slug}", web::get().to(get_public_form))
                .route("/public/{slug}/submit", web::post().to(submit_public_form))
                .route("/submissions/{submission_id}", web::get().to(get_submission))
                .route(
                    "/submissions/{submission_id}/read",
                    web::post().to(mark_submission_read),
                )
                .route(
                    "/submissions/{submission_id}/spam",
                    web::post().to(mark_submission_spam),
                )
                .route(
                    "/submissions/{submission_id}/remap",
                    web::post().to(remap_submission),
                )
                .route("/{id}", web::get().to(get_form))
                .route("/{id}", web::patch().to(update_form))
                .route("/{id}", web::delete().to(delete_form))
                .route("/{id}/embed", web::get().to(get_embed_code))
                .route("/{id}/fields", web::get().to(list_fields))
                .route("/{id}/fields", web::post().to(create_field))
                .route("/{id}/fields/reorder", web::post().to(reorder_fields))
                .route("/{id}/fields/{field_id}", web::get().to(get_field))
                .route("/{id}/fields/{field_id}", web::patch().to(update_field))
                .route("/{id}/fields/{field_id}", web::delete().to(delete_field))
                .route("/{id}/submissions", web::get().to(list_submissions))
                .route("/{id}/submissions/export", web::get().to(export_submissions)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/forms", owner: "forms", description: "List forms (paginated, filter by status, company_id, search)." },
            RouteInfo { method: "POST", path: "/api/v1/forms", owner: "forms", description: "Create a form with its fields (ordered, validated, conditional visibility)." },
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}", owner: "forms", description: "Get one form with its fields." },
            RouteInfo { method: "PATCH", path: "/api/v1/forms/{id}", owner: "forms", description: "Update a form (title, status, labels, honeypot field, mapping)." },
            RouteInfo { method: "DELETE", path: "/api/v1/forms/{id}", owner: "forms", description: "Delete a form with its fields and submissions." },
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}/embed", owner: "forms", description: "Embed code for a form (render + submit URLs, script and iframe snippets)." },
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}/fields", owner: "forms", description: "List fields of a form (position order)." },
            RouteInfo { method: "POST", path: "/api/v1/forms/{id}/fields", owner: "forms", description: "Add a field to a form (validated definition)." },
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}/fields/{field_id}", owner: "forms", description: "Get one field." },
            RouteInfo { method: "PATCH", path: "/api/v1/forms/{id}/fields/{field_id}", owner: "forms", description: "Update a field definition (rules re-validated)." },
            RouteInfo { method: "DELETE", path: "/api/v1/forms/{id}/fields/{field_id}", owner: "forms", description: "Delete a field with its submitted values." },
            RouteInfo { method: "POST", path: "/api/v1/forms/{id}/fields/reorder", owner: "forms", description: "Reorder fields by id list (drag-drop order)." },
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}/submissions", owner: "forms", description: "Submissions inbox (paginated, filter by status, mapping_status, search)." },
            RouteInfo { method: "GET", path: "/api/v1/forms/{id}/submissions/export", owner: "forms", description: "Export submissions as CSV or JSON (?format=csv|json)." },
            RouteInfo { method: "GET", path: "/api/v1/forms/submissions/{submission_id}", owner: "forms", description: "Get one submission with its values." },
            RouteInfo { method: "POST", path: "/api/v1/forms/submissions/{submission_id}/read", owner: "forms", description: "Mark a submission read." },
            RouteInfo { method: "POST", path: "/api/v1/forms/submissions/{submission_id}/spam", owner: "forms", description: "Mark a submission spam." },
            RouteInfo { method: "POST", path: "/api/v1/forms/submissions/{submission_id}/remap", owner: "forms", description: "Retry the submission mapping (only when the mapping failed)." },
            RouteInfo { method: "GET", path: "/api/v1/forms/public/{slug}", owner: "forms", description: "PUBLIC: render a published form definition as JSON for embedding (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "POST", path: "/api/v1/forms/public/{slug}/submit", owner: "forms", description: "PUBLIC: submit a published form (honeypot + rate limits + server-side validation + mapping; no auth; gated by the API Manager ApiAccessRule)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(FormsModule::new()));
}

// ---------------------------------------------------------------------------
// Form handlers
// ---------------------------------------------------------------------------

fn form_view(state: &FormsState, form: &Form) -> Value {
    let fields = fields_for(state, form.id);
    let mut view = serde_json::to_value(form).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut view {
        map.insert(
            "fields".to_string(),
            serde_json::to_value(fields).unwrap_or(Value::Null),
        );
    }
    view
}

async fn list_forms(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Form> = state
            .forms
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|form| {
                if let Some(status) = &status_filter {
                    if form.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if form.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        form.title,
                        form.description.clone().unwrap_or_default(),
                        form.slug
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "slug", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.cmp(&b.title),
                "slug" => a.slug.cmp(&b.slug),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    body: web::Json<CreateFormRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let slug = match &input.slug {
            Some(raw) => {
                let slug = raw.trim().to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 128 characters)",
                    ));
                }
                slug
            }
            None => slugify(&title),
        };
        if slug_in_use(&state, &slug, None) {
            return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
        }
        if input.fields.len() > MAX_FIELDS_PER_FORM {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_FIELDS_PER_FORM} fields are allowed per form"
            )));
        }
        let company_id = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", 128)?),
            None => None,
        };
        let submit_button_label = match &input.submit_button_label {
            Some(raw) => require_text(raw, "submit_button_label", MAX_BUTTON_LABEL_LEN)?,
            None => default_submit_label(),
        };
        let success_message = match &input.success_message {
            Some(raw) => require_text(raw, "success_message", MAX_SUCCESS_MESSAGE_LEN)?,
            None => default_success_message(),
        };
        let honeypot_field = match &input.honeypot_field {
            Some(raw) => {
                let field = raw.trim().to_string();
                if !valid_key(&field) {
                    return Err(UnifiedError::bad_request(
                        "honeypot_field must be a field key ([a-z0-9_]+, max 64 characters)",
                    ));
                }
                field
            }
            None => default_honeypot_field(),
        };
        let mapping = match &input.mapping {
            Some(raw) => Some(validate_mapping(raw)?),
            None => None,
        };
        // Validate every field definition before storing anything:
        // a condition may reference any key of the same batch.
        let batch_keys: Vec<String> = input
            .fields
            .iter()
            .map(|field| field.key.trim().to_string())
            .collect();
        let mut seen_keys: Vec<String> = Vec::new();
        for field in &input.fields {
            if seen_keys.contains(&field.key) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate field key \"{}\"",
                    field.key
                )));
            }
            seen_keys.push(field.key.clone());
            validate_field_definition(field, &batch_keys)?;
        }
        if batch_keys.contains(&honeypot_field) {
            return Err(UnifiedError::bad_request(
                "honeypot_field must not collide with a real field key",
            ));
        }
        let now = Utc::now();
        let form = Form {
            id: Uuid::now_v7(),
            title,
            description,
            slug: slug.clone(),
            status: input.status.unwrap_or_default(),
            company_id,
            owner_id: caller.actor.clone(),
            submit_button_label,
            success_message,
            honeypot_field,
            mapping,
            created_at: now,
            updated_at: now,
        };
        state.forms.insert(form.id, form.clone());
        for (index, field_input) in input.fields.iter().enumerate() {
            let mut field = build_field(&state, form.id, field_input, now);
            if field_input.position.is_none() {
                field.position = index as i64;
            }
            state.fields.insert(field.id, field);
        }
        state.emit(
            "forms.created",
            json!({
                "form_id": form.id.to_string(),
                "slug": form.slug,
                "title": form.title,
                "status": form.status.as_str(),
            }),
        );
        Ok(form_view(&state, &form))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

async fn get_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let form = load_form(&state, &path.into_inner())?;
        Ok(form_view(&state, &form))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFormRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let mut form = load_form(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            form.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            form.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(slug) = &input.slug {
            let slug = slug.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only",
                ));
            }
            if slug_in_use(&state, &slug, Some(form.id)) {
                return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
            }
            form.slug = slug;
        }
        if let Some(status) = input.status {
            form.status = status;
        }
        if let Some(company_id) = &input.company_id {
            form.company_id = match company_id {
                Some(raw) => Some(require_text(raw, "company_id", 128)?),
                None => None,
            };
        }
        if let Some(label) = &input.submit_button_label {
            form.submit_button_label =
                require_text(label, "submit_button_label", MAX_BUTTON_LABEL_LEN)?;
        }
        if let Some(message) = &input.success_message {
            form.success_message =
                require_text(message, "success_message", MAX_SUCCESS_MESSAGE_LEN)?;
        }
        if let Some(honeypot) = &input.honeypot_field {
            let field = honeypot.trim().to_string();
            if !valid_key(&field) {
                return Err(UnifiedError::bad_request(
                    "honeypot_field must be a field key ([a-z0-9_]+, max 64 characters)",
                ));
            }
            if fields_for(&state, form.id)
                .iter()
                .any(|entry| entry.key == field)
            {
                return Err(UnifiedError::bad_request(
                    "honeypot_field must not collide with a real field key",
                ));
            }
            form.honeypot_field = field;
        }
        if let Some(mapping) = &input.mapping {
            form.mapping = match mapping {
                Some(raw) => Some(validate_mapping(raw)?),
                None => None,
            };
        }
        form.updated_at = Utc::now();
        state.forms.insert(form.id, form.clone());
        state.emit(
            "forms.updated",
            json!({
                "form_id": form.id.to_string(),
                "slug": form.slug,
                "status": form.status.as_str(),
            }),
        );
        Ok(form_view(&state, &form))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn delete_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let form = load_form(&state, &path.into_inner())?;
        let id = form.id;
        state.forms.remove(&id);
        let stale_fields: Vec<Uuid> = state
            .fields
            .iter()
            .filter(|entry| entry.value().form_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_fields {
            state.fields.remove(&key);
        }
        let stale_submissions: Vec<Uuid> = state
            .submissions
            .iter()
            .filter(|entry| entry.value().form_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in &stale_submissions {
            state.submissions.remove(key);
        }
        let stale_values: Vec<Uuid> = state
            .values
            .iter()
            .filter(|entry| stale_submissions.contains(&entry.value().submission_id))
            .map(|entry| *entry.key())
            .collect();
        for key in stale_values {
            state.values.remove(&key);
        }
        state.emit(
            "forms.deleted",
            json!({ "form_id": id.to_string(), "slug": form.slug }),
        );
        Ok(json!({ "deleted": true, "form_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_embed_code(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let form = load_form(&state, &path.into_inner())?;
        let render_url = format!("/api/v1/forms/public/{}", form.slug);
        let submit_url = format!("/api/v1/forms/public/{}/submit", form.slug);
        Ok(json!({
            "form_id": form.id.to_string(),
            "slug": form.slug,
            "render_url": render_url,
            "submit_url": submit_url,
            "script_tag": format!(
                "<script src=\"{render_url}\" data-form-submit=\"{submit_url}\"></script>",
                render_url = render_url,
                submit_url = submit_url,
            ),
            "iframe_tag": format!(
                "<iframe src=\"{render_url}\" title=\"{title}\" width=\"100%\" height=\"600\" frameborder=\"0\"></iframe>",
                render_url = render_url,
                title = form.title,
            ),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Field handlers
// ---------------------------------------------------------------------------

async fn list_fields(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let form = load_form(&state, &path.into_inner())?;
        let params = query.into_inner();
        let items = fields_for(&state, form.id);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_field(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateFieldRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FormField> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let form = load_form(&state, &path.into_inner())?;
        let input = body.into_inner();
        if fields_for(&state, form.id).len() >= MAX_FIELDS_PER_FORM {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_FIELDS_PER_FORM} fields are allowed per form"
            )));
        }
        let known_keys: Vec<String> = fields_for(&state, form.id)
            .iter()
            .map(|field| field.key.clone())
            .collect();
        validate_field_definition(&input, &known_keys)?;
        if key_in_use(&state, form.id, input.key.trim(), None) {
            return Err(UnifiedError::bad_request(format!(
                "field key is already in use: {}",
                input.key.trim()
            )));
        }
        if input.key.trim() == form.honeypot_field {
            return Err(UnifiedError::bad_request(
                "field key must not collide with the honeypot field",
            ));
        }
        let now = Utc::now();
        let mut field = build_field(&state, form.id, &input, now);
        if input.position.is_some() {
            shift_fields_for_insert(&state, form.id, field.position);
        }
        state.fields.insert(field.id, field.clone());
        state.forms.insert(form.id, {
            let mut updated = form.clone();
            updated.updated_at = now;
            updated
        });
        Ok(field)
    })();
    match result {
        Ok(field) => created(field),
        Err(error) => err(error, debug),
    }
}

async fn get_field(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FormField> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let (form_id, field_id) = path.into_inner();
        let form = load_form(&state, &form_id)?;
        load_field(&state, form.id, &field_id)
    })();
    match result {
        Ok(field) => ok(field),
        Err(error) => err(error, debug),
    }
}

async fn update_field(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateFieldRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FormField> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let (form_id, field_id) = path.into_inner();
        let form = load_form(&state, &form_id)?;
        let mut field = load_field(&state, form.id, &field_id)?;
        let input = body.into_inner();
        // Rebuild a create-shaped definition from the merged values
        // and validate it as a whole, so an update can never leave
        // a field in a state a fresh create would reject.
        let mut merged = CreateFieldRequest {
            key: field.key.clone(),
            label: field.label.clone(),
            field_type: field.field_type.clone(),
            position: Some(field.position),
            required: field.required,
            placeholder: field.placeholder.clone(),
            help_text: field.help_text.clone(),
            options: field.options.clone(),
            min_length: field.min_length,
            max_length: field.max_length,
            min_value: field.min_value,
            max_value: field.max_value,
            pattern: field.pattern.clone(),
            show_when: field.show_when.clone(),
        };
        if let Some(key) = &input.key {
            merged.key = key.trim().to_string();
        }
        if let Some(label) = &input.label {
            merged.label = label.clone();
        }
        if let Some(field_type) = &input.field_type {
            merged.field_type = field_type.clone();
        }
        if let Some(position) = input.position {
            merged.position = Some(position);
        }
        if let Some(required) = input.required {
            merged.required = required;
        }
        if let Some(placeholder) = &input.placeholder {
            merged.placeholder = placeholder.clone();
        }
        if let Some(help) = &input.help_text {
            merged.help_text = help.clone();
        }
        if let Some(options) = &input.options {
            merged.options = options.clone();
        }
        if let Some(min_length) = &input.min_length {
            merged.min_length = *min_length;
        }
        if let Some(max_length) = &input.max_length {
            merged.max_length = *max_length;
        }
        if let Some(min_value) = &input.min_value {
            merged.min_value = *min_value;
        }
        if let Some(max_value) = &input.max_value {
            merged.max_value = *max_value;
        }
        if let Some(pattern) = &input.pattern {
            merged.pattern = pattern.clone();
        }
        if let Some(show_when) = &input.show_when {
            merged.show_when = show_when.clone();
        }
        let known_keys: Vec<String> = fields_for(&state, form.id)
            .iter()
            .map(|entry| entry.key.clone())
            .collect();
        validate_field_definition(&merged, &known_keys)?;
        if merged.key != field.key && key_in_use(&state, form.id, &merged.key, Some(field.id)) {
            return Err(UnifiedError::bad_request(format!(
                "field key is already in use: {}",
                merged.key
            )));
        }
        if merged.key == form.honeypot_field {
            return Err(UnifiedError::bad_request(
                "field key must not collide with the honeypot field",
            ));
        }
        let now = Utc::now();
        field.key = merged.key;
        field.label = merged.label.trim().to_string();
        field.field_type = merged.field_type;
        field.required = merged.required;
        field.placeholder = merged.placeholder;
        field.help_text = merged.help_text;
        field.options = merged.options;
        field.min_length = merged.min_length;
        field.max_length = merged.max_length;
        field.min_value = merged.min_value;
        field.max_value = merged.max_value;
        field.pattern = merged.pattern;
        field.show_when = merged.show_when;
        if let Some(position) = merged.position {
            field.position = position;
        }
        field.updated_at = now;
        state.fields.insert(field.id, field.clone());
        state.forms.insert(form.id, {
            let mut updated = form.clone();
            updated.updated_at = now;
            updated
        });
        Ok(field)
    })();
    match result {
        Ok(field) => ok(field),
        Err(error) => err(error, debug),
    }
}

async fn delete_field(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let (form_id, field_id) = path.into_inner();
        let form = load_form(&state, &form_id)?;
        let field = load_field(&state, form.id, &field_id)?;
        state.fields.remove(&field.id);
        let stale_values: Vec<Uuid> = state
            .values
            .iter()
            .filter(|entry| entry.value().field_id == field.id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_values {
            state.values.remove(&key);
        }
        Ok(json!({ "deleted": true, "field_id": field.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn reorder_fields(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReorderFieldsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<FormField>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let form = load_form(&state, &path.into_inner())?;
        let input = body.into_inner();
        let current = fields_for(&state, form.id);
        if input.field_ids.len() != current.len() {
            return Err(UnifiedError::bad_request(
                "field_ids must list every field of the form exactly once",
            ));
        }
        let mut seen: Vec<Uuid> = Vec::new();
        for id in &input.field_ids {
            if seen.contains(id) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate field id in reorder list: {id}"
                )));
            }
            seen.push(*id);
            if !current.iter().any(|field| field.id == *id) {
                return Err(UnifiedError::bad_request(format!(
                    "Field id does not belong to this form: {id}"
                )));
            }
        }
        let now = Utc::now();
        let mut updated: Vec<FormField> = Vec::new();
        for (position, id) in input.field_ids.iter().enumerate() {
            if let Some(mut field) = state.fields.get_mut(id) {
                field.position = position as i64;
                field.updated_at = now;
                updated.push(field.value().clone());
            }
        }
        updated.sort_by_key(|field| field.position);
        Ok(updated)
    })();
    match result {
        Ok(fields) => ok(fields),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Submission handlers
// ---------------------------------------------------------------------------

async fn list_submissions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let form = load_form(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mapping_filter = params.filters.get("mapping_status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .submissions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|submission| submission.form_id == form.id)
            .filter(|submission| {
                if let Some(status) = &status_filter {
                    if &submission.status != status {
                        return false;
                    }
                }
                if let Some(mapping) = &mapping_filter {
                    if &submission.mapping_status != mapping {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let view = submission_view(&state, submission);
                    if !view.to_string().to_lowercase().contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|submission| submission_view(&state, &submission))
            .collect();
        items.sort_by(|a, b| {
            let a_created = a.get("created_at").and_then(Value::as_str).unwrap_or("");
            let b_created = b.get("created_at").and_then(Value::as_str).unwrap_or("");
            b_created.cmp(a_created)
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn export_submissions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let format = query.into_inner().format.to_lowercase();
    if format != "csv" && format != "json" {
        return err(
            UnifiedError::bad_request("format must be \"csv\" or \"json\""),
            debug,
        );
    }
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let form = load_form(&state, &path.into_inner())?;
        let fields = fields_for(&state, form.id);
        let mut submissions: Vec<FormSubmission> = state
            .submissions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|submission| submission.form_id == form.id)
            .collect();
        submissions.sort_by_key(|submission| submission.created_at);
        if format == "json" {
            let items: Vec<Value> = submissions
                .iter()
                .map(|submission| submission_view(&state, submission))
                .collect();
            return Ok(json!({ "form_id": form.id.to_string(), "submissions": items }));
        }
        let mut out = String::from("id,status,mapping_status,created_at");
        for field in &fields {
            out.push(',');
            out.push_str(&field.key);
        }
        out.push_str("\r\n");
        for submission in &submissions {
            let view = submission_view(&state, submission);
            let values = view
                .get("values")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut cells: Vec<String> = vec![
                submission.id.to_string(),
                submission.status.clone(),
                submission.mapping_status.clone(),
                submission.created_at.to_rfc3339(),
            ];
            for field in &fields {
                let cell = values
                    .iter()
                    .find(|value| {
                        value.get("field_key").and_then(Value::as_str) == Some(field.key.as_str())
                    })
                    .and_then(|value| value.get("value"))
                    .map(value_to_string)
                    .unwrap_or_default();
                cells.push(escape_csv_field(&cell));
            }
            out.push_str(&cells.join(","));
            out.push_str("\r\n");
        }
        Ok(json!({ "form_id": form.id.to_string(), "format": "csv", "csv": out }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn load_submission(state: &FormsState, raw_id: &str) -> UnifiedResult<FormSubmission> {
    let id = parse_uuid(raw_id, "submission")?;
    state
        .submissions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Submission not found: {id}")))
}

async fn get_submission(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_VIEW)?;
        let submission = load_submission(&state, &path.into_inner())?;
        Ok(submission_view(&state, &submission))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn transition_submission_status(
    state: &FormsState,
    submission: &FormSubmission,
    to: &str,
) -> FormSubmission {
    let mut updated = submission.clone();
    updated.status = to.to_string();
    updated.updated_at = Utc::now();
    state.submissions.insert(updated.id, updated.clone());
    updated
}

async fn mark_submission_read(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let submission = load_submission(&state, &path.into_inner())?;
        if submission.status == "spam" {
            return Err(UnifiedError::conflict(
                "A spam submission cannot be marked read; it stays spam",
            ));
        }
        let updated = transition_submission_status(&state, &submission, "read");
        Ok(submission_view(&state, &updated))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn mark_submission_spam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let submission = load_submission(&state, &path.into_inner())?;
        let updated = transition_submission_status(&state, &submission, "spam");
        Ok(submission_view(&state, &updated))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn remap_submission(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORMS_MANAGE)?;
        let mut submission = load_submission(&state, &path.into_inner())?;
        let form = state
            .forms
            .get(&submission.form_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Form not found: {}", submission.form_id)))?;
        if form.mapping.is_none() {
            return Err(UnifiedError::conflict(
                "This form has no mapping configured; add one before retrying",
            ));
        }
        if submission.mapping_status == "mapped" {
            return Err(UnifiedError::conflict(
                "This submission is already mapped",
            ));
        }
        let mut stored: Vec<(FormField, Value)> = Vec::new();
        for value in state.values.iter() {
            if value.value().submission_id != submission.id {
                continue;
            }
            if let Some(field) = state.fields.get(&value.value().field_id) {
                stored.push((field.value().clone(), value.value().value.clone()));
            }
        }
        stored.sort_by_key(|(field, _)| (field.position, field.created_at));
        let outcome = try_mapping(&state, &form, &submission, &stored);
        apply_mapping_outcome(&state, &form, &mut submission, outcome);
        Ok(submission_view(&state, &submission))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public render + submit
// ---------------------------------------------------------------------------

async fn get_public_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    path: web::Path<String>,
) -> HttpResponse {
    // No capability check here on purpose: this is the public form
    // render for embedding. The API Manager gate (ApiAccessRule
    // with public_access for this path, default deny) is what makes
    // it public — see the module header.
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let form = load_form_by_slug(&state, &path.into_inner())?;
        if form.status != FormStatus::Published {
            return Err(UnifiedError::not_found(format!(
                "Form not found: {}",
                form.slug
            )));
        }
        Ok(form_view(&state, &form))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn submit_public_form(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FormsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SubmitFormRequest>,
) -> HttpResponse {
    // No capability check here on purpose: this is the public
    // submit endpoint. It is protected by the API Manager gate
    // (default deny), the honeypot, and the rate limits instead —
    // see the module header.
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let slug = path.into_inner();
        let form = load_form_by_slug(&state, &slug)?;
        if form.status != FormStatus::Published {
            return Err(UnifiedError::not_found(format!(
                "Form not found: {}",
                form.slug
            )));
        }
        let input = body.into_inner();
        let ip = client_ip(&req).unwrap_or_else(|| "unknown".to_string());
        check_rate_limit(&state, form.id, &ip)?;
        let user_agent = req
            .headers()
            .get("user-agent")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let submitted_by = req
            .headers()
            .get("x-user-id")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        // Honeypot: a filled honeypot field means a bot. The
        // submission is stored as spam (so the inbox shows it) and
        // the caller gets the same generic success message as a
        // real submission, so bots cannot probe the filter.
        let honeypot_filled = input
            .values
            .get(&form.honeypot_field)
            .map(|value| !is_empty_value(value))
            .unwrap_or(false)
            || input
                .honeypot
                .as_deref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false);
        let now = Utc::now();
        if honeypot_filled {
            let submission = FormSubmission {
                id: Uuid::now_v7(),
                form_id: form.id,
                status: "spam".to_string(),
                mapping_status: "none".to_string(),
                mapping_error: Some("honeypot field was filled".to_string()),
                mapped_target_id: None,
                ip_address: Some(ip),
                user_agent,
                submitted_by,
                created_at: now,
                updated_at: now,
            };
            state.submissions.insert(submission.id, submission);
            return Ok(json!({
                "received": true,
                "message": form.success_message,
            }));
        }
        let fields = fields_for(&state, form.id);
        let stored = validate_submission_values(&fields, &input.values, &form.honeypot_field)?;
        let mut submission = FormSubmission {
            id: Uuid::now_v7(),
            form_id: form.id,
            status: "new".to_string(),
            mapping_status: if form.mapping.is_some() {
                "failed".to_string()
            } else {
                "none".to_string()
            },
            mapping_error: None,
            mapped_target_id: None,
            ip_address: Some(ip),
            user_agent,
            submitted_by,
            created_at: now,
            updated_at: now,
        };
        state.submissions.insert(submission.id, submission.clone());
        for (field, value) in &stored {
            let row = FormSubmissionValue {
                id: Uuid::now_v7(),
                submission_id: submission.id,
                field_id: field.id,
                field_key: field.key.clone(),
                value: value.clone(),
                file_storage_key: if field.field_type == "file" {
                    value.as_str().map(str::to_string)
                } else {
                    None
                },
                created_at: now,
            };
            state.values.insert(row.id, row);
        }
        state.emit(
            "forms.submission.received",
            json!({
                "form_id": form.id.to_string(),
                "submission_id": submission.id.to_string(),
                "slug": form.slug,
            }),
        );
        if form.mapping.is_some() {
            let outcome = try_mapping(&state, &form, &submission, &stored);
            apply_mapping_outcome(&state, &form, &mut submission, outcome);
        }
        Ok(json!({
            "received": true,
            "submission_id": submission.id.to_string(),
            "mapping_status": submission.mapping_status,
            "message": form.success_message,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
