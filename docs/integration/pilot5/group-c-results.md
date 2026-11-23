# C3 pilot 5, group C — results (tracker task 319)

Worker group C: `kyc`, `payments`, `phone` (guard work) plus
`library`, `files`, `studio` (verify-only). Date: 2026-11-23. No
push. Task 319 itself was NOT marked done (the coordinator decides
its final status).

`docs/integration/C3_MODULE_GUARD_MAP.md` was NOT edited.

## Totals

| Module | Guarded | Excluded | Missing | Scope-checked | Gate-only |
|---|---|---|---|---|---|
| kyc | 48 | 0 | 0 | 48 | 0 |
| payments | 39 | 4 | 0 | 39 | 0 |
| phone | 62 | 0 | 0 | 62 | 0 |
| **Guard total** | **149** | **4** | **0** | **149** | **0** |
| library (verify-only) | 65 | 0 | 0 | 65 | 0 |
| files (verify-only) | 7 | 0 | 0 | 7 | 0 |
| studio (verify-only) | 32 | 0 | 0 | 24 | 7 (+1 delegated) |

Routed handlers reconciled: kyc 48, payments 43 (39 guarded + 4
excluded), phone 62, library 65, files 7, studio 32.

## Count reconciliation against the guard map table

The guard map has no per-module rows for kyc, payments, or phone:
they are among the 20 coordinator-listed modules that never made it
into the map table (map line 5 lists them as coordinator-listed
guarded, which this work disproves for payments and partly for
kyc/phone — see below). The counts above are the routed counts from
the verifier (`.to(name)` rule), so the coordinator should take
them as the map rows: kyc 48, payments 43, phone 62.

## Guard idiom, and the two documented adaptations

**Adaptation 1 — kyc/phone guard name.** Both modules already had
the fail-closed tenant gate under the name `resolve_tenant`
(`kyc/helpers.rs:72`, `phone/helpers.rs:70`):
`TenantContext::from_http_request(req)`, 401 without resolved
context, never falls back to `public` — the same semantics as the
pilots' `require_tenant`. Renaming it at ~110 call sites would be
pure churn, so the gate was kept and the verifier accepts
`resolve_tenant(&req)` as the guard for these two modules
(`c3_group_c_verify.py`, `MODULES["kyc"/"phone"]["guard"]`).
Only the 3 handlers that lacked the call were touched (see
per-module detail).

**Adaptation 2 — no `caller_company` anywhere in this group.**
No record in kyc, payments, or phone carries a `company_id`
field, so there is nothing for a company-scope helper to check
a record against:

- kyc/phone records carry `tenant_id` (`kyc/types.rs`,
  `phone/types.rs`; e.g. every `KycCase` / `PhoneCall` row), and
  the tenant-scoped loaders enforce the partition:
  `tenant_match` (`kyc/helpers.rs:199`, `phone/helpers.rs:190`)
  reads a cross-tenant row as 404, never 403, so existence does
  not leak. The guard is therefore tenant-gate + tenant-scoped
  loaders, not company scope.
- payments records carry neither `company_id` nor `tenant_id`
  (`payments/records.rs`: intents, transactions, methods,
  subscriptions, split bills are keyed by `customer_id` /
  `connection_id` only). The guard is therefore the new
  fail-closed tenant gate (`payments/adapter.rs:622`,
  `require_tenant`, canonical copy of the elearning/mail helper)
  FIRST in every handler, plus the module's existing
  verified-caller ownership checks (`require_customer` /
  `list_customer_scope` on `customer_id`,
  `require_payments_admin`, `require_cards_capability`).
  No `X-Company-Id` header is read anywhere in this work.

Real per-tenant partitioning of payments records (a `tenant_id`
stamp on every record + list filters) is a C11 follow-up, exactly
like the pilot-4 gate-only residuals: the gate stops
tenant-less requests, but two tenants' records still share the
same in-memory stores and are separated only by customer
ownership. The same residual applies, in weaker form, to
kyc/phone: their records ARE tenant-stamped and loader-checked,
so they are genuinely partitioned by tenant.

