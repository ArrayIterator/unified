# C3 pilot 4, group A results (tracker task 316)

Worker: group A. Date: 2026-11-23. Tree: `/home/hatch/workspace/unified-push`.
Scope: the 8 gate-only modules assigned to this group. The tracker was
not touched and `docs/integration/C3_MODULE_GUARD_MAP.md` was not edited
(the coordinator updates the map centrally).

## Totals

**233 handlers guarded, 0 missing, 7 excluded.** Verifier:
`docs/integration/pilot4/c3_group_a_verify.py` (adapted from pilot 3's
`c3_group_a_verify.py`, plus a read/write guard-kind check and a
double-guard check), re-run per module after the final edits.

| Module | Guarded (reads / writes) | Excluded | Missing | Order violations | Kind mismatches |
|---|---|---|---|---|---|
| academic | 91 (39 / 52) | 0 | 0 | 0 | 0 |
| compliance | 41 (18 / 23) | 0 | 0 | 0 | 0 |
| incidents | 36 (11 / 25) | 0 | 0 | 0 | 0 |
| surveys | 20 (9 / 11) | 2 | 0 | 0 | 0 |
| announcements | 15 (5 / 10) | 3 | 0 | 0 | 0 |
| providers | 15 (6 / 9) | 0 | 0 | 0 | 0 |
| email_templates | 11 (4 / 7) | 0 | 0 | 0 | 0 |
| app_sync | 4 (2 / 2) | 2 | 0 | 0 | 0 |
| **Total** | **233 (94 / 139)** | **7** | **0** | **0** | **0** |

Read/write split is by routed method (GET = read, POST/PUT/PATCH/DELETE
= write) and reconciles exactly with the guard map's per-module
GET / mutating-route counts in every module.

Count reconciliation vs the map: academic has **91** routed handlers,
not the 90 in the map table (39 GET + 52 mutating = 91; the map's
handler count is off by one — most likely `ai_capability` was missed
in the map count). All other modules match the map exactly
(compliance 41, incidents 36, surveys 22, announcements 18,
providers 15, email_templates 11, app_sync 6).

## Guard idiom (same as pilots 1–3)

Each module gained three helpers (in its helpers file, or
`validation.rs` for announcements, or `handlers.rs` for providers /
email_templates, where the auth helpers already live):

- `require_tenant(req)` — `TenantContext::from_http_request(req)`.
  No resolved tenant context → rejected (401; 403 on an invalid
  tenant id), never a fallback to the `public` schema.
- `caller_company(tenant, req)` — copied from the pilot-2 products
  helper: the tenant context's company first, the verified caller's
  server-resolved company (`unified_core::caller::caller_from(req)
  .company_id`) otherwise; 403 when both are present and disagree.
  The `X-Company-Id` header is NEVER read — no code in these 8
  modules reads it (the only mentions are doc comments saying so).
- `require_company(tenant, req)` — new in this pilot: `caller_company`
  plus a fail-closed 403 when no company resolves at all. This is the
  shared `CallerScope::Unresolved` rule applied to writes: an
  unresolved caller writes nothing, because gate-only records carry
  no company field to stamp, so there is no global-row write to fall
  back to.

Every gated handler resolves the guard FIRST, before caller identity
or any capability check:

- Reads (GET): `let _tenant = require_tenant(&req)?;` then
  `let _scope = caller_company(&_tenant, &req)?;`
- Writes (POST/PUT/PATCH/DELETE): `let _tenant =
  require_tenant(&req)?;` then `let _company =
  require_company(&_tenant, &req)?;`

Two insertion shapes, following the pilots: handlers whose auth lives
inside a `(|| { ... })()` / `(|| async { ... })()` result closure got
the guard as the first lines of that closure (200 handlers); handlers
whose auth runs at the top of the function body (`if let Err(error) =
authorize/require_permission(...)` or `let caller = match
caller(&req)`) got a match-form guard at the top of the body, in the
products `list_options` style (33 handlers: academic `ai_capability`;
all 15 gated announcements handlers; providers `list_kinds` /
`get_connection`; all 11 email_templates handlers; all 4 gated
app_sync handlers).

## Exclusions (7, public / non-tenant auth surfaces — NOT gated)

- surveys `get_public_survey`, `submit_response` — anonymous public
  response surface: the response token in the path IS the
  authorization, no `HttpRequest` param; the map already flags both
  as must-stay-ungated.
