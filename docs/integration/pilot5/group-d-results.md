# C3 Pilot 5 — Worker Group D Results (tracker task 319)

Tenant edge guards for `plm`, `referral`, `rental`, `sign`.
Local tree: `/home/hatch/workspace/unified-push`. The guard map
(`docs/integration/C3_MODULE_GUARD_MAP.md`) was NOT edited; the
coordinator merges all groups' rows.

## Summary

| Module | Routed | Guarded | Excluded | Missing |
| --- | --- | --- | --- | --- |
| plm | 29 | 29 | 0 | 0 |
| referral | 37 | 36 | 1 | 0 |
| rental | 31 | 31 | 0 | 0 |
| sign | 30 | 26 | 4 | 0 |
| **Total** | **127** | **122** | **5** | **0** |

Routed counts verified by counting `.to(...)` route targets in each
module's `module.rs`; they match the `pub(crate) async fn` handler
counts exactly (29 / 37 / 31 / 30), so no qualified-route undercount
of the pilot-3 analytics kind exists here. No `.to(...)` target is
qualified (`super::...::name`) in any of the four modules.

## Correction to the guard map's premise (verified, not assumed)

Like the 9 already-gated modules the map documents, these four
modules were **already tenant-gated at handler level before this
task**: every routed handler except the 5 exclusions below already
called the pre-existing fail-closed `resolve_tenant(&req)?`
(`TenantContext::from_http_request`, 401 without resolved context,
never falls back to `public`) as the FIRST statement of its result
closure, before `caller(&req)` / `require_permission`, and every
record already carries a required `tenant_id: String` stamped from
the resolved tenant on create. Partitioning was already real:
single-record loads go through `load_*` helpers that 404 on a
cross-tenant row (`tenant_match`), and list handlers filter by
`tenant_id == tenant_id`. What C3 adds here is the canonical
`require_tenant(&req)?` (returning the `TenantContext`, same
underlying resolution) plus the verified-caller company-mismatch
check (`caller_company`), per the pilot idiom. No handler logic,
route, or record shape was changed; the guard insertion is purely
additive (120 closure sites + 1 fn-top site + 1 shared delegate).

## Guard idiom (all four modules)

In every guarded handler, the two guard lines run FIRST — before
any identity or capability call:

```rust
let _tenant = require_tenant(&req)?;
let _scope = caller_company(&_tenant, &req)?;
```

followed by the pre-existing `let tenant_id = resolve_tenant(&req)?;`
(which performs the same `TenantContext::from_http_request`
resolution and is kept as-is, so the `tenant_id` string derivation
is untouched). Both helpers are added per module in `helpers.rs`
(same text in each, copied from `elearning/helpers.rs` for
`require_tenant` and `mail.rs` / `elearning/helpers.rs` for
`caller_company`):

- `require_tenant(req) -> UnifiedResult<TenantContext>` =
  `TenantContext::from_http_request(req)`: no resolved tenant
  context fails closed (401), never falls back to `public`.
- `caller_company(tenant, req) -> UnifiedResult<Option<String>>`:
  tenant-context company first, server-resolved verified-caller
  company second, 403 on mismatch. The `X-Company-Id` header is
  never read (task 317 removed that trust; no helper here
  reintroduces it).

Scope helpers are the modules' pre-existing tenant loaders
(`load_stage` / `load_version` / `load_eco` / `load_approval` /
`load_bom_version`, `load_link` / `load_referral` / `load_reward` /
`load_rule` / `load_stage` / `load_gaming_rule`,
`load_listing` / `load_listing_by_slug` / `load_rate` /
`load_order` / `load_delivery`, `load_envelope` / `load_signer` /
`load_template` / `load_signer_by_token`) plus the list filters on
`tenant_id`. No `company_id` field exists anywhere in these four
modules (verified by grep over all four `types.rs` files), so there
is no `in_company_scope` / `require_company_match` / `require_record`
helper to add — the tenant loaders already perform the equivalent
single-record check (404 on cross-tenant rows, so existence does
not leak across tenants).

## Write strictness (all four modules: mismatch-only)

| Module | Variant | `require_company` on writes? |
| --- | --- | --- |
| plm | tenant-gate + caller_company mismatch-only | No |
| referral | tenant-gate + caller_company mismatch-only | No |
| rental | tenant-gate + caller_company mismatch-only | No |
| sign | tenant-gate + caller_company mismatch-only | No |

