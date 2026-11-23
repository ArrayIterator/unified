// Diesel models for the Unified infra bounded context.
//
// Infrastructure and governance: compliance, privacy, gamification, network tools, gateway, webhooks, vault, and web3.
//
// Split out of `unified-core/src/db/models.rs`; the structs below
// are byte-identical to the originals, only regrouped. See
// `crates/unified-db-infra/src/lib.rs` for the split rules.
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

pub mod audit;
pub mod calculator;
pub mod cloudflare;
pub mod compliance;
pub mod departments;
pub mod gamification;
pub mod google;
pub mod jobs;
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
