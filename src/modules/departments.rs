// Departments module (CORE).
//
// Departments own support agents, ticket routing rules, per-department
// SLA policies, working hours, and auto-replies. Interconnection
// happens only through the hook bus - no module calls another module
// directly.
//
// What this module answers for others:
//   - `ticket.route_requested` - which department receives a ticket,
//     by category, product, or keyword rule, falling back to the
//     default department.
//   - `department.sla_requested` - a department's first-response and
//     resolution targets in minutes.
//   - `department.membership_check` - which departments a user agents
//     for (used by the tickets list handler for role-based access).
//   - `department.agents_requested` - the agent user ids of a
//     department (used for SLA breach and escalation alerts).
//   - `ticket.created` - sends the department auto-reply to the
//     ticket owner when the ticket arrives outside working hours.
//
// Agents are stored as user ids only. Names are resolved by callers
// through the Users module (`GET /api/v1/users/{id}`); no user lookup
// hook exists, so none is invented here. An agent's role, when one is
// supplied, is validated through the existing `role.check` hook.

pub mod ai_capabilities;
pub mod types;
pub mod state;
mod validation;
mod helpers;
mod handlers_departments;
mod handlers_routing;
mod hooks;
mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::WorkingHours;
pub use self::types::AutoReply;
pub use self::types::Department;
pub use self::types::CreateDepartment;
pub use self::types::UpdateDepartment;
pub use self::types::AddAgent;
pub use self::types::RoutingRule;
pub use self::types::CreateRoutingRule;
pub use self::types::UpdateRoutingRule;
pub use self::types::RoutePreview;
pub use self::state::DepartmentsState;
pub use self::module::DepartmentsModule;
pub use self::module::register;
