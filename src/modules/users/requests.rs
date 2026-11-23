// users module — Request types.
//
// Split from the former single-file module `src/modules/users.rs`.
// The module root (`users.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json::{error_response, success_response};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{NewUser, User};
use unified_core::db::DbPool;
use crate::modules::security::PasswordPolicy;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, LazyLock, OnceLock};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
pub struct CreateUserInput {
    pub email: String,
    pub name: String,
    pub password: String,
    #[serde(default)]
    pub company_id: Option<Uuid>,
    #[serde(default = "default_active")]
    pub is_active: bool,
}

pub(crate) fn default_active() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateUserInput {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub is_active: Option<bool>,
    /// `Some(Some(id))` sets the company, `Some(None)` clears it,
    /// `None` (field absent) leaves it unchanged.
    #[serde(default)]
    pub company_id: Option<Option<Uuid>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssignRoleInput {
    pub role_id: Uuid,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserRolesResponse {
    pub user_id: Uuid,
    pub roles: Vec<Uuid>,
}
