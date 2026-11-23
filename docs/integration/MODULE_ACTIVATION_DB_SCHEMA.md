# Module Activation & Database Schema — Design Rule

**Status:** RULE (user directive, 2026-11-24) — not yet implemented.
**Applies to:** every module with database tables (core + external).

## The rule (user's words, restated)

The database is NEVER migrated all at once. Tables evolve depending on which modules
are active. When a module is enabled, it prepares its tables and checks them. If
there is a conflict, the user is told — e.g. a table does not match, a table does
not fit, a schema mismatch, or a table needs an upgrade (with a notice). Enabling a
module does a dry run FIRST. Whenever a module is active and it has the database as
its source of truth, the database schema is checked.

## Current state (verified 2026-11-24 from source)

- Migrations are ONE global numbered set: `crates/unified-core/migrations/`
  000001–000135 (gapless), run by the runner in `crates/unified-core/src/db/mod.rs`
  (diesel migrations). No migration has ever run on live PostgreSQL.
- Module enable (`enable_with_dependencies` in `src/modules/extensions/service.rs`)
  handles dependency activation only. It does NOT prepare tables, does NOT check
  the schema, and does NOT dry-run.
- Dry-run exists today only in clusters / import_export / workflows — NOT in
  module activation.
- So the rule below is a NEW behaviour, not a description of the current code.

## Design

### 1. Per-module schema, not a big-bang migration

- Each module declares its own schema: the tables it owns, their columns, types,
  constraints, and indexes (a schema definition, versioned per module).
- The global numbered migrations stay as the HISTORY (how the schema got here),
  but activation never means "run all migrations". Activation means: for THIS
  module, prepare + verify THIS module's tables.
- A module's tables live in the tenant schema (schema-per-tenant) or in `public`
  for control-plane modules — same isolation rules as C3 (fail closed, never fall
  back to public).

### 2. Activation flow (enable = dry run first)

1. **Dry run (always first, no writes):** compute the plan for this module —
   tables to create, columns to add, indexes to build, tables to upgrade.
   Show the plan. Nothing is applied yet.
2. **Check (schema as source of truth):** compare each table the module owns
   against the live database schema in the tenant schema:
   - table missing → plan: CREATE.
   - table exists but columns/types/constraints differ → plan: UPGRADE, and
     REPORT the exact mismatch (table, column, expected vs actual).
   - table exists but belongs to another module / unknown shape → CONFLICT,
     block activation, report it.
   - table needs an upgrade (module schema version > stored version) → NOTICE
     to the user (what changes, whether data is touched, whether it is reversible).
3. **Apply (only after the dry run is accepted):** run the plan inside a
   transaction where possible; record the module's schema version per tenant.
4. **Re-check after apply:** verify the live schema matches the module's
   declaration; if it does not, activation FAILS (do not leave a half-enabled
   module).

### 3. Conflict + notice rules

- Any mismatch is reported to the user BEFORE anything is applied: table name,
  what is wrong (missing column / wrong type / missing constraint / extra
  unexpected column), and what the fix would do.
- A table that needs an upgrade produces a NOTICE (not a silent auto-fix):
  what will change, whether existing data is affected, and whether the upgrade
  can be rolled back.
- A hard conflict (table owned by another module, or a shape that cannot be
  reconciled) BLOCKS activation. The module stays disabled and the conflict is
  shown, not auto-resolved.
- Deactivation does NOT drop tables (data stays; re-enabling re-checks).

### 4. Ongoing checks

- While a module is active, its schema is re-checked on startup and on demand
  (a "check schema" action per module): if the live schema drifts from the
  module's declaration, surface a warning with the exact drift.
- The database is the source of truth for what exists; the module's declaration
  is the source of truth for what SHOULD exist. The check is the diff between
  the two.

## Follow-ups (not in this doc's scope)

- Which modules declare their schema first (pilot: one module with tables, e.g.
  sms_marketing or i18n after task 410).
- The exact schema-definition format (per-module manifest vs Rust types vs SQL).
- How the dry-run plan is presented in the admin UI.
- Whether upgrades run inside one transaction per module (PostgreSQL DDL is
  transactional — use it).
