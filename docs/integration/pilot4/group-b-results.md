# C3 pilot 4, group B — results (tracker task 316)

Worker group B: `curriculum`, `supervision`, `community`, `todos`,
`contracts`, `departments`, `email_providers`, `mail`.
Date: 2026-11-23. No push. Task 316 itself was NOT marked done
(the coordinator decides its final status). The guard map
(`docs/integration/C3_MODULE_GUARD_MAP.md`) was NOT edited
(the coordinator updates it centrally).

## Totals

| Module | Routed | Guarded | Excluded | Missing | Order violations |
|---|---|---|---|---|---|
| curriculum | 72 | 72 | 0 | 0 | 0 |
| supervision | 44 | 44 (40 direct, 4 via delegation) | 0 | 0 | 0 |
| community | 38 | 36 | 2 | 0 | 0 |
| todos | 33 | 33 (31 direct, 2 via delegation) | 0 | 0 | 0 |
| contracts | 27 | 24 | 3 | 0 | 0 |
| departments | 16 | 16 | 0 | 0 | 0 |
| email_providers | 13 | 12 | 1 | 0 | 0 |
| mail | 7 | 7 | 0 | 0 | 0 |
| **Total** | **250** | **244** | **6** | **0** | **0** |

All 244 guarded handlers are gate-only: every module's records carry
no company field (map Scope column "none" for all eight), so no list
filter, record check, or body verify applies. Guard call sites:
241 direct (`let _tenant` / `let _scope` pairs) + 6 routed handlers
covered through 3 guarded shared helpers (see Delegation).

Count reconciliation against the guard-map table (all resolved):

- `email_providers`: map says 12 handlers, this pass counts 13
  routed. The map's inventory scanned `handlers*.rs` only and missed
  `test_connection_send`, which is defined in `module.rs` (it is now
  gated).
