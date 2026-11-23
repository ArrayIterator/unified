# C3 Pilot 3 — Worker Group E results (tracker task 314)

Tenant edge guards for 11 modules: company, domains, domain_verification,
hosting, presets, portals, planner, workflows, advisor, risk, auditors.
Work done 2026-11-23 in `/home/hatch/workspace/unified-push`.
No push. No new dependencies. All English.

## Verification status (read first)

- Every touched file passes a `rustfmt --edition 2024 --emit stdout`
  parse check (`rustfmt` from `$HOME/.cargo/bin`).
- Guard counts verified with `/tmp/c3_verify.py` (counts routed
  `pub(crate|super) async fn` handlers whose body contains
  `require_tenant(&req)`; routed = names appearing in `.to(name)`).
- **Root-bin files were NOT typechecked** (`cargo check -p unified` /
  `cargo test` not run: the bin crate is ~5.7 GiB RSS and is OOM-killed
  on this VM). **All guards and all added/adjusted tests are UNRUN.**
- The coordinator owns `docs/integration/C3_MODULE_GUARD_MAP.md`; this
  file does not edit it. The coordinator decides task 314's final status.

## Counts

| Module | Guarded | Excluded | Missing | Routed |
|---|---:|---:|---:|---:|
| company | 11 | 0 | 0 | 11 |
| domains | 8 | 0 | 0 | 8 |
| domain_verification | 8 | 0 | 0 | 8 |
| hosting | 15 | 0 | 0 | 15 |
| presets | 17 | 0 | 0 | 17 |
| portals | 19 | 1 | 0 | 20 |
| planner | 18 | 0 | 0 | 18 |
| workflows | 14 | 1 | 0 | 15 |
| advisor | 8 | 0 | 0 | 8 |
| risk | 30 | 0 | 0 | 30 |
| auditors | 18 | 1 | 0 | 19 |
| **Total** | **166** | **3** | **0** | **169** |

## Exclusions (3)

1. **portals — `get_public_portal`** (`public.rs`): unauthenticated
   public portal render by design. The API Manager gate makes it public;
   it shows only menus with an empty role list. No tenant gate added.
2. **workflows — `webhook_trigger`** (`handlers.rs`): no `HttpRequest`
   parameter; token-authenticated (64-hex token, constant-time hash
   compare), like documents `download_signed` / storage `redeem_download`.
   No tenant gate added.
3. **auditors — `get_public_by_token`** (`handlers_links.rs`): public
   route (`GET /api/v1/sharing/public/{token}`). No identity headers, no
   capability check — the link token IS the credential, granting read-only
   access to exactly one scoped resource; gated by the API Manager
   `ApiAccessRule` for the path (default deny). No tenant gate added.

## Guard idiom (all modules)

Every guarded handler calls `require_tenant(&req)?` FIRST, then resolves
`caller_company` from the tenant context + the VERIFIED caller
(`unified_core::caller::caller_from`); the `X-Company-Id` header is never
trusted and there is no fallback to `public`. A tenant/verifier company
mismatch is a 403. List handlers filter with `in_company_scope` first;
single-record loads are followed by a scope check; create bodies are
verified against the caller scope (omitted body company adopts the caller
scope, never stored unscoped under a scoped caller; an explicit clear
stays a clear). Each module has its own helper block adapted to its
record shape (`Option<String>` company, `Option<Uuid>` company, or
required company):

- `require_tenant`, `caller_company`, `require_company_match[_uuid/_for/_str]`,
  `in_company_scope[_uuid/_str]`, `require_record_in_company_scope[_uuid/_str]`,
  plus parent-scope helpers where records are scoped through a parent
  (workflows runs/steps/subscriptions via workflow; risk plans/tasks via
  risk, instance items via instance; auditors sessions/activity via
  auditor account, access-log entries via public link).

### Scope-semantics note (deviation from the task prose, in the pilot direction)

