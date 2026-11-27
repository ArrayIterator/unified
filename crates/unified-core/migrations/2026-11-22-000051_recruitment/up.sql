-- Recruitment / ATS tables for the Unified Pentagonal Project
-- (`src/modules/recruitment.rs`): job postings, applicants with
-- a validated hiring pipeline, interviews, per-interviewer
-- evaluations, and offers.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/recruitment.rs`, which
-- stay authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `jobs.created_by`, `applicants` contact fields, and
--     `evaluations.interviewer_id` are plain VARCHAR user ids:
--     the module identifies callers by the `X-User-Id` header and
--     keeps user ids as strings, exactly like
--     `store_orders.user_id` in migration 000002. No foreign key
--     to `users` is stored here.
--   - `jobs.department_id` is a plain VARCHAR reference to the
--     Departments module; its existence is not verified here and
--     no foreign key to a departments table is stored.
--   - `jobs.status` is `draft`, `published`, or `closed`
--     (`JobStatus`); `job_type` is `full_time`, `part_time`,
--     `contract`, `internship`, or `freelance` (`JobType`).
--     Salary range (`salary_min` / `salary_max`) is NUMERIC(19,4),
--     never negative, and `salary_max` never below `salary_min`.
--   - `applicants.stage` is `applied`, `screening`, `interview`,
--     `offer`, `hired`, or `rejected` (`ApplicantStage`). The
--     pipeline is forward-only with `rejected` reachable from any
--     open stage; the CHECK below mirrors the module's
--     `allowed_stage_transition` only loosely - the strict
--     transition rules live in the module, not in SQL.
--   - `applicants.resume_storage_target` /
--     `resume_storage_key` are metadata only: the resume bytes
--     live in the storage module. Both are given together or
--     neither (enforced by the CHECK). `consent_contact` gates
--     every outbound contact; `consent_at` records when consent
--     was given. One application per email per job (unique index).
--   - `applicant_stage_history` is append-only: every pipeline
--     move writes a row with the previous stage, the new stage,
--     the actor, and the reason.
--   - `interviews.type` is `phone`, `video`, `onsite`, or `panel`
--     (`InterviewType`); `outcome` is `passed`, `failed`, or
--     `no_show` once recorded; `status` is `scheduled`,
--     `completed`, or `cancelled`. `scheduler_status` is
--     `not_scheduled`, `queued`, or `declined`.
--   - `evaluations.scores` is JSONB: an array of
--     `{"criterion", "score" (1..5), "note"?}` objects
--     (`EvaluationScore`). The score range is enforced in the
--     module; the CHECK below only requires a non-empty array.
--     One evaluation per interviewer per interview (unique index).
--   - `offers.status` is `draft`, `sent`, `accepted`, `declined`,
--     or `expired` (`OfferStatus`); `salary` is NUMERIC(19,4),
--     never negative.

CREATE TABLE IF NOT EXISTS jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title VARCHAR(200) NOT NULL,
    description TEXT,
    department_id VARCHAR(200),
    location VARCHAR(200),
    job_type VARCHAR(20) NOT NULL DEFAULT 'full_time'
        CHECK (job_type IN ('full_time', 'part_time', 'contract', 'internship', 'freelance')),
    salary_min NUMERIC(19, 4) CHECK (salary_min IS NULL OR salary_min >= 0),
    salary_max NUMERIC(19, 4) CHECK (salary_max IS NULL OR salary_max >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    status VARCHAR(20) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'published', 'closed')),
    created_by VARCHAR(200) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (salary_min IS NULL OR salary_max IS NULL OR salary_max >= salary_min)
);

CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs (status);
CREATE INDEX IF NOT EXISTS idx_jobs_job_type ON jobs (job_type);
CREATE INDEX IF NOT EXISTS idx_jobs_department_id ON jobs (department_id);
CREATE INDEX IF NOT EXISTS idx_jobs_location ON jobs (location);
CREATE INDEX IF NOT EXISTS idx_jobs_created_by ON jobs (created_by);

CREATE TABLE IF NOT EXISTS applicants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id UUID NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    name VARCHAR(200) NOT NULL,
    email VARCHAR(254) NOT NULL,
    phone VARCHAR(200),
    source VARCHAR(120),
    resume_storage_target VARCHAR(512),
    resume_storage_key VARCHAR(512),
    consent_contact BOOLEAN NOT NULL DEFAULT FALSE,
    consent_at TIMESTAMPTZ,
    stage VARCHAR(20) NOT NULL DEFAULT 'applied'
        CHECK (stage IN ('applied', 'screening', 'interview', 'offer', 'hired', 'rejected')),
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((resume_storage_target IS NULL) = (resume_storage_key IS NULL)),
    CHECK (consent_contact = FALSE OR consent_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_applicants_job_id ON applicants (job_id);
CREATE INDEX IF NOT EXISTS idx_applicants_stage ON applicants (stage);
CREATE INDEX IF NOT EXISTS idx_applicants_email ON applicants (email);
CREATE INDEX IF NOT EXISTS idx_applicants_source ON applicants (source);
-- One application per email per job.
CREATE UNIQUE INDEX IF NOT EXISTS uq_applicants_job_email ON applicants (job_id, email);

CREATE TABLE IF NOT EXISTS applicant_stage_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    applicant_id UUID NOT NULL REFERENCES applicants (id) ON DELETE CASCADE,
    job_id UUID NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    from_stage VARCHAR(20)
        CHECK (from_stage IS NULL OR from_stage IN ('applied', 'screening', 'interview', 'offer', 'hired', 'rejected')),
    to_stage VARCHAR(20) NOT NULL
        CHECK (to_stage IN ('applied', 'screening', 'interview', 'offer', 'hired', 'rejected')),
    moved_by VARCHAR(200) NOT NULL,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_applicant_stage_history_applicant_id
    ON applicant_stage_history (applicant_id);
CREATE INDEX IF NOT EXISTS idx_applicant_stage_history_job_id
    ON applicant_stage_history (job_id);
CREATE INDEX IF NOT EXISTS idx_applicant_stage_history_created_at
    ON applicant_stage_history (created_at);

CREATE TABLE IF NOT EXISTS interviews (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    applicant_id UUID NOT NULL REFERENCES applicants (id) ON DELETE CASCADE,
    job_id UUID NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    scheduled_at TIMESTAMPTZ NOT NULL,
    type VARCHAR(20) NOT NULL DEFAULT 'video'
        CHECK (type IN ('phone', 'video', 'onsite', 'panel')),
    interviewers JSONB NOT NULL DEFAULT '[]'::jsonb,
    notes TEXT,
    outcome VARCHAR(20)
        CHECK (outcome IS NULL OR outcome IN ('passed', 'failed', 'no_show')),
    outcome_notes TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'scheduled'
        CHECK (status IN ('scheduled', 'completed', 'cancelled')),
    scheduler_status VARCHAR(20) NOT NULL DEFAULT 'not_scheduled'
        CHECK (scheduler_status IN ('not_scheduled', 'queued', 'declined')),
    scheduler_job_id VARCHAR(200),
    created_by VARCHAR(200) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (status <> 'completed' OR outcome IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_interviews_applicant_id ON interviews (applicant_id);
CREATE INDEX IF NOT EXISTS idx_interviews_job_id ON interviews (job_id);
CREATE INDEX IF NOT EXISTS idx_interviews_scheduled_at ON interviews (scheduled_at);
CREATE INDEX IF NOT EXISTS idx_interviews_status ON interviews (status);

CREATE TABLE IF NOT EXISTS evaluations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    applicant_id UUID NOT NULL REFERENCES applicants (id) ON DELETE CASCADE,
    job_id UUID NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    interviewer_id VARCHAR(200) NOT NULL,
    interview_id UUID REFERENCES interviews (id) ON DELETE SET NULL,
    scores JSONB NOT NULL,
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (jsonb_array_length(scores) >= 1)
);

CREATE INDEX IF NOT EXISTS idx_evaluations_applicant_id ON evaluations (applicant_id);
CREATE INDEX IF NOT EXISTS idx_evaluations_job_id ON evaluations (job_id);
CREATE INDEX IF NOT EXISTS idx_evaluations_interviewer_id ON evaluations (interviewer_id);
CREATE INDEX IF NOT EXISTS idx_evaluations_interview_id ON evaluations (interview_id);
-- One scorecard per interviewer per interview.
CREATE UNIQUE INDEX IF NOT EXISTS uq_evaluations_interview_interviewer
    ON evaluations (interview_id, interviewer_id) WHERE interview_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS offers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    applicant_id UUID NOT NULL REFERENCES applicants (id) ON DELETE CASCADE,
    job_id UUID NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    salary NUMERIC(19, 4) NOT NULL CHECK (salary >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    start_date DATE,
    terms TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'sent', 'accepted', 'declined', 'expired')),
    created_by VARCHAR(200) NOT NULL,
    sent_at TIMESTAMPTZ,
    decided_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (status = 'draft' OR sent_at IS NOT NULL),
    CHECK (status IN ('draft', 'sent') OR decided_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_offers_applicant_id ON offers (applicant_id);
CREATE INDEX IF NOT EXISTS idx_offers_job_id ON offers (job_id);
CREATE INDEX IF NOT EXISTS idx_offers_status ON offers (status);
