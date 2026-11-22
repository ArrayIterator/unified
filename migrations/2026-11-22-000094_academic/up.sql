-- Academic System module tables for the Unified Pentagonal Project
-- (`src/modules/academic.rs`): study programs, students, lecturers,
-- subjects linked to e-learning courses, rooms, semesters, calendar
-- events, class schedules, course registrations, attendance, grade
-- components, grades, grade appeals, transcripts, theses, academic
-- standing, admin-editable definitions, and settings. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the e-learning precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `user_id` / `department_id` / `elearning_course_id` columns
--     are plain TEXT with NO foreign key on purpose: users live in
--     the users module, departments in the departments module, and
--     e-learning courses in the e-learning module, and this module
--     must not assume their tables or migration order.
--   - Foreign keys are used only BETWEEN academic tables in this
--     same migration.
--   - Status values are NOT constrained by CHECK lists here on
--     purpose: status vocabularies are admin-editable definitions
--     (`academic_definitions`), not fixed database enums. Only the
--     `active` / `inactive` record flags and the transcript
--     `document_type` contract are fixed.
--   - `academic_transcripts.lines` is JSONB: the snapshot of the
--     transcript content (subjects, grades, credits, GPA) taken at
--     issue time, so later grade changes do not rewrite an issued
--     document.
--   - `academic_definitions.config` is JSONB: kind-specific data
--     (grade scale: {min_score, max_score, points}; standing rule:
--     {min_gpa, max_gpa}; transition: {from, to}).
--   - `academic_settings.value` is JSONB: one configurable default
--     per key (for example the default maximum credits per
--     semester).
--   - Seed rows at the end are CONFIGURABLE DEFAULTS (see the
--     module header): they apply only when no admin definition or
--     setting exists yet, and an administrator can replace them at
--     runtime through the definitions and settings endpoints
--     without touching code.
--   - All ids are Uuid, all timestamps are Timestamptz. No secret
--     is stored in any table here.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Study programs (majors).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_programs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 64),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 2000),
    department_id TEXT CHECK (department_id IS NULL OR char_length(department_id) BETWEEN 1 AND 200),
    max_credits_per_semester NUMERIC(6, 2) CHECK (max_credits_per_semester IS NULL OR max_credits_per_semester BETWEEN 0 AND 60),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_programs_code ON academic_programs (code);
CREATE INDEX IF NOT EXISTS idx_academic_programs_status ON academic_programs (status);

-- ---------------------------------------------------------------------------
-- Students. Only the linked user id is stored; names and emails live
-- in the Users module.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_students (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    student_number TEXT NOT NULL CHECK (char_length(student_number) BETWEEN 1 AND 64),
    program_id UUID NOT NULL REFERENCES academic_programs (id) ON DELETE RESTRICT,
    advisor_id UUID REFERENCES academic_lecturers (id) ON DELETE SET NULL,
    enrollment_year INTEGER NOT NULL CHECK (enrollment_year BETWEEN 1990 AND 2100),
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_students_user ON academic_students (user_id);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_students_number ON academic_students (student_number);
CREATE INDEX IF NOT EXISTS idx_academic_students_program ON academic_students (program_id, status);
CREATE INDEX IF NOT EXISTS idx_academic_students_advisor ON academic_students (advisor_id);

-- ---------------------------------------------------------------------------
-- Lecturers (teachers). Only the linked user id is stored; names and
-- emails live in the Users module.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_lecturers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    lecturer_number TEXT NOT NULL CHECK (char_length(lecturer_number) BETWEEN 1 AND 64),
    department_id TEXT CHECK (department_id IS NULL OR char_length(department_id) BETWEEN 1 AND 200),
    title TEXT CHECK (title IS NULL OR char_length(title) BETWEEN 1 AND 2000),
    status TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_lecturers_user ON academic_lecturers (user_id);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_lecturers_number ON academic_lecturers (lecturer_number);
CREATE INDEX IF NOT EXISTS idx_academic_lecturers_status ON academic_lecturers (status);

-- ---------------------------------------------------------------------------
-- Academic subjects (course records). Lessons, quizzes, and attempts
-- live in the E-Learning module; `elearning_course_id` is only a
-- reference to one of its courses.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_subjects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 64),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 2000),
    program_id UUID NOT NULL REFERENCES academic_programs (id) ON DELETE RESTRICT,
    credits NUMERIC(6, 2) NOT NULL CHECK (credits BETWEEN 0 AND 60),
    semester_number INTEGER CHECK (semester_number IS NULL OR semester_number BETWEEN 1 AND 20),
    elearning_course_id TEXT CHECK (elearning_course_id IS NULL OR char_length(elearning_course_id) BETWEEN 1 AND 200),
    elearning_course_verified BOOLEAN NOT NULL DEFAULT FALSE,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_subjects_code ON academic_subjects (code);
