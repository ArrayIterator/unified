-- Curriculum tables for the Unified Pentagonal Project
-- (`src/modules/curriculum.rs`): competency-based school curricula
-- with a framework registry, school levels, subjects, hierarchical
-- core and basic competencies versioned per curriculum year,
-- curriculum structure with hours allocation, a configurable
-- assessment-domain registry, grade descriptors per band, lesson
-- plans, per-semester report cards with entries, and remedial /
-- enrichment interventions. Matches src/db/schema.rs conventions.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `curriculum_report_cards.student_ref` and
--     `curriculum_interventions.student_ref` are plain VARCHAR
--     references to students owned by the academic module: students
--     are never duplicated here, exactly like `webinars.host_id`
--     and `store_orders.user_id` elsewhere. The same holds for
--     `curriculum_report_card_entries.academic_grade_id`, which
--     links (never copies) a grade record owned by the academic
--     module.
--   - `curriculum_frameworks` is the framework registry. The seeded
--     row is the "curriculum 2013" framework (`is_built_in` true).
--     A newer national curriculum is added as another row here plus
--     its levels, subjects, competencies, and structures — no code
--     change. The module refuses to delete a built-in framework.
--   - `curriculum_levels` rows are data, not an enum: the seeded
--     rows are elementary (6 grades), junior-high (3 grades), and
--     senior-high (3 grades). Schools add their own levels as rows.
--   - Competency hierarchy is subject -> core competency -> basic
--     competency. `version` is the curriculum year a competency
--     version belongs to, so a competency can be revised per year
--     without losing the older version.
--   - `curriculum_assessment_domains` is the configurable domain
--     registry. The seeded rows are knowledge, skills, and
--     attitudes; attitudes has two sub-domains stored as rows whose
--     `parent_domain_id` points at the attitudes row (spiritual and
--     social). New domains or sub-domains are rows, never code.
--     One nesting level only (a sub-domain cannot itself be a
--     parent) is enforced by the module, not by the schema.
--   - `curriculum_grade_descriptors` carries descriptor text per
--     score band, optionally scoped to one assessment domain
--     (`domain_id` NULL means the descriptor applies to every
--     domain). The grade-band labels themselves (A/B/C/D/E) live in
--     `curriculum_settings` as configurable defaults.
--   - Status vocabularies (lesson-plan, report-card, intervention)
--     and the intervention kinds (remedial / enrichment) are NOT
--     enums in this schema: they are plain VARCHAR values validated
--     by the module against the settings lists in
--     `curriculum_settings`, which the admin edits through the API.
--   - `curriculum_lesson_plans.objectives`, `.materials`, and
--     `.activities` are TEXT arrays: a lesson plan keeps structured
--     lists, not one free-text blob.
--   - `curriculum_report_cards` has a uniqueness rule per student,
--     framework, level, grade year, semester, and academic year:
--     one report card per student per semester. Issued cards are
--     locked by the module (entries cannot change once issued).
--   - `curriculum_report_card_entries` is unique per report card,
--     subject, and assessment domain: one line per subject/domain.
--   - `curriculum_settings` is a key/value store (JSONB values) for
--     the module settings: default framework code, default paper
--     size, paper sizes, semesters, intervention kinds, status
--     vocabularies, and default grade bands.
--   - Paper sizes are standard constants (ISO A-series, Letter,
--     Legal) seeded as configurable defaults; the admin can add
--     custom sizes through the settings endpoint.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One curriculum framework (the registry).
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_frameworks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code VARCHAR(64) NOT NULL UNIQUE,
    name VARCHAR(200) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    country VARCHAR(200) NOT NULL DEFAULT '',
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    is_built_in BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- One school level (elementary, junior-high, senior-high, ...).
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_levels (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    framework_id UUID NOT NULL REFERENCES curriculum_frameworks (id) ON DELETE CASCADE,
    code VARCHAR(64) NOT NULL,
    name VARCHAR(200) NOT NULL,
    grade_count INTEGER NOT NULL DEFAULT 0 CHECK (grade_count >= 0),
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (framework_id, code)
);

-- ---------------------------------------------------------------------------
-- One subject taught under a framework at a level.
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_subjects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    framework_id UUID NOT NULL REFERENCES curriculum_frameworks (id) ON DELETE CASCADE,
    level_id UUID NOT NULL REFERENCES curriculum_levels (id) ON DELETE CASCADE,
    code VARCHAR(64) NOT NULL,
    name VARCHAR(200) NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (framework_id, level_id, code)
);

-- ---------------------------------------------------------------------------
-- One core competency (top of the competency hierarchy).
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_core_competencies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    framework_id UUID NOT NULL REFERENCES curriculum_frameworks (id) ON DELETE CASCADE,
    subject_id UUID NOT NULL REFERENCES curriculum_subjects (id) ON DELETE CASCADE,
    level_id UUID NOT NULL REFERENCES curriculum_levels (id) ON DELETE CASCADE,
    code VARCHAR(64) NOT NULL,
    title VARCHAR(300) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    grade_year INTEGER NOT NULL DEFAULT 1 CHECK (grade_year >= 1),
    version INTEGER NOT NULL DEFAULT 2013 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (subject_id, code, version)
);

