// Academic System module (batch 13, builder B1).
//
// Students, lecturers, study programs, academic subjects, class
// schedules and rooms, course registration with advisor approval,
// attendance, grades and GPA, grade appeals, official transcripts,
// academic calendar, thesis / final-project tracking, and academic
// standing. Interconnection happens only through the hook bus — no
// module calls another module directly.
//
// Identity rule: a student or lecturer record stores the linked user
// id ONLY. Names, emails, and all other profile data live in the
// Users module and are resolved by callers through the Users API.
// This module never duplicates user records.
//
// E-learning rule: the E-Learning module owns course content
// (lessons, quizzes, attempts, certificates). An academic subject
// only REFERENCES an e-learning course id (`elearning_course_id`);
// it never stores lesson or quiz data. The reference is verified
// through the `elearning.course.requested` hook query (see the
// CONVERGENCE section below).
//
// Privacy rule (enforced server-side on every handler, never just
// filtered in a UI): a student sees ONLY their own academic data. A
// lecturer sees only the classes they teach and the students
// enrolled in them. Anything else requires `academic:read_all`.
// Records a caller may not read are answered with 404, so ids
// cannot be probed.
//
// Printing rule: transcripts are never printed from here. Issuing
// or printing a transcript only emits `print.job.requested` (and
// `academic.transcript.print_requested`) for the printing module;
// the transcript payload travels inside the event.
//
// Role-based access (enforced server-side on every handler):
//   - `academic:read`       — read academic records (own records
//                             for students; own classes for
//                             lecturers).
//   - `academic:read_all`   — read every student's academic data.
//   - `academic:manage`     — create/update/delete programs,
//                             subjects, schedules, rooms, semesters,
//                             calendar entries, definitions, and
//                             settings; approve or reject
//                             registrations; issue transcripts.
//   - `academic:advise`     — advisor approval of registrations and
//                             thesis advising for assigned students.
//   - `academic:grade`      — record grades for subjects the caller
//                             teaches.
//   - `academic:transcript` — issue official transcripts.
// Capabilities are checked through the `role.check` hook and fail
// closed: no answer means not allowed. The admin role holds `"*":
// ["*"]`, so it passes every check. No `academic:*` capability is
// granted by the default roles in the Roles module; an administrator
// must grant them.
//
// Modularity rule: no business list, rule, or status vocabulary is
// hard-coded. Status vocabularies, grade scales, standing rules,
// and state-machine transitions live in the admin-editable
// `academic_definitions` table (kind + key + JSON config) and are
// exposed through the definitions endpoints. The constants in this
// file marked CONFIGURABLE DEFAULT are only fallbacks used when no
// definition exists yet for that kind; an administrator can replace
// them at runtime through the API without touching code. Credit
// limits are per program (`max_credits_per_semester`) with a
// configurable default from `academic_settings`. Grade component
// weights are configurable per subject. GPA thresholds live in
// standing-rule definitions.
//
// Events emitted:
//   - `academic.student.created`        {student_id, user_id, program_id, application_id}
//                                       (`application_id` is the
//                                       admissions application the
//                                       student was created from, or
//                                       null for a directly created
//                                       student; admissions links back
//                                       on this key.)
//   - `academic.lecturer.created`       {lecturer_id, user_id}
//   - `academic.program.created`        {program_id, code}
//   - `academic.subject.created`        {subject_id, code, program_id}
//   - `academic.subject.updated`        {subject_id}
//   - `academic.schedule.created`       {schedule_id, subject_id, room_id, semester_id}
//   - `academic.enrollment.requested`   {enrollment_id, student_id, subject_id, semester_id, credits}
//   - `academic.enrollment.approved`    {enrollment_id, student_id, subject_id, semester_id}
//   - `academic.enrollment.rejected`    {enrollment_id, student_id, subject_id, reason}
//   - `academic.enrollment.dropped`     {enrollment_id, student_id, subject_id}
//   - `academic.enrollment.completed`   {enrollment_id, student_id, subject_id}
//   - `academic.attendance.recorded`    {attendance_id, student_id, schedule_id, date, status, source}
//   - `academic.grade.recorded`         {grade_id, enrollment_id, component_id, score}
//   - `academic.grade.appeal_filed`     {appeal_id, enrollment_id, student_id}
//   - `academic.grade.appeal_resolved`  {appeal_id, enrollment_id, decision}
//   - `academic.transcript.issued`      {transcript_id, student_id, gpa, total_credits}
//   - `academic.transcript.print_requested` {transcript_id, student_id}
//   - `print.job.requested`             {source: "academic", document_type: "transcript", transcript_id, student_id, title}
//   - `academic.thesis.created`         {thesis_id, student_id, advisor_id}
//   - `academic.thesis.defense_scheduled` {thesis_id, student_id, defense_date}
//   - `academic.thesis.status_changed`  {thesis_id, student_id, from_status, to_status}
//   - `academic.standing.changed`       {student_id, semester_id, standing, gpa}
//   - `notification.requested`          registration decisions, appeal
//                                       outcomes, defense scheduling,
//                                       and standing warnings (the
//                                       Notifications module listens).
// Events consumed:
//   - `timetracker.time.logged`         the event the TimeTracker
//                                       module in this tree actually
//                                       emits (payload: `{entry_id,
//                                       user_id, started_at, ended_at,
//                                       duration_secs, hours, billable,
//                                       source, ...}`).
//   - `timetracker.entry_logged`        the event name the brief
//                                       documents (payload: `{user_id,
//                                       date, minutes, source}`).
//                                       Both shapes are accepted; a
//                                       matching class session on that
//                                       date records attendance with
//                                       source "timetracker".
//   - `elearning.course.created` / `elearning.course.updated` /
//     `elearning.course.deleted`        refresh the verified flag of
//                                       linked subjects.
//   - `elearning.enrollment.completed`  `{enrollment_id, course_id,
//                                       user_id}` — marks a linked
//                                       academic enrollment completed.
//   - `academic.student.create_requested` `{application_id,
//                                       period_id, applicant_user_id
//                                       (or `user_id`), full_name,
//                                       email, phone, program_id,
//                                       level, academic_year, term,
//                                       enrollment_year?}` — sent by
//                                       admissions when an accepted
//                                       application is enrolled; this
//                                       module creates the student
//                                       record and answers with
//                                       `academic.student.created`
//                                       carrying the same
//                                       `application_id`.
//   - `academic.student.lookup`         `{student_ref}` (also
//                                       accepts `student_id` /
//                                       `user_id`) — hook query
//                                       answered with one result
//                                       `{exists, student_id?,
//                                       user_id?, program_id?}`; the
//                                       payload key `exists` is also
//                                       set for callers that read the
//                                       payload instead of results.
//   - `calendar.items_requested`        `{tenant_id, company_id,
//                                       user_id, start, end,
//                                       modules?}` (RFC 3339 UTC
//                                       bounds) — answered with the
//                                       academic calendar events,
//                                       class-schedule occurrences,
//                                       and thesis defenses in the
//                                       window, in the shared calendar
//                                       item shape
//                                       (`{id, module: "academic",
//                                       title, start, end?, all_day,
//                                       label?, deep_link?,
//                                       visibility: "shared",
//                                       owner_user_id?}`); the global
//                                       calendar aggregates, this
//                                       module never renders a
//                                       calendar view of its own.
// Hook queries emitted (answers read, never subscribed):
//   - `elearning.course.requested`      `{course_id}` — expects
//                                       results shaped `{exists,
//                                       course_id, title, status}`.
//   - `role.check`                      capability checks (fail
//                                       closed).
//
// CONVERGENCE (for the integrator):
//   1. No `elearning.course.requested` handler exists in this tree
//      yet (the E-Learning module only subscribes to order and
//      anchor events). A subject link is therefore stored with
//      `elearning_course_verified: false` when nothing answers, and
//      the link is never rejected on that basis. The integrator
//      should add the answering handler to the E-Learning module
//      (or to the API layer) with the payload above.
//   2. No printing module exists in this tree yet, so no handler
//      consumes `print.job.requested` today. The event is the
//      contract: `{source: "academic", document_type:
//      "transcript", transcript_id, student_id, title, issued_at}`.
//      The printing module should answer it and, when it prints,
//      may emit `print.job.completed` with `{transcript_id}`.
//   3. `src/modules/mod.rs` must gain `pub mod academic;` and an
//      `academic::register(registry);` line in `register_all` — this
//      file cannot do that itself (mod.rs is owned by the
//      integrator). Until then the module compiles but is not wired
//      into the binary.
//   4. The audit module only records events listed in its
//      `AUDITED_EVENTS`; the `academic.*` names above must be added
//      there for academic actions to appear in the audit trail.
//   5. Seeded definition rows are CONFIGURABLE DEFAULTS: they are
//      inserted by the migration and used only when no admin
//      definition exists for that kind. Deleting a seeded row and
//      restarting does not resurrect it in memory, but the
//      migration seed remains the database default for fresh
//      installs.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Capabilities (checked through the `role.check` hook; fail closed).
// ---------------------------------------------------------------------------

/// Read academic records (own records for students; own classes for
/// lecturers).
pub const PERM_ACADEMIC_READ: &str = "academic:read";
/// Read every student's academic data.
pub const PERM_ACADEMIC_READ_ALL: &str = "academic:read_all";
/// Manage programs, subjects, schedules, rooms, semesters, calendar,
/// definitions, and settings; approve or reject registrations; issue
/// transcripts.
pub const PERM_ACADEMIC_MANAGE: &str = "academic:manage";
/// Advisor approval of registrations and thesis advising for
/// assigned students.
pub const PERM_ACADEMIC_ADVISE: &str = "academic:advise";
/// Record grades for subjects the caller teaches.
pub const PERM_ACADEMIC_GRADE: &str = "academic:grade";
/// Issue official transcripts.
pub const PERM_ACADEMIC_TRANSCRIPT: &str = "academic:transcript";

/// Longest free-text field accepted (titles, notes, reasons).
const MAX_TEXT: usize = 2000;
/// Longest short code accepted (program codes, subject codes, room
/// codes, student numbers).
const MAX_CODE: usize = 64;
/// A day cannot exceed 24 hours (attendance minutes from TimeTracker).
const MAX_MINUTES_PER_DAY: u64 = 1440;
/// Highest score accepted on any grade component.
const MAX_SCORE: f64 = 100.0;

// --- CONFIGURABLE DEFAULTS (see the module header) -----------------------
// These values apply only when no admin definition or setting exists
// yet. They are defaults, not hard-coded business rules: every one of
// them can be replaced at runtime through the definitions or settings
// endpoints.

/// Default maximum credits a student may register per semester,
/// used when a program has no `max_credits_per_semester` and no
/// `default_max_credits_per_semester` setting exists.
const DEFAULT_MAX_CREDITS_PER_SEMESTER: f64 = 24.0;
/// Default passing grade points, used when no grade-scale definition
/// exists (a final letter with fewer points than this does not earn
/// credit towards graduation totals).
const DEFAULT_PASSING_POINTS: f64 = 2.0;
/// Default grade scale, used when no `grade_scale` definitions exist.
/// Each entry is (letter, minimum score, points).
const DEFAULT_GRADE_SCALE: [(&str, f64, f64); 5] = [
    ("A", 90.0, 4.0),
    ("B", 80.0, 3.0),
    ("C", 70.0, 2.0),
    ("D", 60.0, 1.0),
    ("F", 0.0, 0.0),
];
/// Default standing rules, used when no `standing_rule` definitions
/// exist. Each entry is (standing key, minimum GPA, maximum GPA).
const DEFAULT_STANDING_RULES: [(&str, f64, f64); 4] = [
    ("good", 2.5, 4.0),
    ("warning", 2.0, 2.4999),
    ("probation", 1.0, 1.9999),
    ("dismissed", 0.0, 0.9999),
];
/// Default student statuses, used when no `student_status`
/// definitions exist.
const DEFAULT_STUDENT_STATUSES: [&str; 4] = ["active", "on_leave", "graduated", "withdrawn"];
/// Default lecturer statuses, used when no `lecturer_status`
/// definitions exist.
const DEFAULT_LECTURER_STATUSES: [&str; 3] = ["active", "on_leave", "retired"];
/// Default registration statuses, used when no `enrollment_status`
/// definitions exist.
const DEFAULT_ENROLLMENT_STATUSES: [&str; 5] =
    ["requested", "approved", "rejected", "dropped", "completed"];
/// Default attendance statuses, used when no `attendance_status`
/// definitions exist.
const DEFAULT_ATTENDANCE_STATUSES: [&str; 4] = ["present", "absent", "late", "excused"];
/// Default grade-component kinds, used when no
/// `grade_component_kind` definitions exist.
const DEFAULT_COMPONENT_KINDS: [&str; 6] =
    ["midterm", "final", "assignment", "quiz", "project", "participation"];
/// Default thesis statuses, used when no `thesis_status`
/// definitions exist.
const DEFAULT_THESIS_STATUSES: [&str; 6] = [
    "proposal",
    "in_progress",
    "defense_scheduled",
    "defended",
    "completed",
    "rejected",
];
/// Default appeal statuses, used when no `appeal_status`
/// definitions exist.
const DEFAULT_APPEAL_STATUSES: [&str; 4] = ["open", "under_review", "upheld", "denied"];
/// Default calendar event types, used when no `calendar_event_type`
/// definitions exist.
const DEFAULT_CALENDAR_EVENT_TYPES: [&str; 6] = [
    "holiday",
    "exam_period",
    "semester_start",
    "semester_end",
    "registration",
    "other",
];
/// Default thesis transitions, used when no `thesis_transition`
/// definitions exist. Each entry is (from status, to status).
const DEFAULT_THESIS_TRANSITIONS: [(&str, &str); 7] = [
    ("proposal", "in_progress"),
    ("proposal", "rejected"),
    ("in_progress", "defense_scheduled"),
    ("in_progress", "rejected"),
    ("defense_scheduled", "defended"),
    ("defense_scheduled", "in_progress"),
    ("defended", "completed"),
];
/// Default registration transitions, used when no
/// `enrollment_transition` definitions exist. Each entry is (from
/// status, to status).
const DEFAULT_ENROLLMENT_TRANSITIONS: [(&str, &str); 6] = [
    ("requested", "approved"),
    ("requested", "rejected"),
    ("requested", "dropped"),
    ("approved", "dropped"),
    ("approved", "completed"),
    ("rejected", "requested"),
];

// Definition kinds (the admin-editable vocabulary registry).
const KIND_STUDENT_STATUS: &str = "student_status";
const KIND_LECTURER_STATUS: &str = "lecturer_status";
const KIND_ENROLLMENT_STATUS: &str = "enrollment_status";
const KIND_ATTENDANCE_STATUS: &str = "attendance_status";
const KIND_COMPONENT_KIND: &str = "grade_component_kind";
const KIND_GRADE_SCALE: &str = "grade_scale";
const KIND_STANDING_RULE: &str = "standing_rule";
const KIND_THESIS_STATUS: &str = "thesis_status";
const KIND_THESIS_TRANSITION: &str = "thesis_transition";
const KIND_ENROLLMENT_TRANSITION: &str = "enrollment_transition";
const KIND_APPEAL_STATUS: &str = "appeal_status";
const KIND_CALENDAR_EVENT_TYPE: &str = "calendar_event_type";

// Settings keys (admin-editable scalar defaults).
const SETTING_DEFAULT_MAX_CREDITS: &str = "default_max_credits_per_semester";
const SETTING_DEFAULT_PASSING_POINTS: &str = "default_passing_points";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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
/// a standing rule: `{min_gpa, max_gpa}`; for a transition:
/// `{from, to}`).
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

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// In-memory store. The migration tables are the persistence
/// targets; this store is the live read path (the same split the
/// E-Learning module uses).
#[derive(Debug)]
pub struct AcademicState {
    hooks: RwLock<Arc<HookBus>>,
    programs: DashMap<String, Program>,
    students: DashMap<String, Student>,
    lecturers: DashMap<String, Lecturer>,
    subjects: DashMap<String, Subject>,
    rooms: DashMap<String, Room>,
    semesters: DashMap<String, Semester>,
    calendar_events: DashMap<String, CalendarEvent>,
    schedules: DashMap<String, Schedule>,
    enrollments: DashMap<String, Enrollment>,
    attendance: DashMap<String, Attendance>,
    grade_components: DashMap<String, GradeComponent>,
    grades: DashMap<String, Grade>,
    grade_appeals: DashMap<String, GradeAppeal>,
    transcripts: DashMap<String, Transcript>,
    theses: DashMap<String, Thesis>,
    standing_records: DashMap<String, StandingRecord>,
    definitions: DashMap<String, AcademicDefinition>,
    settings: DashMap<String, AcademicSetting>,
}

