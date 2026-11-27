//! Academic - capability names and configurable defaults.
//!
//! Part of the `academic` module split; see `src/modules/academic.rs`.

// Capabilities (checked through the `role.check` hook; fail closed).

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
pub(crate) const MAX_TEXT: usize = 2000;

/// Longest short code accepted (program codes, subject codes, room
/// codes, student numbers).
pub(crate) const MAX_CODE: usize = 64;

/// A day cannot exceed 24 hours (attendance minutes from TimeTracker).
pub(crate) const MAX_MINUTES_PER_DAY: u64 = 1440;

/// Highest score accepted on any grade component.
pub(crate) const MAX_SCORE: f64 = 100.0;

// CONFIGURABLE DEFAULTS (see the module header)
// These values apply only when no admin definition or setting exists
// yet. They are defaults, not hard-coded business rules: every one of
// them can be replaced at runtime through the definitions or settings
// endpoints.

/// Default maximum credits a student may register per semester,
/// used when a program has no `max_credits_per_semester` and no
/// `default_max_credits_per_semester` setting exists.
pub(crate) const DEFAULT_MAX_CREDITS_PER_SEMESTER: f64 = 24.0;

/// Default passing grade points, used when no grade-scale definition
/// exists (a final letter with fewer points than this does not earn
/// credit towards graduation totals).
pub(crate) const DEFAULT_PASSING_POINTS: f64 = 2.0;

/// Default grade scale, used when no `grade_scale` definitions exist.
/// Each entry is (letter, minimum score, points).
pub(crate) const DEFAULT_GRADE_SCALE: [(&str, f64, f64); 5] = [
    ("A", 90.0, 4.0),
    ("B", 80.0, 3.0),
    ("C", 70.0, 2.0),
    ("D", 60.0, 1.0),
    ("F", 0.0, 0.0),
];

/// Default standing rules, used when no `standing_rule` definitions
/// exist. Each entry is (standing key, minimum GPA). Bands are
/// half-open: a GPA belongs to the first band (in descending
/// minimum-GPA order) whose minimum it reaches, so every GPA
/// falls in exactly one band with no gaps between bands.
pub(crate) const DEFAULT_STANDING_RULES: [(&str, f64); 4] = [
    ("good", 2.5),
    ("warning", 2.0),
    ("probation", 1.0),
    ("dismissed", 0.0),
];

/// Default student statuses, used when no `student_status`
/// definitions exist.
pub(crate) const DEFAULT_STUDENT_STATUSES: [&str; 4] =
    ["active", "on_leave", "graduated", "withdrawn"];

/// Default lecturer statuses, used when no `lecturer_status`
/// definitions exist.
pub(crate) const DEFAULT_LECTURER_STATUSES: [&str; 3] = ["active", "on_leave", "retired"];

/// Default registration statuses, used when no `enrollment_status`
/// definitions exist.
pub(crate) const DEFAULT_ENROLLMENT_STATUSES: [&str; 5] =
    ["requested", "approved", "rejected", "dropped", "completed"];

/// Default attendance statuses, used when no `attendance_status`
/// definitions exist.
pub(crate) const DEFAULT_ATTENDANCE_STATUSES: [&str; 4] = ["present", "absent", "late", "excused"];

/// Default grade-component kinds, used when no
/// `grade_component_kind` definitions exist.
pub(crate) const DEFAULT_COMPONENT_KINDS: [&str; 6] = [
    "midterm",
    "final",
    "assignment",
    "quiz",
    "project",
    "participation",
];

/// Default thesis statuses, used when no `thesis_status`
/// definitions exist.
pub(crate) const DEFAULT_THESIS_STATUSES: [&str; 6] = [
    "proposal",
    "in_progress",
    "defense_scheduled",
    "defended",
    "completed",
    "rejected",
];

/// Default appeal statuses, used when no `appeal_status`
/// definitions exist.
pub(crate) const DEFAULT_APPEAL_STATUSES: [&str; 4] = ["open", "under_review", "upheld", "denied"];

/// Default calendar event types, used when no `calendar_event_type`
/// definitions exist.
pub(crate) const DEFAULT_CALENDAR_EVENT_TYPES: [&str; 6] = [
    "holiday",
    "exam_period",
    "semester_start",
    "semester_end",
    "registration",
    "other",
];

/// Default thesis transitions, used when no `thesis_transition`
/// definitions exist. Each entry is (from status, to status).
pub(crate) const DEFAULT_THESIS_TRANSITIONS: [(&str, &str); 7] = [
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
pub(crate) const DEFAULT_ENROLLMENT_TRANSITIONS: [(&str, &str); 6] = [
    ("requested", "approved"),
    ("requested", "rejected"),
    ("requested", "dropped"),
    ("approved", "dropped"),
    ("approved", "completed"),
    ("rejected", "requested"),
];

// Definition kinds (the admin-editable vocabulary registry).
pub(crate) const KIND_STUDENT_STATUS: &str = "student_status";

pub(crate) const KIND_LECTURER_STATUS: &str = "lecturer_status";

pub(crate) const KIND_ENROLLMENT_STATUS: &str = "enrollment_status";

pub(crate) const KIND_ATTENDANCE_STATUS: &str = "attendance_status";

pub(crate) const KIND_COMPONENT_KIND: &str = "grade_component_kind";

pub(crate) const KIND_GRADE_SCALE: &str = "grade_scale";

pub(crate) const KIND_STANDING_RULE: &str = "standing_rule";

pub(crate) const KIND_THESIS_STATUS: &str = "thesis_status";

pub(crate) const KIND_THESIS_TRANSITION: &str = "thesis_transition";

pub(crate) const KIND_ENROLLMENT_TRANSITION: &str = "enrollment_transition";

pub(crate) const KIND_APPEAL_STATUS: &str = "appeal_status";

pub(crate) const KIND_CALENDAR_EVENT_TYPE: &str = "calendar_event_type";

// Settings keys (admin-editable scalar defaults).
pub(crate) const SETTING_DEFAULT_MAX_CREDITS: &str = "default_max_credits_per_semester";

pub(crate) const SETTING_DEFAULT_PASSING_POINTS: &str = "default_passing_points";
