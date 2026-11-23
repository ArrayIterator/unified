# C3 scope-rule unification — decision brief (task 320)

Status: ADOPTED 2026-11-23 — rule A as canonical. Implemented in the tree; see "Implementation (2026-11-23)" below and the guard map section "Tasks 315 + 320 outcomes".

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

## Implementation (2026-11-23)

- **Read helpers flipped to rule A:** 34 files — every
  `in_company_scope` definition (all shapes: canonical
  `Option<String>`, `Option<Uuid>` variants, `&str` records
  with empty=global for okrs/bi) now returns true for a
  NULL-company record under any resolved scope. Doc wording
  updated; unit-test assertions flipped to positive (30 files).
  Skipped: documents (whole directory — documents worker owns
  it), software/mcp/web3/library (already rule A),
  tax/timetracker/presets-uuid/portals-str (no global rows).
- **Read leak fixed:** risk `handlers_risks.rs:413` was misusing
  the write helper `require_company_match_for` as a read filter
  and, via an `.is_none_or(...)` predicate, showed ALL risks to
  an unresolved caller with no filter. Now uses
  `CallerScope::from_company(scope).scoped_read_company(...)`
  + `in_company_scope` as the list predicate.
- **Write side untouched by 320** (that is task 315's job): the
  strict writable checks and the `require_company_for_write`
  gate are the write enforcement; rule A changes reads only.
- **Docs:** `CallerScope` in `crates/unified-core/src/tenancy.rs`
  carries an "Adopted rules (C3 tasks 315/320, 2026-11-23)"
  section; `require_company_scope` is documented as the shared
  write guard.
- **Verifiers:** the pilot group verifiers were updated for the
  new rules and re-run with the documented exclusions — 135
  runs, all exit 0 (logs
  `~/workspace/unified-check/c3-verify-20261123/`).
