# Integration Notes — Batch 3, Builder B6: Pricing Tables + Marketplace Builder

Module id: `pricing_tables` (`src/modules/pricing_tables.rs`, 3,785 lines).
Migration: `migrations/2026-11-22-000041_pricing_marketplace/` (`up.sql` 263 lines,
`down.sql` 11 lines). Site Builder extension: `src/modules/site_builder.rs`
(surgical edits only, see §6).

The integrator owns: `src/modules/mod.rs`, `src/db/schema.rs`,
`src/db/models.rs`, `README.md`, `TODO.md`. Nothing in those files was
touched by this builder. Apply the snippets below verbatim, in order.

Two briefed event names were wrong and were corrected against the real
source — the module listens to the real names, and the integrator must
not "fix" them back:
- Briefed `store.order_paid` does not exist. The Store module emits
  `order.paid` and `store.order.completed` (`src/modules/store.rs`,
  `pay_order` / `transition_order`). The module registers both.
- Briefed `billing.invoice_created` does not exist. The Billing module
  emits `invoice.created` (`src/modules/billing.rs`, three emit sites).
  The module registers `invoice.created`.

## 1. `src/modules/mod.rs`

### 1a. Module declaration (alphabetical; between `planner` and `products`)

Current lines 57–58:

```rust
pub mod planner;
pub mod products;
```

Change to:

```rust
pub mod planner;
pub mod pricing_tables;
pub mod products;
```

### 1b. Registration (commerce section; after `products`, before `payments`)

Current lines 141–146:

```rust
    store::register(registry);
    licensing::register(registry);
    software::register(registry);
    billing::register(registry);
    products::register(registry);
    payments::register(registry);
```

Change to:

```rust
    store::register(registry);
    licensing::register(registry);
    software::register(registry);
    billing::register(registry);
    products::register(registry);
    pricing_tables::register(registry);
    payments::register(registry);
```

Registration after `products` matters: the module `requires`
`["products", "store", "billing", "site_builder", "notifications"]`, and
`products`/`store`/`billing` are all registered in this same commerce
block (site_builder is registered earlier, in the content block).

## 2. `src/db/schema.rs` — table definitions

Append after the Project Management block (the last `diesel::table!`
block, ending with `project_activity`), and:

- Update the file header comment (lines 3–6): extend the migration range
  from `migrations/2026-11-22-000001_core/up.sql` through
  `migrations/2026-11-22-000031_projects/up.sql` to through
  `migrations/2026-11-22-000041_pricing_marketplace/up.sql`.
  (Other batch-3 migrations 000032–000040 land in the same range; keep
  the range end at the highest migration present.)
- Add the eight table names to the `allow_tables_to_appear_in_same_query!`
  list (line 3064): `pricing_tables`, `pricing_tiers`,
  `pricing_tier_features`, `marketplace_vendors`,
  `marketplace_commission_rules`, `marketplace_product_approvals`,
  `marketplace_commissions`, `marketplace_payouts`.
- Add joinable entries (next to the existing `diesel::joinable!` lines):
  `pricing_tiers -> pricing_tables (table_id)`,
  `pricing_tier_features -> pricing_tiers (tier_id)`,
  `marketplace_product_approvals -> marketplace_vendors (vendor_id)`,
  `marketplace_commissions -> marketplace_vendors (vendor_id)`,
  `marketplace_payouts -> marketplace_vendors (vendor_id)`.
  No joinable entries exist for the plain-VARCHAR references
  (`marketplace_vendors.company_id`, `marketplace_product_approvals.product_id`,
  `marketplace_commissions.order_id` / `product_id`,
  `marketplace_payouts.invoice_id`) — they are deliberately not foreign keys.

