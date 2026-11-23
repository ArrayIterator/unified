// Diesel models for the google domain of the unified-db-infra bounded context.
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
#[diesel(table_name = google_accounts)]
pub struct GoogleAccount {
    pub account_id: String,
    pub email: String,
    pub name: Option<String>,
    #[serde(skip_serializing)]
    pub access_token_encrypted: String,
    #[serde(skip_serializing)]
    pub refresh_token_encrypted: String,
    pub expires_at: DateTime<Utc>,
    pub scopes: Value,
    pub linked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = google_accounts)]
pub struct NewGoogleAccount {
    pub account_id: String,
    pub email: String,
    pub name: Option<String>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = google_drive_links)]
pub struct GoogleDriveLink {
    pub id: Uuid,
    pub name: String,
    pub account_id: String,
    pub email: String,
    pub folder_id: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = google_drive_links)]
pub struct NewGoogleDriveLink {
    pub id: Uuid,
    pub name: String,
    pub account_id: String,
    pub email: String,
    pub folder_id: Option<String>,
    pub status: String,
}