impl AcademicState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            programs: DashMap::new(),
            students: DashMap::new(),
            lecturers: DashMap::new(),
            subjects: DashMap::new(),
            rooms: DashMap::new(),
            semesters: DashMap::new(),
            calendar_events: DashMap::new(),
            schedules: DashMap::new(),
            enrollments: DashMap::new(),
            attendance: DashMap::new(),
            grade_components: DashMap::new(),
            grades: DashMap::new(),
            grade_appeals: DashMap::new(),
            transcripts: DashMap::new(),
            theses: DashMap::new(),
            standing_records: DashMap::new(),
            definitions: DashMap::new(),
            settings: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn notify(&self, user_id: &str, title: &str, body: &str) {
        self.emit(
            "notification.requested",
            json!({
                "user_id": user_id,
                "title": title,
                "body": body,
                "kind": "academic",
            }),
        );
    }

    fn student_by_user(&self, user_id: &str) -> Option<Student> {
        self.students
            .iter()
            .find(|entry| entry.value().user_id == user_id)
            .map(|entry| entry.value().clone())
    }

    fn lecturer_by_user(&self, user_id: &str) -> Option<Lecturer> {
        self.lecturers
            .iter()
            .find(|entry| entry.value().user_id == user_id)
            .map(|entry| entry.value().clone())
    }

    /// Ask the E-Learning module whether a course exists
    /// (`elearning.course.requested`). Returns the answering result
    /// when one arrives, or `None` when nothing answers (no handler
    /// exists in this tree yet — see the module header).
    fn elearning_course(&self, course_id: &str) -> Option<Value> {
        let context = self
            .bus()
            .emit("elearning.course.requested", json!({ "course_id": course_id }));
        context.results.into_iter().find(|result| {
            result
                .get("course_id")
                .and_then(|value| value.as_str())
                == Some(course_id)
        })
    }

    /// True when the e-learning hook answers that the course exists.
    fn elearning_course_exists(&self, course_id: &str) -> bool {
        self.elearning_course(course_id)
            .and_then(|result| result.get("exists").and_then(|value| value.as_bool()))
            .unwrap_or(false)
    }

    /// Active definitions of one kind, in sort order.
    fn definitions_for(&self, kind: &str) -> Vec<AcademicDefinition> {
        let mut items: Vec<AcademicDefinition> = self
            .definitions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|definition| definition.kind == kind && definition.active)
            .collect();
        items.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| a.key.cmp(&b.key))
        });
        items
    }

    /// True when `value` is an allowed key for `kind`: an active
    /// definition key when definitions exist, otherwise one of the
    /// built-in configurable defaults for that kind.
    fn valid_status(&self, kind: &str, value: &str) -> bool {
        let defined = self.definitions_for(kind);
        if !defined.is_empty() {
            return defined.iter().any(|definition| definition.key == value);
        }
        match kind {
            KIND_STUDENT_STATUS => DEFAULT_STUDENT_STATUSES.contains(&value),
            KIND_LECTURER_STATUS => DEFAULT_LECTURER_STATUSES.contains(&value),
            KIND_ENROLLMENT_STATUS => DEFAULT_ENROLLMENT_STATUSES.contains(&value),
            KIND_ATTENDANCE_STATUS => DEFAULT_ATTENDANCE_STATUSES.contains(&value),
            KIND_COMPONENT_KIND => DEFAULT_COMPONENT_KINDS.contains(&value),
            KIND_THESIS_STATUS => DEFAULT_THESIS_STATUSES.contains(&value),
            KIND_APPEAL_STATUS => DEFAULT_APPEAL_STATUSES.contains(&value),
            KIND_CALENDAR_EVENT_TYPE => DEFAULT_CALENDAR_EVENT_TYPES.contains(&value),
            _ => false,
        }
    }

    /// The grade scale in force: admin definitions when any exist,
    /// otherwise the configurable default scale. Each entry is
    /// (letter, minimum score, points), ordered by minimum score
    /// descending.
    fn grade_scale(&self) -> Vec<(String, f64, f64)> {
        let defined = self.definitions_for(KIND_GRADE_SCALE);
        if !defined.is_empty() {
            let mut scale: Vec<(String, f64, f64)> = defined
                .iter()
                .map(|definition| {
                    let min = definition
                        .config
                        .get("min_score")
                        .and_then(|value| value.as_f64())
                        .unwrap_or(0.0);
                    let points = definition
                        .config
                        .get("points")
                        .and_then(|value| value.as_f64())
                        .unwrap_or(0.0);
                    (definition.key.clone(), min, points)
                })
                .collect();
            scale.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            return scale;
        }
        DEFAULT_GRADE_SCALE
            .iter()
            .map(|(letter, min, points)| (letter.to_string(), *min, *points))
            .collect()
    }

    /// The standing rules in force: admin definitions when any
    /// exist, otherwise the configurable default rules. Each entry
    /// is (standing key, minimum GPA, maximum GPA).
    fn standing_rules(&self) -> Vec<(String, f64, f64)> {
        let defined = self.definitions_for(KIND_STANDING_RULE);
        if !defined.is_empty() {
            return defined
                .iter()
                .map(|definition| {
                    let min = definition
                        .config
                        .get("min_gpa")
                        .and_then(|value| value.as_f64())
                        .unwrap_or(0.0);
                    let max = definition
                        .config
                        .get("max_gpa")
                        .and_then(|value| value.as_f64())
                        .unwrap_or(4.0);
                    (definition.key.clone(), min, max)
                })
                .collect();
        }
        DEFAULT_STANDING_RULES
            .iter()
            .map(|(key, min, max)| (key.to_string(), *min, *max))
            .collect()
    }

    /// Allowed state transitions for a kind: admin `*_transition`
    /// definitions when any exist, otherwise the configurable
    /// default table.
    fn allowed_transitions(&self, kind: &str) -> Vec<(String, String)> {
        let defined = self.definitions_for(kind);
        if !defined.is_empty() {
            return defined
                .iter()
                .filter_map(|definition| {
                    let from = definition
                        .config
                        .get("from")
                        .and_then(|value| value.as_str())?
                        .to_string();
                    let to = definition
                        .config
                        .get("to")
                        .and_then(|value| value.as_str())?
                        .to_string();
                    Some((from, to))
                })
                .collect();
        }
        let table = match kind {
            KIND_THESIS_TRANSITION => &DEFAULT_THESIS_TRANSITIONS[..],
            KIND_ENROLLMENT_TRANSITION => &DEFAULT_ENROLLMENT_TRANSITIONS[..],
            _ => &[][..],
        };
        table
            .iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect()
    }

    /// The default maximum credits per semester: the setting when
    /// set, otherwise the configurable default.
    fn default_max_credits(&self) -> f64 {
        self.settings
            .get(SETTING_DEFAULT_MAX_CREDITS)
            .and_then(|entry| entry.value().value.as_f64())
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(DEFAULT_MAX_CREDITS_PER_SEMESTER)
    }

    /// The passing grade points: the setting when set, otherwise
    /// the configurable default.
    fn passing_points(&self) -> f64 {
        self.settings
            .get(SETTING_DEFAULT_PASSING_POINTS)
            .and_then(|entry| entry.value().value.as_f64())
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(DEFAULT_PASSING_POINTS)
    }

    /// The credit cap for one program: the program value when set,
    /// otherwise the default maximum credits.
    fn credit_cap_for(&self, program_id: &str) -> f64 {
        self.programs
            .get(program_id)
            .and_then(|entry| entry.value().max_credits_per_semester)
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or_else(|| self.default_max_credits())
    }

    /// Credits already registered (requested or approved) by one
    /// student in one semester.
    fn registered_credits(&self, student_id: &str, semester_id: &str) -> f64 {
        self.enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| {
                enrollment.student_id == student_id
                    && enrollment.semester_id == semester_id
                    && matches!(enrollment.status.as_str(), "requested" | "approved")
            })
            .map(|enrollment| enrollment.credits)
            .sum()
    }

    /// The final grade of one enrollment: the weight-normalized
    /// score over the components that have grades, with the letter
    /// and points from the grade scale in force. `None` scores mean
    /// no component has been graded yet.
    fn final_grade(&self, enrollment: &Enrollment) -> FinalGrade {
        let components: Vec<GradeComponent> = self
            .grade_components
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|component| component.subject_id == enrollment.subject_id)
            .collect();
        let mut component_grades: Vec<ComponentGrade> = Vec::new();
        let mut weighted_sum = 0.0;
        let mut weight_total = 0.0;
        for component in &components {
            let grade = self
                .grades
                .iter()
                .map(|entry| entry.value().clone())
                .find(|grade| {
                    grade.enrollment_id == enrollment.id && grade.component_id == component.id
                });
            if let Some(grade) = &grade {
                weighted_sum += grade.score * component.weight_percent;
                weight_total += component.weight_percent;
            }
            component_grades.push(ComponentGrade {
                component_id: component.id.clone(),
                component_name: component.name.clone(),
                kind: component.kind.clone(),
                weight_percent: component.weight_percent,
                score: grade.map(|grade| grade.score),
            });
        }
        let weighted_score = if weight_total > 0.0 {
            Some((weighted_sum / weight_total * 100.0).round() / 100.0)
        } else {
            None
        };
        let (letter, points) = match weighted_score {
            Some(score) => {
                let scale = self.grade_scale();
                let found = scale
                    .iter()
                    .find(|(_, min, _)| score >= *min)
                    .map(|(letter, _, points)| (Some(letter.clone()), Some(*points)));
                found.unwrap_or((None, None))
            }
            None => (None, None),
        };
        FinalGrade {
            enrollment_id: enrollment.id.clone(),
            student_id: enrollment.student_id.clone(),
            subject_id: enrollment.subject_id.clone(),
            semester_id: enrollment.semester_id.clone(),
            credits: enrollment.credits,
            weighted_score,
            letter,
            points,
            components: component_grades,
        }
    }

    /// Cumulative GPA over every enrollment of one student that has
    /// a final grade, plus earned credits (credits whose points
    /// reach the passing threshold) and total graded credits.
    fn gpa_for(&self, student_id: &str) -> (f64, f64, f64) {
        let passing = self.passing_points();
        let mut points_sum = 0.0;
        let mut credits_sum = 0.0;
        let mut earned = 0.0;
        for entry in self.enrollments.iter() {
            let enrollment = entry.value();
            if enrollment.student_id != student_id {
                continue;
            }
            let grade = self.final_grade(enrollment);
            if let Some(points) = grade.points {
                points_sum += points * enrollment.credits;
                credits_sum += enrollment.credits;
                if points >= passing {
                    earned += enrollment.credits;
                }
            }
        }
        let gpa = if credits_sum > 0.0 {
            (points_sum / credits_sum * 100.0).round() / 100.0
        } else {
            0.0
        };
        (gpa, credits_sum, earned)
    }

    /// The standing key for a GPA, from the standing rules in force.
    fn standing_for_gpa(&self, gpa: f64) -> String {
        for (key, min, max) in self.standing_rules() {
            if gpa >= min && gpa <= max {
                return key;
            }
        }
        "good".to_string()
    }

    /// True when two weekly sessions overlap (same day, intersecting
    /// time ranges).
    fn sessions_overlap(a_day: i32, a_start: i32, a_end: i32, b_day: i32, b_start: i32, b_end: i32) -> bool {
        a_day == b_day && a_start < b_end && b_start < a_end
    }

    /// Find a conflicting schedule for a new or updated session:
    /// same room or same lecturer, same semester, overlapping time.
    fn schedule_conflict(
        &self,
        semester_id: &str,
        room_id: &str,
        lecturer_id: &str,
        day_of_week: i32,
        start_minutes: i32,
        end_minutes: i32,
        exclude_id: Option<&str>,
    ) -> Option<Schedule> {
        self.schedules
            .iter()
            .map(|entry| entry.value().clone())
            .find(|schedule| {
                schedule.semester_id == semester_id
                    && schedule.status == "active"
                    && exclude_id != Some(schedule.id.as_str())
                    && (schedule.room_id == room_id || schedule.lecturer_id == lecturer_id)
                    && Self::sessions_overlap(
                        schedule.day_of_week,
                        time_to_minutes(&schedule.start_time).unwrap_or(0),
                        time_to_minutes(&schedule.end_time).unwrap_or(0),
                        day_of_week,
                        start_minutes,
                        end_minutes,
                    )
            })
    }
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn today() -> NaiveDate {
    Utc::now().date_naive()
}

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// or id the capability check runs against.
#[derive(Debug, Clone)]
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through the
/// `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// True when the caller holds any staff capability for academic data.
fn is_staff(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_ACADEMIC_READ_ALL)
        || check_capability(ctx, role, PERM_ACADEMIC_MANAGE)
        || check_capability(ctx, role, PERM_ACADEMIC_ADVISE)
}

/// True when `caller` may read `student`'s records: staff, the
/// student themself, or the student's assigned advisor.
fn can_read_student(
    state: &AcademicState,
    ctx: &AppContext,
    caller: &Caller,
    student: &Student,
) -> bool {
    if check_capability(ctx, &caller.role, PERM_ACADEMIC_READ_ALL) {
        return true;
    }
    if student.user_id == caller.actor {
        return true;
    }
    if let Some(advisor_id) = &student.advisor_id {
        if let Some(lecturer) = state.lecturer_by_user(&caller.actor) {
            if lecturer.id == *advisor_id {
                return true;
            }
        }
    }
    false
}

/// Enforce read access for one student record, hiding the not-found
/// case: a caller who may not read the record gets 404, so student
/// ids cannot be probed.
fn require_read_student(
    state: &AcademicState,
    ctx: &AppContext,
    caller: &Caller,
    student: &Student,
) -> UnifiedResult<()> {
    if can_read_student(state, ctx, caller, student) {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Student not found"))
    }
}

fn load_student_for_read(
    state: &AcademicState,
    ctx: &AppContext,
    caller: &Caller,
    student_id: &str,
) -> UnifiedResult<Student> {
    let student = state
        .students
        .get(student_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
    require_read_student(state, ctx, caller, &student)?;
    Ok(student)
}

/// True when `caller` teaches `subject` (through an active schedule
/// for the subject).
fn teaches_subject(state: &AcademicState, caller: &Caller, subject_id: &str) -> bool {
    let Some(lecturer) = state.lecturer_by_user(&caller.actor) else {
        return false;
    };
    state.schedules.iter().any(|entry| {
        let schedule = entry.value();
        schedule.subject_id == subject_id
            && schedule.lecturer_id == lecturer.id
            && schedule.status == "active"
    })
}

/// True when `caller` may grade `enrollment`: staff with the grade
/// capability who teaches the subject, or a manager.
fn can_grade(
    state: &AcademicState,
    ctx: &AppContext,
    caller: &Caller,
    enrollment: &Enrollment,
) -> bool {
    if check_capability(ctx, &caller.role, PERM_ACADEMIC_MANAGE) {
        return true;
    }
    check_capability(ctx, &caller.role, PERM_ACADEMIC_GRADE)
        && teaches_subject(state, caller, &enrollment.subject_id)
}

/// True when `caller` may approve `enrollment`: a manager, or the
/// student's advisor holding the advise capability.
fn can_approve_enrollment(
    state: &AcademicState,
    ctx: &AppContext,
    caller: &Caller,
    enrollment: &Enrollment,
) -> bool {
    if check_capability(ctx, &caller.role, PERM_ACADEMIC_MANAGE) {
        return true;
    }
    if !check_capability(ctx, &caller.role, PERM_ACADEMIC_ADVISE) {
        return false;
    }
    let Some(student) = state.students.get(&enrollment.student_id) else {
        return false;
    };
    let Some(advisor_id) = &student.advisor_id else {
        return false;
    };
    state
        .lecturer_by_user(&caller.actor)
        .is_some_and(|lecturer| lecturer.id == *advisor_id)
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

fn require_code(value: &str, field: &str) -> UnifiedResult<String> {
    let code = require_text(value, field, MAX_CODE)?;
    if !code
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
    {
        return Err(UnifiedError::bad_request(format!(
            "{field} must use only letters, digits, '-', '_' and '.'"
        )));
    }
    Ok(code)
}

fn parse_date(raw: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date (YYYY-MM-DD)")))
}

fn parse_optional_date(raw: &Option<String>, field: &str) -> UnifiedResult<Option<NaiveDate>> {
    match raw {
        None => Ok(None),
        Some(value) => Ok(Some(parse_date(value, field)?)),
    }
}

/// Parse `HH:MM` (24-hour clock) into minutes since midnight.
fn time_to_minutes(raw: &str) -> Option<i32> {
    let (hour, minute) = raw.trim().split_once(':')?;
    let hour: i32 = hour.parse().ok()?;
    let minute: i32 = minute.parse().ok()?;
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    Some(hour * 60 + minute)
}

fn parse_time(raw: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = raw.trim();
    if time_to_minutes(trimmed).is_none() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a time (HH:MM, 24-hour clock)"
        )));
    }
    Ok(trimmed.to_string())
}

fn parse_credits(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value <= 0.0 || value > 60.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and 60 credits"
        )));
    }
    Ok((value * 100.0).round() / 100.0)
}

fn parse_weight(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value <= 0.0 || value > 100.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and 100"
        )));
    }
    Ok((value * 100.0).round() / 100.0)
}

fn parse_score(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value > MAX_SCORE {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and {MAX_SCORE}"
        )));
    }
    Ok((value * 100.0).round() / 100.0)
}

fn parse_year(value: i32, field: &str) -> UnifiedResult<i32> {
    if !(1990..=2100).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a year between 1990 and 2100"
        )));
    }
    Ok(value)
}

fn parse_day_of_week(value: i32) -> UnifiedResult<i32> {
    if !(1..=7).contains(&value) {
        return Err(UnifiedError::bad_request(
            "day_of_week must be 1 (Monday) to 7 (Sunday)",
        ));
    }
    Ok(value)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn filter_matches(params: &PaginationParams, key: &str, value: &str) -> bool {
    params
        .filters
        .get(key)
        .is_none_or(|expected| expected == value)
}

// ---------------------------------------------------------------------------
// Handlers: programs
// ---------------------------------------------------------------------------

async fn list_programs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Program>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Program> = state
            .programs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|program| {
                matches_search(&format!("{} {}", program.code, program.name), &params)
                    && filter_matches(&params, "status", &program.status)
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateProgram>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Program> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let code = require_code(&body.code, "code")?;
        let name = require_text(&body.name, "name", MAX_TEXT)?;
        if state
            .programs
            .iter()
            .any(|entry| entry.value().code.eq_ignore_ascii_case(&code))
        {
            return Err(UnifiedError::conflict(format!(
                "A program with code \"{code}\" already exists"
            )));
        }
        let max_credits = match body.max_credits_per_semester {
            None => None,
            Some(value) => Some(parse_credits(value, "max_credits_per_semester")?),
        };
        let status = match &body.status {
            None => "active".to_string(),
            Some(value) => {
                let status = require_text(value, "status", MAX_CODE)?;
                if status != "active" && status != "inactive" {
                    return Err(UnifiedError::bad_request(
                        "status must be \"active\" or \"inactive\"",
                    ));
                }
                status
            }
        };
        let program = Program {
            id: new_id(),
            code: code.clone(),
            name,
            department_id: optional_text(&body.department_id, "department_id", MAX_CODE)?,
            max_credits_per_semester: max_credits,
            status,
            created_at: now(),
            updated_at: now(),
        };
        state.programs.insert(program.id.clone(), program.clone());
        state.emit(
            "academic.program.created",
            json!({ "program_id": program.id, "code": program.code }),
        );
        Ok(program)
    })();
    json::respond(result, debug)
}

async fn get_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Program> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .programs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Program not found"))
    })();
    json::respond(result, debug)
}

async fn update_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProgram>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Program> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut program = state
            .programs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Program not found"))?;
        if let Some(name) = &body.name {
            program.name = require_text(name, "name", MAX_TEXT)?;
        }
        if let Some(department_id) = &body.department_id {
            program.department_id = optional_text(department_id, "department_id", MAX_CODE)?;
        }
        if let Some(max_credits) = &body.max_credits_per_semester {
            program.max_credits_per_semester = match max_credits {
                None => None,
                Some(value) => Some(parse_credits(*value, "max_credits_per_semester")?),
            };
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if status != "active" && status != "inactive" {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\" or \"inactive\"",
                ));
            }
            program.status = status;
        }
        program.updated_at = now();
        state.programs.insert(program.id.clone(), program.clone());
        Ok(program)
    })();
    json::respond(result, debug)
}