```rust
// Pricing Tables + Marketplace Builder (2026-11-22-000041_pricing_marketplace).
// ---------------------------------------------------------------------------

diesel::table! {
    pricing_tables (id) {
        id -> Uuid,
        name -> Varchar,
        slug -> Varchar,
        description -> Text,
        currency -> Varchar,
        layout -> Varchar,
        theme_accent -> Varchar,
        status -> Varchar,
        yearly_discount_percent -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_tiers (id) {
        id -> Uuid,
        table_id -> Uuid,
        name -> Varchar,
        description -> Text,
        monthly_price -> Numeric,
        yearly_price -> Numeric,
        cta_label -> Varchar,
        cta_url -> Varchar,
        highlighted -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    pricing_tier_features (id) {
        id -> Uuid,
        tier_id -> Uuid,
        feature_text -> Varchar,
        included -> Bool,
        note -> Text,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_vendors (id) {
        id -> Uuid,
        user_id -> Varchar,
        company_id -> Nullable<Varchar>,
        company_name -> Varchar,
        slug -> Varchar,
        email -> Varchar,
        status -> Varchar,
        application_notes -> Text,
        review_reason -> Text,
        reviewed_by -> Nullable<Varchar>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_commission_rules (id) {
        id -> Uuid,
        scope -> Varchar,
        scope_value -> Varchar,
        rate_percent -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_product_approvals (id) {
        id -> Uuid,
        product_id -> Varchar,
        vendor_id -> Uuid,
        category -> Nullable<Varchar>,
        status -> Varchar,
        reason -> Text,
        reviewed_by -> Nullable<Varchar>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_commissions (id) {
        id -> Uuid,
        order_id -> Varchar,
        vendor_id -> Uuid,
        product_id -> Varchar,
        order_total -> Numeric,
        rate_percent -> Numeric,
        commission_amount -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    marketplace_payouts (id) {
        id -> Uuid,
        vendor_id -> Uuid,
        amount -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        invoice_id -> Nullable<Varchar>,
        reference -> Varchar,
        requested_by -> Varchar,
        reviewed_by -> Nullable<Varchar>,
        review_reason -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

Money mapping (per the schema.rs header convention): every
`NUMERIC(19,4)` money column and every `NUMERIC(5,2)` rate/discount
column is Diesel `Numeric` and Rust `diesel::data_types::PgNumeric`
in models.rs. SQL types in the migration: `monthly_price` /
`yearly_price` / `order_total` / `commission_amount` / `amount` are
`NUMERIC(19,4)`; `yearly_discount_percent` / `rate_percent` are
`NUMERIC(5,2)`.

## 3. `src/db/models.rs` — struct definitions

Append at the end of the file, after the `ProjectActivity` /
`NewProjectActivity` structs. `PgNumeric` is already imported
(`use diesel::data_types::PgNumeric;`). These structs mirror §2 and
the migration exactly; `New*` structs carry only the fields an insert
needs (same pattern as `NewDepartment` / `NewProjectActivity`).

```rust
// ---------------------------------------------------------------------------
// Pricing Tables + Marketplace Builder (2026-11-22-000041_pricing_marketplace).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pricing_tables)]
pub struct PricingTable {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub currency: String,
    pub layout: String,
    pub theme_accent: String,
    pub status: String,
    pub yearly_discount_percent: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pricing_tables)]
pub struct NewPricingTable {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub currency: String,
    pub layout: String,
    pub theme_accent: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pricing_tiers)]
pub struct PricingTier {
    pub id: Uuid,
    pub table_id: Uuid,
    pub name: String,
    pub description: String,
    pub monthly_price: PgNumeric,
    pub yearly_price: PgNumeric,
    pub cta_label: String,
    pub cta_url: String,
    pub highlighted: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pricing_tiers)]
pub struct NewPricingTier {
    pub id: Uuid,
    pub table_id: Uuid,
    pub name: String,
    pub monthly_price: PgNumeric,
    pub yearly_price: PgNumeric,
    pub cta_label: String,
    pub cta_url: String,
    pub highlighted: bool,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = pricing_tier_features)]
pub struct PricingTierFeature {
    pub id: Uuid,
    pub tier_id: Uuid,
    pub feature_text: String,
    pub included: bool,
    pub note: String,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = pricing_tier_features)]
pub struct NewPricingTierFeature {
    pub id: Uuid,
    pub tier_id: Uuid,
    pub feature_text: String,
    pub included: bool,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketplace_vendors)]
