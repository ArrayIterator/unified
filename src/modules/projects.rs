// Project Management module.
//
// Every project picks one methodology at creation time, and that choice
// drives its defaults: board kind, columns, statuses, and whether it
// uses sprints, phases, WIP limits, story points, Shape Up cycles, or
// SAFe program increments. Fifteen methodologies share one data model
// (projects, boards, tasks, sprints, phases, cycles, program
// increments, business cases, time entries, automation rules,
// templates), so a project can change methodology without losing its
// work, and reports can aggregate across methodologies.
//
// Interconnection happens only through the hook bus: tasks can be
// created from support tickets and converted back into tickets, and
// billable hours are exposed to the billing module through a hook.
// Nothing here calls another module directly.
//
// Pending-integration hook stubs: this module registers listeners
// for `project.task_from_ticket_requested`,
// `project.ticket_from_task_requested`, and
// `project.billable_hours_requested`, but no module in the codebase
// emits those event names today — the emitters belong to the tickets
// and billing modules, which are owned by other builders. The
// listeners stay registered so the wiring works the moment the
// emitters land; wiring them is integrator work, out of scope for
// this module's builders.

// Projects module root.
//
// The module is split into one file per concern under
// `src/modules/projects/`. The public API is unchanged: every item
// previously reachable as `crate::modules::projects::Item` is
// re-exported here.

pub mod ai_capabilities;
mod handlers_automation;
mod handlers_boards;
mod handlers_cycles;
mod handlers_import;
mod handlers_members;
mod handlers_milestones;
mod handlers_phases;
mod handlers_program;
mod handlers_projects;
mod handlers_reports;
mod handlers_sprints;
mod handlers_task_items;
mod handlers_tasks;
mod handlers_templates;
mod handlers_time;
mod handlers_views;
mod hooks;
pub mod methodologies;
pub mod module;
pub mod s2_helpers;
pub mod s2_requests;
mod s3_helpers;
pub mod s3_requests;
mod s4_helpers;
pub mod s4_requests;
mod s5_helpers;
pub mod s5_requests;
pub mod state;
pub mod types;

pub(crate) use self::handlers_automation::*;
pub(crate) use self::handlers_boards::*;
pub(crate) use self::handlers_cycles::*;
pub(crate) use self::handlers_import::*;
pub(crate) use self::handlers_members::*;
pub(crate) use self::handlers_milestones::*;
pub(crate) use self::handlers_phases::*;
pub(crate) use self::handlers_program::*;
pub(crate) use self::handlers_projects::*;
pub(crate) use self::handlers_reports::*;
pub(crate) use self::handlers_sprints::*;
pub(crate) use self::handlers_task_items::*;
pub(crate) use self::handlers_tasks::*;
pub(crate) use self::handlers_templates::*;
pub(crate) use self::handlers_time::*;
pub(crate) use self::handlers_views::*;
pub(crate) use self::hooks::*;
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::methodologies::*;
pub(crate) use self::methodologies::*;
pub use self::module::*;
pub(crate) use self::module::*;
pub use self::s2_helpers::*;
pub(crate) use self::s2_helpers::*;
pub use self::s2_requests::*;
pub(crate) use self::s2_requests::*;
pub(crate) use self::s3_helpers::*;
pub use self::s3_requests::*;
pub(crate) use self::s3_requests::*;
pub(crate) use self::s4_helpers::*;
pub use self::s4_requests::*;
pub(crate) use self::s4_requests::*;
pub(crate) use self::s5_helpers::*;
pub use self::s5_requests::*;
pub(crate) use self::s5_requests::*;
pub use self::state::*;
pub(crate) use self::state::*;
pub use self::types::*;
pub(crate) use self::types::*;
