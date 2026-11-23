//! incidents - constants.



/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_MESSAGE_LEN: usize = 5000;

pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_SUMMARY_LEN: usize = 10000;

pub(crate) const MAX_ROOT_CAUSE_LEN: usize = 10000;

pub(crate) const MAX_MEMBERS: usize = 100;

pub(crate) const MAX_STEPS: usize = 20;

pub(crate) const MAX_SERVICES: usize = 100;

pub(crate) const MAX_RESPONDERS: usize = 100;


/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_INCIDENTS_VIEW: &str = "incidents.view";

pub(crate) const PERM_INCIDENTS_MANAGE: &str = "incidents.manage";


/// Hook event names this module listens to (the literal repeated
/// here so the module stays decoupled from the Service Monitor
/// module, exactly like the events module repeats the payments
/// constants). The Service Monitor emits `cluster.health.changed`
/// with `service_id`, `service_name`, `health` (`down` on outage),
/// `status`, `detail`, and `checked_at`.
pub(crate) const SERVICE_HEALTH_CHANGED: &str = "cluster.health.changed";
