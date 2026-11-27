// Gamification - Request bodies.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateRuleRequest {
    pub name: String,
    pub action: String,
    pub points: i64,
    pub company_id: Option<String>,
    pub daily_cap: Option<i64>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRuleRequest {
    pub name: Option<String>,
    pub action: Option<String>,
    pub points: Option<i64>,
    pub company_id: Option<Option<String>>,
    pub daily_cap: Option<Option<i64>>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AwardPointsRequest {
    pub user_id: String,
    pub action: String,
    pub company_id: Option<String>,
    pub department_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AdjustPointsRequest {
    pub user_id: String,
    pub points: i64,
    pub reason: Option<String>,
    pub company_id: Option<String>,
    pub department_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBadgeRequest {
    pub name: String,
    pub description: String,
    pub criteria_action: String,
    pub criteria_threshold: i64,
    pub icon: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBadgeRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub criteria_action: Option<String>,
    pub criteria_threshold: Option<i64>,
    pub icon: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSeasonRequest {
    pub name: String,
    pub company_id: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSeasonRequest {
    pub name: Option<String>,
    pub company_id: Option<Option<String>>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRewardRequest {
    pub name: String,
    pub description: String,
    pub points_cost: i64,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRewardRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub points_cost: Option<i64>,
    pub active: Option<bool>,
}

/// Optional scope for a reward request (`?company_id=...&season_id=...`):
/// which company's (and season's) balance the points are spent
/// from. Absent values resolve to the active season of that scope
/// at request time and are snapshotted onto the request.
#[derive(Debug, Deserialize)]
pub struct RequestRewardScope {
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub season_id: Option<Uuid>,
}
