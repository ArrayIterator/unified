// Pricing Tables + Marketplace Builder module.
//
// Two builders in one sellable module (`available_as_product`, category
// `commerce`):
//
//   1. Pricing tables -- an admin builds reusable pricing tables made of
//      tiers with monthly/yearly prices, per-tier feature lists, a
//      billing-cycle toggle (monthly/yearly with a yearly discount
//      rule), and table settings (currency, layout, theme accent).
//      A table is rendered by pages as structured JSON through the
//      render-data endpoint; this module never renders HTML.
//   2. Marketplace builder -- on top of the Site Builder and the Store,
//      an admin runs a marketplace: vendor onboarding, vendor product
//      approval, per-vendor / per-category / global commission rules
//      computed from store order events, vendor dashboards aggregated
//      from those computed records, and vendor payouts requested
//      through billing.
//
// Invariants enforced on every write:
//   - Every price is >= 0; every rate and discount is 0..=100.
//   - Exactly one highlighted tier per table while the table has any
//     tier: creating the first tier highlights it; highlighting a tier
//     clears the flag on every other tier of the same table; clearing
//     the only highlighted tier is rejected; deleting the highlighted
//     tier promotes the lowest sort-order remaining tier.
//   - CTA URLs are relative paths (`/path`) or `https://` URLs only.
//     `javascript:`, `data:`, `vbscript:`, protocol-relative (`//host`)
//     and plain `http://` URLs are rejected (no open redirects).
//
// Money: amounts are `f64` in memory (the same convention as the
// Store module) and `NUMERIC(19,4)` in the database migration
// (`migrations/2026-11-22-000041_pricing_marketplace`). Commission
// amounts are rounded to 4 decimal places when computed.
//
// Source of truth: the Store module owns products and orders. This
// module stores only the approval state + vendor link for a product
// (`marketplace_product_approvals`), the computed commission ledger
// (`marketplace_commissions`, idempotent per order + product), and
// payout records. Vendor dashboard figures are aggregated live from
// those records, never stored twice.
//
// Routes (all under `/api/v1/pricing-tables`):
//   Tables:
//     GET    /tables                                  - paginated tables
//     POST   /tables                                  - create a table
//     GET    /tables/{id}                             - one table
//     PUT    /tables/{id}                             - update a table
//     DELETE /tables/{id}                             - delete a table + tiers + features
//     POST   /tables/{id}/publish                     - draft -> published
//     POST   /tables/{id}/unpublish                   - published -> draft
//     GET    /tables/{id}/render                      - structured render data
//     GET    /tables/{id}/embed                       - table id + render URL + snippet
//   Tiers:
//     GET    /tables/{id}/tiers                       - tiers in sort order
//     POST   /tables/{id}/tiers                       - create a tier
//     GET    /tables/{id}/tiers/{tier_id}             - one tier
//     PUT    /tables/{id}/tiers/{tier_id}             - update a tier
//     DELETE /tables/{id}/tiers/{tier_id}             - delete a tier + features
//   Tier features:
//     GET    /tables/{id}/tiers/{tier_id}/features    - features in sort order
//     POST   /tables/{id}/tiers/{tier_id}/features    - create a feature row
//     GET    /tables/{id}/tiers/{tier_id}/features/{feature_id}
//     PUT    /tables/{id}/tiers/{tier_id}/features/{feature_id}
//     DELETE /tables/{id}/tiers/{tier_id}/features/{feature_id}
//   Marketplace vendors:
//     GET    /marketplace/vendors                      - paginated vendors
//     POST   /marketplace/vendors                      - vendor application
//     GET    /marketplace/vendors/{id}                 - one vendor
//     PUT    /marketplace/vendors/{id}                 - update vendor (admin)
//     POST   /marketplace/vendors/{id}/approve         - pending -> approved
//     POST   /marketplace/vendors/{id}/reject          - pending -> rejected
//     POST   /marketplace/vendors/{id}/suspend         - approved -> suspended
//     POST   /marketplace/vendors/{id}/reactivate      - suspended -> approved
//     GET    /marketplace/vendors/{id}/dashboard       - aggregated sales/orders/payouts
//     GET    /marketplace/vendors/{id}/commissions     - vendor commission ledger
//     GET    /marketplace/vendors/{id}/payouts         - vendor payouts
//     POST   /marketplace/vendors/{id}/payouts         - request a payout
//   Marketplace storefront:
//     GET    /marketplace/storefronts/{slug}           - public storefront data
//   Commission rules:
//     GET    /marketplace/commission-rules             - paginated rules
//     POST   /marketplace/commission-rules             - create a rule
//     GET    /marketplace/commission-rules/{id}        - one rule
//     PUT    /marketplace/commission-rules/{id}        - update a rule
//     DELETE /marketplace/commission-rules/{id}        - delete a rule
//   Commissions:
//     GET    /marketplace/commissions                  - paginated ledger
//     GET    /marketplace/commissions/{id}             - one commission
//   Product approvals:
//     POST   /marketplace/products/{product_id}/submit - vendor submits a product
//     GET    /marketplace/product-approvals            - paginated approvals
//     GET    /marketplace/product-approvals/{id}       - one approval
//     POST   /marketplace/product-approvals/{id}/approve
//     POST   /marketplace/product-approvals/{id}/reject
//   Payouts (admin):
//     GET    /marketplace/payouts                      - paginated payouts
//     GET    /marketplace/payouts/{id}                 - one payout
//     POST   /marketplace/payouts/{id}/approve         - requested -> approved
//     POST   /marketplace/payouts/{id}/reject          - requested -> rejected
//     POST   /marketplace/payouts/{id}/mark-paid       - approved -> paid
//
// Events emitted:
//   `pricing_table.created`, `pricing_table.published`,
//   `pricing_table.unpublished`, `pricing_table.deleted`,
//   `marketplace.vendor_applied`, `marketplace.vendor_approved`,
//   `marketplace.vendor_rejected`, `marketplace.vendor_suspended`,
//   `marketplace.vendor_reactivated`,
//   `marketplace.product_approval_requested`,
//   `marketplace.product_approved`, `marketplace.product_rejected`,
//   `marketplace.payout_requested`, `marketplace.payout_approved`,
//   `marketplace.payout_rejected`, `marketplace.payout_paid`,
//   `marketplace.commission_computed` (also for reversals, with
//   `"status": "reversed"`), `marketplace.commission_rule_created`,
//   `marketplace.commission_rule_updated`,
//   `marketplace.commission_rule_deleted`.
//
// Events consumed:
//   `order.paid` and `store.order.completed` -- the exact event names
//   the Store module emits when an order is paid / completed
//   (there is no `store.order_paid` event in the Store module; the
//   briefed name was corrected to the real ones). Each paid order
//   line is resolved to a vendor through the product approval link,
//   falling back to a vendor whose `company_id` matches the order
//   line's `company_id`, and a commission is computed. The handler
//   is idempotent: a commission already recorded for the same
//   order + product is never computed twice, so an order that emits
//   both events yields one ledger entry per line.
//   `store.order.refunded` -- pending commissions for the order are
//   reversed (status `reversed`). Commissions already paid out are
//   left untouched: clawing back a paid payout is a billing action,
//   not an automatic reversal, and the reversal event carries the
//   paid entries so billing can reconcile them.
//   `invoice.created` -- the exact event name the Billing module
//   emits (there is no `billing.invoice_created` event; the briefed
//   name was corrected to the real one). When the payload names a
//   payout id, the invoice id is attached to that payout for
//   reconciliation; otherwise the invoice reference is kept in the
//   in-memory reconciliation log the dashboard reports.
//
// Events answered (hook requests from other modules):
//   `pricing_table.render_requested` -- `{"table_id"}` answers with
//   the render data for a published table.
//   `marketplace.vendor_check` -- `{"user_id"}` answers with the
//   vendor id and status for that user, when one exists.
//   `marketplace.commission_quote` -- `{"amount", "vendor_id"?,
//   "category"?}` answers with the applicable rate and the computed
//   commission amount, without writing anything.
//   `marketplace.product_approval_status` -- `{"product_id"}` answers
//   with the approval status for that product, when one exists.
//
// Capability checks: every route except the two public read paths
// requires the caller headers `X-User-Id` + `X-Role` and a
// permission checked fail-closed through the `role.check` hook
// (same shape as the AI module). The public paths are
// `GET /tables/{id}/render` and
// `GET /marketplace/storefronts/{slug}`, which serve published
// content only -- a draft table or a non-approved vendor answers
// 404 there, exactly like a published page. All writes require
// `pricing_tables:manage` or `marketplace:manage`; vendor
// self-service (apply, own dashboard, own payouts, own product
// submissions) requires `marketplace:vendor` and ownership of the
// vendor record.


// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

pub mod ai_capabilities;
pub mod types;
pub mod state;
mod validation;
mod auth;
mod handlers_tables;
mod handlers_vendors;
mod handlers_marketplace;
mod hooks;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::*;
pub use self::state::*;
pub use self::validation::*;
pub use self::auth::*;
pub use self::handlers_tables::*;
pub use self::handlers_vendors::*;
pub use self::handlers_marketplace::*;
pub use self::hooks::*;
pub use self::module::*;
