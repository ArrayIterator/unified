//! Academic - programs, students, lecturers and record types.
//!
//! Part of the `academic` module split; see `src/modules/academic.rs`.

use chrono::DateTime;
use chrono::NaiveDate;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

// Types

/// One study program (major). `department_id` is owned by the
/// Departments module and is stored as a plain id only.
/// `max_credits_per_semester` caps registration per semester for
/// students of this program; when absent, the
/// `default_max_credits_per_semester` setting (or its default)
/// applies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Program {
    pub id: String,
    pub code: String,
    pub name: String,
    pub department_id: Option<String>,
    pub max_credits_per_semester: Option<f64>,
    /// `active` or `inactive`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProgram {
    pub code: String,
    pub name: String,
    pub department_id: Option<String>,
    pub max_credits_per_semester: Option<f64>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProgram {
    pub name: Option<String>,
    pub department_id: Option<Option<String>>,
    pub max_credits_per_semester: Option<Option<f64>>,
    pub status: Option<String>,
}

/// One student. Only the linked user id is stored; names and emails
/// live in the Users module. `advisor_id` is a lecturer id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Student {
    pub id: String,
    pub user_id: String,
    pub student_number: String,
    pub program_id: String,
    pub advisor_id: Option<String>,
    pub enrollment_year: i32,
    /// Status key from the `student_status` definitions.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateStudent {
    pub user_id: String,
    pub student_number: Option<String>,
    pub program_id: String,
    pub advisor_id: Option<String>,
    pub enrollment_year: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateStudent {
    pub program_id: Option<String>,
    pub advisor_id: Option<Option<String>>,
    pub status: Option<String>,
}

/// One lecturer (teacher). Only the linked user id is stored; names
/// and emails live in the Users module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lecturer {
    pub id: String,
    pub user_id: String,
    pub lecturer_number: String,
    pub department_id: Option<String>,
    pub title: Option<String>,
    /// Status key from the `lecturer_status` definitions.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateLecturer {
    pub user_id: String,
    pub lecturer_number: Option<String>,
    pub department_id: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateLecturer {
    pub department_id: Option<Option<String>>,
    pub title: Option<Option<String>>,
    pub status: Option<String>,
}

