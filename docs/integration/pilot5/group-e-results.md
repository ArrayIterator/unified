# C3 pilot 5 — Group E results (task 319)

Modules: `sms_marketing`, `social_marketing`, `spreadsheets`, `store`.
Task 319, existing tracker task (no new task created). No push, no new
dependencies, all code and docs in English.

## Counts

| Module | Routed | Guarded | Excluded | Missing | Reads | Writes |
|---|---|---|---|---|---|---|
| sms_marketing | 46 | 44 | 2 | 0 | 15 | 29 |
| social_marketing | 36 | 35 | 1 | 0 | 13 | 22 |
| spreadsheets | 31 | 30 | 1 | 0 | 14 | 16 |
| store | 83 | 76 | 7 | 0 | 28 | 48 |
| **Total** | **196** | **185** | **11** | **0** | **70** | **115** |

Counts are the verifier's output (routed = name appears in `.to(name)`;
reads/writes split by route method). Store guarded per file: `products.rs`
18, `cart.rs` 4, `orders.rs` 13, `catalog.rs` 5, `digital_products.rs` 14,
`digital_sales.rs` 15, `subscriptions.rs` 5, `reference.rs` 2.

## Guard model (shared)

Every guarded handler starts with `require_tenant(&req)?` (or the
match-form equivalent in direct handlers), before any identity or
capability call, and then resolves the company from the verified
caller — never from the `X-Company-Id` header (header trust was
removed by task 317 and is not reintroduced anywhere here).

- `require_tenant` — `TenantContext::from_http_request`: 401 without a
  resolved tenant context, never falls back to `public`.
- `caller_company(&tenant, &req)` — read gate: the verified caller's
  company, or `None` when the caller carries none; a record whose
  company differs from a resolved scope is rejected with 403.
- `require_caller_company(&tenant, &req)` — write gate: 403 when the
  caller carries no company anywhere (tenant context or verified
  caller).

Guard helpers live per module:

- `src/modules/sms_marketing/helpers.rs:94,108,129`
  (`require_tenant`, `caller_company`, `require_caller_company`)
- `src/modules/social_marketing/helpers.rs:93,107,128`
- `src/modules/spreadsheets/helpers.rs:95,109,130`
- `src/modules/store/helpers.rs:174,188,209,225,236,260,272,289,305,321,333`
  (plus `require_company_match`, `require_company_match_for`,
  `in_company_scope`, `require_record_in_company_scope`,
  `require_product_in_scope`, `require_digital_product_in_scope`,
  `entitlement_company`, `license_company`)

## sms_marketing — 44 guarded, 2 excluded

Records are partitioned by `tenant_id`, not `company_id` (55
`tenant_id:` struct fields; only 2 `company_id` mentions in the
module). The tenant gate is therefore the record partition; the
company check is a mismatch check on reads and a presence check on
writes. There is no per-record company field — scoping records by
company is a C11 follow-up, recorded in the helpers doc comment.

The old `resolve_tenant` (tenant-id string) helper is deleted; all
106 call sites now use `require_tenant` + `caller_company` /
`require_caller_company`. `ai_capability` is gated with the write
form before the `caller(&req)` match.

Exclusions:

- `delivery_webhook`, `inbound_webhook` (`handlers_optouts.rs`) —
  provider webhooks: the provider secret is verified via
  `require_webhook_secret`, the tenant is derived from the provider.
  `RouteAuth::Public` in the module route table
  (routes registered at `module.rs:230-236`).

## social_marketing — 35 guarded, 1 excluded

Same shape as sms_marketing: records partitioned by `tenant_id`
(40 `tenant_id:` struct fields), company check is mismatch-on-read /
presence-on-write, no per-record company field (C11 follow-up).

Exclusions:

- `metrics_ingest` (`handlers_metrics.rs`) — account webhook: the
  account webhook secret is verified via the
  `x-social-webhook-secret` header + `webhook_secret_matches`, the
  tenant is derived from the account. `RouteAuth::Public` in the
  module route table (route registered at `module.rs:196-198`).

## spreadsheets — 30 guarded, 1 excluded

