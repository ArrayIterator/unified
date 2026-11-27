// surveys module - Domain types.
//
// Split from the former single-file module `src/modules/surveys.rs`.
// The module root (`surveys.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Datelike, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::helpers::created;

// Types

/// One survey version. A survey family shares `family_id` across
/// versions: version 1 has `family_id == id`; editing a published
/// survey creates a new row with `version + 1`,
/// `supersedes = Some(old id)`, and status `draft`. Publishing a
/// version archives the previously published version of the same
/// family. Responses always reference the exact version row they
/// answered (`survey_id`), never the family.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Survey {
    pub id: Uuid,
    pub family_id: Uuid,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Uuid>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One question of one survey version. `options` applies to
/// `multiple_choice` only. `min_value` / `max_value` record the
/// score bounds the question was created with (NPS 0-10, CSAT and
/// rating 1-5); answers outside the bounds are rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurveyQuestion {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub position: u32,
    pub question_type: String,
    pub text: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_value: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_value: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One distribution of one survey version: how invitations reach
/// recipients. `trigger` is `manual` (explicit recipient list),
/// `ticket_closed` (ticket close events), `purchase` (completed
/// orders), or `scheduled` (scheduler job at `scheduled_at`).
/// `department_id` / `product_id` narrow the trigger filters.
/// `responsible_user_id` receives low-score follow-up notices for
/// responses from this distribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurveyDistribution {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responsible_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<DateTime<Utc>>,
    /// Token lifetime for invitations from this distribution, in
    /// days (default 30).
    pub expires_in_days: i64,
    /// `pending`, `sent`, `completed`, `cancelled`.
    pub status: String,
    /// `not_scheduled`, `queued`, `declined`, or `failed` (a
    /// scheduled send that errored; see the scheduler note in the
    /// module header; only meaningful for `scheduled`
    /// distributions).
    pub scheduler_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_job_id: Option<String>,
    pub sent_count: i64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One response invitation / submitted response. One row per
/// recipient per survey version: the module refuses a second
/// invitation for the same recipient on the same version.
/// `token` is the unguessable response link token (32 random
/// bytes, base64url); `submitted_at` is `None` until the recipient
/// answers. `follow_up_status` is `none`, `requested`, or
/// `created`; `follow_up_task_id` holds the task id when the tasks
/// module answered `task.create_requested`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurveyResponse {
    pub id: Uuid,
    pub survey_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution_id: Option<Uuid>,
    pub recipient_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub token: String,
    pub token_expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<DateTime<Utc>>,
    pub follow_up_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_task_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One answer inside a response. Exactly one of `score`, `text`,
/// or `choice` is set, matching the question type: `score` for
/// `nps` / `csat` / `rating`, `text` for `text`, `choice` for
/// `multiple_choice`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurveyAnswer {
    pub id: Uuid,
    pub response_id: Uuid,
    pub question_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    pub created_at: DateTime<Utc>,
}
