# Integration Notes — Batch 15, Builder B3: Smart Pricing Calculator + Discount/Proration Engine

## 1. Summary

Builder B3 delivered the Smart Pricing module: price recommendations
(cost-plus, tiered, bundle), a discount/coupon engine with a declared
evaluation order and full trace, per-day proration, and server-side
invoice reconciliation with credit notes. All money is integer scaled
minor units (1 major unit = 10,000 scaled units, exactly NUMERIC(19,4));
there is no floating-point money anywhere in the module.

### Files added (nothing else touched)

| File | Lines | Contents |
|---|---|---|
| `src/modules/pricing.rs` | 7,216 | The module: money core, pure engines, 14 business entities, 58 routes, 3 hook answers, 21 unit tests |
| `migrations/2026-11-22-000097_pricing/up.sql` | 495 | 16 tables, 12 seeded currencies |
| `migrations/2026-11-22-000097_pricing/down.sql` | 20 | 16 `DROP TABLE` statements |

Files explicitly NOT touched (per task): `src/modules/mod.rs`,
`src/db/schema.rs`, `src/db/models.rs`, `README.md`, `TODO.md`,
`src/modules/billing.rs`, `LICENSE`, the tracker, `MEMORY.md`, the sync
record. Nothing was pushed. No binary files. No `todo!` /
`unimplemented!`.

### Counts

- Routes: **58** (`POST/GET/PUT/DELETE` under `/api/v1/pricing`; the
  `route(` count and the `RouteInfo` count agree at 58).
- Tables: **16** (`CREATE TABLE` = `DROP TABLE` = 16).
- Hook events answered: 3 (`pricing.discount.evaluate_requested`,
  `pricing.invoice.validate_requested`, `pricing.ai.recommend_requested`).
- Hook events emitted: 13 (see §4).
- Capabilities: `pricing:read`, `pricing:manage`, `pricing:approve`.
- Unit tests: 21, all pure functions (no database, no network).
- Module size is above the 3,000–4,000 line aim (7,216 lines) because
  the brief asked for full CRUD on every entity plus 58 routes plus
  tests; there are no stubs. See OPEN #12.

### Migration number collision — recorded

`000093` was already taken when the migration was written:
`2026-11-22-000093_product_subdomains` (batch 15 B2) exists on disk.
Per the task instructions the pricing migration was renumbered to the
next free number, **`000097`** (`2026-11-22-000097_pricing`). No other
builder's migration was renumbered. Tree-wide duplicate numbers
(pre-existing, not mine): 000056, 000070, 000075, 000077, 000080, 000090
(each ×2), plus 000100 out of sequence. Free numbers around mine at
write time: 000097 was free; 000091 (library), 000092 (entitlements),
000094 (tenant_isolation), 000095 (calendar), 000096 (saas_packages)
were already taken.

## 2. Money rules (enforced in code, documented in the module header)

- Money is `i64` scaled minor units: 1 major unit = 10,000 scaled
  units, mapping exactly onto NUMERIC(19,4). API callers supply money
  as exact decimal strings (`MoneyInput { amount: "19.99", currency:
  "USD" }`), parsed with integer arithmetic only.
- Percentages and rates are integer basis points (10,000 bp = 100%).
- Rounding is **half-up, per line, then summed**. A line amount is
  rounded half-up to the currency minor unit (2 decimal places, or
  whole units for zero-decimal currencies) at computation time; totals
  are exact sums of rounded lines. Tax is per line, rounded half-up,
  then summed. Proration rounds the daily rate half-up first, then
  multiplies by whole days. Currency conversion rounds half-up to the
  target minor unit.
- Exchange rates are integer ratios (`rate_numerator /
  rate_denominator`), entered by an admin or supplied from stored
  rates. A missing rate is an explicit error, never a guessed
  conversion.
- Conversion to the products/store cent representation: cents × 100 =
  scaled; scaled → cents with half-up rounding (`cents_to_scaled` /
  `scaled_to_cents` in the module).
- The calculator module (batch 3) conventions are reused, not rebuilt:
  NUMERIC(19,4), stored rates only, explicit unavailability errors.
  The calculator's own `pricing` endpoint solves price/cost/margin for
  one product; this module versions, approves, tiers, bundles,
  discounts, prorates, and reconciles those prices.

## 3. What the engine does

