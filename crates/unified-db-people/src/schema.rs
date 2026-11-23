// Diesel schema for the Unified people bounded context.
//
// People: HR, recruiting, performance, supervision, holidays, todos, and appointments.
//
// Split out of `unified-core/src/db/schema.rs`; see
// `crates/unified-db-people/src/lib.rs` for the split rules.
// Money is NUMERIC(19,4) everywhere (Diesel `Numeric`, Rust
// `diesel::data_types::PgNumeric`); see docs/SCHEMA.md.
//
// Split into `schema/<group>.rs` (one file per domain group) plus
// `schema/joins.rs` for the join declarations; this file only declares
// the submodules and re-exports their tables, so every
// `crate::schema::*` path is unchanged.
//
// NOTE: never name a `schema/<group>.rs` file module after a table it
// defines. An explicit `pub mod <name>;` binding wins over the glob
// re-export `pub use <name>::*;` of the same name, so `crate::schema::<name>`
// resolves to the file module (which has no column types) instead of the
// table, and every `#[diesel(table_name = <name>)]` fails with E0425
// ("cannot find type `id` in module `<name>`"). The three colliding file
// modules below are therefore declared under `*_tables` names (same files,
// via `#[path = "schema/<group>.rs"]`; note `#[path]` resolves relative to
// the crate root, not the declaring file) with matching re-exports, so every
// `crate::schema::<table>` path is unchanged.

#[path = "schema/appointments.rs"]
pub mod appointments_tables;
#[path = "schema/holidays.rs"]
pub mod holidays_tables;
pub mod hr;
pub mod hr_payroll;
pub mod joins;
pub mod okr;
pub mod recruitment;
pub mod supervision;
#[path = "schema/todos.rs"]
pub mod todos_tables;

pub use appointments_tables::*;
pub use holidays_tables::*;
pub use hr::*;
pub use hr_payroll::*;
pub use okr::*;
pub use recruitment::*;
pub use supervision::*;
pub use todos_tables::*;