The task prose says "company callers read own + global only". The
reference implementations (pilot 1/2 helpers, copied exactly here) are
stricter: with a caller scope, only records carrying exactly that company
match — unscoped (legacy/global) records are HIDDEN from scoped callers;
unresolved callers see global records only. This is also stricter than
`CallerScope::can_access_record` in `unified_core::tenancy`, which would
let a Company caller read global rows. The stricter idiom was kept
deliberately (fail-closed for legacy unscoped rows). If product wants
scoped callers to see global rows, that is a one-line change per module
in `in_company_scope` — but it must be a conscious decision, not an
accident of this pilot.

## Per-module notes

### company — 11 guarded, 0 excluded, 0 missing

Gate-only. Records ARE the scope (companies/sites), so a `company_id`
filter is circular. Guard is `require_tenant` + verified caller on all
11 auth closures (`handlers.rs`, gate inserted as the first line of each
closure; `requests.rs` carries the gate + 2 tests with a doc noting the
gate-only shape and the task-315 control-plane note).
**Residual:** there is no per-record company check here by construction;
who may manage which company is a control-plane decision (task 315).

### domains — 8 guarded, 0 excluded, 0 missing

`Domain.company_id: Option<Uuid>`. Helpers in `state.rs` (Uuid shape)
+ 5 tests. List filter; create verifies the body company (adopts scope)
and uses the resolved id for the `company_exists` check; get scope-checks
both the DB and store paths; update/delete/ssl_renew check after load;
create_subdomain checks the parent scope; `verify_domain` (non-closure
auth) gates first, resolves scope after permission, checks after load.

### domain_verification — 8 guarded, 0 excluded, 0 missing

Tokens carry `tenant_id: Option<Uuid>` + `company_id: Option<Uuid>`;
attempts carry no company (scoped via parent token). Helpers in
`helpers.rs` (Uuid shape) + `require_body_tenant_match` (body tenant must
equal the resolved tenant; the stored value is the body's own, never
adopted) + 6 tests. Get/update_settings are gate-only (global settings);
issue_token verifies body company (adopts scope) + body tenant and
scope-filters the supersede loop; list_tokens filters raw tokens before
mapping to the view (the view strips company); revoke_token checks after
load; verify_domain_alias gates at the handler top (before the
`DOMAIN_VERIFICATION_CTX` stash), resolves scope inside the async
closure, checks token scope, keeps the existing body-vs-token ownership
guards; list_attempts retains via parent token (missing token hidden,
fail closed); get_status checks current token scope before reporting.
Note: the map counts 9 handlers; the 9th (`check_once`) is an unrouted
helper, so 8 routed is correct.

### hosting — 15 guarded, 0 excluded, 0 missing

Accounts carry `company_id: Option<Uuid>` (10 handlers scope-checked);
plans carry NO company field (5 handlers gate-only — shared catalogue).
Helpers in `helpers.rs` (Uuid shape) + `require_company_match_uuid_update`
(`None` = no change, `Some(None)` = explicit clear stays clear,
`Some(Some(id))` must equal scope) + 7 tests. `transition_account`
gained a `scope: &Option<String>` parameter with a scope check after
load; it is called only by the 4 transition handlers (activate /
suspend / unsuspend / cancel), each now resolving tenant + scope before
auth.

### presets — 17 guarded, 0 excluded, 0 missing

`Preset` definitions carry NO company (6 handlers gate-only:
list/create/get/update/clone/archive). `PresetApplication.company_id:
Uuid` (required) and `WizardState.company_id: Uuid` (required) are
scope-checked. `ActorInput.company_id` is untrusted body data, never a
scope source. Helpers in `identity.rs` (all `pub(super)`) + 4 tests:
required-Uuid variants (`in_company_scope_uuid` — scoped sees own only,
unresolved sees none) and `resolve_preview_company` (query company must
equal scope; no/unparseable query falls back to caller scope; unresolved
gives None). apply verifies the required body company (unresolved →
403); preview resolves the effective company via the helper;
list_applications filters by scope; get_application checks after load;
wizard start verifies the body company; all 6 path-company parse blocks
now parse + verify against scope (path company must equal scope;
unresolved reaches no wizard).
**Pre-existing breakage (not fixed, out of scope):** the handler tests
in `preset_handlers.rs` / `wizard_handlers.rs` call async handlers
without `.await` in plain `#[test]` fns and build requests with no
tenant context — they cannot compile/pass under `cargo test` as written.
Flagged here; fixing them is separate work (unverifiable without
`cargo check` on this VM).

