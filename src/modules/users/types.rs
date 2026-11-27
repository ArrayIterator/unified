// users module - User types.
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

pub(crate) static EMAIL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}$")
        .expect("email validation pattern is valid")
});

/// In-memory user record. Mirrors the Diesel `User` model plus the role
/// assignments, which live in `user_roles` when a database is present.
#[derive(Debug, Clone, Serialize)]
pub struct StoredUser {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub is_active: bool,
    pub company_id: Option<Uuid>,
    pub roles: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public user representation: the password hash is never serialized.
#[derive(Debug, Clone, Serialize)]
pub struct UserResponse {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub is_active: bool,
    pub company_id: Option<Uuid>,
    pub roles: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&StoredUser> for UserResponse {
    fn from(record: &StoredUser) -> Self {
        Self {
            id: record.id,
            email: record.email.clone(),
            name: record.name.clone(),
            is_active: record.is_active,
            company_id: record.company_id,
            roles: record.roles.clone(),
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

impl From<(&User, Vec<Uuid>)> for UserResponse {
    fn from((user, roles): (&User, Vec<Uuid>)) -> Self {
        Self {
            id: user.id,
            email: user.email.clone(),
            name: user.name.clone(),
            is_active: user.is_active,
            company_id: user.company_id,
            roles,
            created_at: user.created_at,
            updated_at: user.updated_at,
        }
    }
}

pub type UsersStore = Arc<DashMap<Uuid, StoredUser>>;
