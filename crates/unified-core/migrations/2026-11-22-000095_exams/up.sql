-- Online Exams module tables for the Unified Pentagonal Project
-- (`src/modules/exams.rs`): question banks, versioned questions,
-- exams with scheduled windows, attempts with server-side timers,
-- manual grades with rubrics, proctoring signals, and results.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the e-learning precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `user_id` / `owner_id` / `created_by` / `grader_id` columns
--     are plain TEXT with NO foreign key on purpose: users live in
--     the users module and this module must not assume its table or
--     migration order. `company_id` is plain TEXT for the same
--     reason. `exam_questions.bank_id` references
--     `exam_question_banks.id` only by convention, not by a foreign
--     key, for the same reason.
--   - `exam_questions.correct_answer` and `exam_questions.answer_key`
--     are the ONLY place correct answers live. `exam_attempts.answers`
--     is JSONB holding the student's answers only; correct answers
--     are never stored in an attempt. `exam_results.per_question`
--     holds awarded marks per question, never correct answers.
--   - `exam_attempts.seed` records the seed used for the randomised
--     question and option order, so the order is reproducible for
--     review and grading. `expires_at` is the server-computed
--     deadline; the server clock decides expiry, never the client.
--   - `exam_question_versions.snapshot` is the JSONB snapshot of the
--     superseded question version. Attempts always grade against
--     the pinned version in `exam_question_links.question_version`.
--   - `exam_grades.audit` is a JSONB audit trail (regrade, dispute,
--     second grade, override context) kept on the grade row.
--   - `exam_proctor_signals` records tab-switch / window-blur /
--     navigation-away style signals only. No camera or video
--     proctoring is built anywhere in this module.
--   - `exams.academic_enrollment_id` and
--     `exams.academic_component_id` are plain TEXT links to the
--     academic module: grades live in the academic record and are
--     never duplicated here; the module emits
--     `academic.grade.recorded` on grading completion when both
--     are set.
--   - `exam_results.status` is `draft` until a manager publishes
--     the exam's results; `exam_results.percentage` is the score
--     as a percentage of `max_score`.
--   - All ids are Uuid, all timestamps are Timestamptz. JSON
--     columns are JSONB.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Question banks.
CREATE TABLE IF NOT EXISTS exam_question_banks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT NOT NULL DEFAULT '' CHECK (char_length(description) <= 5000),
    category TEXT NOT NULL CHECK (char_length(category) BETWEEN 1 AND 64),
    owner_id TEXT NOT NULL CHECK (char_length(owner_id) BETWEEN 1 AND 200),
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_exam_question_banks_category ON exam_question_banks (category);
CREATE INDEX IF NOT EXISTS idx_exam_question_banks_owner ON exam_question_banks (owner_id);

-- Questions. Correct answers live ONLY here.
CREATE TABLE IF NOT EXISTS exam_questions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bank_id UUID NOT NULL,
    version INT NOT NULL DEFAULT 1 CHECK (version >= 1),
    previous_version_id UUID,
    superseded_by UUID,
    question_type TEXT NOT NULL CHECK (char_length(question_type) BETWEEN 1 AND 64),
    category TEXT NOT NULL CHECK (char_length(category) BETWEEN 1 AND 64),
    difficulty TEXT NOT NULL CHECK (char_length(difficulty) BETWEEN 1 AND 64),
    text TEXT NOT NULL CHECK (char_length(text) BETWEEN 1 AND 20000),
    options JSONB NOT NULL DEFAULT '[]'::jsonb,
    correct_answer JSONB,
    answer_key TEXT CHECK (answer_key IS NULL OR char_length(answer_key) <= 20000),
    marks DOUBLE PRECISION NOT NULL DEFAULT 1 CHECK (marks >= 0 AND marks <= 1000),
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published')),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_exam_questions_bank ON exam_questions (bank_id);
CREATE INDEX IF NOT EXISTS idx_exam_questions_type ON exam_questions (question_type);
CREATE INDEX IF NOT EXISTS idx_exam_questions_difficulty ON exam_questions (difficulty);
CREATE INDEX IF NOT EXISTS idx_exam_questions_status ON exam_questions (status);

-- Question versions (snapshots of superseded versions).
CREATE TABLE IF NOT EXISTS exam_question_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    question_id UUID NOT NULL,
    version INT NOT NULL CHECK (version >= 1),
    snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    reason TEXT CHECK (reason IS NULL OR char_length(reason) <= 2000),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_exam_question_versions_question ON exam_question_versions (question_id);
CREATE INDEX IF NOT EXISTS idx_exam_question_versions_version ON exam_question_versions (question_id, version);

-- Exams.
CREATE TABLE IF NOT EXISTS exams (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT NOT NULL DEFAULT '' CHECK (char_length(description) <= 5000),
    category TEXT NOT NULL DEFAULT 'general' CHECK (char_length(category) BETWEEN 1 AND 64),
    bank_ids UUID[] NOT NULL DEFAULT '{}',
    course_id TEXT,
    academic_enrollment_id TEXT,
    academic_component_id TEXT,
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published', 'archived', 'closed')),
    opens_at TIMESTAMPTZ,
    closes_at TIMESTAMPTZ,
    duration_minutes INT NOT NULL DEFAULT 60 CHECK (duration_minutes BETWEEN 1 AND 10080),
    pass_mark DOUBLE PRECISION NOT NULL DEFAULT 60 CHECK (pass_mark >= 0 AND pass_mark <= 100),
    attempts_allowed INT NOT NULL DEFAULT 1 CHECK (attempts_allowed >= 1),
    settings JSONB NOT NULL DEFAULT '{}'::jsonb,
    total_marks DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (total_marks >= 0),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (closes_at IS NULL OR opens_at IS NULL OR closes_at > opens_at)
);
CREATE INDEX IF NOT EXISTS idx_exams_status ON exams (status);
CREATE INDEX IF NOT EXISTS idx_exams_category ON exams (category);
CREATE INDEX IF NOT EXISTS idx_exams_course ON exams (course_id);
CREATE INDEX IF NOT EXISTS idx_exams_opens ON exams (opens_at);
CREATE INDEX IF NOT EXISTS idx_exams_closes ON exams (closes_at);

