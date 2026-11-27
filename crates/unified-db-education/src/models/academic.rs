// Diesel models for the academic domain of the unified-db-education bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Academic system (migration 000094_academic).
// Structs carrying NUMERIC columns skip serde derives (PgNumeric
// has no serde implementation), per the repo header rule.

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_programs)]
pub struct AcademicProgram {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub department_id: Option<String>,
    pub max_credits_per_semester: Option<PgNumeric>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_programs)]
pub struct NewAcademicProgram {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub department_id: Option<String>,
    pub max_credits_per_semester: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_students)]
pub struct AcademicStudent {
    pub id: Uuid,
    pub user_id: String,
    pub student_number: String,
    pub program_id: Uuid,
    pub advisor_id: Option<Uuid>,
    pub enrollment_year: i32,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_students)]
pub struct NewAcademicStudent {
    pub id: Uuid,
    pub user_id: String,
    pub student_number: String,
    pub program_id: Uuid,
    pub advisor_id: Option<Uuid>,
    pub enrollment_year: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_lecturers)]
pub struct AcademicLecturer {
    pub id: Uuid,
    pub user_id: String,
    pub lecturer_number: String,
    pub department_id: Option<String>,
    pub title: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_lecturers)]
pub struct NewAcademicLecturer {
    pub id: Uuid,
    pub user_id: String,
    pub lecturer_number: String,
    pub department_id: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_subjects)]
pub struct AcademicSubject {
    pub id: Uuid,
    pub code: String,
    pub title: String,
    pub program_id: Uuid,
    pub credits: PgNumeric,
    pub semester_number: Option<i32>,
    pub elearning_course_id: Option<String>,
    pub elearning_course_verified: bool,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_subjects)]
pub struct NewAcademicSubject {
    pub id: Uuid,
    pub code: String,
    pub title: String,
    pub program_id: Uuid,
    pub credits: PgNumeric,
    pub semester_number: Option<i32>,
    pub elearning_course_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_rooms)]
pub struct AcademicRoom {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub building: Option<String>,
    pub capacity: Option<i32>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_rooms)]
pub struct NewAcademicRoom {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub building: Option<String>,
    pub capacity: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_semesters)]
pub struct AcademicSemester {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_semesters)]
pub struct NewAcademicSemester {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_calendar_events)]
pub struct AcademicCalendarEvent {
    pub id: Uuid,
    pub semester_id: Option<Uuid>,
    pub event_type: String,
    pub title: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_calendar_events)]
pub struct NewAcademicCalendarEvent {
    pub id: Uuid,
    pub semester_id: Option<Uuid>,
    pub event_type: String,
    pub title: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub description: Option<String>,
}
