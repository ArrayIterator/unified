# C3 Pilot 3 — Worker Group D Results (tracker task 314)

Tenant edge guards for `backup`, `import_export`, `tax`, `analytics`, `bi`.
Local tree: `/home/hatch/workspace/unified-push`. The guard map
(`docs/integration/C3_MODULE_GUARD_MAP.md`) was NOT edited; the
coordinator merges all groups' rows.

## Summary

| Module | Routed | Guarded | Excluded | Missing |
| --- | --- | --- | --- | --- |
| bi | 21 | 21 | 0 | 0 |
| backup | 20 | 20 | 0 | 0 |
| import_export | 27 | 27 | 0 | 0 |
| tax | 26 | 26 | 0 | 0 |
| analytics | 27 | 27 | 0 | 0 |
| **Total** | **121** | **121** | **0** | **0** |

Map-count discrepancies (verified by counting `.to(...)` route
targets in each module's route table): backup is truly 20 routed,
not 22 as the map says; tax is truly 26, not 27. No handler was
found unguarded in either module, so the map overcounts rather
than the work undercounting.

`c3_verify.py` reports analytics as 26 guarded because its route
regex misses the qualified route target
`super::handlers_dashboard::record_metric_handler` (POST
`/metrics` in `module.rs:288`). That handler was verified
separately by inspecting its body: it contains
`require_tenant(&req)` and `caller_company(&_tenant, &req)`.
True analytics count is 27/0/0.

## Guard idiom (all five modules)

In every routed handler, `require_tenant(&req)?` runs FIRST —
before any identity or capability call — then `caller_company`
is resolved from the VERIFIED caller (middleware-resolved tenant
context company first, server-resolved caller company second,
403 on mismatch). The `X-Company-Id` header is never trusted;
there is no fallback to `public`. Record access follows the
shared rule: company callers read own + global only; unresolved
callers read global only; writes require a resolved company
(create/update bodies are verified against the caller company;
an omitted body company adopts the caller scope, never stored
unscoped under a scoped caller; an explicit clear stays a clear).

Reference implementations copied: `tickets/validation.rs`,
`chat/helpers.rs` (pilot 1), `products/helpers.rs`,
`projects/state.rs` (pilot 2), shared `CallerScope` in
`crates/unified-core/src/tenancy.rs`, `crates/unified-core/src/caller.rs`.

## Per-module detail

### bi — 21 guarded, 0 excluded, 0 missing

- `state.rs`: deleted the header-first `resolve_caller_company`
  (`X-Company-Id` first, then the `user.company_requested` hook).
  Added `require_tenant`, `caller_company` (verified caller only),
  `require_company_match` / `_for`, `in_company_scope_str`
  (required-String records: unresolved scope matches only
  empty-company/global rows), `require_record_in_company_scope_str`,
  plus a `tenant_guard_tests` module (fail-closed, resolve,
  tenant-prefers, mismatch-403, scope, body-match).
- `handlers.rs`: gate first in all 21 handlers (`export_report`
  gated at fn top via match-return before its format check, since
  format validation precedes its closure). All 14
  `resolve_caller_company` sites replaced with the verified scope.
  `list_reports` / `list_dashboards` filters changed from
  `if let Some(company)` to `in_company_scope_str` — unresolved
  callers previously saw ALL rows, now only global rows.
  `create_report` / `create_dashboard` body check replaced with
  `require_company_match` — unresolved callers can no longer create
  under a company. Scope checks added after load in
  `update_report` / `delete_report` (owner or manage-capable +
  `can_access_company`) and `update_dashboard` /
  `delete_dashboard` (`can_access_company` with manage bypass, per
  the task-312-approved design). `can_read_report` checks added
  after report load in `create_schedule` / `delete_schedule` /
  `run_schedule_now` (mirroring `list_schedules` / `get_schedule`).
  `forecast_churn` reworked: caller company wins; unresolved +
  named query company → 403; unresolved rows filtered to
  global-only. The `can_access_company` `can_manage` bypass and the
  `can_read_report` owner bypass are kept (capability-gated, not
  scope-gated).
- `engine.rs`: `daily_series` and `read_source_rows` `None` arms
  changed from "no filter" (all companies) to global-only
  (company None or empty). Both production call sites of
  `read_source_rows` pass `Some(...)`. Pre-existing quirk noted,
  not changed: global reports (company `""`) match no source rows
  via `Some("")`.
- `validation.rs`: doc reference updated from
  `resolve_caller_company` to `caller_company`. The task-312-fixed
  `can_access_company` (`state.rs:159`) and `can_read_report`
  (`validation.rs:215`) were reused, not redone.

### backup — 20 guarded, 0 excluded, 0 missing

- `validation.rs`: removed the `company_id` field from the
  `Caller` struct and from `caller()` (scope now comes only from
  `caller_company`); fixed a stale doc comment ("model as the ai
  module: the trusted edge sets these headers"). Added
  `require_tenant`, `caller_company` (products-style),
  `in_company_scope`, `require_record_in_company_scope`,
  `require_company_match` / `_for` (generic form of the
  task-312-fixed `job_visible` / `run_visible`, which were kept
  as-is), plus `tenant_guard_tests`.
- `handlers.rs`: gate first in all 20 handlers (closure handlers
  gated inside the closure; `list_jobs` gated inside its inner
  auth closure; `list_job_runs` / `list_runs` gated at fn top via
  match-return). All `caller.company_id` uses replaced with the
  verified scope. `create_job` now stamps the verified scope
  (scoped caller → own company; unresolved → None/global).
  `maintenance_verify` / `maintenance_prune` pass the verified
  scope to `verify_stale_runs` / `prune_all_jobs`. This closes the
  task-312 note at the handler level: a verified-caller `None`
  used to see every backup job/run; handler guards now gate +
  scope-filter on top of the fixed helpers.
- Non-routed helpers NOT gated by design (called only from
  guarded handlers after `visible_job` / `visible_run` scope
  checks): `drill_one`, `perform_restore`, `rollback_restore`,
  `resolve_restore_run` (enforces run.job_id match). `execution.rs`
  background functions are not handlers.

### import_export — 27 guarded, 0 excluded, 0 missing

- `auth.rs`: added `require_tenant`; added `caller_company(tenant,
  req)` (tenant-first parsed as UUID, verified-caller second, 403
  on mismatch; non-UUID/nil values treated as absent) and a
  `parse_company_id` helper. `resolve_caller_company` (hook-only)
  kept as a last-resort source with a repaired doc comment.
  `require_caller_company` signature changed to `(ctx, tenant, req,
  caller)`: tries `caller_company` first, falls back to the hook,
  keeps the fail-closed rule (non-manage + no company → 403).
  `can_view_company` strictness is UNTOUCHED (`(None, _) => false`,
  stricter than the shared rule, pinned by tests). Added
  `tenant_guard_tests` (require_tenant, caller_company
  tenant-first / verified-fallback / mismatch / non-UUID-ignored /
  header-never-trusted). Updated 3 existing
  `require_caller_company` tests to the new signature.
- `handlers.rs` + `handlers_schedules.rs`: gate first in all 27
  handlers (14 closure gates + 2 download top-gates in
  `handlers.rs`; 11 closure gates in `handlers_schedules.rs`);
  all 25 `require_caller_company(&ctx, &req, &caller)` sites
  rebased to `require_caller_company(&ctx, &_tenant, &req, &caller)`.
  `list_entities` / `get_settings` / `update_settings` are
  gate-only (global settings/catalogue, no company data).
  `export.rs:71` `can_view_company` use unchanged.

### tax — 26 guarded, 0 excluded, 0 missing

- `validation.rs`: added `require_tenant`; added
  `parse_company_id` (non-UUID/nil → absent); added
  `caller_company(tenant, req)` (products-style); added
  `in_company_scope(record: Uuid, caller: &Option<Uuid>)` with
  `None => false` — tax records always carry a company, so there
  are no global rows and an unresolved caller matches nothing;
  added `require_record_in_company_scope`. Reworked
  `resolve_caller_company` to hook-only (header block removed).
  Reworked `acting_company` to the new signature `(ctx, tenant,
  req, caller)`: `caller_company` → hook fallback → 400 if none →
  `require_company`. The error message no longer mentions
  `X-Company-Id`. `require_company_scope` untouched (already
  fail-closed per task 312). Added `tenant_guard_tests`
  (fail-closed, resolve, tenant-prefers, verified-fallback,
  mismatch-403, header-never-trusted, scope).
- `handlers_rules.rs` (8), `handlers_filings.rs` (5),
  `handlers_connectors.rs` (7), `handlers_reports.rs` (6): gate
  first in every handler (closure-first-line and match-form auth
  closures); every `acting_company(&ctx, &req, &caller)` rebased
  to `acting_company(&ctx, &_tenant, &req, &caller)` (26 sites).
  `get_rule`: removed the redundant
  `require_company_scope(resolve_caller_company(&ctx, &req, &caller), rule.company_id)?;`
  line — the `rule.company_id != company_id` 404 check right
  after it already enforces the acting company, and the removed
  line re-resolved from the untrusted header. Lists already filter
  by `== company_id`; single loads already 404 on mismatch;
  creates stamp the acting company. `check_due_filings` calls
  `acting_company` at `handlers_filings.rs:392`.
- Non-routed helper NOT gated by design: `submit_through_connector`
  (called only from the gated `submit_filing`).

### analytics — 27 guarded, 0 excluded, 0 missing

- `helpers.rs`: added `require_tenant`, `caller_company`
  (String; tenant-first, verified-caller second, 403 on
  mismatch), `require_company_match` / `_for`,
  `in_company_scope` (Option<String> records; unresolved scope
  matches only unscoped/global rows), and
  `require_record_in_company_scope`, plus `tenant_guard_tests`
  (fail-closed, resolve, tenant-prefers, verified-fallback,
  mismatch-403, scope, body-match).
- `handlers_dashboard.rs` (12), `handlers_definitions.rs` (5),
  `handlers_runs.rs` (4), `handlers_scheduled.rs` (6): gate
  first in all 27 handlers — direct-style handlers
  (`get_dashboard`, `list_sales`, `get_license_summary`,
  `list_metrics`, `list_health`, `list_reports`, `get_report`,
  `list_scheduled`, `export_report`) gated at fn top via
  match-return before `analytics_authorize`; closure-style
  handlers gated inside the closure before `analytics_authorize`.
- Only `SaleRecord.company_id: Option<String>` carries scope
  (`types.rs:42`). Everything else (LicenseSnapshot,
  MetricSample, HealthSample, ReportDefinition, ReportRun,
  ScheduledReport, TaskMirror, PoolMirror) has no company field
  and is gate-only.
- Sales scoping applied:
  - `list_sales`: filter `in_company_scope(&sale.company_id, &_scope)`.
  - `get_dashboard` and `get_revenue_summary`: sales loops skip
    out-of-scope rows.
  - `record_sale`: body company verified via
    `require_company_match_for` — a caller can no longer claim
    another company in the body (previously stored verbatim), an
    omitted body company adopts the caller scope, and the
    duplicate `order_id` check is now scoped to same-company
    records.
  - `get_sales_summary`: scope threaded into `revenue_rows`.
  - `reports.rs`: `company: &Option<String>` threaded through
    `revenue_rows`, `sales_by_product_rows`, `compute_rows`, and
    `run_report`; sales aggregates filter with
    `in_company_scope`. Callers updated: `get_sales_summary`,
    `run_report_now`, `run_scheduled_now`.
  - `hooks.rs:258` `on_report_run_requested`: scheduler-triggered
    runs have no caller, so they run with `&None` (global sales
    only) — fail closed by design, documented at the call site.
  - Residual, documented: license / usage / uptime rows read
    unscoped mirrors and are gate-only; they carry no company
    field to scope on.
- `handlers_dashboard.rs` handler tests updated: the test
  module's `verified_request` helper now attaches a tenant
  context (no company), so the accept/403 tests exercise the
  guarded handlers instead of failing at the tenant gate with
  401. Anonymous-caller tests still expect 401 (the gate fails
  first, which is the same status the old identity check
  returned).

## Exclusions

None. Every routed handler in all five modules is guarded.
Non-routed helpers excluded by design (not routed, called only
from guarded handlers after scope checks): backup `drill_one`,
`perform_restore`, `rollback_restore`, `resolve_restore_run`;
tax `submit_through_connector`; analytics scheduler hook
`on_report_run_requested` runs with unresolved scope (`&None`).
No token-based public handler was found in any of the five
modules.

## Files touched

- `src/modules/bi/state.rs`, `src/modules/bi/handlers.rs`,
  `src/modules/bi/engine.rs`, `src/modules/bi/validation.rs`
- `src/modules/backup/validation.rs`,
  `src/modules/backup/handlers.rs`
- `src/modules/import_export/auth.rs`,
  `src/modules/import_export/handlers.rs`,
  `src/modules/import_export/handlers_schedules.rs`
- `src/modules/tax/validation.rs`,
  `src/modules/tax/handlers_rules.rs`,
  `src/modules/tax/handlers_filings.rs`,
  `src/modules/tax/handlers_connectors.rs`,
  `src/modules/tax/handlers_reports.rs`
- `src/modules/analytics/helpers.rs`,
  `src/modules/analytics/handlers_dashboard.rs`,
  `src/modules/analytics/handlers_definitions.rs`,
  `src/modules/analytics/handlers_runs.rs`,
  `src/modules/analytics/handlers_scheduled.rs`,
  `src/modules/analytics/reports.rs`, `src/modules/analytics/hooks.rs`

The guard map (`docs/integration/C3_MODULE_GUARD_MAP.md`) was NOT
edited. No new dependencies. No push.

## Verification

- `rustfmt --edition 2024 --emit stdout` parse check (with
  `PATH="$HOME/.cargo/bin:$PATH"`) on every file listed above:
  all parse OK.
- `python3 /tmp/c3_verify.py <module>` (routed-handler regex
  count vs `require_tenant(&req)` presence):
  bi 21/0/0, backup 20/0/0, import_export 27/0/0, tax 26/0/0,
  analytics 26/0/0 (see the qualified-route note above for the
  true 27th handler, `record_metric_handler`, verified by body
  inspection).
- Gate-order check (custom script over all 5 modules' routed
  handler files): no identity/capability call precedes
  `require_tenant` in any routed handler. The only three hits
  were non-routed helpers (`backup::drill_one`,
  `backup::perform_restore`, `tax::submit_through_connector`),
  confirmed absent from every route table.
- Leftover-header check: zero remaining old-signature
  `acting_company(&ctx, &req, ...)` calls in tax; remaining
  `x-company-id` mentions are only inside tests that prove the
  header is NOT trusted (tax `validation.rs`, import_export
  `auth.rs`).
- Unit tests added/adjusted (fail-closed / resolve / scope /
  body-match) in `bi/state.rs`, `backup/validation.rs`,
  `import_export/auth.rs`, `tax/validation.rs`,
  `analytics/helpers.rs`, plus the `analytics/handlers_dashboard.rs`
  test-helper update.

## NOT verified

- Root-bin files were NOT typechecked: `cargo check -p unified`
  and `cargo test` on the root bin were not run (bin crate
  ~5.7 GiB RSS, OOM-killed on this VM).
- The new/adjusted unit tests are UNRUN.
- No live WP/render check applies (Rust service, not a theme).