async fn delete_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.programs.contains_key(&id) {
            return Err(UnifiedError::not_found("Program not found"));
        }
        if state
            .students
            .iter()
            .any(|entry| entry.value().program_id == id)
        {
            return Err(UnifiedError::conflict(
                "Program cannot be deleted while students are enrolled in it",
            ));
        }
        if state
            .subjects
            .iter()
            .any(|entry| entry.value().program_id == id)
        {
            return Err(UnifiedError::conflict(
                "Program cannot be deleted while subjects belong to it",
            ));
        }
        state.programs.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: students
// ---------------------------------------------------------------------------

async fn list_students(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Student>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own = state.student_by_user(&caller.actor);
        let advisor = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Student> = state
            .students
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|student| {
                let visible = read_all
                    || own.as_ref().is_some_and(|me| me.id == student.id)
                    || advisor.as_ref().is_some_and(|lecturer| {
                        student.advisor_id.as_deref() == Some(lecturer.id.as_str())
                    });
                visible
                    && matches_search(
                        &format!("{} {}", student.student_number, student.user_id),
                        &params,
                    )
                    && filter_matches(&params, "status", &student.status)
                    && filter_matches(&params, "program_id", &student.program_id)
            })
            .collect();
        items.sort_by(|a, b| a.student_number.cmp(&b.student_number));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_student(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateStudent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Student> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let user_id = require_text(&body.user_id, "user_id", MAX_CODE)?;
        if state.student_by_user(&user_id).is_some() {
            return Err(UnifiedError::conflict(
                "A student record already exists for this user",
            ));
        }
        if !state.programs.contains_key(&body.program_id) {
            return Err(UnifiedError::not_found("Program not found"));
        }
        if let Some(advisor_id) = &body.advisor_id {
            if !state.lecturers.contains_key(advisor_id) {
                return Err(UnifiedError::not_found("Advisor not found"));
            }
        }
        let student_number = match &body.student_number {
            Some(number) => require_code(number, "student_number")?,
            None => format!("STU-{}", &new_id()[..8].to_uppercase()),
        };
        if state
            .students
            .iter()
            .any(|entry| entry.value().student_number == student_number)
        {
            return Err(UnifiedError::conflict(format!(
                "A student with number \"{student_number}\" already exists"
            )));
        }
        let student = Student {
            id: new_id(),
            user_id: user_id.clone(),
            student_number,
            program_id: body.program_id.clone(),
            advisor_id: body.advisor_id.clone(),
            enrollment_year: parse_year(body.enrollment_year, "enrollment_year")?,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.students.insert(student.id.clone(), student.clone());
        state.emit(
            "academic.student.created",
            json!({
                "student_id": student.id,
                "user_id": student.user_id,
                "program_id": student.program_id,
                // No admissions application backs a directly created
                // student; the key is always present so consumers
                // can rely on one canonical payload shape.
                "application_id": Value::Null,
            }),
        );
        Ok(student)
    })();
    json::respond(result, debug)
}

async fn get_student(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Student> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        load_student_for_read(&state, &ctx, &caller, path.as_str())
    })();
    json::respond(result, debug)
}

async fn update_student(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateStudent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Student> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut student = state
            .students
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
        if let Some(program_id) = &body.program_id {
            if !state.programs.contains_key(program_id) {
                return Err(UnifiedError::not_found("Program not found"));
            }
            student.program_id = program_id.clone();
        }
        if let Some(advisor_id) = &body.advisor_id {
            match advisor_id {
                None => student.advisor_id = None,
                Some(id) => {
                    if !state.lecturers.contains_key(id) {
                        return Err(UnifiedError::not_found("Advisor not found"));
                    }
                    student.advisor_id = Some(id.clone());
                }
            }
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if !state.valid_status(KIND_STUDENT_STATUS, &status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown student status \"{status}\""
                )));
            }
            student.status = status;
        }
        student.updated_at = now();
        state.students.insert(student.id.clone(), student.clone());
        Ok(student)
    })();
    json::respond(result, debug)
}

async fn delete_student(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.students.contains_key(&id) {
            return Err(UnifiedError::not_found("Student not found"));
        }
        if state
            .enrollments
            .iter()
            .any(|entry| entry.value().student_id == id)
        {
            return Err(UnifiedError::conflict(
                "Student cannot be deleted while enrollments exist",
            ));
        }
        if state
            .theses
            .iter()
            .any(|entry| entry.value().student_id == id)
        {
            return Err(UnifiedError::conflict(
                "Student cannot be deleted while a thesis record exists",
            ));
        }
        state.students.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn get_student_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StudentSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let student = load_student_for_read(&state, &ctx, &caller, path.as_str())?;
        let (gpa, total_credits, earned_credits) = state.gpa_for(&student.id);
        let enrollments: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| enrollment.student_id == student.id)
            .collect();
        let completed = enrollments
            .iter()
            .filter(|enrollment| enrollment.status == "completed")
            .count();
        let attendance: Vec<Attendance> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| record.student_id == student.id)
            .collect();
        let present = attendance
            .iter()
            .filter(|record| record.status == "present" || record.status == "late")
            .count();
        let rate = if attendance.is_empty() {
            None
        } else {
            Some((present as f64 / attendance.len() as f64 * 100.0).round() / 100.0)
        };
        Ok(StudentSummary {
            student_id: student.id.clone(),
            gpa,
            total_credits,
            earned_credits,
            standing: state.standing_for_gpa(gpa),
            enrollments: enrollments.len(),
            completed_enrollments: completed,
            attendance_present: present,
            attendance_total: attendance.len(),
            attendance_rate: rate,
        })
    })();
    json::respond(result, debug)
}

async fn get_student_gpa(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let student = load_student_for_read(&state, &ctx, &caller, path.as_str())?;
        let (gpa, total_credits, earned_credits) = state.gpa_for(&student.id);
        Ok(json!({
            "student_id": student.id,
            "gpa": gpa,
            "total_credits": total_credits,
            "earned_credits": earned_credits,
            "standing": state.standing_for_gpa(gpa),
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: lecturers
// ---------------------------------------------------------------------------

async fn list_lecturers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Lecturer>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Lecturer> = state
            .lecturers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|lecturer| {
                let visible = read_all
                    || own.as_ref().is_some_and(|me| me.id == lecturer.id)
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && matches_search(
                        &format!("{} {}", lecturer.lecturer_number, lecturer.user_id),
                        &params,
                    )
                    && filter_matches(&params, "status", &lecturer.status)
            })
            .collect();
        items.sort_by(|a, b| a.lecturer_number.cmp(&b.lecturer_number));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_lecturer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateLecturer>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lecturer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let user_id = require_text(&body.user_id, "user_id", MAX_CODE)?;
        if state.lecturer_by_user(&user_id).is_some() {
            return Err(UnifiedError::conflict(
                "A lecturer record already exists for this user",
            ));
        }
        let lecturer_number = match &body.lecturer_number {
            Some(number) => require_code(number, "lecturer_number")?,
            None => format!("LEC-{}", &new_id()[..8].to_uppercase()),
        };
        if state
            .lecturers
            .iter()
            .any(|entry| entry.value().lecturer_number == lecturer_number)
        {
            return Err(UnifiedError::conflict(format!(
                "A lecturer with number \"{lecturer_number}\" already exists"
            )));
        }
        let lecturer = Lecturer {
            id: new_id(),
            user_id: user_id.clone(),
            lecturer_number,
            department_id: optional_text(&body.department_id, "department_id", MAX_CODE)?,
            title: optional_text(&body.title, "title", MAX_TEXT)?,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.lecturers.insert(lecturer.id.clone(), lecturer.clone());
        state.emit(
            "academic.lecturer.created",
            json!({ "lecturer_id": lecturer.id, "user_id": lecturer.user_id }),
        );
        Ok(lecturer)
    })();
    json::respond(result, debug)
}

async fn get_lecturer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lecturer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let lecturer = state
            .lecturers
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Lecturer not found"))?;
        let allowed = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL)
            || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)
            || lecturer.user_id == caller.actor;
        if !allowed {
            return Err(UnifiedError::not_found("Lecturer not found"));
        }
        Ok(lecturer)
    })();
    json::respond(result, debug)
}

async fn update_lecturer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateLecturer>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lecturer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut lecturer = state
            .lecturers
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Lecturer not found"))?;
        if let Some(department_id) = &body.department_id {
            lecturer.department_id = optional_text(department_id, "department_id", MAX_CODE)?;
        }
        if let Some(title) = &body.title {
            lecturer.title = optional_text(title, "title", MAX_TEXT)?;
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if !state.valid_status(KIND_LECTURER_STATUS, &status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown lecturer status \"{status}\""
                )));
            }
            lecturer.status = status;
        }
        lecturer.updated_at = now();
        state.lecturers.insert(lecturer.id.clone(), lecturer.clone());
        Ok(lecturer)
    })();
    json::respond(result, debug)
}

async fn delete_lecturer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.lecturers.contains_key(&id) {
            return Err(UnifiedError::not_found("Lecturer not found"));
        }
        if state
            .schedules
            .iter()
            .any(|entry| entry.value().lecturer_id == id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Lecturer cannot be deleted while assigned to active schedules",
            ));
        }
        if state
            .students
            .iter()
            .any(|entry| entry.value().advisor_id.as_deref() == Some(id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Lecturer cannot be deleted while advising students",
            ));
        }
        state.lecturers.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn list_lecturer_classes(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Schedule>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let lecturer_id = path.into_inner();
        let lecturer = state
            .lecturers
            .get(&lecturer_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Lecturer not found"))?;
        let allowed = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL)
            || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)
            || lecturer.user_id == caller.actor;
        if !allowed {
            return Err(UnifiedError::not_found("Lecturer not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<Schedule> = state
            .schedules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|schedule| {
                schedule.lecturer_id == lecturer_id
                    && filter_matches(&params, "semester_id", &schedule.semester_id)
                    && filter_matches(&params, "status", &schedule.status)
            })
            .collect();
        items.sort_by(|a, b| {
            a.day_of_week
                .cmp(&b.day_of_week)
                .then_with(|| a.start_time.cmp(&b.start_time))
        });
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: subjects
// ---------------------------------------------------------------------------

async fn list_subjects(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Subject>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Subject> = state
            .subjects
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|subject| {
                matches_search(&format!("{} {}", subject.code, subject.title), &params)
                    && filter_matches(&params, "status", &subject.status)
                    && filter_matches(&params, "program_id", &subject.program_id)
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateSubject>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Subject> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let code = require_code(&body.code, "code")?;
        let title = require_text(&body.title, "title", MAX_TEXT)?;
        if !state.programs.contains_key(&body.program_id) {
            return Err(UnifiedError::not_found("Program not found"));
        }
        if state
            .subjects
            .iter()
            .any(|entry| entry.value().code.eq_ignore_ascii_case(&code))
        {
            return Err(UnifiedError::conflict(format!(
                "A subject with code \"{code}\" already exists"
            )));
        }
        let credits = parse_credits(body.credits, "credits")?;
        let semester_number = match body.semester_number {
            None => None,
            Some(value) => {
                if !(1..=20).contains(&value) {
                    return Err(UnifiedError::bad_request(
                        "semester_number must be between 1 and 20",
                    ));
                }
                Some(value)
            }
        };
        let elearning_course_id = optional_text(
            &body.elearning_course_id,
            "elearning_course_id",
            MAX_CODE,
        )?;
        let verified = match &elearning_course_id {
            None => false,
            Some(course_id) => state.elearning_course_exists(course_id),
        };
        let status = match &body.status {
            None => "active".to_string(),
            Some(value) => {
                let status = require_text(value, "status", MAX_CODE)?;
                if status != "active" && status != "inactive" {
                    return Err(UnifiedError::bad_request(
                        "status must be \"active\" or \"inactive\"",
                    ));
                }
                status
            }
        };
        let subject = Subject {
            id: new_id(),
            code: code.clone(),
            title,
            program_id: body.program_id.clone(),
            credits,
            semester_number,
            elearning_course_id,
            elearning_course_verified: verified,
            status,
            created_at: now(),
            updated_at: now(),
        };
        state.subjects.insert(subject.id.clone(), subject.clone());
        state.emit(
            "academic.subject.created",
            json!({
                "subject_id": subject.id,
                "code": subject.code,
                "program_id": subject.program_id,
            }),
        );
        Ok(subject)
    })();
    json::respond(result, debug)
}

async fn get_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Subject> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .subjects
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subject not found"))
    })();
    json::respond(result, debug)
}

async fn update_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSubject>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Subject> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut subject = state
            .subjects
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subject not found"))?;
        if let Some(title) = &body.title {
            subject.title = require_text(title, "title", MAX_TEXT)?;
        }
        if let Some(credits) = body.credits {
            subject.credits = parse_credits(credits, "credits")?;
        }
        if let Some(semester_number) = &body.semester_number {
            subject.semester_number = match semester_number {
                None => None,
                Some(value) => {
                    if !(1..=20).contains(value) {
                        return Err(UnifiedError::bad_request(
                            "semester_number must be between 1 and 20",
                        ));
                    }
                    Some(*value)
                }
            };
        }
        if let Some(elearning_course_id) = &body.elearning_course_id {
            match elearning_course_id {
                None => {
                    subject.elearning_course_id = None;
                    subject.elearning_course_verified = false;
                }
                Some(course_id) => {
                    let course_id = require_text(course_id, "elearning_course_id", MAX_CODE)?;
                    subject.elearning_course_verified =
                        state.elearning_course_exists(&course_id);
                    subject.elearning_course_id = Some(course_id);
                }
            }
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if status != "active" && status != "inactive" {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\" or \"inactive\"",
                ));
            }
            subject.status = status;
        }
        subject.updated_at = now();
        state.subjects.insert(subject.id.clone(), subject.clone());
        state.emit(
            "academic.subject.updated",
            json!({ "subject_id": subject.id }),
        );
        Ok(subject)
    })();
    json::respond(result, debug)
}

async fn delete_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.subjects.contains_key(&id) {
            return Err(UnifiedError::not_found("Subject not found"));
        }
        if state
            .enrollments
            .iter()
            .any(|entry| entry.value().subject_id == id)
        {
            return Err(UnifiedError::conflict(
                "Subject cannot be deleted while enrollments exist",
            ));
        }
        if state
            .schedules
            .iter()
            .any(|entry| entry.value().subject_id == id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Subject cannot be deleted while active schedules exist",
            ));
        }
        state.subjects.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn link_elearning_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LinkElearningCourse>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Subject> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let course_id = require_text(&body.elearning_course_id, "elearning_course_id", MAX_CODE)?;
        let mut subject = state
            .subjects
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subject not found"))?;
        let answer = state.elearning_course(&course_id);
        let verified = answer
            .as_ref()
            .and_then(|result| result.get("exists").and_then(|value| value.as_bool()))
            .unwrap_or(false);
        subject.elearning_course_id = Some(course_id);
        subject.elearning_course_verified = verified;
        subject.updated_at = now();
        state.subjects.insert(subject.id.clone(), subject.clone());
        state.emit(
            "academic.subject.updated",
            json!({ "subject_id": subject.id, "elearning_course_verified": verified }),
        );
        Ok(subject)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: grade components
// ---------------------------------------------------------------------------

async fn list_grade_components(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<GradeComponent>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let subject_id = path.into_inner();
        if !state.subjects.contains_key(&subject_id) {
            return Err(UnifiedError::not_found("Subject not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<GradeComponent> = state
            .grade_components
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|component| {
                component.subject_id == subject_id
                    && filter_matches(&params, "kind", &component.kind)
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_grade_component(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateGradeComponent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GradeComponent> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let subject_id = path.into_inner();
        if !state.subjects.contains_key(&subject_id) {
            return Err(UnifiedError::not_found("Subject not found"));
        }
        let body = body.into_inner();
        let name = require_text(&body.name, "name", MAX_TEXT)?;
        let kind = require_text(&body.kind, "kind", MAX_CODE)?;
        if !state.valid_status(KIND_COMPONENT_KIND, &kind) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown grade component kind \"{kind}\""
            )));
        }
        let weight = parse_weight(body.weight_percent, "weight_percent")?;
        let component = GradeComponent {
            id: new_id(),
            subject_id,
            name,
            kind,
            weight_percent: weight,
            created_at: now(),
            updated_at: now(),
        };
        state
            .grade_components
            .insert(component.id.clone(), component.clone());
        Ok(component)
    })();
    json::respond(result, debug)
}

async fn update_grade_component(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateGradeComponent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GradeComponent> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut component = state
            .grade_components
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade component not found"))?;
        if let Some(name) = &body.name {
            component.name = require_text(name, "name", MAX_TEXT)?;
        }
        if let Some(kind) = &body.kind {
            let kind = require_text(kind, "kind", MAX_CODE)?;
            if !state.valid_status(KIND_COMPONENT_KIND, &kind) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown grade component kind \"{kind}\""
                )));
            }
            component.kind = kind;
        }
        if let Some(weight) = body.weight_percent {
            component.weight_percent = parse_weight(weight, "weight_percent")?;
        }
        component.updated_at = now();
        state
            .grade_components
            .insert(component.id.clone(), component.clone());
        Ok(component)
    })();
    json::respond(result, debug)
}

async fn delete_grade_component(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.grade_components.contains_key(&id) {
            return Err(UnifiedError::not_found("Grade component not found"));
        }
        if state
            .grades
            .iter()
            .any(|entry| entry.value().component_id == id)
        {
            return Err(UnifiedError::conflict(
                "Grade component cannot be deleted while grades reference it",
            ));
        }
        state.grade_components.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: rooms
// ---------------------------------------------------------------------------

async fn list_rooms(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Room>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Room> = state
            .rooms
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|room| {
                matches_search(&format!("{} {}", room.code, room.name), &params)
                    && filter_matches(&params, "status", &room.status)
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_room(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateRoom>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Room> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let code = require_code(&body.code, "code")?;
        let name = require_text(&body.name, "name", MAX_TEXT)?;
        if state
            .rooms
            .iter()
            .any(|entry| entry.value().code.eq_ignore_ascii_case(&code))
        {
            return Err(UnifiedError::conflict(format!(
                "A room with code \"{code}\" already exists"
            )));
        }
        let capacity = match body.capacity {
            None => None,
            Some(value) => {
                if value <= 0 || value > 10_000 {
                    return Err(UnifiedError::bad_request(
                        "capacity must be between 1 and 10000",
                    ));
                }
                Some(value)
            }
        };
        let room = Room {
            id: new_id(),
            code,
            name,
            building: optional_text(&body.building, "building", MAX_TEXT)?,
            capacity,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.rooms.insert(room.id.clone(), room.clone());
        Ok(room)
    })();
    json::respond(result, debug)
}

async fn get_room(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Room> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .rooms
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Room not found"))
    })();
    json::respond(result, debug)
}

