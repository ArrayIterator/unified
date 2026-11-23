//! Diesel schema and models for the Unified commerce bounded context.
//!
//! Selling: products, pricing, payments, billing, subscriptions, marketplace, affiliates, expenses, and tax.
//!
//! Split out of `unified-core/src/db` (schema.rs + models.rs) so no
//! single rustc invocation compiles the whole 675-table schema:
//! each crate owns its tables, its `joinable!` declarations, and
//! its `allow_tables_to_appear_in_same_query!` groups (every join
//! graph component lives whole inside one crate, so no join or
//! allow declaration spans crates). Migrations are unchanged and
//! still live in `crates/unified-core/migrations/`; the tables,
//! columns, and models here are byte-identical to the originals,
//! only regrouped. `unified-core` re-exports everything, so the
//! `unified_core::db::schema::*` / `unified_core::db::models::*`
//! paths are unchanged.

pub mod models;
pub mod schema;
