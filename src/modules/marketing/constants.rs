// Marketing Automation — Event names, permission strings, limits, and other constants.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::service_sequences::*;
use super::handlers_campaigns::*;
use super::handlers_segments::*;
use super::handlers_sequences::*;
use super::handlers_scores::*;
use super::handlers_audience::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

pub const EVENT_MAIL_SEND_REQUESTED: &str = "mail.send_requested";

pub const EVENT_MAIL_SENT: &str = "mail.sent";

pub const EVENT_MAIL_FAILED: &str = "mail.failed";

pub const EVENT_SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";

pub const EVENT_NEWSLETTER_OPEN: &str = "newsletter.open";

pub const EVENT_NEWSLETTER_CLICK: &str = "newsletter.click";

pub const EVENT_NEWSLETTER_UNSUBSCRIBED: &str = "newsletter.unsubscribed";

pub const EVENT_COMPLIANCE_CONSENT_GRANTED: &str = "compliance.consent.granted";

pub const EVENT_COMPLIANCE_CONSENT_WITHDRAWN: &str = "compliance.consent.withdrawn";

pub const EVENT_COMPLIANCE_CONSENT_RECORDED: &str = "compliance.consent.recorded";

pub const EVENT_COMPLIANCE_CONSENT_CHECK: &str = "compliance.consent.check";

pub const EVENT_PRIVACY_CONSENT_CHANGED: &str = "privacy.consent.changed";

pub const EVENT_FORMS_SUBMISSION_RECEIVED: &str = "forms.submission.received";

pub const EVENT_STORE_ORDER_COMPLETED: &str = "store.order.completed";

pub const EVENT_ROLE_CHECK: &str = "role.check";

pub const EVENT_MARKETING_CAMPAIGN_CREATED: &str = "marketing.campaign.created";

pub const EVENT_MARKETING_CAMPAIGN_SENT: &str = "marketing.campaign.sent";

pub const EVENT_MARKETING_CAMPAIGN_AB_WINNER: &str = "marketing.campaign.ab.winner";

pub const EVENT_MARKETING_CAMPAIGN_SEND: &str = "marketing.campaign.send";

pub const EVENT_MARKETING_SEGMENT_EVALUATED: &str = "marketing.segment.evaluated";

pub const EVENT_MARKETING_SEQUENCE_ENROLLED: &str = "marketing.sequence.enrolled";

pub const EVENT_MARKETING_SEQUENCE_STEP_SENT: &str = "marketing.sequence.step.sent";

pub const EVENT_MARKETING_SEQUENCE_COMPLETED: &str = "marketing.sequence.completed";

pub const EVENT_MARKETING_SEQUENCE_PROCESS_DUE: &str = "marketing.sequence.process_due";

pub const EVENT_MARKETING_SEQUENCE_ENROLL: &str = "marketing.sequence.enroll";

pub const EVENT_MARKETING_SCORE_UPDATED: &str = "marketing.score.updated";

pub const EVENT_MARKETING_LEAD_HOT: &str = "marketing.lead.hot";

pub const EVENT_MARKETING_SCORE_RECORD: &str = "marketing.score.record";

pub const EVENT_MARKETING_SEND_SKIPPED: &str = "marketing.send.skipped";

pub const EVENT_MARKETING_UNSUBSCRIBED: &str = "marketing.unsubscribed";

pub const EVENT_MARKETING_UNSUBSCRIBE: &str = "marketing.unsubscribe";

pub const EVENT_MARKETING_CONTACTS_SYNC: &str = "marketing.contacts.sync";

// ---------------------------------------------------------------------------
// Permissions.
// ---------------------------------------------------------------------------

pub(crate) const PERM_MARKETING_VIEW: &str = "marketing.view";

pub(crate) const PERM_MARKETING_MANAGE: &str = "marketing.manage";

pub(crate) const PERM_MARKETING_SEND: &str = "marketing.send";

/// Maximum nesting depth of a segment rule tree (prevents hostile
/// payloads from recursing without bound).
pub(crate) const MAX_RULE_DEPTH: usize = 10;

/// Maximum number of leaf rules in one segment rule tree.
pub(crate) const MAX_RULE_LEAVES: usize = 100;

/// Maximum score events kept per contact (oldest are dropped).
pub(crate) const MAX_SCORE_EVENTS: usize = 100;

/// Maximum size-history snapshots kept per segment.
pub(crate) const MAX_SEGMENT_HISTORY: usize = 100;

/// Maximum recipients processed in one campaign send run. A larger
/// audience must be sent in further runs; this cap keeps one request
/// from blocking the server.
pub(crate) const DEFAULT_MAX_SENDS_PER_RUN: u64 = 1000;
