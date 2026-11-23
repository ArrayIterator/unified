// Support Tickets module.
//
// Tickets with comments, assignment, validated status transitions, and
// knowledgebase suggestions. Tickets can also be created through the
// `ticket.create_requested` hook (Chat / AI Support). Interconnection
// happens only through the hook bus.
//
// Extended behaviour (all additive; every original route keeps working):
//   - Departments: a ticket carries `department_id`, is routed to a
//     department by the Departments module through the
//     `ticket.route_requested` hook, and list/detail responses can be
//     filtered by department. Agent scoping in the list handler uses
//     the `department.membership_check` hook plus the `role.check`
//     hook (`tickets:view_all` capability).
//   - SLA timers: first-response and resolution due dates are computed
//     at creation from the department SLA policy
//     (`department.sla_requested`) or from priority-based defaults.
//     `POST /api/v1/tickets/sla/check` (and the
//     `ticket.sla.check_requested` hook) scans for breaches, emits
//     `ticket.sla.breached`, notifies the assignee or department
//     agents through `notification.requested`, and applies escalation
//     rules.
//   - Replies vs internal notes: customer/agent replies live in the
//     public thread (`comments`); staff-only notes live in
//     `internal_notes` and are stripped from the public view returned
//     to customers.
//   - Attachments reference objects already uploaded through the
//     Storage module (`POST /api/v1/storage/targets/{name}/objects`);
//     this module stores only the target/key reference, never bytes.
//   - Canned responses, escalation rules, ticket merging, and links
//     to orders, licenses, and servers (stored as id + type; a license
//     link is validated through the existing
//     `license.validate_requested` hook, sent without an instance id
//     so linking never consumes an activation seat; no lookup hook
//     exists for orders or servers yet, so those links are stored
//     unverified).

// Tickets module root.
//
// The module is split into one file per concern under
// `src/modules/tickets/`. The public API is unchanged: every item
// previously reachable as `crate::modules::tickets::Item` is
// re-exported here.

pub mod ai_capabilities;
pub mod types;
mod validation;
mod handlers;
mod canned;
mod escalation;
mod hooks;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::*;
pub use self::module::*;
pub(crate) use self::types::*;
pub(crate) use self::validation::*;
pub(crate) use self::handlers::*;
pub(crate) use self::canned::*;
pub(crate) use self::escalation::*;
pub(crate) use self::hooks::*;
pub(crate) use self::module::*;
