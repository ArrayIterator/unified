// Diesel models for the jobs domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = import_jobs)]
pub struct ImportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub total_rows: i64,
    pub valid_rows: i64,
    pub invalid_rows: i64,
    pub row_errors: Value,
    pub results: Value,
    pub actor: String,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = import_jobs)]
pub struct NewImportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = export_jobs)]
pub struct ExportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub fields: Value,
    pub row_count: i64,
    pub data_provider: String,
    pub download_url: String,
    pub error: Option<String>,
    pub actor: String,
    pub company_id: Option<Uuid>,
    pub pii_redacted: bool,
    pub redacted_fields: Value,
    pub row_limit: i64,
    pub row_offset: i64,
    pub has_more: bool,
    pub rows_excluded_by_tenant_scope: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = export_jobs)]
pub struct NewExportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub data_provider: String,
    pub download_url: String,
    pub error: Option<String>,
    pub company_id: Option<Uuid>,
}