- announcements `list_public_announcements` — public by design
  (serves anonymous callers when the audience is `all`);
  `list_changelog`, `get_changelog` — explicitly PUBLIC
  (`RouteAuth::Public`, doc: "No auth is required; public access is
  gated by the API Manager's `ApiAccessRule` for this path"), no
  `HttpRequest` param.
- app_sync `sync_push`, `sync_pull` — HMAC device-token surface
  (`validate_device_token` on the body token), no `HttpRequest`
  param; possession of the device token IS the authorization, same
  rationale as the timetracker `agent_*` and storage_manager
  `redeem_download` exclusions.

## Files touched (33)

- academic: `helpers.rs`, `handlers_assessment.rs`,
  `handlers_catalog.rs`, `handlers_enrollment.rs`,
  `handlers_scheduling.rs`, `handlers_thesis.rs`, `ai.rs`
- compliance: `helpers.rs`, `breaches.rs`, `checklists.rs`,
  `consents.rs`, `deletions.rs`, `exports.rs`, `records.rs`,
  `retention.rs`
- incidents: `helpers.rs`, `handlers_incidents.rs`,
  `handlers_rotations.rs`, `handlers_policies.rs`,
  `handlers_postmortems.rs`, `handlers_auto_open.rs`
- surveys: `helpers.rs`, `handlers_distributions.rs`,
  `handlers_questions.rs`, `handlers_results.rs`,
  `handlers_surveys.rs` (`handlers_public.rs` untouched)
- announcements: `validation.rs`, `handlers_announcements.rs`,
  `handlers_changelog.rs`
- providers: `handlers.rs`
- email_templates: `handlers.rs`
- app_sync: `helpers.rs`, `handlers.rs`

No new dependencies, no record fields added, no push. The tracker and
the guard map were not edited.

## Tests

Four guard tests were added to each module's helper test module (new
`#[cfg(test)]` mod in `app_sync/helpers.rs`, which had none):
`require_tenant_fails_closed_without_resolved_context`,
`require_tenant_resolves_from_middleware_context`,
`require_company_fails_closed_without_a_resolved_company`,
`caller_company_refuses_a_verified_company_mismatch`. All are UNRUN
(see Verification below).

Note on the pre-existing email_templates handler tests: they assert
anonymous callers are rejected with 401 (`UNAUTHORIZED`). The tenant
gate also returns 401 for a request with no resolved tenant context
(it runs before `authorize`), so those assertions still hold in
principle — unrun, see below.

## Verification

- `docs/integration/pilot4/c3_group_a_verify.py` per module after
  the final edits: every module reports `missing=0`,
  `order_violations=0`, `kind_mismatches=0`, `double_guards=0`
  (see table above; the verifier also checks each guarded handler
  carries the guard exactly once, and that GET handlers resolve
  `caller_company` while mutating handlers resolve
  `require_company`).
- `rustfmt --edition 2024 --emit stdout` (parse-level) on all 33
  touched files: **33/33 parse OK** (`rustfmt` via
  `PATH="$HOME/.cargo/bin:$PATH"`).
- `rustfmt --edition 2024 --check` fails on all 33 files, as it did
  before this work — pre-existing import-ordering / style drift
  (e.g. unsorted imports, long lines). Whole-file formatting was NOT
  applied. After reformatting the new lines to rustfmt's preferred
  form (single-line match arm, test `assert!`/chain wrapping, wrapped
  brace-list imports, one import-order move), **zero `+`/`-` diff
  lines touch any new identifier** (`require_tenant`,
  `caller_company`, `require_company`, `_tenant`, `_scope`,
  `_company`).
- **NOT verified: the root-bin files were NOT typechecked
  (`cargo check -p unified` not run — the bin crate OOM-kills this
  VM) and the gates are UNRUN (`cargo test` not run). All new tests
  are UNRUN.** The `?`-in-closure insertion was checked by
  construction (every first closure is a `UnifiedResult` closure),
  and the match-form guards use the same error-response expression
  each handler file already uses, but a full typecheck is still
  owed by whoever can run it.

## Residuals (honest, not partitioned by the guard alone)

- All 8 modules are gate-only: their records carry no scope/company
  field, so the guard cannot filter or check individual records.
  Real per-record partitioning needs a `tenant_id` on the records —
  a C11 follow-up, out of scope here; no fields were added.
- Writes by a caller with no resolvable company (neither tenant
  context nor verified caller carries one) now get 403. That is the
  brief's stated semantics and the shared `Unresolved` rule, but it
  is a behaviour change for company-less platform admins writing in
  these modules — the task-315 (ControlPlaneGuard) decision, same
  caveat as pilot 3.
- Header trust remains open until task 317 elsewhere in the tree;
  nothing in these 8 modules trusts a header (verified: no code
  reads `X-Company-Id` here).
- The 7 excluded handlers stay ungated by design (see Exclusions);
  binding device sync tokens to a tenant is a C11 decision, as with
  the timetracker agent keys.