pub struct MarketplaceVendor {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<String>,
    pub company_name: String,
    pub slug: String,
    pub email: String,
    pub status: String,
    pub application_notes: String,
    pub review_reason: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketplace_vendors)]
pub struct NewMarketplaceVendor {
    pub id: Uuid,
    pub user_id: String,
    pub company_name: String,
    pub slug: String,
    pub email: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketplace_commission_rules)]
pub struct MarketplaceCommissionRule {
    pub id: Uuid,
    pub scope: String,
    pub scope_value: String,
    pub rate_percent: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketplace_commission_rules)]
pub struct NewMarketplaceCommissionRule {
    pub id: Uuid,
    pub scope: String,
    pub scope_value: String,
    pub rate_percent: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketplace_product_approvals)]
pub struct MarketplaceProductApproval {
    pub id: Uuid,
    pub product_id: String,
    pub vendor_id: Uuid,
    pub category: Option<String>,
    pub status: String,
    pub reason: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketplace_product_approvals)]
pub struct NewMarketplaceProductApproval {
    pub id: Uuid,
    pub product_id: String,
    pub vendor_id: Uuid,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketplace_commissions)]
pub struct MarketplaceCommission {
    pub id: Uuid,
    pub order_id: String,
    pub vendor_id: Uuid,
    pub product_id: String,
    pub order_total: PgNumeric,
    pub rate_percent: PgNumeric,
    pub commission_amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketplace_commissions)]
pub struct NewMarketplaceCommission {
    pub id: Uuid,
    pub order_id: String,
    pub vendor_id: Uuid,
    pub product_id: String,
    pub order_total: PgNumeric,
    pub rate_percent: PgNumeric,
    pub commission_amount: PgNumeric,
    pub currency: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketplace_payouts)]
pub struct MarketplacePayout {
    pub id: Uuid,
    pub vendor_id: Uuid,
    pub amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub invoice_id: Option<String>,
    pub reference: String,
    pub requested_by: String,
    pub reviewed_by: Option<String>,
    pub review_reason: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketplace_payouts)]
pub struct NewMarketplacePayout {
    pub id: Uuid,
    pub vendor_id: Uuid,
    pub amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub requested_by: String,
}
```

Note: the module's in-memory structs (`src/modules/pricing_tables.rs`)
use `f64` for money, matching the Store module's convention; the Diesel
models above use `PgNumeric`, matching the `schema.rs` money convention.
The in-memory DashMap store remains authoritative at runtime (see
TODO.md); these tables are the persistence target, not yet the read path.

## 4. `README.md` — lines to add

Insert as a new numbered entry in the module list, between item 59
(**Resource Planner**) and item 60 (**Product Management**), renumbering
the entries after it by +1:

```markdown
60. **Pricing Tables & Marketplace Builder** — reusable pricing tables
    with tiers, per-tier feature lists, a monthly/yearly billing-cycle
    toggle with a yearly discount rule, publish/unpublish, structured
    render data and embed snippets; plus a marketplace builder on top
    of the Site Builder and the Store: vendor onboarding
    (apply/review/approve/reject/suspend), vendor product approval,
    per-vendor / per-category / global commission rules computed from
    paid store orders, vendor dashboards aggregated live from the
    commission ledger, and vendor payouts requested through billing.
    Sellable as a product (category `commerce`).
```

## 5. `TODO.md` — lines to add

Append to the batch-3 / integration section (near the existing
"30 migrations (`2026-11-22-000001_core` … `000030_ai`)" entry, updating
that count when the batch lands), or as a new bullet under the
module-completion list:

```markdown
- Pricing Tables + Marketplace Builder (`pricing_tables`, migration
  `2026-11-22-000041_pricing_marketplace`): 49 routes, 8 tables,
  registered after `products` in `src/modules/mod.rs`. In-memory
  DashMap store is authoritative; the Diesel tables in `schema.rs` /
  `models.rs` are the persistence target, not yet the read path.
  Events consumed are the real Store/Billing names — `order.paid`,
  `store.order.completed`, `store.order.refunded`, `invoice.created`
  (the briefed `store.order_paid` / `billing.invoice_created` names do
  not exist). Unverified: `cargo check` (no toolchain in the sandbox),
  live HTTP round-trip, live Diesel migration run.
