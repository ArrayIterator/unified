// surveys module - Request bodies.
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

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateSurveyRequest {
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSurveyRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateQuestionRequest {
    pub question_type: String,
    pub text: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateQuestionRequest {
    pub text: Option<String>,
    pub required: Option<bool>,
    pub options: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDistributionRequest {
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
    pub expires_in_days: Option<i64>,
    /// Recipients for a `manual` distribution: `{recipient_id,
    /// recipient_email?, agent_id?, department_id?, product_id?}`.
    #[serde(default)]
    pub recipients: Vec<DistributionRecipient>,
}

#[derive(Debug, Deserialize)]
pub struct DistributionRecipient {
    pub recipient_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SendDistributionRequest {
    /// Extra recipients to invite on a manual send (merged with
    /// the distribution's pending recipients). Ignored for
    /// trigger distributions.
    #[serde(default)]
    pub recipients: Vec<DistributionRecipient>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitResponseRequest {
    pub answers: Vec<SubmitAnswer>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitAnswer {
    pub question_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
}