### portals — 19 guarded, 1 excluded, 0 missing

Records carry required `company_id: String`; menus carry no company
(scoped via parent portal); `PortalRecord` scoped by company + user.
Helpers in `helpers.rs` (required-String shape) + 5 tests. List filter;
create verifies the required body company; get/update/
update_domain_status/delete/analytics check after load; all 6 menu
handlers check the parent portal scope after `load_portal`; the 6
`public.rs` client handlers are guarded (`client_portal` gates + uses
`require_caller_company`; `client_portal_home` gates + scope; the 5
`client_*` list handlers gate before delegating to `client_records`).
**Removed:** the old `caller_company(req)` in `helpers.rs` — despite its
stale doc comment it already read the verified caller (it did not read
the header), but it was superseded by the tenant-aware helpers and was
unused after the change, so it was deleted.
Excluded: `get_public_portal` (see Exclusions).

### planner — 18 guarded, 0 excluded, 0 missing

Plans carry `company_id: Option<String>` (8 handlers scope-checked);
pools carry NO company field (7 pool handlers + 3 forecast/overcommit
handlers gate-only — utilisation samples and forecasts are pool-derived).
Helpers in `helpers.rs` (products-style `Option<String>` block) + 4
tests. List loop skips out-of-scope plans; create verifies the
normalized body company (adopts scope); get/delete/approve/activate/
expire check after load; update checks after load and verifies company
change (explicit clear stays clear; newly named must equal scope).

### workflows — 14 guarded, 1 excluded, 0 missing

