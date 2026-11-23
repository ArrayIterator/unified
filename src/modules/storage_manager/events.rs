// Hook event names.

pub const BUCKET_CREATED: &str = "storage.bucket.created";
pub const BUCKET_UPDATED: &str = "storage.bucket.updated";
pub const BUCKET_DELETED: &str = "storage.bucket.deleted";
pub const BINDING_CHANGED: &str = "storage.binding.changed";
pub const UPLOAD_REQUESTED: &str = "storage.object.upload_requested";
pub const OBJECT_STORED: &str = "storage.object.stored";
pub const OBJECT_DELETED: &str = "storage.object.deleted";
pub const OBJECT_ARCHIVED: &str = "storage.object.archived";
pub const QUOTA_EXCEEDED: &str = "storage.quota.exceeded";
pub const QUOTA_WARNING: &str = "storage.quota.warning";
pub const HEALTH_FAILED: &str = "storage.health.failed";
pub const HEALTH_OK: &str = "storage.health.ok";
pub const SIGNED_URL_ISSUED: &str = "storage.signed_url.issued";
pub const SIGNED_URL_REVOKED: &str = "storage.signed_url.revoked";
pub const LIFECYCLE_RUN_COMPLETED: &str = "storage.lifecycle.run.completed";
pub const LIFECYCLE_RUN_REQUESTED: &str = "storage.lifecycle.run_requested";
pub const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";
pub const PLAN_CREATED: &str = "storage.plan.created";
pub const PLAN_UPDATED: &str = "storage.plan.updated";
pub const PLAN_DELETED: &str = "storage.plan.deleted";
pub const PLAN_ASSIGNED: &str = "storage.plan.assigned";
pub const PLAN_CHANGED: &str = "storage.plan.changed";
pub const PLAN_CANCELLED: &str = "storage.plan.cancelled";
pub const PLAN_QUOTA_REFUSED: &str = "storage.plan.quota_refused";
pub const METERING_REPORTED: &str = "storage.metering.reported";
/// Emitted when a download or verification finds bytes whose
/// SHA-256 does not match the recorded checksum. The corrupt
/// bytes are never served.
pub const OBJECT_CHECKSUM_MISMATCH: &str = "storage.object.checksum_mismatch";
pub const BYO_CREATED: &str = "storage.byo.created";
pub const BYO_UPDATED: &str = "storage.byo.updated";
pub const BYO_VALIDATED: &str = "storage.byo.validated";
pub const BYO_VALIDATION_FAILED: &str = "storage.byo.validation_failed";
pub const BYO_ACTIVATED: &str = "storage.byo.activated";
pub const BYO_SUSPENDED: &str = "storage.byo.suspended";
pub const BYO_REVOKED: &str = "storage.byo.revoked";
pub const BYO_TERMS_ACCEPTED: &str = "storage.byo.terms_accepted";
pub const BYO_TERMS_CHANGED: &str = "storage.byo.terms_changed";
pub const BYO_HEALTH_FAILED: &str = "storage.byo.health_failed";
pub const BYO_HEALTH_OK: &str = "storage.byo.health_ok";
pub const BYO_HEALTH_CHECK_REQUESTED: &str = "storage.byo.health_check_requested";
pub const BYO_QUOTA_EXCEEDED: &str = "storage.byo.quota_exceeded";
