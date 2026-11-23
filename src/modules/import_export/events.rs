//! import export - events.

/// Well-known events emitted by this module.
pub mod import_export_events {
    pub const IMPORT_CREATED: &str = "import.created";
    pub const IMPORT_COMPLETED: &str = "import.completed";
    pub const IMPORT_CONFIRMED: &str = "import.confirmed";
    pub const IMPORT_FAILED: &str = "import.failed";
    pub const IMPORT_SESSION_COMPLETED: &str = "import.session.completed";
    pub const EXPORT_CREATED: &str = "export.created";
    pub const EXPORT_COMPLETED: &str = "export.completed";
    pub const EXPORT_REQUESTED: &str = "export.requested";
    pub const EXPORT_LOOKUP: &str = "export.lookup";
    pub const EXPORT_PII_REDACTED: &str = "export.pii_redacted";
    pub const EXPORT_RUN_REQUESTED: &str = "export.run_requested";
    pub const EXPORT_SCHEDULE_CREATED: &str = "export.schedule.created";
    pub const EXPORT_SCHEDULE_UPDATED: &str = "export.schedule.updated";
    pub const EXPORT_SCHEDULE_DELETED: &str = "export.schedule.deleted";
    pub const EXPORT_SCHEDULE_RUN: &str = "export.schedule.run";
    pub const MAPPING_CREATED: &str = "mapping.created";
    pub const MAPPING_DELETED: &str = "mapping.deleted";
    pub const SETTINGS_UPDATED: &str = "import_export.settings.updated";
}

/// Scheduler integration: the kind offered to `scheduler.enqueue`
/// and the event the scheduler emits when it dispatches the run.
pub const EXPORT_RUN_KIND: &str = "export.run";

pub const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";

/// Capabilities enforced by this module (checked through the
/// `role.check` hook; the `admin` role holds `*` and passes all).
pub const PERM_IMPORT_EXPORT_READ: &str = "import_export:read";

pub const PERM_IMPORT_EXPORT_IMPORT: &str = "import_export:import";

pub const PERM_IMPORT_EXPORT_EXPORT: &str = "import_export:export";

pub const PERM_IMPORT_EXPORT_EXPORT_PII: &str = "import_export:export_pii";

pub const PERM_IMPORT_EXPORT_MANAGE: &str = "import_export:manage";
