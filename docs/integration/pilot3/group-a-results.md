# C3 pilot 3, group A — results (tracker task 314)

Worker group A: `pos`, `pricing_tables`, `loyalty`, `affiliates`.
Date: 2026-11-23. No push. Task 314 itself was NOT marked done
(the coordinator decides its final status).

## Totals

| Module | Guarded | Excluded | Missing | Scope-checked | Gate-only |
|---|---|---|---|---|---|
| pos | 35 | 0 | 0 | 16 | 19 |
| pricing_tables | 47 | 2 | 0 | 26 | 21 |
| loyalty | 26 | 0 | 0 | 26 | 0 |
| affiliates | 29 | 1 | 0 | 29 | 0 |
| **Total** | **137** | **3** | **0** | **97** | **40** |

## Guard idiom

Same as pilots 1–2, with the pilot-2 verified-caller company
resolution (never the `X-Company-Id` header): every tenant-data
handler calls `require_tenant(&req)?` FIRST, then
`caller_company(&_tenant, &req)?`, before any identity or
capability call. Record access follows the shared rule: company
callers read own + global only; unresolved callers read global
only; writes require a resolved company. List handlers filter with
`in_company_scope`; single-record loads are followed by
`require_record_in_company_scope` (or a module loader that does
both); create bodies are verified with `require_company_match`
(omitted body company adopts the caller scope, never stored
unscoped under a scoped caller); update bodies verify a newly
named company, while an explicit clear stays a clear.

Each module's helper file carries the shared block
(`require_tenant`, `caller_company`, `require_company_match`,
`require_company_match_for`, `in_company_scope`,
`require_record_in_company_scope`) copied from
`src/modules/products/helpers.rs` (pilot 2), plus a module loader:

- pos (`validation.rs`): `require_sale_in_scope` (sales are the
  only record carrying `company_id: Option<String>`).
- pricing_tables (`auth.rs`): `require_vendor_in_scope`,
  `vendor_in_scope`, `require_commission_in_scope`,
  `require_payout_in_scope`, `require_product_approval_in_scope`
  (vendors are the only record carrying `company_id`; commissions,
  payouts, and product approvals are scoped through their parent
  vendor).
- loyalty (`helpers.rs`): `require_program_in_scope` (programs are
  the only record carrying `company_id`; earn rules, tiers, ledger
  rows, and redemptions are scoped through their parent program).
- affiliates (`helpers.rs`): `require_affiliate_in_scope`,
  `require_affiliate_commission_in_scope`,
  `require_affiliate_payout_in_scope` (affiliates are the only
  record carrying `company_id`; links, clicks, conversions,
  commissions, commission rules, and payouts are scoped through
  their parent affiliate).

## Exclusions (documented public / non-tenant surfaces, NOT gated)

- pricing_tables `render_table` — public when published (drafts
  need `pricing_tables:read`); gating would break the public
  render endpoint. The module header documents both public paths.
- pricing_tables `vendor_storefront` — no `HttpRequest` parameter
  (nothing to gate); public storefront data for approved vendors
  only (non-approved vendors answer 404).
- affiliates `track_click` — public click tracking by design (see
  its doc comment): clicks come from anonymous visitors, no caller
  identity, no capability check; possession of the referral code
  IS the authorization, and the route is gated by the API Manager
  `ApiAccessRule` for `POST /api/v1/affiliates/track`.

No other handler in the four modules is a public surface: every
other routed handler already required a verified caller and a
capability check.

## Per-module detail

### pos — 35 guarded, 0 excluded, 0 missing (16 scope-checked, 19 gate-only)

Scope-checked: `list_sales` (filter), `create_sale` (body verify),
`get_sale`, `update_sale`, `add_sale_item`, `update_sale_item`,
`remove_sale_item` (via `load_editable_sale`, which now takes the
scope and checks the loaded sale), `complete_sale`, `void_sale`,
`refund_sale`, `hold_sale`, `resume_sale` (via
`require_sale_in_scope`), `list_receipts`, `get_receipt`,
`get_receipt_print` (receipts carry no company; scoped through
the parent sale), `end_of_day_report` (sales loop filtered by
scope; refunds filtered through their parent sale).
Gate-only: registers x7 and shifts x6 (`PosRegister` /
`PosShift` carry no company field), payment methods x4 and
settings x2 (no company field), `search_products` (catalogue
lookup is unavailable — returns no products).

