// Diesel schema for the Unified platform bounded context.
//
// Engagement platform: email, newsletters, events, webinars, community, forms, surveys, documents, meetings, contracts, and analytics.
//
// Split out of `unified-core/src/db/schema.rs`; see
// `crates/unified-db-platform/src/lib.rs` for the split rules.
// Money is NUMERIC(19,4) everywhere (Diesel `Numeric`, Rust
// `diesel::data_types::PgNumeric`); see docs/SCHEMA.md.
//
// Split into `schema/<group>.rs` (one file per domain group) plus
// `schema/joins.rs` for the join declarations; this file only declares
// the submodules and re-exports their tables, so every
// `crate::schema::*` path is unchanged.

// NOTE (name-collision rule): a `schema/<group>.rs` file must NEVER be
// declared under the name of a table it defines. The file's
// `diesel::table!` block generates the table module at
// `crate::schema::<group>::<table>`; an explicit `pub mod <group>;`
// binding wins over the `pub use <group>::*` glob re-export of the same
// name, so `crate::schema::<table>` then resolves to the file module
// (which has no columns) instead of the table, and every
// `#[diesel(table_name = <table>)]` fails with E0425 (and the
// `allow_tables_to_appear_in_same_query!` groups emit bogus
// `TableNotEqual` impls, i.e. E0119). The colliding groups below are
// therefore declared as `<group>_tables` (same file, via `#[path]`,
// which resolves relative to the crate root `src/`, not this file);
// every `crate::schema::<table>` path is unchanged.
pub mod analytics;
#[path = "schema/announcements.rs"]
pub mod announcements_tables;
pub mod approvals;
pub mod bi_portals;
pub mod calendar;
pub mod community;
#[path = "schema/contracts.rs"]
pub mod contracts_tables;
#[path = "schema/documents.rs"]
pub mod documents_tables;
pub mod email;
pub mod esg;
#[path = "schema/events.rs"]
pub mod events_tables;
#[path = "schema/forms.rs"]
pub mod forms_tables;
#[path = "schema/forums.rs"]
pub mod forums_tables;
pub mod inbox;
pub mod joins;
pub mod meetings;
#[path = "schema/notifications.rs"]
pub mod notifications_tables;
#[path = "schema/presets.rs"]
pub mod presets_tables;
pub mod quality;
#[path = "schema/saved_views.rs"]
pub mod saved_views_tables;
pub mod studio;
#[path = "schema/surveys.rs"]
pub mod surveys_tables;
#[path = "schema/webinars.rs"]
pub mod webinars_tables;
#[path = "schema/workflows.rs"]
pub mod workflows_tables;
pub mod widgets;

pub use analytics::*;
pub use announcements_tables::*;
pub use approvals::*;
pub use bi_portals::*;
pub use calendar::*;
pub use community::*;
pub use contracts_tables::*;
pub use documents_tables::*;
pub use email::*;
pub use esg::*;
pub use events_tables::*;
pub use forms_tables::*;
pub use forums_tables::*;
pub use inbox::*;
pub use meetings::*;
pub use notifications_tables::*;
pub use presets_tables::*;
pub use quality::*;
pub use saved_views_tables::*;
pub use studio::*;
pub use surveys_tables::*;
pub use webinars_tables::*;
pub use workflows_tables::*;
pub use widgets::*;
