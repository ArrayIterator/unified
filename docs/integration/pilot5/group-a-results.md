# C3 pilot 5 — group A results (task 319)

Modules: `billing`, `cards`, `delivery_notes`, `entitlements`.
Date: 2026-11-23. No push. No new dependencies. All code and docs in English.

## Counts

| Module | Routed | Guarded | Excluded | Missing | Reads | Writes | Scope-checked | Gate-only |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| billing | 69 | 69 | 0 | 0 | 31 | 38 | 44 | 25 |
| cards | 11 | 11 | 0 | 0 | 6 | 5 | 0 | 11 |
| delivery_notes | 22 | 22 | 0 | 0 | 7 | 15 | 19 | 3 |
| entitlements | 20 | 20 | 0 | 0 | 10 | 10 | 9 | 11 |
| **Total** | **122** | **122** | **0** | **0** | **54** | **68** | **72** | **50** |

Routed set = names appearing in `.to(name)` in the module (same rule as
the pilot verifiers). No handler was excluded: every routed handler
got at least `require_tenant(&req)` first, and the guard precedes the
first identity/capability call in all 122 (verifier: 0 order violations).

## Write strictness (recorded, not harmonized — harmonization is task 315)

| Module | Variant | Meaning |
| --- | --- | --- |
| billing | mismatch-only (pilot-3 model) | GET and mutations resolve `caller_company`; a named body company must match the scope (`require_company_match` / `require_company_change`); a stored record outside the scope is 403 (`require_record_in_company_scope`); lists filter with `in_company_scope`. Company is optional metadata on invoices, templates, subscriptions and fee charges; user ownership (`caller_owns` / `require_invoice_owner`) is the module's existing pattern and stays. |
| cards | gate-only + strict writes (pilot-4 model) | Records carry `tenant_id: String` and no company field. GETs resolve `caller_company`; mutations resolve `require_company` (403 when no company resolves). Custom rules and counts are already tenant-filtered; brands are a static catalogue; settings are per-tenant. |
| delivery_notes | mismatch-only, Uuid variant (pilot-3 model) | Company is `Option<Uuid>` on notes. GET and mutations resolve `caller_company`; scope failure on a loaded note is **404** (not 403), matching the module's existing tenant loaders (`load_note` / `load_item` 404 cross-tenant) so existence never leaks. A non-UUID scope string matches nothing, not even global rows. |
| entitlements | tenant surface strict via pre-existing `TenantScope::resolve`; admin surface tenant-gate-only | The tenant surface (`tenant.rs`, `handlers_packages.rs`, `handlers_ai.rs`) requires a company in context (403 `tenant.required`, refuses a requested company != context company) on top of the guard. The admin surface (`handlers_admin.rs`) gets `require_tenant` only: admin acts on body/query company by design; cross-company admin access is a task-315 decision. `ai_capabilities` (GET, no `TenantScope`) gets guard + `caller_company` only. |

## Guard shape per module

Every handler starts with `let _tenant = require_tenant(&req)?;`
(closure handlers) or the match-form equivalent returning
`json::error_response` (direct-body handlers). Then:

- billing: `let _scope = caller_company(&_tenant, &req)?;` everywhere.
  Helpers added in `src/modules/billing/helpers.rs`: `require_tenant`,
  `caller_company`, `require_company_match`, `require_company_match_for`,
  `require_company_change`, `in_company_scope`,
  `require_record_in_company_scope` (+ 7 guard tests).
- cards: GETs bind `_scope` via `caller_company`; mutations bind
  `_company` via `require_company` (strict). Helpers added in
  `src/modules/cards/service.rs` (+ 5 guard tests); the old
  `tenant_id()` helper was deleted after the handler replacement and
  `src/modules/cards.rs` re-exports the new helpers.
- delivery_notes: `let _scope = caller_company(&_tenant, &req)?;`
  everywhere. Helpers added in `src/modules/delivery_notes/helpers.rs`:
  `require_tenant`, `caller_company`, `in_company_scope_uuid`,
  `require_note_in_company_scope` (404), `require_company_match_uuid` /
  `_for`, `require_company_change_uuid` (+ 8 guard tests); the old
  `resolve_tenant` was deleted.
- entitlements: tenant surface binds `_tenant` + `_scope`
  (`caller_company`); admin surface binds `_tenant` only. Helpers added
  in `src/modules/entitlements/helpers.rs`: `require_tenant`,
  `caller_company` (+ 3 guard tests); `src/modules/entitlements.rs`
  re-exports them.

