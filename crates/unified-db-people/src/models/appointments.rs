// Diesel models for the appointments domain of the unified-db-people bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Appointments (migration 000077_appointments).

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = appointment_services)]
pub struct AppointmentService {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub duration_minutes: i32,
    pub price: PgNumeric,
    pub currency: String,
    pub capacity: i32,
    pub requires_staff: bool,
    pub location_type: String,
    pub location: Option<String>,
    pub buffer_before_minutes: Option<i32>,
    pub buffer_after_minutes: Option<i32>,
    pub cancellation_notice_hours: Option<i32>,
    pub cancellation_fee_percent: Option<PgNumeric>,
    pub is_active: bool,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = appointment_services)]
pub struct NewAppointmentService {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub duration_minutes: i32,
    pub location: Option<String>,
    pub buffer_before_minutes: Option<i32>,
    pub buffer_after_minutes: Option<i32>,
    pub cancellation_notice_hours: Option<i32>,
    pub cancellation_fee_percent: Option<PgNumeric>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = appointment_staff_availability)]
pub struct AppointmentStaffAvailability {
    pub id: Uuid,
    pub staff_user_id: String,
    pub kind: String,
    pub weekday: Option<i32>,
    pub date: Option<NaiveDate>,
    pub start_minutes: Option<i32>,
    pub end_minutes: Option<i32>,
    pub break_start_minutes: Option<i32>,
    pub break_end_minutes: Option<i32>,
    pub valid_from: Option<NaiveDate>,
    pub valid_until: Option<NaiveDate>,
    pub reason: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = appointment_staff_availability)]
pub struct NewAppointmentStaffAvailability {
    pub id: Uuid,
    pub staff_user_id: String,
    pub kind: String,
    pub weekday: Option<i32>,
    pub date: Option<NaiveDate>,
    pub start_minutes: Option<i32>,
    pub end_minutes: Option<i32>,
    pub break_start_minutes: Option<i32>,
    pub break_end_minutes: Option<i32>,
    pub valid_from: Option<NaiveDate>,
    pub valid_until: Option<NaiveDate>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = appointment_staff_services)]
pub struct AppointmentStaffService {
    pub id: Uuid,
    pub staff_user_id: String,
    pub service_id: Uuid,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = appointment_staff_services)]
pub struct NewAppointmentStaffService {
    pub id: Uuid,
    pub staff_user_id: String,
    pub service_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = appointments)]
pub struct Appointment {
    pub id: Uuid,
    pub service_id: Uuid,
    pub staff_user_id: Option<String>,
    pub customer_user_id: Option<String>,
    pub customer_name: String,
    pub customer_email: String,
    pub customer_phone: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub source: String,
    pub price: PgNumeric,
    pub currency: String,
    pub cancellation_fee_charged: Option<PgNumeric>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub notes: Option<String>,
    pub confirmation_code: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = appointments)]
pub struct NewAppointment {
    pub id: Uuid,
    pub service_id: Uuid,
    pub staff_user_id: Option<String>,
    pub customer_user_id: Option<String>,
    pub customer_name: String,
    pub customer_email: String,
    pub customer_phone: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub cancellation_fee_charged: Option<PgNumeric>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub notes: Option<String>,
    pub confirmation_code: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = appointment_reminders)]
pub struct AppointmentReminder {
    pub id: Uuid,
    pub appointment_id: Uuid,
    pub offset_minutes: i32,
    pub channels: Vec<String>,
    pub status: String,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = appointment_reminders)]
pub struct NewAppointmentReminder {
    pub id: Uuid,
    pub appointment_id: Uuid,
    pub offset_minutes: i32,
    pub scheduler_job_id: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
}