/// One academic subject (course record). This is the academic
/// record of a course, not the learning content: lessons, quizzes,
/// and attempts live in the E-Learning module, referenced through
/// `elearning_course_id` and verified via the
/// `elearning.course.requested` hook query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub id: String,
    pub code: String,
    pub title: String,
    pub program_id: String,
    pub credits: f64,
    pub semester_number: Option<i32>,
    pub elearning_course_id: Option<String>,
    /// True only when the e-learning hook answered that the linked
    /// course exists.
    #[serde(default)]
    pub elearning_course_verified: bool,
    /// `active` or `inactive`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSubject {
    pub code: String,
    pub title: String,
    pub program_id: String,
    pub credits: f64,
    pub semester_number: Option<i32>,
    pub elearning_course_id: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSubject {
    pub title: Option<String>,
    pub credits: Option<f64>,
    pub semester_number: Option<Option<i32>>,
    pub elearning_course_id: Option<Option<String>>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LinkElearningCourse {
    pub elearning_course_id: String,
}

/// One room where classes meet. Rooms are data: capacity and
/// building are stored here, and schedule conflicts are checked
/// against them on create.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub id: String,
    pub code: String,
    pub name: String,
    pub building: Option<String>,
    pub capacity: Option<i32>,
    /// `active` or `inactive`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRoom {
    pub code: String,
    pub name: String,
    pub building: Option<String>,
    pub capacity: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRoom {
    pub name: Option<String>,
    pub building: Option<Option<String>>,
    pub capacity: Option<Option<i32>>,
    pub status: Option<String>,
}

/// One semester (academic term).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Semester {
    pub id: String,
    pub code: String,
    pub name: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    /// `active`, `current`, or `completed`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSemester {
    pub code: String,
    pub name: String,
    pub start_date: String,
    pub end_date: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSemester {
    pub name: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub status: Option<String>,
}

/// One academic calendar entry: a holiday, an exam period, a
/// registration window, or any other dated event. `event_type` is a
/// key from the `calendar_event_type` definitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub semester_id: Option<String>,
    pub event_type: String,
    pub title: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateCalendarEvent {
    pub semester_id: Option<String>,
    pub event_type: String,
    pub title: String,
    pub start_date: String,
    pub end_date: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCalendarEvent {
    pub event_type: Option<String>,
    pub title: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub description: Option<Option<String>>,
}

/// One weekly class session. `day_of_week` follows ISO numbering
/// (1 = Monday, 7 = Sunday). Times are `HH:MM` (24-hour clock).
/// Conflicts (same room or same lecturer, overlapping times, same
/// semester) are rejected on create and update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub subject_id: String,
    pub lecturer_id: String,
    pub room_id: String,
    pub semester_id: String,
    pub day_of_week: i32,
    pub start_time: String,
    pub end_time: String,
    /// `active` or `inactive`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSchedule {
    pub subject_id: String,
    pub lecturer_id: String,
    pub room_id: String,
    pub semester_id: String,
    pub day_of_week: i32,
    pub start_time: String,
    pub end_time: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSchedule {
    pub lecturer_id: Option<String>,
    pub room_id: Option<String>,
    pub day_of_week: Option<i32>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub status: Option<String>,
}

/// One course registration: a student registers for a subject in a
/// semester. `credits` is snapshotted from the subject at request
/// time so later subject edits do not rewrite history. Status is a
/// key from the `enrollment_status` definitions; transitions follow
/// the `enrollment_transition` definitions (or their defaults).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Enrollment {
    pub id: String,
    pub student_id: String,
    pub subject_id: String,
    pub semester_id: String,
    pub credits: f64,
    /// Status key from the `enrollment_status` definitions.
    pub status: String,
    pub advisor_id: Option<String>,
    pub rejection_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateEnrollment {
    pub student_id: String,
    pub subject_id: String,
    pub semester_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RejectEnrollment {
    pub reason: String,
}

/// One class attendance record. `source` is `manual` for records
/// taken by hand and `timetracker` for records created from
/// TimeTracker events. `entry_id` holds the TimeTracker entry id
/// for de-duplication of repeated event delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attendance {
    pub id: String,
    pub schedule_id: String,
    pub student_id: String,
    pub date: NaiveDate,
    /// Status key from the `attendance_status` definitions.
    pub status: String,
    pub source: String,
    pub entry_id: Option<String>,
    pub minutes: Option<u64>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateAttendance {
    pub schedule_id: String,
    pub student_id: String,
    pub date: String,
    pub status: Option<String>,
    pub minutes: Option<u64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAttendance {
    pub status: Option<String>,
    pub minutes: Option<Option<u64>>,
    pub notes: Option<Option<String>>,
}

/// One grade component of a subject (for example a midterm worth
/// 30%). Weights are configurable per subject and do not have to
/// sum to 100: a final score normalizes over the components that
/// actually have grades. `kind` is a key from the
/// `grade_component_kind` definitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeComponent {
    pub id: String,
    pub subject_id: String,
    pub name: String,
    pub kind: String,
    pub weight_percent: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateGradeComponent {
    pub name: String,
    pub kind: String,
    pub weight_percent: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateGradeComponent {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub weight_percent: Option<f64>,
}

/// One recorded grade for one enrollment and one component. Scores
/// are 0-100.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grade {
    pub id: String,
    pub enrollment_id: String,
    pub component_id: String,
    pub score: f64,
    pub graded_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateGrade {
    pub component_id: String,
    pub score: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateGrade {
    pub score: f64,
}

/// One grade appeal. A student appeals a grade (or a whole
/// enrollment's final grade when `component_id` is absent); staff
/// resolve it. Resolving with decision `upheld` may carry a
/// `corrected_score` that rewrites the appealed grade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeAppeal {
    pub id: String,
    pub enrollment_id: String,
    pub component_id: Option<String>,
    pub student_id: String,
    pub reason: String,
    /// Status key from the `appeal_status` definitions.
    pub status: String,
    pub reviewed_by: Option<String>,
    pub decision: Option<String>,
    pub corrected_score: Option<f64>,
    pub resolution_notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateGradeAppeal {
    pub enrollment_id: String,
    pub component_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolveGradeAppeal {
    pub decision: String,
    pub corrected_score: Option<f64>,
    pub resolution_notes: Option<String>,
}

/// One issued official transcript. `lines` is the snapshot of the
/// transcript content (subjects, grades, credits, GPA) taken at
/// issue time, so later grade changes do not rewrite an issued
/// document. `print_job_id` is filled in only when the printing
/// module answers `print.job.requested`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub id: String,
    pub student_id: String,
    pub issued_by: String,
    pub gpa: f64,
    pub total_credits: f64,
    pub lines: Value,
    pub print_job_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalGrade {
    pub enrollment_id: String,
    pub student_id: String,
    pub subject_id: String,
    pub semester_id: String,
    pub credits: f64,
    pub weighted_score: Option<f64>,
    pub letter: Option<String>,
    pub points: Option<f64>,
    pub components: Vec<ComponentGrade>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentGrade {
    pub component_id: String,
    pub component_name: String,
    pub kind: String,
    pub weight_percent: f64,
    pub score: Option<f64>,
}

/// One thesis / final project. Status is a key from the
/// `thesis_status` definitions; transitions follow the
/// `thesis_transition` definitions (or their defaults).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thesis {
    pub id: String,
    pub student_id: String,
    pub title: String,
    pub advisor_id: String,
    pub co_advisor_id: Option<String>,
    /// Status key from the `thesis_status` definitions.
    pub status: String,
    pub proposal_date: Option<NaiveDate>,
    pub defense_date: Option<NaiveDate>,
    pub defense_result: Option<String>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateThesis {
    pub student_id: String,
    pub title: String,
    pub advisor_id: String,
    pub co_advisor_id: Option<String>,
    pub proposal_date: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateThesis {
    pub title: Option<String>,
    pub advisor_id: Option<String>,
    pub co_advisor_id: Option<Option<String>>,
    pub proposal_date: Option<Option<String>>,
    pub status: Option<String>,
    pub notes: Option<Option<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScheduleDefense {
    pub defense_date: String,
    pub result: Option<String>,
}

/// One academic standing record for a student in a semester.
/// `standing` is a key from the `standing_rule` definitions
/// (resolved by GPA range). Recomputed on demand; the latest record
/// per student and semester wins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StandingRecord {
    pub id: String,
    pub student_id: String,
    pub semester_id: String,
    pub gpa: f64,
    pub total_credits: f64,
    pub standing: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecomputeStanding {
    pub student_id: Option<String>,
    pub semester_id: String,
}

/// One admin-editable definition. `kind` names the vocabulary
/// (for example `grade_scale`, `standing_rule`,
/// `enrollment_status`); `key` is the vocabulary value;
/// `label` is the display label; `config` carries kind-specific
/// data (for a grade scale: `{min_score, max_score, points}`; for
/// a standing rule: `{min_gpa}` (half-open bands: the first rule, in
/// descending `min_gpa` order, whose minimum the GPA reaches wins);
/// for a transition: `{from, to}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcademicDefinition {
    pub id: String,
    pub kind: String,
    pub key: String,
    pub label: String,
    pub config: Value,
    pub active: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateDefinition {
    pub kind: String,
    pub key: String,
    pub label: String,
    pub config: Option<Value>,
    pub sort_order: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateDefinition {
    pub label: Option<String>,
    pub config: Option<Value>,
    pub active: Option<bool>,
    pub sort_order: Option<i32>,
}

/// One admin-editable scalar setting (a configurable default).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcademicSetting {
    pub key: String,
    pub value: Value,
    pub description: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSetting {
    pub value: Value,
    pub description: Option<String>,
}

/// A student's academic summary: GPA, credits, standing, and
/// counts, computed live from enrollments and grades.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudentSummary {
    pub student_id: String,
    pub gpa: f64,
    pub total_credits: f64,
    pub earned_credits: f64,
    pub standing: String,
    pub enrollments: usize,
    pub completed_enrollments: usize,
    pub attendance_present: usize,
    pub attendance_total: usize,
    pub attendance_rate: Option<f64>,
}
