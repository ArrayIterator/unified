// Diesel models for the digital_products domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Digital products (migration 000078_digital_products).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = digital_product_types)]
pub struct DigitalProductType {
    pub id: String,
    pub label: String,
    pub description: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = digital_product_types)]
pub struct NewDigitalProductType {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = digital_products)]
pub struct DigitalProduct {
    pub id: Uuid,
    pub product_id: String,
    pub type_id: String,
    pub author: Option<String>,
    pub publisher: Option<String>,
    pub edition: Option<String>,
    pub isbn: Option<String>,
    pub language: Option<String>,
    pub pages: Option<i32>,
    pub formats: Vec<String>,
    pub cover_storage_id: Option<String>,
    pub sample_allowed: bool,
    pub download_limit: i32,
    pub royalty_kind: String,
    pub royalty_value: PgNumeric,
    pub royalty_payee: Option<String>,
    pub royalty_payee_company_id: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = digital_products)]
pub struct NewDigitalProduct {
    pub id: Uuid,
    pub product_id: String,
    pub type_id: String,
    pub author: Option<String>,
    pub publisher: Option<String>,
    pub edition: Option<String>,
    pub isbn: Option<String>,
    pub language: Option<String>,
    pub pages: Option<i32>,
    pub cover_storage_id: Option<String>,
    pub royalty_payee: Option<String>,
    pub royalty_payee_company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = digital_product_files)]
pub struct DigitalProductFile {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub storage_file_id: String,
    pub format: String,
    pub label: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub checksum: Option<String>,
    pub is_primary: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = digital_product_files)]
pub struct NewDigitalProductFile {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub storage_file_id: String,
    pub format: String,
    pub label: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = digital_product_previews)]
pub struct DigitalProductPreview {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub storage_file_id: String,
    pub format: String,
    pub label: Option<String>,
    pub allowed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = digital_product_previews)]
pub struct NewDigitalProductPreview {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub storage_file_id: String,
    pub format: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = digital_payout_requests)]
pub struct DigitalPayoutRequest {
    pub id: Uuid,
    pub payee: Option<String>,
    pub payee_company_id: Option<String>,
    pub currency: String,
    pub amount: PgNumeric,
    pub royalty_ids: Vec<Uuid>,
    pub status: String,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = digital_payout_requests)]
pub struct NewDigitalPayoutRequest {
    pub id: Uuid,
    pub payee: Option<String>,
    pub payee_company_id: Option<String>,
    pub currency: String,
    pub amount: PgNumeric,
    pub requested_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = digital_royalties)]
pub struct DigitalRoyalty {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub product_id: String,
    pub order_id: String,
    pub buyer_user_id: String,
    pub payee: Option<String>,
    pub payee_company_id: Option<String>,
    pub amount: PgNumeric,
    pub currency: String,
    pub royalty_kind: String,
    pub royalty_value: PgNumeric,
    pub status: String,
    pub payout_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = digital_royalties)]
pub struct NewDigitalRoyalty {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub product_id: String,
    pub order_id: String,
    pub buyer_user_id: String,
    pub payee: Option<String>,
    pub payee_company_id: Option<String>,
    pub amount: PgNumeric,
    pub currency: String,
    pub royalty_kind: String,
    pub royalty_value: PgNumeric,
    pub payout_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = digital_downloads)]
pub struct DigitalDownload {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub file_id: Option<Uuid>,
    pub preview_id: Option<Uuid>,
    pub buyer_user_id: String,
    pub order_id: Option<String>,
    pub grant_id: Option<String>,
    pub kind: String,
    pub watermark_subject: Option<String>,
    pub watermark_applied: bool,
    pub watermark_formats: Vec<String>,
    pub downloaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = digital_downloads)]
pub struct NewDigitalDownload {
    pub id: Uuid,
    pub digital_product_id: Uuid,
    pub file_id: Option<Uuid>,
    pub preview_id: Option<Uuid>,
    pub buyer_user_id: String,
    pub order_id: Option<String>,
    pub grant_id: Option<String>,
    pub kind: String,
    pub watermark_subject: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = digital_watermark_formats)]
pub struct DigitalWatermarkFormat {
    pub format: String,
    pub metadata_supported: bool,
    pub metadata_fields: Vec<String>,
    pub notes: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = digital_watermark_formats)]
pub struct NewDigitalWatermarkFormat {
    pub format: String,
}
