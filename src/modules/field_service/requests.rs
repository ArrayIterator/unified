// field_service module - Request bodies.
//
// Split from the former single-file module `src/modules/field_service.rs`.
// The module root (`field_service.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateEquipmentTypeRequest {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub default_interval_value: Option<i64>,
    pub default_interval_unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEquipmentTypeRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub default_interval_value: Option<i64>,
    pub default_interval_unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateEquipmentRequest {
    pub asset_id: Option<Uuid>,
    pub equipment_type_id: Uuid,
    pub name: String,
    pub serial_number: Option<String>,
    pub location: Option<String>,
    pub install_date: Option<NaiveDate>,
    pub warranty_end: Option<NaiveDate>,
    pub status: Option<String>,
    pub usage_reading: Option<f64>,
    pub usage_unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEquipmentRequest {
    pub equipment_type_id: Option<Uuid>,
    pub name: Option<String>,
    pub serial_number: Option<Option<String>>,
    pub location: Option<Option<String>>,
    pub install_date: Option<Option<NaiveDate>>,
    pub warranty_end: Option<Option<NaiveDate>>,
    pub status: Option<String>,
    pub usage_reading: Option<Option<f64>>,
    pub usage_unit: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ReportFailureRequest {
    pub title: String,
    pub description: Option<String>,
    pub severity: Option<String>,
    pub priority: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTemplateRequest {
    pub name: String,
    pub description: Option<String>,
    pub equipment_type_id: Option<Uuid>,
    pub interval_value: Option<i64>,
    pub interval_unit: Option<String>,
    pub checklist_template: Option<Vec<String>>,
    pub default_priority: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTemplateRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub equipment_type_id: Option<Option<Uuid>>,
    pub interval_value: Option<i64>,
    pub interval_unit: Option<String>,
    pub checklist_template: Option<Vec<String>>,
    pub default_priority: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApplyTemplateRequest {
    pub equipment_id: Uuid,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePlanRequest {
    pub equipment_id: Uuid,
    pub template_id: Option<Uuid>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub interval_value: Option<i64>,
    pub interval_unit: Option<String>,
    pub last_service_at: Option<DateTime<Utc>>,
    pub usage_at_last_service: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePlanRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub interval_value: Option<i64>,
    pub interval_unit: Option<String>,
    pub last_service_at: Option<Option<DateTime<Utc>>>,
    pub usage_at_last_service: Option<Option<f64>>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkOrderRequest {
    pub equipment_id: Uuid,
    pub plan_id: Option<Uuid>,
    pub project_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub order_type: Option<String>,
    pub priority: Option<String>,
    pub assignee_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub checklist_items: Option<Vec<String>>,
    pub required_parts: Option<Vec<CreatePartLineRequest>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePartLineRequest {
    pub stock_id: Option<Uuid>,
    pub part_name: String,
    pub quantity: Option<i64>,
    pub unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkOrderRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub priority: Option<String>,
    pub project_id: Option<Option<String>>,
    pub plan_id: Option<Option<Uuid>>,
    pub scheduled_at: Option<Option<DateTime<Utc>>>,
    pub due_at: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Deserialize)]
pub struct ChangeStatusRequest {
    pub status: String,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssignRequest {
    pub technician_id: String,
    pub sequence: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateItemRequest {
    pub title: String,
    pub position: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateItemRequest {
    pub title: Option<String>,
    pub position: Option<i64>,
    pub notes: Option<Option<String>>,
    pub is_done: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePartRequest {
    pub stock_id: Option<Uuid>,
    pub part_name: String,
    pub quantity: Option<i64>,
    pub unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePartRequest {
    pub part_name: Option<String>,
    pub quantity: Option<i64>,
    pub unit: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TimeLogRequest {
    pub technician_id: String,
    pub hours: f64,
    pub description: Option<String>,
    pub logged_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct RaiseIncidentRequest {
    pub title: String,
    pub description: Option<String>,
    pub severity: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SignOffRequest {
    pub customer_name: String,
    pub signature_ref: String,
    pub notes: Option<String>,
    pub survey_distribution_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct AddDefinitionRequest {
    pub value: String,
    pub label: Option<String>,
    /// Required only for `interval_unit`: days per unit for a
    /// day-based custom unit. Calendar-month and usage-based
    /// units cannot be added by the admin.
    pub days_per_unit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub max_concurrent_jobs_per_technician: Option<i64>,
    pub overdue_alert_days: Option<i64>,
    pub default_interval_days: Option<i64>,
    pub default_priority: Option<String>,
}
