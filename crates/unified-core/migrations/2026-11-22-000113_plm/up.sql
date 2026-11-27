-- PLM module (`src/modules/plm/`): data-driven lifecycle
-- stages, product versions with major.minor.patch revisions,
-- engineering change orders with a status machine and
-- data-driven approval steps, versioned BOM snapshots, and a
-- full audit trail. PostgreSQL only. Run with
-- diesel_migrations (embedded via crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000111_rental: the
-- in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz. `user_id`
--     / `created_by` / `requested_by` / `approver_id` /
--     `actor_id` are plain TEXT with NO foreign key on purpose,
--     matching the rental convention: users live in the users
--     module and this module must not assume its table or
--     migration order. `product_id`, `bom_id`, and
--     `document_ids` are likewise references by id only, with NO
--     foreign key: products live in the products module, BOMs in
--     the manufacturing module, and documents in the documents
--     module.
--   - `tenant_id` is plain TEXT on every row: PLM data is
--     tenant business data and every query filters by it. A row
--     from another tenant reads as 404, never 403.
--   - Lifecycle stages are data, not code: a stage row names its
--     own `slug`, `sort_order`, `allowed_transitions` (target
--     stage slugs), and `required_approvals` (how many approved
--     version approvals must exist before a version may enter
--     the stage). The default set (concept, design, prototype,
--     production, phase-out) is seeded by the module through
--     `POST /stages/seed`, not by this migration, so an admin
--     can change it without touching the schema.
--   - ECO types are data, not code: they live in the
--     `plm_settings.eco_types` JSON array per tenant, and ECO
--     creation validates against the stored list.
--   - Approval steps are data, not code: they are stored as a
--     JSON array on the ECO (`approval_steps`), each step naming
--     its own `approver_role` and whether it is required.
--   - There is no floating point money in this module: PLM
--     carries no money fields. An ECO records cost impact as a
--     free-text assessment plus an impact level only.
--   - Unique indexes enforce the module invariants in the
--     database too: one stage slug per tenant; one revision
--     per (tenant, product); one ECO code per tenant; one
--     approval decision per (ECO, step); one BOM version number
--     per (tenant, BOM). Strictly increasing BOM version
--     numbers are enforced by the module, not by a database
--     constraint, because "greater than the current maximum"
--     cannot be expressed as a unique index.
--   - `plm_ecos.status` is `draft`, `review`, `approved`,
--     `implemented`, `closed`, or `rejected`;
--     `plm_ecos.impact_level` is `low`, `medium`, `high`, or
--     `critical`; `plm_approvals.decision` is `pending`,
--     `approved`, or `rejected`; `plm_approvals.entity_type`
--     is `eco` or `version`.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Stages: one lifecycle stage definition per row. `allowed_transitions`
-- is the list of stage slugs a version may move to from here;
-- `required_approvals` is how many approved version approvals
-- must exist before a version may enter this stage.
CREATE TABLE IF NOT EXISTS plm_stages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    slug TEXT NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 128),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    sort_order BIGINT NOT NULL DEFAULT 0,
    is_initial BOOLEAN NOT NULL DEFAULT FALSE,
    is_terminal BOOLEAN NOT NULL DEFAULT FALSE,
    allowed_transitions TEXT[] NOT NULL DEFAULT '{}',
    required_approvals BIGINT NOT NULL DEFAULT 0 CHECK (required_approvals >= 0),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_plm_stages_tenant_slug ON plm_stages (tenant_id, slug);
CREATE INDEX IF NOT EXISTS idx_plm_stages_tenant_order ON plm_stages (tenant_id, sort_order);

-- Versions: one product revision. `product_id` references a
-- products-module product by id only. The revision is three
-- integers rendered as `major.minor.patch`; `status` is the
-- slug of the lifecycle stage this version is in (data, not an
-- enum). `supersedes_version_id` links to the previous revision
-- of the same product. `product_deleted` is set by the
-- `product.deleted` listener so a deleted product's history
-- stays visible.
CREATE TABLE IF NOT EXISTS plm_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    product_id TEXT NOT NULL CHECK (char_length(product_id) BETWEEN 1 AND 200),
    major BIGINT NOT NULL CHECK (major >= 0),
    minor BIGINT NOT NULL CHECK (minor >= 0),
    patch BIGINT NOT NULL CHECK (patch >= 0),
    change_summary TEXT NOT NULL CHECK (char_length(change_summary) BETWEEN 1 AND 5000),
    status TEXT NOT NULL CHECK (char_length(status) BETWEEN 1 AND 128),
    effective_from TIMESTAMPTZ,
    effective_to TIMESTAMPTZ,
    supersedes_version_id UUID,
    is_current BOOLEAN NOT NULL DEFAULT FALSE,
    product_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chk_plm_versions_effective_range CHECK (effective_to IS NULL OR effective_from IS NULL OR effective_to > effective_from)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_plm_versions_tenant_product_revision ON plm_versions (tenant_id, product_id, major, minor, patch);