-- Exam question links (an exam's question set, pinned versions).
CREATE TABLE IF NOT EXISTS exam_question_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL,
    question_id UUID NOT NULL,
    question_version INT NOT NULL DEFAULT 1 CHECK (question_version >= 1),
    position INT NOT NULL DEFAULT 0 CHECK (position >= 0),
    section TEXT CHECK (section IS NULL OR char_length(section) BETWEEN 1 AND 64),
    marks DOUBLE PRECISION NOT NULL DEFAULT 1 CHECK (marks >= 0 AND marks <= 1000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (exam_id, question_id)
);
CREATE INDEX IF NOT EXISTS idx_exam_question_links_exam ON exam_question_links (exam_id);
CREATE INDEX IF NOT EXISTS idx_exam_question_links_question ON exam_question_links (question_id);

-- Rubrics.
CREATE TABLE IF NOT EXISTS exam_rubrics (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT NOT NULL DEFAULT '' CHECK (char_length(description) <= 5000),
    criteria JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_exam_rubrics_exam ON exam_rubrics (exam_id);

-- Attempts. Answers only; correct answers are never stored here.
CREATE TABLE IF NOT EXISTS exam_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    attempt_number INT NOT NULL DEFAULT 1 CHECK (attempt_number >= 1),
    status TEXT NOT NULL DEFAULT 'in_progress' CHECK (status IN ('in_progress', 'submitted', 'expired', 'graded')),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    duration_seconds BIGINT NOT NULL CHECK (duration_seconds >= 0),
    seed BIGINT NOT NULL,
    question_order JSONB NOT NULL DEFAULT '[]'::jsonb,
    option_orders JSONB NOT NULL DEFAULT '{}'::jsonb,
    answers JSONB NOT NULL DEFAULT '{}'::jsonb,
    autosaved_at TIMESTAMPTZ,
    submitted_at TIMESTAMPTZ,
    score DOUBLE PRECISION CHECK (score IS NULL OR score >= 0),
    max_score DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (max_score >= 0),
    passed BOOLEAN,
    graded_at TIMESTAMPTZ,
    result_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_exam_attempts_exam ON exam_attempts (exam_id);
CREATE INDEX IF NOT EXISTS idx_exam_attempts_student ON exam_attempts (user_id);
CREATE INDEX IF NOT EXISTS idx_exam_attempts_status ON exam_attempts (status);
CREATE INDEX IF NOT EXISTS idx_exam_attempts_exam_student ON exam_attempts (exam_id, user_id);

-- Manual grades (essays) with rubrics and an audit trail.
CREATE TABLE IF NOT EXISTS exam_grades (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    attempt_id UUID NOT NULL,
    exam_id UUID NOT NULL,
    question_id UUID NOT NULL,
    grader_id TEXT,
    rubric_id UUID,
    criterion_scores JSONB NOT NULL DEFAULT '{}'::jsonb,
    marks_awarded DOUBLE PRECISION CHECK (marks_awarded IS NULL OR marks_awarded >= 0),
    max_marks DOUBLE PRECISION NOT NULL CHECK (max_marks >= 0),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'graded', 'disputed', 'second_grading')),
    comment TEXT CHECK (comment IS NULL OR char_length(comment) <= 2000),
    second_grader_id TEXT,
    audit JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_exam_grades_attempt ON exam_grades (attempt_id);
CREATE INDEX IF NOT EXISTS idx_exam_grades_exam ON exam_grades (exam_id);
CREATE INDEX IF NOT EXISTS idx_exam_grades_status ON exam_grades (status);
CREATE INDEX IF NOT EXISTS idx_exam_grades_grader ON exam_grades (grader_id);

-- Proctoring signals (records only; never auto-fail an attempt).
CREATE TABLE IF NOT EXISTS exam_proctor_signals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    attempt_id UUID NOT NULL,
    exam_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    signal_type TEXT NOT NULL CHECK (char_length(signal_type) BETWEEN 1 AND 64),
    detail TEXT CHECK (detail IS NULL OR char_length(detail) <= 2000),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_exam_proctor_signals_attempt ON exam_proctor_signals (attempt_id);
CREATE INDEX IF NOT EXISTS idx_exam_proctor_signals_exam ON exam_proctor_signals (exam_id);
CREATE INDEX IF NOT EXISTS idx_exam_proctor_signals_type ON exam_proctor_signals (signal_type);

-- Results.
CREATE TABLE IF NOT EXISTS exam_results (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    attempt_id UUID NOT NULL,
    exam_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    score DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (score >= 0),
    max_score DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (max_score >= 0),
    percentage DOUBLE PRECISION NOT NULL DEFAULT 0 CHECK (percentage >= 0 AND percentage <= 100),
    passed BOOLEAN NOT NULL DEFAULT FALSE,
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published')),
    per_question JSONB NOT NULL DEFAULT '{}'::jsonb,
    graded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_exam_results_exam ON exam_results (exam_id);
CREATE INDEX IF NOT EXISTS idx_exam_results_student ON exam_results (user_id);
CREATE INDEX IF NOT EXISTS idx_exam_results_status ON exam_results (status);
CREATE INDEX IF NOT EXISTS idx_exam_results_attempt ON exam_results (attempt_id);
