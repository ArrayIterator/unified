# C11 — DB read-path program: scoping brief (task 271)

Status: SCOPING DRAFT — not started. Counts below are from inspection R1
(2026-11-22); re-measure at kickoff, the tree has changed since.

## The problem
In-memory DashMap stores are still authoritative for business data. R1 counted
**784 DashMap store declarations (700 fields + 84 `OnceLock` statics) across
129 files**. Only 8 module groups reference `DbPool` at all; two modules
explicitly say business stores have no `DbPool`
(`src/modules/exams/module.rs:158`, `src/modules/library/module.rs:147`).
No startup hydration was found anywhere. Migrations exist but are labelled
"persistence targets only, not yet the read path".

Representative sites (R1): `src/modules/store.rs:62` ("in-memory maps stay
authoritative until the database layer is wired"),
`src/modules/billing/state.rs:23-86`, `src/modules/elearning/state.rs:35-51`,
`src/modules/scheduler/state.rs:17-19`, `src/modules/web3/state.rs:23-37`.

Consequences: data is lost on restart, invisible to other instances, and
unqueryable — the multi-instance story, the C3 guards (which assume DB-backed
records with `company_id`/`tenant_id`), and the backup/audit story all rest
on a read path that does not exist yet for most modules.

## Program shape (per module, in priority order)
1. **Money + lifecycle state first:** billing, payments, store, licensing,
   subscriptions — anything where loss or cross-instance invisibility is a
   correctness or money bug, not a cosmetic one.
2. **Then tenant-facing business state:** the modules the C3 pilots guarded
   whose records still carry no scope field (gate-only modules are the
   visible symptom — e.g. esg `scope_type`/`scope_id`, forums, payments
   record-level `tenant_id` stamping).
3. **Then the rest**, module by module, each as its own tracker task
   (same pattern as the C3 pilots: guard-map-style tracking per module).

Per module, the fix is one of: load at startup (hydrate the map from DB),
write-through (every mutation writes DB first, map is a cache), or direct
Diesel queries (drop the map). The choice is per-module and must be recorded.

## Deliberately stays in memory (do NOT migrate)
- Lock / dedup / rate-limit maps (ephemeral by nature).
- Caches with a DB behind them (already covered by the cache layer).
- Anything the module documents as intentionally ephemeral.

## Verification per module
- Restart test: state survives a restart (seed → restart → read back).
- Cross-check: DB row count matches what the API returns.
- `cargo test -p unified-core` baseline 54/0 where core is touched;
  root-bin checks need CI/Colab (OOM wall) — state plainly.

## Out of scope
- Registry tables (C12 / task 269) — related but separate program.
- Write-strictness harmonization (task 315) and read-rule unification
  (task 320) — those assume the DB read path exists; C11 is the
  prerequisite, not the same work.
