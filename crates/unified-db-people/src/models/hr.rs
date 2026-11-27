// Diesel models for the hr domain of the unified-db-people bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Human Resource Management (2026-11-22-000037_hr). Money fields are
// `PgNumeric` (NUMERIC(19,4) in SQL); leave days are `PgNumeric`
// (NUMERIC(10,2) in SQL). Money structs carry no serde derives
// (PgNumeric has none) - the notes' snippets show Serialize /
// Deserialize on money structs; those derives are dropped here per
// the repo money convention.

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_employees)]
pub struct HrEmployee {
    pub id: Uuid,
    pub user_id: String,
    pub employee_code: String,
    pub position_id: Option<Uuid>,
    pub department_id: Option<String>,
    pub department_verified: bool,
    pub manager_id: Option<Uuid>,
    pub status: String,
    pub hire_date: NaiveDate,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_employees)]
pub struct NewHrEmployee {
    pub id: Uuid,
    pub user_id: String,
    pub employee_code: String,
    pub position_id: Option<Uuid>,
    pub department_id: Option<String>,
    pub department_verified: bool,
    pub manager_id: Option<Uuid>,
    pub status: String,
    pub hire_date: NaiveDate,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_positions)]
pub struct HrPosition {
    pub id: Uuid,
    pub title: String,
    pub department_id: Option<String>,
    pub level: Option<String>,
    pub salary_band_min: Option<PgNumeric>,
    pub salary_band_max: Option<PgNumeric>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_positions)]
pub struct NewHrPosition {
    pub id: Uuid,
    pub title: String,
    pub department_id: Option<String>,
    pub level: Option<String>,
    pub salary_band_min: Option<PgNumeric>,
    pub salary_band_max: Option<PgNumeric>,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_contracts)]
pub struct HrContract {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub contract_type: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub salary_amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_contracts)]
pub struct NewHrContract {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub contract_type: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub salary_amount: PgNumeric,
    pub currency: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_leave_types)]
pub struct HrLeaveType {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub default_days_per_year: PgNumeric,
    pub paid: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_leave_types)]
pub struct NewHrLeaveType {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub default_days_per_year: PgNumeric,
    pub paid: bool,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_leave_balances)]
pub struct HrLeaveBalance {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub year: i32,
    pub entitled_days: PgNumeric,
    pub used_days: PgNumeric,
    pub carried_over_days: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_leave_balances)]
pub struct NewHrLeaveBalance {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub year: i32,
    pub entitled_days: PgNumeric,
    pub used_days: PgNumeric,
    pub carried_over_days: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_leave_requests)]
pub struct HrLeaveRequest {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub days: PgNumeric,
    pub status: String,
    pub reason: String,
    pub reviewed_by: Option<String>,
    pub review_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_leave_requests)]
pub struct NewHrLeaveRequest {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub leave_type_id: Uuid,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub days: PgNumeric,
    pub status: String,
    pub reason: String,
    pub reviewed_by: Option<String>,
    pub review_note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_attendance_records)]
pub struct HrAttendanceRecord {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub user_id: String,
    pub work_date: NaiveDate,
    pub minutes_worked: i32,
    pub source: String,
    pub entry_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_attendance_records)]
pub struct NewHrAttendanceRecord {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub user_id: String,
    pub work_date: NaiveDate,
    pub minutes_worked: i32,
    pub source: String,
    pub entry_id: Option<String>,
}
