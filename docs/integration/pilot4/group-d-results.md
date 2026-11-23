# C3 Pilot 4 — Worker Group D Results (tracker task 316)

Fail-closed tenant gate for 8 gate-only modules: `field_service`,
`admissions`, `privacy`, `recruitment`, `newsletter`, `notifications`,
`bank_transfer`, `storage`. Local tree: `/home/hatch/workspace/unified-push`.
The guard map (`docs/integration/C3_MODULE_GUARD_MAP.md`) was NOT edited;
the coordinator merges all groups' rows.

## Summary

| Module | Routed | Guarded | Excluded | Missing |
| --- | --- | --- | --- | --- |
| field_service | 58 | 58 | 0 | 0 |
| admissions | 48 | 48 | 0 | 0 |
| privacy | 32 | 32 | 0 | 0 |
| recruitment | 32 | 29 | 3 | 0 |
| newsletter | 28 | 21 | 7 | 0 |
| notifications | 25 | 25 | 0 | 0 |
| bank_transfer | 17 | 15 | 2 | 0 |
| storage | 10 | 10 | 0 | 0 |
| **Total** | **250** | **238** | **12** | **0** |

Map-count discrepancy (verified by counting `.to(...)` route targets):
newsletter is truly 28 routed, not 29 as the map says. The 29th name is
`send_campaign` (`newsletter/sending.rs`), an unrouted helper called
from the campaign handlers, not a route target. No handler was found
unguarded, so the map overcounts rather than the work undercounting.

Verification script (re-runnable):
`docs/integration/pilot4/c3_group_d_verify.py` — adapted from pilot 3's
`docs/integration/pilot3/c3_group_a_verify.py`. Run as
`c3_group_d_verify.py <module> [excluded ...]`; exit 0 only when
`missing == 0`, `order_violations == 0`, and every guarded handler also
resolves `caller_company`. Final re-run output (2026-11-23):

```
field_service: routed=58 guarded=58 missing=0 excluded=0 order_violations=0 no_caller_company=0
admissions: routed=48 guarded=48 missing=0 excluded=0 order_violations=0 no_caller_company=0
privacy: routed=32 guarded=32 missing=0 excluded=0 order_violations=0 no_caller_company=0
recruitment: routed=32 guarded=29 missing=0 excluded=3 order_violations=0 no_caller_company=0
newsletter: routed=28 guarded=21 missing=0 excluded=7 order_violations=0 no_caller_company=0
notifications: routed=25 guarded=25 missing=0 excluded=0 order_violations=0 no_caller_company=0
bank_transfer: routed=17 guarded=15 missing=0 excluded=2 order_violations=0 no_caller_company=0
storage: routed=10 guarded=10 missing=0 excluded=0 order_violations=0 no_caller_company=0
```

## Guard idiom (all eight modules)

Same as pilots 1–3, gate-only variant. In every routed handler,
`require_tenant(&req)?` runs FIRST — before any identity or capability
call — then `caller_company(&_tenant, &req)?` is resolved from the
VERIFIED caller. Both helpers are added per module (same text in each,
copied from `company/requests.rs` for `require_tenant` and from
`products/helpers.rs` for `caller_company`):

- `require_tenant(req) -> UnifiedResult<TenantContext>` =
  `TenantContext::from_http_request(req)`: no resolved tenant context
  fails closed (401), never falls back to the `public` schema.
- `caller_company(tenant, req) -> UnifiedResult<Option<String>>`:
  tenant-context company first, server-resolved verified-caller company
  second, 403 on mismatch. The `X-Company-Id` header is never read.

Gate semantics applied: no resolved tenant context → fail closed;
a resolved tenant + company → proceed; a tenant/caller company
mismatch → 403 in every gated handler (reads and writes alike).