Write strictness is recorded per module in the table below;
harmonization is task 315, NOT this task — no module's strictness
was changed to match another's.

## Per-module detail

### kyc — 48 guarded, 0 excluded, 0 missing

Already guarded before this task except for one handler: 47 of 48
routed handlers already called `resolve_tenant(&req)?` FIRST,
before `caller(&req)?`, and load through the tenant-scoped loaders
(`load_case` helpers.rs:206, `load_visible_case` :220 — a case is
readable by its subject, its submitter, or a manager, others read
404 — `load_document` :240, `load_document_type_by_code` :255,
`load_subject_type_by_code`, `load_context`, `load_instruction`,
`load_risk_level`). Lists filter by tenant (`state.rs`:
`current_settings`, `active_context`, `active_risk_levels` all
take `tenant_id`). Creates stamp the resolved tenant
(`handlers_cases.rs:173` `tenant_id: tenant_id.clone()`).

Gap fixed here (1 handler):

- `handlers_ai.rs:296` `ai_capability` had NO tenant gate: it
  called `caller(&req)` + `require_permission(PERM_KYC_VIEW)` +
  `ai::caller` + the AI runner directly. Added the
  `resolve_tenant` match block first (same shape as
  `verify_case` at handlers_ai.rs:68). The AI runner's own
  company resolution (`ai::resolve_capability_company`) is
  untouched — it is the ai module's business (pilot 3).

Tests: new `tenant_guard_tests` in `helpers.rs` (fail-closed
without context, resolves from middleware context).

### phone — 62 guarded, 0 excluded, 0 missing

Already guarded before this task except for two handlers: 60 of 62
routed handlers already called `resolve_tenant(&req)?` FIRST and
load through the tenant-scoped loaders (`load_call`
helpers.rs:197, `load_visible_call` :216 — caller, participant,
or manager, others read 404 — `load_queue`, `load_member`,
`load_waiter`, `load_rule`, `load_menu`, `load_node`,
`load_script`, `load_recording`, plus `load_queue_by_code` /
`load_menu_by_code` which query by `(tenant_id, code)`).
State accessors filter by tenant (`state.rs`:
`active_routing_rules`, `active_scripts`, `queue_by_code`,
`menu_by_code`).

Gaps fixed here (2 handlers):

- `handlers_ai.rs:63` `ai_capability` had NO tenant gate (same
  shape as kyc's — `caller` + `PERM_PHONE_VIEW` + `ai::caller` +
  runner). Added the `resolve_tenant` match block first.
- `handlers_signalling.rs:330` `phone_ws` called `caller(&req)`
  BEFORE `resolve_tenant(&req)` — an order violation (identity
  before guard). Reordered: `resolve_tenant` first, then
  `caller`, then the call load + tenant check (still 404 on a
  cross-tenant call) + participant check. No behaviour change
  beyond the order.

Tests: new `tenant_guard_tests` in `helpers.rs` (same two tests
as kyc).

### payments — 39 guarded, 4 excluded, 0 missing

The real guard work in this group. Before this task NO payments
handler called any tenant gate, and 7 routed handlers took no
`HttpRequest` at all, so they had no authentication of any kind:

- `get_connection` (masked connection view), `get_subscription`,
  `cancel_subscription`, `renew_subscription` (triggers a charge),
  `renew_due_subscriptions` (batch sweep charging every due
  subscription), `generate_schedule`, `list_webhook_events` —
  all unauthenticated by construction: anyone who could reach the
  route could read/cancel/charge any subscription or connection
  by id. `cancel_subscription` / `renew_subscription` are
  money-moving; `renew_due_subscriptions` charges every due
  subscription at once.

What was done:

- New `require_tenant` in `adapter.rs:622` (canonical fail-closed
  helper) + `replay_for_customer` in `handlers_catalog.rs`
  (idempotency replay guard, see below).
- `let _tenant = require_tenant(&req)?;` FIRST in all 39 guarded
  handlers, before `require_customer` / `require_payments_admin`
  / `list_customer_scope` / `require_cards_capability`.
- Re-signed to take `HttpRequest` + gated (7 handlers):
  `get_connection` (+ `require_payments_admin`, was unauthenticated),
  `get_subscription` (+ `require_customer` on the loaded
  subscription's `customer_id`, same pattern as `get_intent` /
  `get_transaction`), `cancel_subscription` (+ `require_customer`
  after load, before the provider call), `renew_subscription`
  (+ `require_customer` after load, before `renew_one`),
  `renew_due_subscriptions` (+ `require_payments_admin` — a
  batch sweep is an operator/scheduler endpoint, never a customer
  endpoint), `generate_schedule` (+ `require_customer` on the
  named `customer_id` — it creates a split bill when
  `input.create` is set), `list_webhook_events` (+
  `require_payments_admin` — processed provider event ids are an
  operator surface).
- All other guarded handlers keep their existing ownership checks
  unchanged (lists stay admin-sees-all / non-admin-sees-own via
  `list_customer_scope`; single-record reads/mutations stay
  `require_customer` on the record's `customer_id`).
- Test builders updated (pilot-1 pattern): `verified_request` /
  `verified_request_without_roles` in `handlers_catalog.rs`,
  `handlers_charges.rs`, `handlers_split_bills.rs`,
  `handlers_subscriptions.rs` now attach the middleware tenant
  context (`attach_tenant_context`), so existing auth tests still
  reach the handler's own checks. Bare-request tests (expecting
  401) are untouched and still expect 401 — now from the tenant
  gate instead of the caller gate.
- New tests (written, UNRUN — see Verification): `require_tenant`
  fail-closed/resolves + `replay_for_customer` own-vs-foreign
  (`handlers_catalog.rs` tests); `get_subscription` 401 without
  tenant / 403 for another customer / 200 for the owner
  (`handlers_subscriptions.rs` tests).

## Payments money-path leaks: closed vs remaining

