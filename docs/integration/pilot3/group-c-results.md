# C3 Pilot 3 — Group C results (task 314)

Tenant edge guards for **ai, mcp, software, web3**. Work done 2026-11-23.
Coordinator owns the guard-map merge and task 314's final status; this file
is the group's evidence only. No push was made.

## Counts (from `c3_verify.py`, missing must be 0)

| Module | Guarded | Excluded | Missing | Routed total |
|--------|---------|----------|---------|--------------|
| ai | 51 | 0 | 0 | 51 |
| mcp | 7 | 5 | 0 | 12 |
| software | 44 | 2 | 0 | 46 |
| web3 | 31 | 1 | 0 | 32 |
| **Total** | **133** | **8** | **0** | **141** |

Note: the guard map listed ai as 49 handlers; the verifier counts 51 routed
handlers (the route list and the per-file `require_tenant(&req)` count agree
at 51 guarded + the unrouted shared `build_reply_draft` also gated). The
map's 49 is stale/undercounted by 2; the coordinator should take 51.

## Guard idiom

Every guarded handler starts with `require_tenant(&req)?` and resolves the
caller company from the VERIFIED Caller only — never from `X-Company-Id`
headers, never falling back to `public`. Shared rule (per task statement):
company callers read own + global; unresolved callers read global only;
writes require a resolved company. Create/update bodies are verified against
the caller company; an omitted body company adopts the caller scope (never
stored unscoped under a scoped caller); an explicit clear stays a clear.

Deviation from pilot 1, documented: pilot 1's `in_company_scope` hides
global records from scoped callers; the helpers written here implement the
task-stated rule (scoped caller sees own + global). The task statement was
treated as authoritative. The coordinator should confirm which rule is
canonical and align the helpers tree-wide.

## Per-module detail

### ai — 51 guarded, 0 excluded, 0 missing

Helpers added to `src/modules/ai/helpers.rs`: `require_tenant`,
`caller_company_uuid`, `in_company_scope_uuid`,
`require_record_in_company_scope_uuid`, plus 7 `tenant_guard_tests`
(incl. a header-ignored test and a test pinning the task-312
`require_company_scope` fail-closed behaviour for unresolved callers).

- `capabilities.rs` (3): gate-only. `run_capability_endpoint` gated at the
  top; its inner `resolve_capability_company` resolution is untouched
  (see residuals).
- `handlers_drafts.rs` (11 routed + shared `build_reply_draft` also gated):
  gate-only. Drafts carry no company field; actor ownership via
  `load_draft_for`; `ai:approve` reviewers see all drafts (capability arm,
  unchanged). `support_draft`/`chat_draft` each carry their own literal
  gate even though they delegate to `build_reply_draft`.
- `handlers_gating.rs` (5): `get_effective_gate`, `patch_company_gate`,
  `patch_user_gate` now use `caller_company_uuid(&_tenant, &req)?` instead
  of the header-first `resolve_caller_company`; `require_company_scope`
  checks kept. Global settings handlers gate-only.
- `handlers_personal.rs` (12): my-key / my-mcp handlers gate-only
  (user-scoped via `own_user_id` / `load_own_mcp_server`). `usage_by_user`
  and `usage_by_company` use the verified caller company (platform reader
  still names a company in the query; scoped callers pinned to their own).
  `test_my_key` gated at the top; it still uses `resolve_caller_company`
  for the `authorize_ai_call` company (inner machinery, see residuals).
- `handlers_providers.rs` (7): gate-only. Provider connections carry no
  company field. Shared `authorize_call` fn unchanged (see residuals).
- `handlers_usage.rs` (13): all 10 policy handlers use the verified caller
  company (`require_company_scope` checks kept). `list_usage`, `get_usage`,
  `usage_summary` filter/load-check by scope for scoped callers; see the
  platform-reader carve-out below.

AI usage platform-reader carve-out (documented, task-315 territory):
`list_usage` / `get_usage` / `usage_summary` keep full visibility for
unresolved callers holding `ai:read_usage`. Filtering unresolved readers to
global-only would make the platform usage-reader role see almost nothing
(most usage records carry a company). Scoped callers are filtered to
own + global. Revisit when task 315 decides whether an admin capability
implies cross-company access.

Task-312 consequence preserved: a verified platform admin with capability
`ai:manage_providers` and no company still gets 403 on AI policy
management (`require_company_scope` fail-closed). No hole was re-opened.

### mcp — 7 guarded, 5 excluded, 0 missing

Helpers added to `src/modules/mcp/validation.rs`: `require_tenant`,
`caller_company`, `require_company_match[_for]`, `in_company_scope`,
`require_record_in_company_scope`, `require_company_match_uuid`,
`require_server_in_scope` (missing and out-of-scope both read as 404),
plus 9 `tenant_guard_tests`.