async fn update_room(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRoom>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Room> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut room = state
            .rooms
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Room not found"))?;
        if let Some(name) = &body.name {
            room.name = require_text(name, "name", MAX_TEXT)?;
        }
        if let Some(building) = &body.building {
            room.building = optional_text(building, "building", MAX_TEXT)?;
        }
        if let Some(capacity) = &body.capacity {
            room.capacity = match capacity {
                None => None,
                Some(value) => {
                    if *value <= 0 || *value > 10_000 {
                        return Err(UnifiedError::bad_request(
                            "capacity must be between 1 and 10000",
                        ));
                    }
                    Some(*value)
                }
            };
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if status != "active" && status != "inactive" {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\" or \"inactive\"",
                ));
            }
            room.status = status;
        }
        room.updated_at = now();
        state.rooms.insert(room.id.clone(), room.clone());
        Ok(room)
    })();
    json::respond(result, debug)
}

async fn delete_room(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.rooms.contains_key(&id) {
            return Err(UnifiedError::not_found("Room not found"));
        }
        if state
            .schedules
            .iter()
            .any(|entry| entry.value().room_id == id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Room cannot be deleted while active schedules use it",
            ));
        }
        state.rooms.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: semesters
// ---------------------------------------------------------------------------

async fn list_semesters(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Semester>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Semester> = state
            .semesters
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|semester| {
                matches_search(&format!("{} {}", semester.code, semester.name), &params)
                    && filter_matches(&params, "status", &semester.status)
            })
            .collect();
        items.sort_by(|a, b| b.start_date.cmp(&a.start_date));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_semester(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateSemester>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Semester> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let code = require_code(&body.code, "code")?;
        let name = require_text(&body.name, "name", MAX_TEXT)?;
        if state
            .semesters
            .iter()
            .any(|entry| entry.value().code.eq_ignore_ascii_case(&code))
        {
            return Err(UnifiedError::conflict(format!(
                "A semester with code \"{code}\" already exists"
            )));
        }
        let start_date = parse_date(&body.start_date, "start_date")?;
        let end_date = parse_date(&body.end_date, "end_date")?;
        if end_date <= start_date {
            return Err(UnifiedError::bad_request(
                "end_date must be after start_date",
            ));
        }
        let semester = Semester {
            id: new_id(),
            code,
            name,
            start_date,
            end_date,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.semesters.insert(semester.id.clone(), semester.clone());
        Ok(semester)
    })();
    json::respond(result, debug)
}

async fn get_semester(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Semester> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .semesters
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Semester not found"))
    })();
    json::respond(result, debug)
}

async fn update_semester(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSemester>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Semester> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut semester = state
            .semesters
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Semester not found"))?;
        if let Some(name) = &body.name {
            semester.name = require_text(name, "name", MAX_TEXT)?;
        }
        if let Some(start_date) = &body.start_date {
            semester.start_date = parse_date(start_date, "start_date")?;
        }
        if let Some(end_date) = &body.end_date {
            semester.end_date = parse_date(end_date, "end_date")?;
        }
        if semester.end_date <= semester.start_date {
            return Err(UnifiedError::bad_request(
                "end_date must be after start_date",
            ));
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if !matches!(status.as_str(), "active" | "current" | "completed") {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\", \"current\", or \"completed\"",
                ));
            }
            semester.status = status;
        }
        semester.updated_at = now();
        state.semesters.insert(semester.id.clone(), semester.clone());
        Ok(semester)
    })();
    json::respond(result, debug)
}

async fn delete_semester(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.semesters.contains_key(&id) {
            return Err(UnifiedError::not_found("Semester not found"));
        }
        if state
            .enrollments
            .iter()
            .any(|entry| entry.value().semester_id == id)
        {
            return Err(UnifiedError::conflict(
                "Semester cannot be deleted while enrollments reference it",
            ));
        }
        if state
            .schedules
            .iter()
            .any(|entry| entry.value().semester_id == id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Semester cannot be deleted while active schedules reference it",
            ));
        }
        state.semesters.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn activate_semester(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Semester> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        let mut semester = state
            .semesters
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Semester not found"))?;
        // Only one semester is current at a time; the previous
        // current semester returns to active.
        for entry in state.semesters.iter() {
            if entry.value().status == "current" && entry.value().id != id {
                let mut previous = entry.value().clone();
                previous.status = "active".to_string();
                previous.updated_at = now();
                state.semesters.insert(previous.id.clone(), previous);
            }
        }
        semester.status = "current".to_string();
        semester.updated_at = now();
        state.semesters.insert(semester.id.clone(), semester.clone());
        Ok(semester)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: academic calendar
// ---------------------------------------------------------------------------

async fn list_calendar_events(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<CalendarEvent>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<CalendarEvent> = state
            .calendar_events
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|event| {
                matches_search(&event.title, &params)
                    && filter_matches(&params, "event_type", &event.event_type)
                    && params.filters.get("semester_id").is_none_or(|semester_id| {
                        event.semester_id.as_deref() == Some(semester_id.as_str())
                    })
            })
            .collect();
        items.sort_by(|a, b| a.start_date.cmp(&b.start_date));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_calendar_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateCalendarEvent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalendarEvent> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let event_type = require_text(&body.event_type, "event_type", MAX_CODE)?;
        if !state.valid_status(KIND_CALENDAR_EVENT_TYPE, &event_type) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown calendar event type \"{event_type}\""
            )));
        }
        if let Some(semester_id) = &body.semester_id {
            if !state.semesters.contains_key(semester_id) {
                return Err(UnifiedError::not_found("Semester not found"));
            }
        }
        let start_date = parse_date(&body.start_date, "start_date")?;
        let end_date = parse_date(&body.end_date, "end_date")?;
        if end_date < start_date {
            return Err(UnifiedError::bad_request(
                "end_date must not be before start_date",
            ));
        }
        let event = CalendarEvent {
            id: new_id(),
            semester_id: body.semester_id.clone(),
            event_type,
            title: require_text(&body.title, "title", MAX_TEXT)?,
            start_date,
            end_date,
            description: optional_text(&body.description, "description", MAX_TEXT)?,
            created_at: now(),
            updated_at: now(),
        };
        state.calendar_events.insert(event.id.clone(), event.clone());
        Ok(event)
    })();
    json::respond(result, debug)
}

async fn get_calendar_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalendarEvent> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .calendar_events
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Calendar event not found"))
    })();
    json::respond(result, debug)
}

async fn update_calendar_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCalendarEvent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalendarEvent> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut event = state
            .calendar_events
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Calendar event not found"))?;
        if let Some(event_type) = &body.event_type {
            let event_type = require_text(event_type, "event_type", MAX_CODE)?;
            if !state.valid_status(KIND_CALENDAR_EVENT_TYPE, &event_type) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown calendar event type \"{event_type}\""
                )));
            }
            event.event_type = event_type;
        }
        if let Some(title) = &body.title {
            event.title = require_text(title, "title", MAX_TEXT)?;
        }
        if let Some(start_date) = &body.start_date {
            event.start_date = parse_date(start_date, "start_date")?;
        }
        if let Some(end_date) = &body.end_date {
            event.end_date = parse_date(end_date, "end_date")?;
        }
        if event.end_date < event.start_date {
            return Err(UnifiedError::bad_request(
                "end_date must not be before start_date",
            ));
        }
        if let Some(description) = &body.description {
            event.description = optional_text(description, "description", MAX_TEXT)?;
        }
        event.updated_at = now();
        state.calendar_events.insert(event.id.clone(), event.clone());
        Ok(event)
    })();
    json::respond(result, debug)
}

async fn delete_calendar_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if state.calendar_events.remove(&id).is_none() {
            return Err(UnifiedError::not_found("Calendar event not found"));
        }
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: schedules
// ---------------------------------------------------------------------------

async fn list_schedules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Schedule>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_lecturer = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Schedule> = state
            .schedules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|schedule| {
                let visible = read_all
                    || own_lecturer.as_ref().is_some_and(|lecturer| {
                        lecturer.id == schedule.lecturer_id
                    })
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "subject_id", &schedule.subject_id)
                    && filter_matches(&params, "lecturer_id", &schedule.lecturer_id)
                    && filter_matches(&params, "room_id", &schedule.room_id)
                    && filter_matches(&params, "semester_id", &schedule.semester_id)
                    && filter_matches(&params, "status", &schedule.status)
            })
            .collect();
        items.sort_by(|a, b| {
            a.day_of_week
                .cmp(&b.day_of_week)
                .then_with(|| a.start_time.cmp(&b.start_time))
        });
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateSchedule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Schedule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        if !state.subjects.contains_key(&body.subject_id) {
            return Err(UnifiedError::not_found("Subject not found"));
        }
        if !state.lecturers.contains_key(&body.lecturer_id) {
            return Err(UnifiedError::not_found("Lecturer not found"));
        }
        if !state.rooms.contains_key(&body.room_id) {
            return Err(UnifiedError::not_found("Room not found"));
        }
        if !state.semesters.contains_key(&body.semester_id) {
            return Err(UnifiedError::not_found("Semester not found"));
        }
        let day_of_week = parse_day_of_week(body.day_of_week)?;
        let start_time = parse_time(&body.start_time, "start_time")?;
        let end_time = parse_time(&body.end_time, "end_time")?;
        let start_minutes = time_to_minutes(&start_time).unwrap_or(0);
        let end_minutes = time_to_minutes(&end_time).unwrap_or(0);
        if end_minutes <= start_minutes {
            return Err(UnifiedError::bad_request(
                "end_time must be after start_time",
            ));
        }
        if let Some(conflict) = state.schedule_conflict(
            &body.semester_id,
            &body.room_id,
            &body.lecturer_id,
            day_of_week,
            start_minutes,
            end_minutes,
            None,
        ) {
            return Err(UnifiedError::conflict(format!(
                "Schedule conflicts with existing schedule {} (room or lecturer already booked at that time)",
                conflict.id
            )));
        }
        let schedule = Schedule {
            id: new_id(),
            subject_id: body.subject_id.clone(),
            lecturer_id: body.lecturer_id.clone(),
            room_id: body.room_id.clone(),
            semester_id: body.semester_id.clone(),
            day_of_week,
            start_time,
            end_time,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.schedules.insert(schedule.id.clone(), schedule.clone());
        state.emit(
            "academic.schedule.created",
            json!({
                "schedule_id": schedule.id,
                "subject_id": schedule.subject_id,
                "room_id": schedule.room_id,
                "semester_id": schedule.semester_id,
            }),
        );
        Ok(schedule)
    })();
    json::respond(result, debug)
}

async fn get_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Schedule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let schedule = state
            .schedules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Schedule not found"))?;
        let allowed = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL)
            || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)
            || state
                .lecturer_by_user(&caller.actor)
                .is_some_and(|lecturer| lecturer.id == schedule.lecturer_id);
        if !allowed {
            return Err(UnifiedError::not_found("Schedule not found"));
        }
        Ok(schedule)
    })();
    json::respond(result, debug)
}

async fn update_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSchedule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Schedule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut schedule = state
            .schedules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Schedule not found"))?;
        if let Some(lecturer_id) = &body.lecturer_id {
            if !state.lecturers.contains_key(lecturer_id) {
                return Err(UnifiedError::not_found("Lecturer not found"));
            }
            schedule.lecturer_id = lecturer_id.clone();
        }
        if let Some(room_id) = &body.room_id {
            if !state.rooms.contains_key(room_id) {
                return Err(UnifiedError::not_found("Room not found"));
            }
            schedule.room_id = room_id.clone();
        }
        if let Some(day_of_week) = body.day_of_week {
            schedule.day_of_week = parse_day_of_week(day_of_week)?;
        }
        if let Some(start_time) = &body.start_time {
            schedule.start_time = parse_time(start_time, "start_time")?;
        }
        if let Some(end_time) = &body.end_time {
            schedule.end_time = parse_time(end_time, "end_time")?;
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if status != "active" && status != "inactive" {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\" or \"inactive\"",
                ));
            }
            schedule.status = status;
        }
        let start_minutes = time_to_minutes(&schedule.start_time).unwrap_or(0);
        let end_minutes = time_to_minutes(&schedule.end_time).unwrap_or(0);
        if end_minutes <= start_minutes {
            return Err(UnifiedError::bad_request(
                "end_time must be after start_time",
            ));
        }
        if schedule.status == "active" {
            if let Some(conflict) = state.schedule_conflict(
                &schedule.semester_id,
                &schedule.room_id,
                &schedule.lecturer_id,
                schedule.day_of_week,
                start_minutes,
                end_minutes,
                Some(&schedule.id),
            ) {
                return Err(UnifiedError::conflict(format!(
                    "Schedule conflicts with existing schedule {} (room or lecturer already booked at that time)",
                    conflict.id
                )));
            }
        }
        schedule.updated_at = now();
        state.schedules.insert(schedule.id.clone(), schedule.clone());
        Ok(schedule)
    })();
    json::respond(result, debug)
}

async fn delete_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.schedules.contains_key(&id) {
            return Err(UnifiedError::not_found("Schedule not found"));
        }
        if state
            .attendance
            .iter()
            .any(|entry| entry.value().schedule_id == id)
        {
            return Err(UnifiedError::conflict(
                "Schedule cannot be deleted while attendance records exist",
            ));
        }
        state.schedules.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: enrollments (course registration)
// ---------------------------------------------------------------------------

async fn list_enrollments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Enrollment>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_student = state.student_by_user(&caller.actor);
        let own_lecturer = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| {
                let visible = read_all
                    || own_student
                        .as_ref()
                        .is_some_and(|student| student.id == enrollment.student_id)
                    || own_lecturer.as_ref().is_some_and(|_| {
                        teaches_subject(&state, &caller, &enrollment.subject_id)
                    })
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "student_id", &enrollment.student_id)
                    && filter_matches(&params, "subject_id", &enrollment.subject_id)
                    && filter_matches(&params, "semester_id", &enrollment.semester_id)
                    && filter_matches(&params, "status", &enrollment.status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn list_student_enrollments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Enrollment>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let student = load_student_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| {
                enrollment.student_id == student.id
                    && filter_matches(&params, "subject_id", &enrollment.subject_id)
                    && filter_matches(&params, "semester_id", &enrollment.semester_id)
                    && filter_matches(&params, "status", &enrollment.status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateEnrollment>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let body = body.into_inner();
        let student = state
            .students
            .get(&body.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
        // A student registers themself; staff may register any
        // student. Anyone else gets the same 404 as a missing
        // student, so ids cannot be probed.
        let self_registration = student.user_id == caller.actor;
        let staff = is_staff(&ctx, &caller.role);
        if !self_registration && !staff {
            return Err(UnifiedError::not_found("Student not found"));
        }
        if student.status != "active" {
            return Err(UnifiedError::conflict(
                "Only students with status \"active\" may register for courses",
            ));
        }
        let subject = state
            .subjects
            .get(&body.subject_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subject not found"))?;
        if subject.status != "active" {
            return Err(UnifiedError::conflict(
                "Only active subjects accept registrations",
            ));
        }
        if subject.program_id != student.program_id {
            return Err(UnifiedError::bad_request(
                "Subject belongs to a different program than the student",
            ));
        }
        let semester = state
            .semesters
            .get(&body.semester_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Semester not found"))?;
        if semester.status == "completed" {
            return Err(UnifiedError::conflict(
                "Registrations are closed for a completed semester",
            ));
        }
        if state.enrollments.iter().any(|entry| {
            let enrollment = entry.value();
            enrollment.student_id == student.id
                && enrollment.subject_id == subject.id
                && enrollment.semester_id == semester.id
                && matches!(enrollment.status.as_str(), "requested" | "approved")
        }) {
            return Err(UnifiedError::conflict(
                "Student is already registered for this subject in this semester",
            ));
        }
        let cap = state.credit_cap_for(&student.program_id);
        let used = state.registered_credits(&student.id, &semester.id);
        if used + subject.credits > cap {
            return Err(UnifiedError::conflict(format!(
                "Credit limit exceeded: {used} registered + {} for this subject exceeds the cap of {cap} credits for this program",
                subject.credits
            )));
        }
        let enrollment = Enrollment {
            id: new_id(),
            student_id: student.id.clone(),
            subject_id: subject.id.clone(),
            semester_id: semester.id.clone(),
            credits: subject.credits,
            status: "requested".to_string(),
            advisor_id: student.advisor_id.clone(),
            rejection_reason: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .enrollments
            .insert(enrollment.id.clone(), enrollment.clone());
        state.emit(
            "academic.enrollment.requested",
            json!({
                "enrollment_id": enrollment.id,
                "student_id": enrollment.student_id,
                "subject_id": enrollment.subject_id,
                "semester_id": enrollment.semester_id,
                "credits": enrollment.credits,
            }),
        );
        Ok(enrollment)
    })();
    json::respond(result, debug)
}

async fn get_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let student = state
            .students
            .get(&enrollment.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let visible = can_read_student(&state, &ctx, &caller, &student)
            || teaches_subject(&state, &caller, &enrollment.subject_id);
        if !visible {
            return Err(UnifiedError::not_found("Enrollment not found"));
        }
        Ok(enrollment)
    })();
    json::respond(result, debug)
}

fn transition_enrollment(
    state: &AcademicState,
    enrollment: &mut Enrollment,
    to: &str,
) -> UnifiedResult<()> {
    let allowed = state.allowed_transitions(KIND_ENROLLMENT_TRANSITION);
    if !allowed
        .iter()
        .any(|(from, target)| from == &enrollment.status && target == to)
    {
        return Err(UnifiedError::conflict(format!(
            "Enrollment cannot move from \"{}\" to \"{to}\"",
            enrollment.status
        )));
    }
    enrollment.status = to.to_string();
    enrollment.updated_at = now();
    Ok(())
}

async fn approve_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let caller = caller(&req)?;
        let mut enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        if !can_approve_enrollment(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the student's advisor or a manager may approve this registration",
            ));
        }
        transition_enrollment(&state, &mut enrollment, "approved")?;
        state
            .enrollments
            .insert(enrollment.id.clone(), enrollment.clone());
        state.emit(
            "academic.enrollment.approved",
            json!({
                "enrollment_id": enrollment.id,
                "student_id": enrollment.student_id,
                "subject_id": enrollment.subject_id,
                "semester_id": enrollment.semester_id,
            }),
        );
        if let Some(student) = state.students.get(&enrollment.student_id) {
            state.notify(
                &student.user_id,
                "Course registration approved",
                "Your course registration was approved by your advisor.",
            );
        }
        Ok(enrollment)
    })();
    json::respond(result, debug)
}

