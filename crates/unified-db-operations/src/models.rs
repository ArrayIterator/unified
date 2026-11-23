// Diesel models for the Unified operations bounded context.
//
// Operations: projects, manufacturing, logistics, assets, field work, storage, clusters, incidents, and risk.
//
// Split out of `unified-core/src/db/models.rs`; the structs below
// are byte-identical to the originals, only regrouped. See
// `crates/unified-db-operations/src/lib.rs` for the split rules.
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

pub mod assets;
pub mod backup;
pub mod cluster_sync;
pub mod clusters;
pub mod field_service;
pub mod health;
pub mod incidents;
pub mod load_balancer;
pub mod logistics;
pub mod manufacturing;
pub mod project_tasks;
pub mod projects;
pub mod properties;
pub mod risk;
pub mod storage_buckets;
pub mod storage_plans;
pub mod storage_targets;

pub use assets::*;
pub use backup::*;
pub use cluster_sync::*;
pub use clusters::*;
pub use field_service::*;
pub use health::*;
pub use incidents::*;
pub use load_balancer::*;
pub use logistics::*;
pub use manufacturing::*;
pub use project_tasks::*;
pub use projects::*;
pub use properties::*;
pub use risk::*;
pub use storage_buckets::*;
pub use storage_plans::*;
pub use storage_targets::*;
