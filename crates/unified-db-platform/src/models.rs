// Diesel models for the Unified platform bounded context.
//
// Engagement platform: email, newsletters, events, webinars, community, forms, surveys, documents, meetings, contracts, and analytics.
//
// Split out of `unified-core/src/db/models.rs`; the structs below
// are byte-identical to the originals, only regrouped. See
// `crates/unified-db-platform/src/lib.rs` for the split rules.
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

pub mod analytics;
pub mod announcements;
pub mod approvals;
pub mod bi_portals;
pub mod calendar;
pub mod community;
pub mod contracts;
pub mod documents;
pub mod email;
pub mod events;
pub mod forms;
pub mod inbox;
pub mod meetings;
pub mod notifications;
pub mod presets;
pub mod quality;
pub mod saved_views;
pub mod studio;
pub mod surveys;
pub mod webinars;
pub mod workflows;

pub use analytics::*;
pub use announcements::*;
pub use approvals::*;
pub use bi_portals::*;
pub use calendar::*;
pub use community::*;
pub use contracts::*;
pub use documents::*;
pub use email::*;
pub use events::*;
pub use forms::*;
pub use inbox::*;
pub use meetings::*;
pub use notifications::*;
pub use presets::*;
pub use quality::*;
pub use saved_views::*;
pub use studio::*;
pub use surveys::*;
pub use webinars::*;
pub use workflows::*;