Same shape as sms_marketing: records partitioned by `tenant_id`
(27 `tenant_id:` struct fields), company check is mismatch-on-read /
presence-on-write, no per-record company field (C11 follow-up).

Exclusions:

- `get_public_workbook` (`handlers_shares.rs`) — `RouteAuth::Public`
  (`module.rs:228`): the share-token hash is the credential, the
  response is read-only sheets + cells, and it never returns shares
  or the token hash. The handler takes no `HttpRequest`, so no gate
  can be applied.

## store — 76 guarded, 7 excluded

The store is a marketplace, so the scope model is mixed by design:

- **Seller / manage flows are scope-checked.** Product writes
  (create/update/delete/publish/unpublish) require the product's
  company in scope; tier writes go through the parent product
  (`require_product_in_scope`); coupon create/update verify every
  referenced `product_id` in scope; catalog create/update/delete
  verify the linked `product_id` in scope (update checks the
  existing link and the new link); digital products are scoped by
  `royalty_payee_company_id` (create adopts it via
  `require_company_match`, update/delete/publish verify it, files
  and previews go through the parent digital product);
  royalties/payouts are payee-or-manage, with the manage path
  scope-checked on `payee_company_id`; order update-to-`paid` and
  manual-pay manager paths scope-check the order company; manage
  list views (`list_orders`, `list_entitlements`,
  `list_subscriptions`, `list_license_downloads`,
  `list_digital_downloads`, `list_digital_royalties`,
  `list_digital_payouts`) are filtered to the caller's company.
- **Buyer / owner flows get the gate + the existing verified-owner
  check, with no vendor-company check** — the buyer legitimately
  buys from another company. Cart, checkout, order get/update/
  delete/complete/cancel/refund/download, entitlement get/download-
  link, subscription get/change/cancel, and the digital download
  link (paid-order check stays) are in this class.
- **Catalogue reads are public offers**: `list_products`,
  `get_product`, `list_tiers`, `get_tier`, `calculate_price` show
  published records to any tenant caller; drafts are visible only
  in scope via `in_company_scope`.
- **Shared registries are gate-only**: digital types, digital
  settings, watermark formats, reference settings, coupon list/get/
  delete (global code namespace, no company field).

Re-signed handlers (22, gained an `HttpRequest` parameter):
`products.rs` all 18, `catalog.rs` `list_catalog` +
`get_catalog_entry`, `subscriptions.rs` `list_subscriptions`,
`digital_sales.rs` `vendor_payout_summary`.

Signature changes (call sites updated, no other callers exist):

- `set_digital_product_status` (`digital_products.rs`) gained a
  `scope: &Option<String>` parameter and scope-checks the digital
  product's payee company inside; only used by
  `publish_digital_product` / `unpublish_digital_product`.
- `move_digital_payout` (`digital_sales.rs`) gained a
  `scope: &Option<String>` parameter and scope-checks the payout's
  `payee_company_id` inside; only used by `approve_digital_payout` /
  `mark_digital_payout_paid` / `reject_digital_payout`.

`list_subscriptions` was wide open (no `HttpRequest`, no auth at
all): re-signed, gated, manage view filtered to in-scope company,
non-manage view own-only.