No module uses the strict 403-no-company variant (pilot 4 group A
`require_company`) and none uses tenant-gate-only. Reason, same in
all four: records carry a required `tenant_id` and no `company_id`,
while both `TenantContext.company_id` and the verified caller's
`company_id` are `Option` — a strict "every write must resolve a
company" rule would 403 legitimate tenant writes whose tenant
context / verified caller carries no company, even though the write
is fully tenant-partitioned by `tenant_id`. The mismatch check is
the gate-only analog for writes here: a request naming two different
companies (tenant context vs verified caller) is 403 in reads and
writes alike. Whether a company-less caller may write is a task-315
(ControlPlaneGuard) decision, as in pilot 4. Harmonization is task
315, NOT this task — the variant is recorded here, not changed.

## Per-module detail

### plm — 29 guarded, 0 excluded, 0 missing

- Scoped 27, gate-only 2. The gate-only handlers are
  `get_settings` / `update_settings`: the settings value is a
  single global `RwLock<PlmSettings>` (`state.rs:30,83`), shared
  across tenants with no `tenant_id` on the value, so the tenant
  gate is the only applicable guard (same shape as the pilot-4
  group-D gate-only modules; per-tenant settings is a C11
  follow-up).
- All other handlers are scoped via `tenant_id`: lists filter by
  it, single loads use `load_stage` / `load_stage_by_slug` /
  `load_version` / `load_eco` / `load_approval` /
  `load_bom_version` (all `tenant_match` → 404), creates stamp it
  (`tenant_id: tenant_id.clone()`), `next_eco_code` /
  `max_bom_version` / `approved_version_approvals` count within it.
- `delete_stage` is the one handler whose result closure opens on
  its own line (`let result: UnifiedResult<Value> =\n (|| {`);
  its guard lines carry the 12-space indent, verified by inspection.
- Files: `helpers.rs`, `handlers_admin.rs`,
  `handlers_bom.rs`, `handlers_ecos.rs`, `handlers_stages.rs`,
  `handlers_versions.rs`.

### referral — 36 guarded, 1 excluded, 0 missing

- Scoped 36, gate-only 0. Lists filter by `tenant_id`
  (`referrals_of` / `rewards_of` / `links_of` / `reward_rules_of` /
  `stages_of` / `anti_gaming_rules_of` / `audit_of` in `state.rs`);
  single loads use `load_link` / `load_referral` / `load_reward` /
  `load_rule` / `load_stage` / `load_gaming_rule` (all
  `tenant_match` → 404); eligibility / reward evaluation is
  tenant-scoped (`eligibility_for_job`, `reward_rule_for_job`,
  `default_reward_rule`).
- `ai_capability` (POST `/ai/{capability}`) is the only fn-top
  handler in the group: it has no result closure, so the guard is
  a match-style block right after `let debug = ctx.is_debug();`,
  before `caller(&req)` / `require_permission` / `ai::caller` /
  `ai::require_permission`. The AI runner's own company
  resolution (`ai::resolve_capability_company`) and allowlist gate
  (`run_ai_capability`) are kept as-is; grounding facts are built
  tenant-scoped (`grounding_facts(state, tenant_id, job_id)`).
- Excluded (1): `click_link` — anonymous referral-link click
  tracking, no `HttpRequest` param; the link token hash IS the
  credential (`link_for_token_hash`), same class as affiliates'
  `track_click` in pilot 3. Gating it would break anonymous
  referral attribution.
- Files: `helpers.rs`, `handlers_ai.rs`, `handlers_audit.rs`,
  `handlers_gaming.rs`, `handlers_jobs.rs`, `handlers_links.rs`,
  `handlers_referrals.rs`, `handlers_rewards.rs`,
  `handlers_rules.rs`, `handlers_stages.rs`.

### rental — 31 guarded, 0 excluded, 0 missing

- Scoped 29, gate-only 2. The gate-only handlers are
  `get_settings` / `update_settings`: the settings value is a
  single global `RwLock<RentalSettings>` (`state.rs:29,81`), as in
  plm.
