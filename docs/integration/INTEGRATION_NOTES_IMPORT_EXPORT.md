# Integration Notes — Import / Export Hardening

Module: `src/modules/import_export.rs` (rewritten in place, no rebuild).
Migration: `2026-11-22-000079_import_export_hardening` (new tables
`mapping_templates`, `export_schedules`, `import_sessions`; new
columns on `import_jobs` / `export_jobs` from `000018`).
Nothing was pushed. `cargo check` has never run in this VM (no
toolchain, crates.io blocked) — compilation is NOT verified.

## What changed in this round

- Capability checks on all 27 routes (9 existing + 18 new):
  `import_export:read` (catalog, lists, single reads, error report),
  `import_export:import` (submit, confirm, chunked sessions),
  `import_export:export` (create and download exports),
  `import_export:export_pii` (unredacted personal-data export),
  `import_export:manage` (mappings, schedules, settings). Checked
  through the `role.check` hook, fail closed. The `admin` role holds
  `*` and passes all of these; other roles need an explicit grant
  through the roles API. No role defaults were changed (roles.rs was
  not touched).
- Tenant scoping: `X-Company-Id` header with `user.company_requested`
  hook fallback (same shape as the AI module). Jobs, mappings,
  schedules, and sessions record `actor` + `company_id`. Lists are
  scoped to the caller's company unless the caller holds the manage
  capability; single-object reads of another company's record answer
  404, never 403. Import rows whose `company_id` fails validation;
  export rows tagged with another company are dropped and counted
  (`rows_excluded_by_tenant_scope`).
- PII redaction: exports reuse `crate::modules::privacy::redact_value`
  (no duplicated logic). Per-entity PII columns are declared in the
  catalog (`EntitySpec::pii_columns`). Unredacted export requires the
  PII capability and an explicit opt-out; otherwise redaction is
  forced and recorded (`pii_redacted`, `redacted_fields`), and
  `export.pii_redacted` is emitted.
- XML format added both ways (hand-rolled parser/serializer, no new
  crate): export serializes `<export><row>…</row></export>`; import
  accepts root `rows|export|items|dataset|records` with flat
  `row|item|record` children. `<!DOCTYPE` / `<!ENTITY` are rejected
  (no external entities).
- Saved field-mapping templates (`/mappings`, CRUD minus update:
  delete + recreate). `POST /imports` and chunked sessions accept
  `mapping_id`; a per-request `mapping` wins over the template.
- Chunked imports (`/import-sessions`): open a session, append
  self-contained chunks (CSV chunk repeats its header, JSON chunk is
  an array, XML chunk is a `<rows>` document), each chunk validated
  before acceptance, duplicate chunk index rejected (409), declared
  `total_chunks` enforced at completion. Completion creates an
  import job; the existing confirm flow still stages it. Nothing is
  written to owning modules at any point before confirmation.
- Scheduled exports (`/export-schedules`): stored definition + cron
  schedule (five fields, or interval seconds — same rule the
  scheduler validates with). `run-now` works today; creation/update
  offers the schedule to the scheduler via `scheduler.enqueue`
  (kind `export.run`) and records the outcome on the schedule.
- Chunked exports: `limit` / `offset` on `POST /exports`, forwarded
  to the `export.requested` hook payload; the job records `limit`,
  `offset`, and `has_more` (true when exactly `limit` rows were
  returned — a heuristic, documented on the field).
- Error reports: `GET /imports/{id}/errors/download` returns the
  row errors as CSV.
- Audit: every lifecycle event is emitted with `actor`, `actor_id`,
  `resource`, and row counts (new events: `import.created`,
  `import.confirmed`, `export.created`, `export.pii_redacted`,
  `export.schedule.*`, `mapping.created/deleted`,
  `import.session.completed`, `import_export.settings.updated`).
- Configurable defaults: `GET / PATCH /settings` (manage) for
  `max_file_bytes`, `max_rows_per_file`, `max_chunk_bytes`,
  `max_rows_per_session`, `max_export_rows`, `max_export_bytes`,
  `default_redact_pii`, `allowed_formats`. Settings are runtime-only
  (in-memory, same as the rest of this module); they are not
  persisted yet.