`vendor_payout_summary` was wide open (no `HttpRequest`, no auth;
any path company returned that company's order totals): re-signed,
gated with `require_caller_company`, and the path `company_id`
must equal the caller's verified company (403 otherwise).

Exclusions:

- `list_digital_products`, `get_digital_product`,
  `list_digital_previews` (`digital_products.rs`) —
  `RouteAuth::Public` storefront (`routes.rs:257,259,266`):
  published-only / seller-allowed samples by design.
- `download_sample` (`digital_sales.rs`) — `RouteAuth::Public`
  (`routes.rs:268`): free sample when the seller allows it,
  rate-limited per identity and preview and logged.
- `signed_download` (`orders.rs`) — HMAC-signed expiring download
  token redemption (route `routes.rs:331`); the signature is the
  credential, the handler takes no `HttpRequest`. Parallel to the
  storage_manager `redeem_download` exclusion.
- `check_update`, `download_package` (`catalog.rs`) — license-key
  authenticated WordPress update API (routes `routes.rs:347-348`);
  the license key validated through the licensing hook is the
  credential, the handlers take no `HttpRequest`; free packages are
  public by design.

### Store money-path conflict leaks

Closed by a parent scope check (the 409 can now only fire for a
record the caller already owns):

| Leak | Location | Closed by |
|---|---|---|
| Tier slug duplicate per product | `src/modules/store/products.rs:582` | `create_tier` / `update_tier` require the parent product in scope |
| Digital profile duplicate per product | `src/modules/store/digital_products.rs:282` | `create_digital_product` requires the parent product in scope |
| Product-has-tiers delete conflict | `src/modules/store/products.rs:412` | `delete_product` requires the product in scope first (own-record state conflict after the scope check) |

Remaining — global namespace by design, no company field to scope
on (C11 schema change, not this task):

| Leak | Location | Why it remains |
|---|---|---|
| Coupon code duplicate | `src/modules/store/products.rs:847` | Redemption is by code alone; the code namespace is global |
| Catalog slug duplicate | `src/modules/store/catalog.rs:154` | The slug is the global update-API key |
| Digital product type id duplicate | `src/modules/store/digital_products.rs:106` | Types are a shared registry |

## Write-strictness table

| Module | Reads | Writes | Variant |
|---|---|---|---|
| sms_marketing | `caller_company`, mismatch → 403 | `require_caller_company`, 403 when the caller has no company | Strict on writes, mismatch-only on reads (tenant-partitioned records) |
| social_marketing | `caller_company`, mismatch → 403 | `require_caller_company`, 403 when the caller has no company | Strict on writes, mismatch-only on reads (tenant-partitioned records) |
| spreadsheets | `caller_company`, mismatch → 403 | `require_caller_company`, 403 when the caller has no company | Strict on writes, mismatch-only on reads (tenant-partitioned records) |
| store | Seller/manage reads: company scope check (`require_record_in_company_scope` / `in_company_scope`); buyer/owner reads: gate + verified-owner check; shared registries: gate-only | Seller/manage writes: strict scope check, 403 on mismatch or missing company; buyer/owner writes: gate + verified-owner check; shared registries: gate-only | Mixed — strict seller scope + gate-only buyer/owner + gate-only shared registry |

Harmonization is task 315, not this task; the variants above are
recorded as-is. Note in particular: a manage caller whose verified
identity carries no company gets 403 from `require_caller_company`
on scoped writes — whether platform admins without a company should
bypass that is a task 315 decision.

## Residuals

- `digital_caller` (`src/modules/store/digital.rs:130-145`) reads
  identity from the raw `X-User-Id` / `X-Role` headers, not the
  verified caller. Deliberately NOT changed here (auth-model change,
  task 317 follow-up). Mitigated by the C3 gate: every handler that
  reaches it now runs `require_tenant` + verified `caller_company`
  first.
- `expire_due_subscriptions` (`subscriptions.rs`) is a manager
  batch sweep across all companies by design; gate-only.
- `checkout` derives `order.company_id` from the first item's
  product company; a mixed-company order is attributed to the
  first item. Per-item companies are preserved on `OrderItem`.
- Global namespaces stay global (see the leak table): coupon codes,
  catalog slugs, digital product type ids.
- Royalties / payouts whose payee is a user with no
  `payee_company_id` are hidden from manage callers viewing another
  payee (fail-closed strict idiom); they stay visible to the payee
  themselves and to company-less platform admins.
- `list_products` shows published products to any tenant caller;
  drafts are visible only to callers in the owning company.

## Verifier

`docs/integration/pilot5/c3_group_e_verify.py` — adapted from
`docs/integration/pilot4/c3_group_a_verify.py` (same rules:
guarded = body contains `require_tenant(&req)`; guard precedes
identity/capability calls; GET → `caller_company`, mutations →
`require_caller_company`).

Adaptations, documented here as required:

1. `IDENTITY_CALLS` extended with the store's identity and
   capability calls: `require_digital_permission(`,
   `require_digital_manage(`, `require_digital_read(`,
   `require_digital_public(`, `digital_can_manage(`,
   `require_catalog_manage(`, `require_catalog_permission(`,
   `catalog_caller(`, `digital_caller(`, `caller_from(`,
   `caller_owns(`, `require_order_owner(`,
   `require_entitlement_owner(`, `require_subscription_owner(`,
   `owner_of(`, `record_visible_to_caller(`.
2. Write-rule substring note: `caller_company(` is a substring of
   `require_caller_company(`, so a GET handler guarded with the
   stricter `require_caller_company` (store
   `vendor_payout_summary`, which requires the caller's company
   and compares it to the path company) satisfies the read check.
   That is intentional strictness, not a loophole: the check
   verifies a company resolution is present, and the stricter
   form resolves one.
3. Route-method detection is unchanged: every store route uses
   `web::<method>().to(name)`.

Run results (exit 0 for every module):

```
sms_marketing: guarded=44 (reads=15, writes=29) missing=0 excluded=2 order_violations=0 write_rule_violations=0
social_marketing: guarded=35 (reads=13, writes=22) missing=0 excluded=1 order_violations=0 write_rule_violations=0
spreadsheets: guarded=30 (reads=14, writes=16) missing=0 excluded=1 order_violations=0 write_rule_violations=0
store: guarded=76 (reads=28, writes=48) missing=0 excluded=7 order_violations=0 write_rule_violations=0
```

## Verification

- `rustfmt --edition 2024 --emit stdout` (parse check) on all 34
  touched files: all parse. (`rustfmt` is not on `PATH` in a bare
  exec shell; `PATH="$HOME/.cargo/bin:$PATH"` first.)
- `rustfmt --edition 2024 --check` on all 34 touched files: **0
  diff lines touch a new identifier** (`require_tenant`,
  `caller_company`, `require_caller_company`, `in_company_scope`,
  `require_company_match*`, `require_record_in_company_scope`,
  `require_product_in_scope`, `require_digital_product_in_scope`,
  `entitlement_company`, `license_company`). Remaining `--check`
  diffs are pre-existing style drift (import-group ordering,
  long-line wrapping in untouched code); no whole-file format was
  applied. The import blocks and test asserts this task added were
  hand-adjusted to rustfmt's preferred form.
- Tests: new guard tests were added (4 per small module in
  `helpers.rs`; 7 in `src/modules/store/helpers.rs`; catalog test
  fixtures updated to attach a `TenantContext` + verified caller
  company `"company-1"` so existing expectations still hold).
  **All new/updated tests are UNRUN** — `cargo check -p unified`
  and `cargo test` on the root bin were not run (bin crate ~5.7 GiB
  RSS, OOM-killed on this VM). The root bin was NOT typechecked.
- No push. `docs/integration/C3_MODULE_GUARD_MAP.md` not edited.

## Files changed

Code (34 files):

- `src/modules/sms_marketing/helpers.rs`, `handlers_ai.rs`,
  `handlers_campaigns.rs`, `handlers_lists.rs`,
  `handlers_messages.rs`, `handlers_optouts.rs`,
  `handlers_providers.rs`, `handlers_settings.rs`,
  `handlers_templates.rs`
- `src/modules/social_marketing/helpers.rs`, `handlers_ai.rs`,
  `handlers_accounts.rs`, `handlers_audit.rs`,
  `handlers_campaigns.rs`, `handlers_metrics.rs`,
  `handlers_networks.rs`, `handlers_posts.rs`
- `src/modules/spreadsheets/helpers.rs`, `handlers_ai.rs`,
  `handlers_cells.rs`, `handlers_export.rs`,
  `handlers_settings.rs`, `handlers_shares.rs`,
  `handlers_versions.rs`, `handlers_workbooks.rs`
- `src/modules/store/helpers.rs`, `products.rs`, `cart.rs`,
  `orders.rs`, `catalog.rs`, `digital_products.rs`,
  `digital_sales.rs`, `subscriptions.rs`, `reference.rs`

Docs (new):

- `docs/integration/pilot5/group-e-results.md` (this file)
- `docs/integration/pilot5/c3_group_e_verify.py`
