// CRM module.
//
// Contacts and leads, a pipeline grouped by stage, lead conversion into
// a contact (emitting `company.create_requested` and
// `user.create_requested`), and activities on contacts and leads.
// Interconnection happens only through the hook bus.

// crm — types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::store::*;
use super::common::*;
use super::handlers_contacts::*;
use super::handlers_leads::*;
use super::handlers_activities::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Capability required to read contacts, leads, the pipeline and
/// activities.
pub const PERM_CRM_READ: &str = "crm:read";

/// Capability required to create, update, delete, convert and add
/// activities.
pub const PERM_CRM_MANAGE: &str = "crm:manage";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub id: String,
    pub name: String,
    pub email: String,
    pub company: Option<String>,
    pub phone: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateContact {
    pub name: String,
    pub email: String,
    pub company: Option<String>,
    pub phone: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateContact {
    pub name: Option<String>,
    pub email: Option<String>,
    pub company: Option<String>,
    pub phone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lead {
    pub id: String,
    pub name: String,
    pub email: String,
    pub company: Option<String>,
    pub source: Option<String>,
    /// `new`, `contacted`, `qualified`, `negotiation`, `won`, `lost`, `converted`.
    pub stage: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateLead {
    pub name: String,
    pub email: String,
    pub company: Option<String>,
    pub source: Option<String>,
    pub stage: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateLead {
    pub name: Option<String>,
    pub email: Option<String>,
    pub company: Option<String>,
    pub source: Option<String>,
    pub stage: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub id: String,
    pub record_type: String,
    pub record_id: String,
    pub kind: String,
    pub note: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateActivity {
    pub note: String,
    pub kind: Option<String>,
}
