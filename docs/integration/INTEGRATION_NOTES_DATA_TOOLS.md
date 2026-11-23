# Integration notes — Data Tools core module

Status: IMPLEMENTED (v1) — 2026-11-23. No push by this worker (GitHub
connector is read-only; the coordinator pushes).

## What was built

Core module `data_tools` (`src/modules/data_tools/` + root
`src/modules/data_tools.rs`, registered in `src/modules/mod.rs`):

- `types.rs` — `CleanupRule`, `CleanupRun`, `TargetOutcome`,
  `ScanReport`, `DocumentShareRecord`, `RunRequest`, `RulePatch`,
  the six v1 `TARGETS` with their configurable defaults, and the
  `data_tools:read` / `data_tools:admin` capabilities.
- `state.rs` — the resolver. In-memory DashMap stores are the runtime
  authority (same convention as the other batch modules); the tables
  in migration 000125 are the persistence targets for the C11
  write-through. One cleanup runs at a time (`try_lock` — a second
  concurrent run gets 409, never a deadlock).
- `events.rs` — the hook contract + the `cleanup.requested` handler.
- `handlers.rs` — the five JSON endpoints + the HTML admin page.
- `validation.rs` — module-local auth helpers (caller + `role.check`,
  fail closed), pagination, HTML escaping.
- `module.rs` — `DataToolsModule`, routes, `register_hooks`.
- `ai_capabilities.rs` — `explain_cleanup_report`,
  `suggest_retention` (non-PII).

Migration `crates/unified-core/migrations/2026-11-23-000125_data_tools/`:

- `cleanup_rules` — target (unique), retention_days (0..=3650),
  enabled, dry_run_default, scope (`tenant`/`platform`), seeded
  with one default rule per v1 target.
- `cleanup_runs` — id, started_at, finished_at, dry_run, status
  (`running`/`completed`/`failed`), per-target counts (JSONB),
  errors (JSONB), triggered_by (`manual`/`scheduler`/`api`).
- `document_shares` — see the decision below.

Endpoints (`/api/v1/data-tools`, all authenticated, `data_tools:read`
for reads, `data_tools:admin` for changes/scans/runs/admin page):

- `GET /rules` — list retention rules (paginated).
- `PATCH /rules/{id}` — tune `retention_days` (0..=3650), `enabled`,
  `dry_run_default`. `target` and `scope` are immutable here.
- `POST /scan` — dry-run preview: counts per target, ZERO deletes.
- `POST /run` — execute a cleanup. Body `{"dry_run": true|false}`
  is optional: omitted means each target uses its rule's
  `dry_run_default`; present overrides every rule for that run.
  `cross_check` stays dry-run regardless.
- `GET /runs` — run history, newest first (paginated).
- `GET /admin` — HTML admin page (rules table + scan now + run +
  run history), served by this module.

## The `document_shares` decision

The `document_shares` table does NOT exist yet (sharing was never
built; signed URLs are stateless HMAC today). Decision taken:
**create the minimal table inside migration 000125** — `id`,
`document_id` (FK to `documents`, cascade), `scope`
(`owner`/`company`/`module`/`public`), `expires_at` (nullable),
`revoked`, `revoked_at`, `created_by`, `created_at` — so the
resolver is real instead of a stub that always reports zero.

Consequences, recorded honestly:

- Data Tools owns the table AND an in-memory share map in v1 (the
  runtime authority, same convention as the other modules). There is
  no sharing UI yet, so the map starts empty and the target reports
  zero until shares are recorded.
- When the Documents module grows real sharing, it takes over this
  table (and answers `data_tools.cleanup.document_shares`); Data
  Tools then drops its share map and keeps only the rule + run
  records. No Data Tools code change is needed beyond that removal.
- The cleaner deletes only the share row — never the document, never
  an audit event. A share is purgeable when it expired beyond the
  rule's grace period, or was revoked beyond grace. A revoked share
  with no `revoked_at` is purgeable immediately: revocation is the
  operator's explicit decision and the row grants nothing any more.

## Ownership boundary (hard, as specified)

- Data Tools reaches other modules' tables ONLY via the
  `data_tools.cleanup.<target>` hook. It never reads or deletes
  another module's rows directly.
