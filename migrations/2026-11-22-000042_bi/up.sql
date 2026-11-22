-- Business Intelligence + AI Insights tables for the Unified Pentagonal Project
-- (`src/modules/bi.rs`): saved reports, scheduled deliveries, and KPI
-- dashboards. Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/bi.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `bi_reports.company_id` and `bi_reports.owner_id` are plain
--     VARCHAR references (the module identifies callers by the
--     `X-User-Id` header and keeps user ids as strings, exactly like
--     `store_orders.user_id` in migration 000002). No foreign key
--     to `users` is stored here.
--   - `bi_reports.source` is one of the closed allowlist in the
--     module: billing, store, tickets, licensing, crm, projects,
--     timetracker, subscriptions. `chart_type` is table, bar, line,
--     or pie. `visibility` is private (owner only) or shared
--     (whole company).
--   - `bi_reports.fields` is a JSONB array of selected field names
--     (validated against the per-source allowlist in the module);
--     `bi_reports.filters` is a JSONB object of exact-match
--     field/value pairs plus optional `from` / `to` RFC 3339 bounds
--     on `occurred_at`. `group_by` is one field name or NULL.
--   - `bi_report_schedules.schedule` is an interval in seconds or
--     five cron-like fields (validated in the module, same shape as
--     the scheduler module). `recipients` is a JSONB array of user
--     ids; `channel` is email or notification. `scheduler_status`
--     is queued or declined: `bi.report.deliver` is not in the
--     scheduler's KNOWN_KINDS yet, so today a schedule records
--     `declined` instead of pretending it was queued.
--   - `bi_kpi_dashboards.kpis` is a JSONB array of KPI definitions
--     (`metric`, `source`, `period`). Metrics are revenue_total,
--     order_count, ticket_count, active_licenses, tasks_completed,
--     payment_count, or average_order_value; periods are 7d, 30d,
--     90d, or all.
--   - The source-activity mirror itself (`SourceRecord` in the
--     module) has no table here on purpose: it is a live feed of
--     source-module events, rebuilt from those events on startup,
--     not durable state.

CREATE TABLE IF NOT EXISTS bi_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id VARCHAR NOT NULL,
    owner_id VARCHAR NOT NULL,
    name VARCHAR(200) NOT NULL,
    description TEXT,
    source VARCHAR(64) NOT NULL CHECK (source IN (
        'billing', 'store', 'tickets', 'licensing', 'crm', 'projects',
        'timetracker', 'subscriptions'
    )),
    fields JSONB NOT NULL DEFAULT '[]'::jsonb,
    filters JSONB NOT NULL DEFAULT '{}'::jsonb,
    group_by VARCHAR(128),
    chart_type VARCHAR(16) NOT NULL DEFAULT 'table' CHECK (chart_type IN (
        'table', 'bar', 'line', 'pie'
    )),
    visibility VARCHAR(16) NOT NULL DEFAULT 'private' CHECK (visibility IN (
        'private', 'shared'
    )),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_bi_reports_company_id ON bi_reports (company_id);
CREATE INDEX IF NOT EXISTS idx_bi_reports_owner_id ON bi_reports (owner_id);
CREATE INDEX IF NOT EXISTS idx_bi_reports_source ON bi_reports (source);
CREATE INDEX IF NOT EXISTS idx_bi_reports_created_at ON bi_reports (created_at);

CREATE TABLE IF NOT EXISTS bi_report_schedules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    report_id UUID NOT NULL REFERENCES bi_reports (id) ON DELETE CASCADE,
    company_id VARCHAR NOT NULL,
    schedule VARCHAR(64) NOT NULL,
    recipients JSONB NOT NULL DEFAULT '[]'::jsonb,
    channel VARCHAR(16) NOT NULL DEFAULT 'notification' CHECK (channel IN (
        'email', 'notification'
    )),
    scheduler_status VARCHAR(16) NOT NULL DEFAULT 'declined' CHECK (scheduler_status IN (
        'queued', 'declined'
    )),
    scheduler_job_id VARCHAR,
    last_delivered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_bi_report_schedules_report_id ON bi_report_schedules (report_id);
CREATE INDEX IF NOT EXISTS idx_bi_report_schedules_company_id ON bi_report_schedules (company_id);

CREATE TABLE IF NOT EXISTS bi_kpi_dashboards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id VARCHAR NOT NULL,
    owner_id VARCHAR NOT NULL,
    name VARCHAR(200) NOT NULL,
    description TEXT,
    kpis JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_bi_kpi_dashboards_company_id ON bi_kpi_dashboards (company_id);
CREATE INDEX IF NOT EXISTS idx_bi_kpi_dashboards_owner_id ON bi_kpi_dashboards (owner_id);
