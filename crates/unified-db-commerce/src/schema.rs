// Diesel schema for the Unified commerce bounded context.
//
// Selling: products, pricing, payments, billing, subscriptions, marketplace, affiliates, expenses, and tax.
//
// Split out of `unified-core/src/db/schema.rs`; see
// `crates/unified-db-commerce/src/lib.rs` for the split rules.
// Money is NUMERIC(19,4) everywhere (Diesel `Numeric`, Rust
// `diesel::data_types::PgNumeric`); see docs/SCHEMA.md.
//
// Split into `schema/<group>.rs` (one file per domain group) plus
// `schema/joins.rs` for the join declarations; this file only declares
// the submodules and re-exports their tables, so every
// `crate::schema::*` path is unchanged.

// Five group files are named after a table they contain
// (`affiliates`, `digital_products`, `expenses`, `products`,
// `saas_packages`). An explicit `pub mod <name>` binding wins over
// the `pub use <name>::*` re-export of the same name, so the table
// module would be shadowed and every derive / `joinable!` reference
// to it fails with E0425 ("cannot find type/value ... in module
// ..."). Those five file modules are therefore declared under a
// `_tables` name (same file, via `#[path = "schema/<group>.rs"]`;
// note: `#[path]` resolves relative to the crate root, not the
// declaring file); every
// `crate::schema::<table>` path is unchanged. Never name a new
// `schema/<group>.rs` file after a table it defines.

#[path = "schema/affiliates.rs"]
pub mod affiliates_tables;
pub mod cards;
#[path = "schema/digital_products.rs"]
pub mod digital_products_tables;
#[path = "schema/expenses.rs"]
pub mod expenses_tables;
pub mod joins;
pub mod loyalty;
pub mod marketing;
pub mod payments;
pub mod pos;
pub mod pricing;
pub mod pricing_tables_marketplace_saas_domains;
pub mod printing;
pub mod procurement;
#[path = "schema/products.rs"]
pub mod products_tables;
#[path = "schema/saas_packages.rs"]
pub mod saas_packages_tables;
pub mod tax_subscriptions;

pub use affiliates_tables::*;
pub use cards::*;
pub use digital_products_tables::*;
pub use expenses_tables::*;
pub use loyalty::*;
pub use marketing::*;
pub use payments::*;
pub use pos::*;
pub use pricing::*;
pub use pricing_tables_marketplace_saas_domains::*;
pub use printing::*;
pub use procurement::*;
pub use products_tables::*;
pub use saas_packages_tables::*;
pub use tax_subscriptions::*;
