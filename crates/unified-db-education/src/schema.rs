// Diesel schema for the Unified education bounded context.
//
// Learning: academic records, e-learning, curriculum, admissions, exams, school fees, and library.
//
// Split out of `unified-core/src/db/schema.rs`; see
// `crates/unified-db-education/src/lib.rs` for the split rules.
// Money is NUMERIC(19,4) everywhere (Diesel `Numeric`, Rust
// `diesel::data_types::PgNumeric`); see docs/SCHEMA.md.
//
// Split into `schema/<group>.rs` (one file per domain group) plus
// `schema/joins.rs` for the join declarations; this file only declares
// the submodules and re-exports their tables, so every
// `crate::schema::*` path is unchanged.

pub mod academic;
pub mod academic_records;
pub mod admissions;
pub mod curriculum;
pub mod elearning;
pub mod exams;
pub mod joins;
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
