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

// forms — types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::store::*;
use super::common::*;
use super::validation::*;
use super::submission::*;
use super::module::*;
use super::handlers_forms::*;
use super::handlers_fields::*;
use super::handlers_submissions::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_SLUG_LEN: usize = 128;

pub(crate) const MAX_LABEL_LEN: usize = 200;

pub(crate) const MAX_KEY_LEN: usize = 64;

pub(crate) const MAX_PLACEHOLDER_LEN: usize = 200;

pub(crate) const MAX_HELP_LEN: usize = 500;

pub(crate) const MAX_OPTION_LEN: usize = 128;

pub(crate) const MAX_OPTIONS: usize = 50;

pub(crate) const MAX_FIELDS_PER_FORM: usize = 100;

pub(crate) const MAX_PATTERN_LEN: usize = 500;

pub(crate) const MAX_STORAGE_KEY_LEN: usize = 512;

pub(crate) const MAX_TITLE_TEMPLATE_LEN: usize = 500;

pub(crate) const MAX_SUCCESS_MESSAGE_LEN: usize = 1000;

pub(crate) const MAX_BUTTON_LABEL_LEN: usize = 80;

pub(crate) const MAX_HONEYPOT_LEN: usize = 64;

/// Spam protection: at most 10 submissions per IP per form per
/// rolling hour, and at most 200 submissions per form per rolling
/// hour (all IPs together).
pub(crate) const MAX_SUBMISSIONS_PER_IP_PER_HOUR: usize = 10;

pub(crate) const MAX_SUBMISSIONS_PER_FORM_PER_HOUR: usize = 200;

/// Field types a form field may have.
pub(crate) const FIELD_TYPES: [&str; 12] = [
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
pub(crate) const MAPPING_TARGETS: [&str; 3] = ["crm_lead", "support_ticket", "task"];

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_FORMS_VIEW: &str = "forms.view";

pub(crate) const PERM_FORMS_MANAGE: &str = "forms.manage";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle of a form. Allowed transitions (enforced by
/// `update_form`; a move to the same status is a no-op):
/// `draft -> published`, `draft -> archived`,
/// `published -> archived`, `archived -> draft` (explicit
/// republish step: an archived form is never published directly).
/// `archived` hides the form from public render and submit.
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
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::Archived => "archived",
        }
    }

    /// True when a form may move from `from` to `to`. Same-status
    /// moves are no-ops and always allowed.
    pub(crate) fn transition_allowed(from: Self, to: Self) -> bool {
        if from == to {
            return true;
        }
        matches!(
            (from, to),
            (Self::Draft, Self::Published)
                | (Self::Draft, Self::Archived)
                | (Self::Published, Self::Archived)
                | (Self::Archived, Self::Draft)
        )
    }
}

/// Conditional visibility: this field is shown only when the field
/// named `field_key` currently equals `equals` (compared as
/// strings). For a multi-select (array) value the field shows when
/// any selected option equals `equals`. Hidden fields are skipped
/// by submit validation and never stored.
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

pub(crate) fn default_submit_label() -> String {
    "Submit".to_string()
}

pub(crate) fn default_success_message() -> String {
    "Thank you for your submission.".to_string()
}

pub(crate) fn default_honeypot_field() -> String {
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

pub(crate) fn default_export_format() -> String {
    "json".to_string()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_status_transitions_follow_the_table() {
        // Allowed moves (plus same-status no-ops).
        assert!(FormStatus::transition_allowed(
            FormStatus::Draft,
            FormStatus::Published
        ));
        assert!(FormStatus::transition_allowed(
            FormStatus::Draft,
            FormStatus::Archived
        ));
        assert!(FormStatus::transition_allowed(
            FormStatus::Published,
            FormStatus::Archived
        ));
        assert!(FormStatus::transition_allowed(
            FormStatus::Archived,
            FormStatus::Draft
        ));
        assert!(FormStatus::transition_allowed(
            FormStatus::Published,
            FormStatus::Published
        ));
        assert!(FormStatus::transition_allowed(
            FormStatus::Archived,
            FormStatus::Archived
        ));
        // Rejected moves: archived is never published directly and
        // published never returns to draft.
        assert!(!FormStatus::transition_allowed(
            FormStatus::Archived,
            FormStatus::Published
        ));
        assert!(!FormStatus::transition_allowed(
            FormStatus::Published,
            FormStatus::Draft
        ));
    }
}
