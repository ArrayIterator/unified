# C3 Module Guard Map

Worker D classification of the 129 modules the coordinator listed as unguarded
(149 total modules minus the 20 coordinator-listed guarded modules:
billing, cards, delivery_notes, entitlements, esg, exams, forums, inbox, kyc,
library, payments, phone, plm, referral, rental, sign, sms_marketing,
social_marketing, spreadsheets, store).

Date: 2026-11-23. Tree: `/home/hatch/workspace/unified-push`.

## Method (grep-based, no full-file reads)

Evidence commands (run from the tree root):

```sh
# Scope fields: struct-field occurrences + total mentions, per module
grep -rhoE "tenant_id|company_id" src/modules/<mod> --include="*.rs" | sort | uniq -c
grep -rhoE "(tenant_id|company_id)[[:space:]]*:" src/modules/<mod> --include="*.rs" | sort | uniq -c
# Mutating routes (RouteInfo method strings) + GET routes
grep -rhoE 'method: "(POST|PUT|PATCH|DELETE|GET)"' src/modules/<mod> --include="*.rs" | sort | uniq -c
# Handler inventory: pub(crate|super) async fn in handlers*.rs (plus all *.rs for the
# 13 modules with no handlers*.rs files); per-handler body scan for caller( /
# authorize( / actor_id( / resolve_tenant( / require_tenant( (see /tmp/c3_audit.py,
# /tmp/c3_audit2.py, /tmp/c3_auth.py, /tmp/c3_shape.py, /tmp/c3_routes.py)
# Guard-helper definition: grep -rn "fn resolve_tenant|fn require_tenant" src/modules/<mod>/
# Caller-resolves-tenant: read the module's caller() in helpers.rs / auth.rs / service.rs
#   / common.rs and check whether it calls resolve_tenant()/require_tenant() FIRST.
```

Kinds: **tenant-facing** = serves tenant business data over HTTP;
**control-plane** = serves the main site (tenants, users, roles, billing-admin,
settings, etc.); **infra-or-shared** = no tenant business data of its own
(cache, geo, network, public status pages, etc.). Control-plane and
infra-or-shared modules get NO guard; the reason is in the Notes column.

## Correction to the coordinator's premise (verified, not assumed)

9 of the 129 "unguarded" modules are in fact **already fully gated**: every
routed handler reaches the tenant gate, because the module's `caller()` helper
resolves the tenant FIRST (`resolve_tenant()` / `require_tenant()` /
`tenancy::TenantContext::from_http_request`, 401 without context, 403 on a
bad id) before caller identity or any capability check. The earlier
coordinator count treated "no handler literally calls `require_tenant`" as
"unguarded" and missed this convergence: approvals, calendar, expenses,
holidays, manufacturing, pricing, printing, quality, vault. Quality was the
subtlest case: 4 handlers look ungated by grep but delegate to shared helpers
(`transition_alert`, `set_point_active`) that call the tenant-resolving
`caller()` first — verified by reading those helpers. One more module,
saas_packages (control-plane), is also guarded in practice via
`require_tenant_scope()` + `require_control_plane()`, but it is control-plane
so it stays out of the pilot either way.

So: **120 modules are genuinely unguarded, 9 are already gated.**

## Summary counts

| Kind | Modules | Already gated | Genuinely unguarded |
|---|---|---|---|
| tenant-facing | 93 | 9 | 84 |
| control-plane | 22 | 0 | 22 (no guard by design) |
| infra-or-shared | 14 | 0 | 14 (no guard by design) |
| **Total** | **129** | **9** | **120** |

Pilot (Worker D, this round): 10 highest-risk tenant-facing modules —
chat, documents, gamification, logistics, timetracker, appointments, tickets,
tasks, forms, okrs. Risk = mutating routes + records already carrying a scope
field (`company_id`/`tenant_id`), so the guard is real partitioning, not
theater. Follow-up pilot 2 (2026-11-23, task 313) guarded projects (76 mutating
routes / 121 handlers), products (43 / 69), storage_manager (46 / 85,
SC62) — see Pilot 2 outcomes. Gate-only candidates (mutating handlers but
records carry NO scope field — the gate is real, partitioning needs a
`tenant_id` on records, a C11 follow-up): webinars, hr, field_service,
curriculum, and others marked "none" in the Scope column.

## Pilot outcomes (Worker D, 2026-11-23)

All 10 pilot modules are guarded. Totals: **346 handlers guarded, 0 missing,
19 excluded** (documented public / non-tenant auth surfaces, see below).
Guard order verified per module with `c3_order.py`: **0 order violations**
(no identity/capability call precedes the guard anywhere). Per-module
`c3_verify.py` re-run after the final edits: every module reports
`missing=0`.

Guard idiom (same in every pilot module, adapted to the record's company
shape): `require_tenant(req)` first in every tenant-data handler, then
`caller_company()` (tenant company first, `X-Company-Id` header fallback,
403 on mismatch); list handlers filter by `in_company_scope(...)`;
single-record loads are followed by `require_record_in_company_scope(...)`;
create bodies are verified with `require_company_match(...)` (omitted body
company adopts the caller scope, so a record is never stored unscoped under
a scoped caller); update bodies verify a newly named company, while an
explicit clear (`None`) stays a clear and is never re-stamped with the
caller scope. Company shapes: `Option<String>` (chat, documents,
gamification, appointments, tickets, forms), `Option<Uuid>` (logistics,
tasks), required `Uuid` (timetracker teams/staff/invites), required
`String` (okrs cycles/objectives).

| Module | Guarded | Excluded | Helpers (file / company shape) | Filters + checks added | Tests |
|---|---|---|---|---|---|
| chat | 46 | 4 (widget surface) | helpers.rs / Option<String> | 13 channel + 9 session record checks; filters on list_channels, list_support_sessions, list_queue; create_channel / create_support_session / save_widget_config body verify; update_channel nested company handled (explicit clear stays clear) | verified_request builders now attach tenant context; 2 require_tenant tests |
| documents | 31 | 1 (download_signed) | validation.rs / String | filters on list_documents, list_sessions, list_quotas (company-scope quotas only); 9 load_document checks; upload multipart + initiate_session body verify | tenant_request + fail-closed / resolve / scope tests |
| gamification | 38 | 0 | helpers.rs / Option<String> | filters on list_rules, list_ledger, list_seasons, list_reward_requests; load checks (rule x3, season x4, reward_request x4, ledger entry); create_rule / create_season / award_points body verify | fail-closed / resolve / scope tests |
| logistics | 54 | 1 (track_shipment) | helpers.rs / Option<Uuid> | filters on list_shipments, list_vehicles, list_warehouses, list_routes, list_rate_tables; 5 tail getters + 38 one-liner load checks; 5 create + 4 update body verifies | fail-closed / resolve / scope tests |
| timetracker | 61 | 7 (agent_*) | auth.rs / required Uuid (+ Opt variant) | No new filters needed: team/staff/invite handlers were ALREADY company-partitioned via staff_company() (header claim + hook membership check) with per-record company-equality checks and list filters; C3 adds the fail-closed tenant gate + tenant/header mismatch check | fail-closed / resolve / scope tests |
| appointments | 34 | 4 (public booking) | common.rs / Option<String> | filters on list_appointments, list_services; 22 load checks (appointment + its service); create_service / create_appointment + update_service body verify | fail-closed / resolve / scope tests |
| tickets | 29 | 0 | validation.rs / Option<String> | filter on list_tickets; 13 load checks + merge source/target checks; create_ticket body verify | 3 verified_request builders now attach tenant context; fail-closed / resolve / scope tests |
| tasks | 13 | 0 | validation.rs / Option<Uuid> (+ require_company_match_uuid) | filter on list_tasks; 8 mutation-load + get_task + list_comments checks; create/update body verify | 2 verified_request builders now attach tenant context; fail-closed / resolve / scope tests |
| forms | 18 | 2 (public form) | common.rs / Option<String> | filter on list_forms; 12 load_form checks; submissions checked via their parent form (4 checks; list_submissions via the parent form load); create/update body verify | fail-closed / resolve / scope tests |
| okrs | 22 | 0 | helpers.rs / required String (+ require_company_match_str) | filters on list_cycles, list_objectives; cycle/objective load checks (incl. get_cycle tail); key results / check-ins / links checked via parent objective (5 parent checks); create_cycle / create_objective body verify (objective already required company == cycle company; parent must be in the same cycle) | fail-closed / resolve / scope tests |

Residuals (honest, not partitioned by the guard alone):

- chat: `list_dms` has NO scope filter on purpose — DMs are stored with
  `company_id: None` by design and are membership-gated; filtering would
  hide all DMs from scoped callers. C11 should stamp DM company_id at
  creation if DM scoping is wanted. `chat_ws` is gate-only (subscription is
  query-filtered). PresenceEntry, SupportAgent, CannedResponse, ChatRating,
  OfflineMessage carry no company field — gate only.
- documents: AllowlistRule carries no company field — gate only.
  `quota_usage` already has `may_view_company_quota` logic — left alone.
- gamification: GamificationBadge, GamificationAward, GamificationReward
  carry no company field — gate only.
- logistics: Driver, RouteStop, ProofOfDelivery, DeliveryException,
  ShipmentTimelineEntry, MaintenanceReminder, StockMovement carry no company
  field — gate only. Shipment updates carry no company change (only
  vehicle/route/rate-table/warehouse updates do).
- timetracker: time entries, timers, activity samples and screenshots are
  user-scoped (no company id) — gate only; per-company entry partitioning
  is a C11 follow-up if wanted.
- appointments: StaffAvailability, AppointmentReminder, AppointmentSettings
  carry no company field — gate only.
- tickets: CannedResponse and EscalationRule carry no company field — gate
  only.
- forms: form submissions carry no company of their own — scoped through
  the parent form. Form fields carry none — scoped through the parent form
  load.
- okrs: key results and check-ins carry no company — scoped through the
  parent objective. Update bodies carry no company id, so there is no
  company change to verify on update.
- timetracker `agent_*` handlers and all other pilot exclusions stay
  ungated by design (see Pilot exclusions below); binding agent API keys
  to a tenant is a C11 decision.

Verification (final sweep, 2026-11-23): `rustfmt --edition 2024 --check`
fails on all 57 edited files — but every one of those files was already
NOT rustfmt-clean before this work (pre-existing style: unsorted imports,
long lines, double blank lines), so whole-file formatting was NOT applied
(it would swamp the diff). Instead, every diff line mentioning a new
identifier (`require_tenant`, `_tenant`, `_scope`, `in_company_scope`,
`require_company_match*`, `require_record_in_company_scope`,
`caller_company`, `TenantKind`, …) was checked: **zero such lines remain**
(after wrapping a handful of long asserts/signatures to rustfmt's preferred
form and reflowing one handler, timetracker `add_team_member`, whose
pre-existing over-long `let result` line rustfmt wants wrapped). Two
`caller_scope` / `resolve_reward_scope` matches in that grep are
pre-existing identifiers, not C3 code, and were left alone.
**NOT verified: the guards were NOT typechecked (`cargo check` not run,
OOM on the root bin) and NOT run (`cargo test` not run).**

## Table