- Guarded: `list_servers` (scope filter), `create_server` (body company
  verified via `require_company_match_uuid`; omitted company adopts caller
  scope), `get_server`, `update_server`, `delete_server` (via
  `require_server_in_scope`), `get_config`, `update_config` (gate-only).
- Excluded (see table below).

### software — 44 guarded, 2 excluded, 0 missing

Helpers added to `src/modules/software/helpers.rs`: String-scope helpers
(`require_tenant`, `caller_company`, `in_company_scope`,
`require_company_match[_for]`, `require_record_in_company_scope`) plus
parent-chain scope (`version_company`, `release_company`, `file_company`;
a standalone version or a missing parent chain counts as global) and
`require_software_in_scope` / `require_version_in_scope` /
`require_release_in_scope` / `require_file_in_scope`, plus 8
`tenant_guard_tests` (incl. version-inherits-parent-vendor).

- `handlers_software.rs` (13 guarded): list scope filter; create verifies
  vendor; get/update/delete/publish/unpublish scope-checked loads
  (out-of-scope reads as 404); version/release/tag sub-lists check the
  parent software scope; `get_stats` scopes the software-derived
  aggregates (by_status/by_kind counts, totals, downloads) via the
  parent chain, while taxonomy/tag counts stay global (shared catalogue).
  `UpdateSoftware.vendor_company_id: None` means "no change" (an explicit
  clear cannot be expressed); a supplied vendor is verified.
- `handlers_versions.rs` (12 guarded): list filters by parent scope;
  create/update verify the parent software scope; get/delete/publish/
  unpublish/release_files scope-checked loads. `download_release`
  excluded (see below).
- `handlers_taxonomy.rs` (14 guarded): gate-only — taxonomy nodes/tags
  carry no company field (shared catalogue).
- `handlers_files.rs` (5 guarded): create/get/update/delete/verify all
  scope-checked via the parent release/file chain.
- Test builders in `handlers_software.rs` and `handlers_versions.rs` now
  attach a `TenantContext` (pilot-1 pattern) so the existing handler tests
  exercise the gate instead of tripping it.

### web3 — 31 guarded, 1 excluded, 0 missing

Helpers added to `src/modules/web3/helpers.rs`: String-scope helpers
(`require_tenant`, `caller_company`, `in_company_scope`,
`require_company_match[_for]`, `require_record_in_company_scope`),
plus 7 `tenant_guard_tests`.

- Networks (7) + settings (2): gate-only (`ChainNetwork` / `Web3Settings`
  carry no company field).
- Wallets (8): list filter adds the scope check for non-admins; link
  verifies the body company; get/unlink/challenge/verify/list_balances/
  refresh add a non-admin scope check after the existing owner/admin check.
- Watched addresses (7): list filter; create verifies body company;
  delete/poll add non-admin scope checks; `poll_all_watched` gate-only
  (admin-only bulk); `list_transactions` restricts the non-admin
  `own_watched` set by scope; `get_transaction` checks the parent watched
  address company for non-admins.
- Anchors (7 guarded): create verifies body company; list/list_ledger
  filter; get/submit/refresh add non-admin scope checks; `verify_anchor`
  scope-filters candidate anchors for non-admins, so an out-of-scope
  anchor reads as "not-found", never 403 (no existence leak).
- `is_admin` arms unchanged (task-315 territory, same as pilot 2's
  storage_manager).

## Exclusions

| Module | Handler | Reason |
|--------|---------|--------|
| mcp | `get_server_public` | Public registry view: public + active servers only, public_view fields. |
| mcp | `list_tools` | Static catalogue, no `req`, no tenant data. |
| mcp | `list_resources` | Static catalogue, no `req`, no tenant data. |
| mcp | `list_prompts` | Static catalogue, no `req`, no tenant data. |
| mcp | `mcp_endpoint` | API-key token surface; per-server owner/scope auth inside `dispatch_rpc`; gating would break key clients on non-tenant hosts. Same treatment as pilot 1's timetracker `agent_*` token surface. |
| software | `latest_releases` | Public catalogue endpoint by design; `ApiAccessRule` outer gate; no caller. |
| software | `download_release` | Public delivery endpoint by design; license-key check; `ApiAccessRule` outer gate; no caller. |
| web3 | `public_ledger` | Anonymous read by design (must stay ungated per task). |

## Files touched (20)

