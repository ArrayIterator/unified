// Academic System module.
//
// Students, lecturers, study programs, academic subjects, class
// schedules and rooms, course registration with advisor approval,
// attendance, grades and GPA, grade appeals, official transcripts,
// academic calendar, thesis / final-project tracking, and academic
// standing. Interconnection happens only through the hook bus; no
// module calls another module directly.
//
// Privacy rule (enforced server-side on every handler): a student
// sees only their own academic data; a lecturer sees only the
// classes they teach. Records a caller may not read are answered
// with 404, so ids cannot be probed.
//
// Capabilities (`academic:read`, `academic:read_all`,
// `academic:manage`, `academic:advise`, `academic:grade`,
// `academic:transcript`) are checked through the `role.check` hook
// and fail closed. Status vocabularies, grade scales, standing
// rules and state-machine transitions live in the admin-editable
// definitions table; the constants in `constants.rs` are only
// fallbacks used when no definition exists yet.
//
// This module is split by concern under `src/modules/academic/`;
// the full module notes (events emitted and consumed, identity,
// e-learning and printing rules) are preserved at the top of
// `academic/module.rs`.

pub mod ai;
pub mod ai_capabilities;
pub mod constants;
mod handlers_assessment;
mod handlers_catalog;
mod handlers_enrollment;
mod handlers_scheduling;
mod handlers_thesis;
mod helpers;
mod hooks;
pub mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::{
    PERM_ACADEMIC_ADVISE, PERM_ACADEMIC_GRADE, PERM_ACADEMIC_MANAGE, PERM_ACADEMIC_READ,
    PERM_ACADEMIC_READ_ALL, PERM_ACADEMIC_TRANSCRIPT,
};
pub use self::module::*;
pub use self::state::*;
pub use self::types::*;
