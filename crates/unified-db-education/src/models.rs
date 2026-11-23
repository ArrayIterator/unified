// Diesel models for the Unified education bounded context.
//
// Learning: academic records, e-learning, curriculum, admissions, exams, school fees, and library.
//
// Split out of `unified-core/src/db/models.rs`; the structs below
// are byte-identical to the originals, only regrouped. See
// `crates/unified-db-education/src/lib.rs` for the split rules.
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

pub mod academic;
pub mod academic_records;
pub mod admissions;
pub mod curriculum;
pub mod elearning;
pub mod exams;
pub mod library;
pub mod school_fees;

pub use academic::*;
pub use academic_records::*;
pub use admissions::*;
pub use curriculum::*;
pub use elearning::*;
pub use exams::*;
pub use library::*;
pub use school_fees::*;
