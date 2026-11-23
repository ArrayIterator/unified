// Elearning — Request bodies.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateCourseRequest {
    pub title: String,
    pub description: String,
    pub category: String,
    pub level: String,
    pub language: Option<String>,
    pub thumbnail_ref: Option<String>,
    pub price_minor: Option<i64>,
    pub currency: Option<String>,
    pub certificate_enabled: Option<bool>,
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCourseRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub level: Option<String>,
    pub language: Option<String>,
    pub thumbnail_ref: Option<Option<String>>,
    pub price_minor: Option<i64>,
    pub currency: Option<String>,
    pub certificate_enabled: Option<bool>,
    pub company_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateModuleRequest {
    pub title: String,
    pub description: Option<String>,
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateModuleRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateLessonRequest {
    pub title: String,
    pub lesson_type: String,
    pub content_ref: Option<String>,
    pub body: Option<String>,
    pub position: Option<i64>,
    pub required: Option<bool>,
    pub available_from: Option<DateTime<Utc>>,
    pub requires_previous: Option<bool>,
    pub duration_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateLessonRequest {
    pub title: Option<String>,
    pub lesson_type: Option<String>,
    pub content_ref: Option<Option<String>>,
    pub body: Option<Option<String>>,
    pub position: Option<i64>,
    pub required: Option<bool>,
    pub available_from: Option<Option<DateTime<Utc>>>,
    pub requires_previous: Option<bool>,
    pub duration_minutes: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct EnrollRequest {
    /// Display name recorded on the certificate when one is issued.
    pub learner_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateQuizRequest {
    pub title: String,
    pub description: Option<String>,
    pub pass_mark: Option<i64>,
    pub max_attempts: Option<Option<i64>>,
    pub allow_review: Option<bool>,
    pub time_limit_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateQuizRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub pass_mark: Option<i64>,
    pub max_attempts: Option<Option<i64>>,
    pub allow_review: Option<bool>,
    pub time_limit_minutes: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateQuestionRequest {
    pub question: String,
    pub options: Vec<String>,
    pub correct_index: usize,
    pub points: Option<i64>,
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateQuestionRequest {
    pub question: Option<String>,
    pub options: Option<Vec<String>>,
    pub correct_index: Option<usize>,
    pub points: Option<i64>,
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitAttemptRequest {
    pub answers: Vec<AttemptAnswer>,
}

#[derive(Debug, Deserialize)]
pub struct CreateReviewRequest {
    pub rating: i64,
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateReviewRequest {
    pub rating: Option<i64>,
    pub comment: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePathRequest {
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePathRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct AddPathCourseRequest {
    pub course_id: String,
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub levels: Option<Vec<String>>,
    pub categories: Option<Vec<String>>,
    pub languages: Option<Vec<String>>,
    pub lesson_types: Option<Vec<String>>,
    pub video_lesson_types: Option<Vec<String>>,
    pub text_lesson_types: Option<Vec<String>>,
    pub default_pass_mark: Option<i64>,
    pub default_max_attempts: Option<Option<i64>>,
    pub default_currency: Option<String>,
    pub certificate_issuer_name: Option<String>,
    pub certificate_anchoring_enabled: Option<bool>,
    pub anchoring_network_id: Option<String>,
}
