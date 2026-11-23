//! roles - types.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{store::*, module::*, capabilities::*, seeds::*, validation::*, handlers::*};


pub type RolesStore = Arc<DashMap<Uuid, RoleRecord>>;


/// One role. `permissions` is a JSON object mapping a resource name to the
/// list of actions allowed on it; `"*"` matches any resource or action.
#[derive(Debug, Clone, Serialize)]
pub struct RoleRecord {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub is_system: bool,
    pub permissions: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreateRoleInput {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "empty_permissions")]
    pub permissions: Value,
}


#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRoleInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub permissions: Option<Value>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CheckRoleInput {
    /// Role id (UUID) or slug.
    pub role: String,
    /// Permission to check, as `resource:action` (or `*` for any).
    pub permission: String,
}


#[derive(Debug, Clone, Serialize)]
pub struct CheckRoleResponse {
    pub role: String,
    pub permission: String,
    pub allowed: bool,
}