async fn reject_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RejectEnrollment>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let caller = caller(&req)?;
        let reason = require_text(&body.reason, "reason", MAX_TEXT)?;
        let mut enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        if !can_approve_enrollment(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the student's advisor or a manager may reject this registration",
            ));
        }
        transition_enrollment(&state, &mut enrollment, "rejected")?;
        enrollment.rejection_reason = Some(reason.clone());
        state
            .enrollments
            .insert(enrollment.id.clone(), enrollment.clone());
        state.emit(
            "academic.enrollment.rejected",
            json!({
                "enrollment_id": enrollment.id,
                "student_id": enrollment.student_id,
                "subject_id": enrollment.subject_id,
                "reason": reason,
            }),
        );
        if let Some(student) = state.students.get(&enrollment.student_id) {
            state.notify(
                &student.user_id,
                "Course registration rejected",
                &format!("Your course registration was rejected: {reason}"),
            );
        }
        Ok(enrollment)
    })();
    json::respond(result, debug)
}

async fn drop_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let caller = caller(&req)?;
        let mut enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let student = state
            .students
            .get(&enrollment.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let own = student.user_id == caller.actor;
        let staff = is_staff(&ctx, &caller.role);
        if !own && !staff {
            return Err(UnifiedError::not_found("Enrollment not found"));
        }
        if enrollment.status == "completed" {
            return Err(UnifiedError::conflict(
                "A completed enrollment cannot be dropped",
            ));
        }
        transition_enrollment(&state, &mut enrollment, "dropped")?;
        state
            .enrollments
            .insert(enrollment.id.clone(), enrollment.clone());
        state.emit(
            "academic.enrollment.dropped",
            json!({
                "enrollment_id": enrollment.id,
                "student_id": enrollment.student_id,
                "subject_id": enrollment.subject_id,
            }),
        );
        Ok(enrollment)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: attendance
// ---------------------------------------------------------------------------

async fn list_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Attendance>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_student = state.student_by_user(&caller.actor);
        let own_lecturer = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Attendance> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                let visible = read_all
                    || own_student
                        .as_ref()
                        .is_some_and(|student| student.id == record.student_id)
                    || own_lecturer.as_ref().is_some_and(|lecturer| {
                        state
                            .schedules
                            .get(&record.schedule_id)
                            .is_some_and(|schedule| schedule.lecturer_id == lecturer.id)
                    })
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "schedule_id", &record.schedule_id)
                    && filter_matches(&params, "student_id", &record.student_id)
                    && filter_matches(&params, "status", &record.status)
                    && filter_matches(&params, "source", &record.source)
                    && params.filters.get("date").is_none_or(|date| {
                        record.date.format("%Y-%m-%d").to_string() == *date
                    })
            })
            .collect();
        items.sort_by(|a, b| b.date.cmp(&a.date));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateAttendance>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Attendance> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let schedule = state
            .schedules
            .get(&body.schedule_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Schedule not found"))?;
        let student = state
            .students
            .get(&body.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
        if !state.enrollments.iter().any(|entry| {
            let enrollment = entry.value();
            enrollment.student_id == student.id
                && enrollment.subject_id == schedule.subject_id
                && enrollment.semester_id == schedule.semester_id
                && matches!(enrollment.status.as_str(), "approved" | "completed")
        }) {
            return Err(UnifiedError::conflict(
                "Student is not enrolled in this class",
            ));
        }
        let date = parse_date(&body.date, "date")?;
        if date > today() {
            return Err(UnifiedError::bad_request(
                "Attendance cannot be recorded for a future date",
            ));
        }
        let status = match &body.status {
            None => "present".to_string(),
            Some(value) => {
                let status = require_text(value, "status", MAX_CODE)?;
                if !state.valid_status(KIND_ATTENDANCE_STATUS, &status) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown attendance status \"{status}\""
                    )));
                }
                status
            }
        };
        if state.attendance.iter().any(|entry| {
            let record = entry.value();
            record.schedule_id == schedule.id
                && record.student_id == student.id
                && record.date == date
        }) {
            return Err(UnifiedError::conflict(
                "Attendance for this student, class, and date already exists",
            ));
        }
        let minutes = match body.minutes {
            None => None,
            Some(value) => {
                if value > MAX_MINUTES_PER_DAY {
                    return Err(UnifiedError::bad_request(
                        "minutes must not exceed 1440",
                    ));
                }
                Some(value)
            }
        };
        let record = Attendance {
            id: new_id(),
            schedule_id: schedule.id.clone(),
            student_id: student.id.clone(),
            date,
            status: status.clone(),
            source: "manual".to_string(),
            entry_id: None,
            minutes,
            notes: optional_text(&body.notes, "notes", MAX_TEXT)?,
            created_at: now(),
            updated_at: now(),
        };
        state.attendance.insert(record.id.clone(), record.clone());
        state.emit(
            "academic.attendance.recorded",
            json!({
                "attendance_id": record.id,
                "student_id": record.student_id,
                "schedule_id": record.schedule_id,
                "date": record.date.format("%Y-%m-%d").to_string(),
                "status": record.status,
                "source": record.source,
            }),
        );
        Ok(record)
    })();
    json::respond(result, debug)
}

async fn get_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Attendance> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let record = state
            .attendance
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attendance record not found"))?;
        let student = state
            .students
            .get(&record.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attendance record not found"))?;
        if !can_read_student(&state, &ctx, &caller, &student) {
            return Err(UnifiedError::not_found("Attendance record not found"));
        }
        Ok(record)
    })();
    json::respond(result, debug)
}

async fn update_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAttendance>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Attendance> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut record = state
            .attendance
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attendance record not found"))?;
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if !state.valid_status(KIND_ATTENDANCE_STATUS, &status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown attendance status \"{status}\""
                )));
            }
            record.status = status;
        }
        if let Some(minutes) = &body.minutes {
            record.minutes = match minutes {
                None => None,
                Some(value) => {
                    if *value > MAX_MINUTES_PER_DAY {
                        return Err(UnifiedError::bad_request(
                            "minutes must not exceed 1440",
                        ));
                    }
                    Some(*value)
                }
            };
        }
        if let Some(notes) = &body.notes {
            record.notes = optional_text(notes, "notes", MAX_TEXT)?;
        }
        record.updated_at = now();
        state.attendance.insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    json::respond(result, debug)
}

async fn list_schedule_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Attendance>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let schedule_id = path.into_inner();
        let schedule = state
            .schedules
            .get(&schedule_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Schedule not found"))?;
        let allowed = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL)
            || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)
            || state
                .lecturer_by_user(&caller.actor)
                .is_some_and(|lecturer| lecturer.id == schedule.lecturer_id);
        if !allowed {
            return Err(UnifiedError::not_found("Schedule not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<Attendance> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                record.schedule_id == schedule_id
                    && filter_matches(&params, "student_id", &record.student_id)
                    && filter_matches(&params, "status", &record.status)
                    && params.filters.get("date").is_none_or(|date| {
                        record.date.format("%Y-%m-%d").to_string() == *date
                    })
            })
            .collect();
        items.sort_by(|a, b| b.date.cmp(&a.date));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: grades
// ---------------------------------------------------------------------------

async fn list_grades_for_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Grade>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let student = state
            .students
            .get(&enrollment.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let visible = can_read_student(&state, &ctx, &caller, &student)
            || teaches_subject(&state, &caller, &enrollment.subject_id);
        if !visible {
            return Err(UnifiedError::not_found("Enrollment not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<Grade> = state
            .grades
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|grade| grade.enrollment_id == enrollment.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_grade(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateGrade>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Grade> = (|| {
        let caller = caller(&req)?;
        let enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        if !can_grade(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the subject's lecturer or a manager may record grades",
            ));
        }
        if !matches!(enrollment.status.as_str(), "approved" | "completed") {
            return Err(UnifiedError::conflict(
                "Grades can only be recorded for approved or completed enrollments",
            ));
        }
        let body = body.into_inner();
        let component = state
            .grade_components
            .get(&body.component_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade component not found"))?;
        if component.subject_id != enrollment.subject_id {
            return Err(UnifiedError::bad_request(
                "Grade component belongs to a different subject",
            ));
        }
        if state.grades.iter().any(|entry| {
            let grade = entry.value();
            grade.enrollment_id == enrollment.id && grade.component_id == component.id
        }) {
            return Err(UnifiedError::conflict(
                "A grade for this enrollment and component already exists; update it instead",
            ));
        }
        let grade = Grade {
            id: new_id(),
            enrollment_id: enrollment.id.clone(),
            component_id: component.id.clone(),
            score: parse_score(body.score, "score")?,
            graded_by: state
                .lecturer_by_user(&caller.actor)
                .map(|lecturer| lecturer.id),
            created_at: now(),
            updated_at: now(),
        };
        state.grades.insert(grade.id.clone(), grade.clone());
        state.emit(
            "academic.grade.recorded",
            json!({
                "grade_id": grade.id,
                "enrollment_id": grade.enrollment_id,
                "component_id": grade.component_id,
                "score": grade.score,
            }),
        );
        Ok(grade)
    })();
    json::respond(result, debug)
}

async fn update_grade(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateGrade>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Grade> = (|| {
        let caller = caller(&req)?;
        let mut grade = state
            .grades
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
        let enrollment = state
            .enrollments
            .get(&grade.enrollment_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
        if !can_grade(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the subject's lecturer or a manager may update grades",
            ));
        }
        grade.score = parse_score(body.score, "score")?;
        grade.updated_at = now();
        state.grades.insert(grade.id.clone(), grade.clone());
        state.emit(
            "academic.grade.recorded",
            json!({
                "grade_id": grade.id,
                "enrollment_id": grade.enrollment_id,
                "component_id": grade.component_id,
                "score": grade.score,
            }),
        );
        Ok(grade)
    })();
    json::respond(result, debug)
}

async fn delete_grade(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let grade = state
            .grades
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
        let enrollment = state
            .enrollments
            .get(&grade.enrollment_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
        if !can_grade(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the subject's lecturer or a manager may delete grades",
            ));
        }
        state.grades.remove(&grade.id);
        Ok(json!({ "deleted": true, "id": grade.id }))
    })();
    json::respond(result, debug)
}

async fn get_final_grade(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FinalGrade> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let student = state
            .students
            .get(&enrollment.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let visible = can_read_student(&state, &ctx, &caller, &student)
            || teaches_subject(&state, &caller, &enrollment.subject_id);
        if !visible {
            return Err(UnifiedError::not_found("Enrollment not found"));
        }
        Ok(state.final_grade(&enrollment))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: grade appeals
// ---------------------------------------------------------------------------

async fn list_grade_appeals(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<GradeAppeal>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_student = state.student_by_user(&caller.actor);
        let mut items: Vec<GradeAppeal> = state
            .grade_appeals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|appeal| {
                let visible = read_all
                    || own_student
                        .as_ref()
                        .is_some_and(|student| student.id == appeal.student_id)
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "enrollment_id", &appeal.enrollment_id)
                    && filter_matches(&params, "student_id", &appeal.student_id)
                    && filter_matches(&params, "status", &appeal.status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_grade_appeal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateGradeAppeal>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GradeAppeal> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let body = body.into_inner();
        let enrollment = state
            .enrollments
            .get(&body.enrollment_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let student = state
            .students
            .get(&enrollment.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        // A student appeals their own grade; staff may file for any
        // student. Anyone else gets the same 404 as a missing
        // enrollment.
        if student.user_id != caller.actor && !is_staff(&ctx, &caller.role) {
            return Err(UnifiedError::not_found("Enrollment not found"));
        }
        if let Some(component_id) = &body.component_id {
            let component = state
                .grade_components
                .get(component_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found("Grade component not found"))?;
            if component.subject_id != enrollment.subject_id {
                return Err(UnifiedError::bad_request(
                    "Grade component belongs to a different subject",
                ));
            }
            if !state.grades.iter().any(|entry| {
                let grade = entry.value();
                grade.enrollment_id == enrollment.id && grade.component_id == component.id
            }) {
                return Err(UnifiedError::conflict(
                    "No grade exists yet for the appealed component",
                ));
            }
        }
        if state.grade_appeals.iter().any(|entry| {
            let appeal = entry.value();
            appeal.enrollment_id == enrollment.id
                && appeal.component_id == body.component_id
                && matches!(appeal.status.as_str(), "open" | "under_review")
        }) {
            return Err(UnifiedError::conflict(
                "An open appeal already exists for this grade",
            ));
        }
        let appeal = GradeAppeal {
            id: new_id(),
            enrollment_id: enrollment.id.clone(),
            component_id: body.component_id.clone(),
            student_id: student.id.clone(),
            reason: require_text(&body.reason, "reason", MAX_TEXT)?,
            status: "open".to_string(),
            reviewed_by: None,
            decision: None,
            corrected_score: None,
            resolution_notes: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .grade_appeals
            .insert(appeal.id.clone(), appeal.clone());
        state.emit(
            "academic.grade.appeal_filed",
            json!({
                "appeal_id": appeal.id,
                "enrollment_id": appeal.enrollment_id,
                "student_id": appeal.student_id,
            }),
        );
        Ok(appeal)
    })();
    json::respond(result, debug)
}

async fn get_grade_appeal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GradeAppeal> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let appeal = state
            .grade_appeals
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade appeal not found"))?;
        let student = state
            .students
            .get(&appeal.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade appeal not found"))?;
        if !can_read_student(&state, &ctx, &caller, &student) {
            return Err(UnifiedError::not_found("Grade appeal not found"));
        }
        Ok(appeal)
    })();
    json::respond(result, debug)
}

async fn resolve_grade_appeal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ResolveGradeAppeal>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GradeAppeal> = (|| {
        let caller = caller(&req)?;
        let body = body.into_inner();
        let decision = require_text(&body.decision, "decision", MAX_CODE)?;
        if decision != "upheld" && decision != "denied" {
            return Err(UnifiedError::bad_request(
                "decision must be \"upheld\" or \"denied\"",
            ));
        }
        let corrected_score = match body.corrected_score {
            None => None,
            Some(value) => Some(parse_score(value, "corrected_score")?),
        };
        let mut appeal = state
            .grade_appeals
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade appeal not found"))?;
        if !matches!(appeal.status.as_str(), "open" | "under_review") {
            return Err(UnifiedError::conflict(
                "Only an open appeal can be resolved",
            ));
        }
        let enrollment = state
            .enrollments
            .get(&appeal.enrollment_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade appeal not found"))?;
        if !can_grade(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the subject's lecturer or a manager may resolve an appeal",
            ));
        }
        if decision == "upheld" {
            if let (Some(component_id), Some(score)) =
                (&appeal.component_id, corrected_score)
            {
                if let Some(mut grade) = state
                    .grades
                    .iter()
                    .find(|entry| {
                        let grade = entry.value();
                        grade.enrollment_id == enrollment.id
                            && grade.component_id == *component_id
                    })
                    .map(|entry| entry.value().clone())
                {
                    grade.score = score;
                    grade.updated_at = now();
                    state.grades.insert(grade.id.clone(), grade);
                }
            }
        }
        appeal.status = if decision == "upheld" { "upheld" } else { "denied" }.to_string();
        appeal.decision = Some(decision.clone());
        appeal.corrected_score = corrected_score;
        appeal.resolution_notes =
            optional_text(&body.resolution_notes, "resolution_notes", MAX_TEXT)?;
        appeal.reviewed_by = state
            .lecturer_by_user(&caller.actor)
            .map(|lecturer| lecturer.id);
        appeal.updated_at = now();
        state
            .grade_appeals
            .insert(appeal.id.clone(), appeal.clone());
        state.emit(
            "academic.grade.appeal_resolved",
            json!({
                "appeal_id": appeal.id,
                "enrollment_id": appeal.enrollment_id,
                "decision": decision,
            }),
        );
        if let Some(student) = state.students.get(&appeal.student_id) {
            state.notify(
                &student.user_id,
                "Grade appeal resolved",
                &format!("Your grade appeal was {decision}."),
            );
        }
        Ok(appeal)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: transcripts
// ---------------------------------------------------------------------------

async fn list_transcripts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Transcript>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let student = load_student_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<Transcript> = state
            .transcripts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|transcript| transcript.student_id == student.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn issue_transcript(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Transcript> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_TRANSCRIPT)?;
        let student = state
            .students
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
        let (gpa, total_credits, _) = state.gpa_for(&student.id);
        let enrollments: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| enrollment.student_id == student.id)
            .collect();
        let mut lines: Vec<Value> = Vec::new();
        for enrollment in &enrollments {
            let grade = state.final_grade(enrollment);
            let subject = state
                .subjects
                .get(&enrollment.subject_id)
                .map(|entry| entry.value().clone());
            let semester = state
                .semesters
                .get(&enrollment.semester_id)
                .map(|entry| entry.value().clone());
            lines.push(json!({
                "subject_code": subject.as_ref().map(|subject| subject.code.clone()),
                "subject_title": subject.as_ref().map(|subject| subject.title.clone()),
                "semester_code": semester.as_ref().map(|semester| semester.code.clone()),
                "credits": enrollment.credits,
                "status": enrollment.status,
                "weighted_score": grade.weighted_score,
                "letter": grade.letter,
                "points": grade.points,
            }));
        }
        let transcript = Transcript {
            id: new_id(),
            student_id: student.id.clone(),
            issued_by: caller.actor.clone(),
            gpa,
            total_credits,
            lines: Value::Array(lines),
            print_job_id: None,
            created_at: now(),
        };
        state
            .transcripts
            .insert(transcript.id.clone(), transcript.clone());
        state.emit(
            "academic.transcript.issued",
            json!({
                "transcript_id": transcript.id,
                "student_id": transcript.student_id,
                "gpa": transcript.gpa,
                "total_credits": transcript.total_credits,
            }),
        );
        Ok(transcript)
    })();
    json::respond(result, debug)
}

async fn get_transcript(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Transcript> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let transcript = state
            .transcripts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Transcript not found"))?;
        let student = state
            .students
            .get(&transcript.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Transcript not found"))?;
        require_read_student(&state, &ctx, &caller, &student)?;
        Ok(transcript)
    })();
    json::respond(result, debug)
}