CREATE INDEX IF NOT EXISTS idx_academic_subjects_program ON academic_subjects (program_id, status);
CREATE INDEX IF NOT EXISTS idx_academic_subjects_elearning ON academic_subjects (elearning_course_id);

-- ---------------------------------------------------------------------------
-- Rooms where classes meet.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_rooms (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 64),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 2000),
    building TEXT CHECK (building IS NULL OR char_length(building) BETWEEN 1 AND 2000),
    capacity INTEGER CHECK (capacity IS NULL OR capacity BETWEEN 1 AND 10000),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_rooms_code ON academic_rooms (code);
CREATE INDEX IF NOT EXISTS idx_academic_rooms_status ON academic_rooms (status);

-- ---------------------------------------------------------------------------
-- Semesters (academic terms).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_semesters (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 64),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 2000),
    start_date DATE NOT NULL,
    end_date DATE NOT NULL CHECK (end_date > start_date),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'current', 'completed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_semesters_code ON academic_semesters (code);
CREATE INDEX IF NOT EXISTS idx_academic_semesters_status ON academic_semesters (status);

-- ---------------------------------------------------------------------------
-- Academic calendar events: holidays, exam periods, registration
-- windows, and other dated events.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_calendar_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    semester_id UUID REFERENCES academic_semesters (id) ON DELETE SET NULL,
    event_type TEXT NOT NULL CHECK (char_length(event_type) BETWEEN 1 AND 64),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 2000),
    start_date DATE NOT NULL,
    end_date DATE NOT NULL CHECK (end_date >= start_date),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 2000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_academic_calendar_semester ON academic_calendar_events (semester_id, start_date);
CREATE INDEX IF NOT EXISTS idx_academic_calendar_type ON academic_calendar_events (event_type, start_date);

-- ---------------------------------------------------------------------------
-- Weekly class sessions. `day_of_week` follows ISO numbering
-- (1 = Monday, 7 = Sunday); times are `HH:MM` (24-hour clock).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_schedules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_id UUID NOT NULL REFERENCES academic_subjects (id) ON DELETE RESTRICT,
    lecturer_id UUID NOT NULL REFERENCES academic_lecturers (id) ON DELETE RESTRICT,
    room_id UUID NOT NULL REFERENCES academic_rooms (id) ON DELETE RESTRICT,
    semester_id UUID NOT NULL REFERENCES academic_semesters (id) ON DELETE RESTRICT,
    day_of_week INTEGER NOT NULL CHECK (day_of_week BETWEEN 1 AND 7),
    start_time TEXT NOT NULL CHECK (start_time ~ '^[0-2][0-9]:[0-5][0-9]$'),
    end_time TEXT NOT NULL CHECK (end_time ~ '^[0-2][0-9]:[0-5][0-9]$'),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_academic_schedules_subject ON academic_schedules (subject_id, semester_id);
