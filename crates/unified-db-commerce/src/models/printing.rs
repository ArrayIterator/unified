// Diesel models for the printing domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Printing (migration 000091_printing).
// PaperSize carries NUMERIC columns, so it skips serde derives
// (PgNumeric has no serde implementation).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = paper_sizes)]
pub struct PaperSize {
    pub code: String,
    pub name: String,
    pub width_mm: PgNumeric,
    pub height_mm: Option<PgNumeric>,
    pub margin_top_mm: PgNumeric,
    pub margin_bottom_mm: PgNumeric,
    pub margin_left_mm: PgNumeric,
    pub margin_right_mm: PgNumeric,
    pub is_standard: bool,
    pub is_thermal: bool,
    pub thermal_width_mm: Option<i32>,
    pub company_id: Option<String>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = paper_sizes)]
pub struct NewPaperSize {
    pub code: String,
    pub name: String,
    pub width_mm: PgNumeric,
    pub height_mm: Option<PgNumeric>,
    pub is_standard: bool,
    pub is_thermal: bool,
    pub thermal_width_mm: Option<i32>,
    pub company_id: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = invoice_templates)]
pub struct InvoiceTemplate {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub is_default: bool,
    pub paper_size_code: Option<String>,
    pub header_text: Option<String>,
    pub footer_text: Option<String>,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub logo_url: Option<String>,
    pub primary_color: String,
    pub accent_color: String,
    pub font_family: String,
    pub columns_: Value,
    pub line_item_layout: Value,
    pub show_tax_breakdown: bool,
    pub payment_instructions: Option<String>,
    pub footer_notes: Option<String>,
    pub version: i32,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = invoice_templates)]
pub struct NewInvoiceTemplate {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub paper_size_code: Option<String>,
    pub header_text: Option<String>,
    pub footer_text: Option<String>,
    pub logo_storage_target: Option<String>,
    pub logo_storage_key: Option<String>,
    pub logo_url: Option<String>,
    pub payment_instructions: Option<String>,
    pub footer_notes: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = invoice_template_versions)]
pub struct InvoiceTemplateVersion {
    pub id: Uuid,
    pub template_id: Uuid,
    pub version: i32,
    pub snapshot: Value,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = invoice_template_versions)]
pub struct NewInvoiceTemplateVersion {
    pub id: Uuid,
    pub template_id: Uuid,
    pub version: i32,
    pub snapshot: Value,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = printer_profiles)]
pub struct PrinterProfile {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub connection_type: String,
    pub connection_target: Option<String>,
    pub auth_token_ref: Option<String>,
    pub width_columns: i32,
    pub character_set: String,
    pub code_page: String,
    pub supports_drawer_kick: bool,
    pub supports_cut: bool,
    pub paper_size_code: Option<String>,
    pub is_default: bool,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = printer_profiles)]
pub struct NewPrinterProfile {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub name: String,
    pub connection_target: Option<String>,
    pub auth_token_ref: Option<String>,
    pub paper_size_code: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = print_jobs)]
pub struct PrintJob {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub invoice_id: String,
    pub template_id: Option<Uuid>,
    pub template_version: Option<i32>,
    pub paper_size_code: Option<String>,
    pub output_kind: String,
    pub printer_profile_id: Option<Uuid>,
    pub status: String,
    pub document_ref: Option<String>,
    pub escpos_preview: Option<String>,
    pub error_message: Option<String>,
    pub attempts: i32,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = print_jobs)]
pub struct NewPrintJob {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub invoice_id: String,
    pub template_id: Option<Uuid>,
    pub template_version: Option<i32>,
    pub paper_size_code: Option<String>,
    pub printer_profile_id: Option<Uuid>,
    pub document_ref: Option<String>,
    pub escpos_preview: Option<String>,
    pub error_message: Option<String>,
    pub requested_by: String,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}