async fn print_transcript(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Transcript> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_TRANSCRIPT)?;
        let transcript = state
            .transcripts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Transcript not found"))?;
        // This module never prints. It only asks the printing module
        // (see the module header: no printing module exists in this
        // tree yet, so the event is the contract).
        state.emit(
            "academic.transcript.print_requested",
            json!({
                "transcript_id": transcript.id,
                "student_id": transcript.student_id,
            }),
        );
        state.emit(
            "print.job.requested",
            json!({
                "source": "academic",
                "document_type": "transcript",
                "transcript_id": transcript.id,
                "student_id": transcript.student_id,
                "title": "Official Transcript",
                "issued_at": transcript.created_at.to_rfc3339(),
            }),
        );
        Ok(transcript)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: theses
// ---------------------------------------------------------------------------

async fn list_theses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Thesis>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_student = state.student_by_user(&caller.actor);
        let own_lecturer = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Thesis> = state
            .theses
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|thesis| {
                let visible = read_all
                    || own_student
                        .as_ref()
                        .is_some_and(|student| student.id == thesis.student_id)
                    || own_lecturer.as_ref().is_some_and(|lecturer| {
                        lecturer.id == thesis.advisor_id
                            || thesis.co_advisor_id.as_deref() == Some(lecturer.id.as_str())
                    })
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "student_id", &thesis.student_id)
                    && filter_matches(&params, "advisor_id", &thesis.advisor_id)
                    && filter_matches(&params, "status", &thesis.status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_thesis(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateThesis>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Thesis> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let body = body.into_inner();
        let student = state
            .students
            .get(&body.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
        let self_filed = student.user_id == caller.actor;
        let staff = is_staff(&ctx, &caller.role);
        if !self_filed && !staff {
            return Err(UnifiedError::not_found("Student not found"));
        }
        if !state.lecturers.contains_key(&body.advisor_id) {
            return Err(UnifiedError::not_found("Advisor not found"));
        }
        if let Some(co_advisor_id) = &body.co_advisor_id {
            if !state.lecturers.contains_key(co_advisor_id) {
                return Err(UnifiedError::not_found("Co-advisor not found"));
            }
            if co_advisor_id == &body.advisor_id {
                return Err(UnifiedError::bad_request(
                    "Co-advisor must be different from the advisor",
                ));
            }
        }
        if state
            .theses
            .iter()
            .any(|entry| {
                let thesis = entry.value();
                thesis.student_id == student.id
                    && !matches!(thesis.status.as_str(), "completed" | "rejected")
            })
        {
            return Err(UnifiedError::conflict(
                "Student already has an open thesis record",
            ));
        }
        let thesis = Thesis {
            id: new_id(),
            student_id: student.id.clone(),
            title: require_text(&body.title, "title", MAX_TEXT)?,
            advisor_id: body.advisor_id.clone(),
            co_advisor_id: body.co_advisor_id.clone(),
            status: "proposal".to_string(),
            proposal_date: parse_optional_date(&body.proposal_date, "proposal_date")?,
            defense_date: None,
            defense_result: None,
            notes: optional_text(&body.notes, "notes", MAX_TEXT)?,
            created_at: now(),
            updated_at: now(),
        };
        state.theses.insert(thesis.id.clone(), thesis.clone());
        state.emit(
            "academic.thesis.created",
            json!({
                "thesis_id": thesis.id,
                "student_id": thesis.student_id,
                "advisor_id": thesis.advisor_id,
            }),
        );
        Ok(thesis)
    })();
    json::respond(result, debug)
}

async fn get_thesis(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Thesis> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let thesis = state
            .theses
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Thesis not found"))?;
        let student = state
            .students
            .get(&thesis.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Thesis not found"))?;
        let visible = can_read_student(&state, &ctx, &caller, &student)
            || state
                .lecturer_by_user(&caller.actor)
                .is_some_and(|lecturer| {
                    lecturer.id == thesis.advisor_id
                        || thesis.co_advisor_id.as_deref() == Some(lecturer.id.as_str())
                });
        if !visible {
            return Err(UnifiedError::not_found("Thesis not found"));
        }
        Ok(thesis)
    })();
    json::respond(result, debug)
}

fn transition_thesis(state: &AcademicState, thesis: &mut Thesis, to: &str) -> UnifiedResult<()> {
    let allowed = state.allowed_transitions(KIND_THESIS_TRANSITION);
    if !allowed
        .iter()
        .any(|(from, target)| from == &thesis.status && target == to)
    {
        return Err(UnifiedError::conflict(format!(
            "Thesis cannot move from \"{}\" to \"{to}\"",
            thesis.status
        )));
    }
    thesis.status = to.to_string();
    thesis.updated_at = now();
    Ok(())
}

async fn update_thesis(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateThesis>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Thesis> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let body = body.into_inner();
        let mut thesis = state
            .theses
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Thesis not found"))?;
        let student = state
            .students
            .get(&thesis.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Thesis not found"))?;
        let is_advisor = state
            .lecturer_by_user(&caller.actor)
            .is_some_and(|lecturer| lecturer.id == thesis.advisor_id);
        let manager = check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
        let own_student = student.user_id == caller.actor;
        if !manager && !is_advisor && !own_student {
            return Err(UnifiedError::not_found("Thesis not found"));
        }
        if let Some(title) = &body.title {
            thesis.title = require_text(title, "title", MAX_TEXT)?;
        }
        if let Some(advisor_id) = &body.advisor_id {
            if !manager {
                return Err(UnifiedError::forbidden(
                    "Only a manager may change the thesis advisor",
                ));
            }
            if !state.lecturers.contains_key(advisor_id) {
                return Err(UnifiedError::not_found("Advisor not found"));
            }
            thesis.advisor_id = advisor_id.clone();
        }
        if let Some(co_advisor_id) = &body.co_advisor_id {
            match co_advisor_id {
                None => thesis.co_advisor_id = None,
                Some(id) => {
                    if !state.lecturers.contains_key(id) {
                        return Err(UnifiedError::not_found("Co-advisor not found"));
                    }
                    thesis.co_advisor_id = Some(id.clone());
                }
            }
        }
        if let Some(proposal_date) = &body.proposal_date {
            thesis.proposal_date = parse_optional_date(proposal_date, "proposal_date")?;
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if !state.valid_status(KIND_THESIS_STATUS, &status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown thesis status \"{status}\""
                )));
            }
            if status != thesis.status {
                if !manager && !is_advisor {
                    return Err(UnifiedError::forbidden(
                        "Only the advisor or a manager may change the thesis status",
                    ));
                }
                let from = thesis.status.clone();
                transition_thesis(&state, &mut thesis, &status)?;
                state.emit(
                    "academic.thesis.status_changed",
                    json!({
                        "thesis_id": thesis.id,
                        "student_id": thesis.student_id,
                        "from_status": from,
                        "to_status": status,
                    }),
                );
            }
        }
        if let Some(notes) = &body.notes {
            thesis.notes = optional_text(notes, "notes", MAX_TEXT)?;
        }
        thesis.updated_at = now();
        state.theses.insert(thesis.id.clone(), thesis.clone());
        Ok(thesis)
    })();
    json::respond(result, debug)
}

async fn schedule_defense(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ScheduleDefense>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Thesis> = (|| {
        let caller = caller(&req)?;
        let body = body.into_inner();
        let mut thesis = state
            .theses
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Thesis not found"))?;
        let is_advisor = state
            .lecturer_by_user(&caller.actor)
            .is_some_and(|lecturer| lecturer.id == thesis.advisor_id);
        let manager = check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
        if !is_advisor && !manager {
            return Err(UnifiedError::forbidden(
                "Only the advisor or a manager may schedule a defense",
            ));
        }
        let defense_date = parse_date(&body.defense_date, "defense_date")?;
        if defense_date < today() {
            return Err(UnifiedError::bad_request(
                "defense_date must not be in the past",
            ));
        }
        if thesis.status != "defense_scheduled" {
            let from = thesis.status.clone();
            transition_thesis(&state, &mut thesis, "defense_scheduled")?;
            state.emit(
                "academic.thesis.status_changed",
                json!({
                    "thesis_id": thesis.id,
                    "student_id": thesis.student_id,
                    "from_status": from,
                    "to_status": "defense_scheduled",
                }),
            );
        }
        thesis.defense_date = Some(defense_date);
        if let Some(result) = &body.result {
            let result = require_text(result, "result", MAX_TEXT)?;
            thesis.defense_result = Some(result.clone());
            let to = if result.eq_ignore_ascii_case("passed") {
                "defended"
            } else if result.eq_ignore_ascii_case("failed") {
                "in_progress"
            } else {
                return Err(UnifiedError::bad_request(
                    "result must be \"passed\" or \"failed\"",
                ));
            };
            let from = thesis.status.clone();
            transition_thesis(&state, &mut thesis, to)?;
            state.emit(
                "academic.thesis.status_changed",
                json!({
                    "thesis_id": thesis.id,
                    "student_id": thesis.student_id,
                    "from_status": from,
                    "to_status": to,
                }),
            );
        }
        thesis.updated_at = now();
        state.theses.insert(thesis.id.clone(), thesis.clone());
        state.emit(
            "academic.thesis.defense_scheduled",
            json!({
                "thesis_id": thesis.id,
                "student_id": thesis.student_id,
                "defense_date": thesis.defense_date.map(|date| date.format("%Y-%m-%d").to_string()),
            }),
        );
        if let Some(student) = state.students.get(&thesis.student_id) {
            state.notify(
                &student.user_id,
                "Thesis defense scheduled",
                &format!(
                    "Your thesis defense is scheduled for {}.",
                    defense_date.format("%Y-%m-%d")
                ),
            );
        }
        Ok(thesis)
    })();
    json::respond(result, debug)
}

async fn delete_thesis(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if state.theses.remove(&id).is_none() {
            return Err(UnifiedError::not_found("Thesis not found"));
        }
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: academic standing
// ---------------------------------------------------------------------------

async fn list_standing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<StandingRecord>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_student = state.student_by_user(&caller.actor);
        let mut items: Vec<StandingRecord> = state
            .standing_records
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                let visible = read_all
                    || own_student
                        .as_ref()
                        .is_some_and(|student| student.id == record.student_id)
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "student_id", &record.student_id)
                    && filter_matches(&params, "semester_id", &record.semester_id)
                    && filter_matches(&params, "standing", &record.standing)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn recompute_standing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<RecomputeStanding>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<StandingRecord>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        if !state.semesters.contains_key(&body.semester_id) {
            return Err(UnifiedError::not_found("Semester not found"));
        }
        let student_ids: Vec<String> = match &body.student_id {
            Some(student_id) => {
                if !state.students.contains_key(student_id) {
                    return Err(UnifiedError::not_found("Student not found"));
                }
                vec![student_id.clone()]
            }
            None => state
                .students
                .iter()
                .map(|entry| entry.value().id.clone())
                .collect(),
        };
        let mut records: Vec<StandingRecord> = Vec::new();
        for student_id in student_ids {
            let (gpa, total_credits, _) = state.gpa_for(&student_id);
            let standing = state.standing_for_gpa(gpa);
            let previous = state
                .standing_records
                .iter()
                .map(|entry| entry.value().clone())
                .find(|record| {
                    record.student_id == student_id && record.semester_id == body.semester_id
                });
            let mut record = match previous {
                Some(mut existing) => {
                    existing.gpa = gpa;
                    existing.total_credits = total_credits;
                    existing.standing = standing.clone();
                    existing.updated_at = now();
                    existing
                }
                None => StandingRecord {
                    id: new_id(),
                    student_id: student_id.clone(),
                    semester_id: body.semester_id.clone(),
                    gpa,
                    total_credits,
                    standing: standing.clone(),
                    reason: None,
                    created_at: now(),
                    updated_at: now(),
                },
            };
            if standing != "good" {
                record.reason = Some(format!(
                    "GPA {gpa} falls under the \"{standing}\" standing rule for this semester"
                ));
            }
            state
                .standing_records
                .insert(record.id.clone(), record.clone());
            state.emit(
                "academic.standing.changed",
                json!({
                    "student_id": record.student_id,
                    "semester_id": record.semester_id,
                    "standing": record.standing,
                    "gpa": record.gpa,
                }),
            );
            if standing != "good" {
                if let Some(student) = state.students.get(&student_id) {
                    state.notify(
                        &student.user_id,
                        "Academic standing update",
                        &format!(
                            "Your academic standing for this semester is \"{standing}\" (GPA {gpa})."
                        ),
                    );
                }
            }
            records.push(record);
        }
        Ok(records)
    })();
    json::respond(result, debug)
}

async fn list_student_standing(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<StandingRecord>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let student = load_student_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<StandingRecord> = state
            .standing_records
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                record.student_id == student.id
                    && filter_matches(&params, "semester_id", &record.semester_id)
                    && filter_matches(&params, "standing", &record.standing)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: definitions and settings
// ---------------------------------------------------------------------------

async fn list_definitions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AcademicDefinition>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<AcademicDefinition> = state
            .definitions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|definition| {
                matches_search(&format!("{} {}", definition.kind, definition.key), &params)
                    && filter_matches(&params, "kind", &definition.kind)
            })
            .collect();
        items.sort_by(|a, b| {
            a.kind
                .cmp(&b.kind)
                .then_with(|| a.sort_order.cmp(&b.sort_order))
                .then_with(|| a.key.cmp(&b.key))
        });
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateDefinition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AcademicDefinition> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let kind = require_text(&body.kind, "kind", MAX_CODE)?;
        let key = require_text(&body.key, "key", MAX_CODE)?;
        let label = require_text(&body.label, "label", MAX_TEXT)?;
        if state.definitions.iter().any(|entry| {
            let definition = entry.value();
            definition.kind == kind && definition.key == key
        }) {
            return Err(UnifiedError::conflict(format!(
                "A definition for kind \"{kind}\" with key \"{key}\" already exists"
            )));
        }
        let definition = AcademicDefinition {
            id: new_id(),
            kind,
            key,
            label,
            config: body.config.unwrap_or(Value::Object(Default::default())),
            active: true,
            sort_order: body.sort_order.unwrap_or(0),
            created_at: now(),
            updated_at: now(),
        };
        state
            .definitions
            .insert(definition.id.clone(), definition.clone());
        Ok(definition)
    })();
    json::respond(result, debug)
}

async fn get_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AcademicDefinition> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .definitions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Definition not found"))
    })();
    json::respond(result, debug)
}

async fn update_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDefinition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AcademicDefinition> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut definition = state
            .definitions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Definition not found"))?;
        if let Some(label) = &body.label {
            definition.label = require_text(label, "label", MAX_TEXT)?;
        }
        if let Some(config) = &body.config {
            definition.config = config.clone();
        }
        if let Some(active) = body.active {
            definition.active = active;
        }
        if let Some(sort_order) = body.sort_order {
            definition.sort_order = sort_order;
        }
        definition.updated_at = now();
        state
            .definitions
            .insert(definition.id.clone(), definition.clone());
        Ok(definition)
    })();
    json::respond(result, debug)
}

async fn delete_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if state.definitions.remove(&id).is_none() {
            return Err(UnifiedError::not_found("Definition not found"));
        }
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn list_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AcademicSetting>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<AcademicSetting> = state
            .settings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|setting| matches_search(&setting.key, &params))
            .collect();
        items.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn update_setting(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSetting>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AcademicSetting> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let key = require_text(path.as_str(), "key", MAX_CODE)?;
        let body = body.into_inner();
        let setting = AcademicSetting {
            key: key.clone(),
            value: body.value,
            description: optional_text(&body.description, "description", MAX_TEXT)?,
            updated_at: now(),
        };
        state.settings.insert(key, setting.clone());
        Ok(setting)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Accept both TimeTracker event names and both payload shapes (see
/// the module header): `timetracker.entry_logged` with `{user_id,
/// date, minutes, source}` and `timetracker.time.logged` with
/// `{entry_id, user_id, started_at, ended_at, duration_secs, hours,
/// source}`. A matching class session on that date records
/// attendance with source "timetracker".
fn on_timetracker_entry(state: Arc<AcademicState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = data.get("user_id").and_then(|v| v.as_str()) else {
        return;
    };
    let user_id = user_id.to_string();
    let work_date = match data.get("date").and_then(|v| v.as_str()) {
        Some(raw) => match NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d") {
            Ok(date) => date,
            Err(_) => return,
        },
        None => {
            let timestamp = data
                .get("started_at")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("ended_at").and_then(|v| v.as_str()));
            let Some(timestamp) = timestamp else {
                return;
            };
            match NaiveDate::parse_from_str(&timestamp[..timestamp.len().min(10)], "%Y-%m-%d") {
                Ok(date) => date,
                Err(_) => return,
            }
        }
    };
    let minutes: u64 = if let Some(minutes) = data.get("minutes").and_then(|v| v.as_u64()) {
        minutes
    } else if let Some(secs) = data.get("duration_secs").and_then(|v| v.as_u64()) {
        secs.div_ceil(60)
    } else if let Some(hours) = data.get("hours").and_then(|v| v.as_f64()) {
        (hours * 60.0).round() as u64
    } else {
        return;
    };
    if minutes == 0 || minutes > MAX_MINUTES_PER_DAY {
        return;
    }
    // A class match needs the entry's time window, so entries
    // without start and end timestamps cannot be matched to a
    // session and are ignored.
    let (Some(started_at), Some(ended_at)) = (
        data.get("started_at").and_then(|v| v.as_str()),
        data.get("ended_at").and_then(|v| v.as_str()),
    ) else {
        return;
    };
    let (Some(entry_start), Some(entry_end)) = (
        started_at.get(11..16).and_then(time_to_minutes),
        ended_at.get(11..16).and_then(time_to_minutes),
    ) else {
        return;
    };
    let entry_id = data
        .get("entry_id")
        .and_then(|v| v.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(student) = state.student_by_user(&user_id) else {
        return;
    };
    // De-duplicate repeated delivery of the same TimeTracker entry.
    if let Some(entry_id) = &entry_id {
        if state
            .attendance
            .iter()
            .any(|entry| entry.value().entry_id.as_deref() == Some(entry_id.as_str()))
        {
            return;
        }
    }
    let day_of_week = work_date.weekday().number_from_monday() as i32;
    let sessions: Vec<Schedule> = state
        .schedules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|schedule| {
            schedule.status == "active"
                && schedule.day_of_week == day_of_week
                && state
                    .semesters
                    .get(&schedule.semester_id)
                    .is_some_and(|semester| {
                        work_date >= semester.start_date && work_date <= semester.end_date
                    })
                && state.enrollments.iter().any(|entry| {
                    let enrollment = entry.value();
                    enrollment.student_id == student.id
                        && enrollment.subject_id == schedule.subject_id
                        && enrollment.semester_id == schedule.semester_id
                        && matches!(enrollment.status.as_str(), "approved" | "completed")
                })
        })
        .collect();
    for schedule in sessions {
        let class_start = time_to_minutes(&schedule.start_time).unwrap_or(0);
        let class_end = time_to_minutes(&schedule.end_time).unwrap_or(0);
        if entry_start >= class_end || class_start >= entry_end {
            continue;
        }
        if state.attendance.iter().any(|entry| {
            let record = entry.value();
            record.schedule_id == schedule.id
                && record.student_id == student.id
                && record.date == work_date
        }) {
            continue;
        }
        let record = Attendance {
            id: new_id(),
            schedule_id: schedule.id.clone(),
            student_id: student.id.clone(),
            date: work_date,
            status: "present".to_string(),
            source: "timetracker".to_string(),
            entry_id: entry_id.clone(),
            minutes: Some(minutes),
            notes: None,
            created_at: now(),
            updated_at: now(),
        };
        state.attendance.insert(record.id.clone(), record.clone());
        state.emit(
            "academic.attendance.recorded",
            json!({
                "attendance_id": record.id,
                "student_id": record.student_id,
                "schedule_id": record.schedule_id,
                "date": record.date.format("%Y-%m-%d").to_string(),
                "status": record.status,
                "source": record.source,
            }),
        );
    }
}