CREATE INDEX IF NOT EXISTS idx_academic_schedules_lecturer ON academic_schedules (lecturer_id, semester_id, day_of_week);
CREATE INDEX IF NOT EXISTS idx_academic_schedules_room ON academic_schedules (room_id, semester_id, day_of_week);

-- ---------------------------------------------------------------------------
-- Course registrations. `credits` is snapshotted from the subject at
-- request time so later subject edits do not rewrite history.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_enrollments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    student_id UUID NOT NULL REFERENCES academic_students (id) ON DELETE RESTRICT,
    subject_id UUID NOT NULL REFERENCES academic_subjects (id) ON DELETE RESTRICT,
    semester_id UUID NOT NULL REFERENCES academic_semesters (id) ON DELETE RESTRICT,
    credits NUMERIC(6, 2) NOT NULL CHECK (credits BETWEEN 0 AND 60),
    status TEXT NOT NULL DEFAULT 'requested',
    advisor_id UUID REFERENCES academic_lecturers (id) ON DELETE SET NULL,
    rejection_reason TEXT CHECK (rejection_reason IS NULL OR char_length(rejection_reason) BETWEEN 1 AND 2000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_enrollments_live
    ON academic_enrollments (student_id, subject_id, semester_id)
    WHERE status IN ('requested', 'approved');
CREATE INDEX IF NOT EXISTS idx_academic_enrollments_student ON academic_enrollments (student_id, semester_id, status);
CREATE INDEX IF NOT EXISTS idx_academic_enrollments_subject ON academic_enrollments (subject_id, semester_id, status);

-- ---------------------------------------------------------------------------
-- Class attendance. `source` is `manual` or `timetracker`;
-- `entry_id` holds the TimeTracker entry id for de-duplication.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_attendance (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    schedule_id UUID NOT NULL REFERENCES academic_schedules (id) ON DELETE RESTRICT,
    student_id UUID NOT NULL REFERENCES academic_students (id) ON DELETE RESTRICT,
    date DATE NOT NULL,
    status TEXT NOT NULL DEFAULT 'present',
    source TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'timetracker')),
    entry_id TEXT CHECK (entry_id IS NULL OR char_length(entry_id) BETWEEN 1 AND 200),
    minutes BIGINT CHECK (minutes IS NULL OR minutes BETWEEN 0 AND 1440),
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 2000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_attendance_session
    ON academic_attendance (schedule_id, student_id, date);
CREATE INDEX IF NOT EXISTS idx_academic_attendance_student ON academic_attendance (student_id, date);
CREATE INDEX IF NOT EXISTS idx_academic_attendance_entry ON academic_attendance (entry_id);

