// Diesel schema for the core tables.
//
// This schema matches every migration from
// `migrations/2026-11-22-000001_core/up.sql` through
// `migrations/2026-11-22-000108_paid_certification_exams/up.sql` (migration 000003 is
// ALTER-only; its added columns and NUMERIC(19,4) conversions are
// folded into the table definitions below; migrations 000032-000107
// were added by the integration round from the batch integration
// notes, with column definitions copied from each migration's
// `up.sql` or from the notes' `table!` snippets where the notes
// carry them).
// Modules that need persistence beyond their in-memory store use these
// tables; the software, store, and VCS modules still keep their live
// data in memory (see TODO.md) and these tables are their persistence
// target, not yet their read path.
//
// KNOWN COLLISIONS (resolved during the final inspection; do not
// reintroduce - see the integration report):
// - `certificates` is the SSL-certificate table (migration
//   000005). Migration 000079_elearning used to create a second
//   `certificates` table with a different shape (course
//   certificates); it was renamed to `elearning_certificates`
//   (migration + `table!` + structs below) before any migration
//   run, so both shapes exist now.
// - `pricing_tiers` is the 000041_pricing_marketplace shape
//   (pricing-table feature tiers). Migration 000103_pricing used
//   to create a second `pricing_tiers` table (volume quantity
//   tiers); it was renamed to `pricing_volume_tiers` (migration +
//   `table!` + structs below) before any migration run.
// - `reference_id_settings` has no migration yet (snippet only in
//   the batch-18 notes); `tenant_destroy_requests` likewise has no
//   migration yet (derived from the `DestroyRequest` struct in
//   `src/modules/tenant_destroy.rs`). `destroyed_tenants` is NOT
//   added: no struct or DDL for it exists anywhere (the module
//   states destroyed tenants have no record of their own - the
//   `tenants` row itself is the guard record, with `destroyed_at`
//   and `export_reference` already recorded there).
//
// Money is NUMERIC(19,4) everywhere (Diesel `Numeric`, Rust
// `diesel::data_types::PgNumeric`); see docs/SCHEMA.md.
//
// Split into `schema/<group>.rs` (one file per domain group) plus
// `schema/joins.rs` for the join declarations; this file only declares
// the submodules and re-exports their tables, so every
// `crate::schema::*` path is unchanged.

pub mod ai_policies;
pub mod api;
pub mod certificates;
pub mod domain_verification;
pub mod entitlements;
pub mod hosting;
pub mod identity;
pub mod invoices;
pub mod joins;
pub mod licensing;
pub mod security_mcp_ai;
pub mod software;
pub mod store;
pub mod tasks;
pub mod tenants;
pub mod timetracker;
pub mod tracker_staff;
pub mod vcs;
pub mod whitelabel;

pub use ai_policies::*;
pub use api::*;
pub use certificates::*;
pub use domain_verification::*;
pub use entitlements::*;
pub use hosting::*;
pub use identity::*;
pub use invoices::*;
pub use licensing::*;
pub use security_mcp_ai::*;
pub use software::*;
pub use store::*;
pub use tasks::*;
pub use tenants::*;
pub use timetracker::*;
pub use tracker_staff::*;
pub use vcs::*;
pub use whitelabel::*;