`Workflow.company_id: Option<String>`; `WorkflowRun` has NO company_id
(scoped via parent workflow); `UpdateWorkflowRequest.company_id:
Option<Option<String>>` (explicit clear stays clear).
Helpers in `state.rs` (`Option<String>` shape) + `load_workflow_in_scope`
(load then scope-check; runs/steps carry no company, scoped via parent
workflow) + 5 tests. Gate + scope in the 12 closure handlers (VIEW 5x,
MANAGE 7x); run_workflow / retry_workflow_run (non-closure auth) gate
before the caller match and resolve scope after the permission check;
list_workflows filters by scope; create verifies the body company;
update verifies company change; get_workflow_run scope-checks the parent
workflow; retry scope-checks the parent workflow before the is_active
check; list_trigger_subscriptions only lists events of in-scope
workflows.
**Behavior change:** `get_workflow_run` previously returned a run with
`workflow: null` when its parent workflow was gone; it now returns 404
for an orphaned run (matching `retry_workflow_run`'s convention), and
403 when the parent workflow is out of scope.
Excluded: `webhook_trigger` (see Exclusions).

### advisor — 8 guarded, 0 excluded, 0 missing

Facts, snapshots, suggestions and rule runs carry `company_id:
Option<String>`; rule definitions are static (no company).
Helpers in `helpers.rs` (`Option<String>` shape) + 4 tests.
`list_rules` is gate-only (static definitions). `list_suggestions` /
`list_rule_runs` filter by scope; `get_suggestion` /
`acknowledge_suggestion` / `dismiss_suggestion` / `snooze_suggestion`
check after load.

#### Handler-level fix: `re_evaluate` (the `evaluate(None)` hole)

`state.rs:237` `evaluate` keeps its `None`-means-all shape UNTOUCHED —
`hooks.rs:177` deliberately calls `evaluate(None, ...)` for the
system-wide licensing-expiry sweep. The fix is at the handler
(`handlers_suggestions.rs`, `re_evaluate`):

- Scope is resolved first; **an unresolved caller may never evaluate**
  (403) — previously an unresolved caller omitting `company_id` would
  evaluate suggestions for ALL companies.
- The body company is verified against the scope
  (`require_company_match_for`); a body company other than the caller's
  scope is refused; an omitted body company adopts the caller scope.
- `evaluate` is now always called with `Some(&company_id)`.
- Each supplied CRM/subscription snapshot is verified against the
  resolved scope and stamped with it when omitted, so a snapshot naming
  another company can no longer pollute that company's facts
  (snapshots are stored per contact/subscription id).
- Observable change: the response `company_id` is now always a string
  (was `Option`, nullable when omitted).

### risk — 30 guarded, 0 excluded, 0 missing

`Risk`, `ChecklistInstance` and `RiskAlert` carry `company_id:
Option<String>`. `MitigationPlan` / `MitigationTask` carry NO company
(scoped via parent risk). `ChecklistTemplate` carries NO company
(shared catalogue — all 8 template handlers gate-only).
Helpers in `common.rs` (`Option<String>` shape) + parent-scope helpers
(`require_risk_in_scope`, `require_plan_risk_in_scope`,
`require_task_risk_in_scope`, `require_instance_in_scope`) + 4 tests.
`viewer_of` / `actor_of` already read the verified caller
(`unified_core::caller::caller_from`) despite their stale "request
headers" doc comments — no change needed there.
- Risks: list filter; create verifies body company; get/update/delete
  check after load (delete checks before the cascade removal);
  `risk_matrix` resolves an *effective* company filter via
  `require_company_match_for` — a scoped caller without a filter sees
  only their own company's risk ids, an unresolved caller only global
  ones; a filter naming another company is refused. The response
  `company_id` now echoes the effective filter, not the raw query.
- Plans/tasks: parent-risk scope check on every handler (list/create/
  get/update/delete, incl. task handlers via `task.risk_id`).
- Instances: create verifies body company + linked-risk scope;
  list filter; get checks after load; complete/reopen check the parent
  instance before mutating the item.
- Alerts: list filter by scope.

#### Handler-level fix: `review_check` (the `run_review_check(None)` hole)

`store.rs:207` `run_review_check` keeps its `None`-means-all shape
UNTOUCHED — its own tests at `:348,:350` pin it and a system path may
rely on it. The fix is at the handlers:

- `handlers_plans.rs:416` `review_check`: scope resolved first;
  **an unresolved caller may never run the scan** (403); the body
  company is verified against the scope (omitted adopts it); the scan
  is now always called with `Some(&company_id)`. The response
  `company_id` is now always a string (was `Option`, nullable).
- `handlers_instances.rs:299` is NOT a second call site of the scan:
  that handler (`complete_instance_item`) does not call
  `run_review_check`; the map's line reference points at the
  `complete_instance_item` handler, which is guarded with the standard
  idiom (parent-instance scope check before mutation). The only
  `run_review_check` call from a handler is `review_check` above; the
  other caller is the `on_review_check_requested` hook handler (system
  path, untouched).
- `risk_matrix` (above) is the second read path that could leak
  cross-company ids; it is fixed via the effective company filter.

### auditors — 18 guarded, 1 excluded, 0 missing

`AuditorAccount` carries NO company; its boundary is its scope entries
(`AuditorScopeEntry.company_id: Option<Uuid>`, stored per account in
`state.scopes`). `PublicLink` carries NO company — the target record's
company lives in the owning module, so the creator is the company proxy.
Helpers in `helpers.rs` (Uuid shape, scope kept as `Option<String>` like
domains) + `account_in_scope` / `require_account_in_scope` (account
visible iff at least one scope entry is in scope) +
`require_scope_entry_company` + `link_in_scope` /
`require_link_in_scope` (creator-based) + 4 tests. New helpers are
re-exported from `src/modules/auditors.rs`.
- create_auditor: every granted scope entry is verified — a scoped
  caller may only grant scopes naming exactly their own company (a
  global entry would grant audit power over every company and is
  refused); an unresolved caller (platform control plane) may grant any
  scope.
- list_auditors: accounts filtered by scope-entry visibility (existing
  `company_id` query filter kept as an additional narrowing filter);
  get/revoke/extend check the account scope after load (revoke/extend
  check BEFORE the write-guard block, per the deadlock note in the
  file); sessions/activity/revoke_session check the parent account;
  list_active_auditor_sessions skips out-of-scope accounts.
- get_my_scope: gate-only (self-only read — the caller can only ever
  see their own account's scopes; no capability check by design).
- expire_sweep: gate-only (admin-triggered sweep over all accounts with
  `PERM_AUDITORS_MANAGE`; no per-record access).
- Links: list_links filters to links the scoped caller created;
  get/revoke/list_link_access_log check the parent link (revoke checks
  BEFORE the write guard); list_access_log entries are scoped through
  their parent link (entries with no link are platform-wide, visible to
  unresolved callers only); create_link and list_shareable_kinds are
  gate-only.
- **Residual:** `create_link` mints a link for a target record in
  another module; the target record's company is enforced by the owning
  module's `sharing.allow_check` hook, not by this module (the company
  is not visible here). A scoped caller holding `PERM_SHARING_CREATE`
  could mint a link for another company's record if that record's
  `allow_public_link` flag is set — the owning module would have to
  enforce caller-company there.
- **Test fixtures updated (necessary, not cosmetic):** the existing
  handler tests call handlers directly with a verified request that
  carried no tenant context, so the new gate would have failed them.
  `verified_request` in both handler test modules now attaches a
  `TenantContext` (platform admin, no company → unresolved scope), and
  `seed_account` in `handlers_auditors.rs` now seeds a global scope
  entry (no company) so the unresolved test admin sees the account
  through the account-scope check. These tests are UNRUN (see header).
Excluded: `get_public_by_token` (see Exclusions).

## Files touched

- company: `src/modules/company/requests.rs`,
  `src/modules/company/handlers.rs`
- domains: `src/modules/domains/state.rs`,
  `src/modules/domains/handlers.rs`
- domain_verification: `src/modules/domain_verification/helpers.rs`,
  `src/modules/domain_verification/handlers.rs`
- hosting: `src/modules/hosting/helpers.rs`,
  `src/modules/hosting/handlers_accounts.rs`,
  `src/modules/hosting/handlers_plans.rs`
- presets: `src/modules/presets/identity.rs`,
  `src/modules/presets/preset_handlers.rs`,
  `src/modules/presets/wizard_handlers.rs`
- portals: `src/modules/portals/helpers.rs`,
  `src/modules/portals/handlers.rs`, `src/modules/portals/menus.rs`,
  `src/modules/portals/public.rs`
- planner: `src/modules/planner/helpers.rs`,
  `src/modules/planner/handlers_plans.rs`,
  `src/modules/planner/handlers_pools.rs`,
  `src/modules/planner/forecast.rs`
- workflows: `src/modules/workflows/state.rs`,
  `src/modules/workflows/handlers.rs`
- advisor: `src/modules/advisor/helpers.rs`,
  `src/modules/advisor/handlers_suggestions.rs`,
  `src/modules/advisor/handlers_rules.rs`
- risk: `src/modules/risk/common.rs`,
  `src/modules/risk/handlers_risks.rs`,
  `src/modules/risk/handlers_plans.rs`,
  `src/modules/risk/handlers_instances.rs`,
  `src/modules/risk/handlers_templates.rs`
- auditors: `src/modules/auditors.rs`,
  `src/modules/auditors/helpers.rs`,
  `src/modules/auditors/handlers_auditors.rs`,
  `src/modules/auditors/handlers_links.rs`
- This file: `docs/integration/pilot3/group-e-results.md`

Not touched: `docs/integration/C3_MODULE_GUARD_MAP.md` (coordinator
merges), inbox (out of scope — needs a product decision), any other
module.

## Verification output

```
company: guarded=11 missing=0 excluded=0
domains: guarded=8 missing=0 excluded=0
domain_verification: guarded=8 missing=0 excluded=0
hosting: guarded=15 missing=0 excluded=0
presets: guarded=17 missing=0 excluded=0
planner: guarded=18 missing=0 excluded=0
advisor: guarded=8 missing=0 excluded=0
risk: guarded=30 missing=0 excluded=0
portals: guarded=19 missing=0 excluded=1
  EXCL: public.rs:get_public_portal guarded=no (excluded)
workflows: guarded=14 missing=0 excluded=1
  EXCL: handlers.rs:webhook_trigger guarded=no (excluded)
auditors: guarded=18 missing=0 excluded=1
  EXCL: handlers_links.rs:get_public_by_token guarded=no (excluded)
```

Every touched file: `rustfmt --edition 2024 --emit stdout` parse check
passed. Root-bin files NOT typechecked; guards and tests UNRUN (see
header).
