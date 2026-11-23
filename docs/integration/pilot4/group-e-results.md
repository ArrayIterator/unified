# C3 Pilot 4 — Worker Group E results (tracker task 316)

Fail-closed tenant gate for 9 gate-only modules: procurement, vcs,
content, crm, ai_support, maintenance, webhooks, saved_views,
downloader. Work done 2026-11-23 in `/home/hatch/workspace/unified-push`.
No push. No new dependencies. All English.

## Verification status (read first)

- Every touched file passes a `rustfmt --edition 2024 --emit stdout`
  parse check (`rustfmt` from `$HOME/.cargo/bin`): **33 files, 0 parse
  failures**. After reflowing only the added lines to rustfmt's
  preferred form, **zero `rustfmt --check` diff lines touch any new
  identifier** (`require_tenant`, `caller_company`,
  `require_caller_company`); the remaining `--check` diffs are
  pre-existing style drift (unsorted imports, long lines), so no
  whole-file formatting was applied.
- Guard counts verified with
  `docs/integration/pilot4/c3_group_e_verify.py` (routed = names
  appearing in `.to(name)`; also checks the guard precedes the first
  identity/capability call, and the write rule below). All 9 modules
  report `missing=0`, `order_violations=0`,
  `write_rule_violations=0`.
- **Root-bin files were NOT typechecked** (`cargo check -p unified` /
  `cargo test` not run: the bin crate is ~5.7 GiB RSS and is OOM-killed
  on this VM). **All guards and all added/adjusted tests are UNRUN.**
- The coordinator owns `docs/integration/C3_MODULE_GUARD_MAP.md`; this
  file does not edit it. The coordinator decides task 316's final status.

## Counts

| Module | Guarded | Excluded | Missing | Routed | Reads | Writes |
|---|---:|---:|---:|---:|---:|---:|
| procurement | 31 | 0 | 0 | 31 | 12 | 19 |
| vcs | 20 | 3 | 0 | 23 | 10 | 10 |
| content | 20 | 0 | 0 | 20 | 6 | 14 |
| crm | 14 | 0 | 0 | 14 | 6 | 8 |
| ai_support | 12 | 0 | 0 | 12 | 4 | 8 |
| maintenance | 12 | 0 | 0 | 12 | 4 | 8 |
| webhooks | 12 | 0 | 0 | 12 | 5 | 7 |
| saved_views | 8 | 0 | 0 | 8 | 4 | 4 |
| downloader | 7 | 0 | 0 | 7 | 2 | 5 |
| **Total** | **136** | **3** | **0** | **139** | **53** | **83** |

Count reconciliation: the guard map lists vcs at 22 handlers, but the
code routes **23** (`RouteInfo` count in `vcs/module.rs` is 23, and 23
distinct `.to(name)` targets). The map's 22 is a miscount; 23 routed
is correct. Total routed is therefore 139, not the brief's ~138.

## Exclusions (3)

All three are in vcs (`webhooks.rs`), `RouteAuth::Public` in
`route_info`:

1. **`github_webhook`** — inbound GitHub webhook receiver.
   HMAC-SHA256 signature verified against the connection's configured
   secret (`verify_webhook_signature`); possession of a valid
   signature IS the authorization. No verified caller exists on this
   request and there is no tenant context to gate on; gating would
   403 every genuine provider delivery.
2. **`gitlab_webhook`** — inbound GitLab webhook receiver. Shared
   token verified the same way. Same reason as above.
3. **`bitbucket_webhook`** — inbound Bitbucket webhook receiver.
   Configured shared secret verified the same way. Same reason as
   above.

All three delegate to the shared `handle_webhook`, which is itself
unrouted and was not gated either (its callers are exactly these
three excluded receivers). Drafts imported by a webhook therefore
carry no tenant stamp; partitioning webhook-imported drafts is a
C11 follow-up, like every other record in this group.

## Guard idiom (all modules)

Every guarded handler calls `require_tenant(&req)?` FIRST, then
resolves the company scope from the VERIFIED caller
(`unified_core::caller::caller_from`); the `X-Company-Id` header is
never trusted and there is no fallback to `public`. A tenant/verifier
company mismatch is a 403. Each module carries the same helper block
(copied from the pilot-3 `hosting/helpers.rs` shape):

- `require_tenant(req)` — `TenantContext::from_http_request(req)`;
  no resolved tenant context is 401, never `public`.
- `caller_company(tenant, req)` — tenant-context company first,
  verified caller's server-resolved company otherwise; 403 on
  mismatch; `Ok(None)` when neither resolves one.
- `require_caller_company(tenant, req)` — `caller_company` plus a
  403 when the scope is `None`.

