// Diesel models for the forms domain of the unified-db-platform bounded context.
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
// Forms (migration 000047_forms).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forms)]
pub struct Form {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub status: String,
    pub company_id: Option<String>,
    pub owner_id: String,
    pub submit_button_label: String,
    pub success_message: String,
    pub honeypot_field: String,
    pub mapping: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forms)]
pub struct NewForm {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub company_id: Option<String>,
    pub owner_id: String,
    pub mapping: Option<Value>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = form_fields)]
pub struct FormField {
    pub id: Uuid,
    pub form_id: Uuid,
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub position: i64,
    pub required: bool,
    pub placeholder: Option<String>,
    pub help_text: Option<String>,
    pub options: Vec<String>,
    pub min_length: Option<i64>,
    pub max_length: Option<i64>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub pattern: Option<String>,
    pub show_when: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = form_fields)]
pub struct NewFormField {
    pub id: Uuid,
    pub form_id: Uuid,
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub placeholder: Option<String>,
    pub help_text: Option<String>,
    pub min_length: Option<i64>,
    pub max_length: Option<i64>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub pattern: Option<String>,
    pub show_when: Option<Value>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = form_submissions)]
pub struct FormSubmission {
    pub id: Uuid,
    pub form_id: Uuid,
    pub status: String,
    pub mapping_status: String,
    pub mapping_error: Option<String>,
    pub mapped_target_id: Option<String>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub submitted_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = form_submissions)]
pub struct NewFormSubmission {
    pub id: Uuid,
    pub form_id: Uuid,
    pub mapping_error: Option<String>,
    pub mapped_target_id: Option<String>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub submitted_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = form_submission_values)]
pub struct FormSubmissionValue {
    pub id: Uuid,
    pub submission_id: Uuid,
    pub field_id: Uuid,
    pub field_key: String,
    pub value: Value,
    pub file_storage_key: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = form_submission_values)]
pub struct NewFormSubmissionValue {
    pub id: Uuid,
    pub submission_id: Uuid,
    pub field_id: Uuid,
    pub field_key: String,
    pub file_storage_key: Option<String>,
}