- **Recommendations (suggestions only).** Cost-plus (cost lines with
  source/label + margin target → total cost, margin amount, suggested
  price, every step shown), tiered (per-quantity tiers, tier breakdown),
  bundle (component sum minus bundle discount, line by line). The
  engine NEVER silently sets a price. Confirming a recommendation
  creates a **draft** price version; activation is a separate step.
- **Market reference prices.** Human-entered only, with mandatory
  source + observation date. Never scraped, never guessed. Summary
  endpoint returns count / lowest / highest / median per currency
  (currencies are never averaged together).
- **Price versions + approvals.** Status machine `draft →
  pending_approval → approved → active`, `rejected` back to `draft`.
  A change above the configured threshold (default 1,000 bp = 10.00%,
  admin-editable per tenant) needs a second approver who is not the
  requester (`pricing:approve`). Activation supersedes the previous
  active version for the same product in the same tenant. An
  unmeasurable change (old price zero, new price above zero) always
  needs a second approver.
- **Discount engine.** Kinds: `percent`, `fixed`, `buy_x_get_y`,
  `volume`. Rules run in declared `evaluation_order` (ties by rule
  id). Stackable rules apply in sequence on the running remainder; an
  exclusive rule, or a non-stackable rule that applies, stops later
  rules. The trace records every rule (applied or skipped, with the
  reason) and the running total after each step. Coupons add validity
  windows, global usage limits, and per-customer usage limits;
  redemption re-checks limits at confirmation time so evaluation and
  confirmation cannot overspend a coupon. The engine never mutates an
  order: it only returns the evaluation.
- **Proration.** Kinds `upgrade` / `downgrade` / `plan_change` /
  `seat_change`; basis `per_day` or `per_month` (configurable
  `days_per_month`, default 30). Every record shows period, daily
  rates, days used/remaining, unused credit, new charge, net due.
  The change day counts as a used day on the old plan.
- **Reconciliation (the accuracy guard).** `reconcile_invoice`
  recomputes lines + discounts + proration + tax with integer math
  and compares with the claimed figures **per component** (a match on
  the total alone is not enough). A mismatch blocks issuing with the
  exact discrepancy per component. `POST /invoices/preview` shows the
  admin the exact invoice first, with `can_issue`. Issued invoices are
  immutable; corrections go through credit notes only, and a credit
  note carries its own reconciliation (lines must equal the credit
  amount exactly; issuing a mismatched credit note is rejected).
- **AI wiring.** `pricing.ai.recommend_requested` and
  `GET /recommendations/{id}/narrative` build a narrative draft from
  the stored recommendation record only — every number comes from the
  engine; the AI only words the summary and never invents prices. No
  LLM call lives in this module.

## 4. Events

Emitted (all payloads carry `tenant_id`, see §5):

- `pricing.recommendation.created` / `.confirmed`
- `pricing.price_version.submitted` / `.approved` / `.rejected` / `.activated`
- `pricing.discount.evaluated`
- `pricing.coupon.redeemed`
- `pricing.proration.computed`
- `pricing.invoice.previewed`, `pricing.invoice.reconciled`,
  `pricing.invoice.reconciliation_failed`
- `pricing.credit_note.created` / `.issued`
- `pricing.ai.draft_created`

Answered (requested by other modules; answers via `add_result`,
never mutating the caller):

- `pricing.discount.evaluate_requested` — payload: `tenant_id`
  (required), `lines` (`product_id`, `unit_price` as decimal string
  or scaled integer, `quantity`), optional `coupon_code`,
  `customer_id`, `currency`. Answer: the full evaluation plus
  `valid: true`, or `{"valid": false, "error": ...}`.
- `pricing.invoice.validate_requested` — payload: `tenant_id`
  (required), `invoice_id`, `lines` (`name`, `quantity`,
  `unit_price`, `tax_bp`, `tax_inclusive`), `discounts`,
  `proration_lines`, `claimed` (`subtotal`, `discounts`, `proration`,
  `tax_total`, `total` — each a decimal string, scaled integer, or
  JSON number), `currency`. Answer: the full reconciliation plus
  `valid` (true only when every component matches) and
  `reconciliation_id`. Billing must block issuing when `valid` is
  false or when no result is returned.
- `pricing.ai.recommend_requested` — payload: `tenant_id` (required),
  `recommendation_id`. Answer: `{"draft", "numbers"}` built from the
  stored record, or no result when the record does not exist or
  belongs to another tenant.

## 5. TENANT-SCOPED DATA ACCESS (standing rule, user 2026-11-22 12:13)

