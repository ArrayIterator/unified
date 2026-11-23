// Diesel models for the tables in `schema.rs` (migrations
// 000001-000107).
//
// These are the shared persistence types for every table in the
// schema: users, roles, companies, domains, Geo overrides, licenses,
// all software, store, and VCS tables, and every table added by
// migrations 000004-000030. Modules that keep their own data in an
// in-memory store today use these tables as their persistence target,
// not yet their read path; see TODO.md.
//
// Money fields use `diesel::data_types::PgNumeric` (see the note above
// the store models); models that carry money skip the serde derives
// because `PgNumeric` has no serde implementation.
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

pub mod ai_policies;
pub mod api;
pub mod certificates;
pub mod domain_verification;
pub mod entitlements;
pub mod hosting;
pub mod identity;
pub mod invoices;
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
