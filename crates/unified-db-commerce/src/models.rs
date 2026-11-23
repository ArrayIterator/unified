// Diesel models for the Unified commerce bounded context.
//
// Selling: products, pricing, payments, billing, subscriptions, marketplace, affiliates, expenses, and tax.
//
// Split out of `unified-core/src/db/models.rs`; the structs below
// are byte-identical to the originals, only regrouped. See
// `crates/unified-db-commerce/src/lib.rs` for the split rules.
//
// Money fields use `diesel::data_types::PgNumeric` (see the note
// above the store models in the original file); models that carry
// money skip the serde derives because `PgNumeric` has no serde
// implementation.
//
// `New*` structs carry every column except `created_at` /
// `updated_at` and except columns the database fills with a DEFAULT
// (primary-key columns are always carried, callers supply ids);
// the DEFAULT then fires on insert. Secret columns (stored
// credentials, key hashes, credential blobs) carry
// `#[serde(skip_serializing)]` on the read struct, like
// `User.password_hash`.
//
// Split into `models/<group>.rs` (one file per domain group); this file
// only declares the submodules and re-exports their structs, so every
// `crate::models::*` path is unchanged.

pub mod affiliates;
pub mod cards;
pub mod digital_products;
pub mod expenses;
pub mod loyalty;
pub mod marketing;
pub mod payments;
pub mod pos;
pub mod pricing;
pub mod pricing_tables_marketplace_saas_domains;
pub mod printing;
pub mod procurement;
pub mod products;
pub mod saas_packages;
pub mod tax_subscriptions;

pub use affiliates::*;
pub use cards::*;
pub use digital_products::*;
pub use expenses::*;
pub use loyalty::*;
pub use marketing::*;
pub use payments::*;
pub use pos::*;
pub use pricing::*;
pub use pricing_tables_marketplace_saas_domains::*;
pub use printing::*;
pub use procurement::*;
pub use products::*;
pub use saas_packages::*;
pub use tax_subscriptions::*;