/// Keep the e-learning link flag fresh: a created or updated course
/// verifies linked subjects; a deleted course un-verifies them.
fn on_elearning_course_event(state: Arc<AcademicState>, ctx: &mut HookContext, verified: bool) {
    let data = ctx.data.clone();
    let Some(course_id) = data.get("course_id").and_then(|v| v.as_str()) else {
        return;
    };
    for entry in state.subjects.iter() {
        if entry.value().elearning_course_id.as_deref() == Some(course_id) {
            let mut subject = entry.value().clone();
            subject.elearning_course_verified = verified;
            subject.updated_at = now();
            state.subjects.insert(subject.id.clone(), subject);
        }
    }
}

/// When a learner completes an e-learning course, mark the linked
/// academic enrollment completed (payload: `{enrollment_id,
/// course_id, user_id}`).
fn on_elearning_enrollment_completed(state: Arc<AcademicState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let (Some(course_id), Some(user_id)) = (
        data.get("course_id").and_then(|v| v.as_str()),
        data.get("user_id").and_then(|v| v.as_str()),
    ) else {
        return;
    };
    let Some(student) = state.student_by_user(user_id) else {
        return;
    };
    let subject_ids: Vec<String> = state
        .subjects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|subject| subject.elearning_course_id.as_deref() == Some(course_id))
        .map(|subject| subject.id)
        .collect();
    for entry in state.enrollments.iter() {
        let mut enrollment = entry.value().clone();
        if enrollment.student_id == student.id
            && subject_ids.contains(&enrollment.subject_id)
            && enrollment.status == "approved"
        {
            if transition_enrollment(&state, &mut enrollment, "completed").is_ok() {
                state
                    .enrollments
                    .insert(enrollment.id.clone(), enrollment.clone());
                state.emit(
                    "academic.enrollment.completed",
                    json!({
                        "enrollment_id": enrollment.id,
                        "student_id": enrollment.student_id,
                        "subject_id": enrollment.subject_id,
                    }),
                );
            }
        }
    }
}

/// Derive the enrollment year for a student created from an
/// admissions application: the caller-supplied `enrollment_year`
/// when it is a plausible year, otherwise the leading four digits
/// of the free-text `academic_year` (for example "2026/2027"),
/// otherwise the current calendar year. Never fails: a hook
/// handler has no error channel, so an unparseable year falls back
/// instead of dropping the handoff.
fn enrollment_year_for(
    supplied: Option<i64>,
    academic_year: Option<&str>,
) -> i32 {
    if let Some(year) = supplied {
        if (1990..=2100).contains(&year) {
            return year as i32;
        }
    }
    if let Some(raw) = academic_year {
        let digits: String = raw.trim().chars().take(4).collect();
        if let Ok(year) = digits.parse::<i32>() {
            if (1990..=2100).contains(&year) {
                return year;
            }
        }
    }
    Utc::now().date_naive().year()
}