CREATE INDEX IF NOT EXISTS idx_plm_versions_tenant_product ON plm_versions (tenant_id, product_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_plm_versions_tenant_status ON plm_versions (tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_plm_versions_supersedes ON plm_versions (supersedes_version_id) WHERE supersedes_version_id IS NOT NULL;

-- ECOs: one engineering change order. `code` is unique per
-- tenant (for example `ECO-0001`). `eco_type` is a value from
-- the tenant's settings list. `affected_product_ids` and
-- `linked_document_ids` are references by id only;
-- `affected_version_ids` references PLM versions by id.
-- `approval_steps` is the ordered, data-driven approval
-- workflow for this ECO, stored as JSON.
CREATE TABLE IF NOT EXISTS plm_ecos (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 64),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    eco_type TEXT NOT NULL CHECK (char_length(eco_type) BETWEEN 1 AND 128),
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'review', 'approved', 'implemented', 'closed', 'rejected')),
    affected_product_ids TEXT[] NOT NULL DEFAULT '{}',
    affected_version_ids UUID[] NOT NULL DEFAULT '{}',
    linked_document_ids TEXT[] NOT NULL DEFAULT '{}',
    impact_level TEXT CHECK (impact_level IS NULL OR impact_level IN ('low', 'medium', 'high', 'critical')),
    impact_assessment TEXT CHECK (impact_assessment IS NULL OR char_length(impact_assessment) BETWEEN 1 AND 5000),
    approval_steps JSONB NOT NULL DEFAULT '[]'::jsonb,
    requested_by TEXT NOT NULL CHECK (char_length(requested_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_plm_ecos_tenant_code ON plm_ecos (tenant_id, code);
CREATE INDEX IF NOT EXISTS idx_plm_ecos_tenant_status ON plm_ecos (tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_plm_ecos_tenant_type ON plm_ecos (tenant_id, eco_type, created_at DESC);

-- Approvals: one decision record per row. `entity_type` is
-- `eco` (the decision is for step `step_index` of that ECO's
-- `approval_steps`) or `version` (a stage-entry approval;
-- `step_index` is 0 and the record counts towards the target
-- stage's `required_approvals` gate). `decision` is `pending`,
-- `approved`, or `rejected`.
CREATE TABLE IF NOT EXISTS plm_approvals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    entity_type TEXT NOT NULL CHECK (entity_type IN ('eco', 'version')),
    entity_id UUID NOT NULL,
    step_index BIGINT NOT NULL DEFAULT 0 CHECK (step_index >= 0),
    step_name TEXT NOT NULL CHECK (char_length(step_name) BETWEEN 1 AND 200),
    approver_role TEXT NOT NULL CHECK (char_length(approver_role) BETWEEN 1 AND 128),
    approver_id TEXT,
    decision TEXT NOT NULL DEFAULT 'pending' CHECK (decision IN ('pending', 'approved', 'rejected')),
    comment TEXT CHECK (comment IS NULL OR char_length(comment) BETWEEN 1 AND 2000),
    decided_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chk_plm_approvals_decided_fields CHECK ((decision = 'pending' AND decided_at IS NULL) OR (decision <> 'pending' AND decided_at IS NOT NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_plm_approvals_eco_step ON plm_approvals (entity_id, step_index) WHERE entity_type = 'eco';
CREATE INDEX IF NOT EXISTS idx_plm_approvals_entity ON plm_approvals (tenant_id, entity_type, entity_id, created_at ASC);

-- BOM versions: one snapshot per row. `bom_id` references a
-- manufacturing BOM by id only; `version_number` is strictly
-- increasing per BOM. `eco_id` links the snapshot to the ECO
-- that drove the change, when there is one. `snapshot` is the
-- component/line snapshot as stored at versioning time.
CREATE TABLE IF NOT EXISTS plm_bom_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    bom_id TEXT NOT NULL CHECK (char_length(bom_id) BETWEEN 1 AND 200),
    version_number BIGINT NOT NULL CHECK (version_number > 0),
    eco_id UUID,
    snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    effective_from TIMESTAMPTZ,
    notes TEXT CHECK (notes IS NULL OR char_length(notes) BETWEEN 1 AND 5000),
    created_by TEXT NOT NULL CHECK (char_length(created_by) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_plm_bom_versions_tenant_bom_number ON plm_bom_versions (tenant_id, bom_id, version_number);
CREATE INDEX IF NOT EXISTS idx_plm_bom_versions_eco ON plm_bom_versions (eco_id) WHERE eco_id IS NOT NULL;

-- Audit trail: one row per lifecycle event (stage change,
-- version creation, ECO status change, approval decision, BOM
-- versioning). `from_value` / `to_value` carry the old and new
-- status, decision, or version number.
CREATE TABLE IF NOT EXISTS plm_audit_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    entity_type TEXT NOT NULL CHECK (char_length(entity_type) BETWEEN 1 AND 128),
    entity_id UUID NOT NULL,
    action TEXT NOT NULL CHECK (char_length(action) BETWEEN 1 AND 128),
    actor_id TEXT NOT NULL CHECK (char_length(actor_id) BETWEEN 1 AND 200),
    comment TEXT CHECK (comment IS NULL OR char_length(comment) BETWEEN 1 AND 2000),
    from_value TEXT,
    to_value TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_plm_audit_tenant_entity ON plm_audit_entries (tenant_id, entity_type, entity_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_plm_audit_tenant_action ON plm_audit_entries (tenant_id, action, created_at DESC);

-- Per-tenant PLM settings. `eco_types` is the data-driven list
-- ECO creation validates against; `require_eco_for_bom` makes
-- an ECO link mandatory on every BOM version when true.
CREATE TABLE IF NOT EXISTS plm_settings (
    tenant_id TEXT PRIMARY KEY CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    eco_types JSONB NOT NULL DEFAULT '["design_change", "material_change", "process_change", "cost_reduction", "compliance", "supplier_change"]'::jsonb,
    require_eco_for_bom BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
