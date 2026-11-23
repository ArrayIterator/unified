// Diesel models for the Unified people bounded context.
//
// People: HR, recruiting, performance, supervision, holidays, todos, and appointments.
//
// Split out of `unified-core/src/db/models.rs`; the structs below
// are byte-identical to the originals, only regrouped. See
// `crates/unified-db-people/src/lib.rs` for the split rules.
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

pub mod appointments;
pub mod holidays;
pub mod hr;
pub mod hr_payroll;
pub mod okr;
pub mod recruitment;
pub mod supervision;
pub mod todos;

pub use appointments::*;
pub use holidays::*;
pub use hr::*;
pub use hr_payroll::*;
pub use okr::*;
pub use recruitment::*;
pub use supervision::*;
pub use todos::*;