-- ---------------------------------------------------------------------------
-- Grade components of a subject (for example a midterm worth 30%).
-- Weights are configurable per subject.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_grade_components (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_id UUID NOT NULL REFERENCES academic_subjects (id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 2000),
    kind TEXT NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    weight_percent NUMERIC(6, 2) NOT NULL CHECK (weight_percent BETWEEN 0 AND 100),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_academic_grade_components_subject ON academic_grade_components (subject_id);

-- ---------------------------------------------------------------------------
-- Recorded grades: one score (0-100) per enrollment and component.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_grades (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    enrollment_id UUID NOT NULL REFERENCES academic_enrollments (id) ON DELETE CASCADE,
    component_id UUID NOT NULL REFERENCES academic_grade_components (id) ON DELETE RESTRICT,
    score NUMERIC(6, 2) NOT NULL CHECK (score BETWEEN 0 AND 100),
    graded_by UUID REFERENCES academic_lecturers (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_grades_enrollment_component
    ON academic_grades (enrollment_id, component_id);
CREATE INDEX IF NOT EXISTS idx_academic_grades_enrollment ON academic_grades (enrollment_id);

-- ---------------------------------------------------------------------------
-- Grade appeals. A whole-enrollment appeal has a NULL component_id;
-- a component appeal names the component.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_grade_appeals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    enrollment_id UUID NOT NULL REFERENCES academic_enrollments (id) ON DELETE RESTRICT,
    component_id UUID REFERENCES academic_grade_components (id) ON DELETE SET NULL,
    student_id UUID NOT NULL REFERENCES academic_students (id) ON DELETE RESTRICT,
    reason TEXT NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 2000),
    status TEXT NOT NULL DEFAULT 'open',
    reviewed_by UUID REFERENCES academic_lecturers (id) ON DELETE SET NULL,
    decision TEXT CHECK (decision IS NULL OR decision IN ('upheld', 'denied')),
    corrected_score NUMERIC(6, 2) CHECK (corrected_score IS NULL OR corrected_score BETWEEN 0 AND 100),
    resolution_notes TEXT CHECK (resolution_notes IS NULL OR char_length(resolution_notes) BETWEEN 1 AND 2000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_academic_grade_appeals_enrollment ON academic_grade_appeals (enrollment_id, status);
CREATE INDEX IF NOT EXISTS idx_academic_grade_appeals_student ON academic_grade_appeals (student_id, status);

-- ---------------------------------------------------------------------------
-- Issued official transcripts. `lines` is the content snapshot
-- taken at issue time.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_transcripts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    student_id UUID NOT NULL REFERENCES academic_students (id) ON DELETE RESTRICT,
    issued_by TEXT NOT NULL CHECK (char_length(issued_by) BETWEEN 1 AND 200),
    gpa NUMERIC(5, 2) NOT NULL CHECK (gpa BETWEEN 0 AND 4),
    total_credits NUMERIC(8, 2) NOT NULL CHECK (total_credits >= 0),
    lines JSONB NOT NULL DEFAULT '[]'::jsonb,
    print_job_id TEXT CHECK (print_job_id IS NULL OR char_length(print_job_id) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_academic_transcripts_student ON academic_transcripts (student_id, created_at);

-- ---------------------------------------------------------------------------
-- Theses / final projects.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_theses (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    student_id UUID NOT NULL REFERENCES academic_students (id) ON DELETE RESTRICT,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 2000),
    advisor_id UUID NOT NULL REFERENCES academic_lecturers (id) ON DELETE RESTRICT,
    co_advisor_id UUID REFERENCES academic_lecturers (id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'proposal',
    proposal_date DATE,
    defense_date DATE,
    defense_result TEXT CHECK (defense_result IS NULL OR char_length(defense_result) BETWEEN 1 AND 2000),
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 2000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_academic_theses_student ON academic_theses (student_id, status);
CREATE INDEX IF NOT EXISTS idx_academic_theses_advisor ON academic_theses (advisor_id, status);

-- ---------------------------------------------------------------------------
-- Academic standing records: one per student and semester (latest
-- wins). `standing` is a key from the `standing_rule` definitions.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_standing_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    student_id UUID NOT NULL REFERENCES academic_students (id) ON DELETE RESTRICT,
    semester_id UUID NOT NULL REFERENCES academic_semesters (id) ON DELETE RESTRICT,
    gpa NUMERIC(5, 2) NOT NULL CHECK (gpa BETWEEN 0 AND 4),
    total_credits NUMERIC(8, 2) NOT NULL CHECK (total_credits >= 0),
    standing TEXT NOT NULL CHECK (char_length(standing) BETWEEN 1 AND 64),
    reason TEXT CHECK (reason IS NULL OR char_length(reason) BETWEEN 1 AND 2000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_standing_student_semester
    ON academic_standing_records (student_id, semester_id);
CREATE INDEX IF NOT EXISTS idx_academic_standing_standing ON academic_standing_records (standing);

-- ---------------------------------------------------------------------------
-- Admin-editable definitions: status vocabularies, grade scales,
-- standing rules, and state-machine transitions.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_definitions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind TEXT NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    key TEXT NOT NULL CHECK (char_length(key) BETWEEN 1 AND 64),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 2000),
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_academic_definitions_kind_key ON academic_definitions (kind, key);
CREATE INDEX IF NOT EXISTS idx_academic_definitions_kind ON academic_definitions (kind, sort_order);

-- ---------------------------------------------------------------------------
-- Admin-editable scalar settings (configurable defaults).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS academic_settings (
    key TEXT PRIMARY KEY CHECK (char_length(key) BETWEEN 1 AND 64),
    value JSONB NOT NULL DEFAULT 'null'::jsonb,
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 2000),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Seed rows: CONFIGURABLE DEFAULTS (see the header). They apply only
-- when no admin definition or setting exists yet.
-- ---------------------------------------------------------------------------
INSERT INTO academic_definitions (kind, key, label, config, sort_order) VALUES
    ('grade_scale', 'A', 'A', '{"min_score": 90, "max_score": 100, "points": 4.0}'::jsonb, 1),
    ('grade_scale', 'B', 'B', '{"min_score": 80, "max_score": 89.99, "points": 3.0}'::jsonb, 2),
    ('grade_scale', 'C', 'C', '{"min_score": 70, "max_score": 79.99, "points": 2.0}'::jsonb, 3),
    ('grade_scale', 'D', 'D', '{"min_score": 60, "max_score": 69.99, "points": 1.0}'::jsonb, 4),
    ('grade_scale', 'F', 'F', '{"min_score": 0, "max_score": 59.99, "points": 0.0}'::jsonb, 5),
    ('standing_rule', 'good', 'Good standing', '{"min_gpa": 2.5, "max_gpa": 4.0}'::jsonb, 1),
    ('standing_rule', 'warning', 'Academic warning', '{"min_gpa": 2.0, "max_gpa": 2.4999}'::jsonb, 2),
    ('standing_rule', 'probation', 'Academic probation', '{"min_gpa": 1.0, "max_gpa": 1.9999}'::jsonb, 3),
    ('standing_rule', 'dismissed', 'Dismissed', '{"min_gpa": 0.0, "max_gpa": 0.9999}'::jsonb, 4),
    ('student_status', 'active', 'Active', '{}'::jsonb, 1),
    ('student_status', 'on_leave', 'On leave', '{}'::jsonb, 2),
    ('student_status', 'graduated', 'Graduated', '{}'::jsonb, 3),
    ('student_status', 'withdrawn', 'Withdrawn', '{}'::jsonb, 4),
    ('lecturer_status', 'active', 'Active', '{}'::jsonb, 1),
    ('lecturer_status', 'on_leave', 'On leave', '{}'::jsonb, 2),
    ('lecturer_status', 'retired', 'Retired', '{}'::jsonb, 3),
    ('enrollment_status', 'requested', 'Requested', '{}'::jsonb, 1),
    ('enrollment_status', 'approved', 'Approved', '{}'::jsonb, 2),
    ('enrollment_status', 'rejected', 'Rejected', '{}'::jsonb, 3),
    ('enrollment_status', 'dropped', 'Dropped', '{}'::jsonb, 4),
    ('enrollment_status', 'completed', 'Completed', '{}'::jsonb, 5),
    ('attendance_status', 'present', 'Present', '{}'::jsonb, 1),
    ('attendance_status', 'absent', 'Absent', '{}'::jsonb, 2),
    ('attendance_status', 'late', 'Late', '{}'::jsonb, 3),
    ('attendance_status', 'excused', 'Excused', '{}'::jsonb, 4),
    ('grade_component_kind', 'midterm', 'Midterm', '{}'::jsonb, 1),
    ('grade_component_kind', 'final', 'Final', '{}'::jsonb, 2),
    ('grade_component_kind', 'assignment', 'Assignment', '{}'::jsonb, 3),
    ('grade_component_kind', 'quiz', 'Quiz', '{}'::jsonb, 4),
    ('grade_component_kind', 'project', 'Project', '{}'::jsonb, 5),
    ('grade_component_kind', 'participation', 'Participation', '{}'::jsonb, 6),
    ('thesis_status', 'proposal', 'Proposal', '{}'::jsonb, 1),
    ('thesis_status', 'in_progress', 'In progress', '{}'::jsonb, 2),
    ('thesis_status', 'defense_scheduled', 'Defense scheduled', '{}'::jsonb, 3),
    ('thesis_status', 'defended', 'Defended', '{}'::jsonb, 4),
    ('thesis_status', 'completed', 'Completed', '{}'::jsonb, 5),
    ('thesis_status', 'rejected', 'Rejected', '{}'::jsonb, 6),
    ('appeal_status', 'open', 'Open', '{}'::jsonb, 1),
    ('appeal_status', 'under_review', 'Under review', '{}'::jsonb, 2),
    ('appeal_status', 'upheld', 'Upheld', '{}'::jsonb, 3),
    ('appeal_status', 'denied', 'Denied', '{}'::jsonb, 4),
    ('calendar_event_type', 'holiday', 'Holiday', '{}'::jsonb, 1),
    ('calendar_event_type', 'exam_period', 'Exam period', '{}'::jsonb, 2),
    ('calendar_event_type', 'semester_start', 'Semester start', '{}'::jsonb, 3),
    ('calendar_event_type', 'semester_end', 'Semester end', '{}'::jsonb, 4),
    ('calendar_event_type', 'registration', 'Registration', '{}'::jsonb, 5),
    ('calendar_event_type', 'other', 'Other', '{}'::jsonb, 6),
    ('thesis_transition', 'proposal->in_progress', 'Proposal to in progress', '{"from": "proposal", "to": "in_progress"}'::jsonb, 1),
    ('thesis_transition', 'proposal->rejected', 'Proposal to rejected', '{"from": "proposal", "to": "rejected"}'::jsonb, 2),
    ('thesis_transition', 'in_progress->defense_scheduled', 'In progress to defense scheduled', '{"from": "in_progress", "to": "defense_scheduled"}'::jsonb, 3),
    ('thesis_transition', 'in_progress->rejected', 'In progress to rejected', '{"from": "in_progress", "to": "rejected"}'::jsonb, 4),
    ('thesis_transition', 'defense_scheduled->defended', 'Defense scheduled to defended', '{"from": "defense_scheduled", "to": "defended"}'::jsonb, 5),
    ('thesis_transition', 'defense_scheduled->in_progress', 'Defense scheduled to in progress', '{"from": "defense_scheduled", "to": "in_progress"}'::jsonb, 6),
    ('thesis_transition', 'defended->completed', 'Defended to completed', '{"from": "defended", "to": "completed"}'::jsonb, 7),
    ('enrollment_transition', 'requested->approved', 'Requested to approved', '{"from": "requested", "to": "approved"}'::jsonb, 1),
    ('enrollment_transition', 'requested->rejected', 'Requested to rejected', '{"from": "requested", "to": "rejected"}'::jsonb, 2),
    ('enrollment_transition', 'requested->dropped', 'Requested to dropped', '{"from": "requested", "to": "dropped"}'::jsonb, 3),
    ('enrollment_transition', 'approved->dropped', 'Approved to dropped', '{"from": "approved", "to": "dropped"}'::jsonb, 4),
    ('enrollment_transition', 'approved->completed', 'Approved to completed', '{"from": "approved", "to": "completed"}'::jsonb, 5),
    ('enrollment_transition', 'rejected->requested', 'Rejected to requested', '{"from": "rejected", "to": "requested"}'::jsonb, 6)
ON CONFLICT (kind, key) DO NOTHING;

INSERT INTO academic_settings (key, value, description) VALUES
    ('default_max_credits_per_semester', '24', 'Default maximum credits a student may register per semester, used when a program has no per-program cap.'),
    ('default_passing_points', '2.0', 'Default passing grade points: a final letter with fewer points does not earn credit towards graduation totals.')
ON CONFLICT (key) DO NOTHING;