Fail closed everywhere: no tenant context -> error (401 from
`TenantContext::from_http_request`), never a fallback to `public`.
No code in the four modules reads the `X-Company-Id` header (grep:
only the doc comments on `caller_company` stating it is never
trusted). Two `TenantContext` shapes exist in the tree and both are
respected: tests attach the middleware context
(`unified_core::middleware::TenantContext`, `tenant_id`/`company_id`
as `String`), which `unified_core::tenancy::TenantContext::from_http_request`
reads and converts (`tenant_id: Uuid`, `company_id: Option<String>`).

## Scope checks (key sites)

### billing — 44 scope-checked, 25 gate-only

- `list_invoices` filter `src/modules/billing/handlers_invoices.rs:40`,
  `list_overdue` filter `:544`; `create_invoice` body verify `:91`
  (verified value stored); `from_order` owner + scope on both return
  paths `:240`-`:254` (existing invoice: owner + record scope; built
  invoice: owner + `require_company_match_for` on the order's company,
  unnamed adopts the caller scope so a scoped caller never stores an
  unscoped invoice); `get`/`update`/`delete`/`pay`/`void`/`render`/
  `record_payment`/`create_credit_note`/`pdf_data`/`remind_invoice`
  record scope after load (`:281`, `:302`, `:359`, `:412`, `:478`,
  `:514`, `:704`, `:829`, `:950`, `:1079`); `list_payments` /
  `list_credit_notes` parent-invoice scope (`:776`, `:914`).
- `update_invoice` also dropped its inline
  `TenantContext::from_http_request` (issuing path now uses
  `_tenant.tenant_id`, `:326`).
- Subscriptions: `list_subscriptions` filter
  `src/modules/billing/hooks_subscriptions.rs:965`,
  `get_subscription` scope via `and_then` (`:1016`),
  `list_dunning_attempts` loads the parent mirror and scope-checks it
  (`:1048`).
- Templates: `list_invoice_templates` filter
  `src/modules/billing/templates_handlers.rs:86`,
  `create_invoice_template` body verify (`:128`),
  `update_invoice_template` scope + `require_company_change` (`:284`),
  `get`/`delete`/`restore`/`set_default`/`preview` record scope
  (`:260`, `:424`, `:469`, `:562`, `:629`),
  `set_default_invoice_template` also verifies the body company
  (`:576`), `list_invoice_template_versions` loads the parent template
  and scope-checks it, `print_invoice` / `print_ready_invoice` /
  `email_invoice_pdf` check invoice and template
  (`:755`/`:762`, `:840`/`:847`, `:994`/`:1006`),
  `list_invoice_print_jobs` loads the invoice and scope-checks it
  (`:888`); `preview_invoice_template` scope-checks the template
  (`:629`) and, when an `invoice_id` is given, the previewed invoice
  (`:643`).
- School fees: `list_fee_charges` filter
  `src/modules/billing/school_fees_handlers.rs:864`,
  `get`/`pay`/`void_fee_charge` record scope (`:915`, `:967`, `:1017`),
  `get_fee_receipt` / `issue_fee_receipt` / `remind_fee_charge` record
  scope (`src/modules/billing/school_fees_charges.rs:32`, `:91`, `:155`),
  `run_fee_reminders` filters charges by scope (`:234`),
  `list_fee_arrears` (`:373`) and `list_my_fee_charges` (`:869`)
  filter by scope, `generate_fee_charges` takes the company from the
  guard-resolved context (tenant id fallback when no company, no
  silent `.ok()` fallback anymore).
- Gate-only (25, no company field): `BillingSettings`,
  `ReferenceIdSettings`, `SchoolFeeType`, `StudentFeeSchedule`,
  `FeeDiscount`, `FeeGuardianLink`, `FeeReceipt`, `SchoolFeeSettings`
  handlers, plus `generate_fee_charges` (company stamped from the
  guard context — scoped by construction) and
  `reprint_invoice_print_job` (job id only; see residual below).
  Student-visibility checks (`fee_can_view_student`,
  `fee_visible_student_ids`, `fee_require_charge_access`) already
  existed and stay.

### cards — 0 scope-checked, 11 gate-only (by design)

No company field exists on any card record, so there is nothing to
scope-check: every handler is tenant-gated and tenant-filtered, and
the 5 mutations additionally require a resolved company
(`require_company`). Gate-only list: `validate`, `list_brands`,
`get_brand`, `get_settings`, `update_settings`,
`list_custom_rules`, `create_custom_rule`, `get_custom_rule`,
`update_custom_rule`, `delete_custom_rule`, `list_counts`.

