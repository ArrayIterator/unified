// Diesel schema for the Unified operations bounded context.
//
// Operations: projects, manufacturing, logistics, assets, field work, storage, clusters, incidents, and risk.
//
// Split out of `unified-core/src/db/schema.rs`; see
// `crates/unified-db-operations/src/lib.rs` for the split rules.
// Money is NUMERIC(19,4) everywhere (Diesel `Numeric`, Rust
// `diesel::data_types::PgNumeric`); see docs/SCHEMA.md.
//
// Split into `schema/<group>.rs` (one file per domain group) plus
// `schema/joins.rs` for the join declarations; this file only declares
// the submodules and re-exports their tables, so every
// `crate::schema::*` path is unchanged.
//
// NOTE (name-collision rule): a file module and a table must never share
// a name. Each `diesel::table!` block defines a table module inside its
// file, so `schema/<group>.rs` defining a table named `<group>` would make
// `crate::schema::<group>` resolve to the *file* module (an explicit
// `pub mod` binding wins over the `pub use <group>::*` glob re-export),
// and every `#[diesel(table_name = <group>)]` then fails with E0425
// ("cannot find type `id` in module `<group>`") while the `allow_tables_
// to_appear_in_same_query!` groups emit bogus `TableNotEqual<{error}>`
// impls that poison coherence (E0119). The nine groups below define a
// table of their own name, so they are declared under a `_tables` name
// (same file, via `#[path = "schema/<group>.rs"]` — `#[path]` resolves
// relative to the crate root, not this file) and re-exported from there.
// Never name a new `schema/<group>.rs` file after a table it defines.

pub mod backup;
pub mod cluster_sync;
pub mod field_service;
pub mod health;
pub mod joins;
pub mod load_balancer;
pub mod logistics;
pub mod manufacturing;
pub mod risk;

#[path = "schema/assets.rs"]
pub mod assets_tables;
#[path = "schema/clusters.rs"]
pub mod clusters_tables;
#[path = "schema/incidents.rs"]
pub mod incidents_tables;
#[path = "schema/project_tasks.rs"]
pub mod project_tasks_tables;
#[path = "schema/projects.rs"]
pub mod projects_tables;
#[path = "schema/properties.rs"]
pub mod properties_tables;
#[path = "schema/storage_buckets.rs"]
pub mod storage_buckets_tables;
#[path = "schema/storage_plans.rs"]
pub mod storage_plans_tables;
#[path = "schema/storage_targets.rs"]
pub mod storage_targets_tables;

pub use assets_tables::*;
pub use backup::*;
pub use cluster_sync::*;
pub use clusters_tables::*;
pub use field_service::*;
pub use health::*;
pub use incidents_tables::*;
pub use load_balancer::*;
pub use logistics::*;
pub use manufacturing::*;
pub use project_tasks_tables::*;
pub use projects_tables::*;
pub use properties_tables::*;
pub use risk::*;
pub use storage_buckets_tables::*;
pub use storage_plans_tables::*;
pub use storage_targets_tables::*;
