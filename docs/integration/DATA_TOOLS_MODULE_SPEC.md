# Data Tools — core module spec (DRAFT, user-proposed 2026-11-23)

Status: DRAFT — written as the user's requested note. NOT dispatched.
No agent has been sent; the build task below awaits the user's "gas".

## Why (verdict)
- Worth it: orphaned/expired data grows silently (DB size, slow queries,
  bigger backups); one central module beats scattered per-module purge
  endpoints; cheap to build because the hooks already exist (scheduler
  `cleanup` kind + `cleanup.requested` event, purge-expired pattern,
  token pruning, spool orphan sweep) — Data Tools mostly wires them
  together + admin page + resolver.
- ERP standard: every mature ERP has data lifecycle/housekeeping — Odoo
  runs it via ir.cron scheduled actions (e.g. attachment garbage
  collection) plus a Data Cleaning app (dedup/merge); SAP has archiving
  + ILM (Information Lifecycle Management). Unified's edge: a dedicated
  visible core module with dry-run/preview admin page + hookable cleanup
  (modules answer `cleanup.requested`), and a resolver that reconciles
  DB rows against stored objects (most systems never cross-check the
  two).

## What it owns
- Orphaned data resolver (find + resolve orphaned rows/objects).
- Expired-data purging (shares, sessions, tokens, logs past retention).
- Cleanup cron jobs (definitions + schedules).
- Other data maintenance (retention rules as data, run history).
- Admin page: rules, scan preview (dry-run), run, history.

## What it does NOT own
- Scheduler execution (the Scheduler module owns WHEN).
- The data itself (each module owns its tables; Data Tools reaches them
  via hook, never by reaching into another module's tables directly).
- Backups (Backup Manager owns them).
- Postgres VACUUM / ANALYZE (DBA-level; may be exposed as an action
  later, not in v1).

## Ownership split (Data Tools vs Scheduler)
- Data Tools: WHAT gets cleaned + the rules (retention as data) +
  the admin page + the run records.
- Scheduler: WHEN (the `cleanup` kind runs on schedule; emits
  `cleanup.requested`).
- Modules: HOW for their own tables (answer `cleanup.requested` /
  `data_tools.cleanup.<target>` with counts + deletes).

## Orphan / expiry targets (v1)
- `document_shares`: expired beyond grace / revoked beyond grace.
- `document_upload_sessions`: expired sessions + spool dirs with no live
  session (the spool orphan sweep already exists as a test).
- `auth_revoked_tokens`: pruned on init today (C13); Data Tools owns the
  scheduled pass.
- `document_downloads`: log rows older than retention.
- Spool/temp files: temps older than 24h (already recommended).
- Files on disk with no DB row / DB rows with no file (the resolver's
  cross-check — the part most systems never do).

## Rules as data (modularity rule)
- `cleanup_rules` table: target, retention_days, enabled,
  dry_run_default, scope (tenant/platform).
- No hard-coded retention in code; defaults are configurable defaults.

## Runs + audit
- `cleanup_runs` table: id, started_at, finished_at, dry_run,
  per-target removed counts, errors.
- Every delete is counted first (preview) and logged; audit events for
  runs. Audit events (shared/revoked/expired/accessed) are never deleted
  by the cleaner — only the share row is.

## Admin page
- Rules table (tune retention, enable/disable per target).
- "Scan now" → dry-run preview (counts per target, zero deletes).
- "Run cleanup" → executes (dry_run flag), shows removed counts.
- Run history.

## Endpoints (proposed)
- `GET /api/v1/data-tools/rules`
- `PATCH /api/v1/data-tools/rules/{id}`
- `POST /api/v1/data-tools/scan` (dry-run preview)
- `POST /api/v1/data-tools/run`
- `GET /api/v1/data-tools/runs`

## Tenant scoping
- Runs execute inside the tenant schema (`SET LOCAL search_path`) for
  tenant tables; platform tables under control-plane scope.
- Tenant admins tune their own retention; platform defaults apply
  otherwise.

## Build task (what would be forwarded — AWAITING USER GO)
- Task: build core module `data_tools` — migration for `cleanup_rules`
  + `cleanup_runs`, resolver for the v1 targets above, admin page +
  scan/run endpoints, scheduler wiring via `cleanup.requested`, tests,
  verification per the repo's standard checks (rustfmt parse; no
  root-bin `cargo check`, OOM wall — state plainly).
- Tracker: create the task first, close with `status_id: 4` only after
  verification, verified by fresh read.
- No push. No new dependencies. All English.
- NOT dispatched — awaiting the user's confirmation ("gas").