/// Handle `academic.student.create_requested` (admissions →
/// academic handoff). Creates the student record from the admitted
/// application and answers with `academic.student.created`
/// carrying the same `application_id`, so admissions can link the
/// student back to the application.
///
/// Idempotent: when a student already exists for the applicant
/// user (redelivery, or an earlier manual creation), the existing
/// record is reused and the answer is still emitted — never a
/// duplicate student. Skipped silently, with no student and no
/// answer, only when the payload names no user, names no program,
/// or names a program this module does not know: a student record
/// must reference a real program, and a hook handler cannot report
/// the failure back to the caller.
fn on_admissions_student_create_requested(state: Arc<AcademicState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let user_id = data
        .get("applicant_user_id")
        .or_else(|| data.get("user_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string());
    let Some(user_id) = user_id else {
        return;
    };
    let program_id = data
        .get("program_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string());
    let Some(program_id) = program_id else {
        return;
    };
    if !state.programs.contains_key(&program_id) {
        return;
    }
    let application_id = data
        .get("application_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    // Reuse the existing student for this user when there is one.
    let student = match state.student_by_user(&user_id) {
        Some(existing) => existing,
        None => {
            let mut student_number = format!("STU-{}", &new_id()[..8].to_uppercase());
            while state
                .students
                .iter()
                .any(|entry| entry.value().student_number == student_number)
            {
                student_number = format!("STU-{}", &new_id()[..8].to_uppercase());
            }
            let enrollment_year = enrollment_year_for(
                data.get("enrollment_year").and_then(|value| value.as_i64()),
                data.get("academic_year").and_then(|value| value.as_str()),
            );
            let created = Student {
                id: new_id(),
                user_id: user_id.clone(),
                student_number,
                program_id: program_id.clone(),
                advisor_id: None,
                enrollment_year,
                status: "active".to_string(),
                created_at: now(),
                updated_at: now(),
            };
            state.students.insert(created.id.clone(), created.clone());
            created
        }
    };
    state.emit(
        "academic.student.created",
        json!({
            "student_id": student.id,
            "user_id": student.user_id,
            "program_id": student.program_id,
            "application_id": application_id,
        }),
    );
}

/// Answer the `academic.student.lookup` hook query. The reference
/// may be a student id, a linked user id, or a student number —
/// curriculum's `student_ref` is opaque, so all three are accepted.
/// Answers with one result `{exists, student_id?, user_id?,
/// program_id?}` (the tree's hook-query convention) and also sets
/// the `exists` key on the payload, so callers that read the
/// payload instead of the results see the same answer. No answer
/// is added when the reference is empty: an empty reference is not
/// a lookup.
fn on_academic_student_lookup(state: Arc<AcademicState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let reference = data
        .get("student_ref")
        .or_else(|| data.get("student_id"))
        .or_else(|| data.get("user_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string());
    let Some(reference) = reference else {
        return;
    };
    let found = state
        .students
        .get(&reference)
        .map(|entry| entry.value().clone())
        .or_else(|| state.student_by_user(&reference))
        .or_else(|| {
            state
                .students
                .iter()
                .find(|entry| entry.value().student_number == reference)
                .map(|entry| entry.value().clone())
        });
    match found {
        Some(student) => {
            ctx.set("exists", json!(true));
            ctx.add_result(json!({
                "exists": true,
                "student_id": student.id,
                "user_id": student.user_id,
                "program_id": student.program_id,
            }));
        }
        None => {
            ctx.set("exists", json!(false));
            ctx.add_result(json!({ "exists": false }));
        }
    }
}

/// Parse an RFC 3339 bound from a `calendar.items_requested`
/// request. Returns `None` when the value is missing or malformed;
/// the caller then contributes nothing rather than guessing a
/// range.
fn parse_calendar_bound(raw: Option<&Value>) -> Option<DateTime<Utc>> {
    let text = raw.and_then(|value| value.as_str())?;
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|fixed| fixed.with_timezone(&Utc))
}

/// Answer `calendar.items_requested` with this module's dated
/// items in the shared calendar shape (`{id, module: "academic",
/// title, start, end?, all_day, label?, deep_link?, visibility:
/// "shared", owner_user_id?}`). Contributed, in the requested
/// `[start, end]` window:
///   - academic calendar events (holidays, exam periods,
///     registration windows) as all-day items spanning
///     `start_date` through `end_date` inclusive;
///   - one item per occurrence of each active weekly class
///     schedule inside its semester's dates;
///   - thesis defenses with a scheduled date.
/// Semester start/end dates themselves are not contributed: they
/// are boundaries, and an administrator records them as calendar
/// events when they should appear. The window expansion is capped
/// at 366 days and 1,000 items so a wide-open range cannot explode
/// into unbounded occurrences. Academic items carry no tenant or
/// company scope (this store has no tenant partition yet), so no
/// `tenant_id`/`company_id` is set and the calendar's foreign-scope
/// exclusion never drops them. When the request's `modules` filter
/// does not include `academic`, nothing is contributed.
fn on_calendar_items_requested(state: Arc<AcademicState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    if let Some(modules) = data.get("modules").and_then(|value| value.as_array()) {
        let wanted = modules
            .iter()
            .any(|value| value.as_str() == Some("academic"));
        if !wanted {
            return;
        }
    }
    let (Some(start), Some(end)) = (
        parse_calendar_bound(data.get("start")),
        parse_calendar_bound(data.get("end")),
    ) else {
        return;
    };
    if end < start {
        return;
    }
    let mut items: Vec<Value> = Vec::new();
    // Academic calendar events: all-day, start_date 00:00 UTC
    // through the day after end_date 00:00 UTC (end exclusive).
    for event in state.calendar_events.iter().map(|entry| entry.value().clone()) {
        if items.len() >= 1000 {
            break;
        }
        let Some(item_start) = event.start_date.and_hms_opt(0, 0, 0).map(|time| time.and_utc())
        else {
            continue;
        };
        let Some(item_end) = (event.end_date + Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .map(|time| time.and_utc())
        else {
            continue;
        };
        if item_start > end || item_end < start {
            continue;
        }
        items.push(json!({
            "id": format!("calendar-event:{}", event.id),
            "module": "academic",
            "title": event.title,
            "start": item_start.to_rfc3339(),
            "end": item_end.to_rfc3339(),
            "all_day": true,
            "label": event.event_type,
            "deep_link": format!("/api/v1/academic/calendar/{}", event.id),
            "visibility": "shared",
        }));
    }
    // Class schedules: one item per weekly occurrence inside the
    // semester's dates and the requested window.
    let window_days = (end.date_naive() - start.date_naive()).num_days().min(366);
    for schedule in state
        .schedules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|schedule| schedule.status == "active")
    {
        if items.len() >= 1000 {
            break;
        }
        let Some(semester) = state
            .semesters
            .get(&schedule.semester_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        let (Some(class_start_minutes), Some(class_end_minutes)) = (
            time_to_minutes(&schedule.start_time),
            time_to_minutes(&schedule.end_time),
        ) else {
            continue;
        };
        let first = start.date_naive().max(semester.start_date);
        let last = end
            .date_naive()
            .min(semester.end_date)
            .min(first + Duration::days(window_days));
        let mut date = first;
        while date <= last {
            if items.len() >= 1000 {
                break;
            }
            if date.weekday().number_from_monday() as i32 == schedule.day_of_week {
                let start_time = (
                    class_start_minutes / 60,
                    class_start_minutes % 60,
                );
                let end_time = (class_end_minutes / 60, class_end_minutes % 60);
                if let (Some(item_start), Some(item_end)) = (
                    date.and_hms_opt(start_time.0 as u32, start_time.1 as u32, 0)
                        .map(|time| time.and_utc()),
                    date.and_hms_opt(end_time.0 as u32, end_time.1 as u32, 0)
                        .map(|time| time.and_utc()),
                ) {
                    if item_start >= start && item_start <= end {
                        let title = state
                            .subjects
                            .get(&schedule.subject_id)
                            .map(|entry| entry.value().title.clone())
                            .unwrap_or_else(|| "Class".to_string());
                        let owner_user_id = state
                            .lecturers
                            .get(&schedule.lecturer_id)
                            .map(|entry| entry.value().user_id.clone());
                        items.push(json!({
                            "id": format!("schedule:{}:{}", schedule.id, date.format("%Y-%m-%d")),
                            "module": "academic",
                            "title": title,
                            "start": item_start.to_rfc3339(),
                            "end": item_end.to_rfc3339(),
                            "all_day": false,
                            "label": "Class",
                            "deep_link": format!("/api/v1/academic/schedules/{}", schedule.id),
                            "visibility": "shared",
                            "owner_user_id": owner_user_id,
                        }));
                    }
                }
            }
            date += Duration::days(1);
        }
    }
    // Thesis defenses with a scheduled date: all-day items.
    for thesis in state.theses.iter().map(|entry| entry.value().clone()) {
        if items.len() >= 1000 {
            break;
        }
        let Some(defense_date) = thesis.defense_date else {
            continue;
        };
        let Some(item_start) = defense_date
            .and_hms_opt(0, 0, 0)
            .map(|time| time.and_utc())
        else {
            continue;
        };
        let Some(item_end) = (defense_date + Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .map(|time| time.and_utc())
        else {
            continue;
        };
        if item_start < start || item_start > end {
            continue;
        }
        let owner_user_id = state
            .students
            .get(&thesis.student_id)
            .map(|entry| entry.value().user_id.clone());
        items.push(json!({
            "id": format!("thesis-defense:{}", thesis.id),
            "module": "academic",
            "title": format!("Thesis defense: {}", thesis.title),
            "start": item_start.to_rfc3339(),
            "end": item_end.to_rfc3339(),
            "all_day": true,
            "label": "Thesis defense",
            "deep_link": format!("/api/v1/academic/theses/{}", thesis.id),
            "visibility": "shared",
            "owner_user_id": owner_user_id,
        }));
    }
    ctx.add_result(json!({ "items": items }));
}

// ---------------------------------------------------------------------------
// AI capabilities (AI Capabilities Contract, see `crate::modules::ai`).
//
// Declared here, registered once in `init`, grounded through the
// `academic.ai.summarize_progress_requested` hook, and run through
// the AI module's gated runner. The summary is returned inline and
// is never published, sent, or stored by this module.
// ---------------------------------------------------------------------------

/// AI capabilities this module exposes through the AI module's
/// contract. The capability is non-PII: the grounding summary is
/// anonymised (aggregates only — no user id, no student number, no
/// names), and the input is redacted before it reaches the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "summarize_progress",
        module_id: "academic",
        name: "Summarize student progress",
        description: "Summarize one student's academic progress from anonymised grade and attendance aggregates (GPA, credits, standing, per-subject average scores, attendance rate). Never receives raw personal data or student identities.",
        input_fields: &["student_id"],
        allows_pii: false,
        use_case: "academic.summarize_progress",
    },
];

/// Anonymised progress grounding for one student: GPA, credits,
/// standing, enrollment and attendance aggregates, and per-subject
/// average component scores. Carries no user id, no student
/// number, and no names — only opaque subject codes and titles.
fn academic_progress_grounding(state: &AcademicState, student_id: &str) -> Value {
    let (gpa, total_credits, earned_credits) = state.gpa_for(student_id);
    let enrollments: Vec<Enrollment> = state
        .enrollments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|enrollment| enrollment.student_id == student_id)
        .collect();
    let completed = enrollments
        .iter()
        .filter(|enrollment| enrollment.status == "completed")
        .count();
    let attendance: Vec<Attendance> = state
        .attendance
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| record.student_id == student_id)
        .collect();
    let present = attendance
        .iter()
        .filter(|record| record.status == "present" || record.status == "late")
        .count();
    let attendance_rate = if attendance.is_empty() {
        None
    } else {
        Some((present as f64 / attendance.len() as f64 * 100.0).round() / 100.0)
    };
    let mut per_subject: std::collections::HashMap<String, (f64, usize)> =
        std::collections::HashMap::new();
    for grade in state.grades.iter().map(|entry| entry.value().clone()) {
        if !enrollments
            .iter()
            .any(|enrollment| enrollment.id == grade.enrollment_id)
        {
            continue;
        }
        let Some(component) = state
            .grade_components
            .get(&grade.component_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        let entry = per_subject
            .entry(component.subject_id)
            .or_insert((0.0, 0));
        entry.0 += grade.score;
        entry.1 += 1;
    }
    let mut subjects: Vec<Value> = per_subject
        .into_iter()
        .filter_map(|(subject_id, (sum, count))| {
            let subject = state
                .subjects
                .get(&subject_id)
                .map(|entry| entry.value().clone())?;
            Some(json!({
                "subject_code": subject.code,
                "subject_title": subject.title,
                "average_score": (sum / count as f64 * 100.0).round() / 100.0,
                "graded_components": count,
            }))
        })
        .collect();
    subjects.sort_by(|a, b| {
        a.get("subject_code")
            .and_then(|value| value.as_str())
            .cmp(
                &b.get("subject_code")
                    .and_then(|value| value.as_str()),
            )
    });
    json!({
        "gpa": (gpa * 100.0).round() / 100.0,
        "total_credits": total_credits,
        "earned_credits": earned_credits,
        "standing": state.standing_for_gpa(gpa),
        "enrollments": enrollments.len(),
        "completed_enrollments": completed,
        "attendance_present": present,
        "attendance_total": attendance.len(),
        "attendance_rate": attendance_rate,
        "subjects": subjects,
    })
}

/// Run one registered academic AI capability:
/// `POST /api/v1/academic/ai/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use` (plus the module's own
/// `academic:read`, and — for `summarize_progress` — the module's
/// read scoping for the named student, so an AI caller cannot
/// summarize a student they may not read). Delegates to the AI
/// module's gated runner.
async fn ai_capability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ) {
        return json::error_response(&error, debug);
    }
    if capability == "summarize_progress" {
        let student_id = match input
            .get("student_id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(id) => id.to_string(),
            None => {
                return json::error_response(
                    &UnifiedError::unprocessable("input.student_id is required"),
                    debug,
                )
            }
        };
        if let Err(error) = load_student_for_read(&state, &ctx, &caller, &student_id) {
            return json::error_response(&error, debug);
        }
    }
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return json::error_response(&error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    match ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "academic",
        &capability,
        input,
    )
    .await
    {
        Ok(data) => json::success_response(data, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AcademicModule {
    state: Arc<AcademicState>,
}

impl AcademicModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AcademicState::new()),
        }
    }
}

impl Default for AcademicModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AcademicModule {
    fn id(&self) -> &'static str {
        "academic"
    }

    fn name(&self) -> &'static str {
        "Academic System"
    }

    fn description(&self) -> &'static str {
        "Academic system: students, lecturers, study programs, subjects linked to e-learning courses, class schedules and rooms, course registration with advisor approval, attendance, grades and GPA, grade appeals, official transcripts, academic calendar, thesis tracking, and academic standing."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "elearning", "timetracker", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "elearning", "timetracker", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        ai::register_ai_capabilities("academic", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Both TimeTracker event names (see the module header): the
        // documented `timetracker.entry_logged` and the
        // `timetracker.time.logged` the TimeTracker module in this
        // tree actually emits. One handler accepts both payloads.
        for event in ["timetracker.entry_logged", "timetracker.time.logged"] {
            let state = self.state.clone();
            let handler: HookHandler =
                Arc::new(move |ctx: &mut HookContext| on_timetracker_entry(state.clone(), ctx));
            bus.register(event, "academic", handler);
        }
        // E-learning course lifecycle: keep subject link flags fresh.
        for (event, verified) in [
            ("elearning.course.created", true),
            ("elearning.course.updated", true),
            ("elearning.course.deleted", false),
        ] {
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                on_elearning_course_event(state.clone(), ctx, verified)
            });
            bus.register(event, "academic", handler);
        }
        let state = self.state.clone();
        bus.register(
            "elearning.enrollment.completed",
            "academic",
            Arc::new(move |ctx: &mut HookContext| {
                on_elearning_enrollment_completed(state.clone(), ctx)
            }),
        );
        // Admissions handoff: an accepted application is enrolled
        // by creating the student record here and answering with
        // `academic.student.created` carrying the application id.
        let state = self.state.clone();
        bus.register(
            "academic.student.create_requested",
            "academic",
            Arc::new(move |ctx: &mut HookContext| {
                on_admissions_student_create_requested(state.clone(), ctx)
            }),
        );
        // Student lookup hook query (curriculum asks for it): one
        // result `{exists, student_id?, user_id?, program_id?}`.
        let state = self.state.clone();
        bus.register(
            "academic.student.lookup",
            "academic",
            Arc::new(move |ctx: &mut HookContext| {
                on_academic_student_lookup(state.clone(), ctx)
            }),
        );
        // Global calendar aggregation: contribute the academic
        // calendar events, class-schedule occurrences, and thesis
        // defenses in the requested window.
        let state = self.state.clone();
        bus.register(
            "calendar.items_requested",
            "academic",
            Arc::new(move |ctx: &mut HookContext| {
                on_calendar_items_requested(state.clone(), ctx)
            }),
        );
        // Ground the `summarize_progress` AI capability with the
        // anonymised progress aggregates (see
        // `academic_progress_grounding`). Tenant scoping: this store
        // is in-memory with no tenant partition yet; when batch 17
        // persistence lands, this handler must only ground students
        // of the calling tenant, keyed by the caller company id in
        // the payload — the endpoint already enforces the module's
        // own read scoping for the named student.
        let state = self.state.clone();
        bus.register(
            "academic.ai.summarize_progress_requested",
            "academic",
            Arc::new(move |ctx: &mut HookContext| {
                let student_id = ctx
                    .get("input")
                    .and_then(|input| input.get("student_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if student_id.trim().is_empty() {
                    ctx.add_result(json!({}));
                    return;
                }
                ctx.add_result(academic_progress_grounding(&state, &student_id));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/academic")
                .route("/programs", web::get().to(list_programs))
                .route("/programs", web::post().to(create_program))
                .route("/programs/{id}", web::get().to(get_program))
                .route("/programs/{id}", web::put().to(update_program))
                .route("/programs/{id}", web::delete().to(delete_program))
                .route("/students", web::get().to(list_students))
                .route("/students", web::post().to(create_student))
                .route("/students/{id}", web::get().to(get_student))
                .route("/students/{id}", web::put().to(update_student))
                .route("/students/{id}", web::delete().to(delete_student))
                .route("/students/{id}/summary", web::get().to(get_student_summary))
                .route("/students/{id}/gpa", web::get().to(get_student_gpa))
                .route("/students/{id}/enrollments", web::get().to(list_student_enrollments))
                .route("/students/{id}/transcripts", web::get().to(list_transcripts))
                .route("/students/{id}/transcripts", web::post().to(issue_transcript))
                .route("/students/{id}/standing", web::get().to(list_student_standing))
                .route("/lecturers", web::get().to(list_lecturers))
                .route("/lecturers", web::post().to(create_lecturer))
                .route("/lecturers/{id}", web::get().to(get_lecturer))
                .route("/lecturers/{id}", web::put().to(update_lecturer))
                .route("/lecturers/{id}", web::delete().to(delete_lecturer))
                .route("/lecturers/{id}/classes", web::get().to(list_lecturer_classes))
                .route("/subjects", web::get().to(list_subjects))
                .route("/subjects", web::post().to(create_subject))
                .route("/subjects/{id}", web::get().to(get_subject))
                .route("/subjects/{id}", web::put().to(update_subject))
                .route("/subjects/{id}", web::delete().to(delete_subject))
                .route("/subjects/{id}/link-elearning", web::post().to(link_elearning_course))
                .route("/subjects/{id}/components", web::get().to(list_grade_components))
                .route("/subjects/{id}/components", web::post().to(create_grade_component))
                .route("/grade-components/{id}", web::put().to(update_grade_component))
                .route("/grade-components/{id}", web::delete().to(delete_grade_component))
                .route("/rooms", web::get().to(list_rooms))
                .route("/rooms", web::post().to(create_room))
                .route("/rooms/{id}", web::get().to(get_room))
                .route("/rooms/{id}", web::put().to(update_room))
                .route("/rooms/{id}", web::delete().to(delete_room))
                .route("/semesters", web::get().to(list_semesters))
                .route("/semesters", web::post().to(create_semester))
                .route("/semesters/{id}", web::get().to(get_semester))
                .route("/semesters/{id}", web::put().to(update_semester))
                .route("/semesters/{id}", web::delete().to(delete_semester))
                .route("/semesters/{id}/activate", web::post().to(activate_semester))
                .route("/calendar", web::get().to(list_calendar_events))
                .route("/calendar", web::post().to(create_calendar_event))
                .route("/calendar/{id}", web::get().to(get_calendar_event))
                .route("/calendar/{id}", web::put().to(update_calendar_event))
                .route("/calendar/{id}", web::delete().to(delete_calendar_event))
                .route("/schedules", web::get().to(list_schedules))
                .route("/schedules", web::post().to(create_schedule))
                .route("/schedules/{id}", web::get().to(get_schedule))
                .route("/schedules/{id}", web::put().to(update_schedule))
                .route("/schedules/{id}", web::delete().to(delete_schedule))
                .route("/schedules/{id}/attendance", web::get().to(list_schedule_attendance))
                .route("/enrollments", web::get().to(list_enrollments))
                .route("/enrollments", web::post().to(create_enrollment))
                .route("/enrollments/{id}", web::get().to(get_enrollment))
                .route("/enrollments/{id}/approve", web::post().to(approve_enrollment))
                .route("/enrollments/{id}/reject", web::post().to(reject_enrollment))
                .route("/enrollments/{id}/drop", web::post().to(drop_enrollment))
                .route("/attendance", web::get().to(list_attendance))
                .route("/attendance", web::post().to(create_attendance))
                .route("/attendance/{id}", web::get().to(get_attendance))
                .route("/attendance/{id}", web::put().to(update_attendance))
                .route("/enrollments/{id}/grades", web::get().to(list_grades_for_enrollment))
                .route("/enrollments/{id}/grades", web::post().to(create_grade))
                .route("/enrollments/{id}/final-grade", web::get().to(get_final_grade))
                .route("/grades/{id}", web::put().to(update_grade))
                .route("/grades/{id}", web::delete().to(delete_grade))
                .route("/grade-appeals", web::get().to(list_grade_appeals))
                .route("/grade-appeals", web::post().to(create_grade_appeal))
                .route("/grade-appeals/{id}", web::get().to(get_grade_appeal))
                .route("/grade-appeals/{id}/resolve", web::post().to(resolve_grade_appeal))
                .route("/transcripts/{id}", web::get().to(get_transcript))
                .route("/transcripts/{id}/print", web::post().to(print_transcript))
                .route("/theses", web::get().to(list_theses))
                .route("/theses", web::post().to(create_thesis))
                .route("/theses/{id}", web::get().to(get_thesis))
                .route("/theses/{id}", web::put().to(update_thesis))
                .route("/theses/{id}", web::delete().to(delete_thesis))
                .route("/theses/{id}/defense", web::post().to(schedule_defense))
                .route("/standing", web::get().to(list_standing))
                .route("/standing/recompute", web::post().to(recompute_standing))
                .route("/definitions", web::get().to(list_definitions))
                .route("/definitions", web::post().to(create_definition))
                .route("/definitions/{id}", web::get().to(get_definition))
                .route("/definitions/{id}", web::put().to(update_definition))
                .route("/definitions/{id}", web::delete().to(delete_definition))
                .route("/settings", web::get().to(list_settings))
                .route("/settings/{key}", web::put().to(update_setting))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/academic/programs", owner: "academic", description: "List study programs." },
            RouteInfo { method: "POST", path: "/api/v1/academic/programs", owner: "academic", description: "Create a study program (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/programs/{id}", owner: "academic", description: "Get one study program." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/programs/{id}", owner: "academic", description: "Update a study program (academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/programs/{id}", owner: "academic", description: "Delete a study program (academic:manage; refused with students or subjects)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/students", owner: "academic", description: "List students (staff see all; a student sees only their own record; advisors see their advisees)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/students", owner: "academic", description: "Create a student record linked to a user account (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/students/{id}", owner: "academic", description: "Get one student (self, advisor, or academic:read_all)." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/students/{id}", owner: "academic", description: "Update a student (academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/students/{id}", owner: "academic", description: "Delete a student (academic:manage; refused with enrollments or a thesis)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/students/{id}/summary", owner: "academic", description: "A student's academic summary: GPA, credits, standing, enrollment and attendance counts (self, advisor, or academic:read_all)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/students/{id}/gpa", owner: "academic", description: "A student's GPA, credits, and standing (self, advisor, or academic:read_all)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/students/{id}/enrollments", owner: "academic", description: "List one student's enrollments (self, advisor, or academic:read_all)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/students/{id}/transcripts", owner: "academic", description: "List one student's issued transcripts (self, advisor, or academic:read_all)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/students/{id}/transcripts", owner: "academic", description: "Issue an official transcript (academic:transcript)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/students/{id}/standing", owner: "academic", description: "List one student's standing records (self, advisor, or academic:read_all)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/lecturers", owner: "academic", description: "List lecturers (a lecturer sees only their own record)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/lecturers", owner: "academic", description: "Create a lecturer record linked to a user account (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/lecturers/{id}", owner: "academic", description: "Get one lecturer (self, manager, or academic:read_all)." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/lecturers/{id}", owner: "academic", description: "Update a lecturer (academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/lecturers/{id}", owner: "academic", description: "Delete a lecturer (academic:manage; refused with active schedules or advisees)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/lecturers/{id}/classes", owner: "academic", description: "List the class schedules one lecturer teaches (self, manager, or academic:read_all)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/subjects", owner: "academic", description: "List academic subjects." },
            RouteInfo { method: "POST", path: "/api/v1/academic/subjects", owner: "academic", description: "Create an academic subject; an e-learning course id may be linked and is verified through the e-learning hook (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/subjects/{id}", owner: "academic", description: "Get one subject." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/subjects/{id}", owner: "academic", description: "Update a subject (academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/subjects/{id}", owner: "academic", description: "Delete a subject (academic:manage; refused with enrollments or active schedules)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/subjects/{id}/link-elearning", owner: "academic", description: "Link (or re-verify) an e-learning course to a subject through the e-learning hook (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/subjects/{id}/components", owner: "academic", description: "List a subject's grade components with their weights." },
            RouteInfo { method: "POST", path: "/api/v1/academic/subjects/{id}/components", owner: "academic", description: "Add a grade component with a configurable weight (academic:manage)." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/grade-components/{id}", owner: "academic", description: "Update a grade component (academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/grade-components/{id}", owner: "academic", description: "Delete a grade component (academic:manage; refused while grades reference it)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/rooms", owner: "academic", description: "List rooms." },
            RouteInfo { method: "POST", path: "/api/v1/academic/rooms", owner: "academic", description: "Create a room (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/rooms/{id}", owner: "academic", description: "Get one room." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/rooms/{id}", owner: "academic", description: "Update a room (academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/rooms/{id}", owner: "academic", description: "Delete a room (academic:manage; refused with active schedules)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/semesters", owner: "academic", description: "List semesters." },
            RouteInfo { method: "POST", path: "/api/v1/academic/semesters", owner: "academic", description: "Create a semester (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/semesters/{id}", owner: "academic", description: "Get one semester." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/semesters/{id}", owner: "academic", description: "Update a semester (academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/semesters/{id}", owner: "academic", description: "Delete a semester (academic:manage; refused with enrollments or active schedules)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/semesters/{id}/activate", owner: "academic", description: "Mark a semester current (academic:manage; the previous current semester returns to active)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/calendar", owner: "academic", description: "List academic calendar events (holidays, exam periods, registration windows)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/calendar", owner: "academic", description: "Create a calendar event (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/calendar/{id}", owner: "academic", description: "Get one calendar event." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/calendar/{id}", owner: "academic", description: "Update a calendar event (academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/calendar/{id}", owner: "academic", description: "Delete a calendar event (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/schedules", owner: "academic", description: "List class schedules (a lecturer sees only their own classes)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/schedules", owner: "academic", description: "Create a class schedule; room and lecturer conflicts are rejected (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/schedules/{id}", owner: "academic", description: "Get one schedule (its lecturer, manager, or academic:read_all)." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/schedules/{id}", owner: "academic", description: "Update a schedule; conflicts are re-checked (academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/schedules/{id}", owner: "academic", description: "Delete a schedule (academic:manage; refused with attendance records)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/schedules/{id}/attendance", owner: "academic", description: "List attendance for one class session (its lecturer, manager, or academic:read_all)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/enrollments", owner: "academic", description: "List enrollments (a student sees only their own; a lecturer sees their subjects' enrollments)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/enrollments", owner: "academic", description: "Register for a course (self or staff; program credit cap enforced)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/enrollments/{id}", owner: "academic", description: "Get one enrollment (self, advisor, subject lecturer, or academic:read_all)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/enrollments/{id}/approve", owner: "academic", description: "Approve a registration (the student's advisor with academic:advise, or academic:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/enrollments/{id}/reject", owner: "academic", description: "Reject a registration with a reason (the student's advisor with academic:advise, or academic:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/enrollments/{id}/drop", owner: "academic", description: "Drop a registration (self or staff; refused once completed)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/attendance", owner: "academic", description: "List attendance records (a student sees only their own)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/attendance", owner: "academic", description: "Record attendance manually (academic:manage; the student must be enrolled in the class)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/attendance/{id}", owner: "academic", description: "Get one attendance record (self, advisor, or academic:read_all)." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/attendance/{id}", owner: "academic", description: "Update an attendance record (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/enrollments/{id}/grades", owner: "academic", description: "List the grades of one enrollment (self, advisor, subject lecturer, or academic:read_all)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/enrollments/{id}/grades", owner: "academic", description: "Record a grade for one component (the subject's lecturer with academic:grade, or academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/enrollments/{id}/final-grade", owner: "academic", description: "The computed final grade of one enrollment: weight-normalized score, letter, and points (self, advisor, subject lecturer, or academic:read_all)." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/grades/{id}", owner: "academic", description: "Update a grade (the subject's lecturer with academic:grade, or academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/grades/{id}", owner: "academic", description: "Delete a grade (the subject's lecturer with academic:grade, or academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/grade-appeals", owner: "academic", description: "List grade appeals (a student sees only their own)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/grade-appeals", owner: "academic", description: "File a grade appeal (self or staff)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/grade-appeals/{id}", owner: "academic", description: "Get one grade appeal (self, advisor, or academic:read_all)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/grade-appeals/{id}/resolve", owner: "academic", description: "Resolve a grade appeal; an upheld appeal with a corrected score rewrites the grade (the subject's lecturer with academic:grade, or academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/transcripts/{id}", owner: "academic", description: "Get one issued transcript (self, advisor, or academic:read_all)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/transcripts/{id}/print", owner: "academic", description: "Request printing of a transcript: emits print.job.requested for the printing module (academic:transcript)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/theses", owner: "academic", description: "List theses (a student sees only their own; an advisor sees their students')." },
            RouteInfo { method: "POST", path: "/api/v1/academic/theses", owner: "academic", description: "Create a thesis record (self or staff)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/theses/{id}", owner: "academic", description: "Get one thesis (self, advisor, co-advisor, or academic:read_all)." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/theses/{id}", owner: "academic", description: "Update a thesis; status changes follow the thesis transition definitions (self, advisor, or academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/theses/{id}", owner: "academic", description: "Delete a thesis (academic:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/theses/{id}/defense", owner: "academic", description: "Schedule a defense or record its result (advisor or academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/standing", owner: "academic", description: "List academic standing records (a student sees only their own)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/standing/recompute", owner: "academic", description: "Recompute standing from GPA for one student or all students in a semester (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/definitions", owner: "academic", description: "List admin-editable academic definitions (grade scales, standing rules, status vocabularies, transitions)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/definitions", owner: "academic", description: "Create an academic definition (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/definitions/{id}", owner: "academic", description: "Get one definition." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/definitions/{id}", owner: "academic", description: "Update a definition (academic:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/academic/definitions/{id}", owner: "academic", description: "Delete a definition (academic:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/academic/settings", owner: "academic", description: "List academic settings (configurable defaults)." },
            RouteInfo { method: "PUT", path: "/api/v1/academic/settings/{key}", owner: "academic", description: "Set one academic setting (academic:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/academic/ai/{capability}", owner: "academic", description: "Run one registered academic AI capability (summarize_progress; requires ai:use plus academic:read and the module's read scoping for the student)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AcademicModule::new()));
}
