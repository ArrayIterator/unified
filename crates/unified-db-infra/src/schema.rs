// Diesel schema for the Unified infra bounded context.
//
// Infrastructure and governance: compliance, privacy, gamification, network tools, gateway, webhooks, vault, and web3.
//
// Split out of `unified-core/src/db/schema.rs`; see
// `crates/unified-db-infra/src/lib.rs` for the split rules.
// Money is NUMERIC(19,4) everywhere (Diesel `Numeric`, Rust
// `diesel::data_types::PgNumeric`); see docs/SCHEMA.md.
//
// Split into `schema/<group>.rs` (one file per domain group) plus
// `schema/joins.rs` for the join declarations; this file only declares
// the submodules and re-exports their tables, so every
// `crate::schema::*` path is unchanged.

pub mod audit;
pub mod calculator;
pub mod cloudflare;
pub mod compliance;
pub mod departments;
pub mod gamification;
pub mod google;
pub mod jobs;
pub mod joins;
pub mod maintenance_status;
pub mod network_tools;
pub mod privacy;
pub mod providers;
pub mod routes;
pub mod sessions;
pub mod vault;
pub mod web3;
pub mod webhooks;

pub use audit::*;
pub use calculator::*;
pub use cloudflare::*;
pub use compliance::*;
pub use departments::*;
pub use gamification::*;
pub use google::*;
pub use jobs::*;
pub use maintenance_status::*;
pub use network_tools::*;
pub use privacy::*;
pub use providers::*;
pub use routes::*;
pub use sessions::*;
pub use vault::*;
pub use web3::*;
pub use webhooks::*;