### pricing_tables — 47 guarded, 2 excluded, 0 missing (26 scope-checked, 21 gate-only)

Scope-checked: `list_vendors` (filter), `apply_vendor` (body
verify), `get_vendor`, `update_vendor` (load check + newly named
company verify; explicit clear stays a clear),
`approve_vendor` / `reject_vendor` / `suspend_vendor` /
`reactivate_vendor`, `vendor_dashboard`,
`list_vendor_commissions`, `list_vendor_payouts`,
`submit_product`, `request_payout` (vendor load + scope check),
`create_commission_rule` / `update_commission_rule` (vendor-scoped
rules are checked against their vendor),
`list_commissions` / `get_commission`, `list_product_approvals` /
`get_product_approval` / `approve_product` / `reject_product`,
`list_payouts` / `get_payout` / `approve_payout` /
`reject_payout` / `mark_payout_paid` (via the parent-vendor
loaders / list filters).
Gate-only: tables / tiers / features x19 (`PricingTable`,
`PricingTier`, `TierFeature` carry no company field — shared
catalogue), `embed_table`, commission rules list/get/delete
(`CommissionRule` carries no company field; vendor-scoped rules
are checked at create/update time only — see residuals).

### loyalty — 26 guarded, 0 excluded, 0 missing (26 scope-checked, 0 gate-only)

All 24 `load_program` sites in `handlers.rs` now go through
`require_program_in_scope`, so every earn-rule / tier / points /
balance / ledger / redemption handler is scoped through its
parent program. `list_programs` filters by scope;
`create_program` verifies the body company (omitted company
adopts the caller scope).

### affiliates — 29 guarded, 1 excluded, 0 missing (29 scope-checked, 0 gate-only)

`list_affiliates` filters by scope; `create_affiliate` verifies
the body company; `get_affiliate` / `update_affiliate` (newly
named company verified) / `delete_affiliate` /
approve/reject/suspend/reactivate (scope check before
`transition_affiliate`) load through
`require_affiliate_in_scope`. Links, commission rules, clicks,
conversions, commissions, payouts, and dashboards are scoped
through their parent affiliate (loads via
`require_affiliate_in_scope`; approve/void commission via
`require_affiliate_commission_in_scope`; approve/mark-paid/reject
payout via `require_affiliate_payout_in_scope` before
`transition_payout`; `my_dashboard` checks the caller's own
affiliate record). `request_payout` verifies each named
commission belongs to the already scope-checked affiliate.

## Files touched (15)

- `src/modules/pos/validation.rs` — guard block,
  `require_sale_in_scope`, guard tests.
- `src/modules/pos/handlers_sales.rs` — gates, list filter,
  create body verify, scoped loads, `load_editable_sale` scope.
- `src/modules/pos/handlers_checkout.rs` — gates, scoped sale loads.
- `src/modules/pos/handlers_admin.rs` — gates, report scope filters.
- `src/modules/pos/handlers_registers.rs` — gates only.
- `src/modules/pos/handlers_receipts.rs` — gates, receipt scope
  via parent sale.
- `src/modules/pricing_tables/auth.rs` — guard block, vendor /
  commission / payout / approval loaders, guard tests.
- `src/modules/pricing_tables/handlers_tables.rs` — gates only
  (`render_table` excluded).
- `src/modules/pricing_tables/handlers_vendors.rs` — gates
  (`vendor_storefront` excluded), vendor scope checks, body verify.
- `src/modules/pricing_tables/handlers_marketplace.rs` — gates,
  vendor-scoped checks, commission-rule vendor checks.
- `src/modules/loyalty/helpers.rs` — guard block,
  `require_program_in_scope`, guard tests.
- `src/modules/loyalty/handlers.rs` — gates, scoped program loads,
  list filter, create body verify.
- `src/modules/affiliates/helpers.rs` — guard block, affiliate /
  commission / payout loaders, guard tests.
- `src/modules/affiliates/handlers_affiliates.rs` — gates, affiliate
  scope checks, body verify.
- `src/modules/affiliates/handlers_tracking.rs` — gates
  (`track_click` excluded), parent-affiliate scope checks.

Supporting files created here (not in the guard map — the
coordinator merges map rows):

- `docs/integration/pilot3/c3_group_a_verify.py` — per-handler
  guard + order + scope-classification check for the four
  modules (same routed-set rule as `/tmp/c3_verify.py`).
- `docs/integration/pilot3/group-a-results.md` — this file.