### delivery_notes — 19 scope-checked, 3 gate-only

- `list_notes` filter `src/modules/delivery_notes/handlers_notes.rs:225`,
  `list_notes_for_shipment` filter (`:909`); `create_note` /
  `create_note_from_shipment` body verify (`:323`, `:390`, verified
  value passed to `build_note`); `update_note` company change via
  `require_company_change_uuid` (`:528`); `require_note_in_company_scope`
  after all 15 note loads (notes `:441`, `:468`, `:557`, `:645`,
  `:682`, `:737`, `:792`, `:867`; items
  `src/modules/delivery_notes/handlers_items.rs:58`, `:90`, `:153`,
  `:182`, `:261`; print
  `src/modules/delivery_notes/handlers_print.rs:104`, `:193`).
- Gate-only (3): `ai_capability` (capability text, no record) and
  `get_settings` / `update_settings` (per-tenant settings).

### entitlements — 9 scope-checked, 11 gate-only

- Tenant surface scope is `TenantScope::resolve` (pre-existing,
  strict): `tenant.rs:101`, `:189`, `:242`, `:356`;
  `handlers_packages.rs:38`, `:243`, `:281`, `:315`;
  `handlers_ai.rs:32` (`ai_suggest`).
- Gate-only (11): the whole admin surface (`handlers_admin.rs`, 10:
  `list_entitlements`, `create_grant`, `get_entitlement`,
  `update_grant`, `delete_grant`, `list_history`,
  `list_denied_attempts`, `get_settings`, `update_settings`,
  `run_sweep`) plus `ai_capabilities`.

## Scans: parameterised, not global

- `run_reminder_scan(state, scope)` (`handlers_invoices.rs:1099`):
  skips out-of-scope invoices; the summary counts only in-scope
  invoices (`invoices_checked`). HTTP `run_reminders` passes the
  caller scope (`:1161`); the scheduler hook in `hooks_payments.rs`
  passes `&None` (global system scan, same precedent as the
  analytics scheduler hook).
- `BillingState::run_subscription_scan(&self, scope)`
  (`state.rs:875`): skips out-of-scope mirrors. HTTP
  `run_subscription_scan_endpoint` passes the caller scope
  (`hooks_subscriptions.rs:1077`); the scheduler hook passes `&None`
  (`hooks_payments.rs:590`, `:601`).
- `run_fee_reminders` is HTTP-only and filters charges by scope
  (`school_fees_charges.rs:234`).
- Existing `state.rs` tests updated to the new signatures
  (`run_subscription_scan(&None)`, 3 sites).

## Money-path leaks: closed vs remaining

All `conflict(` sites in the four modules were reviewed.

Closed by this work:

1. billing `list_invoice_templates` company_id filter let a
   template-view caller list other companies' templates
   (`templates_handlers.rs:86` scope filter).
2. delivery_notes `list_notes` / `list_notes_for_shipment`
   company_id filter let a caller read another company's notes
   within the tenant (`handlers_notes.rs:225`, `:909`).
3. billing subscriptions surface had NO auth at all
   (`list_subscriptions`, `get_subscription`,
   `list_dunning_attempts`, `run_subscription_scan_endpoint`) —
   closed by guard + scope checks + scoped scan.
4. billing `from_order` had NO owner check (any order id, no auth
   when the body user id was omitted) — closed by guard +
   owner/scope checks (`handlers_invoices.rs:240`-`:254`).
5. billing print handlers (`print_invoice`, `print_ready_invoice`,
   `email_invoice_pdf`, `list_invoice_print_jobs`) loaded invoices
   with only `PERM_BILLING_PRINT`, no owner/scope check — closed by
   company-scope checks on invoice and template
   (`templates_handlers.rs:755`-`:1006`).