- New routes are declared in `route_info()`, so the API Manager
  collects them into the versioned public API registry automatically;
  no api_manager.rs change was needed.

## Integrator work (files this round did not touch)

1. `src/modules/audit.rs` — add the import/export event names to
   `AUDITED_EVENTS` so the emitted events are actually recorded:
   `"import.created"`, `"import.completed"`, `"import.confirmed"`,
   `"import.failed"`, `"import.session.completed"`,
   `"export.created"`, `"export.completed"`, `"export.requested"`,
   `"export.pii_redacted"`, `"export.schedule.created"`,
   `"export.schedule.updated"`, `"export.schedule.deleted"`,
   `"export.schedule.run"`, `"mapping.created"`,
   `"mapping.deleted"`, `"import_export.settings.updated"`.
   The payloads already carry `actor`, `actor_id`, `resource`, and
   row counts in the shape `AuditState::record` reads.
2. `src/modules/scheduler.rs` — add `"export.run"` to `KNOWN_KINDS`
   and a dispatch arm that emits `export.run_requested` with the
   scheduler payload (`export_schedule_id` / `job_id`). The
   import/export module already registers a handler for
   `export.run_requested` that runs the stored schedule, records
   `last_run_at` / `last_export_id` / `run_count` / `last_error`, and
   answers with `started` / `error`. Until the arm exists,
   `notify_scheduler` records the refusal on the schedule and
   `run-now` is the working path. Do NOT route `export.run` through
   any existing kind (e.g. `report.generate` dispatches to analytics).
3. `src/db/schema.rs` + `src/db/models.rs` — mirror migration
   `000079`: new tables `mapping_templates`, `export_schedules`,
   `import_sessions`, and the new columns on `import_jobs` /
   `export_jobs` (see `up.sql` for the exact columns; export uses
   `row_limit` / `row_offset`, not `limit` / `offset`).
4. Owning modules (`users`, `software`, `store`, `billing`, `tasks`,
   `licensing`) — still no public write/import consumer for
   `import.completed` staged rows, and still no `export.requested`
   row provider. This round did not invent those write paths; the
   wiring snippet from the earlier coordinator report still applies.
   Note for providers: `export.requested` now carries `limit`,
   `offset`, and `company_id` — providers should honour all three
   and tag rows with `company_id` when they can.
5. Privacy module (optional) — subscribe to `export.pii_redacted`
   if redaction usage should also land in the privacy audit trail;
   this module already emits the event with actor, entity, and
   redacted fields.

## Migration collisions observed during this round

- `000074` exists three times in the tree
  (`backup_drill_verify_tenant`, `pos`, plus this round's first
  attempt, which was moved). `000075` (`appointments`), `000076`
  (`elearning`), `000077` (`webinars`), `000078`
  (`cluster_sync_reliability`) were all taken while this round was
  running, so this round used `000079_import_export_hardening`.
  Pre-existing collisions still open: `000056` twice, `000068`
  twice, `000070` twice, `000100_calculator` out of sequence.

## Known open items (not fixed here)

- Persistence: stores are in-memory; the DB tables exist but no
  Diesel persistence writer exists yet (same as before this round).
  Settings are runtime-only and reset on restart.
- `has_more` on exports is a heuristic (exactly-`limit` rows
  returned); providers that ignore `limit`/`offset` make it
  approximate. Rows from providers without a `company_id` tag are
  kept (hook payload carries the company so providers can scope
  server-side; the drop is a second line of defence).
- Scheduled runs use the PII capability captured at creation
  (`pii_allowed`); a role revoked after creation is not re-checked
  at run time (run-now does re-check the live capability).
- No owning-module consumer for staged import rows and no row
  provider for exports yet (see item 4 above).
- Compilation never verified: `cargo check` has never run in this
  VM (no toolchain, crates.io blocked).