- The public catalogue handlers are GUARDED, not excluded:
  `list_public_listings` / `get_public_listing` already resolved
  the tenant and filter by `tenant_id` + `active` only
  (`load_listing_by_slug` + active check); they now also resolve
  `caller_company`, which is safe for anonymous callers
  (`caller_from` returns the anonymous caller, company `None`, so
  no mismatch can fire). Public by tenant + active, not public by
  the world.
- All other handlers are scoped via `tenant_id`: lists filter by
  it, single loads use `load_listing` / `load_listing_by_slug` /
  `load_rate` / `load_order` / `load_delivery` (`tenant_match` →
  404), availability / booking windows are computed within the
  tenant (`booked_periods`, `check_availability`).
- Files: `helpers.rs`, `handlers_fulfilment.rs`,
  `handlers_listings.rs`, `handlers_orders.rs`,
  `handlers_settings.rs`.

### sign — 26 guarded, 4 excluded, 0 missing

- Scoped 26, gate-only 0. Lists filter by `tenant_id`, single
  loads use `load_envelope` / `load_signer` / `load_template`
  (`tenant_match` → 404), settings are per-tenant
  (`settings_for(&tenant_id)`), audit / reminders are per-envelope
  (hence per-tenant).
- `cancel_envelope` / `void_envelope` are guarded via delegation
  to the shared `transition_terminal` helper, which gates first
  (guard lines at its closure top, before `caller` /
  `require_permission` / `load_envelope`); the verifier checks the
  delegate's body for both. This matches the pilot-3 elearning
  (`transition_course`) and pilot-4 supervision
  (`decide_inspection`) delegation shapes. The delegate also
  revokes outstanding signing tokens on transition.
- Excluded (4): `get_sign_page`, `post_sign`, `post_decline`
  (signing-link surface: the URL token IS the credential,
  `load_signer_by_token` + `valid_token_shape`; handlers take a
  `HttpRequest` they deliberately ignore, `let _ = &req;`) and
  `verify_certificate` (public certificate verification by SHA-256
  hash; same `let _ = &req;` shape). Gating any of them on tenant
  context would break external signers, who have no tenant
  session — same class as contracts' signing links in pilot 4.
  Note `post_sign` / `post_decline` are writes with no caller
  identity at all, only the token; that is by design of the
  signing-link surface.
- Files: `helpers.rs`, `handlers_envelopes.rs`,
  `handlers_settings.rs`, `handlers_signers.rs`,
  `handlers_templates.rs`.

## Exclusions (all 5, documented)

| Handler | Module | Reason |
| --- | --- | --- |
| `click_link` | referral | Anonymous click tracking; no `HttpRequest`; token hash IS the credential |
| `get_sign_page` | sign | Signing-link surface; token IS the credential; `req` deliberately unused |
| `post_sign` | sign | Signing-link write; token IS the credential; no caller identity by design |
| `post_decline` | sign | Signing-link write; token IS the credential; no caller identity by design |
| `verify_certificate` | sign | Public certificate verification by SHA-256 hash; `req` deliberately unused |

## Verification