6. Scans were global side-effect triggers (`run_reminders`,
   `run_subscription_scan_endpoint`, `run_fee_reminders` iterated
   all tenants' invoices/subscriptions/charges) — closed by scoping
   to the caller company (scheduler hook keeps the global scan).

Reviewed, no close needed:

- billing `create_fee_type` duplicate-code 409: `SchoolFeeType`
  has no company field; the code namespace is per-tenant via the
  gate.
- billing `generate_fee_charges` duplicate-period 409: manage-only,
  caller names the schedule.
- billing `create_fee_guardian` duplicate-link 409: manage-only,
  caller names both user ids.
- billing `issue_fee_receipt` duplicate-receipt 409: manage-only,
  caller names the charge — and the charge is now scope-checked
  first (`school_fees_charges.rs:91`).
- delivery_notes status-transition 409s: all on tenant-loaded
  notes (404 cross-tenant).
- entitlements status-transition 409s: admin surface;
  package-change settled 409 happens after the scope check.
- cards has NO uniqueness/conflict checks at all.

Remaining (residuals, for task 315 / the owning module):

- Cross-USER within a company remains on the print surface:
  `PERM_BILLING_PRINT` is staff-level, so a staff member can print
  any invoice in their own company; the company-scope check closes
  cross-company, not cross-user.
- `reprint_invoice_print_job` (`templates_handlers.rs`) takes only
  a job id and emits to the printing bus with no invoice/company
  load: a caller with `PERM_BILLING_PRINT` who guesses another
  company's job id could reprint it. Job authorization lives in the
  printing module; the billing side has no record to check.
- Entitlements admin surface (`handlers_admin.rs`) is
  tenant-gate-only by design: an admin caller can act on any
  company named in the body/query. Cross-company admin access is a
  task-315 decision, not closed here.

## Test updates (existing tests, all UNRUN — see verification)

- billing `handlers_invoices.rs` tests: `verified_request` and
  `verified_request_without_roles` now attach the middleware
  `TenantContext` (valid uuidv7 tenant id) so the guard resolves;
  the anonymous 401 test still passes.
- billing `state.rs` tests: `run_subscription_scan(&None)` (3 sites).
- entitlements `handlers_admin.rs` tests: `verified_request`
  gains the tenant context; the anonymous 401 test now asserts the
  guard message ("did not resolve to a tenant") instead of
  "verified caller" (status stays 401).
- entitlements `handlers_packages.rs` tests: read-only-role tests
  use `tenant_request(&Uuid::now_v7().to_string())` so the role
  check runs; the manage test was renamed to
  `apply_package_change_with_manage_rejects_missing_tenant_context`
  and expects 401 + guard message (was 403 "Tenant context is
  required"); the role-spoof test expects 401 (was 403).
- entitlements `handlers_ai.rs` tests: spoof test expects 401
  (was 403); the accepts test's `verified_request` gains the tenant
  context.
- entitlements `tenant.rs` tests: read-only-role tests use
  `tenant_request` with a uuidv7; the manage test was renamed to
  `enable_module_with_manage_rejects_missing_tenant_context` and
  expects 401 + guard message; both role-spoof tests expect 401
  (were 403).
- Two test names were renamed because the old names described
  behaviour that no longer exists (the guard rejects before tenant
  scope is ever resolved); no test logic was weakened — the
  read-only tests still assert the 403 capability rejection, now
  reached past a resolved tenant.

## Verifier

`docs/integration/pilot5/c3_group_a_verify.py` — copied from
`docs/integration/pilot4/c3_group_a_verify.py` and adapted.
Adaptations (also in the script header):

- Per-module identity calls for the order check: billing adds
  `fee_caller(&req)`, `billing_authorize(`, `billing_caller(`,
  `fee_require_view(`, `fee_require_manage(`; entitlements adds
  `caller_of(`, `admin_caller(`, `caller_from(`,
  `require_capability(`; cards / delivery_notes keep `caller(&req)`
  + `require_permission(`.
- Kind rules: `strict` (cards, pilot-4 rule), `mismatch`
  (billing, delivery_notes — GET and mutation both require
  `caller_company`, never `require_company`), `entitlements`
  (tenant surface: `caller_company` + `TenantScope::resolve`,
  except `ai_capabilities` which is `caller_company` only;
  `handlers_admin.rs` is gate-only, no kind requirement).
- Scope counting: per-module scope markers; billing also counts
  the two scoped triggers (`run_reminder_scan(&state, &_scope)`,
  `run_subscription_scan(&_scope)`), which pass the caller scope
  into the scan instead of checking a record.

Results (re-run after all edits):

```
billing: guarded=69 (reads=31, writes=38) scope_checked=44 gate_only=25 missing=0 excluded=0 order_violations=0 kind_mismatches=0 double_guards=0
cards: guarded=11 (reads=6, writes=5) scope_checked=0 gate_only=11 missing=0 excluded=0 order_violations=0 kind_mismatches=0 double_guards=0
delivery_notes: guarded=22 (reads=7, writes=15) scope_checked=19 gate_only=3 missing=0 excluded=0 order_violations=0 kind_mismatches=0 double_guards=0
entitlements: guarded=20 (reads=10, writes=10) scope_checked=9 gate_only=11 missing=0 excluded=0 order_violations=0 kind_mismatches=0 double_guards=0
```

Exit code 0 for all four modules. No code in the four modules reads
`X-Company-Id` (grep: only `caller_company` doc comments stating it
is never trusted).

## Verification

- `rustfmt --edition 2024 --emit stdout` (parse-level) passes on all
  24 touched source files.
- `rustfmt --edition 2024 --check` still reports diffs, but they are
  pre-existing repo-wide style drift (import ordering and long-line
  wrapping the codebase does not follow rustfmt for), not new code.
  The new-code lines rustfmt flagged were fixed in place: the four
  `caller_company` match arms (single-line guard form), the
  `set_default_invoice_template` company line, the
  `list_notes_for_shipment` filter join, and several guard-test
  assert wrappings. No whole-file formatting was applied.
- Signature audit (no compiler available): every new-helper call site
  was checked against its definition. One real bug was caught this
  way — `require_company_change` takes `(scope, new)`, not
  `(tenant, req, current, new)`; the 4-argument call first written
  in `update_invoice_template` was corrected to the
  `if let Some(company_id)` form before verification.
- `cargo check -p unified` and `cargo test` were NOT run (root bin
  crate ~5.7 GiB RSS, OOM-killed on this VM, per task instruction).
  All new tests (billing helpers 7, cards 5, delivery_notes 8,
  entitlements 3) and all updated existing tests are written but
  UNRUN here; state that plainly. The parent should run the
  per-crate suites (`cargo test -p unified-core` does not cover
  these; the module tests live in the root bin crate) or a full
  typecheck when a bigger machine is available.

## Files changed

Source (24):

- `src/modules/billing/helpers.rs` — guard + scope helpers, 7 tests.
- `src/modules/billing/handlers_invoices.rs` — 19 guards, scope
  checks, scoped `run_reminder_scan`, test-helper tenant context.
- `src/modules/billing/hooks_subscriptions.rs` — 4 guards, scope
  checks, scoped scan endpoint call, `HttpRequest` import.
- `src/modules/billing/hooks_payments.rs` — scheduler hook call
  sites pass `&None`.
- `src/modules/billing/reference.rs` — 2 guards, helpers import.
- `src/modules/billing/school_fees_handlers.rs` — 16 guards, scope
  checks, guard-context company in `generate_fee_charges`.
- `src/modules/billing/school_fees_charges.rs` — 14 guards, scope
  checks, scoped `run_fee_reminders`.
- `src/modules/billing/templates_handlers.rs` — 14 guards, scope
  checks.
- `src/modules/billing/state.rs` — scoped `run_subscription_scan`,
  3 test call sites.
- `src/modules/cards/service.rs` — guard + strict-write helpers,
  5 tests; old `tenant_id()` deleted.
- `src/modules/cards/handlers.rs` — 11 guards.
- `src/modules/cards.rs` — re-exports updated.
- `src/modules/delivery_notes/helpers.rs` — guard + Uuid scope
  helpers, 8 tests; old `resolve_tenant` deleted.
- `src/modules/delivery_notes/handlers_items.rs` — 5 guards, note
  scope after each load.
- `src/modules/delivery_notes/handlers_notes.rs` — 12 guards, list
  filters, create/update company verify, note scope after loads.
- `src/modules/delivery_notes/handlers_print.rs` — 2 guards, note
  scope after loads.
- `src/modules/delivery_notes/handlers_settings.rs` — 2 guards
  (gate-only).
- `src/modules/delivery_notes/handlers_ai.rs` — guard (gate-only).
- `src/modules/entitlements/helpers.rs` — `require_tenant` /
  `caller_company`, 3 tests.
- `src/modules/entitlements.rs` — re-export added.
- `src/modules/entitlements/tenant.rs` — 4 guards, 3 test updates.
- `src/modules/entitlements/handlers_packages.rs` — 4 guards,
  3 test updates.
- `src/modules/entitlements/handlers_ai.rs` — 2 guards, 2 test
  updates.
- `src/modules/entitlements/handlers_admin.rs` — 10 guards
  (gate-only), 1 test message update + test-helper tenant context.

Docs (2, new):

- `docs/integration/pilot5/c3_group_a_verify.py`
- `docs/integration/pilot5/group-a-results.md` (this file)

Not touched: `docs/integration/C3_MODULE_GUARD_MAP.md` (coordinator
owns it), any other group's modules, no new dependencies, no push.