`docs/integration/C3_MODULE_GUARD_MAP.md` was NOT edited.

## Verification

- `python3 /tmp/c3_verify.py <module> [exclusions]` (the pilot
  1–2 script): pos `guarded=35 missing=0`; pricing_tables
  `guarded=47 missing=0 excluded=2`; loyalty `guarded=26
  missing=0`; affiliates `guarded=29 missing=0 excluded=1`.
- `python3 docs/integration/pilot3/c3_group_a_verify.py <module>
  [exclusions]`: pos `guarded=35 (scope-checked=16,
  gate-only=19) missing=0 excluded=0 order_violations=0`;
  pricing_tables `guarded=47 (scope-checked=26, gate-only=21)
  missing=0 excluded=2 order_violations=0`; loyalty `guarded=26
  (scope-checked=26, gate-only=0) missing=0 excluded=0
  order_violations=0`; affiliates `guarded=29 (scope-checked=29,
  gate-only=0) missing=0 excluded=1 order_violations=0`.
  The order check verifies `require_tenant(&req)` precedes the
  first identity/capability call in every guarded handler body.
- `rustfmt --edition 2024 --emit stdout` parse check on all 15
  touched files: all parse. `rustfmt --check` diff lines
  mentioning a new identifier were fixed (loyalty import
  ordering, affiliates import wrapping, three over-long lines
  wrapped to rustfmt's preferred form); after the fixes, zero
  `--check` diff lines mention any new identifier. Whole-file
  `--check` still fails on pre-existing style drift, as in
  pilots 1–2, so whole-file formatting was NOT applied.
- Leftover-load audit: no unscoped `load_sale` /
  `load_program` / `load_affiliate` remains in any handler body
  except inside the scoped loaders, the transition helpers
  (whose handlers scope-check first), and the excluded handlers.
  The one `load_commission` in affiliates `request_payout` is
  followed by an explicit `commission.affiliate_id ==
  affiliate.id` check against the already scope-checked
  affiliate.
- Tests added (same fail-closed / resolve / scope shape as
  pilot 2): `tenant_guard_tests` in `pos/validation.rs`,
  `pricing_tables/auth.rs`, `loyalty/helpers.rs`, and
  `affiliates/helpers.rs` (7 tests each: fail-closed without
  context, resolve from middleware context, verified-caller
  fallback, mismatch rejection, scope matrix, body-match
  refusal from unresolved callers, plus one loader
  partition test per module).

## NOT verified

- The guards were NOT typechecked (`cargo check` not run — the
  root bin crate OOM-kills this VM) and NOT run (`cargo test`
  not run). The new unit tests are UNRUN.
- No live WP / HTTP render was performed.
- One real defect was caught by the verifier during this work
  and fixed in the same pass: the marketplace
  `approve_product` / `reject_product` scope checks were missing
  (the edit script's count assertion failed on the reject
  shape); the final verification run confirms all three
  `require_product_approval_in_scope` sites are present.

## Residuals (honest, not partitioned by the guard alone)

- pos: `PosRegister`, `PosShift`, `PosPaymentMethod`, and
  `PosSettings` carry no company field — gate only; stamping a
  company on registers/shifts is a C11 follow-up. The
  end-of-day report's shift-variance section is unscoped for the
  same reason, and `close_shift`'s summary aggregates the
  shift's sales regardless of company. Global uniqueness checks
  (register code, payment-method code) can leak cross-company
  existence via conflict errors — scoped uniqueness is
  follow-up work.
- pricing_tables: pricing tables / tiers / features are a shared
  catalogue with no company field — gate only; per-company
  tables need a `company_id` on `PricingTable` (C11). Commission
  rules carry no company field: vendor-scoped rules are checked
  against their vendor at create/update, but list/get/delete do
  not filter by vendor scope, so a company caller with
  `marketplace:read` can read another company's vendor-scoped
  rule. Vendor slug uniqueness can leak existence via conflict
  errors.
- affiliates: referral-code uniqueness (`code_in_use`) can leak
  existence via conflict errors. Anonymous clicks recorded by
  `track_click` are attributed to the affiliate only through the
  referral code, by design.
- Shared: a platform admin with no resolvable company can now
  only read global records and cannot write company records
  (fail-closed per the shared rule); whether an admin capability
  implies cross-company access is the task-315 decision.
  `caller_company` here never reads the `X-Company-Id` header,
  so a forged header is ignored by these four modules; header
  trust elsewhere is task 317's job.