Idempotency keys are scoped to `(connection, key)`, NOT to a
customer. Every replay lookup therefore could hand one customer's
transaction — amount, provider id, order/invoice — to another
customer who guessed or reused the key, even though the caller
had already passed `require_customer` for their OWN customer id.
Closed here with `replay_for_customer` (10 production call sites,
all returning a generic "already in progress" conflict for a
foreign replay, never the other customer's record):

| Site | File:line (after edit) | Check |
|---|---|---|
| `create_charge` fast replay | `handlers_charges.rs:45` | vs `input.customer_id` |
| `create_charge` claim-loser wait | `handlers_charges.rs:77` | vs `input.customer_id` |
| `confirm_intent` fast replay | `handlers_intents.rs:210` | vs `intent.customer_id` |
| `confirm_intent` claim-loser wait | `handlers_intents.rs:252` | vs `intent.customer_id` |
| `pay_order` fast replay | `handlers_checkout.rs:50` | vs `input.customer_id` |
| `pay_order` claim-loser wait | `handlers_checkout.rs:70` | vs `input.customer_id` |
| `refund_transaction` fast replay | `handlers_charges.rs:386` | vs `original.customer_id` |
| `refund_transaction` claim-loser wait | `handlers_charges.rs:407` | vs `original.customer_id` |
| `pay_split_part` fast replay | `handlers_split_bills.rs:381` | vs `customer_id` (= part payer) |
| `pay_split_part` claim-loser wait | `handlers_split_bills.rs:409` | vs `customer_id` (= part payer) |

Not changed, by design:

- `create_payout` (`handlers_charges.rs:769,779`): replay returns
  the recorded payout, but the endpoint is admin-only
  (`require_payments_admin`) and admins already see every
  customer's records via `list_customer_scope`, so there is no
  cross-customer leak beyond admin visibility.
- `renew_one` (`handlers_subscriptions.rs:125,170`): internal
  helper, not routed; its key is deterministic
  `renew:{subscription_id}:{period}` so a cross-customer
  collision requires the same subscription id.
- `webhook_receiver` (`handlers_webhooks.rs:518`): the
  signature-verified provider path; the lookup is replay
  deduplication, not a customer read.
- `wait_for_idempotency` itself (`handlers_catalog.rs`): the
  helper returns whatever the winner recorded; every HTTP call
  site above now checks the result before returning it.

Conflict-error (duplicate slug/SKU/bucket-name style) audit:
no global uniqueness check exists in payments — `create_connection`
does not check duplicate names, no slug/SKU/bucket field is
uniqueness-checked anywhere — so there is no conflict error that
leaks cross-customer existence. The `conflict` errors that do
exist are all in-use / status / amount errors on a record the
caller already named and owns (or, for `delete_connection`,
admin-only in-use checks: "has active subscriptions / intents /
transactions" reveals only that a connection the admin named has
history, not another customer's data).

Remaining (not closed here, no leak introduced):

- Guest checkout: `caller_owns` (`adapter.rs:497`) matches
  `owner == "guest"` for unauthenticated callers, so a request
  with tenant context but no verified caller can still create /
  read `guest`-owned records. Pre-existing design (guest
  checkout), preserved deliberately: the tenant gate still
  applies first, so guest access is now tenant-scoped, just not
  caller-scoped.
- Order ownership: `pay_order` / `create_charge` /
  `create_intent` verify the amount against a pending checkout
  for the named `order_id` but cannot verify the order belongs
  to the caller — the store owns the order and payments has no
  read path to it (the same limitation is already documented in
  `build_split_bill`). A caller who knows a victim's order id and
  total can pay for that order with their own card; the resulting
  `payment.captured` event marks the victim's order paid. This is
  a cross-module ownership question, not a tenant-gate question.
- `check_pending_checkout_amount` / pending checkouts are keyed
  by order id globally (same store-ownership limitation).
- Record-level tenant partitioning (a `tenant_id` stamp on
  payments records) is still a C11 follow-up — see Guard idiom.

## Exclusions (documented public / token surfaces, NOT gated)

- payments `list_providers`, `get_provider`,
  `get_provider_settings` — static provider catalogue (kinds,
  capabilities, currencies, settings field definitions). No
  tenant data, no record access. Same class as pilot 2's
  `list_methodologies` and pilot 3's `list_tools`. Route metadata
  says `Authenticated`, but the handlers take no `HttpRequest`
  and serve only static data; gating would add nothing.
- payments `webhook_receiver` — public route by design
  (`RouteAuth::Public` in `module.rs`): the provider signature is
  verified first against every enabled connection of the kind
  (`handlers_webhooks.rs:261`, 401 on verification failure), and
  the event id deduplicates redeliveries. Gating it on tenant
  context would break provider delivery (providers carry no
  tenant context). Same class as pilot 4's provider webhooks.

No kyc or phone handler is a public surface: every other routed
handler already required a verified caller.

## Write-strictness table

| Module | Variant | What it means here |
|---|---|---|
| kyc | tenant-gate + tenant-scoped loaders (404 cross-tenant) | `resolve_tenant` first; single-record loads go through `load_*` which 404s a row from another tenant; lists filter by `tenant_id`; creates stamp the resolved tenant. Reads: subject/submitter/manager only (`load_visible_case`). Writes: manager capability + tenant match. No company dimension exists. |
| phone | tenant-gate + tenant-scoped loaders (404 cross-tenant) | Same as kyc: `resolve_tenant` first; `load_visible_call` / `load_actionable_call` restrict reads/actions to caller, participants, or managers (others 404); queue/IVR/script/rule loads 404 cross-tenant; lists filter by `tenant_id`. No company dimension exists. |
| payments | tenant-gate-only at the tenant layer + verified-caller customer ownership | `require_tenant` first; then the pre-existing ownership layer: `require_customer` (single record / body customer must equal the verified caller id; `guest` still matches unauthenticated callers), `list_customer_scope` (admin sees all, others forced to own `customer_id`), `require_payments_admin` (connections, payouts, sweeps, webhook-event list), `require_cards_capability` (cards settings). Writes do NOT require a resolved company — there is no company field to verify against. |
| library | strict 403-no-company (via `CallerScope`) | `require_tenant` + verified-caller `caller_company` (403 on tenant/verified mismatch, header never trusted); `require_company_match` (body company must match scope, omitted adopts caller scope, unresolved may claim none); `require_record_in_company_scope` 403s out-of-scope records; lists filter with `in_company_scope` (company caller reads own + global, unresolved reads global only, platform reads all). |
| files | mismatch-only / scope-resolved, NO tenant gate | `authorize()` resolves `CallerScope` from verified caller, tenant context, or hook (403 on mismatch inside `resolve_caller_company` only when sources disagree — files' resolver takes the first source that resolves, it does not 403 on tenant/verified disagreement the way library's does); lists filter and single reads check `can_access_record`. A request with NO resolved tenant context is NOT rejected (see verification evidence). |
| studio | strict 403-no-company (via `CallerScope`) + tenant gate | `caller()` resolves tenant FIRST (401 without context) then verified caller; `resolve_caller_company` 403s when verified/tenant/hook sources disagree; `require_company_scope` / `scoped_read_company` enforce the scope on writes/reads; lists filter with `can_access_record`. |

## Library verification evidence (verify-only, no edits)

The map row is stale: library IS already `CallerScope`-converted.
Verified first-hand in the current tree:

- `library/auth.rs:73` `require_tenant` — canonical fail-closed
  gate (`TenantContext::from_http_request`).
- `library/auth.rs:95` `caller_company(tenant, req)` —
  verified caller company first, tenant-context company second,
  403 on mismatch; the `X-Company-Id` header is NOT a source
  (pinned by tests at auth.rs:297-354, incl. "ignores header
  without tenant company" → `Unresolved`).
- `library/auth.rs:124` `require_company_match` /
  `:135` `require_company_match_for`, `:160` `in_company_scope`,
  `:168` `require_record_in_company_scope` — the full pilot
  helper set.
- Verifier: **65 routed handlers, 65 guarded, 0 missing, 0
  order violations** — every handler calls `require_tenant`
  first, then `caller_company`, then a scope check or filter.
  (`handlers_catalog.rs`, `handlers_circulation.rs`,
  `handlers_holds.rs`, `handlers_admin.rs`; e.g.
  `handlers_catalog.rs:34-35` gate + scope, `:46` list filter.)
- Residual (pre-existing, in auth.rs docs): the maps carry no
  `tenant_id`, so records are partitioned by `company_id`, not
  by tenant; hook listeners carry no scope. Full per-tenant
  isolation lands with the Diesel writers.

## Files verification evidence (verify-only, no edits)

The map row is stale in one direction and still true in another.
Verified first-hand:

- Conversion IS real for scope: every one of the 7 routed
  handlers (`list_files`, `create_folder`, `get_file`,
  `upload_file` — two routes, one handler — `download_file`,
  `delete_file`, `move_file`) calls the shared `authorize()`
  (`files/handlers.rs:53`), which requires the verified caller +
  capability and returns the resolved `CallerScope`
  (`files/types.rs:301` `resolve_caller_company`: verified caller
  company first, tenant-context company second, hook third; a
  header value is never trusted — see the comment at types.rs
  :304-312). Lists filter with `can_access_record`
  (handlers.rs:79, :161, :357, :414, :463); creates stamp the
  resolved scope (`:139`, `:328`).
- **BUT the tenant gate is NOT present.** `authorize()` calls
  `caller(req)` (`files/types.rs:249`), which only does
  `require_caller` — it never calls
  `TenantContext::from_http_request` as a gate. The tenant
  context is read only as an *optional* scope source
  (`ResolvedTenantContext::from_request(req)` → `Option`,
  types.rs:315-320): a request with no resolved tenant context
  proceeds as `Unresolved` (global rows only) or `Platform`
  (control-plane admin), it is not rejected with 401. The
  verifier reports this as `no_tenant_gate=7/7` (see below).
  Files is therefore scope-resolved but NOT tenant-gated — unlike
  studio, whose `caller()` resolves the tenant first. This is a
  finding for the coordinator, not something this task edits
  (files/studio code was verify-only). Whether "CallerScope-
  converted" counts as guarded without the tenant gate is a
  task-315 / coordinator decision.

## Studio verification evidence (verify-only, no edits)

The map row is stale: studio IS already `CallerScope`-converted
AND tenant-gated. Verified first-hand:

- `studio/auth.rs:63` `resolve_tenant` — fail-closed gate.
- `studio/auth.rs:67` `caller()` — resolves the tenant FIRST,
  then the verified caller; the resolved context is kept on the
  `Caller` (`tenant` field).
- `studio/auth.rs:114` `guard()` = `caller()` + capability;
  `:208` `authorize()` = `guard()` + `:160`
  `resolve_caller_company` (verified caller, tenant context,
  `user.company_requested` hook; 403 when sources disagree; no
  header trust).
- Verifier: **32 routed handlers, 32 guarded, 0 missing, 0 order
  violations** — 24 scope-checked (`can_access_record` /
  `require_company_scope` / `scoped_read_company`), 7 gate-only
  (the record-type / system-field handlers via `guard()`:
  `list_field_types`, `list_record_types`,
  `create_record_type`, `get_record_type`,
  `delete_record_type`, `list_system_fields`,
  `create_system_field` — those rows carry no `company_id`),
  1 via delegation (`delete_definition` →
  `archive_definition`, verified by reading the delegating body).
- Residual (pre-existing, in auth.rs docs): the in-memory stores
  carry no `tenant_id`, so there is no per-record tenant filter
  beyond the company check; that is safe only while company ids
  are globally unique.

## Verification

Verifier: `docs/integration/pilot5/c3_group_c_verify.py`
(adapted from `docs/integration/pilot4/c3_group_a_verify.py`;
adaptations documented in its header: per-module guard shapes —
kyc/phone accept the pre-existing `resolve_tenant(&req)`,
payments uses `require_tenant(&req)` + ownership markers,
library/files/studio are verify-only with their own guard shapes,
files reports `no_tenant_gate` separately, studio counts the
`delete_definition` → `archive_definition` delegation).

Re-run after the final edits (exit 0 in every case):

```
$ python3 c3_group_c_verify.py kyc
kyc: guarded=48 (scope-checked=48, gate-only=0, delegated=0) missing=0 excluded=0 order_violations=0
$ python3 c3_group_c_verify.py phone
phone: guarded=62 (scope-checked=62, gate-only=0, delegated=0) missing=0 excluded=0 order_violations=0
$ python3 c3_group_c_verify.py payments list_providers get_provider get_provider_settings webhook_receiver
payments: guarded=39 (scope-checked=39, gate-only=0, delegated=0) missing=0 excluded=4 order_violations=0
$ python3 c3_group_c_verify.py library
library: guarded=65 (scope-checked=65, gate-only=0, delegated=0) missing=0 excluded=0 order_violations=0
$ python3 c3_group_c_verify.py files
files: guarded=7 (scope-checked=7, gate-only=0, delegated=0) missing=0 excluded=0 order_violations=0 no_tenant_gate=7
$ python3 c3_group_c_verify.py studio
studio: guarded=32 (scope-checked=24, gate-only=7, delegated=1) missing=0 excluded=0 order_violations=0
```

`rustfmt --edition 2024 --emit stdout` (parse-level, `PATH`
prefixed with `$HOME/.cargo/bin`) passes on all 15 touched
files. Whole-file `rustfmt --check` was NOT run (pre-existing
style in these files is not rustfmt-clean; formatting them would
swamp the diff — same call as the pilots).

NOT verified: the guards were NOT typechecked (`cargo check -p
unified` not run — the root bin crate OOM-kills this VM) and NOT
run (`cargo test` not run). All new tests below are written but
UNRUN here; state that plainly.

New tests written (unrun):

- `payments/handlers_catalog.rs` tests: `require_tenant`
  fail-closed without context / resolves from middleware
  context; `replay_for_customer` returns only the own
  transaction.
- `payments/handlers_subscriptions.rs` tests:
  `get_subscription_is_gated_and_owner_scoped` (401 without
  tenant, 403 for another customer, 200 for the owner).
- `kyc/helpers.rs` + `phone/helpers.rs`: `tenant_guard_tests`
  (`resolve_tenant` fail-closed / resolves).
- Test builders updated in `handlers_catalog.rs`,
  `handlers_charges.rs`, `handlers_split_bills.rs`,
  `handlers_subscriptions.rs` (`attach_tenant_context`).

## Files changed

Guard work (kyc/phone — 3 handlers):

- `src/modules/kyc/handlers_ai.rs` — `ai_capability`: tenant
  gate added first.
- `src/modules/kyc/helpers.rs` — `tenant_guard_tests` added.
- `src/modules/phone/handlers_ai.rs` — `ai_capability`: tenant
  gate added first.
- `src/modules/phone/handlers_signalling.rs` — `phone_ws`:
  `resolve_tenant` moved before `caller`.
- `src/modules/phone/helpers.rs` — `tenant_guard_tests` added.

Guard work (payments — 39 handlers, 2 helpers, test builders):

- `src/modules/payments/adapter.rs` — new `require_tenant`
  fail-closed helper.
- `src/modules/payments/handlers_catalog.rs` — gate in 6 admin
  handlers; `get_connection` re-signed (now takes `HttpRequest`,
  gate + admin); new `replay_for_customer`; test builder +
  3 new tests.
- `src/modules/payments/handlers_charges.rs` — gate in 7
  handlers; idempotency replay checks in `create_charge` (x2)
  and `refund_transaction` (x2); test builder updated.
- `src/modules/payments/handlers_intents.rs` — gate in 5
  handlers; replay checks in `confirm_intent` (x2).
- `src/modules/payments/handlers_methods.rs` — gate in 5
  handlers.
- `src/modules/payments/handlers_checkout.rs` — gate in
  `pay_order`; replay checks (x2).
- `src/modules/payments/handlers_split_bills.rs` — gate in 5
  handlers; `generate_schedule` re-signed (gate +
  `require_customer`); replay checks in `pay_split_part` (x2);
  test builder updated.
- `src/modules/payments/handlers_subscriptions.rs` — gate in
  `create_subscription` / `list_subscriptions`;
  `get_subscription` / `cancel_subscription` /
  `renew_subscription` / `renew_due_subscriptions` re-signed
  and gated (+ ownership / admin checks); test builder updated
  + 1 new test.
- `src/modules/payments/handlers_webhooks.rs` —
  `list_webhook_events` re-signed (gate + admin).
- `src/modules/payments/card_validation.rs` —
  `get_cards_settings`: gate first, before the capability check.

New files (not code changes to other modules):

- `docs/integration/pilot5/c3_group_c_verify.py`
- `docs/integration/pilot5/group-c-results.md` (this file)

Not touched: `docs/integration/C3_MODULE_GUARD_MAP.md`,
`library/*`, `files/*`, `studio/*`, any other group's modules.
No push. No new dependencies. All code/docs in English.
