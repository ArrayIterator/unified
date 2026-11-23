// Diesel models for the studio domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Studio customizer (migration 000090_studio).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_record_types)]
pub struct StudioRecordType {
    pub record_type: String,
    pub module_id: String,
    pub label: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_record_types)]
pub struct NewStudioRecordType {
    pub record_type: String,
    pub module_id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_system_fields)]
pub struct StudioSystemField {
    pub id: Uuid,
    pub record_type: String,
    pub field_key: String,
    pub label: String,
    pub field_type: String,
    pub required: bool,
    pub hideable: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_system_fields)]
pub struct NewStudioSystemField {
    pub id: Uuid,
    pub record_type: String,
    pub field_key: String,
    pub label: String,
    pub field_type: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_field_definitions)]
pub struct StudioFieldDefinition {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub field_key: String,
    pub label: String,
    pub field_type: String,
    pub version: i64,
    pub status: String,
    pub required: bool,
    pub default_value: Option<Value>,
    pub help_text: Option<String>,
    pub options: Value,
    pub validation: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_field_definitions)]
pub struct NewStudioFieldDefinition {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub field_key: String,
    pub label: String,
    pub field_type: String,
    pub default_value: Option<Value>,
    pub help_text: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_field_values)]
pub struct StudioFieldValue {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub record_id: String,
    pub field_key: String,
    pub definition_id: Option<Uuid>,
    pub value_type: String,
    pub value: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_field_values)]
pub struct NewStudioFieldValue {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub record_id: String,
    pub field_key: String,
    pub definition_id: Option<Uuid>,
    pub value_type: String,
    pub value: Value,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_list_views)]
pub struct StudioListView {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: String,
    pub record_type: String,
    pub name: String,
    pub columns_: Value,
    pub filters: Value,
    pub sort_field: Option<String>,
    pub sort_order: Option<String>,
    pub group_by: Option<String>,
    pub visibility: String,
    pub scope: String,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_list_views)]
pub struct NewStudioListView {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: String,
    pub record_type: String,
    pub name: String,
    pub sort_field: Option<String>,
    pub sort_order: Option<String>,
    pub group_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = studio_form_layouts)]
pub struct StudioFormLayout {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub name: String,
    pub sections: Value,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = studio_form_layouts)]
pub struct NewStudioFormLayout {
    pub id: Uuid,
    pub company_id: String,
    pub record_type: String,
    pub name: String,
}