-- ---------------------------------------------------------------------------
-- One basic competency (child of one core competency).
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_basic_competencies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    framework_id UUID NOT NULL REFERENCES curriculum_frameworks (id) ON DELETE CASCADE,
    core_competency_id UUID NOT NULL REFERENCES curriculum_core_competencies (id) ON DELETE CASCADE,
    code VARCHAR(64) NOT NULL,
    title VARCHAR(300) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    grade_year INTEGER NOT NULL DEFAULT 1 CHECK (grade_year >= 1),
    version INTEGER NOT NULL DEFAULT 2013 CHECK (version >= 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (core_competency_id, code, version)
);

-- ---------------------------------------------------------------------------
-- Curriculum structure: subjects per level and grade year, with
-- weekly hours allocation.
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_structures (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    framework_id UUID NOT NULL REFERENCES curriculum_frameworks (id) ON DELETE CASCADE,
    level_id UUID NOT NULL REFERENCES curriculum_levels (id) ON DELETE CASCADE,
    grade_year INTEGER NOT NULL DEFAULT 1 CHECK (grade_year >= 1),
    subject_id UUID NOT NULL REFERENCES curriculum_subjects (id) ON DELETE CASCADE,
    hours_per_week NUMERIC(5, 2) NOT NULL DEFAULT 0 CHECK (hours_per_week >= 0 AND hours_per_week <= 168),
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (framework_id, level_id, grade_year, subject_id)
);

-- ---------------------------------------------------------------------------
-- One assessment domain from the configurable registry. Sub-domains
-- point at their parent domain through parent_domain_id.
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_assessment_domains (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code VARCHAR(64) NOT NULL UNIQUE,
    name VARCHAR(200) NOT NULL,
    parent_domain_id UUID REFERENCES curriculum_assessment_domains (id) ON DELETE CASCADE,
    description TEXT NOT NULL DEFAULT '',
    sort_order INTEGER NOT NULL DEFAULT 0,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (parent_domain_id IS NULL OR parent_domain_id <> id)
);

-- ---------------------------------------------------------------------------
-- Descriptor text for one grade band, optionally scoped to one
-- assessment domain (NULL domain_id = applies to every domain).
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_grade_descriptors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    framework_id UUID NOT NULL REFERENCES curriculum_frameworks (id) ON DELETE CASCADE,
    domain_id UUID REFERENCES curriculum_assessment_domains (id) ON DELETE CASCADE,
    min_score NUMERIC(5, 2) NOT NULL CHECK (min_score >= 0 AND min_score <= 100),
    max_score NUMERIC(5, 2) NOT NULL CHECK (max_score >= 0 AND max_score <= 100),
    grade_label VARCHAR(64) NOT NULL,
    descriptor TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (min_score <= max_score)
);

-- ---------------------------------------------------------------------------
-- One structured lesson plan, tied to competencies.
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_lesson_plans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    framework_id UUID NOT NULL REFERENCES curriculum_frameworks (id) ON DELETE CASCADE,
    subject_id UUID NOT NULL REFERENCES curriculum_subjects (id) ON DELETE CASCADE,
    core_competency_id UUID REFERENCES curriculum_core_competencies (id) ON DELETE SET NULL,
    basic_competency_id UUID REFERENCES curriculum_basic_competencies (id) ON DELETE SET NULL,
    title VARCHAR(300) NOT NULL,
    objectives TEXT[] NOT NULL DEFAULT '{}',
    materials TEXT[] NOT NULL DEFAULT '{}',
    activities TEXT[] NOT NULL DEFAULT '{}',
    assessment_notes TEXT NOT NULL DEFAULT '',
    time_minutes INTEGER NOT NULL DEFAULT 0 CHECK (time_minutes >= 0),
    grade_year INTEGER NOT NULL DEFAULT 1 CHECK (grade_year >= 1),
    semester INTEGER NOT NULL DEFAULT 1 CHECK (semester >= 1),
    teacher_id VARCHAR(128) NOT NULL,
    status VARCHAR(64) NOT NULL DEFAULT 'draft',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- One per-semester report card for one student.
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_report_cards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    student_ref VARCHAR(128) NOT NULL,
    framework_id UUID NOT NULL REFERENCES curriculum_frameworks (id) ON DELETE CASCADE,
    level_id UUID NOT NULL REFERENCES curriculum_levels (id) ON DELETE CASCADE,
    grade_year INTEGER NOT NULL DEFAULT 1 CHECK (grade_year >= 1),
    semester INTEGER NOT NULL DEFAULT 1 CHECK (semester >= 1),
    academic_year VARCHAR(64) NOT NULL,
    status VARCHAR(64) NOT NULL DEFAULT 'draft',
    issued_at TIMESTAMPTZ,
    issued_by VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (student_ref, framework_id, level_id, grade_year, semester, academic_year)
);

