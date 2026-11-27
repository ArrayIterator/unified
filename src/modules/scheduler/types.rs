// Scheduler - Job, run and queue types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// Types

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    /// One of `KNOWN_KINDS` (see below), for example `mail.send`,
    /// `geo.update`, `ssl.renew`, `cleanup`, `backup.run`,
    /// `backup.verify`, `backup.prune`, `cluster.health`,
    /// `cluster.sync`, `cluster.sync.retry_due`,
    /// `payment.subscription.renew`, `report.generate`,
    /// `newsletter.send`, `notification.digest`,
    /// `notification.digest.send`, `meeting.start`,
    /// `announcement.publish_due`, `ticket.sla.check`,
    /// `export.run`, `webinar.start`, `webinar.reminder`,
    /// `appointments.reminders.send`, `marketing.campaign.send`,
    /// `marketing.sequence.process_due`,
    /// `field_service.plans.check`, `calendar.feeds.prune`,
    /// `holidays.sync`, `holidays.sync_all`,
    /// `tenant.destroy_due`, `approvals.sweep_expiry`,
    /// `license.expiry.check`, `auditors.expire`,
    /// `logistics.maintenance.remind`, `web3.poll`,
    /// `timetracker.screenshots.purge`, `storage.lifecycle.run`,
    /// `storage.byo.health_check`, `events.reminders.send`,
    /// `todos.reminders.send`, `saas_domains.reservations.expire`,
    /// `ssl.cloudflare.recheck`, `advisor.evaluate`,
    /// `billing.subscriptions.scan`,
    /// `saas.product_subdomain.expire_grace`, `workflows.run`,
    /// `contracts.expiry_alert`, `assets.warranty.check`,
    /// `inbox.unsnooze`.
    pub kind: String,
    /// Interval in seconds (`"300"`) or a cron-like expression
    /// (`"*/5 * * * *"`).
    pub schedule: String,
    pub payload: Value,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateJob {
    pub name: String,
    pub kind: String,
    pub schedule: String,
    pub payload: Option<Value>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateJob {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub schedule: Option<String>,
    pub payload: Option<Value>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRun {
    pub id: String,
    pub job_id: String,
    pub kind: String,
    /// `success` or `failed`.
    pub status: String,
    pub result: Value,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: String,
    pub kind: String,
    pub payload: Value,
    /// `queued`, `processing`, `done`, `failed`.
    pub status: String,
    pub attempts: u32,
    pub max_attempts: u32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnqueueRequest {
    pub kind: String,
    pub payload: Option<Value>,
}

/// Known job kinds. Anything else is rejected with 422 when it is run.
///
/// Each kind has a dispatch arm in `dispatch` below. Most arms emit
/// the `<kind>_requested` event the owning module listens to; a few
/// kinds name the event directly (the owning module registered a
/// handler for the kind name itself), and those arms emit the kind
/// name unchanged - see the arm comments.
pub(crate) const KNOWN_KINDS: &[&str] = &[
    "mail.send",
    "geo.update",
    "ssl.renew",
    "cleanup",
    "backup.run",
    "backup.verify",
    "backup.prune",
    "cluster.health",
    "cluster.sync",
    "cluster.sync.retry_due",
    "payment.subscription.renew",
    "report.generate",
    "newsletter.send",
    "notification.digest",
    "notification.digest.send",
    "meeting.start",
    "announcement.publish_due",
    "ticket.sla.check",
    "export.run",
    "webinar.start",
    "webinar.reminder",
    "appointments.reminders.send",
    "marketing.campaign.send",
    "marketing.sequence.process_due",
    "field_service.plans.check",
    "calendar.feeds.prune",
    "holidays.sync",
    "holidays.sync_all",
    "tenant.destroy_due",
    "approvals.sweep_expiry",
    "license.expiry.check",
    "auditors.expire",
    "logistics.maintenance.remind",
    "web3.poll",
    "timetracker.screenshots.purge",
    "storage.lifecycle.run",
    "storage.byo.health_check",
    "events.reminders.send",
    "todos.reminders.send",
    "saas_domains.reservations.expire",
    "ssl.cloudflare.recheck",
    "advisor.evaluate",
    "billing.subscriptions.scan",
    "saas.product_subdomain.expire_grace",
    "workflows.run",
    "contracts.expiry_alert",
    "assets.warranty.check",
    "inbox.unsnooze",
    "domain.verify.retry",
];