- Guard counts: `docs/integration/pilot5/c3_group_d_verify.py`
  (adapted from pilot 4's `c3_group_a_verify.py`) re-run per
  module after the final edits — every module reports `missing=0`,
  `order_violations=0`, `kind_mismatches=0`, `double_guards=0`.
  Final re-run output (2026-11-23):

  ```
  plm: guarded=29 (reads=12, writes=17) missing=0 excluded=0 order_violations=0 kind_mismatches=0 double_guards=0
  referral: guarded=36 (reads=16, writes=20) missing=0 excluded=1 order_violations=0 kind_mismatches=0 double_guards=0
  rental: guarded=31 (reads=13, writes=18) missing=0 excluded=0 order_violations=0 kind_mismatches=0 double_guards=0
  sign: guarded=26 (reads=10, writes=16) missing=0 excluded=4 order_violations=0 kind_mismatches=0 double_guards=0
  ```

  Verifier adaptations (documented in its docstring, as the task
  requires): the kind check is mismatch-only — every guarded
  handler must resolve `caller_company(&_tenant, &req)` and
  `require_company` must NOT appear (the template's GET-vs-mutation
  split does not apply: no `company_id` on any record, writes are
  tenant-partitioned by `tenant_id`); sign's `cancel_envelope` /
  `void_envelope` are checked via the `transition_terminal`
  delegate body.
- Counts reconcile: 120 closure guard sites + 1 fn-top site
  (`ai_capability`) + 1 shared delegate site
  (`transition_terminal`, covering 2 routed handlers) = 122
  guarded handlers; `require_tenant(&req)` occurs 137 times in the
  four modules (121 handler sites + 16 test calls — the 5th test
  per module calls `require_tenant(&bare)`), `caller_company`
  121 times in handlers.
- `rustfmt --edition 2024 --emit stdout` (parse-level, with
  `PATH="$HOME/.cargo/bin:$PATH"`) on all 26 touched files: all
  parse clean. `rustfmt --edition 2024 --check` still fails on
  these files, but only on pre-existing style drift (unsorted `use`
  blocks, long lines — the files were already not rustfmt-clean
  before this work, same as prior pilots); after reflowing the
  `caller_company` match arm to rustfmt's single-line form in all
  4 helpers files and 2 import blocks to rustfmt's exact packing,
  **zero `--check` diff lines mention a new identifier**
  (`require_tenant`, `caller_company`, `tenant_company`,
  `verified_company`, `_tenant`, `_scope`). Whole-file formatting
  was NOT applied (it would swamp the diff).
- Import resolution was audited per file: every handler file's
  explicit `use super::helpers::{...}` list gained exactly
  `caller_company` + `require_tenant` (verified by name-set diff
  against a pre-edit backup); nothing was removed or reordered
  beyond the two insertions and the two rustfmt reflows above.
- Leftover-header check: the only `x-company-id` / `X-Company-Id`
  mentions in the four modules are the doc comment on
  `caller_company` (stating the header is never trusted) and the
  per-module test proving a forged header is ignored. Zero
  production header reads.
- Unit tests added (fail-closed / resolve / tenant-prefers /
  mismatch-403 / header-never-trusted) in a new
  `tenant_guard_tests` module in each `helpers.rs` (referral had
  no test module before; the other three gained a second one).
  The request builder is copied verbatim from
  `elearning/helpers.rs` (`TestRequest` + middleware
  `TenantContext` + `auth::Caller::verified(...).with_company(...)`).

## NOT verified

- Root-bin files were NOT typechecked: `cargo check -p unified`
  and `cargo test` on the root bin were not run (bin crate ~5.7 GiB
  RSS, OOM-killed on this VM).
- The new unit tests are UNRUN.
- No live render check applies (Rust service, not a theme).

## Residuals (honest, not partitioned by the guard alone)

- Partitioning is by `tenant_id`, not by company: two companies
  sharing one tenant are not separated by this guard, because no
  record carries a `company_id`. Company-level partitioning would
  need a `company_id` stamped on records (C11 follow-up).
- plm / rental settings values are global (single `RwLock`,
  shared across tenants); the gate is the only guard that applies
  to them. Per-tenant settings is a C11 / product follow-up.
- sign's token surface (`get_sign_page` / `post_sign` /
  `post_decline`) stays ungated by design; the token is the only
  credential, and token revocation on cancel/void is the
  compensating control. `verify_certificate` is public by design.
- referral's `click_link` stays an anonymous counter increment;
  the token hash is the only credential.
- Platform admins / callers with no resolvable company can read
  and write (the gate only requires a resolved tenant + a
  non-mismatching company). Whether a company-less caller may
  write is a task-315 decision.

## Files touched (28)

- plm (6): `helpers.rs`, `handlers_admin.rs`,
  `handlers_bom.rs`, `handlers_ecos.rs`, `handlers_stages.rs`,
  `handlers_versions.rs`.
- referral (10): `helpers.rs`, `handlers_ai.rs`,
  `handlers_audit.rs`, `handlers_gaming.rs`, `handlers_jobs.rs`,
  `handlers_links.rs`, `handlers_referrals.rs`,
  `handlers_rewards.rs`, `handlers_rules.rs`, `handlers_stages.rs`.
- rental (5): `helpers.rs`, `handlers_fulfilment.rs`,
  `handlers_listings.rs`, `handlers_orders.rs`,
  `handlers_settings.rs`.
- sign (5): `helpers.rs`, `handlers_envelopes.rs`,
  `handlers_settings.rs`, `handlers_signers.rs`,
  `handlers_templates.rs`.
- Plus this results file and `c3_group_d_verify.py` in
  `docs/integration/pilot5/`. No push, no new dependencies, all
  English.