CREATE INDEX curriculum_report_cards_student_ref_idx ON curriculum_report_cards (student_ref);

-- ---------------------------------------------------------------------------
-- One subject/domain line on a report card.
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_report_card_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    report_card_id UUID NOT NULL REFERENCES curriculum_report_cards (id) ON DELETE CASCADE,
    subject_id UUID NOT NULL REFERENCES curriculum_subjects (id) ON DELETE CASCADE,
    domain_id UUID NOT NULL REFERENCES curriculum_assessment_domains (id) ON DELETE CASCADE,
    score NUMERIC(5, 2) CHECK (score IS NULL OR (score >= 0 AND score <= 100)),
    grade_label VARCHAR(64),
    descriptor TEXT,
    notes TEXT NOT NULL DEFAULT '',
    academic_grade_id VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (report_card_id, subject_id, domain_id)
);

-- ---------------------------------------------------------------------------
-- One remedial or enrichment intervention for one student and one
-- basic competency.
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_interventions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    student_ref VARCHAR(128) NOT NULL,
    basic_competency_id UUID NOT NULL REFERENCES curriculum_basic_competencies (id) ON DELETE CASCADE,
    kind VARCHAR(64) NOT NULL,
    status VARCHAR(64) NOT NULL DEFAULT 'scheduled',
    teacher_id VARCHAR(128) NOT NULL,
    scheduled_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    notes TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX curriculum_interventions_student_ref_idx ON curriculum_interventions (student_ref);

-- ---------------------------------------------------------------------------
-- Module settings (key/value, JSONB values).
-- ---------------------------------------------------------------------------
CREATE TABLE curriculum_settings (
    key TEXT PRIMARY KEY,
    value JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Seed data: the "curriculum 2013" framework, its three school
-- levels, the assessment-domain registry (knowledge, skills,
-- attitudes with spiritual and social sub-domains), and the
-- configurable-default settings rows.
-- ---------------------------------------------------------------------------
INSERT INTO curriculum_frameworks (code, name, description, country, is_active, is_built_in)
VALUES (
    'curriculum-2013',
    'Curriculum 2013',
    'National competency-based curriculum framework (2013): core and basic competencies, three-domain assessment, and per-semester report cards.',
    'Indonesia',
    TRUE,
    TRUE
);

INSERT INTO curriculum_levels (framework_id, code, name, grade_count, sort_order)
SELECT id, 'elementary', 'Elementary School', 6, 1 FROM curriculum_frameworks WHERE code = 'curriculum-2013'
UNION ALL
SELECT id, 'junior-high', 'Junior High School', 3, 2 FROM curriculum_frameworks WHERE code = 'curriculum-2013'
UNION ALL
SELECT id, 'senior-high', 'Senior High School', 3, 3 FROM curriculum_frameworks WHERE code = 'curriculum-2013';

INSERT INTO curriculum_assessment_domains (code, name, parent_domain_id, description, sort_order, is_active)
VALUES
    ('knowledge', 'Knowledge', NULL, 'Assessment of knowledge mastery.', 1, TRUE),
    ('skills', 'Skills', NULL, 'Assessment of practical skills.', 2, TRUE),
    ('attitudes', 'Attitudes', NULL, 'Assessment of attitudes, split into spiritual and social sub-domains.', 3, TRUE);

INSERT INTO curriculum_assessment_domains (code, name, parent_domain_id, description, sort_order, is_active)
SELECT 'spiritual', 'Spiritual Attitude', id, 'Attitude domain: spiritual behaviour and values.', 1, TRUE
FROM curriculum_assessment_domains WHERE code = 'attitudes'
UNION ALL
SELECT 'social', 'Social Attitude', id, 'Attitude domain: social behaviour and interaction.', 2, TRUE
FROM curriculum_assessment_domains WHERE code = 'attitudes';

INSERT INTO curriculum_settings (key, value) VALUES
    ('default_framework_code', '"curriculum-2013"'),
    ('default_paper_size', '"a4"'),
    ('paper_sizes', '["a4", "letter", "legal"]'),
    ('semesters', '[1, 2]'),
    ('intervention_kinds', '["remedial", "enrichment"]'),
    ('lesson_plan_statuses', '["draft", "published", "archived"]'),
    ('report_card_statuses', '["draft", "issued", "archived"]'),
    ('intervention_statuses', '["scheduled", "in_progress", "completed", "cancelled"]'),
    ('default_grade_bands', '[{"min_score": 90, "max_score": 100, "label": "A"}, {"min_score": 80, "max_score": 89.99, "label": "B"}, {"min_score": 70, "max_score": 79.99, "label": "C"}, {"min_score": 60, "max_score": 69.99, "label": "D"}, {"min_score": 0, "max_score": 59.99, "label": "E"}]');
