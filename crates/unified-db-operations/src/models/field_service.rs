// Diesel models for the field_service domain of the unified-db-operations bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Field service (migration 000080_field_service).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_equipment)]
pub struct FieldEquipment {
    pub id: Uuid,
    pub asset_id: Option<Uuid>,
    pub equipment_type_id: Uuid,
    pub name: String,
    pub serial_number: Option<String>,
    pub location: Option<String>,
    pub install_date: Option<NaiveDate>,
    pub warranty_end: Option<NaiveDate>,
    pub status: String,
    pub usage_reading: Option<f64>,
    pub usage_unit: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_equipment)]
pub struct NewFieldEquipment {
    pub id: Uuid,
    pub asset_id: Option<Uuid>,
    pub equipment_type_id: Uuid,
    pub name: String,
    pub serial_number: Option<String>,
    pub location: Option<String>,
    pub install_date: Option<NaiveDate>,
    pub warranty_end: Option<NaiveDate>,
    pub status: String,
    pub usage_reading: Option<f64>,
    pub usage_unit: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_maintenance_plans)]
pub struct FieldMaintenancePlan {
    pub id: Uuid,
    pub equipment_id: Uuid,
    pub template_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub interval_value: i64,
    pub interval_unit: String,
    pub last_service_at: Option<DateTime<Utc>>,
    pub usage_at_last_service: Option<f64>,
    pub next_due_at: Option<DateTime<Utc>>,
    pub next_due_usage: Option<f64>,
    pub is_active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_maintenance_plans)]
pub struct NewFieldMaintenancePlan {
    pub id: Uuid,
    pub equipment_id: Uuid,
    pub template_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub interval_value: i64,
    pub interval_unit: String,
    pub last_service_at: Option<DateTime<Utc>>,
    pub usage_at_last_service: Option<f64>,
    pub next_due_at: Option<DateTime<Utc>>,
    pub next_due_usage: Option<f64>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_work_orders)]
pub struct FieldWorkOrder {
    pub id: Uuid,
    pub equipment_id: Uuid,
    pub plan_id: Option<Uuid>,
    pub project_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub order_type: String,
    pub priority: String,
    pub status: String,
    pub assignee_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_work_orders)]
pub struct NewFieldWorkOrder {
    pub id: Uuid,
    pub equipment_id: Uuid,
    pub plan_id: Option<Uuid>,
    pub project_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub order_type: String,
    pub priority: String,
    pub status: String,
    pub assignee_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_work_order_items)]
pub struct FieldWorkOrderItem {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub title: String,
    pub position: i64,
    pub is_done: bool,
    pub done_by: Option<String>,
    pub done_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_work_order_items)]
pub struct NewFieldWorkOrderItem {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub title: String,
    pub done_by: Option<String>,
    pub done_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_work_order_parts)]
pub struct FieldWorkOrderPart {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub stock_id: Option<Uuid>,
    pub part_name: String,
    pub quantity: i64,
    pub unit: String,
    pub status: String,
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_work_order_parts)]
pub struct NewFieldWorkOrderPart {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub stock_id: Option<Uuid>,
    pub part_name: String,
    pub quantity: i64,
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_dispatch_assignments)]
pub struct FieldDispatchAssignment {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub technician_id: String,
    pub sequence: i64,
    pub status: String,
    pub assigned_by: String,
    pub assigned_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_dispatch_assignments)]
pub struct NewFieldDispatchAssignment {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub technician_id: String,
    pub assigned_by: String,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub released_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = field_signoffs)]
pub struct FieldSignoff {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub customer_name: String,
    pub signature_ref: String,
    pub signed_at: DateTime<Utc>,
    pub notes: Option<String>,
    pub survey_distribution_id: Option<Uuid>,
    pub follow_up_status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = field_signoffs)]
pub struct NewFieldSignoff {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub customer_name: String,
    pub signature_ref: String,
    pub notes: Option<String>,
    pub survey_distribution_id: Option<Uuid>,
    pub created_by: String,
}