```

## 6. `src/modules/site_builder.rs` — edits made (verify, do not re-apply)

Two surgical edits, nothing else changed in the file (1,196 → 1,245 lines).
No existing block type, Page/Section shape, route, or handler was modified.

### 6a. `BLOCK_TYPES` constant + doc comment — lines 58–82

Was (2 lines, 58–59):

```rust
/// Block types the server-side renderer knows how to render.
pub const BLOCK_TYPES: &[&str] = &["hero", "text", "image", "gallery", "form", "custom"];
```

Now (lines 58–82): the same doc comment extended with the two new block
types and their content keys, and the constant expanded to:

```rust
pub const BLOCK_TYPES: &[&str] = &[
    "hero",
    "text",
    "image",
    "gallery",
    "form",
    "custom",
    "pricing_table",
    "marketplace_storefront",
];
```

This is the only change to block-type validation: `create_block` /
`update_block` already validate against `BLOCK_TYPES`, so the two new
types are accepted by the existing handlers with no further edits.

### 6b. `render_block_default` — two new match arms, lines 393–418

Inserted immediately after the `"custom"` arm (line 392), before the
`other =>` fallback (line 419). Both arms render placeholders only —
escaped ids, no data resolution, no dependency on the pricing tables
module:

- `"pricing_table"` (lines 393–406): reads content `pricing_table_id`
  (fallback `table_id`) and renders
  `<section class="sb-pricing-table" data-pricing-table-id="…"
  data-render-url="/api/v1/pricing-tables/tables/{id}/render"></section>`.
- `"marketplace_storefront"` (lines 407–418): reads content
  `vendor_slug` (fallback `vendor_id`) and renders
  `<section class="sb-marketplace-storefront" data-vendor-slug="…"
  data-storefront-url="/api/v1/pricing-tables/marketplace/storefronts/{slug}"></section>`.

Verify with: `grep -n "pricing_table\|marketplace_storefront"
src/modules/site_builder.rs` — expected hits: the doc comment (lines
60–71), the constant (lines 80–81), and the two render arms (lines
393, 407). No other file references these block types.

## 7. What the integrator must NOT do

- Do not rename the consumed events to `store.order_paid` or
  `billing.invoice_created`: those names do not exist in `store.rs` /
  `billing.rs`.
- Do not add a foreign key from `marketplace_product_approvals` to any
  product table, or from `marketplace_commissions` to any order table:
  the Store module owns products and orders, and this module stores
  only the approval state + vendor link and the computed ledger.
- Do not add a database CHECK enforcing exactly one highlighted tier:
  the invariant is enforced in code (`pricing_tables.rs`,
  `clear_other_highlights` / `promote_next_highlight` / the
  update-tier rejection), because "exactly one true per table while
  tiers exist" cannot be expressed as a partial unique index.
- Do not wire the Site Builder blocks to resolve pricing/vendor data
  server-side: the blocks intentionally render placeholders, and the
  live data comes from the render/storefront endpoints.
- The public render endpoint (`GET /tables/{id}/render`) and the public
  storefront endpoint (`GET /marketplace/storefronts/{slug}`) serve
  published content only; draft tables and non-approved vendors answer
  404 there. Every other route requires `X-User-Id` + `X-Role` headers
  and a permission checked fail-closed through `role.check`.

## 8. Verification the integrator should run (not run by this builder)

- `cargo check` (no Rust toolchain exists in this sandbox; never run).
- Live HTTP round-trip of all 49 routes, including the
  exactly-one-highlighted-tier transitions and the CTA URL rejections.
- `diesel migration run` for `2026-11-22-000041_pricing_marketplace`
  against a real PostgreSQL instance, then `diesel migration revert`
  with `down.sql`.
- Confirm migration number 000041 does not collide with other batch-3
  builders (coordinators were assigned 000032+ / 000036+ / 000041+;
  000041 is this builder's number; no other 000041 migration existed
  in the staging directory at build time).