> **Filing note:** the follow-up handoff asked for this section to be
> recorded in `INTEGRATION_NOTES_BATCH15_B2.md`. That file belongs to
> builder B2 (product subdomains / `saas_domains`); B3 does not write
> to another builder's notes file while B2 is running in parallel. The
> section is recorded here, in B3's own notes. Coordinator: please
> copy or cross-reference it into the B2 notes if a single location is
> wanted. This module does no product-subdomain work itself (that is
> B2's `saas_domains` module); the tenant rule below is how it applies
> to **pricing**.

### What was implemented in `src/modules/pricing.rs`

- **Every pricing record carries `tenant_id`** (uuidv7, from the tenant
  context): `pricing_rules`, `pricing_coupons`,
  `pricing_coupon_redemptions`, `pricing_recommendations`,
  `pricing_cost_lines`, `pricing_tiers`, `pricing_bundles`,
  `pricing_bundle_components`, `pricing_reference_prices`,
  `pricing_price_versions`, `pricing_price_approvals`,
  `pricing_proration_records`, `pricing_credit_notes`,
  `pricing_reconciliation_reports`, and `pricing_settings` (one row
  per tenant). Verified by script: `tenant_id` present on all 14
  business structs.
- **Single choke point.** `resolve_tenant()` reads ONLY the
  middleware-attached `TenantContext::from_request`
  (`src/core/middleware.rs`, resolved through the
  `tenant.resolve_requested` hook). `caller()` calls it first, so
  **every one of the 58 HTTP handlers fails closed with 403 when the
  request did not resolve to a tenant** — verified: all 58 handlers
  call `caller(&req)` and `require_permission`. Nothing silently falls
  back to the `public` schema. No tenant id, schema name, or package
  id is hard-coded anywhere; no client header is trusted for the
  tenant.
- **Scoping on every access.** Lists filter by the calling tenant.
  Single-record reads, updates, deletes, redemptions, approvals, and
  narrative drafts check ownership and answer **404** (never leak)
  for another tenant's record. Discount evaluation scopes active
  rules, extra rules, and coupons to the tenant. Redemption keys are
  `tenant_id:coupon_id:customer_id`. Price-version numbering and the
  active-price lookup are per tenant. Settings are per tenant
  (`DashMap<String, PricingSettings>`, lazily seeded with the
  configurable defaults).
- **Hooks fail closed.** All three answered hook events require
  `tenant_id` in the payload (validated as uuidv7 canonical form by
  `validate_tenant_id`: 36 chars, hyphens at 8/13/18/23, version
  nibble `7`). A missing or malformed tenant id is answered with
  `{"valid": false, "error": ...}` (discount/invoice) or no result
  (AI). Every emitted event payload carries `tenant_id`, so downstream
  modules (billing, store, pricing tables, AI) resolve their business
  data through the tenant schema, never the control-plane DB.
- **Documented exception — currency registry.** `pricing_currencies`
  has no `tenant_id`: ISO 4217 codes are the sanctioned standard
  constants and the currency rows are platform reference data,
  identical for every tenant. The currency endpoints still require a
  resolved tenant context, so there is no anonymous or public access.
- **No SQL in this module.** All reads/writes go through the
  in-memory DashMap store (repo pattern) scoped by `tenant_id`; the
  Diesel tables are persistence targets. Control-plane tables
  (tenants, subscriptions, domains, billing, licensing) are touched
  only through their own modules/events, never from here.

### Convergence point with batch 17 (tenancy plumbing — NOT on disk yet)

Verified 2026-11-22: there is **no `schema_name` helper, no tenant
schema extractor, and no `search_path` handling anywhere in `src/`**.
`TenantContext` (`src/core/middleware.rs`) carries tenant id, company
id, site id, kind, and host — but **no schema name and no package
entitlements**. The pattern implemented here follows batch 16's
`src/modules/calendar.rs` (`resolve_tenant`, fail-closed 403), which
already shipped the same way.

This module deliberately does **not** invent its own schema-name
derivation. When batch 17 lands its plumbing, the integrator swaps
**exactly one function — `resolve_tenant` in `src/modules/pricing.rs`**
— to also return the batch-17 schema name / `search_path` handle, and
re-points the migration tables (every `pricing_*` table already carries
a `tenant_id` column) at the per-tenant schema instead of `public`. No
handler, no record shape, and no hook contract changes: `tenant_id` is
already on every record and every filter.

**Package entitlements** (which pricing features a tenant's purchased
package unlocks) are enforced by the entitlements module once batch 17
wires them into `TenantContext`; until then the capability check
(`pricing:read` / `pricing:manage` / `pricing:approve`) is the only
gate. That limitation is OPEN #5.

## 6. Integrator snippets

### 6.1 `src/modules/mod.rs` — exact lines to add

```rust
// Alphabetical declaration list, after `pub mod planner;` (line 57):
pub mod planner;
pub mod pricing;
pub mod products;
```

```rust
// In `register_all`, in the `// Commerce.` group, after
// `billing::register(registry);`:
    billing::register(registry);
    pricing::register(registry);
    products::register(registry);
```

Pre-existing condition the integrator should know: `mod.rs` declares
only 72 of the 134 module files on disk. `calculator.rs` and
`pricing_tables.rs` are also **not** declared (verified), so the
`requires()` of this module lists only `billing`, `store`, and
`products` (all three are registered) to avoid adding new
missing-dependency report noise; the calculator / pricing-tables
relationships are event-only and documented in the module header.

### 6.2 `src/db/schema.rs` — append (table definitions follow the
migration column-for-column; `Numeric` is `diesel::data_types::PgNumeric`)

```rust
diesel::table! {
    pricing_settings (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        rounding_mode -> Varchar,
        proration_basis -> Varchar,
        days_per_month -> Int8,
        approval_threshold_bp -> Int8,
        require_second_approver -> Bool,
        default_currency -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_currencies (code) {
        code -> Varchar,
        exponent -> Int8,
        active -> Bool,
        rate_numerator -> Nullable<Int8>,
        rate_denominator -> Nullable<Int8>,
        rate_fetched_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_rules (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        name -> Varchar,
        kind -> Varchar,
        percent_bp -> Int8,
        amount_scaled -> Numeric,
        currency -> Varchar,
        buy_quantity -> Int8,
        get_quantity -> Int8,
        min_quantity -> Int8,
        min_subtotal_scaled -> Numeric,
        product_ids -> Jsonb,
        evaluation_order -> Int8,
        stackable -> Bool,
        exclusive -> Bool,
        status -> Varchar,
        valid_from -> Nullable<Timestamptz>,
        valid_to -> Nullable<Timestamptz>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_coupons (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        code -> Varchar,
        kind -> Varchar,
        percent_bp -> Int8,
        amount_scaled -> Numeric,
        currency -> Varchar,
        buy_quantity -> Int8,
        get_quantity -> Int8,
        product_ids -> Jsonb,
        valid_from -> Nullable<Timestamptz>,
        valid_to -> Nullable<Timestamptz>,
        usage_limit -> Nullable<Int8>,
        per_customer_limit -> Nullable<Int8>,
        used_count -> Int8,
        stackable -> Bool,
        exclusive -> Bool,
        active -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_coupon_redemptions (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        coupon_id -> Uuid,
        customer_id -> Varchar,
        count -> Int8,
        last_redeemed_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_recommendations (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        product_id -> Varchar,
        kind -> Varchar,
        suggested_price_scaled -> Numeric,
        currency -> Varchar,
        lines -> Jsonb,
        inputs -> Jsonb,
        status -> Varchar,
        created_by -> Varchar,
        confirmed_by -> Nullable<Varchar>,
        price_version_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_cost_lines (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        recommendation_id -> Uuid,
        label -> Varchar,
        source -> Varchar,
        cost_scaled -> Numeric,
        currency -> Varchar,
    }
}

diesel::table! {
    pricing_tiers (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        product_id -> Varchar,
        min_quantity -> Int8,
        max_quantity -> Nullable<Int8>,
        unit_price_scaled -> Numeric,
        currency -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_bundles (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        name -> Varchar,
        product_id -> Varchar,
        discount_kind -> Varchar,
        discount_percent_bp -> Int8,
        discount_amount_scaled -> Numeric,
        currency -> Varchar,
        active -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_bundle_components (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        bundle_id -> Uuid,
        product_id -> Varchar,
        label -> Varchar,
        price_scaled -> Numeric,
        currency -> Varchar,
    }
}

diesel::table! {
    pricing_reference_prices (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        product_id -> Varchar,
        source -> Varchar,
        source_url -> Nullable<Varchar>,
        price_scaled -> Numeric,
        currency -> Varchar,
        observed_at -> Timestamptz,
        note -> Text,
        entered_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_price_versions (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        product_id -> Varchar,
        version -> Int8,
        price_scaled -> Numeric,
        currency -> Varchar,
        previous_price_scaled -> Numeric,
        change_bp -> Nullable<Int8>,
        above_threshold -> Bool,
        status -> Varchar,
        requested_by -> Varchar,
        approved_by -> Nullable<Varchar>,
        reason -> Nullable<Text>,
        recommendation_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_price_approvals (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        price_version_id -> Uuid,
        approver -> Varchar,
        decision -> Varchar,
        reason -> Nullable<Text>,
        decided_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_proration_records (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        subscription_id -> Nullable<Varchar>,
        customer_id -> Nullable<Varchar>,
        kind -> Varchar,
        basis -> Varchar,
        period_start -> Timestamptz,
        period_end -> Timestamptz,
        change_at -> Timestamptz,
        period_days -> Int8,
        days_used -> Int8,
        days_remaining -> Int8,
        old_daily_rate_scaled -> Numeric,
        new_daily_rate_scaled -> Numeric,
        unused_credit_scaled -> Numeric,
        new_charge_scaled -> Numeric,
        net_due_scaled -> Numeric,
        currency -> Varchar,
        lines -> Jsonb,
        created_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_credit_notes (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        number -> Varchar,
        invoice_id -> Varchar,
        amount_scaled -> Numeric,
        currency -> Varchar,
        lines -> Jsonb,
        reason -> Text,
        status -> Varchar,
        reconciled -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_reconciliation_reports (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        invoice_id -> Nullable<Varchar>,
        matched -> Bool,
        currency -> Varchar,
        components -> Jsonb,
        total_discrepancy_scaled -> Numeric,
        checked_by -> Varchar,
        checked_at -> Timestamptz,
    }
}

diesel::joinable!(pricing_coupon_redemptions -> pricing_coupons (coupon_id));
diesel::joinable!(pricing_cost_lines -> pricing_recommendations (recommendation_id));
diesel::joinable!(pricing_bundle_components -> pricing_bundles (bundle_id));
diesel::joinable!(pricing_price_approvals -> pricing_price_versions (price_version_id));
```

Add the 16 table names to the `allow_tables_to_appear_in_same_query!`
list in `src/db/schema.rs` (same block as the other tables).

### 6.3 `src/db/models.rs` — Diesel models (Queryable + Insertable)

One `Queryable`/`Insertable` pair per table, mirroring the schema
above; money fields are `diesel::data_types::PgNumeric` (NUMERIC(19,4)),
basis points are `i64`, JSONB fields are `serde_json::Value`, ids are
`uuid::Uuid`, `tenant_id` is `uuid::Uuid` on every model except
`PricingCurrency`. Suggested model names (same order as the schema
snippet): `PricingSetting`, `NewPricingSetting`, `PricingCurrency`,
`NewPricingCurrency`, `PricingRule`, `NewPricingRule`,
`PricingCoupon`, `NewPricingCoupon`, `PricingCouponRedemption`,
`NewPricingCouponRedemption`, `PricingRecommendation`,
`NewPricingRecommendation`, `PricingCostLine`, `NewPricingCostLine`,
`PricingTier`, `NewPricingTier`, `PricingBundle`, `NewPricingBundle`,
`PricingBundleComponent`, `NewPricingBundleComponent`,
`PricingReferencePrice`, `NewPricingReferencePrice`,
`PricingPriceVersion`, `NewPricingPriceVersion`,
`PricingPriceApproval`, `NewPricingPriceApproval`,
`PricingProrationRecord`, `NewPricingProrationRecord`,
`PricingCreditNote`, `NewPricingCreditNote`,
`PricingReconciliationReport`, `NewPricingReconciliationReport`.
(The in-memory DashMap store stays authoritative until the DB-backed
read path is wired; the models are the persistence target, per the
repo pattern.)

### 6.4 `src/modules/billing.rs` — invoice-validation hook snippet

Billing must call the pricing validator **before** issuing an invoice
and block issuing when it does not validate. Add this to `billing.rs`
(near `create_invoice` / the issue path) — billing owns the call, the
pricing module only answers. Billing stores amounts as float major
units (pre-existing); the boundary converts each amount through its
4-decimal string form, which matches NUMERIC(19,4) storage. The
`tenant_id` must come from the billing request's resolved tenant
context (`TenantContext::from_request`), never from a client header.

```rust
/// Ask the pricing module to reconcile an invoice before it is
/// issued. Returns `Ok(())` only when every component matches.
/// A missing answer, an invalid answer, or a mismatch blocks
/// issuing. Billing amounts are float major units; each one is
/// converted through its 4-decimal string form so the pricing
/// module parses it with integer math.
fn validate_invoice_with_pricing(
    hooks: &Arc<HookBus>,
    tenant_id: &str,
    invoice: &Invoice,
) -> UnifiedResult<()> {
    let line = |item: &InvoiceItem| {
        json!({
            "name": item.name,
            "quantity": item.quantity,
            // 4-decimal string form of the float amount; the pricing
            // module parses it as scaled integer units.
            "unit_price": format!("{:.4}", item.unit_price),
            "tax_bp": (item.tax_rate * 10_000.0).round() as i64,
            "tax_inclusive": invoice.tax_inclusive,
        })
    };
    let context = hooks.emit(
        "pricing.invoice.validate_requested",
        json!({
            "tenant_id": tenant_id,
            "invoice_id": invoice.id,
            "lines": invoice.items.iter().map(line).collect::<Vec<_>>(),
            "discounts": [],
            "proration_lines": [],
            "claimed": {
                "subtotal": format!("{:.4}", invoice.subtotal),
                "discounts": "0.0000",
                "proration": "0.0000",
                "tax_total": format!("{:.4}", invoice.tax_total),
                "total": format!("{:.4}", invoice.total),
            },
            "currency": invoice.currency,
        }),
    );
    let answer = context.results.into_iter().next().ok_or_else(|| {
        UnifiedError::unprocessable(
            "Invoice reconciliation unavailable: no pricing validator answered",
        )
    })?;
    match answer.get("valid").and_then(|value| value.as_bool()) {
        Some(true) => Ok(()),
        _ => {
            let discrepancy = answer
                .get("total_discrepancy")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            Err(UnifiedError::unprocessable(format!(
                "Invoice reconciliation failed (discrepancy {discrepancy}): issuing is blocked until the invoice figures match"
            )))
        }
    }
}
```

Call it at the top of the issue path (before the status flips to
`issued`), and pass the real discount/proration lines when the
invoice carries them instead of the empty arrays above. Note the two
honest limits: (1) billing's explicit-total override (`create_invoice`
lets a caller-supplied total differ from the computed one) will fail
reconciliation by design — that is the guard working; (2) the
`{:.4}` conversion can only prove the rounded figures, not the
unrounded float — full proof needs billing on integer money (OPEN #2).

### 6.5 Store hook convergence (coupons)

The store already has coupons (`store.rs` `Coupon`: percent/fixed,
expiry, global usage limit, product scope, **float** amounts). This
module's engine is the shared evaluator the store should call:

- Store checkout calls `pricing.discount.evaluate_requested` with
  `tenant_id`, lines, `coupon_code`, `customer_id`, `currency`, and
  applies the returned trace (amounts are scaled integers; convert to
  cents with half-up rounding at the store boundary).
- On order confirmation the store calls
  `POST /api/v1/pricing/coupons/{id}/redeem` with `customer_id`
  (limits are re-checked there).
- The store keeps its own coupon records as the source of truth for
  checkout **until the integrator converges them**; this module's
  coupons add validity windows, per-customer limits, stacking /
  exclusivity, buy-X-get-Y, and volume rules, all in integer money.
  Do not run both coupon maths in parallel on one order: one
  evaluator (pricing) and one record set per coupon code, or the
  totals will disagree. The store's `coupon_discount_for` float math
  is the piece to retire (OPEN #4).

### 6.6 Capability seeds (roles module)

```text
pricing:read     — Read pricing rules, coupons, recommendations, price versions, proration records, reconciliations, and settings.
pricing:manage   — Create, update, and delete pricing rules, coupons, reference prices, tiers, bundles, price versions, and settings; compute recommendations, discounts, proration, previews, and credit notes.
pricing:approve  — Approve or reject a price version that needs a second approver.
```

Seed these under the `pricing` module id. Every route checks its
capability through the `role.check` hook and fails closed (verified:
59 `require_permission` call sites for 58 handlers — the extra one is
the helper definition; the tenant gate is inside `caller()`).

### 6.7 ApiAccessRule rows (api_manager, version `v1`)

All 58 pricing routes register in the versioned public API registry
through `route_info()` (owner `pricing`). Seed one `ApiAccessRule` per
route with `allowed = true`, **`public_access = false`** (no pricing
endpoint is public; every call needs a key/session plus the tenant
context and the capability). Paths (method + path):

```text
POST /api/v1/pricing/recommendations/cost-plus
POST /api/v1/pricing/recommendations/tiered
POST /api/v1/pricing/recommendations/bundle
GET  /api/v1/pricing/recommendations
GET  /api/v1/pricing/recommendations/{id}
POST /api/v1/pricing/recommendations/{id}/confirm
GET  /api/v1/pricing/recommendations/{id}/narrative
GET  /api/v1/pricing/rules
POST /api/v1/pricing/rules
GET  /api/v1/pricing/rules/{id}
PUT  /api/v1/pricing/rules/{id}
DELETE /api/v1/pricing/rules/{id}
GET  /api/v1/pricing/coupons
POST /api/v1/pricing/coupons
GET  /api/v1/pricing/coupons/{id}
PUT  /api/v1/pricing/coupons/{id}
DELETE /api/v1/pricing/coupons/{id}
POST /api/v1/pricing/coupons/{id}/redeem
GET  /api/v1/pricing/coupons/{id}/redemptions
POST /api/v1/pricing/discounts/evaluate
GET  /api/v1/pricing/reference-prices
POST /api/v1/pricing/reference-prices
GET  /api/v1/pricing/reference-prices/summary
GET  /api/v1/pricing/reference-prices/{id}
DELETE /api/v1/pricing/reference-prices/{id}
GET  /api/v1/pricing/price-versions
POST /api/v1/pricing/price-versions
GET  /api/v1/pricing/price-versions/{id}
POST /api/v1/pricing/price-versions/{id}/submit
POST /api/v1/pricing/price-versions/{id}/approve
POST /api/v1/pricing/price-versions/{id}/reject
POST /api/v1/pricing/price-versions/{id}/activate
GET  /api/v1/pricing/price-versions/{id}/approvals
GET  /api/v1/pricing/tiers
POST /api/v1/pricing/tiers
GET  /api/v1/pricing/tiers/price
DELETE /api/v1/pricing/tiers/{id}
GET  /api/v1/pricing/bundles
POST /api/v1/pricing/bundles
GET  /api/v1/pricing/bundles/{id}
POST /api/v1/pricing/bundles/{id}/price
POST /api/v1/pricing/proration/compute
GET  /api/v1/pricing/proration
GET  /api/v1/pricing/proration/{id}
POST /api/v1/pricing/invoices/preview
POST /api/v1/pricing/invoices/reconcile
GET  /api/v1/pricing/reconciliations
GET  /api/v1/pricing/reconciliations/{id}
GET  /api/v1/pricing/credit-notes
POST /api/v1/pricing/credit-notes
GET  /api/v1/pricing/credit-notes/{id}
POST /api/v1/pricing/credit-notes/{id}/reconcile
GET  /api/v1/pricing/settings
PUT  /api/v1/pricing/settings
GET  /api/v1/pricing/currencies
POST /api/v1/pricing/currencies
PUT  /api/v1/pricing/currencies/{code}
POST /api/v1/pricing/convert
```

### 6.8 README.md / TODO.md lines (for the integrator to add)

README (modules section):

```text
- **Smart Pricing** (`pricing`): price recommendations (cost-plus,
  tiered, bundle), discount/coupon engine with declared evaluation
  order and full trace, per-day proration, and server-side invoice
  reconciliation with credit notes. Integer money only (NUMERIC(19,4)).
  Tenant-scoped: every record carries `tenant_id`; no tenant context
  means fail closed.
```

TODO (persistence section):

```text
- Pricing: wire the Diesel tables from `2026-11-22-000097_pricing` as
  the durable store behind the in-memory DashMap state (16 tables);
  add the billing-side `pricing.invoice.validate_requested` call before
  issuing; converge store coupons onto the pricing evaluator.
```

## 7. Verification (done by read-back; cargo was never run)

- Lines: `pricing.rs` 7,216; migration up 495, down 20.
- Routes: `.route(` count 58; `RouteInfo` entries 58 — they agree.
- Tables: `CREATE TABLE` 16; `DROP TABLE` 16 — they agree.
- Migration number: `000097` is unique (verified: only one
  `000097_*` dir; `000093` collides with B2's product-subdomains
  migration, hence the renumber).
- Forbidden reference-project name: 0 occurrences in the new files
  (the reference material was never read, run, or copied; the
  reference inspection directory was never accessed).
- Indonesian words: 0 (scanned for harga, diskon, pelanggan, pajak,
  bayar, pesanan, tanggal, jumlah, potongan, rupiah, pengguna, toko,
  belanja, koperasi, padang).
- `todo!` / `unimplemented!`: 0.
- Float money: `f64` occurrences in `pricing.rs`: 0 (the one earlier
  hit was the word in a doc comment stating the prohibition; reworded
  so the grep is exactly 0). No float arithmetic on prices anywhere.
- Capability checks: every one of the 58 async handlers calls
  `caller(&req)` (tenant gate) and `require_permission`
  (59 occurrences: 58 handlers + the helper definition). Verified by
  script, handler by handler — none missing.
- Tenant scoping: `tenant_id` present on all 14 business structs
  (verified by script); `pricing_currencies` excluded by design
  (reference data, §5). Redemption keys are tenant-scoped.
  `validate_tenant_id` unit-tested (uuidv7 shape only).
- Delimiters: braces 1119/1119, parens 4015/4015, brackets 332/332.
- Unit-test logic walked through by reading (values re-checked):
  cost-plus 4.00 + 6.00 at 25% → 12.50; tiered 10×10.00 + 5×8.00 →
  140.00; bundle 30.00 + 20.00 − 10% → 45.00; stacked 10% then 10% →
  81.00; exclusive stops later rules; buy-2-get-1 on 7 units frees 2;
  proration upgrade 30.00 → 60.00 at day 15 of 30 → net due 15.00;
  downgrade net −20.00; reconciliation match and exact discrepancy
  (−10.00 on tax); inclusive-tax split 110.00 → net 100.00 + tax
  10.00; credit-note mismatch −100 scaled units; threshold 10% →
  1,000 bp boundary.
- **Not verified: compilation.** `cargo check` has never run in this
  VM (no toolchain, crates.io blocked) — do not claim compilation is
  verified. Two spots were fixed by hand for likely compile errors
  (a moved `bundle.discount_kind` reused after move; a
  `Value::get(String)` call) but nothing was compiled.

## 8. OPEN list

1. **Compilation unverified.** No toolchain in the VM; `cargo check`
   never ran. The module follows the `saved_views` / `calculator` /
   `pricing_tables` patterns exactly, and delimiters balance, but the
   first compile is still owed.
2. **Billing stores float money.** `billing.rs` `Invoice` /
   `InvoiceItem` use `f64` (pre-existing). The reconciliation contract
   is integer-only; the §6.4 snippet converts through the 4-decimal
   string form. Full end-to-end proof needs billing on integer money.
3. **Billing does not call the validator yet.** No listener for
   `pricing.invoice.validate_requested` exists in `billing.rs` (it was
   not edited, per the task). Until the §6.4 snippet lands,
   reconciliation is available over HTTP but not enforced at issue
   time. Billing's explicit-total override will fail reconciliation by
   design once enforced — that is the guard working, not a bug.
4. **Store coupon convergence outstanding.** The store keeps its own
   float coupons; the pricing engine is the shared evaluator via hook
   (§6.5). The store must call the hook and retire
   `coupon_discount_for`; until then two coupon maths exist and must
   not run on the same order.
5. **Package entitlements not enforced.** `TenantContext` carries no
   entitlements yet (batch 17). Capability checks are the only gate
   until batch 17 wires entitlements in; see §5 convergence point.
6. **Batch 17 schema plumbing not on disk.** Migration tables are
   created in the runner's schema until re-pointed at per-tenant
   schemas. Single swap point: `resolve_tenant` in `pricing.rs`.
7. **`mod.rs` is behind the tree.** Only 72 of 134 module files are
   declared; `calculator` and `pricing_tables` are unwired too
   (pre-existing). The integrator must add the §6.1 lines and audit
   the rest; this module's `requires()` deliberately lists only
   registered modules.
8. **In-memory store is authoritative.** Diesel tables are persistence
   targets only (repo pattern); rows do not survive a restart until
   the DB-backed read path is wired.
9. **AI narrative is a template draft**, not a live LLM call. The AI
   module's capability runner is the place for live rewording; the
   numbers must always come from the engine record.
10. **No live end-to-end test.** No live invoice-issue run, no live
    store checkout through the hook, no live WP render — all
    verification is read-back plus pure-function unit tests.
11. **Module size.** 7,216 lines vs the 3,000–4,000 aim (58 routes +
    full CRUD + 21 tests, no stubs). Integrator to confirm the size is
    acceptable or split the read surface later.
12. **Tenant-rule filing.** The follow-up asked for the tenant section
    in `INTEGRATION_NOTES_BATCH15_B2.md`; it is recorded here instead
    because that file is B2's (see §5 filing note). Coordinator to
    copy or cross-reference.
