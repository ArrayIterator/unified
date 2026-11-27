// Diesel models for the school_fees domain of the unified-db-education bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde_json::Value;

// School fees (migration 000096_school_fees).

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_types)]
pub struct SchoolFeeType {
    pub id: String,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub category: String,
    pub billing_period: String,
    pub default_amount_minor: PgNumeric,
    pub currency: String,
    pub auto_assign_on_enrolment: bool,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_types)]
pub struct NewSchoolFeeType {
    pub id: String,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub category: String,
    pub billing_period: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_schedules)]
pub struct SchoolFeeSchedule {
    pub id: String,
    pub student_user_id: String,
    pub program_code: String,
    pub level_code: String,
    pub academic_period_id: String,
    pub fee_type_id: String,
    pub amount_minor: PgNumeric,
    pub currency: String,
    pub due_date: DateTime<Utc>,
    pub status: String,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_schedules)]
pub struct NewSchoolFeeSchedule {
    pub id: String,
    pub student_user_id: String,
    pub program_code: String,
    pub level_code: String,
    pub academic_period_id: String,
    pub fee_type_id: String,
    pub due_date: DateTime<Utc>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_charges)]
pub struct SchoolFeeCharge {
    pub id: String,
    pub schedule_id: String,
    pub student_user_id: String,
    pub fee_type_id: String,
    pub academic_period_id: String,
    pub period_label: String,
    pub amount_minor: PgNumeric,
    pub discount_minor: PgNumeric,
    pub total_minor: PgNumeric,
    pub amount_paid_minor: PgNumeric,
    pub currency: String,
    pub invoice_id: Option<String>,
    pub discount_ids: Value,
    pub due_date: DateTime<Utc>,
    pub status: String,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_charges)]
pub struct NewSchoolFeeCharge {
    pub id: String,
    pub schedule_id: String,
    pub student_user_id: String,
    pub fee_type_id: String,
    pub academic_period_id: String,
    pub period_label: String,
    pub invoice_id: Option<String>,
    pub due_date: DateTime<Utc>,
    pub paid_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_discounts)]
pub struct SchoolFeeDiscount {
    pub id: String,
    pub student_user_id: String,
    pub fee_type_id: Option<String>,
    pub kind: String,
    pub percent_basis_points: i64,
    pub amount_minor: PgNumeric,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub approved: bool,
    pub approved_by: Option<String>,
    pub reason: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_discounts)]
pub struct NewSchoolFeeDiscount {
    pub id: String,
    pub student_user_id: String,
    pub fee_type_id: Option<String>,
    pub kind: String,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub approved_by: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_receipts)]
pub struct SchoolFeeReceipt {
    pub id: String,
    pub number: String,
    pub charge_id: String,
    pub student_user_id: String,
    pub amount_minor: PgNumeric,
    pub currency: String,
    pub method: Option<String>,
    pub reference: Option<String>,
    pub issued_by: String,
    pub issued_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_receipts)]
pub struct NewSchoolFeeReceipt {
    pub id: String,
    pub number: String,
    pub charge_id: String,
    pub student_user_id: String,
    pub method: Option<String>,
    pub reference: Option<String>,
    pub issued_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_guardians)]
pub struct SchoolFeeGuardian {
    pub id: String,
    pub guardian_user_id: String,
    pub student_user_id: String,
    pub relationship: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_guardians)]
pub struct NewSchoolFeeGuardian {
    pub id: String,
    pub guardian_user_id: String,
    pub student_user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = school_fee_reminders)]
pub struct SchoolFeeReminder {
    pub charge_id: String,
    pub offset_days: i32,
    pub recipient_user_id: String,
    pub sent_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = school_fee_reminders)]
pub struct NewSchoolFeeReminder {
    pub charge_id: String,
    pub offset_days: i32,
    pub recipient_user_id: String,
}
