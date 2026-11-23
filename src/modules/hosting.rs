// Hosting module — hosting plans and hosting accounts.
//
// A hosting plan is a sellable resource bundle (disk / CPU / RAM /
// bandwidth, price, billing cycle) optionally linked to a store product
// id, so the plan can be sold through the Store. A hosting account
// binds one plan to one domain and one company, and moves through the
// lifecycle `pending -> active -> suspended -> active` with `cancelled`
// as the terminal state.
//
// Module requirements (see `Module::requires`): the Store (plans are
// sold as store products), Domains (accounts are bound to a registered
// domain), and Network (domain syntax checks and the DNS / TLS probe
// behind domain verification). There is no separate `ssl` module in
// this codebase: TLS/SSL is a core HTTP-layer capability
// (`unified_core::tls`, ACME in `crate::http::server`), surfaced to
// modules through the Domains module — domain `ssl_status` /
// `ssl_expires_at` plus the `ssl.renewed` / `ssl.swapped` / `ssl.swap`
// hook events — and
// through the Network module's TLS probe. Requiring `domains` and
// `network` is therefore the closest real declaration of the SSL
// capability, and this module never invents an `ssl` module id.
//
// Interconnection happens through the hook bus only; this module does
// not edit or call into the Store or Domains modules:
//   - It listens to `store.product.published` /
//     `store.product.unpublished` to learn which store product ids
//     exist and are published.
//   - It listens to `store.order.completed` and `order.paid` to record
//     paid orders for a hosting plan as provisioning claims, so an
//     account created with `store_order_id` can be verified against a
//     real paid order.
//   - It listens to `domain.verify` and to the completed
//     `ssl.renewed` / `ssl.swapped` / `ssl.swap` events to observe
//     domain registration and SSL state without a database.
//   - It emits `hosting.plan.created`, `hosting.plan.updated`,
//     `hosting.account.created`, `hosting.account.updated`,
//     `hosting.account.activated`, `hosting.account.suspended`,
//     `hosting.account.cancelled`, and `hosting.requirements.checked`.
//
// Persistence: in-memory stores (DashMap), exactly like the Store
// module. The Diesel tables for this module do not exist yet — see the
// migration + `schema.rs` snippet in the delivery report; once they
// are applied, persistence follows the Domains module pattern (insert
// on create, update on change, load from the pool when one is
// attached). The domain / SSL requirement checks already read the
// `domains` table from the pool when one is attached, because that
// table exists today.

pub mod ai_capabilities;
pub mod types;
mod requests;
pub mod views;
mod helpers;
mod handlers_plans;
mod handlers_accounts;
mod hooks;
pub mod module;

// Public API (unchanged): re-exported from the split files.
// From types.rs:
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::BILLING_CYCLES;
pub use self::types::HostingAccount;
pub use self::types::HostingPlan;
pub use self::types::HostingState;
// From views.rs:
pub use self::views::AccountRequirementReport;
pub use self::views::RequirementItem;
// From module.rs:
pub use self::module::HostingModule;
pub use self::module::register;