Gate semantics (per the task brief): no resolved tenant context
fails closed; a resolved tenant + company proceeds. **Reads**
resolve `caller_company` (mismatch check only — the records carry
no company field, so there is nothing to partition on read);
**writes** (POST/PUT/PATCH/DELETE) call
`require_caller_company`, so a write with no resolvable company
scope is refused with 403. The verifier enforces this per-handler
(write-rule column above).

Handler shapes (the gate adapts to each, never the other way round):

- Closure shape (procurement, ai_support, maintenance, webhooks,
  saved_views, downloader): the two gate lines are the first lines
  of the `(|| { ... })()` closure, before `caller(&req)?` /
  `actor_id(&req)?` / `authorize_connectors(&state, &req)?`.
  Maintenance uses the same shape inside its
  `if let Err(error) = (|| { ... })()` pre-check closures.
- Direct-authorize shape (vcs, content, crm): a `let tenant =
  match require_tenant(&req) { ... };` + `let _company = match
  caller_company / require_caller_company(&tenant, &req) { ... };`
  block inserted before the `if let Err(error) = authorize(...)`
  pre-check (or before the `let actor = match caller(&req)` /
  `let caller = match ai::caller(&req)` match in content's
  `create_post`, `create_page`, `ai_capability`).

## Per-module notes

### procurement — 31 guarded, 0 excluded, 0 missing

Helpers in `helpers.rs` (`pub(crate)`) + 4 guard tests. All 31
handlers are the uniform closure shape (`caller` + `require_permission`
with `PERM_PROCUREMENT_VIEW` / `PERM_PROCUREMENT_MANAGE`). No handler
tests exist in this module, so no fixture changes were needed.
**Residual:** vendors, purchase orders, lines, receipts and budgets
carry no company field — gate only (C11).

### vcs — 20 guarded, 3 excluded, 0 missing

Helpers in `helpers.rs` (`pub(crate)`) + 4 guard tests. The 20
gated handlers use the direct `authorize(&ctx, &req, PERM_VCS_VIEW |
MANAGE)` pre-check. Excluded: the 3 public HMAC/token webhook
receivers (see Exclusions). Unrouted helpers `provider_get`,
`fetch_releases` and `handle_webhook` are not gated (they run behind
a gated routed handler, or behind the excluded receivers).
**Residual:** connections, drafts, provider settings and sync history
carry no company field — gate only (C11).

### content — 20 guarded, 0 excluded, 0 missing

