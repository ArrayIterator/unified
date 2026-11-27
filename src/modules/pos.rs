// Point of Sale (POS) module - in-person sales at a physical till.
//
// Registers (terminals) with open/close shifts and cashier
// assignment, an in-person sale flow (cart lines resolved from the
// catalogue by product id, barcode, or SKU), configurable payment
// methods with split payment and change calculation, structured
// receipts with a printable line format, refunds and voids, and
// end-of-shift / end-of-day reports.
//
// Interconnection happens only through the hook bus; this module
// never calls another module directly:
//
//   - Sale completion emits `order.created` carrying a
//     store-shaped order payload, so the store module records the
//     order, the billing module builds its invoice from it, and the
//     tax module computes and stores the tax for it (the tax
//     module listens to `order.created`). The payload always
//     carries `tax_inclusive` (the tax module defaults it to
//     false, so omitting it would mis-tax inclusive sales) and
//     `country_code` when the store country is configured. It also
//     emits `pos.sale.completed` with the full sale and receipt id.
//   - Stock is never decremented here. Completion emits one
//     `assets.stock.decrement_requested` event per line
//     (`{asset_id, quantity, reason, sale_id, register_id,
//     product_id, sku, barcode}`); the inventory side maps the
//     catalogue product to its stock item and applies the move.
//     The assets module registers listeners for both the
//     decrement and the increment events, so stock moves are
//     applied by the inventory side.
//     Refunds emit `assets.stock.increment_requested` the same way.
//   - Refunds emit `payment.refunded` (`{order_id, sale_id,
//     amount, method}`) so the store and billing modules apply
//     their own refund handling (licence suspension, credit notes).
//   - Product lookup does NOT emit `pos.product.search_requested`:
//     no module in the repo listens to that event, so the search
//     endpoint reports honestly that lookup is unavailable instead
//     of emitting an event nobody answers. The follow-up is a
//     catalogue integration that registers a listener for it.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory registers, shifts, sales, payments, receipts, refunds,
// and payment-method definitions; shift cash reconciliation with a
// variance report; split payments with change; receipt data plus a
// structured print-line format. What is NOT built here:
//   - True offline queueing / sync. There is no offline client in
//     this codebase and this module does not pretend one exists:
//     every sale is recorded server-side at completion time. The
//     settings response reports
//     `offline_queueing: "not_supported"` and offline support is a
//     FUTURE item for a client that can queue and replay sales.
//   - A request/response tax-quote hook. The tax module has no
//     `tax.compute.requested` listener (verified: it listens to
//     `invoice.created`, `order.created`, and
//     `tax.filing.check_requested` only), so POS computes line tax
//     from the caller-supplied per-line tax rate (resolved by the
//     terminal from the tax rules, or the configured default rate)
//     and the tax module independently computes and stores the tax
//     from the emitted `order.created` payload. The POS tax figures
//     are a snapshot; the tax module's computation is authoritative.
//   - Stock application. The decrement/increment events above
//     are a defined contract, and the assets module registers
//     listeners for both (`assets.stock.decrement_requested` and
//     `assets.stock.increment_requested`), so stock moves are
//     applied by the inventory side.
//   - Persistence. Durable rows are defined by migration
//     `2026-11-22-000074_pos`; until that migration is executed,
//     POS records do not survive a restart. Payment-method
//     definitions and POS settings are admin-editable runtime
//     configuration (same pattern as the billing settings); their
//     durable form is a follow-up for the settings store.
//
// There are no public (unauthenticated) routes in this module:
// every route requires the `X-User-Id` / `X-Role` headers and a
// capability check, and no POS route is exposed through the API
// Manager public-access rules (default deny).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the assets module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).

// Pos module root.
//
// The module is split into one file per concern under
// `src/modules/pos/`. The public API is unchanged: every item
// previously reachable as `crate::modules::pos::Item` is
// re-exported here.

mod constants;
pub mod types;
pub mod requests;
pub mod state;
mod validation;
pub mod module;
mod handlers_registers;
mod handlers_sales;
mod handlers_checkout;
mod handlers_receipts;
mod handlers_admin;

pub use self::types::*;
pub use self::requests::*;
pub use self::state::*;
pub use self::module::*;
pub(crate) use self::constants::*;
pub(crate) use self::types::*;
pub(crate) use self::requests::*;
pub(crate) use self::state::*;
pub(crate) use self::validation::*;
pub(crate) use self::module::*;
pub(crate) use self::handlers_registers::*;
pub(crate) use self::handlers_sales::*;
pub(crate) use self::handlers_checkout::*;
pub(crate) use self::handlers_receipts::*;
pub(crate) use self::handlers_admin::*;