Design note on writes: these modules' records carry no `company_id` /
`tenant_id` field, so a write stamps no company and there is no body
company to verify — the gate-only analog of pilot 1's
`require_company_match` is therefore the `caller_company` resolution
itself (mismatch → 403), not a hard "every write must resolve a
company" rule. A hard company requirement would break legitimate
self-service writes whose actor is the data subject, not a company
member (an applicant submitting/withdrawing their own application,
a user granting/withdrawing their own consent, a caller acting on
their own notifications). Real partitioning of these records needs a
`tenant_id` on the record — a C11 follow-up, out of scope here; no
fields were added.

Handler shapes (two insertion modes, both verified for order):

- Closure handlers (`let result ... = (|| { ... })()`, the majority):
  the two `?` lines are the first statements of the result closure.
- Fn-top handlers (newsletter's `authorize` early-returns, storage's
  early-return `authorize` handlers, notifications' `match
  authenticate(...)` / `if let Err(require_admin(...))` handlers —
  these return `HttpResponse`, so `?` is unavailable): a match-style
  gate block right after `let debug = ...;`, returning the module's
  error response (`api_json::error_response` for newsletter,
  `json::error_response` elsewhere) before any identity call.

## Per-module detail

### field_service — 58 guarded, 0 excluded, 0 missing

- Helpers in `helpers.rs` (after `require_permission`):
  `require_tenant`, `caller_company`. All 10 handler files import
  both names via their explicit `use super::helpers::{...}` lists.
- All 58 handlers are result-closure shaped; gate inserted at the top
  of the first (result) closure. No handler needed a signature change.
- Files: `helpers.rs`, `handlers_definitions.rs`,
  `handlers_dispatch.rs`, `handlers_equipment.rs`,
  `handlers_equipment_types.rs`, `handlers_items.rs`,
  `handlers_parts.rs`, `handlers_plans.rs`, `handlers_templates.rs`,
  `handlers_time_signoff.rs`, `handlers_work_orders.rs`.

### admissions — 48 guarded, 0 excluded, 0 missing

- Helpers in `helpers.rs` (after `require_permission`):
  `require_tenant`, `caller_company`. All 12 handler files import
  both names via their explicit `use super::helpers::{...}` lists.
- All 48 handlers are result-closure shaped (several have nested
  `ok_or_else(|| ...)` closures; the gate went into the outermost
  result closure, which executes first). No signature changes.
- Files: `helpers.rs`, `handlers_applications.rs`,
  `handlers_checklist.rs`, `handlers_documents.rs`, `handlers_fees.rs`,
  `handlers_periods.rs`, `handlers_quotas.rs`, `handlers_results.rs`,
  `handlers_scores.rs`, `handlers_settings.rs`, `handlers_stages.rs`,
  `handlers_summary.rs`, `handlers_waitlist.rs`.

### privacy — 32 guarded, 0 excluded, 0 missing

- Helpers in `helpers.rs` (after `require_permission`):
  `require_tenant`, `caller_company`; the module root (`privacy.rs`)
  re-export list `pub(crate) use self::helpers::{...}` extended with
  both names, so the handler files' `use super::*;` resolves them.
- All 32 handlers are result-closure shaped with `caller(&req)` first;
  gate inserted at the closure top. `get_current_consent` /
  `get_current_notice` are authenticated self-service reads (they call
  `caller()`), so they are gated, not excluded.
- Files: `helpers.rs`, `privacy.rs` (root re-export),
  `handlers_anonymization.rs`, `handlers_consent.rs`,
  `handlers_erasure.rs`, `handlers_fields.rs`, `handlers_notices.rs`.

### recruitment — 29 guarded, 3 excluded, 0 missing

- Helpers in `helpers.rs` (after `require_permission`):
  `require_tenant`, `caller_company`; handler files use glob imports
  (`use super::{... helpers::* ...}`), so no import edits were needed.
- All 29 gated handlers are result-closure shaped; gate at closure top.
- Excluded (3): `apply_public` (anonymous application submission, no
  `HttpRequest` param), `list_public_jobs` and `get_public_job`
  (anonymous public job board, no `HttpRequest` param). Gating any of
  them would break the public careers surface; their anonymity is by
  design, matching the map note for `apply_public`.
- Files: `helpers.rs`, `handlers_applicants.rs`,
  `handlers_evaluations.rs`, `handlers_interviews.rs`,
  `handlers_jobs.rs`, `handlers_offers.rs`.

### newsletter — 21 guarded, 7 excluded, 0 missing

- Helpers in `state.rs` (after `authorize`, where `caller` /
  `require_permission` live): `require_tenant`, `caller_company`.
  `campaigns.rs`, `lists.rs`, `subscribers.rs`, `webhooks.rs` import
  both names via their explicit `use super::state::{...}` lists.
- All 21 gated handlers are fn-top `authorize(&req, &state, PERM)`
  early-return shaped; the match-style gate block was inserted after
  `let debug = state.is_debug();`, before `authorize`.
- Excluded (7): `subscribe`, `confirm_subscriber`,
  `confirm_subscriber_post`, `unsubscribe_via_link` (token-based
  subscriber self-service, no `HttpRequest` param; the confirmation /
  unsubscribe token IS the credential), `track_open`, `track_click`
  (tracking-pixel / redirect endpoints, no `HttpRequest` param; the
  tracking token IS the credential), `provider_webhook` (has
  `HttpRequest`, but authenticates by verifying the provider signature
  through the Email Providers hook
  `EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY` before any processing —
  a signature-authenticated webhook surface, not a caller-authenticated
  one; gating it on tenant context would break provider delivery
  reports). This matches the map note listing these as must-stay-ungated.
- Unrouted helper `send_campaign` (`sending.rs`) is not a route target
  and was not gated (called only from gated campaign handlers).
- Files: `state.rs`, `campaigns.rs`, `lists.rs`, `subscribers.rs`,
  `webhooks.rs`.

### notifications — 25 guarded, 0 excluded, 0 missing

- Helpers in `helpers.rs` (after `authenticate`): `require_tenant`,
  `caller_company`; the module root re-exports `helpers::*`, so
  `handlers.rs` (`use super::*;`) resolves them with no import edit.
- Shapes: 16 result-closure handlers (gate at closure top, before
  `authenticate` / `require_admin`), 3 early-return `require_admin`
  handlers (`list_audiences`, `list_templates`, `create_template`),
  4 `match authenticate(...)` handlers (`list_notifications`,
  `unread_count`, `list_deliveries`, `get_preferences`), and
  `send_digest` / `create_notification` (result closure after a few
  fn-top statements; gate at closure top, before `authenticate`).
  `require_admin` itself is defined in `handlers.rs` and delegates to
  `authenticate`, so gating before it covers the admin check.
- Files: `helpers.rs`, `handlers.rs`.

### bank_transfer — 15 guarded, 2 excluded, 0 missing

- Helpers in `service.rs` (after `require_admin`, where
  `admin_caller` / `require_admin` / `can_manage` live):
  `require_tenant`, `caller_company`; `handlers.rs` imports both via
  its explicit `use super::service::{...}` list.
- Four handlers took no `HttpRequest` and no auth at all; three were
  gated by adding a `req: HttpRequest` extractor param (actix injects
  it; `.to(handler)` routing is unchanged):
  `get_status` (aggregate counts), `get_settings` (module settings),
  `list_accounts` (full list incl. inactive accounts — the admin list,
  unlike the checkout list below). `get_settings` / `get_account`
  were result-direct (`let result = ...;`) and were converted to the
  result-closure shape so the `?` gate lines fit. `get_account`
  returns the masked `BankAccountView` and is the admin CRUD companion
  to `update_account` / `delete_account` (both `require_admin`), so it
  was gated rather than treated as public.
- Excluded (2): `list_accounts_for_currency` — documented in-code as
  "the list a shopper picks from at checkout" (masked, active-only);
  `create_verification` — unauthenticated checkout creation (takes
  caller-supplied `order_id` / `customer_id`, stores with actor
  `"system"`); gating either would break checkout. Both are public
  checkout surface by design.
- No handler in this module calls `caller()`; admin handlers use
  `require_admin`, owner handlers use `require_verification_owner` /
  `verification_visible_to_caller` / `can_manage` — the gate precedes
  all of them (0 order violations).
- Files: `service.rs`, `handlers.rs`.

### storage — 10 guarded, 0 excluded, 0 missing

- Helpers in `store.rs` (after `authorize`, where the verified-caller
  capability check lives): `require_tenant`, `caller_company`;
  `handlers.rs` imports both via `use super::store::caller_company;`
  / `use super::store::require_tenant;`.
- 5 result-closure handlers (gate at closure top) + 5 fn-top
  early-return `authorize` handlers (match-style gate block after
  `let debug`, before `authorize`). `signed_url` / `download_object`
  are authenticated `authorize` handlers, not token-redemption
  endpoints, so they are gated (unlike storage_manager's
  `redeem_download`, which was excluded in pilot 2).
- Files: `store.rs`, `handlers.rs`.

## Exclusions (all 12, documented)

| Handler | Module | Reason |
| --- | --- | --- |
| `apply_public` | recruitment | Anonymous application submission; no `HttpRequest`; anonymity by design |
| `list_public_jobs` | recruitment | Anonymous public job board; no `HttpRequest` |
| `get_public_job` | recruitment | Anonymous public job board; no `HttpRequest` |
| `subscribe` | newsletter | Token-based subscriber self-service; no `HttpRequest` |
| `confirm_subscriber` | newsletter | Confirmation token IS the credential; no `HttpRequest` |
| `confirm_subscriber_post` | newsletter | Same as `confirm_subscriber` (POST form variant) |
| `unsubscribe_via_link` | newsletter | Unsubscribe token IS the credential; no `HttpRequest` |
| `track_open` | newsletter | Tracking-pixel endpoint; no `HttpRequest`; tracking token IS the credential |
| `track_click` | newsletter | Tracking-redirect endpoint; no `HttpRequest`; tracking token IS the credential |
| `provider_webhook` | newsletter | Provider-signature authenticated (Email Providers hook verification); gating on tenant context would break provider delivery reports |
| `list_accounts_for_currency` | bank_transfer | Public checkout list (masked, active-only) |
| `create_verification` | bank_transfer | Unauthenticated checkout creation (actor `"system"`); gating would break checkout |

## Verification

- Guard counts: `c3_group_d_verify.py` re-run per module after the
  final edits — every module reports `missing=0`,
  `order_violations=0`, and every guarded handler resolves
  `caller_company` (`no_caller_company=0`). Excluded handlers were
  additionally checked to contain no gate (all 12 confirmed ungated).
- `rustfmt --edition 2024 --emit stdout` (parse-level) on all 48
  touched files: all parse clean. `rustfmt --edition 2024 --check`
  still fails on these files, but only on pre-existing style drift
  (unsorted `use` blocks, long lines) — after the reflow pass below,
  **zero `--check` diff lines mention a new identifier**
  (`require_tenant`, `caller_company`, `_tenant`, `_company`).
  Whole-file formatting was NOT applied (it would swamp the diff).
- Import resolution was audited by hand per file (explicit import
  lists / glob re-exports / `use super::*` chains) after the incident
  below; every gated handler's `require_tenant` / `caller_company`
  resolves through its module's helper file.
- NOT verified: the guards were NOT typechecked (`cargo check -p
  unified` not run — the root bin crate OOM-kills this VM at ~5.7 GiB
  RSS) and NOT run (`cargo test` not run). Root-bin files were NOT
  typechecked here and the gates are unrun.

## Incident during this work (fixed, not hidden)

The first rustfmt-conformance pass used a difflib-based script that
applied rustfmt's suggestions selectively. Difflib misaligned hunks
where rustfmt reorders the `use` block, and the script deleted the
entire `use super::helpers::{...}` import line in 6 field_service
handler files (`handlers_definitions.rs`, `handlers_equipment.rs`,
`handlers_equipment_types.rs`, `handlers_items.rs`,
`handlers_parts.rs`, `handlers_templates.rs`) and duplicated it in 8
others. The import-presence audit caught it: the 6 imports were
restored from a usage analysis (used identifiers ∩ helpers definitions,
cross-checked against the original import lists visible in the
pre-reflow diff output — all 6 match exactly), the 8 duplicates had
their second copy removed, and the affected statements were then
reflowed to rustfmt's exact form one statement at a time. Final state
re-verified: all 48 files parse, verifier counts unchanged
(238/12/0), zero identifier-touching `--check` diff lines, and every
handler file's helpers import is present exactly once. Pre-existing
duplicate `use` lines elsewhere in the tree (e.g. duplicate
`use super::state::...` / `use super::types::...` lines, and duplicate
`use crate::modules::ai;` in `ai_capabilities.rs` files) were left
alone.

## Residuals (honest, not partitioned by the guard alone)

- All eight modules are gate-only: their records carry no
  `company_id` / `tenant_id`, so a tenant's records are not
  partitioned from another tenant's by this guard — a caller with a
  resolved tenant context can read/write the module's shared store.
  Real partitioning needs a `tenant_id` on the records (C11).
- `bank_transfer` `create_verification` remains an unauthenticated
  write (caller-supplied `order_id` / `customer_id`, actor
  `"system"`); it is checkout surface by design, but it is a write
  with no caller identity at all, not just no tenant gate.
- `newsletter` `provider_webhook` trusts the Email Providers hook's
  signature verification; if that hook is unanswered the handler
  fails closed (verified in the handler body), but the tenant gate
  does not apply to it.
- Platform admins / callers with no resolvable company can read (the
  gate only requires a resolved tenant + a non-mismatching company),
  and write handlers do not require a resolved company (see the
  design note above). Whether a company-less caller may write is a
  task-315 (ControlPlaneGuard) decision, as in pilot 3.

## Files touched (48)

- field_service (11): `helpers.rs`, `handlers_definitions.rs`,
  `handlers_dispatch.rs`, `handlers_equipment.rs`,
  `handlers_equipment_types.rs`, `handlers_items.rs`,
  `handlers_parts.rs`, `handlers_plans.rs`, `handlers_templates.rs`,
  `handlers_time_signoff.rs`, `handlers_work_orders.rs`.
- admissions (13): `helpers.rs`, `handlers_applications.rs`,
  `handlers_checklist.rs`, `handlers_documents.rs`, `handlers_fees.rs`,
  `handlers_periods.rs`, `handlers_quotas.rs`, `handlers_results.rs`,
  `handlers_scores.rs`, `handlers_settings.rs`, `handlers_stages.rs`,
  `handlers_summary.rs`, `handlers_waitlist.rs`.
- privacy (7): `helpers.rs`, `privacy.rs` (root re-export),
  `handlers_anonymization.rs`, `handlers_consent.rs`,
  `handlers_erasure.rs`, `handlers_fields.rs`, `handlers_notices.rs`.
- recruitment (6): `helpers.rs`, `handlers_applicants.rs`,
  `handlers_evaluations.rs`, `handlers_interviews.rs`,
  `handlers_jobs.rs`, `handlers_offers.rs`.
- newsletter (5): `state.rs`, `campaigns.rs`, `lists.rs`,
  `subscribers.rs`, `webhooks.rs`.
- notifications (2): `helpers.rs`, `handlers.rs`.
- bank_transfer (2): `service.rs`, `handlers.rs`.
- storage (2): `store.rs`, `handlers.rs`.
- Plus this results file and `c3_group_d_verify.py` in
  `docs/integration/pilot4/`. No push, no new dependencies, all English.