Helpers in `helpers.rs` (`pub(super)`, matching the file's existing
visibility) + 4 guard tests. Handlers are `pub(super) async fn`
(the verifier matches `pub(crate)` and `pub(super)`); all use the
direct `authorize(&req, &ctx, PERM_CONTENT_*)` pre-check except
`create_post` / `create_page` (match on `caller(&req)` first, then
`require_permission`) and `ai_capability` (uses `ai::caller` +
`ai::require_permission` with `ai::PERM_AI_USE`; the gate was added
before both, and `ai::resolve_capability_company` downstream is
untouched — header trust there is task 317's job, not this gate).
**Test fixtures updated (necessary, not cosmetic):** the
`verified_request` / `verified_request_without_roles` helpers in
`posts.rs`, `pages.rs` and `categories.rs` now attach a middleware
`TenantContext` (company `"company-1"`) and a verified caller with
the same company, so success-path tests still reach the permission
checks; anonymous tests still use bare requests and still expect
401 (the gate returns 401, same as the old caller failure).
**Residual:** posts, pages and categories carry no company field —
gate only (C11).

### crm — 14 guarded, 0 excluded, 0 missing

Helpers in `common.rs` (`pub(crate)`) + 4 guard tests. All 14
handlers use the direct `authorize(&ctx, &req, PERM_CRM_READ |
MANAGE)` pre-check. No handler tests exist, so no fixture changes
were needed. The `authorize` doc note in `common.rs` already records
that contacts/leads carry no owner field and tenant scoping needs a
schema change; the gate does not change that.
**Residual:** contacts, leads and activities carry no company field
— gate only (C11).

### ai_support — 12 guarded, 0 excluded, 0 missing

Helpers in `helpers.rs` (`pub(crate)`; the block is anchored after
`authorize_connectors`, since this module has no
`require_permission` — its capability fn is
`require_connector_manage`) + 4 guard tests. The three new helpers
are added to the explicit `pub(crate) use` re-export list in
`ai_support.rs` (handler files reach them via `use super::*`).
Closures start with `actor_id(&req)?` (`ask`, conversations,
`handoff`, `create_ticket`) or
`authorize_connectors(&state, &req)?` (connector handlers); the gate
is first in all 12. `ask` is a write (it creates a conversation and
messages), so it requires a resolved company.
**Test fixtures updated (necessary, not cosmetic):**
`verified_request` / `verified_request_without_roles` in
`handlers_ask.rs`, `handlers_connectors.rs` and
`handlers_conversations.rs` now attach the tenant context + company
(same shape as content). The `ask` tests use
`verified_request_without_roles` and expect 200/409; the fixture
company keeps the write rule satisfied.
**Residual:** conversations, messages and connectors carry no company
field (connectors are owner-scoped by user id, not by company) —
gate only (C11).

### maintenance — 12 guarded, 0 excluded, 0 missing

Helpers in `validation.rs` (`pub(crate)`, re-exported by the
`pub(crate) use self::validation::*;` glob in `maintenance.rs`) +
4 guard tests. All 12 handlers use the pre-check closure shape
(`caller` + `require_permission` with `PERM_MAINTENANCE_VIEW` /
`MANAGE`). `run_tick` (POST `/tick`) is a routed handler with no
internal callers — only the HTTP route invokes it — so gating it
cannot break a scheduler path; the module's own docs say the tick is
driven by pointing a scheduler job at the HTTP route (or the
`maintenance.check` hook, which does not call this handler).
`current_status` is a `PERM_MAINTENANCE_VIEW` read, not a public
status page, and is gated like the other reads.
**Residual:** windows and history carry no company field — gate only
(C11).

### webhooks — 12 guarded, 0 excluded, 0 missing

Helpers in `helpers.rs` (`pub(crate)`) + 4 guard tests; the three
new helpers are added to the `pub(crate) use self::helpers::{...}`
list in `webhooks.rs`. All 12 handlers are the uniform closure
shape. The unrouted `run_delivery` (delivery.rs) is a background
worker spawned from the gated `test_endpoint` / `redeliver`
handlers and takes no `HttpRequest`; it was not gated.
**Test fixtures updated (necessary, not cosmetic):**
`verified_request` / `verified_request_without_roles` in
`handlers.rs` now attach the tenant context + company.
**Residual:** endpoints and deliveries carry no company field —
gate only (C11).

### saved_views — 8 guarded, 0 excluded, 0 missing

Helpers in `helpers.rs` (`pub(crate)`) + 4 guard tests; the three
new helpers are added to the `pub(crate) use self::helpers::{...}`
list in `saved_views.rs` and to the explicit `use super::{...}`
import in `handlers.rs`. All 8 handlers are the uniform closure
shape. Views are already owner-scoped by user id (`require_view_owner`
/ `is_visible_to`); the gate adds the tenant edge on top.
`apply_view` is a GET and is treated as a read.
**Test fixtures updated (necessary, not cosmetic):**
`verified_request` / `verified_request_without_roles` in
`handlers.rs` now attach the tenant context + company; ownership
tests still distinguish users by user id, not by company.
**Residual:** saved views carry no company field — gate only (C11).

### downloader — 7 guarded, 0 excluded, 0 missing

Helpers are private `fn`s in `handlers.rs` itself (where `caller` /
`require_permission` already live as private fns) + 4 guard tests.
All 7 handlers are the uniform closure shape. The unrouted
`run_download` (engine.rs) is a background worker spawned from the
gated `create_job` / `retry_job` handlers and takes no
`HttpRequest`; it was not gated.
**Residual:** download jobs carry no company field — gate only (C11).

## Guard unit tests (4 per module, 36 total, UNRUN)

Appended to each helpers test module (same shape in all 9, with a
`tenant_request(company_id, caller_company)` builder following the
pilot-3 `hosting/helpers.rs` model):

- `require_tenant_fails_closed_without_resolved_context` — bare
  request is rejected.
- `caller_company_prefers_tenant_company` — tenant company wins
  when present; the verified caller's company is the scope when the
  tenant carries none.
- `caller_company_rejects_verified_caller_mismatch` — tenant and
  verified-caller companies that disagree are a 403.
- `require_caller_company_refuses_writes_without_a_resolved_company`
  — no company anywhere is a 403; a matching company resolves.

Note (correction, not a copy): pilot 3's `hosting` test of the same
name asserts the tenant company wins over a *different* verified
caller company — but the `caller_company` implementation errors on
any mismatch, so that assertion cannot pass as written (the pilot-3
tests are unrun, so it was never caught). The group-E tests assert
the behaviour the code actually implements: tenant company wins
when the caller agrees with it, and any disagreement is a 403.

## Files touched (33)

- procurement: `src/modules/procurement/helpers.rs`,
  `src/modules/procurement/handlers_lines.rs`,
  `src/modules/procurement/handlers_orders.rs`,
  `src/modules/procurement/handlers_receipts_budgets.rs`,
  `src/modules/procurement/handlers_vendors.rs`
- vcs: `src/modules/vcs/helpers.rs`,
  `src/modules/vcs/handlers_connections.rs`,
  `src/modules/vcs/handlers_drafts.rs`,
  `src/modules/vcs/handlers_providers.rs`
  (`src/modules/vcs/webhooks.rs` untouched — exclusions only)
- content: `src/modules/content/helpers.rs`,
  `src/modules/content/posts.rs`, `src/modules/content/pages.rs`,
  `src/modules/content/categories.rs`,
  `src/modules/content/ai_capabilities.rs`
- crm: `src/modules/crm/common.rs`,
  `src/modules/crm/handlers_activities.rs`,
  `src/modules/crm/handlers_contacts.rs`,
  `src/modules/crm/handlers_leads.rs`
- ai_support: `src/modules/ai_support.rs`,
  `src/modules/ai_support/helpers.rs`,
  `src/modules/ai_support/handlers_ask.rs`,
  `src/modules/ai_support/handlers_connectors.rs`,
  `src/modules/ai_support/handlers_conversations.rs`,
  `src/modules/ai_support/handlers_handoff.rs`
- maintenance: `src/modules/maintenance/validation.rs`,
  `src/modules/maintenance/handlers.rs`
- webhooks: `src/modules/webhooks.rs`,
  `src/modules/webhooks/helpers.rs`,
  `src/modules/webhooks/handlers.rs`
- saved_views: `src/modules/saved_views.rs`,
  `src/modules/saved_views/helpers.rs`,
  `src/modules/saved_views/handlers.rs`
- downloader: `src/modules/downloader/handlers.rs`
- This file: `docs/integration/pilot4/group-e-results.md`
- Verifier: `docs/integration/pilot4/c3_group_e_verify.py`

Not touched: `docs/integration/C3_MODULE_GUARD_MAP.md` (coordinator
merges), any other module, no new dependencies.

## Verification output

```
procurement: guarded=31 (reads=12, writes=19) missing=0 excluded=0 order_violations=0 write_rule_violations=0
content: guarded=20 (reads=6, writes=14) missing=0 excluded=0 order_violations=0 write_rule_violations=0
crm: guarded=14 (reads=6, writes=8) missing=0 excluded=0 order_violations=0 write_rule_violations=0
ai_support: guarded=12 (reads=4, writes=8) missing=0 excluded=0 order_violations=0 write_rule_violations=0
maintenance: guarded=12 (reads=4, writes=8) missing=0 excluded=0 order_violations=0 write_rule_violations=0
webhooks: guarded=12 (reads=5, writes=7) missing=0 excluded=0 order_violations=0 write_rule_violations=0
saved_views: guarded=8 (reads=4, writes=4) missing=0 excluded=0 order_violations=0 write_rule_violations=0
downloader: guarded=7 (reads=2, writes=5) missing=0 excluded=0 order_violations=0 write_rule_violations=0
vcs: guarded=20 (reads=10, writes=10) missing=0 excluded=3 order_violations=0 write_rule_violations=0
  EXCL: webhooks.rs:github_webhook
  EXCL: webhooks.rs:gitlab_webhook
  EXCL: webhooks.rs:bitbucket_webhook
```

`rustfmt --edition 2024 --emit stdout` parse check: 33 files, 0
failures. `rustfmt --check` diff lines touching a new identifier:
0 (after reflowing only the added lines). Root-bin files NOT
typechecked; guards and tests UNRUN (see header).

Cross-checks by grep: 82 `let _tenant = require_tenant(&req)?;`
closure gates + 22 `let _company = match caller_company(...)`
direct read gates + 32 direct write gates = 136 gated handlers;
`require_caller_company` call sites = 83 handler sites + 18 guard-test
calls; `require_tenant` appears 0 times in `vcs/webhooks.rs`.

## Residuals (honest, not partitioned by the guard alone)

- Every record in all 9 modules carries no company/tenant field, so
  the guard is a tenant edge only — a resolved tenant's callers can
  still read/write every record in the module. Real partitioning
  needs a `tenant_id` on the record (C11), as the brief states.
- Writes now require a resolved company: a verified platform admin
  with no company gets 403 on every mutating handler in these 9
  modules (reads still proceed on the tenant gate). Whether platform
  admins should write without a company is the task-315
  (ControlPlaneGuard) decision, same caveat as pilot 3's ai module.
- `content/ai_capability` still resolves its capability company via
  `ai::resolve_capability_company` downstream of the gate; header
  trust there is task 317's job.
- vcs webhook-imported drafts carry no tenant stamp (see
  Exclusions); a forged-but-validly-signed provider delivery is
  trusted by design (the secret is the credential).
- All 36 new guard tests and all 8 updated handler-test fixture
  files are UNRUN (root bin OOM-blocks `cargo test` on this VM).
