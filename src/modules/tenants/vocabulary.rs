//! tenants - vocabulary.

use super::{constants::*, events::*, settings::*, records::*, state::*, auth::*, helpers::*, provisioning::*, destroy::*, handlers::*, module::*, tenancy::*};



// Lifecycle vocabulary (data, not code paths).
/// Tenant lifecycle statuses. The vocabulary is data: the state
/// machine below is the only place that gives it meaning.
pub const STATUS_PROVISIONING: &str = "provisioning";

pub const STATUS_ACTIVE: &str = "active";

pub const STATUS_SUSPENDED: &str = "suspended";

pub const STATUS_DEPROVISIONING: &str = "deprovisioning";

pub const STATUS_DELETED: &str = "deleted";

pub const STATUS_PROVISION_FAILED: &str = "provision_failed";


/// Every lifecycle status, in lifecycle order.
pub const TENANT_STATUSES: &[&str] = &[
    STATUS_PROVISIONING,
    STATUS_ACTIVE,
    STATUS_SUSPENDED,
    STATUS_DEPROVISIONING,
    STATUS_DELETED,
    STATUS_PROVISION_FAILED,
];


/// Allowed transitions, as data: (from, to) pairs. Anything not
/// listed is rejected with 409. `deleted` is terminal; a failed
/// provision returns to `provisioning` through the retry endpoint,
/// never straight to `active`.
pub const TENANT_TRANSITIONS: &[(&str, &str)] = &[
    (STATUS_PROVISIONING, STATUS_ACTIVE),
    (STATUS_PROVISIONING, STATUS_PROVISION_FAILED),
    (STATUS_PROVISIONING, STATUS_DEPROVISIONING),
    (STATUS_PROVISION_FAILED, STATUS_PROVISIONING),
    (STATUS_PROVISION_FAILED, STATUS_DEPROVISIONING),
    (STATUS_ACTIVE, STATUS_SUSPENDED),
    (STATUS_ACTIVE, STATUS_DEPROVISIONING),
    (STATUS_SUSPENDED, STATUS_ACTIVE),
    (STATUS_SUSPENDED, STATUS_DEPROVISIONING),
    (STATUS_DEPROVISIONING, STATUS_DELETED),
];


/// True when the lifecycle allows moving from `from` to `to`.
pub fn transition_allowed(from: &str, to: &str) -> bool {
    TENANT_TRANSITIONS.contains(&(from, to))
}



// Provisioning pipeline vocabulary (data).
/// Pipeline steps, in execution order. A re-run resumes at the
/// first step whose recorded status is not `completed`.
pub const STEP_CREATE_RECORD: &str = "create_record";

pub const STEP_CREATE_SCHEMA: &str = "create_schema";

pub const STEP_RUN_MIGRATIONS: &str = "run_migrations";

pub const STEP_REGISTER_SUBDOMAIN: &str = "register_subdomain";

pub const STEP_GRANT_MODULES: &str = "grant_modules";

pub const STEP_ACTIVATE: &str = "activate";


/// Every provisioning step, in execution order.
pub const PROVISION_STEPS: &[&str] = &[
    STEP_CREATE_RECORD,
    STEP_CREATE_SCHEMA,
    STEP_RUN_MIGRATIONS,
    STEP_REGISTER_SUBDOMAIN,
    STEP_GRANT_MODULES,
    STEP_ACTIVATE,
];


/// Step statuses. `skipped` is a recorded, explained outcome -
///
/// never a silent drop.
pub const STEP_PENDING: &str = "pending";

pub const STEP_RUNNING: &str = "running";

pub const STEP_COMPLETED: &str = "completed";

pub const STEP_FAILED: &str = "failed";

pub const STEP_SKIPPED: &str = "skipped";
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_allowed_follows_the_lifecycle_graph() {
        assert!(transition_allowed("provisioning", "active"));
        assert!(transition_allowed("provisioning", "provision_failed"));
        assert!(transition_allowed("active", "suspended"));
        assert!(transition_allowed("suspended", "active"));
        assert!(transition_allowed("deprovisioning", "deleted"));
    }

    #[test]
    fn transition_allowed_rejects_unlisted_edges() {
        assert!(!transition_allowed("active", "deleted"));
        assert!(!transition_allowed("deleted", "active"));
        assert!(!transition_allowed("active", "provisioning"));
        assert!(!transition_allowed("", "active"));
    }
}
