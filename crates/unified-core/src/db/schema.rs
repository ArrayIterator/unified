// Re-export shim: the Diesel schema/models now live in the
// `unified-db-*` bounded-context crates (see
// `crates/unified-db-core/src/lib.rs` for the split rules).
// This module only re-exports them so every existing
// `unified_core::db::schema::<table>` /
// `unified_core::db::models::<Struct>` path keeps working with no
// changes in the 1,600+ files that use them. Tables, columns, and
// models are unchanged; migrations still live in
// `crates/unified-core/migrations/`.

pub use unified_db_core::schema::*;
pub use unified_db_commerce::schema::*;
pub use unified_db_education::schema::*;
pub use unified_db_people::schema::*;
pub use unified_db_operations::schema::*;
pub use unified_db_platform::schema::*;
pub use unified_db_infra::schema::*;