- `mail`: map says 6 handlers, this pass counts 7 routed. The map
  missed `ai_capability`, defined in `ai.rs`, not `handlers.rs`
  (it is now gated; it delegates to the AI module's gated runner).
- `supervision`: map says 46, this pass counts 44 routed + the 2
  unrouted delegation helpers (`decide_inspection`,
  `decide_progress_report`), which are the other 2 functions the
  map counted.
- `todos`: map says 34, this pass counts 33 routed + the 1 unrouted
  delegation helper (`set_list_archived`).
- The map's `.to(name)` inventory also produced one false positive
  in `mail` (the string `"to"` from a non-route `.to(` call); it is
  not a handler and is not counted here.

## Guard idiom

Same as pilots 1–3, verified-caller company resolution (never the
`X-Company-Id` header): every tenant-data handler calls
`require_tenant(&req)?` FIRST, then
`caller_company(&_tenant, &req)?`, before any identity or capability
call. The two helpers were added per module, copied from the pilot-2
`products/helpers.rs` shape:

- `require_tenant` wraps
  `unified_core::tenancy::TenantContext::from_http_request(req)` —
  401 with no resolved tenant context, never falls back to the
  `public` schema.
- `caller_company` resolves tenant-context company first, the
  verified caller's server-resolved company otherwise, and fails
  closed (403) when both are present and disagree. The
  `X-Company-Id` header is never read: no `x-company-id` header
  access exists anywhere in these eight modules (verified by grep),
  and none was added.

Helper locations: `helpers.rs` (curriculum as `pub(super)`,
supervision / community / contracts / departments as `pub(crate)`),
`todos/common.rs`, `email_providers/handlers.rs`, `mail.rs` root
(`pub(crate)`, imported by `mail/handlers.rs` and `mail/ai.rs`).
Because the records carry no company field, `_scope` is bound and
unused in every handler — the guard is a fail-closed tenant gate
only. Real partitioning needs a `tenant_id` on the records, a C11
follow-up (out of scope; no fields were added).

## Exclusions (documented public / token surfaces, NOT gated)

- community `list_public_topics`, `get_public_topic` — anonymous
  public surface by design: no `HttpRequest`, no caller, and both
  filter to `CategoryVisibility::Public` categories only (non-public
  topics answer 404). Gating would break the public community read
  surface.
- contracts `get_signing_link`, `sign_via_link`, `decline_via_link`
  — token-gated signing links by design (see the module header and
  the map note): no capability check, the unguessable signing token
  IS the credential; unknown/expired/revoked tokens all answer 404
  with the same message. Gating would break anonymous signers.
  `get_signing_link` takes `HttpRequest` only for audit metadata
  (IP / user-agent via `record_audit`).
- email_providers `list_catalog` — static catalogue (provider
  kinds, capabilities, settings fields, domain/webhook notes), no
  tenant data, no `HttpRequest` — same exclusion shape as pilot 2's
  `list_methodologies`.

No other handler in the eight modules is a public surface.

## Per-module detail

### curriculum — 72 guarded, 0 excluded, 0 missing (72 gate-only)

Handlers are `pub(super)` (not `pub(crate)`); the verifier was
adapted for that. All 72 take `HttpRequest` and use the closure
pattern (`let caller = caller(&req)?;` inside `(|| { ... })()`),
except `ai_capability`, which uses `let caller = match
caller(&req) { ... }` — the guard was inserted before the match.
Guard lines sit first inside each closure, before `caller()` and
`require_permission`. One handler, `create_level` in `levels.rs`,
was reflowed to rustfmt's preferred form (`let result ... =\n
(|| {` with the closure body indented one level deeper) because
rustfmt wants that closure wrapped once the guard lines are in.

### supervision — 44 guarded, 0 excluded, 0 missing (40 gate-only + 4 via delegation)

40 routed handlers gated directly (closure pattern, guard first
inside the closure). The 4 approve/return handlers carry no
identity call of their own — they delegate to the shared helpers,
which are guarded at their top (direct style, before `caller()`):

- `approve_inspection` / `return_inspection` → `decide_inspection`
  (guarded, `handlers_inspections.rs`)
- `approve_progress_report` / `return_progress_report` →
  `decide_progress_report` (guarded, `handlers_reports.rs`)

### community — 36 guarded, 2 excluded, 0 missing (36 gate-only)

36 routed handlers gated directly (closure pattern). The 2 public
topic handlers are excluded (see Exclusions). The three handler
files import the helpers explicitly from `super::helpers`
(`caller, caller_company, require_tenant, ...`); the import lists
were reflowed to rustfmt's preferred multi-line form.

### todos — 33 guarded, 0 excluded, 0 missing (31 gate-only + 2 via delegation)

31 routed handlers gated directly (closure pattern, guard first
inside the closure), including AI draft handlers `ai_summarize` /
`ai_plan_day` (they render only the caller's own todos). The 2
archive handlers carry no identity call of their own — they
delegate to the shared helper, which is guarded at its top:

- `archive_list` / `unarchive_list` → `set_list_archived`
  (guarded, `handlers_lists.rs`)

### contracts — 24 guarded, 3 excluded, 0 missing (24 gate-only)

24 routed handlers gated directly (closure pattern). The 3
token-gated signing-link handlers are excluded (see Exclusions).
Helpers are re-exported through the module root's explicit
`pub(crate) use self::helpers::{...}` list, so the handler files'
`use super::*;` resolves them.

### departments — 16 guarded, 0 excluded, 0 missing (16 gate-only)

All 16 handlers use the `authorize(&ctx, &req, PERM_...)` pattern
(direct style, no closure); the guard was inserted immediately
before the `if let Err(error) = authorize(...)` line in each
handler. Both handler files import the helpers explicitly from
`super::helpers`; the import lists were reflowed to rustfmt's
preferred multi-line form.

### email_providers — 12 guarded, 1 excluded, 0 missing (12 gate-only)

8 handlers with `HttpRequest` gated directly, before
`require_permission(&state, &req, PERM_EMAIL_PROVIDERS_MANAGE)`:
`create_connection`, `update_connection`, `delete_connection`,
`test_connection_handler`, `send_mail_handler`,
`send_batch_handler`, `set_default_connection`,
`test_connection_send` (in `module.rs`).
4 read handlers took no `HttpRequest` and performed no caller
check at all, despite `RouteAuth::Authenticated` on their routes —
they were re-signed with an added `req: HttpRequest` parameter and
gated at the top of the body (actix-web extractor, no other Rust
caller exists — verified by grep): `list_connections`,
`get_connection_handler`, `list_send_log`, `get_default_connection`.
`list_catalog` is excluded as a static catalogue (see Exclusions).
Note for the coordinator: the four re-signed reads previously
returned masked connection views / send log with no authentication
beyond the route metadata; they now require a resolved tenant
context.

### mail — 7 guarded, 0 excluded, 0 missing (7 gate-only)

- `get_settings`, `update_settings`: guard inserted before the
  `require_caller(&req)` check (settings include the stored SMTP
  password; already admin-only per the C2 comment).
- `ai_capability`: guard inserted before `ai::caller(&req)`.
- 4 handlers took no `HttpRequest` and performed no caller check
  at all — they were re-signed with an added `req: HttpRequest`
  parameter and gated at the top of the body (actix-web extractor,
  no other Rust caller exists, and no internal HTTP self-call to
  these routes exists — verified by grep): `send_mail`, `test_mail`,
  `list_log`, `list_connectors`. Note for the coordinator:
  `send_mail` / `test_mail` previously sent mail through the
  configured SMTP connector with no authentication on the HTTP
  route; they now require a resolved tenant context. Other modules
  send mail through the `mail.send_requested` hook, not these
  routes, so the hook path is unaffected.

## Files touched (45 source files)

Helpers added (8 files): `curriculum/helpers.rs`,
`supervision/helpers.rs`, `community/helpers.rs`,
`todos/common.rs`, `contracts/helpers.rs`,
`departments/helpers.rs`, `email_providers/handlers.rs`,
`mail.rs`.

Import wiring (3 files beyond the handler files):
`contracts.rs` (helpers re-export list),
`mail/handlers.rs` + `mail/ai.rs` (`use super::{caller_company,
require_tenant};` — note the import-block splice also sorted the
`use` blocks in the 3 community handler files, the 2 departments
handler files, and `mail/handlers.rs` to rustfmt's preferred form;
only the import blocks were reformatted, no other code).

Handler files with guard insertions (37 files):
`curriculum/ai_capabilities.rs`, `assessment_domains.rs`,
`basic_competencies.rs`, `core_competencies.rs`, `frameworks.rs`,
`grade_component_mappings.rs`, `grade_descriptors.rs`,
`interventions.rs`, `lesson_plans.rs`, `levels.rs`,
`report_card_entries.rs`, `report_cards.rs`, `settings.rs`,
`structures.rs`, `subjects.rs`; `supervision/handlers_assignments.rs`,
`handlers_defects.rs`, `handlers_inspections.rs`, `handlers_logs.rs`,
`handlers_reports.rs`; `community/handlers_categories.rs`,
`handlers_members.rs`, `handlers_posts.rs`; `todos/handlers_ai.rs`,
`handlers_lists.rs`, `handlers_subtasks.rs`, `handlers_todos.rs`;
`contracts/contract_handlers.rs`, `settings_handlers.rs`,
`signers.rs`, `templates.rs`; `departments/handlers_departments.rs`,
`handlers_routing.rs`; `email_providers/handlers.rs`,
`email_providers/module.rs`; `mail/handlers.rs`, `mail/ai.rs`.

Verifier: `docs/integration/pilot4/c3_group_b_verify.py` (adapted
from pilot 3's `c3_group_a_verify.py`: `pub(super)` handlers,
delegation map, `authorize` / `require_caller` / `ai::caller`
identity calls).

## Verification

`docs/integration/pilot4/c3_group_b_verify.py` re-run after the
final edits, per module (exit 0 for all eight):

- `curriculum: guarded=72 (scope-checked=0, gate-only=72,
  via-delegation=0) missing=0 excluded=0 order_violations=0`
- `supervision: guarded=44 (scope-checked=0, gate-only=40,
  via-delegation=4) missing=0 excluded=0 order_violations=0`
- `community: guarded=36 (scope-checked=0, gate-only=36,
  via-delegation=0) missing=0 excluded=2 order_violations=0`
- `todos: guarded=33 (scope-checked=0, gate-only=31,
  via-delegation=2) missing=0 excluded=0 order_violations=0`
- `contracts: guarded=24 (scope-checked=0, gate-only=24,
  via-delegation=0) missing=0 excluded=3 order_violations=0`
- `departments: guarded=16 (scope-checked=0, gate-only=16,
  via-delegation=0) missing=0 excluded=0 order_violations=0`
- `email_providers: guarded=12 (scope-checked=0, gate-only=12,
  via-delegation=0) missing=0 excluded=1 order_violations=0`
- `mail: guarded=7 (scope-checked=0, gate-only=7,
  via-delegation=0) missing=0 excluded=0 order_violations=0`

Additional checks, all passing:

- Exactly one guard call site per guarded handler body (delegating
  callers have zero in their own body and are covered through the
  guarded helper; excluded handlers have zero).
- Name-resolution check: every file with a call site resolves
  `require_tenant` / `caller_company` — defined in-file, glob
  import (`use super::helpers::*;` / `use super::*;` with the
  module-root re-export), or explicit import. `TenantContext`
  carries `company_id: Option<String>`
  (`crates/unified-core/src/tenancy.rs:253`), matching the
  `caller_company` signature.
- `rustfmt --edition 2024 --emit stdout` (with
  `PATH="$HOME/.cargo/bin:$PATH"`): all 37 call-site files parse
  clean. `rustfmt --check` still fails on these files, as before
  (pre-existing style: unsorted imports, long lines, chain
  formatting) — whole-file formatting was NOT applied. After
  reflowing the one `create_level` closure and the six edited
  import blocks to rustfmt's preferred form, **zero** diff lines
  mentioning a new identifier (`require_tenant`, `_tenant`,
  `_scope`, `caller_company`) remain in `rustfmt --check` output.
- No `X-Company-Id` header is read anywhere in these eight
  modules, and no `public`-schema fallback was added.

**NOT verified: the guards were NOT typechecked (`cargo check`
was not run on the root bin — the bin crate is ~5.7 GiB RSS and
OOM-killed on this VM) and NOT run (`cargo test` not run).** The
root-bin files were NOT typechecked here; the gates are unrun.
Name resolution was verified only by the import-tracing check
above, not by a compiler.

## Residuals (honest, not partitioned by the guard alone)

- All eight modules are gate-only: a resolved tenant can still
  read/write every record in the module, because the records carry
  no company/tenant field to partition on. Real partitioning needs
  a `tenant_id` on the records (C11 follow-up).
- `mail` and `email_providers` reads/sends that previously ran
  with no caller check now require a resolved tenant context
  (see the per-module notes above). If any non-HTTP or
  pre-authenticated caller relied on those routes, it will now get
  401/403 — that is the intended fail-closed behaviour, but it is a
  behaviour change worth a live smoke test.
- The `company` module's gate-only model was followed, but unlike
  `company` (whose records ARE the scope), these modules' records
  are ordinary tenant data with no scope at all — the gate is
  strictly weaker here until C11 lands.
