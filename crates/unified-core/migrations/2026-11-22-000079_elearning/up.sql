-- E-Learning module tables for the Unified Pentagonal Project
-- (`src/modules/elearning.rs`): courses, modules, lessons,
-- enrollments, lesson progress, quizzes, questions, attempts,
-- certificates, reviews, and learning paths. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the gamification precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `instructor_id` / `user_id` / `learner` columns are plain
--     TEXT with NO foreign key on purpose: users live in the users
--     module and this module must not assume its table or
--     migration order. `company_id` is plain TEXT for the same
--     reason.
--   - `courses.thumbnail_ref` and `lessons.content_ref` are storage
--     references only (opaque strings handed to the storage
--     module); no bytes are stored here.
--   - `elearning_certificates.code` is unique and is the public verification
--     code; `content_sha256` is the SHA-256 fingerprint of the
--     canonical certificate payload, handed to the anchoring flow.
--     No secret is stored in any table here.
--     (Renamed from `certificates` during the final inspection:
--     migration 000005 already creates `certificates` as the SSL
--     certificate table, so this table is `elearning_certificates`.)
--   - `quiz_attempts.answers` is JSONB: an array of
--     `{"question_id", "selected_index"}` objects. Correct answers
--     are never stored in an attempt; they live on
--     `quiz_questions.correct_index` and are shown only when the
--     quiz allows review.
--   - `enrollments` has a partial unique index on
--     (course_id, user_id) for non-cancelled rows, matching the
--     one-live-enrollment rule in the module.
--   - All ids are Uuid, all timestamps are Timestamptz. JSON
--     columns are JSONB.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Courses.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS courses (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT NOT NULL CHECK (char_length(description) BETWEEN 1 AND 5000),
    category TEXT NOT NULL CHECK (char_length(category) BETWEEN 1 AND 64),
    level TEXT NOT NULL CHECK (char_length(level) BETWEEN 1 AND 64),
    language TEXT NOT NULL CHECK (char_length(language) BETWEEN 1 AND 32),
    thumbnail_ref TEXT CHECK (thumbnail_ref IS NULL OR char_length(thumbnail_ref) BETWEEN 1 AND 500),
    instructor_id TEXT NOT NULL CHECK (char_length(instructor_id) BETWEEN 1 AND 200),
    status TEXT NOT NULL CHECK (status IN ('draft', 'published', 'archived')),
    price_minor BIGINT NOT NULL DEFAULT 0 CHECK (price_minor >= 0),
    currency TEXT NOT NULL DEFAULT 'USD' CHECK (char_length(currency) = 3),
    certificate_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_courses_status ON courses (status);
CREATE INDEX IF NOT EXISTS idx_courses_category ON courses (category);
CREATE INDEX IF NOT EXISTS idx_courses_instructor ON courses (instructor_id);

-- ---------------------------------------------------------------------------
-- Modules inside a course.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS course_modules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    course_id UUID NOT NULL REFERENCES courses (id) ON DELETE CASCADE,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    position BIGINT NOT NULL DEFAULT 0 CHECK (position >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_course_modules_course ON course_modules (course_id, position);

-- ---------------------------------------------------------------------------
-- Lessons inside a module. `course_id` is denormalized so progress
-- and drip queries do not need a join through the module.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS lessons (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    module_id UUID NOT NULL REFERENCES course_modules (id) ON DELETE CASCADE,
    course_id UUID NOT NULL REFERENCES courses (id) ON DELETE CASCADE,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    lesson_type TEXT NOT NULL CHECK (char_length(lesson_type) BETWEEN 1 AND 64),
    content_ref TEXT CHECK (content_ref IS NULL OR char_length(content_ref) BETWEEN 1 AND 500),
    body TEXT CHECK (body IS NULL OR char_length(body) BETWEEN 1 AND 20000),
    position BIGINT NOT NULL DEFAULT 0 CHECK (position >= 0),
    required BOOLEAN NOT NULL DEFAULT TRUE,
    available_from TIMESTAMPTZ,
    requires_previous BOOLEAN NOT NULL DEFAULT FALSE,
    duration_minutes BIGINT CHECK (duration_minutes IS NULL OR duration_minutes BETWEEN 0 AND 100000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_lessons_module ON lessons (module_id, position);
CREATE INDEX IF NOT EXISTS idx_lessons_course ON lessons (course_id);

-- ---------------------------------------------------------------------------
-- Enrollments of users in courses.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS enrollments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    course_id UUID NOT NULL REFERENCES courses (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    status TEXT NOT NULL CHECK (status IN ('active', 'pending_payment', 'completed', 'cancelled', 'suspended')),
    price_paid_minor BIGINT NOT NULL DEFAULT 0 CHECK (price_paid_minor >= 0),
    currency TEXT NOT NULL DEFAULT 'USD' CHECK (char_length(currency) = 3),
    order_id TEXT,
    learner_name TEXT CHECK (learner_name IS NULL OR char_length(learner_name) BETWEEN 1 AND 200),
    enrolled_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_enrollments_live
    ON enrollments (course_id, user_id) WHERE status <> 'cancelled';
CREATE INDEX IF NOT EXISTS idx_enrollments_user ON enrollments (user_id);
CREATE INDEX IF NOT EXISTS idx_enrollments_course ON enrollments (course_id, status);

-- ---------------------------------------------------------------------------
-- Per-lesson completion rows.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS lesson_progress (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    enrollment_id UUID NOT NULL REFERENCES enrollments (id) ON DELETE CASCADE,
    lesson_id UUID NOT NULL REFERENCES lessons (id) ON DELETE CASCADE,
    course_id UUID NOT NULL REFERENCES courses (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    completed BOOLEAN NOT NULL DEFAULT FALSE,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_lesson_progress_enrollment_lesson
    ON lesson_progress (enrollment_id, lesson_id);
CREATE INDEX IF NOT EXISTS idx_lesson_progress_user ON lesson_progress (user_id, course_id);

-- ---------------------------------------------------------------------------
-- Quizzes attached to lessons (one quiz per lesson).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS quizzes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    lesson_id UUID NOT NULL REFERENCES lessons (id) ON DELETE CASCADE,
    course_id UUID NOT NULL REFERENCES courses (id) ON DELETE CASCADE,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    pass_mark BIGINT NOT NULL DEFAULT 70 CHECK (pass_mark BETWEEN 0 AND 100),
    max_attempts BIGINT CHECK (max_attempts IS NULL OR max_attempts BETWEEN 1 AND 1000),
    allow_review BOOLEAN NOT NULL DEFAULT FALSE,
    time_limit_minutes BIGINT CHECK (time_limit_minutes IS NULL OR time_limit_minutes BETWEEN 1 AND 100000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_quizzes_lesson ON quizzes (lesson_id);
CREATE INDEX IF NOT EXISTS idx_quizzes_course ON quizzes (course_id);

-- ---------------------------------------------------------------------------
-- Multiple-choice questions of a quiz.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS quiz_questions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    quiz_id UUID NOT NULL REFERENCES quizzes (id) ON DELETE CASCADE,
    question TEXT NOT NULL CHECK (char_length(question) BETWEEN 1 AND 20000),
    options JSONB NOT NULL,
    correct_index BIGINT NOT NULL CHECK (correct_index >= 0),
    points BIGINT NOT NULL DEFAULT 1 CHECK (points BETWEEN 1 AND 1000),
    position BIGINT NOT NULL DEFAULT 0 CHECK (position >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_quiz_questions_quiz ON quiz_questions (quiz_id, position);

-- ---------------------------------------------------------------------------
-- Quiz attempts (auto-graded at submission).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS quiz_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    quiz_id UUID NOT NULL REFERENCES quizzes (id) ON DELETE CASCADE,
    lesson_id UUID NOT NULL REFERENCES lessons (id) ON DELETE CASCADE,
    enrollment_id UUID NOT NULL REFERENCES enrollments (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    answers JSONB NOT NULL DEFAULT '[]'::jsonb,
    score BIGINT NOT NULL DEFAULT 0 CHECK (score >= 0),
    max_score BIGINT NOT NULL DEFAULT 0 CHECK (max_score >= 0),
    percent BIGINT NOT NULL DEFAULT 0 CHECK (percent BETWEEN 0 AND 100),
    passed BOOLEAN NOT NULL DEFAULT FALSE,
    submitted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_quiz_attempts_quiz_user ON quiz_attempts (quiz_id, user_id);
CREATE INDEX IF NOT EXISTS idx_quiz_attempts_enrollment ON quiz_attempts (enrollment_id);

-- ---------------------------------------------------------------------------
-- Certificates issued on course completion.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS elearning_certificates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL UNIQUE CHECK (char_length(code) BETWEEN 1 AND 64),
    course_id UUID NOT NULL REFERENCES courses (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    learner_name TEXT NOT NULL CHECK (char_length(learner_name) BETWEEN 1 AND 200),
    course_title TEXT NOT NULL CHECK (char_length(course_title) BETWEEN 1 AND 200),
    issuer_name TEXT NOT NULL CHECK (char_length(issuer_name) BETWEEN 1 AND 200),
    issued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    content_sha256 TEXT NOT NULL CHECK (char_length(content_sha256) = 64),
    status TEXT NOT NULL CHECK (status IN ('valid', 'revoked')),
    anchor_status TEXT NOT NULL DEFAULT 'none' CHECK (anchor_status IN ('none', 'requested', 'confirmed', 'failed')),
    anchor_tx TEXT,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_elearning_certificates_course_user ON elearning_certificates (course_id, user_id);
CREATE INDEX IF NOT EXISTS idx_elearning_certificates_code ON elearning_certificates (code);

-- ---------------------------------------------------------------------------
-- Course reviews (one per user per course).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS reviews (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    course_id UUID NOT NULL REFERENCES courses (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    rating BIGINT NOT NULL CHECK (rating BETWEEN 1 AND 5),
    comment TEXT CHECK (comment IS NULL OR char_length(comment) BETWEEN 1 AND 2000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_reviews_course_user ON reviews (course_id, user_id);
CREATE INDEX IF NOT EXISTS idx_reviews_course ON reviews (course_id);

-- ---------------------------------------------------------------------------
-- Learning paths (ordered sequences of courses).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS learning_paths (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS learning_path_courses (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    path_id UUID NOT NULL REFERENCES learning_paths (id) ON DELETE CASCADE,
    course_id UUID NOT NULL REFERENCES courses (id) ON DELETE CASCADE,
    position BIGINT NOT NULL DEFAULT 0 CHECK (position >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_learning_path_courses_path_course
    ON learning_path_courses (path_id, course_id);
CREATE INDEX IF NOT EXISTS idx_learning_path_courses_path
    ON learning_path_courses (path_id, position);
