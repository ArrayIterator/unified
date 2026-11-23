// Diesel models for the library domain of the unified-db-education bounded context.
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
// Library (migration 000097_library).
// Structs carrying NUMERIC columns skip serde derives (PgNumeric
// has no serde implementation), per the repo header rule.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_categories)]
pub struct LibraryCategory {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_categories)]
pub struct NewLibraryCategory {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub parent_id: Option<Uuid>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_items)]
pub struct LibraryItem {
    pub id: Uuid,
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Value,
    pub isbn: Option<String>,
    pub isbn_normalized: Option<String>,
    pub edition: Option<String>,
    pub series: Option<String>,
    pub series_position: Option<i64>,
    pub publisher: Option<String>,
    pub published_year: Option<i64>,
    pub language: String,
    pub category_id: Option<Uuid>,
    pub category_name: Option<String>,
    pub item_type: String,
    pub status: String,
    pub description: Option<String>,
    pub cover_ref: Option<String>,
    pub digital_ref: Option<String>,
    pub document_ref: Option<String>,
    pub storage_ref: Option<String>,
    pub tags: Value,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_items)]
pub struct NewLibraryItem {
    pub id: Uuid,
    pub title: String,
    pub subtitle: Option<String>,
    pub isbn: Option<String>,
    pub isbn_normalized: Option<String>,
    pub edition: Option<String>,
    pub series: Option<String>,
    pub series_position: Option<i64>,
    pub publisher: Option<String>,
    pub published_year: Option<i64>,
    pub language: String,
    pub category_id: Option<Uuid>,
    pub category_name: Option<String>,
    pub item_type: String,
    pub status: String,
    pub description: Option<String>,
    pub cover_ref: Option<String>,
    pub digital_ref: Option<String>,
    pub document_ref: Option<String>,
    pub storage_ref: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_copies)]
pub struct LibraryCopy {
    pub id: Uuid,
    pub item_id: Uuid,
    pub barcode: String,
    pub serial: Option<String>,
    pub condition: String,
    pub location: Option<String>,
    pub shelf: Option<String>,
    pub status: String,
    pub notes: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_copies)]
pub struct NewLibraryCopy {
    pub id: Uuid,
    pub item_id: Uuid,
    pub barcode: String,
    pub serial: Option<String>,
    pub condition: String,
    pub location: Option<String>,
    pub shelf: Option<String>,
    pub status: String,
    pub notes: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_members)]
pub struct LibraryMember {
    pub id: Uuid,
    pub user_id: String,
    pub member_type: String,
    pub status: String,
    pub joined_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_members)]
pub struct NewLibraryMember {
    pub id: Uuid,
    pub user_id: String,
    pub member_type: String,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_loans)]
pub struct LibraryLoan {
    pub id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub item_id: Uuid,
    pub copy_id: Option<Uuid>,
    pub status: String,
    pub checked_out_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub returned_at: Option<DateTime<Utc>>,
    pub renewal_count: i64,
    pub last_reminded_at: Option<DateTime<Utc>>,
    pub last_overdue_noticed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_loans)]
pub struct NewLibraryLoan {
    pub id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub item_id: Uuid,
    pub copy_id: Option<Uuid>,
    pub status: String,
    pub due_at: DateTime<Utc>,
    pub returned_at: Option<DateTime<Utc>>,
    pub last_reminded_at: Option<DateTime<Utc>>,
    pub last_overdue_noticed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_holds)]
pub struct LibraryHold {
    pub id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub item_id: Uuid,
    pub copy_id: Option<Uuid>,
    pub status: String,
    pub position: i64,
    pub ready_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_holds)]
pub struct NewLibraryHold {
    pub id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub item_id: Uuid,
    pub copy_id: Option<Uuid>,
    pub status: String,
    pub position: i64,
    pub ready_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = library_fines)]
pub struct LibraryFine {
    pub id: Uuid,
    pub loan_id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub amount_minor: PgNumeric,
    pub currency: String,
    pub reason: String,
    pub status: String,
    pub waived_by: Option<String>,
    pub paid_reference: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = library_fines)]
pub struct NewLibraryFine {
    pub id: Uuid,
    pub loan_id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub amount_minor: PgNumeric,
    pub currency: String,
    pub reason: String,
    pub status: String,
    pub waived_by: Option<String>,
    pub paid_reference: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = library_loan_policies)]
pub struct LibraryLoanPolicy {
    pub id: Uuid,
    pub name: String,
    pub member_type: String,
    pub item_type: String,
    pub loan_days: i64,
    pub max_concurrent: i64,
    pub max_renewals: i64,
    pub fine_per_day_minor: PgNumeric,
    pub currency: String,
    pub grace_days: i64,
    pub active: bool,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = library_loan_policies)]
pub struct NewLibraryLoanPolicy {
    pub id: Uuid,
    pub name: String,
    pub member_type: String,
    pub item_type: String,
    pub loan_days: i64,
    pub max_concurrent: i64,
    pub max_renewals: i64,
    pub fine_per_day_minor: PgNumeric,
    pub currency: String,
    pub grace_days: i64,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_inventory_audits)]
pub struct LibraryInventoryAudit {
    pub id: Uuid,
    pub name: String,
    pub scope_location: Option<String>,
    pub scope_shelf: Option<String>,
    pub status: String,
    pub started_by: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub items_checked: i64,
    pub discrepancies_found: i64,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_inventory_audits)]
pub struct NewLibraryInventoryAudit {
    pub id: Uuid,
    pub name: String,
    pub scope_location: Option<String>,
    pub scope_shelf: Option<String>,
    pub status: String,
    pub started_by: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_inventory_discrepancies)]
pub struct LibraryInventoryDiscrepancy {
    pub id: Uuid,
    pub audit_id: Uuid,
    pub copy_id: Uuid,
    pub item_id: Uuid,
    pub kind: String,
    pub note: Option<String>,
    pub status: String,
    pub resolved_by: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_inventory_discrepancies)]
pub struct NewLibraryInventoryDiscrepancy {
    pub id: Uuid,
    pub audit_id: Uuid,
    pub copy_id: Uuid,
    pub item_id: Uuid,
    pub kind: String,
    pub note: Option<String>,
    pub resolved_by: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = library_digital_access)]
pub struct LibraryDigitalAccess {
    pub id: Uuid,
    pub loan_id: Uuid,
    pub item_id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
    pub redeemed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = library_digital_access)]
pub struct NewLibraryDigitalAccess {
    pub id: Uuid,
    pub loan_id: Uuid,
    pub item_id: Uuid,
    pub member_id: Uuid,
    pub user_id: String,
    pub expires_at: DateTime<Utc>,
    pub redeemed_at: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
}
