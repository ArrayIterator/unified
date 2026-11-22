-- Quality Management module tables for the Unified project
-- (`src/modules/quality.rs`): quality points, criteria,
-- inspections, measurements, alerts, and CAPA records.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs).
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see the e-learning precedent):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `target_ref` / `inspector_id` / `owner_id` / `company_id`
--     columns are plain TEXT with NO foreign key on purpose:
--     products, work orders, shipments, suppliers, and users live
--     in other modules and this module must not assume their
--     tables or migration order.
--   - `evidence_refs` is a TEXT array of opaque storage /
--     document references; no bytes are stored here.
--   - `measurements.value_text` and `failure_causes` are stored
--     as JSONB / TEXT arrays; tolerance bands live on
--     `quality_criteria` (min/max/expected values), never in code.
--   - `quality_alerts.blocked` marks a target that must not
--     proceed until the alert is resolved; resolving the alert
--     unblocks it.
--   - `quality_capas.state` follows open -> in_progress ->
--     verified -> closed; verification notes, verifier, and
--     verification time are stored on the row.
--   - All ids are Uuid, all timestamps are Timestamptz. JSON
--     columns are JSONB.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Quality points.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS quality_points (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT NOT NULL DEFAULT '' CHECK (char_length(description) <= 5000),
    scope TEXT NOT NULL CHECK (char_length(scope) BETWEEN 1 AND 64),
    target_ref TEXT CHECK (target_ref IS NULL OR char_length(target_ref) BETWEEN 1 AND 500),
    target_kind TEXT CHECK (target_kind IS NULL OR char_length(target_kind) BETWEEN 1 AND 64),
    check_kind TEXT NOT NULL CHECK (char_length(check_kind) BETWEEN 1 AND 64),
    on_fail_policy TEXT NOT NULL DEFAULT 'alert_only' CHECK (char_length(on_fail_policy) BETWEEN 1 AND 64),
    alert_severity TEXT NOT NULL DEFAULT 'medium' CHECK (char_length(alert_severity) BETWEEN 1 AND 64),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_quality_points_scope ON quality_points (scope);
CREATE INDEX IF NOT EXISTS idx_quality_points_target ON quality_points (target_ref);
CREATE INDEX IF NOT EXISTS idx_quality_points_active ON quality_points (active);

-- ---------------------------------------------------------------------------
-- Criteria of a quality point (tolerance definitions).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS quality_criteria (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    point_id UUID NOT NULL REFERENCES quality_points (id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    kind TEXT NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    unit TEXT CHECK (unit IS NULL OR char_length(unit) BETWEEN 1 AND 64),
    min_value DOUBLE PRECISION,
    max_value DOUBLE PRECISION,
    expected_bool BOOLEAN,
    expected_text TEXT CHECK (expected_text IS NULL OR char_length(expected_text) <= 20000),
    failure_cause TEXT CHECK (failure_cause IS NULL OR char_length(failure_cause) BETWEEN 1 AND 64),
    weight DOUBLE PRECISION NOT NULL DEFAULT 1.0 CHECK (weight > 0),
    required BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (min_value IS NULL OR max_value IS NULL OR min_value <= max_value)
);
CREATE INDEX IF NOT EXISTS idx_quality_criteria_point ON quality_criteria (point_id);

-- ---------------------------------------------------------------------------
-- Inspections.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS quality_inspections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    point_id UUID NOT NULL REFERENCES quality_points (id) ON DELETE RESTRICT,
    target_ref TEXT NOT NULL CHECK (char_length(target_ref) BETWEEN 1 AND 500),
    target_kind TEXT NOT NULL CHECK (char_length(target_kind) BETWEEN 1 AND 64),
    inspector_id TEXT CHECK (inspector_id IS NULL OR char_length(inspector_id) BETWEEN 1 AND 500),
    status TEXT NOT NULL CHECK (status IN ('requested', 'completed', 'cancelled')),
    result TEXT CHECK (result IS NULL OR result IN ('pass', 'fail', 'conditional')),
    measurements JSONB NOT NULL DEFAULT '[]'::jsonb,
    failure_causes TEXT[] NOT NULL DEFAULT '{}',
    notes TEXT CHECK (notes IS NULL OR char_length(notes) <= 20000),
    evidence_refs TEXT[] NOT NULL DEFAULT '{}',
    inspected_at TIMESTAMPTZ,
    company_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_quality_inspections_point ON quality_inspections (point_id);
CREATE INDEX IF NOT EXISTS idx_quality_inspections_target ON quality_inspections (target_ref);
CREATE INDEX IF NOT EXISTS idx_quality_inspections_status ON quality_inspections (status);
CREATE INDEX IF NOT EXISTS idx_quality_inspections_result ON quality_inspections (result);
CREATE INDEX IF NOT EXISTS idx_quality_inspections_created ON quality_inspections (created_at);

-- ---------------------------------------------------------------------------
-- Quality alerts.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS quality_alerts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    inspection_id UUID NOT NULL REFERENCES quality_inspections (id) ON DELETE CASCADE,
    point_id UUID NOT NULL REFERENCES quality_points (id) ON DELETE RESTRICT,
    target_ref TEXT NOT NULL CHECK (char_length(target_ref) BETWEEN 1 AND 500),
    target_kind TEXT NOT NULL CHECK (char_length(target_kind) BETWEEN 1 AND 64),
    severity TEXT NOT NULL CHECK (char_length(severity) BETWEEN 1 AND 64),
    status TEXT NOT NULL CHECK (status IN ('open', 'acknowledged', 'resolved')),
    message TEXT NOT NULL CHECK (char_length(message) BETWEEN 1 AND 5000),
    blocked BOOLEAN NOT NULL DEFAULT FALSE,
    acknowledged_by TEXT,
    resolved_by TEXT,
    resolved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_quality_alerts_inspection ON quality_alerts (inspection_id);
CREATE INDEX IF NOT EXISTS idx_quality_alerts_target ON quality_alerts (target_ref);
CREATE INDEX IF NOT EXISTS idx_quality_alerts_status ON quality_alerts (status);
CREATE INDEX IF NOT EXISTS idx_quality_alerts_blocked ON quality_alerts (blocked) WHERE blocked = TRUE;

-- ---------------------------------------------------------------------------
-- CAPA (corrective and preventive actions).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS quality_capas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    inspection_id UUID NOT NULL REFERENCES quality_inspections (id) ON DELETE CASCADE,
    alert_id UUID REFERENCES quality_alerts (id) ON DELETE SET NULL,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT NOT NULL DEFAULT '' CHECK (char_length(description) <= 5000),
    root_cause TEXT CHECK (root_cause IS NULL OR char_length(root_cause) <= 20000),
    owner_id TEXT NOT NULL CHECK (char_length(owner_id) BETWEEN 1 AND 500),
    due_date TIMESTAMPTZ,
    state TEXT NOT NULL CHECK (state IN ('open', 'in_progress', 'verified', 'closed')),
    verification_notes TEXT CHECK (verification_notes IS NULL OR char_length(verification_notes) <= 20000),
    verified_by TEXT,
    verified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_quality_capas_inspection ON quality_capas (inspection_id);
CREATE INDEX IF NOT EXISTS idx_quality_capas_alert ON quality_capas (alert_id);
CREATE INDEX IF NOT EXISTS idx_quality_capas_state ON quality_capas (state);
CREATE INDEX IF NOT EXISTS idx_quality_capas_owner ON quality_capas (owner_id);
CREATE INDEX IF NOT EXISTS idx_quality_capas_due ON quality_capas (due_date);