| Module | Kind | Scope field (struct fields) | Mutating routes | Handlers (gated) | Auth pattern in handlers | Guard status | Notes |
|---|---|---|---|---|---|---|---|
| academic | tenant-facing | none (mentions T2/C4) | 52 (POST 26, PUT 14, PATCH 0, DEL 12; GET 39) | 91 (91 gated) | caller( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 91 guarded, 0 excluded. Map had 90; code routes 91 (likely ai_capability). |
| admissions | tenant-facing | none (mentions T0/C0) | 33 (POST 20, PUT 1, PATCH 6, DEL 6; GET 15) | 48 (48 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 48 guarded, 0 excluded. |
| advisor | tenant-facing | company_id x13 (mentions T0/C60) | 4 (POST 4, PUT 0, PATCH 0, DEL 0; GET 4) | 8 (8 gated) | require_capability( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). Handler-level fix: `re_evaluate` denies unresolved callers (body company verified, snapshots stamped with resolved scope); `evaluate` untouched — hooks.rs:177 licensing sweep relies on its None-means-all shape. |
| affiliates | tenant-facing | company_id x6 (mentions T0/C18) | 19 (POST 15, PUT 0, PATCH 1, DEL 3; GET 11) | 30 (29 gated, 1 excluded) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). 1 excluded: `track_click` (anonymous click tracking; referral code is the authorization, API Manager ApiAccessRule gate). |
| ai | tenant-facing | company_id x22 (mentions T0/C165) | 29 (POST 16, PUT 0, PATCH 7, DEL 6; GET 22) | 51 (51 gated) | caller( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). Policy + gating handlers switched from header-first `resolve_caller_company` to the verified Caller; task-312 fail-closed `require_company_scope` kept — platform admins with no company still get 403 on policy management (task 315). Usage list/get/summary scope-filtered with a documented `ai:read_usage` platform-reader carve-out. |
| ai_support | tenant-facing | none (mentions T0/C0) | 8 (POST 4, PUT 0, PATCH 2, DEL 2; GET 4) | 12 (12 gated) | actor_id( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 12 guarded, 0 excluded. Writes call require_caller_company (group E, strict). |
| analytics | tenant-facing | company_id x4 (mentions T0/C8) | 11 (POST 7, PUT 2, PATCH 0, DEL 2; GET 16) | 27 (27 gated) | authorize( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). `record_sale` verifies the body company (previously stored the claimed company verbatim); duplicate `order_id` check scoped to same-company records; scheduler hook runs with `&None` (global sales only). License/usage/uptime rows gate-only (no company field). |
| announcements | tenant-facing | none (mentions T0/C0) | 10 (POST 6, PUT 2, PATCH 0, DEL 2; GET 8) | 18 (15 gated, 3 excluded) | authorize( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 15 guarded, 3 excluded (list_public_announcements, list_changelog, get_changelog — public / RouteAuth::Public). |
| api_manager | control-plane | none (mentions T0/C1) | 16 (POST 7, PUT 5, PATCH 0, DEL 4; GET 16) | 30 (0 gated) | require_admin( | unguarded (identity-only, no tenant gate) | Platform API registry + keys; require_admin() in 29 handlers - control-plane. |
| apikeys | control-plane | none (mentions T0/C0) | 5 (POST 4, PUT 1, PATCH 0, DEL 0; GET 4) | 9 (0 gated) | require_admin( | unguarded (identity-only, no tenant gate) | Credential management; require_admin() in all handlers - control-plane. |
| app_sync | tenant-facing | none (mentions T0/C0) | 4 (POST 4, PUT 0, PATCH 0, DEL 0; GET 2) | 6 (4 gated, 2 excluded) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 4 guarded, 2 excluded (sync_push, sync_pull — HMAC device-token surface, no HttpRequest). |
| appointments | tenant-facing | company_id x11 (mentions T0/C29) | 22 (POST 13, PUT 0, PATCH 4, DEL 5; GET 16) | 38 (34 gated, 4 excluded) | require_permission( | GUARDED (C3-D pilot, see Pilot outcomes) | Public handlers guest_book / public_booking_lookup / list_public_services / list_public_service_slots must stay ungated (anonymous booking surface). PILOT (C3-D). |
| approvals | tenant-facing | company_id x10 (mentions T1/C37) | 14 (POST 9, PUT 0, PATCH 2, DEL 3; GET 11) | 25 (25 gated) | require_permission( | gated (caller resolves tenant first) | Already gated: every handler calls tenant-resolving caller() (helpers.rs:43-54); records carry company_id (SC10). |
| assets | tenant-facing | none (mentions T0/C0) | 18 (POST 14, PUT 0, PATCH 3, DEL 1; GET 10) | 28 (28 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 28 guarded, 0 excluded. Gate is require_tenant only (group C). |
| audit | control-plane | none (mentions T0/C0) | 0 (POST 0, PUT 0, PATCH 0, DEL 0; GET 3) | 3 (0 gated) | require_caller( | unguarded (identity-only, no tenant gate) |  |
| auditors | tenant-facing | company_id x6 (mentions T0/C17) | 7 (POST 7, PUT 0, PATCH 0, DEL 0; GET 12) | 19 (18 gated, 1 excluded) | caller( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). 1 excluded: `get_public_by_token` (public route; link token IS the credential, API Manager ApiAccessRule, default deny). Accounts scoped via scope-entry companies; links scoped by creator. |
| auth_manager | control-plane | none (mentions T0/C0) | 19 (POST 17, PUT 1, PATCH 0, DEL 1; GET 8) | 4 (0 gated) | caller( | unguarded (identity-only, no tenant gate) |  |
| backup | tenant-facing | company_id x7 (mentions T0/C38) | 13 (POST 10, PUT 1, PATCH 0, DEL 2; GET 7) | 20 (20 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). Closes the task-312 note at handler level (verified-caller `None` used to see every job/run); task-312 `job_visible`/`run_visible` kept as-is. |
| bank_transfer | tenant-facing | none (mentions T0/C0) | 9 (POST 6, PUT 2, PATCH 0, DEL 1; GET 8) | 17 (15 gated, 2 excluded) | require_admin( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 15 guarded, 2 excluded (list_accounts_for_currency — public masked checkout list; create_verification — unauthenticated checkout creation). 4 handlers re-signed to take HttpRequest (get_status, get_settings, list_accounts, get_account were unauthenticated). |
| bi | tenant-facing | company_id x19 (mentions T0/C91) | 10 (POST 5, PUT 0, PATCH 2, DEL 3; GET 11) | 21 (21 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). Header-first `resolve_caller_company` deleted; `engine.rs` `None` arms now global-only (were 'no filter'); task-312 `can_access_company`/`can_read_report` reused. |
| billing | tenant-facing | tenant_id x4; company_id x30 (mentions T19/C156) | 38 (POST 25, PUT 8, PATCH 0, DEL 5; GET 31) | 69 (69 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group A). 69 guarded, 0 excluded. 6 money leaks closed incl. unauthenticated subscriptions surface + from_order owner check. |
| cache | infra-or-shared | none (mentions T12/C0) | 8 (POST 6, PUT 0, PATCH 0, DEL 2; GET 3) | 11 (0 gated) | authorize( | unguarded (identity-only, no tenant gate) | get/set_tenant_entry handlers are tenant-keyed by an explicit tenant key, not by resolved context - infra cache, no guard. |
| calendar | tenant-facing | tenant_id x16; company_id x9 (mentions T107/C40) | 7 (POST 4, PUT 2, PATCH 0, DEL 1; GET 6) | 11 (11 gated) | require_permission( | gated (caller resolves tenant first) | Already gated: every handler calls tenant-resolving caller() (helpers.rs:67); records carry tenant_id (ST16) + company_id (SC9). |
| cards | tenant-facing | tenant_id x16; company_id x2 (mentions T68/C6) | 5 (POST 2, PUT 2, PATCH 0, DEL 1; GET 6) | 11 (11 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group A). 11 guarded, 0 excluded. No company field on records; mutations require_company (403 when none). |
| certificates | infra-or-shared | none (mentions T0/C0) | 16 (POST 13, PUT 3, PATCH 0, DEL 0; GET 11) | 16 (0 gated) | is_admin | unguarded (identity-only, no tenant gate) | TLS certificate lifecycle for domains; no identity check in handlers - infra, operator surface. |
| chat | tenant-facing | company_id x16 (mentions T0/C66) | 29 (POST 19, PUT 1, PATCH 4, DEL 5; GET 21) | 46 (46 gated, 4 excluded) | actor_id( | GUARDED (C3-D pilot, see Pilot outcomes) | PILOT (C3-D). Handlers use actor_id() (verified caller only); support-widget handlers get_widget_config / bootstrap_widget_session / poll_widget_session / create_offline_message take no HttpRequest and are public widget surface - excluded. |
| cloudflare | infra-or-shared | none (mentions T0/C0) | 8 (POST 4, PUT 3, PATCH 0, DEL 1; GET 10) | 19 (0 gated) | - | unguarded (identity-only, no tenant gate) |  |
| clusters | infra-or-shared | none (mentions T0/C0) | 21 (POST 14, PUT 3, PATCH 0, DEL 4; GET 20) | 41 (0 gated) | require_permission( | unguarded (identity-only, no tenant gate) | Cluster/node management; 38 handlers use require_permission() but no tenant concept - platform infra. |
| community | tenant-facing | none (mentions T0/C0) | 22 (POST 13, PUT 0, PATCH 5, DEL 4; GET 16) | 38 (36 gated, 2 excluded) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 36 guarded, 2 excluded (list_public_topics, get_public_topic — anonymous public surface). |
| company | tenant-facing | company_id x6 (mentions T0/C28) | 6 (POST 2, PUT 2, PATCH 0, DEL 2; GET 5) | 11 (11 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). Gate-only: records ARE the scope (companies/sites); a company_id filter is circular here. Per-company control is a task-315 decision. |
| compliance | tenant-facing | none (mentions T0/C0) | 23 (POST 16, PUT 0, PATCH 4, DEL 3; GET 18) | 41 (41 gated) | - | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 41 guarded, 0 excluded. |
| content | tenant-facing | none (mentions T0/C2) | 14 (POST 8, PUT 3, PATCH 0, DEL 3; GET 6) | 20 (20 gated) | - | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 20 guarded, 0 excluded. Writes call require_caller_company (group E, strict). |
| contracts | tenant-facing | none (mentions T0/C0) | 19 (POST 13, PUT 0, PATCH 3, DEL 3; GET 8) | 27 (24 gated, 3 excluded) | - | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 24 guarded, 3 excluded (get_signing_link, sign_via_link, decline_via_link — token-gated signing links). |
| crm | tenant-facing | none (mentions T0/C0) | 8 (POST 4, PUT 2, PATCH 0, DEL 2; GET 6) | 14 (14 gated) | authorize( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 14 guarded, 0 excluded. Writes call require_caller_company (group E, strict). |
| curriculum | tenant-facing | none (mentions T0/C2) | 47 (POST 21, PUT 0, PATCH 13, DEL 13; GET 25) | 72 (72 gated) | - | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 72 guarded, 0 excluded. |
| dashboards | control-plane | company_id x4 (mentions T0/C26) | 5 (POST 2, PUT 1, PATCH 0, DEL 2; GET 17) | 23 (0 gated) | require_capability( | unguarded (identity-only, no tenant gate) | tenant_scope referenced in service.rs (optional resolve, no fail-closed gate). |
| delivery_notes | tenant-facing | tenant_id x20; company_id x13 (mentions T138/C68) | 15 (POST 10, PUT 0, PATCH 3, DEL 2; GET 7) | 22 (22 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group A). 22 guarded, 0 excluded. Scope failure on a loaded note is 404 (no existence leak). |
| departments | tenant-facing | none (mentions T0/C0) | 9 (POST 4, PUT 2, PATCH 0, DEL 3; GET 7) | 16 (16 gated) | authorize( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 16 guarded, 0 excluded. |
| documents | tenant-facing | company_id x15 (mentions T0/C82) | 16 (POST 11, PUT 0, PATCH 2, DEL 3; GET 16) | 32 (31 gated, 1 excluded) | authorize( | GUARDED (C3-D pilot, see Pilot outcomes) | PILOT (C3-D). Handlers use authorize() (identity-only); download_signed has no HttpRequest (signed-URL token auth) - excluded. |
| domain_verification | tenant-facing | tenant_id x11; company_id x11 (mentions T33/C33) | 4 (POST 2, PUT 1, PATCH 0, DEL 1; GET 4) | 8 (8 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). |
| domains | tenant-facing | company_id x8 (mentions T0/C19) | 6 (POST 4, PUT 1, PATCH 0, DEL 1; GET 2) | 8 (8 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). |
| downloader | tenant-facing | none (mentions T0/C0) | 5 (POST 5, PUT 0, PATCH 0, DEL 0; GET 2) | 7 (7 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 7 guarded, 0 excluded. Writes call require_caller_company (group E, strict). |
| elearning | tenant-facing | company_id x5 (mentions T0/C16) | 32 (POST 17, PUT 1, PATCH 7, DEL 7; GET 26) | 58 (57 gated, 1 excluded) | - | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). 1 excluded: `verify_certificate` (public verification endpoint). `publish_course`/`archive_course` guarded via the shared `transition_course` (gates first). Paths have no company field (`get_path_progress` gate-only). |
| email_providers | tenant-facing | none (mentions T0/C0) | 8 (POST 5, PUT 2, PATCH 0, DEL 1; GET 5) | 13 (12 gated, 1 excluded) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 12 guarded, 1 excluded (list_catalog — static catalogue). Map had 12; code routes 13 (test_connection_send). |
| email_templates | tenant-facing | none (mentions T0/C0) | 7 (POST 5, PUT 1, PATCH 0, DEL 1; GET 4) | 11 (11 gated) | authorize( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 11 guarded, 0 excluded. |
| entitlements | tenant-facing | tenant_id x27; company_id x59 (mentions T111/C511) | 10 (POST 7, PUT 1, PATCH 1, DEL 1; GET 10) | 20 (20 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group A). 20 guarded, 0 excluded. Tenant surface strict via TenantScope::resolve; admin surface gate-only (task-315 decision). |
| esg | tenant-facing | tenant_id x43; company_id x1 (mentions T215/C3) | 18 (POST 8, PUT 0, PATCH 5, DEL 5; GET 15) | 33 (33 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group B). 33 guarded, 0 excluded. scope_type/scope_id refs unresolved against caller scope (C11). |
| events | tenant-facing | company_id x4 (mentions T0/C13) | 18 (POST 13, PUT 0, PATCH 2, DEL 3; GET 19) | 37 (32 gated, 5 excluded) | caller( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). 5 excluded: public feed handlers (`list_public_events`, `get_public_event`, `public_event_ics`, `public_calendar_ics`, `public_calendar_json`; API Manager ApiAccessRule). `publish_event`/`cancel_event`/`complete_event` guarded via the shared `transition_event` (gates first). |
| exams | tenant-facing | tenant_id x2; company_id x17 (mentions T7/C51) | 35 (POST 25, PUT 1, PATCH 4, DEL 5; GET 23) | 58 (58 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group B). 58 guarded, 0 excluded. Full company-scope layer (elearning model); in-memory stores not tenant-partitioned. |
| expenses | tenant-facing | tenant_id x2; company_id x19 (mentions T2/C91) | 22 (POST 11, PUT 1, PATCH 4, DEL 6; GET 19) | 41 (41 gated) | require_permission( | gated (caller resolves tenant first) | Already gated: every handler calls tenant-resolving caller() (helpers.rs:86-98); tenant_scope/require_company_scope used in 34 handlers. |
| extensions | control-plane | none (mentions T0/C0) | 3 (POST 3, PUT 0, PATCH 0, DEL 0; GET 6) | 9 (0 gated) | require_permission( | unguarded (identity-only, no tenant gate) |  |
| field_service | tenant-facing | none (mentions T0/C0) | 38 (POST 22, PUT 0, PATCH 8, DEL 8; GET 20) | 58 (58 gated) | caller( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 58 guarded, 0 excluded. |
| files | tenant-facing | company_id x8 (mentions T0/C27) | 4 (POST 3, PUT 0, PATCH 0, DEL 1; GET 3) | 7 (7 gated) | authorize( | GUARDED (task 319, see Task 319 outcomes) | TASK 319. Gate added in the shared authorize() funnel (handlers.rs) via new helpers.rs require_tenant, fail-closed; 7/7 gated, no_tenant_gate=0. Records carry no tenant_id (C11). |
| forms | tenant-facing | company_id x3 (mentions T0/C15) | 11 (POST 7, PUT 0, PATCH 2, DEL 2; GET 9) | 20 (18 gated, 2 excluded) | require_permission( | GUARDED (C3-D pilot, see Pilot outcomes) | PILOT (C3-D). get_public_form / submit_public_form are anonymous public-form surface - excluded. |
| forums | tenant-facing | tenant_id x53; company_id x1 (mentions T339/C1) | 28 (POST 19, PUT 0, PATCH 5, DEL 4; GET 23) | 51 (51 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group B). 51 guarded, 0 excluded. 5 public read handlers gated, not excluded; no company field (C11). |
| frontend | control-plane | none (mentions T0/C0) | 0 (POST 0, PUT 0, PATCH 0, DEL 0; GET 6) | 5 (0 gated) | - | unguarded (identity-only, no tenant gate) |  |
| gamification | tenant-facing | company_id x28 (mentions T0/C131) | 19 (POST 12, PUT 0, PATCH 4, DEL 3; GET 19) | 38 (38 gated) | - | GUARDED (C3-D pilot, see Pilot outcomes) | PILOT (C3-D). |
| geo | infra-or-shared | none (mentions T0/C0) | 5 (POST 3, PUT 1, PATCH 0, DEL 1; GET 4) | 10 (0 gated) | require_permission( | unguarded (identity-only, no tenant gate) |  |
| holidays | tenant-facing | tenant_id x25; company_id x17 (mentions T133/C33) | 15 (POST 7, PUT 0, PATCH 4, DEL 4; GET 12) | 27 (27 gated) | require_permission( | gated (caller resolves tenant first) | Already gated: every handler calls tenant-resolving caller() (auth.rs:67); records carry tenant_id (ST25) + company_id (SC17). |
| hosting | tenant-facing | company_id x8 (mentions T0/C26) | 10 (POST 6, PUT 2, PATCH 0, DEL 2; GET 5) | 15 (15 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). |
| hr | tenant-facing | none (mentions T0/C0) | 34 (POST 23, PUT 7, PATCH 0, DEL 4; GET 28) | 62 (62 gated) | caller( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 62 guarded, 0 excluded. Gate is require_tenant only (group C; see Pilot 4 outcomes on write strictness). |
| i18n | infra-or-shared | none (mentions T0/C0) | 5 (POST 4, PUT 0, PATCH 0, DEL 1; GET 5) | 10 (0 gated) | - | unguarded (identity-only, no tenant gate) |  |
| import_export | tenant-facing | company_id x9 (mentions T5/C132) | 13 (POST 9, PUT 0, PATCH 2, DEL 2; GET 14) | 27 (27 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). `can_view_company` strictness (`(None, _) => false`) kept. |
| inbox | tenant-facing | tenant_id x16; company_id x14 (mentions T130/C43) | 13 (POST 10, PUT 0, PATCH 2, DEL 1; GET 5) | 18 (18 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group B). 18 guarded, 0 excluded. No code changes (already guarded); tenant_visible STILL OPEN — product decision. |
| incidents | tenant-facing | none (mentions T0/C0) | 25 (POST 17, PUT 0, PATCH 4, DEL 4; GET 11) | 36 (36 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 36 guarded, 0 excluded. |
| knowledgebase | tenant-facing | company_id x8 (mentions T0/C58) | 9 (POST 4, PUT 0, PATCH 2, DEL 3; GET 8) | 17 (17 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). Old header/hook-fallback `caller_company_id` helper deleted; reads now filter by company. |
| kyc | tenant-facing | tenant_id x41; company_id x1 (mentions T243/C6) | 29 (POST 15, PUT 0, PATCH 7, DEL 7; GET 19) | 48 (48 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group C). 48 guarded, 0 excluded. 1 gap fixed: ai_capability had no gate. |
| library | tenant-facing | tenant_id x2; company_id x35 (mentions T8/C147) | 37 (POST 26, PUT 1, PATCH 5, DEL 5; GET 28) | 65 (65 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group C). 65 guarded, 0 excluded. Verify-only; CallerScope conversion real (auth.rs:73,95). |
| licensing | control-plane | company_id x12 (mentions T0/C39) | 23 (POST 17, PUT 3, PATCH 0, DEL 3; GET 15) | 12 (0 gated) | - | unguarded (identity-only, no tenant gate) |  |
| load_balancer | infra-or-shared | none (mentions T0/C0) | 0 (POST 0, PUT 0, PATCH 0, DEL 0; GET 0) | 29 (0 gated) | require_permission( | unguarded (identity-only, no tenant gate) |  |
| logistics | tenant-facing | company_id x18 (mentions T0/C69) | 35 (POST 21, PUT 0, PATCH 7, DEL 7; GET 20) | 55 (54 gated, 1 excluded) | - | GUARDED (C3-D pilot, see Pilot outcomes) | PILOT (C3-D). track_shipment is token-based public tracking - excluded. |
| logs | control-plane | none (mentions T0/C0) | 3 (POST 2, PUT 1, PATCH 0, DEL 0; GET 5) | 8 (0 gated) | authorize( | unguarded (identity-only, no tenant gate) |  |
| loyalty | tenant-facing | company_id x6 (mentions T0/C39) | 16 (POST 10, PUT 0, PATCH 3, DEL 3; GET 10) | 26 (26 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). |
| mail | tenant-facing | none (mentions T0/C2) | 4 (POST 3, PUT 1, PATCH 0, DEL 0; GET 3) | 7 (7 gated) | require_caller( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 7 guarded, 0 excluded. Map had 6; code routes 7 (ai_capability). 4 handlers re-signed to take HttpRequest (send_mail, test_mail, list_log, list_connectors were unauthenticated on the route). |
| maintenance | tenant-facing | none (mentions T0/C0) | 8 (POST 6, PUT 1, PATCH 0, DEL 1; GET 4) | 12 (12 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 12 guarded, 0 excluded. Writes call require_caller_company (group E, strict). |
| manufacturing | tenant-facing | company_id x4 (mentions T1/C8) | 23 (POST 14, PUT 1, PATCH 4, DEL 4; GET 23) | 42 (42 gated) | require_permission( | gated (caller resolves tenant first) | Already gated: every handler calls tenant-resolving caller() (helpers.rs:70); in-memory stores carry no tenant_id yet (helper comment) - residual. |
| marketing | tenant-facing | none (mentions T0/C0) | 33 (POST 19, PUT 7, PATCH 0, DEL 7; GET 21) | 54 (53 gated, 1 excluded) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 53 guarded, 1 excluded (unsubscribe — signed-token recipient path). Gate is require_tenant only (group C). |
| mcp | tenant-facing | company_id x4 (mentions T0/C14) | 5 (POST 2, PUT 2, PATCH 0, DEL 1; GET 7) | 12 (7 gated, 5 excluded) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). 5 excluded: `get_server_public` (public registry view), `list_tools`/`list_resources`/`list_prompts` (static catalogues, no req), `mcp_endpoint` (API-key token surface; per-server auth inside `dispatch_rpc`). |
| meetings | tenant-facing | none (mentions T0/C0) | 21 (POST 19, PUT 0, PATCH 1, DEL 1; GET 10) | 31 (31 gated) | actor_id( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 31 guarded, 0 excluded. Map had 30; code routes 31 (meetings_ws). Gate is require_tenant only (group C). |
| network | infra-or-shared | none (mentions T0/C0) | 2 (POST 2, PUT 0, PATCH 0, DEL 0; GET 2) | 4 (0 gated) | require_permission( | unguarded (identity-only, no tenant gate) |  |
| network_tools | infra-or-shared | company_id x13 (mentions T0/C77) | 24 (POST 19, PUT 2, PATCH 0, DEL 3; GET 9) | 33 (0 gated) | require_capability( | unguarded (identity-only, no tenant gate) |  |
| newsletter | tenant-facing | none (mentions T0/C0) | 16 (POST 10, PUT 3, PATCH 0, DEL 3; GET 12) | 28 (21 gated, 7 excluded) | - | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 21 guarded, 7 excluded (subscribe, confirm_subscriber, confirm_subscriber_post, unsubscribe_via_link, track_open, track_click — token-based, no HttpRequest; provider_webhook — provider-signature authenticated). Map had 29; send_campaign is an unrouted helper. |
| notifications | tenant-facing | none (mentions T0/C0) | 15 (POST 10, PUT 2, PATCH 0, DEL 3; GET 10) | 25 (25 gated) | is_admin | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 25 guarded, 0 excluded. |
| okrs | tenant-facing | company_id x8 (mentions T0/C36) | 13 (POST 8, PUT 0, PATCH 2, DEL 3; GET 9) | 22 (22 gated) | require_permission( | GUARDED (C3-D pilot, see Pilot outcomes) | PILOT (C3-D). |
| panels | tenant-facing | none (mentions T0/C0) | 10 (POST 8, PUT 0, PATCH 1, DEL 1; GET 11) | 20 (20 gated) | - | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 20 guarded, 0 excluded. Map had 14; code routes 20 (6 list_*_records from the list_records_handler! macro, gate inside the macro). Gate is require_tenant only (group C). |
| payments | tenant-facing | tenant_id x4; company_id x4 (mentions T11/C5) | 27 (POST 24, PUT 1, PATCH 0, DEL 2; GET 16) | 43 (39 gated, 4 excluded) | authorize( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group C). 39 guarded, 4 excluded. 7 no-req handlers re-signed (incl. money-moving cancel/renew); 10 replay leaks closed. |
| phone | tenant-facing | tenant_id x64; company_id x1 (mentions T336/C7) | 39 (POST 26, PUT 0, PATCH 6, DEL 7; GET 23) | 62 (62 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group C). 62 guarded, 0 excluded. 2 gaps fixed: ai_capability had no gate; phone_ws order. |
| planner | tenant-facing | company_id x5 (mentions T0/C15) | 10 (POST 6, PUT 2, PATCH 0, DEL 2; GET 8) | 18 (18 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). |
| plm | tenant-facing | tenant_id x35; company_id x4 (mentions T214/C10) | 17 (POST 9, PUT 0, PATCH 4, DEL 4; GET 12) | 29 (29 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group D). 29 guarded, 0 excluded. Already tenant-gated; C3 added canonically. |
| portals | tenant-facing | company_id x5 (mentions T0/C44) | 8 (POST 3, PUT 0, PATCH 3, DEL 2; GET 12) | 20 (19 gated, 1 excluded) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). 1 excluded: `get_public_portal` (unauthenticated public portal render by design; API Manager gate; only empty-role menus shown). Old `caller_company` helper removed (superseded). |
| pos | tenant-facing | company_id x4 (mentions T0/C10) | 21 (POST 13, PUT 1, PATCH 4, DEL 3; GET 14) | 35 (35 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). |
| presets | tenant-facing | company_id x25 (mentions T0/C163) | 11 (POST 10, PUT 0, PATCH 1, DEL 0; GET 6) | 17 (17 gated) | - | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). Pre-existing handler tests call async handlers without `.await` and with no tenant context — cannot compile/pass as written; flagged, not fixed. |
| pricing | tenant-facing | tenant_id x50 (mentions T323/C0) | 32 (POST 24, PUT 4, PATCH 0, DEL 4; GET 26) | 58 (58 gated) | require_permission( | gated (caller resolves tenant first) | Already gated: every handler calls tenant-resolving caller() (common.rs:116); resolve_tenant uses middleware TenantContext (403 when missing); require_same_tenant 404s cross-tenant records; records carry tenant_id (ST50). |
| pricing_tables | tenant-facing | company_id x8 (mentions T0/C37) | 27 (POST 18, PUT 5, PATCH 0, DEL 4; GET 22) | 49 (47 gated, 2 excluded) | caller( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). 2 excluded: `render_table` (public when published), `vendor_storefront` (no `HttpRequest`; approved-vendor storefront only). |
| printing | tenant-facing | tenant_id x17; company_id x17 (mentions T78/C64) | 15 (POST 10, PUT 3, PATCH 0, DEL 2; GET 8) | 23 (23 gated) | require_permission( | gated (caller resolves tenant first) | Already gated: every handler calls tenant-resolving caller() (auth.rs:87); records carry tenant_id (ST17) + company_id (SC17). |
| privacy | tenant-facing | none (mentions T0/C0) | 15 (POST 13, PUT 1, PATCH 0, DEL 1; GET 17) | 32 (32 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 32 guarded, 0 excluded. |
| procurement | tenant-facing | none (mentions T0/C0) | 19 (POST 11, PUT 0, PATCH 4, DEL 4; GET 12) | 31 (31 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 31 guarded, 0 excluded. Writes call require_caller_company (group E, strict). |
| products | tenant-facing | company_id x8 (mentions T0/C20) | 43 (POST 21, PUT 9, PATCH 0, DEL 13; GET 26) | 69 (69 gated) | require_permission( | GUARDED (C3 pilot 2, see Pilot 2 outcomes) | PILOT 2 (task 313). 51 scope-checked, 18 gate-only; 0 excluded. |
| projects | tenant-facing | company_id x14 (mentions T0/C17) | 76 (POST 43, PUT 15, PATCH 0, DEL 18; GET 45) | 121 (120 gated, 1 excluded) | viewer_role | GUARDED (C3 pilot 2, see Pilot 2 outcomes) | PILOT 2 (task 313). 120 guarded, 1 excluded (list_methodologies — static catalogue). |
| properties | tenant-facing | company_id x5 (mentions T0/C15) | 20 (POST 13, PUT 0, PATCH 4, DEL 3; GET 17) | 37 (33 gated, 4 excluded) | - | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). 4 excluded: `list_public_properties`, `get_public_property`, `get_public_plan_svg`, `get_public_plan_print` (anonymous; `public_handlers.rs` untouched). |
| providers | tenant-facing | none (mentions T0/C0) | 9 (POST 7, PUT 0, PATCH 0, DEL 2; GET 6) | 15 (15 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 15 guarded, 0 excluded. |
| quality | tenant-facing | tenant_id x31; company_id x9 (mentions T145/C21) | 17 (POST 11, PUT 1, PATCH 3, DEL 2; GET 15) | 28 (28 gated, 4 via delegation) | caller( | gated (caller resolves tenant first) | Already gated: 24 handlers call tenant-resolving caller() directly; the other 4 (acknowledge/resolve_alert, activate/deactivate_point) delegate to transition_alert/set_point_active, which call caller() first. Verified by reading the helpers. |
| records | control-plane | none (mentions T0/C0) | 1 (POST 1, PUT 0, PATCH 0, DEL 0; GET 6) | 7 (0 gated) | require_permission( | unguarded (identity-only, no tenant gate) |  |
| recruitment | tenant-facing | none (mentions T0/C1) | 21 (POST 15, PUT 0, PATCH 4, DEL 2; GET 11) | 32 (29 gated, 3 excluded) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 29 guarded, 3 excluded (apply_public, list_public_jobs, get_public_job — anonymous, no HttpRequest). |
| referral | tenant-facing | tenant_id x62; company_id x4 (mentions T298/C12) | 20 (POST 10, PUT 1, PATCH 4, DEL 5; GET 17) | 37 (36 gated, 1 excluded) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group D). 36 guarded, 1 excluded. 1 excluded: anonymous click_link (token hash is the credential). |
| rental | tenant-facing | tenant_id x28; company_id x4 (mentions T179/C10) | 18 (POST 11, PUT 0, PATCH 4, DEL 3; GET 13) | 31 (31 gated) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group D). 31 guarded, 0 excluded. Already tenant-gated; public listings guarded, not excluded. |
| risk | tenant-facing | company_id x11 (mentions T0/C44) | 18 (POST 9, PUT 4, PATCH 0, DEL 5; GET 12) | 30 (30 gated) | require_capability( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). Handler-level fix: `review_check` denies unresolved callers and always calls the scan with `Some(company)`; `run_review_check` shape preserved (tests :348,:350). The other caller is the system `on_review_check_requested` hook (untouched, system path). `risk_matrix` refuses a filter naming another company. |
| roles | control-plane | none (mentions T0/C0) | 4 (POST 2, PUT 1, PATCH 0, DEL 1; GET 3) | 7 (0 gated) | authorize( | unguarded (identity-only, no tenant gate) |  |
| routes | control-plane | none (mentions T0/C0) | 2 (POST 0, PUT 1, PATCH 0, DEL 1; GET 3) | 5 (0 gated) | require_permission( | unguarded (identity-only, no tenant gate) |  |
| saas | control-plane | company_id x16 (mentions T0/C44) | 6 (POST 3, PUT 2, PATCH 0, DEL 1; GET 3) | 9 (0 gated) | authorize( | unguarded (identity-only, no tenant gate) |  |
| saas_domains | control-plane | company_id x35 (mentions T8/C209) | 25 (POST 21, PUT 2, PATCH 0, DEL 2; GET 10) | 26 (0 gated) | caller_from( | unguarded (identity-only, no tenant gate) | Records carry company_id (SC35); control-plane domain provisioning - guard decision belongs to the control-plane owner, not C3. |
| saas_packages | control-plane | company_id x21 (mentions T1/C164) | 17 (POST 14, PUT 1, PATCH 1, DEL 1; GET 15) | 32 (0 gated) | caller( | unguarded (identity-only, no tenant gate) | Control-plane but NOT unguarded in practice: handlers call require_tenant_scope() (tenant/company match, 403 on mismatch) and require_control_plane() for admin routes; storefront reads intentionally tenant-visible. |
| saved_views | tenant-facing | none (mentions T0/C0) | 4 (POST 2, PUT 1, PATCH 0, DEL 1; GET 4) | 8 (8 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 8 guarded, 0 excluded. Writes call require_caller_company (group E, strict). |
| scheduler | infra-or-shared | none (mentions T0/C0) | 5 (POST 3, PUT 1, PATCH 0, DEL 1; GET 4) | 9 (0 gated) | authorize( | unguarded (identity-only, no tenant gate) | OFF-LIMITS (other worker). Job/queue infra; no tenant business data. |
| search | infra-or-shared | none (mentions T0/C0) | 3 (POST 2, PUT 0, PATCH 0, DEL 1; GET 3) | 6 (0 gated) | authorize( | unguarded (identity-only, no tenant gate) |  |
| security | control-plane | none (mentions T0/C0) | 8 (POST 5, PUT 2, PATCH 0, DEL 1; GET 11) | 18 (0 gated) | - | unguarded (identity-only, no tenant gate) | Hardening / brute-force / IP rules; no handler calls caller()/authorize() - control-plane hardening surface. |
| seo | tenant-facing | none (mentions T0/C0) | 3 (POST 2, PUT 1, PATCH 0, DEL 0; GET 3) | 6 (6 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 6 guarded, 0 excluded. Gate is require_tenant only (group C). |
| service_monitor | infra-or-shared | none (mentions T0/C0) | 4 (POST 3, PUT 0, PATCH 0, DEL 1; GET 7) | 11 (0 gated) | - | unguarded (identity-only, no tenant gate) |  |
| settings | control-plane | none (mentions T0/C0) | 2 (POST 0, PUT 1, PATCH 0, DEL 1; GET 2) | 4 (0 gated) | authorize( | unguarded (identity-only, no tenant gate) |  |
| sign | tenant-facing | tenant_id x30; company_id x4 (mentions T157/C10) | 18 (POST 11, PUT 0, PATCH 4, DEL 3; GET 12) | 30 (26 gated, 4 excluded) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group D). 26 guarded, 4 excluded. 4 excluded: token signing surface + public cert verify. |
| site_builder | tenant-facing | none (mentions T0/C0) | 13 (POST 5, PUT 5, PATCH 0, DEL 3; GET 5) | 18 (17 gated, 1 excluded) | authorize( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 17 guarded, 1 excluded (render_page — public render when published). Gate is require_tenant only (group C). |
| sms_marketing | tenant-facing | tenant_id x56; company_id x2 (mentions T353/C8) | 31 (POST 18, PUT 1, PATCH 5, DEL 7; GET 15) | 46 (44 gated, 2 excluded) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group E). 44 guarded, 2 excluded. 2 excluded: provider webhooks (secret-verified). |
| social_marketing | tenant-facing | tenant_id x41; company_id x2 (mentions T276/C8) | 23 (POST 15, PUT 0, PATCH 4, DEL 4; GET 13) | 36 (35 gated, 1 excluded) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group E). 35 guarded, 1 excluded. 1 excluded: account webhook (secret-verified). |
| software | tenant-facing | company_id x6 (mentions T0/C15) | 25 (POST 13, PUT 5, PATCH 0, DEL 7; GET 21) | 46 (44 gated, 2 excluded) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). 2 excluded: `latest_releases`, `download_release` (public by design, `ApiAccessRule` outer gate, no caller). |
| spreadsheets | tenant-facing | tenant_id x28; company_id x2 (mentions T198/C8) | 16 (POST 8, PUT 0, PATCH 5, DEL 3; GET 15) | 31 (30 gated, 1 excluded) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group E). 30 guarded, 1 excluded. 1 excluded: public share-token workbook. |
| statuspage | infra-or-shared | none (mentions T0/C0) | 8 (POST 5, PUT 2, PATCH 0, DEL 1; GET 8) | 16 (0 gated) | - | unguarded (identity-only, no tenant gate) | public_summary / public_page are intentionally unauthenticated public status pages. |
| storage | tenant-facing | none (mentions T0/C0) | 6 (POST 4, PUT 0, PATCH 0, DEL 2; GET 4) | 10 (10 gated) | authorize( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 10 guarded, 0 excluded. |
| storage_manager | tenant-facing | company_id x62 (mentions T0/C305) | 46 (POST 29, PUT 0, PATCH 8, DEL 9; GET 37) | 85 (82 gated, 1 excluded) | authorize( | GUARDED (C3 pilot 2, see Pilot 2 outcomes) | PILOT 2 (task 313). 83 routed handlers: 82 gated (66 scope-checked, 16 gate-only), 1 excluded (redeem_download); 2 unrouted helpers (run_health_check, run_one_rule). |
| store | tenant-facing | tenant_id x3; company_id x39 (mentions T5/C154) | 49 (POST 29, PUT 10, PATCH 0, DEL 10; GET 34) | 83 (76 gated, 7 excluded) | caller( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (group E). 76 guarded, 7 excluded. 3 conflict leaks closed + vendor_payout_summary was unauthenticated; 3 global-namespace leaks remain (C11). |
| studio | tenant-facing | company_id x18 (mentions T2/C201) | 17 (POST 9, PUT 3, PATCH 0, DEL 5; GET 15) | 32 (32 gated) | authorize( | GUARDED (task 319, see Task 319 outcomes) | TASK 319 (verify-only). Conversion real: auth.rs:67 caller resolves tenant first, :208 authorize(); 24 scope-checked, 7 gate-only, 1 delegated. |
| supervision | tenant-facing | none (mentions T0/C0) | 28 (POST 19, PUT 0, PATCH 6, DEL 3; GET 16) | 46 (44 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 44 routed gated, 0 excluded; 2 unrouted helpers. 4 handlers via delegation to guarded decide_inspection / decide_progress_report. |
| surveys | tenant-facing | none (mentions T0/C0) | 12 (POST 7, PUT 0, PATCH 2, DEL 3; GET 10) | 22 (20 gated, 2 excluded) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 20 guarded, 2 excluded (get_public_survey, submit_response — anonymous token-gated public surface). |
| tasks | tenant-facing | company_id x8 (mentions T0/C15) | 10 (POST 7, PUT 1, PATCH 0, DEL 2; GET 3) | 13 (13 gated) | require_permission( | GUARDED (C3-D pilot, see Pilot outcomes) | PILOT (C3-D). |
| tax | tenant-facing | company_id x21 (mentions T0/C187) | 12 (POST 7, PUT 3, PATCH 0, DEL 2; GET 14) | 26 (26 gated) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). |
| tenant_destroy | control-plane | tenant_id x8 (mentions T100/C13) | 9 (POST 8, PUT 1, PATCH 0, DEL 0; GET 4) | 13 (2 gated) | require_permission( | mixed (2/13 gated) | OFF-LIMITS (other worker). Mixed: 2/13 handlers tenant-gated; rest identity-only. |
| tenants | control-plane | tenant_id x24; company_id x2 (mentions T71/C23) | 7 (POST 5, PUT 1, PATCH 0, DEL 1; GET 7) | 14 (0 gated) | require_permission( | unguarded (identity-only, no tenant gate) | Control-plane provisioning records; tenancy.rs documents the tenant edge, handlers themselves are control-plane. |
| theme | control-plane | none (mentions T0/C0) | 1 (POST 1, PUT 0, PATCH 0, DEL 0; GET 3) | 4 (0 gated) | authorize( | unguarded (identity-only, no tenant gate) |  |
| tickets | tenant-facing | company_id x7 (mentions T0/C13) | 20 (POST 13, PUT 3, PATCH 0, DEL 4; GET 9) | 29 (29 gated) | caller( | GUARDED (C3-D pilot, see Pilot outcomes) | PILOT (C3-D). Handlers use authenticate() helper (identity-only). |
| timeline | infra-or-shared | none (mentions T0/C0) | 0 (POST 0, PUT 0, PATCH 0, DEL 0; GET 4) | 4 (0 gated) | - | unguarded (identity-only, no tenant gate) | Handlers use authorize_timeline_view() (identity-only); audit-timeline feed, no tenant business data of its own. |
| timetracker | tenant-facing | company_id x20 (mentions T0/C177) | 39 (POST 26, PUT 3, PATCH 4, DEL 6; GET 29) | 68 (61 gated, 7 excluded) | require_permission( | GUARDED (C3-D pilot, see Pilot outcomes) | PILOT (C3-D). 7 agent_* handlers authenticate by API key (authenticate_agent), not by caller - excluded; binding agent keys to a tenant is a C11 decision. |
| todos | tenant-facing | none (mentions T0/C0) | 22 (POST 13, PUT 0, PATCH 3, DEL 6; GET 11) | 34 (33 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 33 routed gated, 0 excluded; 1 unrouted helper. 2 handlers via delegation to guarded set_list_archived. |
| users | control-plane | company_id x12 (mentions T0/C79) | 4 (POST 2, PUT 1, PATCH 0, DEL 1; GET 3) | 7 (0 gated) | caller( | unguarded (identity-only, no tenant gate) | OFF-LIMITS (other worker). Control-plane identity records. |
| vault | tenant-facing | tenant_id x17; company_id x10 (mentions T71/C28) | 16 (POST 12, PUT 0, PATCH 2, DEL 2; GET 11) | 27 (27 gated) | caller( | gated (caller resolves tenant first) | Already gated: every handler calls tenant-resolving caller() (helpers.rs:49); records carry tenant_id (ST17) + company_id (SC10). |
| vcs | tenant-facing | none (mentions T0/C0) | 13 (POST 9, PUT 2, PATCH 0, DEL 2; GET 10) | 23 (20 gated, 3 excluded) | authorize( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 20 guarded, 3 excluded (github/gitlab/bitbucket_webhook — HMAC-signed provider webhooks). Map had 22; code routes 23. Writes call require_caller_company (group E, strict). |
| web3 | tenant-facing | company_id x7 (mentions T0/C19) | 17 (POST 12, PUT 0, PATCH 2, DEL 3; GET 15) | 32 (31 gated, 1 excluded) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). 1 excluded: `public_ledger` (anonymous read, must stay ungated). |
| webhooks | tenant-facing | none (mentions T0/C0) | 7 (POST 5, PUT 1, PATCH 0, DEL 1; GET 5) | 12 (12 gated) | require_permission( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 12 guarded, 0 excluded. Writes call require_caller_company (group E, strict). |
| webinars | tenant-facing | none (mentions T0/C0) | 30 (POST 24, PUT 0, PATCH 2, DEL 4; GET 14) | 44 (44 gated) | actor_id( | GUARDED (C3 pilot 4, see Pilot 4 outcomes) | PILOT 4 (task 316). 44 guarded, 0 excluded. Gate is require_tenant only (group C). |
| whitelabel | control-plane | tenant_id x12; company_id x21 (mentions T74/C157) | 5 (POST 0, PUT 4, PATCH 0, DEL 1; GET 10) | 15 (0 gated) | require_permission( | unguarded (identity-only, no tenant gate) |  |
| workflows | tenant-facing | company_id x4 (mentions T0/C28) | 10 (POST 8, PUT 0, PATCH 1, DEL 1; GET 5) | 15 (14 gated, 1 excluded) | require_permission( | GUARDED (C3 pilot 3, see Pilot 3 outcomes) | PILOT 3 (task 314). 1 excluded: `webhook_trigger` (no `HttpRequest`; token-authenticated, 64-hex, constant-time compare). `get_workflow_run` now 404s orphaned runs (was `workflow: null`) and 403s out-of-scope parents. |

## Pilot 2 outcomes (2026-11-23, tracker task 313)

Pilot 2 guarded the three deferred follow-up modules — projects,
products, storage_manager. Totals: **271 handlers guarded, 0 missing,
2 excluded** (`c3_verify.py` re-run per module after the final edits;
`c3_order.py`: **0 order violations** in all three). Scope verification
for storage_manager is `c3_storage_scope.py` (per-handler record-check
classification: 66 scope-checked, 16 gate-only, 1 excluded, 0 missing).

Guard idiom: same as pilot 1 (`require_tenant(req)` first, then
`caller_company()` from the verified Caller — never the `X-Company-Id`
header), plus the storage_manager header migration: all 7
`caller_company_id(&req)` sites now use the verified scope and the
header helper is deleted. Record checks follow the module's company
shape (`Option<String>` in all three). Two real defects were found and
fixed along the way: the gate pass had left `require_tenant` /
`caller_company` unresolvable in the products handler files (explicit
imports, not `use super::*;`) and in all 82 storage_manager handlers
(names missing from the `storage_manager.rs` re-export list) — both
fixed by adding the imports / re-exports (verified by a minimal rustc
repro of the E0425, not by a full build: the root bin was NOT
typechecked here).

| Module | Guarded | Excluded | Helpers (file / company shape) | Filters + checks added | Tests |
|---|---|---|---|---|---|
| projects | 120 | 1 (list_methodologies — static catalogue) | state.rs / Option<String> | `require_project_access` now takes the scope; 77 handler + 36 s4 sites threaded; list_projects filters by `in_company_scope`; create_project verifies the body company; templates gate-only | — |
| products | 69 | 0 | helpers.rs / Option<String> | 51 scope-checked (list filters, `require_product_in_scope` load checks, create/update body verify), 18 gate-only (module offerings, tax rules, taxonomy, tags — shared catalogue, no scope field) | — |
| storage_manager | 82 | 1 (redeem_download — token redemption) | helpers.rs / Option<String> (+ bucket / company / user / module quota shapes) | 66 scope-checked (bucket / object / binding / quota / lifecycle / health / signed-URL / stats / defaults / plan-assignment / BYO checks + 7 header sites migrated to the verified scope), 16 gate-only (plans, BYO settings / terms, status counts, admin batch) | `c3_storage_scope.py` per-handler verification (exit 0); helpers.rs scope tests (pre-existing) |

Residuals (honest, not partitioned by the guard alone):

- storage_manager: `manage_buckets` / `manage_byo` / `manage_plans`
  holders are admins by module design (the `check_bucket_access` /
  `load_byo_connection_for` / `get_my_plan` admin arms) — a
  company-scoped holder of one of those capabilities can touch another
  company's buckets / BYO connections / plan buyers. Whether an admin
  capability implies cross-company access is the task-315
  (ControlPlaneGuard) decision. Global uniqueness checks (bucket name)
  can leak cross-company existence via conflict errors — scoped
  uniqueness is follow-up work. `create_quota` is fail-closed: user
  quotas only for the caller, module quotas only for platform callers
  (a user-directory lookup would be needed to relax either).
  `enforce_byo_reacceptance` is an admin-only batch over all
  connections. `get_my_plan`'s admin arm (any buyer via query) is kept
  as designed; the assignment / metering lists stay purely
  scope-filtered.
- products: global uniqueness checks (product slug, variant SKU) can
  leak cross-company existence via conflict errors — scoped uniqueness
  is follow-up work.
- projects: no residuals recorded beyond the shared admin / uniqueness
  notes above.

## Pilot 3 outcomes (2026-11-23, tracker task 314)

Pilot 3 guarded the 28 remaining scoped tenant-facing modules — the
arithmetic from the synthesis: 33 scoped in the map after pilot 1, minus
3 (pilot 2: projects/products/storage_manager), minus 2 (files/studio,
already `CallerScope`-converted by Worker A) = 28. Totals:
**696 handlers guarded, 0 missing, 24 excluded** (independent
coordinator re-run of `c3_verify.py` for all 28 modules after the final
edits; the 5 delegation cases the verifier cannot see inline —
elearning `publish_course`/`archive_course` → shared `transition_course`,
events `publish_event`/`cancel_event`/`complete_event` → shared
`transition_event`, analytics `record_metric_handler` (qualified route
target) — were verified by reading the shared helper, which gates
first). Guard idiom: same as pilots 1–2 (`require_tenant(req)` first,
then `caller_company()` from the verified Caller — never the
`X-Company-Id` header). Five worker groups ran in parallel; per-group
detail lives in `docs/integration/pilot3/group-{a,b,c,d,e}-results.md`.

| Module | Guarded | Excluded | Helpers (file / company shape) | Filters + checks added | Tests |
|---|---|---|---|---|---|
| advisor | 8 | 0 | helpers.rs / Option<String> | handler-level fix: `re_evaluate` denies unresolved callers (body company verified, snapshots stamped with resolved scope); `evaluate` untouched (hooks.rs:177 sweep) | — |
| affiliates | 29 | 1 (track_click) | helpers.rs / Option<String> | list filters, `require_affiliate_in_scope` + commission/payout loaders, create/update body verify | 7 guard tests |
| ai | 51 | 0 | helpers.rs / Option<Uuid> | policy + gating handlers switched from header-first `resolve_caller_company` to verified Caller; usage scope-filtered (`ai:read_usage` platform-reader carve-out); drafts gate-only | 7 tests |
| analytics | 27 | 0 | helpers.rs / Option<String> | `record_sale` body verify (was stored verbatim); duplicate `order_id` check same-company only; scheduler hook runs `&None` (global sales only) | handler tests updated |
| auditors | 18 | 1 (get_public_by_token) | helpers.rs / Option<String> | accounts scoped via scope-entry companies; links scoped by creator | fixture updates (tenant context) |
| backup | 20 | 0 | validation.rs / Option<String> | `create_job` stamps verified scope; closes the task-312 note at handler level | — |
| bi | 21 | 0 | state.rs / Option<String> | header-first `resolve_caller_company` deleted; `engine.rs` `None` arms global-only; task-312 helpers reused | — |
| company | 11 | 0 | requests.rs + handlers.rs / Option<String> | gate-only: records ARE the scope (filter is circular); fail-closed gate | — |
| domain_verification | 8 | 0 | — / Option<String> | standard gate + scope checks | — |
| domains | 8 | 0 | state.rs / Option<String> | standard gate + scope checks | — |
| elearning | 57 | 1 (verify_certificate) | helpers.rs / Option<String> | list filters, create/update body verify, load+scope after every load; publish/archive via shared `transition_course` | — |
| events | 32 | 5 (public feed) | helpers.rs / String | gates in 25 closure handlers + shared `transition_event`; registrations scope through their event; authenticated calendar feed split from the public feed | tenant_guard_tests |
| hosting | 15 | 0 | — / Option<String> | standard gate + scope checks | — |
| import_export | 27 | 0 | auth.rs / Option<String> | 25 `require_caller_company` sites rebased to `(ctx, tenant, req, caller)`; `can_view_company` strictness kept | — |
| knowledgebase | 17 | 0 | helpers.rs / Option<String> | header/hook-fallback `caller_company_id` deleted; reads now filter by company | articles tests rewritten |
| loyalty | 26 | 0 | helpers.rs / Option<String> | list filters, `require_program_in_scope`, create/update body verify | 7 guard tests |
| mcp | 7 | 5 (public/catalogue/key) | validation.rs / Option<String> | list filter, create body verify, scope-checked loads, `require_server_in_scope` 404s out-of-scope | 9 tests |
| planner | 18 | 0 | — / Option<String> | standard gate + scope checks | — |
| portals | 19 | 1 (get_public_portal) | helpers.rs / Option<String> | standard gate + scope checks; old `caller_company` helper removed | — |
| pos | 35 | 0 | validation.rs / Option<String> | `require_sale_in_scope`; create/update body verify | 7 guard tests |
| presets | 17 | 0 | — / Option<String> | standard gate + scope checks | pre-existing tests broken (see residuals) |
| pricing_tables | 47 | 2 (render_table, vendor_storefront) | auth.rs / Option<String> | `require_vendor_in_scope` + commission/payout/approval loaders; create/update body verify | 7 guard tests |
| properties | 33 | 4 (public property/plan) | helpers.rs / Option<Uuid> | list filter, create body verify, update load+scope+`require_company_change_uuid_for`; plan/block handlers via parent property | tenant context in test builders |
| risk | 30 | 0 | common.rs / Option<String> | handler-level fix: `review_check` denies unresolved, always calls scan with `Some(company)`; `risk_matrix` refuses a filter naming another company | — |
| software | 44 | 2 (latest_releases, download_release) | helpers.rs / Option<String> | parent-chain `version_company`/`release_company`/`file_company`; scoped `get_stats`; taxonomy gate-only | 8 tests |
| tax | 26 | 0 | validation.rs / Option<String> | `acting_company` reworked to `(ctx, tenant, req, caller)`, header block removed; `in_company_scope` `None => false` (no global rows) | tests in validation.rs |
| web3 | 31 | 1 (public_ledger) | helpers.rs / Option<String> | wallets/watched/anchors scope-checked for non-admins; `verify_anchor` scope-filters candidates (out-of-scope reads as not-found, never 403) | 7 tests |
| workflows | 14 | 1 (webhook_trigger) | — / Option<String> | standard gate + scope checks; `get_workflow_run` 404s orphaned runs, 403s out-of-scope parents | — |

Remaining after pilot 3 (reconciled against the synthesis counts): the
map still shows **43 tenant-facing modules unguarded — 2 scoped
(files, studio; already `CallerScope`-converted by Worker A, map rows
stale) + 41 gate-only (task 316)** — plus the **20 modules not in the
map at all (task 319)**. The task-312 appendix items for advisor
(`evaluate`) and risk (`run_review_check`) are FIXED by the handler-level
work above; inbox `tenant_visible` is STILL OPEN (product decision
pending) and was not touched.

Scope-rule correction (found by group E, verified by the coordinator
against pilot 1's own `tickets/validation.rs:236`): the brief's prose
"company callers read own + global only" is imprecise. The idiom as
implemented in every pilot is: unresolved callers read global records
only; **scoped callers see ONLY their own company's rows (global/legacy
rows hidden)**; platform access is explicit, never the default. That is
the canonical rule; the brief prose was the inaccurate part, not the
code.

> **Correction to the "canonical rule" above (pilot 4, 2026-11-23,
> tracker task 316) — the claim above is WRONG as a tree-wide statement
> and is kept here only as the historical record of what pilot 3
> asserted. What the code actually says, verified in source:**
> - Core `CallerScope` (`crates/unified-core/src/tenancy.rs:401-410`,
>   doc comment on `Company`): a company caller reads **that company's
>   rows PLUS global rows**, and writes that company only.
> - Per-module `in_company_scope` helpers (e.g.
>   `src/modules/tickets/validation.rs:236`): with a scope, **only
>   records carrying exactly that company match; unscoped (legacy)
>   records are hidden from scoped callers**.
> - Both variants are fail-closed; they differ ONLY on whether company
>   callers see global/legacy rows. Neither variant leaks another
>   company's rows. Unification of the rule into one tree-wide
>   statement is tracker task 320 (already created); this note does
>   not change any guard code.

Residuals (honest, not partitioned by the guard alone):

- Gate-only records with no company field (pos registers/shifts/payment
  methods/settings; pricing tables/tiers/features; commission-rule
  list/get/delete not vendor-filtered; elearning paths; analytics
  license/usage/uptime rows; software taxonomy; drafts in ai) — real
  partitioning needs a `tenant_id`/`company_id` on the record (C11).
- Global uniqueness checks can leak cross-company existence via
  conflict errors (products slug/SKU, properties slug, software slug).
- Platform admins without a resolvable company can now only read global
  records / cannot write company records (task-315 decision); ai keeps
  the `ai:read_usage` platform-reader carve-out and admin arms.
- Header trust remains open until task 317 (a caller WITH a forged
  `X-Company-Id` can still act as that company where a header helper
  survives — ai inner `authorize_ai_call`/`resolve_capability_company`,
  bi engine internals); pilot 3 deleted the header-first helpers in
  bi, ai gating/usage, tax, knowledgebase and portals.
- presets: pre-existing handler tests call async handlers without
  `.await` and with no tenant context — cannot compile/pass as written;
  flagged, not fixed (pre-existing, not a pilot-3 regression).
- KB `delete_category` global in-use check can leak cross-company use.
- `cargo check`/`cargo test` on the root bin were NOT run (OOM); all new
  tests are UNRUN.

## Pilot 4 outcomes (2026-11-23, tracker task 316)

Pilot 4 gated the 41 gate-only tenant-facing modules — records carry no
scope/company field, so the guard is a fail-closed tenant gate at the
handler, not a record check. Totals: **1,112 handlers gated, 0 missing,
30 excluded** (coordinator independently re-ran all five group
verifiers after the final edits: guarded=1,112, excluded=30,
missing=0, order_violations=0; the verifiers list each documented
exclusion as MISSING when run standalone, which is why their raw
missing total is 30 — every one of the 30 is a documented exclusion
below). Five worker groups ran in parallel; per-group detail lives in
`docs/integration/pilot4/group-{a,b,c,d,e}-results.md` with per-group
verifiers `c3_group_{a,b,c,d,e}_verify.py`.

| Group | Modules | Gated | Excluded | Missing |
|---|---|---|---|---|
| A | academic, compliance, incidents, surveys, announcements, providers, email_templates, app_sync | 233 | 7 | 0 |
| B | curriculum, supervision, community, todos, contracts, departments, email_providers, mail | 244 | 6 | 0 |
| C | hr, marketing, webinars, meetings, assets, site_builder, panels, seo | 261 | 2 | 0 |
| D | field_service, admissions, privacy, recruitment, newsletter, notifications, bank_transfer, storage | 238 | 12 | 0 |
| E | procurement, vcs, content, crm, ai_support, maintenance, webhooks, saved_views, downloader | 136 | 3 | 0 |
| **Total** | **41** | **1,112** | **30** | **0** |

Guard idiom: `require_tenant(req)` first in every handler, then
`caller_company()` from the verified Caller — never the `X-Company-Id`
header. Gate-only throughout: no record in any of the 41 modules
carries a company field, so this is a tenant edge only — real
partitioning needs a `tenant_id` on the record (C11 follow-up).

**Write strictness is NOT uniform across groups — recorded honestly,
not harmonized here.** The brief said writes require a resolved
company; the groups implemented three variants, and the difference is
a task-315 decision, not a pilot-4 gate defect:
- **Strict (A, E — 16 modules):** reads resolve `caller_company`;
  writes call `require_company` / `require_caller_company` and 403
  when no company resolves. This is STRICTER than the scoped pilots,
  where an unresolved caller may still write a global (unscoped)
  record — e.g. pilot 1's create bodies adopt the caller scope.
- **Mismatch-only (B, D — 17 modules):** `caller_company` 403s only
  when the tenant company and the verified-caller company disagree;
  it returns `Ok(None)` when neither carries a company
  (`curriculum/helpers.rs:109-127`). Writes by a company-less caller
  proceed. D deliberately kept this so self-service writes keep
  working (applicant submit/withdraw, consent grant/withdraw, own
  notifications) — those flows would break under the strict rule and
  need a product decision, not a silent gate change.
- **Tenant-gate only (C — 8 modules):** `require_tenant` only, no
  company resolution on any handler. C followed the cited gate-only
  model (`company/handlers.rs`, pilot 3) and declined the company
  requirement on writes as task-315 territory.
Harmonization (one rule for writes by company-less callers, and what
happens to D's self-service handlers under it) is a task-315
follow-up. Until then the three variants coexist, all fail-closed on
the tenant gate itself.

Exclusions (30, all documented): public/anonymous surfaces
(community public topics, recruitment public jobs, announcements
public/changelog, surveys public, site_builder render_page, marketing
unsubscribe); token-gated surfaces (contracts signing links, newsletter
subscribe/confirm/unsubscribe/track, app_sync HMAC device tokens,
bank_transfer public checkout list/creation); provider webhooks
(vcs github/gitlab/bitbucket HMAC webhooks, newsletter
provider_webhook); static catalogue (email_providers list_catalog).

Notable work: 12 handlers that took no `HttpRequest` and did no
caller check were re-signed to take one (mail send_mail/test_mail/
list_log/list_connectors — mail send/test routes previously sent mail
with no auth on the HTTP route; bank_transfer get_status/get_settings/
list_accounts/get_account). Count corrections vs the map table:
academic 90→91, email_providers 12→13, mail 6→7, meetings 30→31,
panels 14→20 (macro-generated list handlers), vcs 22→23,
newsletter 29→28 (send_campaign is an unrouted helper),
supervision/todos counts include unrouted helpers (46/34 in the map,
44/33 routed).

Residuals (honest, not partitioned by the gate alone):

- Gate-only records with no company field in all 41 modules — real
  partitioning needs a `tenant_id`/`company_id` on the record (C11).
- Platform admins without a resolvable company now get 403 on writes
  in the 16 strict modules (A, E) until the task-315 decision; the
  other 25 modules still let a company-less caller write.
- Header trust remains open until task 317.
- Pilot-3 hosting test bug found by group E: its
  `caller_company_prefers_tenant_then_verified_caller` test asserts
  the tenant company wins over a *different* verified-caller company,
  but the implementation 403s on any mismatch — the assertion cannot
  pass as written (never caught because pilot-3 tests are unrun).
  Group E's 36 guard tests assert the behaviour the code actually
  implements.
- Group D incident (fixed, disclosed in group-d-results.md): a
  selective-rustfmt script deleted the `use super::helpers::{...}`
  import in 6 field_service handler files and duplicated it in 8
  others. Caught by an import-presence audit; the 6 imports were
  restored (cross-checked against the original lists — exact match),
  duplicates removed. Coordinator re-verified: all 6 files carry the
  import exactly once and parse clean.
- `cargo check`/`cargo test` on the root bin were NOT run (OOM); all
  new tests are UNRUN.

Remaining after pilot 4: the map now shows **2 tenant-facing modules
unguarded — files and studio (already `CallerScope`-converted by
Worker A, map rows stale)** — plus the **20 modules not in the map at
all (task 319)**. Inbox `tenant_visible` is STILL OPEN (product
decision pending) and was not touched.

## Pilot exclusions (documented public / non-tenant auth surfaces, NOT gated)

- appointments: `guest_book`, `public_booking_lookup`, `list_public_services`,
  `list_public_service_slots` — anonymous public booking surface (no `HttpRequest`
  on two of them; gating would break public booking).
- chat: `get_widget_config`, `bootstrap_widget_session`, `poll_widget_session`,
  `create_offline_message` — public support-widget surface, no `HttpRequest` param.
- documents: `download_signed` — signed-URL token auth, no `HttpRequest` param.
- forms: `get_public_form`, `submit_public_form` — anonymous public-form surface.
- logistics: `track_shipment` — token-based public tracking.
- timetracker: all 7 `agent_*` handlers — desktop-agent API-key auth
  (`authenticate_agent`); binding agent keys to a tenant is a C11 decision, not
  a C3 gate.
- projects: `list_methodologies` — static catalogue, no tenant data.
- storage_manager: `redeem_download` — signed-URL token redemption, no
  `HttpRequest` param; possession of the token IS the authorization.
- Pilot 3 (task 314): affiliates `track_click` (anonymous click
  tracking; referral code is the authorization, API Manager
  `ApiAccessRule`); pricing_tables `render_table` (public when
  published) + `vendor_storefront` (no `HttpRequest`; approved-vendor
  storefront only); elearning `verify_certificate` (public verification
  endpoint); properties `list_public_properties` / `get_public_property`
  / `get_public_plan_svg` / `get_public_plan_print` (anonymous, no
  `HttpRequest`); events `list_public_events` / `get_public_event` /
  `public_event_ics` / `public_calendar_ics` / `public_calendar_json`
  (anonymous, no `HttpRequest`, API Manager `ApiAccessRule`); mcp
  `get_server_public` (public registry view), `list_tools` /
  `list_resources` / `list_prompts` (static catalogues, no req),
  `mcp_endpoint` (API-key token surface; per-server auth inside
  `dispatch_rpc`); software `latest_releases` / `download_release`
  (public by design, `ApiAccessRule` outer gate, no caller); web3
  `public_ledger` (anonymous read); portals `get_public_portal`
  (unauthenticated public portal render by design; API Manager gate;
  only empty-role menus shown); workflows `webhook_trigger` (no
  `HttpRequest`; token-authenticated, 64-hex, constant-time compare);
  auditors `get_public_by_token` (public route; link token IS the
  credential, API Manager `ApiAccessRule`, default deny).
- Other modules (not piloted): public handlers exist in contracts (signing links),
  newsletter (subscribe/confirm/unsubscribe/track/webhook), properties (public
  listings), surveys (public survey + submit), recruitment (`apply_public`),
  elearning (`verify_certificate`), web3 (`public_ledger`), statuspage
  (`public_summary` / `public_page`) — any future pilot must exclude these.

## Task 312 audit: fail-open None-scope helpers (2026-11-23)

Scope of this audit: shared helpers that take an `Option<...>` caller
company/scope and decide access. Rule applied (the shared
`CallerScope::Unresolved` rule from `crates/unified-core/src/tenancy.rs`):
an unresolved caller (no resolvable company) reads global records only
and writes nothing under a company; platform access is explicit, never
the default for a missing company. Header trust (`X-Company-Id`
choosing a company) is NOT fixed here — it is task 317's job: after
this fix a caller with NO company can no longer read/write company
records, but a caller WITH a forged header company can still act as
that company until 317 lands. All fixes below were verified by
`rustfmt --edition 2024` parse (all edited files parse; zero rustfmt
diff lines touch the edited lines — the tree-wide `--check` failures
are pre-existing import-ordering/style drift) and by grep (zero
`(Some(body), _) => Ok(Some(body.clone()))` arms remain tree-wide).
Root-bin files were NOT typechecked here (bin crate OOM-kills this VM).

### Fixed in task 312 (clear-cut fail-open cases)

Read side — `in_company_scope` `None` arm `true` (every record) became
global-only (`record.is_none()` / `record.is_empty()`; timetracker
records always carry a company, so `false`):
- `src/modules/gamification/helpers.rs:127` — `None => record_company_id.is_none()`.
- `src/modules/okrs/helpers.rs:129` — `None => record_company_id.is_empty()`.
- `src/modules/logistics/helpers.rs:142` — `None => record_company_id.is_none()`.
- `src/modules/appointments/common.rs:146` — `None => record_company_id.is_none()`.
- `src/modules/forms/common.rs:144` — `None => record_company_id.is_none()`.
- `src/modules/tickets/validation.rs:236` — `None => record_company_id.is_none()`.
- `src/modules/chat/helpers.rs:116` — `None => record_company_id.is_none()`.
- `src/modules/tasks/validation.rs:161` — `None => record_company_id.is_none()`.
- `src/modules/documents/validation.rs:437` — `None => record_company_id.is_empty()`.
- `src/modules/timetracker/auth.rs:127` — `None => false` (no global rows).
- `src/modules/timetracker/auth.rs:135` (`in_company_scope_opt`) — `None => record_company_id.is_none()`.

Write side — `require_company_match_for` / wrappers `(Some(body), _) => Ok(Some(body))`
(an unresolved caller could store a record under ANY company) became
`(Some(body), None) => Err(forbidden)`; `(None, None) => Ok(None)`
(global row) stays, matching the library reference test:
- `src/modules/gamification/helpers.rs:93,104` (wrapper delegates to `_for`).
- `src/modules/logistics/helpers.rs:106,117` (wrapper delegates to `_for`).
- `src/modules/appointments/common.rs:112,123` (wrapper delegates to `_for`).
- `src/modules/forms/common.rs:110,121` (wrapper delegates to `_for`).
- `src/modules/tickets/validation.rs:202,213` (wrapper delegates to `_for`).
- `src/modules/chat/helpers.rs:91` (inline match, no `_for` fn).
- `src/modules/tasks/validation.rs:90,101` (`_for`) and `:126,138` (`_uuid_for`, same split).
- `src/modules/documents/validation.rs:392,414` (wrapper had its own inline match; fixed separately).
- `src/modules/timetracker/auth.rs:93,104` (wrapper delegates to `_for`).
- `src/modules/okrs/helpers.rs:106` (`require_company_match_str`, rewritten match).

Other fail-open `None` arms fixed the same way:
- `src/modules/bi/state.rs:159` (`can_access_company`) — `None => true` became `None => company_id.is_empty()` (the `can_manage` bypass stays: it is capability-gated, not scope-gated).
- `src/modules/bi/validation.rs:215` (`can_read_report`) — `None => true` became `None => report.company_id.is_empty()`.
- `src/modules/tax/validation.rs:120` (`require_company_scope`) — `None => Ok(())` became `None => Err(forbidden)` (unreachable at its one call site — `acting_company` already fails closed first — fixed anyway as the pattern is fail-open on its face).
- `src/modules/ai/validation.rs:63` (`require_company_scope`) — `None => Ok(())` became `None => Err(forbidden)`; 11 call sites in `handlers_gating.rs` + `handlers_usage.rs` pass the resolved caller company. Consequence: a verified platform admin (capability `ai:manage_providers`, no company) managing AI policies now gets 403 until an explicit platform decision exists — that is task 315 territory, not a reason to keep the hole.
- `src/modules/backup/validation.rs:159` (`job_visible`) and `:168` (`run_visible`) — `None => true` became `None => job/run.company_id.is_none()` (global jobs/runs only); `src/modules/backup/types.rs` field doc corrected to match. Callers pass the resolved caller company (`handlers.rs`, `execution.rs`, `visible_job`/`visible_run`).

Tests updated/pinned with the fix (flipped assertions + new denial tests):
`gamification/helpers.rs`, `okrs/helpers.rs`, `logistics/helpers.rs`,
`appointments/common.rs`, `forms/common.rs`, `tickets/validation.rs`,
`chat/helpers.rs`, `tasks/validation.rs`, `documents/validation.rs`,
`timetracker/auth.rs`, `bi/state.rs` (new test module),
`bi/validation.rs`, `tax/validation.rs`, `ai/validation.rs`,
`backup/validation.rs` (new visibility test). Tests are UNRUN (root bin
OOM-blocks `cargo test` on this VM; unified-core files were not touched,
so no `cargo test -p unified-core` run was required).

### Remaining (a) — fail-open, NOT fixed here (needs its owning task)

- `src/modules/advisor/state.rs:237` (`evaluate`, `in_scope` closure) — `None => true`: an unresolved caller via `handlers_suggestions.rs:148` evaluates facts of ALL companies. Not fixed inside `evaluate`: `hooks.rs:177` deliberately calls `evaluate(None, ...)` for a system-wide licensing-expiry sweep, so the fix belongs at the handler (resolve scope first / deny unresolved), not in the shared fn. Owner: advisor handler guard (pilots 3/4). — FIXED in pilot 3 (task 314): `handlers_suggestions.rs` `re_evaluate` denies unresolved callers; `evaluate` untouched.
- `src/modules/risk/store.rs:207` (`run_review_check`) — `None => true`: handlers `handlers_plans.rs:416` + `handlers_instances.rs:299` pass the caller's resolved company, so an unresolved caller scans ALL companies' risks and raises alerts/notifications. Fix belongs at the handlers (deny unresolved before calling); the fn's `None`-means-all shape is also pinned by its own tests (`:348,:350`). Owner: risk handler guard (pilots 3/4). — FIXED in pilot 3 (task 314): `handlers_plans.rs` `review_check` denies unresolved callers, always calls the scan with `Some(company)`; `run_review_check` shape preserved.
- `src/modules/inbox/auth.rs:92` (`tenant_visible`) — `(record_company, scope_company)` pair: `_ => true` when either side lacks a company, so a company-less scope sees any company's threads within the tenant (tenant id must still match exactly). Documented "match whenever both carry one" rule; changing it alters documented inbox behaviour. Needs a product decision, not a silent fix. Owner: inbox guard work (pilots 3/4) + product decision. — STILL OPEN after pilot 3 (product decision pending); not touched by pilot 3.

### (b) — already fail-closed (checked, no change)

- `src/modules/library/auth.rs:172` (`in_company_scope`, `CallerScope`) — the converted reference; `files/types.rs` + `studio/auth.rs` same shape.
- `crates/unified-core/src/tenancy.rs:488` (`CallerScope::require_company_scope`) — `Platform` explicit, `Unresolved` errors.
- `src/modules/expenses/helpers.rs:73` (`require_company_scope`) — row must equal the scope's company, else 404 (a `None` company row is unreachable).
- `src/modules/import_export/auth.rs:159` (`can_view_company`) — `(None, _) => false`, pinned by tests (`:273,:280,:289`); stricter than the shared rule (a no-company caller sees nothing, not even global rows).
- `src/modules/entitlements/state/lifecycle.rs:107` (`used_outside_package`) — `None` outcome means "used outside package" (restrictive), not "entitled".
- `src/modules/ai/capabilities.rs:615` — unknown module means gated (restrictive), not open.
- `src/modules/quality/helpers.rs:40` (`tenant_matches`) — record-side `None` (legacy unscoped record stays visible); documented migration-compat decision, not a caller-scope hole.
- `src/modules/printing/auth.rs:65` (`tenant_visible`) — record-side `None` (global standards / pre-stamping records); documented, record-side.
- `src/modules/ai/validation.rs` + `tax/validation.rs` capability checks around the fixed fns (`ai:manage_providers`, `require_permission`) are separate gates and were not changed.

### (c) — control-plane / infra / non-scope (checked, no change)

- `src/modules/whitelabel/helpers.rs:31` (`caller_company`) — reads `X-Company-Id` directly, errors when absent; header trust is task 317's job.
- `src/modules/ai/helpers.rs:347` (`resolve_caller_company`) — header-first resolution fn, not an access decision; header trust is task 317's job.
- `src/modules/storage_manager/helpers.rs` (`caller_company_id`, was :71) — REMOVED in pilot 2 (task 313): all 7 header sites now use the verified caller scope; `check_object_access` is fail-closed (owner-or-company); `check_bucket_access` non-admin + company-less bucket => Ok is the documented shared-bucket design. Header trust elsewhere is task 317's job.
- `src/modules/knowledgebase/helpers.rs:73` (`caller_company_id`) — verified caller company first, then hook; header NOT trusted in code (doc comment is stale, says header first). Used only to stamp articles; reads do NOT filter by company — unguarded module, pilot territory.
- `src/modules/portals/helpers.rs:54` (`caller_company`) — fail-closed (errors when no company).
- `src/modules/clusters/handlers_sync.rs:101` ("accepted as-is") — plan correlation ids in cluster sync, not company scope.
- `src/modules/licensing/releases.rs:421` (`license_may_download`) — `expires_at: None` means a perpetual licence; expiry semantics, not scope.
- `src/modules/projects/s3_helpers.rs:61`, `src/modules/store/digital.rs:290`, `src/modules/webinars/helpers.rs:358`, `src/modules/inbox/state.rs:97`, `src/modules/load_balancer/health.rs:183`, `src/modules/pricing/reconciliation.rs:354`, `src/modules/geo/lookup.rs:108`, `src/modules/cards/types.rs:120,137,545,716,743`, `src/modules/cards/state.rs:89`, `src/modules/curriculum/helpers.rs:368`, `src/modules/storage_manager/plans_helpers.rs:124`, `src/modules/phone/types.rs:251`, `src/modules/phone/handlers_signalling.rs:450`, `src/modules/sms_marketing/handlers_providers.rs:537`, `src/modules/social_marketing/handlers_accounts.rs:641` — query/search filters, "best candidate" selection, expiry/methodology/language/read-receipt/credential-health semantics; no caller-company decision in any of them.
- `src/modules/bi/engine.rs:116,120,124,628` (`read_source_rows` company/date filters) — filter parameters, not caller scope; both production call sites (`:305`, `:647`) always pass `Some(...)`, so the `None` arm is unreachable in production.
- `src/modules/exams/helpers.rs:218`, `src/modules/security/helpers.rs:38`, `src/modules/bank_transfer/service.rs:220`, `src/modules/load_balancer/requests.rs:163`, `src/modules/payments/handlers_catalog.rs:37`, `src/modules/auditors/handlers_auditors.rs:526`, `src/modules/auditors/handlers_links.rs:182,186,314,354,358`, `src/modules/portals/public.rs:135`, `src/modules/chat/handlers_presence.rs:59`, `src/modules/chat/helpers.rs:477`, `src/modules/quality/hooks.rs:39,43,102`, `src/modules/quality/handlers_inspections.rs:183`, `src/modules/quality/evaluation.rs:162`, `src/modules/panels/adapters_whmcs.rs:78`, `src/modules/entitlements/state/lifecycle.rs:107` (see (b) above) — search/query filters, inspector-assignment (not tenant scope), hook target-ref filters, pagination; no caller-company decision.
- Unguarded modules with no scope helpers at all (no (a) helper to fix; guarding is pilots 3/4 or task 319): `risk`, `advisor` (see (a) above for its handler-level hole), `exams`, `knowledgebase` (reads unfiltered by company), `store`, `payments`, `billing`, `entitlements`, `forums`, `kyc`, `rental`, `inbox`, `phone`, `sign`, `spreadsheets`, `esg`, `plm`, `referral`, `cards`, `delivery_notes`, `sms_marketing`, `social_marketing`, `library` (already `CallerScope`-converted by Worker A; needs its map row).
- Task 319 (2026-11-23) guarded every module in the list above except `risk`, `advisor`, and `knowledgebase`, which remain open; `library` got its map row (see Task 319 outcomes).
- `src/modules/gamification/helpers.rs` `user_balance(state, user_id, company_id: Option<&str>, ...)` — `None` sums the user's entries across companies, but only that user's own rows (`user_id` filter still applies) and only as the balance of the entry being appended; not an access decision, left unchanged.

## Task 317 header-trust removal (2026-11-23)

A forged `X-Company-Id` header could still make a caller act as that
company wherever a `caller_company` helper treated the header as a
scope source. After this task, zero production reads of the header
feed any scope or authorization decision (verified by a tree-wide
grep: the only remaining `.get("x-company-id")` in production code
is the timetracker claim below). The only company sources, in order:
(a) the verified Caller's server-resolved `company_id`
(`unified_core::caller::caller_from`); (b) the tenant-context
company where the handler already required a tenant; (c) the
`user.company_requested` hook answer where a module already uses it.

### Fixed — the 10 identical `caller_company` helpers

Each had a private `resolve_company_id` reading the header with the
fallback `(None, header) => Ok(header)`; the helper is deleted and
`caller_company` is now tenant-first / verified-caller-second, 403
on mismatch, `Ok(None)` otherwise (bi/state.rs model). 4 new tests
per module (tenant wins, verified wins when tenant has none,
mismatch 403, forged header ignored).

- `src/modules/gamification/helpers.rs` (caller_company doc :54)
- `src/modules/okrs/helpers.rs` (doc :69)
- `src/modules/logistics/helpers.rs` (doc :67, `pub(super)`)
- `src/modules/appointments/common.rs` (doc :72)
- `src/modules/forms/common.rs` (doc :70)
- `src/modules/tickets/validation.rs` (doc :162)
- `src/modules/chat/helpers.rs` (doc :52)
- `src/modules/tasks/validation.rs` (doc :51)
- `src/modules/documents/validation.rs` (doc :351)
- `src/modules/timetracker/auth.rs` (doc :54)

### Fixed — single-funnel resolvers

- `src/modules/ai/helpers.rs` `resolve_caller_company` — was
  header-first, then hook; now verified caller first, then hook.
  This is the funnel for every AI capability call
  (`authorize_ai_call` / `resolve_capability_company`), so the fix
  covers exams, import_export, tax, elearning, curriculum, content,
  knowledgebase, mail and academic AI endpoints at once. Covered by
  the pre-existing import_export tests
  (`resolve_caller_company_ignores_the_company_header`), which
  asserted the fixed behaviour and now pass against the fixed code.
- `src/modules/documents/validation.rs`
  `resolve_caller_company_id` — was header-first, then hook; now
  verified caller first, then hook. Only caller is the quota read
  (`handlers_admin.rs` `may_view_company_quota`). New test
  `resolve_caller_company_id_prefers_the_verified_caller_over_a_forged_header`.
- `src/modules/whitelabel/helpers.rs` `caller_company` — REQUIRED
  the header (401 without it); now reads the verified caller's
  company and is unauthorized when absent. 3 new tests.
- `src/modules/library/auth.rs` `resolve_company_id` — DELETED
  (its doc claimed the header was "only a hint", but the hint was
  the company the AI gate enforces). Both call sites
  (`handlers_admin.rs` `ai_search_assist`, `ai_recommendations`)
  now derive the company from the already-computed `CallerScope`
  and fail closed (403) when a scope company does not parse.
- `crates/unified-calculator/src/handlers.rs`
  `history_company_id` — was header, else body fallback; now
  verified caller company, else fallback. 3 new tests
  (`cargo test -p unified-calculator --lib` — see log
  `~/workspace/unified-check/check-calculator-317.log`).

### Fixed — stale docs / error strings (code already fixed, docs lied)

- `src/modules/backup.rs` (per-tenant scoping), `src/modules/portals.rs`
  (:50, :84), `src/modules/saas_domains.rs` (:96),
  `src/modules/exams/module.rs` (:173),
  `src/modules/knowledgebase.rs` (:14),
  `src/modules/whitelabel.rs` (:120),
  `src/modules/storage_manager/module.rs` (:147),
  `src/modules/tax.rs` (:11), `src/modules/ai/types.rs` (:340),
  `src/modules/backup/types.rs` (:78),
  `src/modules/saas_domains/caller.rs` (authorize_company /
  authorize_list docs + the "Non-admin list reads require an
  X-Company-Id header" error string — the `Caller.company_id`
  compared there already comes from `require_caller`),
  `src/modules/inbox/module.rs` (:84).
- `src/modules/inbox/auth.rs` — `resolve_company_id` already read
  the verified caller (not the header); its doc and the
  "X-Company-Id does not match..." error string were stale. Its
  tests were the one place still asserting header trust and would
  have FAILED if run: the helper set the header but attached a
  company-less verified caller, so the mismatch/malformed tests
  asserted 403 from a header the code ignores. Rewritten: the
  helper now attaches the verified company (plus an optional forged
  header) and the tests assert verified-company match / mismatch /
  malformed, plus a new forged-header-ignored test.

### Legitimate exception — kept, documented

- `src/modules/timetracker/auth.rs:170` `claimed_company_id` — the
  ONLY production `.get("x-company-id")` left. The header value is
  only a claim: all 4 call sites pair it immediately with
  `require_company_member`, which verifies the company exists AND
  the `timetracker.company.membership.check` hook answers `member:
  true` for (company_id, user_id), fail closed. A forged header can
  only select companies the caller is actually a member of.

### Residuals (not scope decisions, left as-is)

- `src/modules/whitelabel/helpers.rs:53` `caller_tenant`
  (`X-Tenant-Id`) — out of scope for 317. With the company fixed to
  the verified caller, a forged tenant id can only select profiles
  inside the caller's own company.
- The 8 deliberate `insert_header` regression tests in
  `#[cfg(test)]` that prove headers grant nothing stay untouched,
  as do all other test `insert_header(("x-company-id", ...))`
  fixtures (24 total, all in test mods, each asserting the header
  is ignored or the verified caller wins).
- `src/modules/knowledgebase/helpers.rs` `caller_company` was
  already fixed in pilot 3 (tenant-first, verified-second); the
  old `caller_company_id` helper named in the 317 brief no longer
  exists. Its module doc (`knowledgebase.rs:14`) was stale and is
  fixed above.

### Verification

- `rustfmt --edition 2024 --emit stdout`: parse-clean on all 30
  touched files (20 in the first pass, 10 more found by the sweep).
- Tree-wide grep: 1 production `.get("x-company-id")`
  (timetracker claim, above); 0 production reads feeding any
  scope/authorization decision.
- Root-bin files were NOT typechecked (`cargo check -p unified`
  OOM-kills this VM); the new guard tests in `src/modules/*` are
  unrun for the same reason. The calculator crate's 3 new tests
  were run via `cargo test -p unified-calculator --lib` (see log).

## Task 319 outcomes (2026-11-23, tracker task 319)

Coordinator for the 20 modules Worker D's classification excluded
as "coordinator-listed guarded" (header, line 5): they were never
guarded. Five worker groups (pilot5, 4 modules each) guarded them
all; the coordinator re-ran every group verifier afterwards.
Detail: `docs/integration/pilot5/group-{a,b,c,d,e}-results.md`;
verifiers `docs/integration/pilot5/c3_group_{a,b,c,d,e}_verify.py`.

### Counts (verifier re-runs, coordinator, missing == 0 everywhere)

| Module | Routed | Guarded | Excluded | Missing | Group |
|---|---|---|---|---|---|
| billing | 69 | 69 | 0 | 0 | A |
| cards | 11 | 11 | 0 | 0 | A |
| delivery_notes | 22 | 22 | 0 | 0 | A |
| entitlements | 20 | 20 | 0 | 0 | A |
| esg | 33 | 33 | 0 | 0 | B |
| exams | 58 | 58 | 0 | 0 | B |
| forums | 51 | 51 | 0 | 0 | B |
| inbox | 18 | 18 | 0 | 0 | B |
| kyc | 48 | 48 | 0 | 0 | C |
| library | 65 | 65 | 0 | 0 | C (verify-only) |
| payments | 43 | 39 | 4 | 0 | C |
| phone | 62 | 62 | 0 | 0 | C |
| plm | 29 | 29 | 0 | 0 | D |
| referral | 37 | 36 | 1 | 0 | D |
| rental | 31 | 31 | 0 | 0 | D |
| sign | 30 | 26 | 4 | 0 | D |
| sms_marketing | 46 | 44 | 2 | 0 | E |
| social_marketing | 36 | 35 | 1 | 0 | E |
| spreadsheets | 31 | 30 | 1 | 0 | E |
| store | 83 | 76 | 7 | 0 | E |
| **Total (20 modules)** | **823** | **803** | **20** | **0** | |
| files (verify + gate) | 7 | 7 | 0 | 0 | coord |
| studio (verify-only) | 32 | 32 | 0 | 0 | C |

Exclusions (20): payments `list_providers` / `get_provider` /
`get_provider_settings` (static catalogue) + `webhook_receiver`
(public, signature-verified first); referral `click_link`
(anonymous click tracking, token hash is the credential); sign
`get_sign_page` / `post_sign` / `post_decline` (signing-link
surface, token is the credential) + `verify_certificate` (public
SHA-256 verification); sms `delivery_webhook` /
`inbound_webhook` (provider webhooks, secret-verified, tenant
derived from provider); social `metrics_ingest` (account webhook,
secret-verified, tenant from account); spreadsheets
`get_public_workbook` (public, share-token hash is the
credential); store public storefront `list_digital_products` /
`get_digital_product` / `list_digital_previews` /
`download_sample` + `signed_download` (HMAC-signed expiring
token) + `check_update` / `download_package` (license-key auth).

### Write strictness (recorded, NOT harmonized — task 315)

| Module | Variant |
|---|---|
| billing | mismatch-only (pilot-3) |
| cards | gate-only + strict writes (`require_company` on mutations) |
| delivery_notes | mismatch-only (Uuid; scope failure on a loaded note is 404) |
| entitlements | tenant surface strict (`TenantScope::resolve`); admin surface tenant-gate-only |
| esg | tenant-gate-only |
| exams | mismatch-only |
| forums | tenant-gate-only |
| inbox | tenant-gate + `tenant_visible` |
| kyc | tenant-gate + tenant-scoped loaders (404 cross-tenant) |
| library | strict 403-no-company (`CallerScope`) |
| payments | tenant-gate-only + verified-caller customer ownership |
| phone | tenant-gate + tenant-scoped loaders (404 cross-tenant) |
| plm | mismatch-only |
| referral | mismatch-only |
| rental | mismatch-only |
| sign | mismatch-only |
| sms_marketing | strict writes / mismatch-only reads |
| social_marketing | strict writes / mismatch-only reads |
| spreadsheets | strict writes / mismatch-only reads |
| store | mixed: strict seller/manage; buyer/owner gate + verified-owner; shared registries gate-only |
| files | tenant-gate via shared `authorize()` funnel + scope resolution |
| studio | strict 403-no-company + tenant gate |

### Money-path leaks

Closed: billing (6) — `list_invoice_templates` company filter
leak; delivery_notes list filters; subscriptions surface had no
auth at all; `from_order` had no owner check; print handlers
loaded invoices on `PERM_BILLING_PRINT` alone; global
side-effect scans now scoped to the caller. Payments (10) —
`replay_for_customer` idempotency keys were scoped to
(connection, key), not customer, so a replay could hand one
customer's transaction to another; foreign replays now return a
generic conflict (`create_charge`, `confirm_intent`, `pay_order`,
`refund_transaction`, `pay_split_part`). Store (4) — tier slug
and digital profile duplicates (parent scope check first, 409
only for a record the caller owns); product-has-tiers delete
conflict; `vendor_payout_summary` was wide open (no auth at
all), now gated + company-checked.
Remaining: billing cross-USER within a company on the print
surface (`PERM_BILLING_PRINT` is staff-level); billing
`reprint_invoice_print_job` takes only a job id (job
authorization lives in the printing module); entitlements admin
surface gate-only by design (task-315 decision); payments
`create_payout` replay (admin-only); payments guest caller
ownership (pre-existing guest checkout, now tenant-gated);
payments order-ownership verification (store owns orders);
payments record-level `tenant_id` stamping (C11); store coupon
code / catalog slug / digital type id duplicates (global
namespace by design, no company field — C11 schema change);
store `digital_caller` reads raw `X-User-Id`/`X-Role` headers
(`digital.rs:130-145`, deliberately unchanged — task-317
follow-up, mitigated by the C3 gate); store
`expire_due_subscriptions` sweeps all companies by design.

### Inbox `tenant_visible` — STILL OPEN (product decision)

`src/modules/inbox/auth.rs:94`: the `(record_company,
scope_company)` `_ => true` arm means a company-less scope sees
any company's threads within the tenant (tenant id must still
match). Group B guarded every inbox handler around it without
changing that behaviour. Do NOT close this item here; it needs
the user's product decision (known task-312 item).

### Files + studio rows (stale since Worker D)

- files: the row said "unguarded (identity-only, no tenant
  gate)". Group C verified the scope conversion is real
  (`resolve_caller_company`, no header trust) but found NO
  tenant gate: `authorize()` never resolved a tenant context, so
  a request with none proceeded as Unresolved/Platform. The
  coordinator fixed it in this task: new `require_tenant` in
  `src/modules/files/helpers.rs` (mail.rs pattern) called first
  in the shared `authorize()` funnel (`handlers.rs`), plus a
  fail-closed test. Verifier now reports 7/7 guarded,
  `no_tenant_gate=0`.
- studio: verify-only, conversion real and tenant-gated
  (`auth.rs:67` caller resolves tenant first, `:208`
  `authorize()`); 24 scope-checked, 7 gate-only, 1 delegated
  (`delete_definition` -> `archive_definition`).
- library: verify-only, `CallerScope` conversion real
  (`auth.rs:73` `require_tenant`, `:95` `caller_company`,
  header never trusted, pinned by tests); 65/65 guarded.

### Milestone

After this task the main table holds 149 module rows and **zero
tenant-facing rows remain unguarded** (control-plane and
infra-or-shared rows stay unguarded by design). The remaining
C3 open items are the follow-ups, not unguarded modules: task
315 (ControlPlaneGuard / platform-admin scope / write-strictness
harmonization), task 317 (header trust — done in its own pass;
the one production `.get("x-company-id")` left is the
timetracker claim exception), task 320 (scope-rule
unification), inbox `tenant_visible` (product decision above),
and C11 (task 271, real record-level partitioning for the
gate-only modules whose records carry no scope field).

### Verification (coordinator)

- All five group verifiers re-run by the coordinator after the
  final edits: every module exit 0, missing == 0, 0 order
  violations, 0 kind mismatches (payments run with its 4
  exclusions; sign/referral/store/sms/social/spreadsheets with
  theirs).
- `rustfmt --edition 2024 --emit stdout` parse-clean on the 2
  files the coordinator touched (`src/modules/files/helpers.rs`,
  `src/modules/files/handlers.rs`); the groups reported parse-clean
  on all of theirs (24 A, 25 B, 15 C, 26 D, 34 E).
- Root-bin files were NOT typechecked (`cargo check -p unified`
  OOM-kills this VM); all new guard tests are written but UNRUN.
- No header trust reintroduced: no production `X-Company-Id`
  reads in the 20 modules (only doc comments stating it is never
  trusted and test code asserting a forged header is ignored).