- `src/modules/ai/helpers.rs` — helpers + 7 tests
- `src/modules/ai/capabilities.rs`
- `src/modules/ai/handlers_drafts.rs`
- `src/modules/ai/handlers_gating.rs`
- `src/modules/ai/handlers_personal.rs`
- `src/modules/ai/handlers_providers.rs`
- `src/modules/ai/handlers_usage.rs`
- `src/modules/mcp/validation.rs` — helpers + 9 tests
- `src/modules/mcp/handlers.rs`
- `src/modules/software/helpers.rs` — helpers + 8 tests
- `src/modules/software/handlers_software.rs`
- `src/modules/software/handlers_versions.rs`
- `src/modules/software/handlers_taxonomy.rs`
- `src/modules/software/handlers_files.rs`
- `src/modules/web3/helpers.rs` — helpers + 7 tests
- `src/modules/web3/handlers_networks.rs`
- `src/modules/web3/handlers_wallets.rs`
- `src/modules/web3/handlers_watched.rs`
- `src/modules/web3/handlers_anchors.rs`
- `src/modules/web3/handlers_settings.rs`

`docs/integration/C3_MODULE_GUARD_MAP.md` was NOT edited (coordinator merges).

## Verification

`c3_verify.py` (`~/workspace/c3-scripts/c3_verify.py`):

```text
ai: guarded=51 missing=0 excluded=0
mcp: guarded=7 missing=0 excluded=5
  EXCL: handlers.rs:get_server_public guarded=no (excluded)
  EXCL: handlers.rs:list_tools guarded=no (excluded)
  EXCL: handlers.rs:list_resources guarded=no (excluded)
  EXCL: handlers.rs:list_prompts guarded=no (excluded)
  EXCL: handlers.rs:mcp_endpoint guarded=no (excluded)
software: guarded=44 missing=0 excluded=2
  EXCL: handlers_software.rs:latest_releases guarded=no (excluded)
  EXCL: handlers_versions.rs:download_release guarded=no (excluded)
web3: guarded=31 missing=0 excluded=1
  EXCL: handlers_anchors.rs:public_ledger guarded=no (excluded)
```

`rustfmt --edition 2024 --emit stdout` parse check on all 20 touched
files: **20 ok, 0 fail**.

Cross-checks by grep: `require_tenant(&req)` occurs 52 times across the ai
handler files (51 routed handlers + the unrouted shared `build_reply_draft`),
7 in mcp, 44 across the four software handler files, 31 across the five
web3 handler files — matching the verifier counts. No
`resolve_caller_company` guard use remains in ai `handlers_gating.rs` or
`handlers_usage.rs`; in `handlers_personal.rs` it remains only for
`test_my_key`'s `authorize_ai_call` company (inner machinery) and in
`handlers_providers.rs` only inside the shared `authorize_call` fn.

List-filter precedence was checked by hand: the software list filters are
all conjunctive `&&` chains (`matches_search` takes one formatted string),
so the prepended scope check cannot be bypassed by a later `||` arm; the
ai usage filters parenthesise the platform-reader carve-out explicitly.

**Not typechecked, not run:** the root bin was NOT typechecked
(`cargo check -p unified` / `cargo test` deliberately not run — the bin
crate OOM-kills this VM) and no guard or unit test was executed. The new
and adjusted tests are UNRUN. In particular, name resolution that rustfmt
cannot catch (imports, helper signatures) was verified only by grep and by
re-reading the edited sites.

## Residuals / follow-ups for the coordinator

1. **AI inner call-authorization still resolves company via header/hook.**
   `authorize_ai_call` / `resolve_capability_company` (tenant-first, then
   header/hook fallback) and the `resolve_caller_company` uses in
   `test_my_key` and the shared `authorize_call` fn are untouched. Header
   trust is task 317's job; the C3 guards are the tenant gate plus
   verified-caller scope checks on the policy/gating/usage handlers.
2. **AI usage platform-reader carve-out** (see above) — needs the task-315
   platform decision to settle.
3. **Admin / capability arms unchanged** (task-315 territory): web3
   `is_admin` arms, ai `ai:approve` reviewers seeing all drafts,
   `ai:read_usage` platform readers.
4. **Shared-rule deviation from pilot 1** (see "Guard idiom") — needs a
   canonical decision.
5. **Software global-uniqueness leaks:** slug / name uniqueness checks can
   leak cross-company existence via conflict errors (same class of residual
   as pilot 2's bucket-name / product-slug / variant-SKU leaks).
6. **Software `get_stats` taxonomy/tag counts stay global** (shared
   catalogue) — by design, noted here.
7. **ai routed count is 51, not the map's 49** — the map row needs the
   corrected count.
8. `resolve_caller_company` itself was NOT deleted (public helper, used by
   other modules); only its use in ai guards was replaced.