- Hook targets and their owners (v1: no owner answers yet, so they
  report zero with a note — the resolver does not crash):
  - `document_upload_sessions` — Documents module (expired sessions
    + spool dirs with no live session; the spool orphan sweep already
    exists as a test in `documents/state.rs`).
  - `auth_revoked_tokens` — Auth module (scheduled pass; pruned on
    init today).
  - `document_downloads` — Documents module (log rows past retention).
  - `spool_temps` — Documents module (temps older than 24h).
  - `cross_check` — Documents module (files on disk with no DB row /
    DB rows with no file). Report-only: always asked with
    `dry_run: true`, never deletes.
- Hook answer contract (one result per answering handler):
  `{"target", "counted", "removed", "errors": [], "note"}`.
  `dry_run: true` in the request means count only — zero deletes.
  Malformed answers are recorded as errors, never a panic.
- Audit events are NEVER deleted by the cleaner. Every run emits
  `data_tools.run.started` / `data_tools.run.completed`; the
  `cleanup_runs` row is the audit record for the run.

## Scheduler wiring

- The `cleanup` job kind already exists in the Scheduler: dispatch
  prunes finished queue items older than 7 days, then emits
  `cleanup.requested`.
- Data Tools subscribes to `cleanup.requested` (`register_hooks`) and
  runs a cleanup recorded as `triggered_by: scheduler`. A `dry_run`
  flag in the payload is honored; without one, each rule's
  `dry_run_default` applies.
- The Scheduler owns WHEN. An admin creates/enables a `cleanup`
  scheduler job (no job is seeded by this module — seeding scheduler
  jobs is the Scheduler's business, not Data Tools').

## Admin page — two parts

1. Live now: `GET /api/v1/data-tools/admin` serves a self-contained
   HTML page from this module (rules table server-rendered and
   escaped; actions call the JSON endpoints; dynamic values are
   inserted with `textContent`, never `innerHTML`).
2. Follow-up (dashboards owner): wire `/admin/data-tools` into the
   dashboards admin sections — add `"data-tools"` to the section
   allowlist + capability map + content-template match in
   `src/modules/dashboards/handlers_admin.rs` and to `section_flags`
   in `src/modules/dashboards/service.rs`. The Handlebars content
   template ships at
   `templates/default/data-tools/admin-data-tools.hbs`. This worker
   did NOT touch the dashboards files (outside its boundary).

## Follow-up (not in v1, no stubs left)

- Audit module: add `data_tools.run.started` /
  `data_tools.run.completed` (and `data_tools.scan.completed`) to
  `AUDITED_EVENTS` in `src/modules/audit/constants.rs` so runs land
  in the hash-chained audit log. (Touching the audit module was
  outside this worker's boundary.)
- Owning modules answer their `data_tools.cleanup.<target>` hooks
  (Documents: sessions/downloads/spool/cross-check; Auth: revoked
  tokens). Until then those targets honestly report zero + note.
- Per-tenant rules: the `scope` column exists; v1 ships platform
  rules only and the endpoints do not scope by tenant (same as the
  Scheduler's jobs). Tenant admins tuning their own retention needs
  the tenant-scoping pass.
- C11 write-through: rules/runs/shares do not survive a restart yet
  (same honest limitation as the other batch modules — see the
  storage notes for `finalize_jobs`).
- DB migration run: `000125` was added as embedded Diesel migrations
  (no registry edit needed); it has not been applied to a live
  database from this VM.

## Verification

- `rustfmt` parse check on every changed/added `.rs` file (no
  tree-wide reformat — pre-existing fmt debt untouched).
- Tests (in-module `#[cfg(test)]`, root-bin tests are unrun on this
  VM — stated plainly per the task): scan dry-run returns counts
  with zero deletes; run deletes + counts; rules CRUD (list defaults,
  patch, bad retention / unknown id rejected); a run is recorded and
  its `data_tools.run.completed` event is emitted; scheduler
  `cleanup.requested` records a run with `triggered_by: scheduler`;
  hook targets with no owner report zero with a note; a stub owner
  aggregates counts; `cross_check` is always asked dry-run; revoked
  shares without a timestamp are purgeable; a second concurrent run
  is refused with 409; auth fails closed without a verified caller
  and without a `role.check` answer.
- No new dependencies. All code and docs in English.
