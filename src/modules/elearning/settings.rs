// Elearning - Configurable settings (admin-editable; defaults are defaults only).

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

// Configurable settings (admin-editable; defaults are defaults only)

/// All behaviour-shaping lists and defaults of this module. An
/// administrator edits these through `PUT /settings`; every
/// validation and default in this file reads the live value, so
/// changing a list here changes what the module accepts without
/// any code change. The values below are CONFIGURABLE DEFAULTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElearningSettings {
    /// Allowed course levels (for example beginner / intermediate).
    pub levels: Vec<String>,
    /// Allowed course categories.
    pub categories: Vec<String>,
    /// Allowed course languages, as language tags.
    pub languages: Vec<String>,
    /// Allowed lesson types.
    pub lesson_types: Vec<String>,
    /// Lesson types that need a storage reference (`content_ref`),
    /// such as video and file lessons.
    #[serde(default)]
    pub video_lesson_types: Vec<String>,
    /// Lesson types that need a text body, such as text lessons.
    #[serde(default)]
    pub text_lesson_types: Vec<String>,
    /// Pass mark in percent (0-100) used when a quiz does not set one.
    pub default_pass_mark: i64,
    /// Attempt limit used when a quiz does not set one; None means
    /// unlimited attempts.
    pub default_max_attempts: Option<i64>,
    /// Default currency for paid courses (ISO code).
    pub default_currency: String,
    /// Name printed on certificates as the issuer.
    pub certificate_issuer_name: String,
    /// Whether issued certificates also request tamper-evidence
    /// anchoring through the Web3 flow (events only).
    pub certificate_anchoring_enabled: bool,
    /// Anchoring network identifier handed to the Web3 flow.
    pub anchoring_network_id: String,
}

impl Default for ElearningSettings {
    fn default() -> Self {
        Self {
            levels: vec![
                "beginner".to_string(),
                "intermediate".to_string(),
                "advanced".to_string(),
                "all-levels".to_string(),
            ],
            categories: vec![
                "business".to_string(),
                "technology".to_string(),
                "design".to_string(),
                "language".to_string(),
                "health".to_string(),
                "finance".to_string(),
            ],
            languages: vec!["en".to_string()],
            lesson_types: vec![
                "video".to_string(),
                "text".to_string(),
                "file".to_string(),
                "quiz".to_string(),
            ],
            video_lesson_types: vec!["video".to_string(), "file".to_string()],
            text_lesson_types: vec!["text".to_string()],
            default_pass_mark: 70,
            default_max_attempts: None,
            default_currency: "USD".to_string(),
            certificate_issuer_name: "Unified Academy".to_string(),
            certificate_anchoring_enabled: false,
            anchoring_network_id: "mainnet".to_string(),
        }
    }
}
