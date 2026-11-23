# C3 scope-rule unification — decision brief (task 320)

Status: DRAFT — awaiting the user's pick. Do not implement until the user chooses.

## The two rules in the tree (both fail-closed; they differ only on global/legacy-row visibility)

### A. Core rule — `CallerScope` (`crates/unified-core/src/tenancy.rs:401-410`)
- `Company(c)`: reads that company's rows PLUS global rows (`company_id` NULL); writes that company only.
- `Platform`: reads/writes any.
- `Unresolved`: reads global only; writes nothing.

### B. Module-helper rule — e.g. `src/modules/tickets/validation.rs:236` (`in_company_scope`)
- `None` record (global/legacy): visible ONLY to an unresolved caller.
- `Some(c)` record: visible ONLY to a caller of company `c`.
- Net effect: a company caller NEVER sees global rows.

## Why it matters
- Global rows are real: legacy rows with no company, shared defaults (templates,
  catalogues, system presets). Under rule B a company user cannot see shared
  content at all; under rule A they can.
- Writes are company-scoped under BOTH rules, so the leak surface is read-only,
  and only for rows that are unowned by construction (NULL company).
- Pilots 2–4 implemented rule B in most modules; the C3 synthesis recorded rule A
  as the core statement. Task 312's audit also used rule B.

## Recommendation (parent's): adopt rule A as canonical
1. It is what the core type already documents, and the C3 synthesis already
   records it — rule B is the deviation, not the other way around.
2. Shared/global content is a real product need; hiding it breaks features
   (default templates, public catalogues) for every company caller.
3. The risk is bounded: global rows are unowned by definition, and writes stay
   strictly company-scoped either way.
4. Migration: adjust the per-module read helpers to treat a NULL-company record
   as visible to any resolved scope (tenant/company/platform); keep the write
   side strict (a write always requires the record's company to match the
   caller, or the record to be global AND the caller to be platform/admin —
   per task 315's decision). Unresolved callers keep global-only reads.
5. Cost: ~40+ helper functions across modules plus their tests; every pilot
   verifier must be re-run (the verifiers encode the current rule per module).

## If the user picks rule B instead
- The core `CallerScope` docs must be corrected (they currently promise A).
- Shared content needs an explicit mechanism (e.g. a `shared_with` list or
  per-row visibility flag) or it stays invisible to companies — a product gap
  to decide separately.
- Migration is the mirror image: tighten the core-adjacent read paths; the
  module helpers mostly stay as they are.

## Out of scope for 320
- Write-strictness harmonization (task 315) — 320 is the READ rule only.
- Record-level `tenant_id` stamping for gate-only records (C11 / task 271).
- `inbox.tenant_visible` (product decision, open since task 312).
