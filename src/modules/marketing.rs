// Marketing Automation module.
//
// Campaigns, dynamic segments, drip sequences, lead scoring, and
// send analytics, in the style of a full marketing-automation suite.
// This module never sends mail itself and never stores its own copy
// of CRM contacts or newsletter subscribers: sending goes through
// the Mail module (`mail.send_requested`), tracking and unsubscribe
// signals are consumed from the Mail and Newsletter modules, and
// contacts live in CRM. What this module owns is the automation
// around those facts: who should receive what, when, in which
// variant, and whether they consented to it.
//
// Routes (all under `/api/v1/marketing`, every route requires the
// `X-User-Id` and `X-Role` headers and a capability check through
// the `role.check` hook, failing closed):
//   Campaigns:
//     GET    /campaigns                   - paginated campaigns
//     POST   /campaigns                   - create a campaign
//     GET    /campaigns/{id}              - one campaign
//     PUT    /campaigns/{id}              - update a draft campaign
//     DELETE /campaigns/{id}              - delete a draft campaign
//     POST   /campaigns/{id}/schedule     - schedule a campaign
//     POST   /campaigns/{id}/send-now     - send a campaign immediately
//     POST   /campaigns/{id}/pause        - pause a scheduled/sending campaign
//     POST   /campaigns/{id}/resume       - resume a paused campaign
//     GET    /campaigns/{id}/stats        - campaign stats with rates
//     GET    /campaigns/{id}/variants     - list A/B variants
//     POST   /campaigns/{id}/variants     - create an A/B variant
//     PUT    /campaigns/{id}/variants/{variant_id} - update a variant
//     DELETE /campaigns/{id}/variants/{variant_id} - delete a variant
//     POST   /campaigns/{id}/ab-decide    - pick the A/B winner from real data
//   Segments:
//     GET    /segments                    - paginated segments
//     POST   /segments                    - create a segment
//     GET    /segments/{id}               - one segment
//     PUT    /segments/{id}               - update a segment
//     DELETE /segments/{id}               - delete a segment
//     POST   /segments/{id}/evaluate      - evaluate membership, record size history
//     GET    /segments/{id}/members       - paginated current members
//     GET    /segments/{id}/history       - paginated size history
//   Sequences (drips):
//     GET    /sequences                   - paginated sequences
//     POST   /sequences                   - create a sequence
//     GET    /sequences/{id}              - one sequence
//     PUT    /sequences/{id}              - update a sequence
//     DELETE /sequences/{id}              - delete a sequence
//     GET    /sequences/{id}/steps        - list steps in order
//     POST   /sequences/{id}/steps        - add a step
//     PUT    /sequences/{id}/steps/{step_id} - update a step
//     DELETE /sequences/{id}/steps/{step_id} - delete a step
//     POST   /sequences/{id}/enroll       - enroll one contact
//     GET    /sequences/{id}/enrollments  - paginated enrollments
//     GET    /sequences/{id}/funnel       - per-step funnel
//     POST   /sequences/process-due       - run all due steps now
//     POST   /enrollments/{id}/advance    - run the next due step for one enrollment
//   Scoring:
//     GET    /scores                      - paginated contact scores
//     GET    /scores/{contact_id}         - one contact score (decay applied)
//     POST   /scores/{contact_id}/record  - record an action, apply matching rules
//     GET    /score-rules                 - paginated scoring rules
//     POST   /score-rules                 - create a scoring rule
//     PUT    /score-rules/{id}            - update a scoring rule
//     DELETE /score-rules/{id}            - delete a scoring rule
//     POST   /scores/apply-decay          - recompute all scores with decay
//   Audience, consent, suppression, audit, settings:
//     POST   /audience/sync               - upsert contact snapshots (from CRM / sync jobs)
//     GET    /audience                    - paginated known audience contacts
//     POST   /unsubscribe                 - record a global or per-campaign unsubscribe
//     GET    /suppressions                - paginated suppression list
//     DELETE /suppressions/{id}           - remove a suppression record
//     GET    /sends                       - paginated send audit trail
//     GET    /sends/{id}                  - one send record
//     GET    /settings                    - marketing settings (configurable defaults)
//     PUT    /settings                    - update marketing settings
//
// Hook contracts:
//   Emits `mail.send_requested` for every accepted recipient (the
//   Mail module owns delivery; the returned job id is stored on the
//   send record so `mail.sent` / `mail.failed` can update it).
//   Emits `scheduler.enqueue` for scheduled campaigns and due
//   sequence steps; the scheduler calls back through
//   `marketing.campaign.send` and `marketing.sequence.process_due`.
//   Emits `marketing.campaign.created`, `marketing.campaign.sent`,
//   `marketing.campaign.ab.winner`, `marketing.segment.evaluated`,
//   `marketing.sequence.enrolled`, `marketing.sequence.step.sent`,
//   `marketing.sequence.completed`, `marketing.score.updated`,
//   `marketing.lead.hot` (hot-lead handoff: a contact whose score
//   crossed the configured hot threshold; CRM consumes it),
//   `marketing.send.skipped`, `marketing.unsubscribed`.
//   Listens to `mail.sent` / `mail.failed` (delivery outcomes),
//   `newsletter.open` / `newsletter.click` (tracking events, only
//   when they reference a marketing campaign id),
//   `newsletter.unsubscribed` (suppression),
//   `compliance.consent.recorded` (the name the compliance
//   module actually emits, carrying the decision in `granted`),
//   `compliance.consent.granted` / `compliance.consent.withdrawn`
//   and `privacy.consent.changed` (consent cache),
//   `forms.submission.received` and `store.order.completed`
//   (sequence enrollment triggers and scoring actions; see the
//   note on `hook_sequence_trigger`: the forms event carries no
//   submitter identity, and the store order carries a user id but
//   no email, so both resolve the contact through the known
//   audience and otherwise do nothing),
//   `marketing.campaign.send`, `marketing.sequence.process_due`,
//   `marketing.sequence.enroll`, `marketing.contacts.sync`,
//   `marketing.unsubscribe`, `marketing.score.record`.
//
// Notes on honesty:
//   - Records are kept in memory (same pattern as the other modules
//     in this tree); the migration `2026-11-22-000076_marketing` is
//     the persistence target and the in-memory stores stay
//     authoritative until it is executed and verified.
//   - Open/click tracking is reported only from tracking events
//     actually received on the hook bus. Campaigns sent through the
//     Mail module carry no tracking pixel (the Mail module sends
//     the body as given), so their open/click counts stay zero
//     until a tracking source reports them; nothing is estimated
//     or fabricated. A/B winner selection refuses to decide
//     (`422`) until every variant reached the configured minimum
//     sample of sends and at least one tracking event exists for
//     the chosen metric.
//   - Consent is fail-closed. Before every send the module checks
//     (1) the local suppression list, (2) the consent cache built
//     from compliance/privacy events, and (3) the
//     `compliance.consent.check` event. A contact with no granted
//     consent on record, or for whom no handler answers the check,
//     is skipped and the skip is written to the send audit trail.
//     The compliance module does not answer
//     `compliance.consent.check` yet; until it does, consent must
//     arrive through the consent events or the audience sync
//     payload (`consented: true`), and unverified contacts are
//     skipped rather than mailed.
//   - Segments evaluate only against the audience snapshots this
//     module knows (synced through `POST /audience/sync` or the
//     `marketing.contacts.sync` event, plus contacts seen through
//     enrollments and form/purchase events). Every evaluation
//     reports how many contacts were actually evaluated, so a
//     segment size is never presented as full CRM coverage. The
//     CRM module currently answers no contact-list event; when it
//     gains one, evaluation should ask it first.
//   - All behaviour lists are data-driven: segment fields and
//     operators, scoring actions, split rules, thresholds, and
//     decay are admin-editable settings, not code constants. The
//     values shipped in `MarketingSettings::default` are
//     configurable defaults, clearly marked as such.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

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

const PERM_MARKETING_VIEW: &str = "marketing.view";
const PERM_MARKETING_MANAGE: &str = "marketing.manage";
const PERM_MARKETING_SEND: &str = "marketing.send";

/// Maximum nesting depth of a segment rule tree (prevents hostile
/// payloads from recursing without bound).
const MAX_RULE_DEPTH: usize = 10;
/// Maximum number of leaf rules in one segment rule tree.
const MAX_RULE_LEAVES: usize = 100;
/// Maximum score events kept per contact (oldest are dropped).
const MAX_SCORE_EVENTS: usize = 100;
/// Maximum size-history snapshots kept per segment.
const MAX_SEGMENT_HISTORY: usize = 100;
/// Maximum recipients processed in one campaign send run. A larger
/// audience must be sent in further runs; this cap keeps one request
/// from blocking the server.
const DEFAULT_MAX_SENDS_PER_RUN: u64 = 1000;

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CampaignStats {
    pub queued: u64,
    pub sent: u64,
    pub delivered: u64,
    pub failed: u64,
    pub skipped: u64,
    pub opened: u64,
    pub clicked: u64,
    pub unsubscribed: u64,
    pub unique_opened: u64,
    pub unique_clicked: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignStatsView {
    #[serde(flatten)]
    pub stats: CampaignStats,
    pub open_rate: f64,
    pub click_rate: f64,
    pub delivery_rate: f64,
    pub skip_rate: f64,
}

impl CampaignStats {
    fn view(&self) -> CampaignStatsView {
        let rate = |count: u64, base: u64| {
            if base == 0 {
                0.0
            } else {
                (count as f64 / base as f64 * 10000.0).round() / 100.0
            }
        };
        let attempted = self.sent + self.delivered + self.failed;
        CampaignStatsView {
            stats: self.clone(),
            open_rate: rate(self.unique_opened, self.sent),
            click_rate: rate(self.unique_clicked, self.sent),
            delivery_rate: rate(self.delivered, attempted),
            skip_rate: rate(self.skipped, self.queued + self.skipped),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketingCampaign {
    pub id: String,
    pub name: String,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newsletter_list_id: Option<String>,
    /// `draft`, `scheduled`, `sending`, `sent`, `paused`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
    pub ab_enabled: bool,
    /// Winner metric: `open_rate` or `click_rate`.
    pub ab_winner_metric: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ab_min_sample: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winning_variant_id: Option<String>,
    pub stats: CampaignStats,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignVariant {
    pub id: String,
    pub campaign_id: String,
    pub name: String,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_html: Option<String>,
    /// Share of recipients (0-100). The campaign base content takes
    /// the remainder (100 minus the sum of variant weights).
    pub weight_percent: u8,
    pub sent: u64,
    pub opened: u64,
    pub clicked: u64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Rule tree, stored as data: a group
    /// `{"operator": "and"|"or", "rules": [...]}` or a leaf
    /// `{"field": ..., "op": ..., "value": ...}`.
    pub rule_tree: Value,
    /// `active` or `archived`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_evaluated_at: Option<String>,
    pub last_size: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentSizeSnapshot {
    pub id: String,
    pub segment_id: String,
    pub size: u64,
    /// Contacts actually evaluated (the known audience), so the size
    /// is never mistaken for full CRM coverage.
    pub evaluated_contacts: u64,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sequence {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Trigger kind: `manual`, `form_submitted`, `purchase`,
    /// `tag_added`. Trigger details live in the trigger fields.
    pub trigger_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_form_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_tag: Option<String>,
    /// `active`, `paused`, `archived`.
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceStep {
    pub id: String,
    pub sequence_id: String,
    /// 1-based order inside the sequence.
    pub order: i64,
    /// Delay after the previous step (or after enrollment for the
    /// first step), in seconds.
    pub delay_seconds: i64,
    /// Branching condition, stored as data:
    /// `{"type": "always"|"opened"|"clicked"|"did_not_open"|
    /// "did_not_click", "step_id": "...", "window_hours": 72}`.
    /// The step runs when the condition holds for the referenced
    /// earlier step; `did_not_*` conditions only fire once the
    /// window has expired.
    pub condition: Value,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentStepEvent {
    pub step_id: String,
    pub step_order: i64,
    /// `sent`, `skipped`, `waiting`, `completed`.
    pub action: String,
    pub detail: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Enrollment {
    pub id: String,
    pub sequence_id: String,
    pub contact_id: String,
    pub email: String,
    /// Order of the next step to run (1-based). Zero means the
    /// sequence is finished.
    pub next_step: i64,
    /// `active`, `paused`, `completed`, `unsubscribed`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_step_at: Option<String>,
    pub history: Vec<EnrollmentStepEvent>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreEvent {
    pub id: String,
    pub action: String,
    pub points: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactScore {
    pub contact_id: String,
    pub email: String,
    /// Last computed effective score (decay applied at read time).
    pub score: i64,
    pub warm: bool,
    pub hot: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hot_notified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_action_at: Option<String>,
    pub events: Vec<ScoreEvent>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreRule {
    pub id: String,
    /// Action name this rule scores (for example `email.opened`).
    /// Action names are data, defined by administrators; the
    /// defaults in settings are only UI hints.
    pub action: String,
    pub points: i64,
    /// Linear decay window in days: after this many days the
    /// points from an event fade to zero. None means no decay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decay_days: Option<u64>,
    pub active: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketingSend {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrollment_id: Option<String>,
    pub contact_id: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant_id: Option<String>,
    /// `queued`, `sent`, `failed`, `skipped`, `opened`, `clicked`,
    /// `bounced`, `unsubscribed`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mail_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudienceContact {
    pub contact_id: String,
    pub email: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Extra contact fields, keyed by field name, for segment rules.
    #[serde(default)]
    pub fields: HashMap<String, Value>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suppression {
    pub id: String,
    pub email: String,
    /// `global` or `campaign`.
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentCacheEntry {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_id: Option<String>,
    pub granted: bool,
    pub purpose: String,
    pub source: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackingEvent {
    pub id: String,
    pub campaign_id: String,
    pub contact_ref: String,
    /// `open` or `click`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub created_at: String,
}

/// Admin-editable marketing settings. Every value here is a
/// configurable default: nothing in the module treats these as
/// fixed behaviour, and an administrator may change them through
/// `PUT /api/v1/marketing/settings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketingSettings {
    /// Default share (0-100) for the first A/B variant when a
    /// variant is created without an explicit weight.
    pub default_split_percent_a: u8,
    /// Minimum sends per variant before an A/B winner may be
    /// picked. Below this, `ab-decide` answers 422.
    pub ab_min_sample_per_variant: u64,
    /// Default winner metric: `open_rate` or `click_rate`.
    pub ab_winner_metric: String,
    /// Score at or above which a contact counts as warm.
    pub warm_threshold: i64,
    /// Score at or above which a contact counts as hot and the
    /// hot-lead handoff event fires.
    pub hot_threshold: i64,
    /// Default decay window (days) for new scoring rules that do
    /// not set one. None means no decay by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_score_decay_days: Option<u64>,
    /// Maximum recipients processed in one campaign send run.
    pub max_sends_per_run: u64,
    /// Contact fields segment rules may reference. This is the
    /// allowlist: a rule naming any other field fails validation.
    pub segment_fields: Vec<String>,
    /// Operators segment rules may use.
    pub segment_operators: Vec<String>,
    /// Known scoring action names, shown as hints in admin UI.
    /// Scoring rules may use any action name; this list never
    /// restricts them.
    pub score_actions: Vec<String>,
    /// Consent purpose checked before every send.
    pub consent_purpose: String,
}

impl Default for MarketingSettings {
    fn default() -> Self {
        Self {
            // Configurable defaults (see the struct docs).
            default_split_percent_a: 50,
            ab_min_sample_per_variant: 100,
            ab_winner_metric: "open_rate".to_string(),
            warm_threshold: 25,
            hot_threshold: 50,
            default_score_decay_days: None,
            max_sends_per_run: DEFAULT_MAX_SENDS_PER_RUN,
            segment_fields: vec![
                "name".to_string(),
                "email".to_string(),
                "company".to_string(),
                "phone".to_string(),
                "tags".to_string(),
                "score".to_string(),
                "source".to_string(),
                "created_at".to_string(),
            ],
            segment_operators: vec![
                "eq".to_string(),
                "ne".to_string(),
                "contains".to_string(),
                "starts_with".to_string(),
                "ends_with".to_string(),
                "gt".to_string(),
                "gte".to_string(),
                "lt".to_string(),
                "lte".to_string(),
                "in".to_string(),
                "not_in".to_string(),
                "is_empty".to_string(),
                "is_not_empty".to_string(),
            ],
            score_actions: vec![
                "email.opened".to_string(),
                "email.clicked".to_string(),
                "form.submitted".to_string(),
                "purchase.completed".to_string(),
                "page.visited".to_string(),
                "sequence.completed".to_string(),
            ],
            consent_purpose: "marketing".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct MarketingState {
    hooks: RwLock<Option<Arc<HookBus>>>,
    debug: AtomicBool,
    settings: RwLock<MarketingSettings>,
    campaigns: DashMap<String, MarketingCampaign>,
    variants: DashMap<String, CampaignVariant>,
    segments: DashMap<String, Segment>,
    segment_history: DashMap<String, Vec<SegmentSizeSnapshot>>,
    segment_members: DashMap<String, Vec<String>>,
    sequences: DashMap<String, Sequence>,
    steps: DashMap<String, SequenceStep>,
    enrollments: DashMap<String, Enrollment>,
    scores: DashMap<String, ContactScore>,
    score_rules: DashMap<String, ScoreRule>,
    sends: DashMap<String, MarketingSend>,
    audience: DashMap<String, AudienceContact>,
    suppressions: DashMap<String, Suppression>,
    consent_cache: DashMap<String, ConsentCacheEntry>,
    tracking_events: DashMap<String, TrackingEvent>,
}

impl MarketingState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
            settings: RwLock::new(MarketingSettings::default()),
            campaigns: DashMap::new(),
            variants: DashMap::new(),
            segments: DashMap::new(),
            segment_history: DashMap::new(),
            segment_members: DashMap::new(),
            sequences: DashMap::new(),
            steps: DashMap::new(),
            enrollments: DashMap::new(),
            scores: DashMap::new(),
            score_rules: DashMap::new(),
            sends: DashMap::new(),
            audience: DashMap::new(),
            suppressions: DashMap::new(),
            consent_cache: DashMap::new(),
            tracking_events: DashMap::new(),
        }
    }

    fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("marketing hooks lock poisoned") = Some(hooks);
    }

    fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks
            .read()
            .expect("marketing hooks lock poisoned")
            .clone()
    }

    fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    fn settings_snapshot(&self) -> MarketingSettings {
        self.settings
            .read()
            .expect("marketing settings lock poisoned")
            .clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(hooks) = self.hooks() {
            hooks.emit(event, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn now_string() -> String {
    Utc::now().to_rfc3339()
}

fn new_id() -> String {
    Uuid::now_v7().to_string()
}

/// Caller identity: `X-User-Id` plus `X-Role` (required for
/// capability checks), exactly like the affiliates module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        EVENT_ROLE_CHECK,
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn valid_email(email: &str) -> bool {
    let email = email.trim();
    if email.is_empty() || email.len() > 254 || email.contains(' ') {
        return false;
    }
    match email.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.is_empty()
                && domain.contains('.')
                && email.matches('@').count() == 1
        }
        None => false,
    }
}

fn parse_datetime(raw: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be an RFC 3339 timestamp")))
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    api_json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    api_json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    api_json::error_response(&error, debug)
}

/// Deterministic 64-bit hash used only to assign A/B variants:
/// the same contact always lands in the same variant bucket.
fn hash_contact(contact_id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in contact_id.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// Segment rule evaluation (rules are data, evaluated server-side).
// ---------------------------------------------------------------------------

/// Validate a segment rule tree against the configured field and
/// operator allowlists. Returns the number of leaf rules found.
fn validate_rule_tree(
    tree: &Value,
    settings: &MarketingSettings,
    depth: usize,
) -> UnifiedResult<usize> {
    if depth > MAX_RULE_DEPTH {
        return Err(UnifiedError::bad_request(format!(
            "Segment rule tree is nested too deep (maximum depth {MAX_RULE_DEPTH})"
        )));
    }
    let object = tree
        .as_object()
        .ok_or_else(|| UnifiedError::bad_request("Segment rule must be a JSON object"))?;
    if let Some(rules) = object.get("rules") {
        let operator = object
            .get("operator")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                UnifiedError::bad_request("Segment rule group needs an \"operator\" of \"and\" or \"or\"")
            })?;
        if operator != "and" && operator != "or" {
            return Err(UnifiedError::bad_request(
                "Segment rule group operator must be \"and\" or \"or\"",
            ));
        }
        let rules = rules
            .as_array()
            .ok_or_else(|| UnifiedError::bad_request("Segment rule group \"rules\" must be an array"))?;
        if rules.is_empty() || rules.len() > MAX_RULE_LEAVES {
            return Err(UnifiedError::bad_request(format!(
                "Segment rule group must contain 1 to {MAX_RULE_LEAVES} rules"
            )));
        }
        let mut leaves = 0usize;
        for rule in rules {
            leaves += validate_rule_tree(rule, settings, depth + 1)?;
        }
        Ok(leaves)
    } else {
        let field = object
            .get("field")
            .and_then(|value| value.as_str())
            .ok_or_else(|| UnifiedError::bad_request("Segment rule leaf needs a \"field\""))?;
        if !settings.segment_fields.iter().any(|name| name == field) {
            return Err(UnifiedError::bad_request(format!(
                "Segment field \"{field}\" is not in the configured segment field list"
            )));
        }
        let op = object
            .get("op")
            .and_then(|value| value.as_str())
            .ok_or_else(|| UnifiedError::bad_request("Segment rule leaf needs an \"op\""))?;
        if !settings.segment_operators.iter().any(|name| name == op) {
            return Err(UnifiedError::bad_request(format!(
                "Segment operator \"{op}\" is not in the configured operator list"
            )));
        }
        if !object.contains_key("value") && op != "is_empty" && op != "is_not_empty" {
            return Err(UnifiedError::bad_request(
                "Segment rule leaf needs a \"value\" for this operator",
            ));
        }
        Ok(1)
    }
}

fn value_as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Array(items) => items
            .iter()
            .map(value_as_text)
            .collect::<Vec<String>>()
            .join(","),
        _ => String::new(),
    }
}

fn value_as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn eval_leaf(field_value: &Value, op: &str, expected: &Value) -> bool {
    match op {
        "is_empty" => value_is_empty(field_value),
        "is_not_empty" => !value_is_empty(field_value),
        "eq" => value_as_text(field_value).to_lowercase() == value_as_text(expected).to_lowercase(),
        "ne" => value_as_text(field_value).to_lowercase() != value_as_text(expected).to_lowercase(),
        "contains" => value_as_text(field_value)
            .to_lowercase()
            .contains(&value_as_text(expected).to_lowercase()),
        "starts_with" => value_as_text(field_value)
            .to_lowercase()
            .starts_with(&value_as_text(expected).to_lowercase()),
        "ends_with" => value_as_text(field_value)
            .to_lowercase()
            .ends_with(&value_as_text(expected).to_lowercase()),
        "gt" | "gte" | "lt" | "lte" => {
            match (value_as_number(field_value), value_as_number(expected)) {
                (Some(actual), Some(wanted)) => match op {
                    "gt" => actual > wanted,
                    "gte" => actual >= wanted,
                    "lt" => actual < wanted,
                    _ => actual <= wanted,
                },
                _ => {
                    let actual = value_as_text(field_value);
                    let wanted = value_as_text(expected);
                    match op {
                        "gt" => actual > wanted,
                        "gte" => actual >= wanted,
                        "lt" => actual < wanted,
                        _ => actual <= wanted,
                    }
                }
            }
        }
        "in" => match expected.as_array() {
            Some(options) => {
                if let Some(items) = field_value.as_array() {
                    items.iter().any(|item| {
                        options.iter().any(|option| {
                            value_as_text(item).to_lowercase()
                                == value_as_text(option).to_lowercase()
                        })
                    })
                } else {
                    options.iter().any(|option| {
                        value_as_text(field_value).to_lowercase()
                            == value_as_text(option).to_lowercase()
                    })
                }
            }
            None => false,
        },
        "not_in" => match expected.as_array() {
            Some(options) => {
                if let Some(items) = field_value.as_array() {
                    !items.iter().any(|item| {
                        options.iter().any(|option| {
                            value_as_text(item).to_lowercase()
                                == value_as_text(option).to_lowercase()
                        })
                    })
                } else {
                    !options.iter().any(|option| {
                        value_as_text(field_value).to_lowercase()
                            == value_as_text(option).to_lowercase()
                    })
                }
            }
            None => false,
        },
        _ => false,
    }
}

fn eval_rule_tree(tree: &Value, contact: &Value) -> bool {
    let Some(object) = tree.as_object() else {
        return false;
    };
    if let Some(rules) = object.get("rules").and_then(|value| value.as_array()) {
        let operator = object
            .get("operator")
            .and_then(|value| value.as_str())
            .unwrap_or("and");
        if operator == "or" {
            rules.iter().any(|rule| eval_rule_tree(rule, contact))
        } else {
            rules.iter().all(|rule| eval_rule_tree(rule, contact))
        }
    } else {
        let Some(field) = object.get("field").and_then(|value| value.as_str()) else {
            return false;
        };
        let Some(op) = object.get("op").and_then(|value| value.as_str()) else {
            return false;
        };
        // Unknown fields never match: a rule may only match data
        // that is actually present on the contact snapshot.
        let Some(field_value) = contact.get(field) else {
            return false;
        };
        let expected = object.get("value").cloned().unwrap_or(Value::Null);
        eval_leaf(field_value, op, &expected)
    }
}

/// Build the JSON document a segment rule evaluates against for one
/// audience contact: snapshot fields plus the live lead score.
fn contact_document(state: &MarketingState, contact: &AudienceContact) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("contact_id".to_string(), json!(contact.contact_id));
    map.insert("name".to_string(), json!(contact.name));
    map.insert("email".to_string(), json!(contact.email));
    map.insert(
        "company".to_string(),
        contact.company.clone().map(Value::String).unwrap_or(Value::Null),
    );
    map.insert(
        "phone".to_string(),
        contact.phone.clone().map(Value::String).unwrap_or(Value::Null),
    );
    map.insert("tags".to_string(), json!(contact.tags));
    map.insert(
        "source".to_string(),
        contact.source.clone().map(Value::String).unwrap_or(Value::Null),
    );
    map.insert("created_at".to_string(), json!(contact.updated_at));
    let score = state
        .scores
        .get(&contact.contact_id)
        .map(|entry| entry.score)
        .unwrap_or(0);
    map.insert("score".to_string(), json!(score));
    for (key, value) in &contact.fields {
        map.insert(key.clone(), value.clone());
    }
    Value::Object(map)
}

/// Evaluate a segment against the known audience. Returns the
/// matching contact ids and how many contacts were evaluated.
fn evaluate_segment(state: &MarketingState, segment: &Segment) -> (Vec<String>, u64) {
    let mut members = Vec::new();
    let mut evaluated = 0u64;
    for entry in state.audience.iter() {
        let contact = entry.value();
        evaluated += 1;
        let document = contact_document(state, contact);
        if eval_rule_tree(&segment.rule_tree, &document) {
            members.push(contact.contact_id.clone());
        }
    }
    members.sort();
    (members, evaluated)
}

fn record_segment_evaluation(
    state: &MarketingState,
    segment_id: &str,
    members: &[String],
    evaluated: u64,
) {
    let snapshot = SegmentSizeSnapshot {
        id: new_id(),
        segment_id: segment_id.to_string(),
        size: members.len() as u64,
        evaluated_contacts: evaluated,
        recorded_at: now_string(),
    };
    let mut history = state
        .segment_history
        .entry(segment_id.to_string())
        .or_default();
    history.push(snapshot);
    if history.len() > MAX_SEGMENT_HISTORY {
        let excess = history.len() - MAX_SEGMENT_HISTORY;
        history.drain(0..excess);
    }
    state
        .segment_members
        .insert(segment_id.to_string(), members.to_vec());
    if let Some(mut segment) = state.segments.get_mut(segment_id) {
        segment.last_evaluated_at = Some(now_string());
        segment.last_size = members.len() as u64;
        segment.updated_at = now_string();
    }
}

// ---------------------------------------------------------------------------
// Consent, suppression, and the send gate.
// ---------------------------------------------------------------------------

/// True when the contact is suppressed globally, or for this
/// campaign. Suppression is always honoured before any send.
fn is_suppressed(state: &MarketingState, email: &str, campaign_id: Option<&str>) -> bool {
    let needle = email.trim().to_lowercase();
    state.suppressions.iter().any(|entry| {
        let record = entry.value();
        if record.email.to_lowercase() != needle {
            return false;
        }
        if record.scope == "global" {
            return true;
        }
        match (record.campaign_id.as_deref(), campaign_id) {
            (Some(scope_campaign), Some(wanted)) => scope_campaign == wanted,
            _ => false,
        }
    })
}

/// Outcome of the consent gate: `Ok(())` means the send may go
/// ahead, `Err(reason)` names why it must not.
fn consent_gate(
    state: &MarketingState,
    contact_id: &str,
    email: &str,
) -> Result<(), String> {
    let settings = state.settings_snapshot();
    let key = email.trim().to_lowercase();
    if let Some(entry) = state.consent_cache.get(&key) {
        if entry.purpose == settings.consent_purpose {
            return if entry.granted {
                Ok(())
            } else {
                Err("consent_withdrawn".to_string())
            };
        }
    }
    // Consent events from compliance carry a user id rather than an
    // email address, so the cache is also keyed by contact id.
    if let Some(entry) = state.consent_cache.get(contact_id) {
        if entry.purpose == settings.consent_purpose {
            return if entry.granted {
                Ok(())
            } else {
                Err("consent_withdrawn".to_string())
            };
        }
    }
    if let Some(hooks) = state.hooks() {
        let answer = hooks.emit(
            EVENT_COMPLIANCE_CONSENT_CHECK,
            json!({
                "contact_id": contact_id,
                "email": email,
                "purpose": settings.consent_purpose,
            }),
        );
        if let Some(result) = answer.results.first() {
            if result.get("error").is_none() {
                if let Some(granted) = result.get("granted").and_then(|value| value.as_bool()) {
                    state.consent_cache.insert(
                        key.clone(),
                        ConsentCacheEntry {
                            key: key.clone(),
                            contact_id: Some(contact_id.to_string()),
                            granted,
                            purpose: settings.consent_purpose.clone(),
                            source: EVENT_COMPLIANCE_CONSENT_CHECK.to_string(),
                            updated_at: now_string(),
                        },
                    );
                    return if granted {
                        Ok(())
                    } else {
                        Err("consent_not_granted".to_string())
                    };
                }
            }
        }
    }
    // Fail closed: no consent on record and no handler answered the
    // check, so the contact is never mailed.
    Err("consent_unverified".to_string())
}

fn record_skipped_send(
    state: &MarketingState,
    campaign_id: Option<&str>,
    sequence_id: Option<&str>,
    step_id: Option<&str>,
    enrollment_id: Option<&str>,
    contact_id: &str,
    email: &str,
    variant_id: Option<&str>,
    reason: &str,
) {
    let record = MarketingSend {
        id: new_id(),
        campaign_id: campaign_id.map(str::to_string),
        sequence_id: sequence_id.map(str::to_string),
        step_id: step_id.map(str::to_string),
        enrollment_id: enrollment_id.map(str::to_string),
        contact_id: contact_id.to_string(),
        email: email.to_string(),
        variant_id: variant_id.map(str::to_string),
        status: "skipped".to_string(),
        mail_job_id: None,
        skip_reason: Some(reason.to_string()),
        created_at: now_string(),
        updated_at: now_string(),
    };
    state.sends.insert(record.id.clone(), record);
    state.emit(
        EVENT_MARKETING_SEND_SKIPPED,
        json!({
            "send_id": record.id,
            "campaign_id": campaign_id,
            "sequence_id": sequence_id,
            "contact_id": contact_id,
            "email": email,
            "reason": reason,
        }),
    );
    if let Some(campaign_id) = campaign_id {
        if let Some(mut campaign) = state.campaigns.get_mut(campaign_id) {
            campaign.stats.skipped += 1;
        }
    }
}

/// Ask the Mail module to send one message. Returns the mail job id
/// when a handler queued it, or an error describing why no handler
/// answered.
fn request_mail_send(state: &MarketingState, payload: Value) -> UnifiedResult<String> {
    let Some(hooks) = state.hooks() else {
        return Err(UnifiedError::new(
            "Mail service is unavailable (no hook bus)",
            503,
        ));
    };
    let answer = hooks.emit(EVENT_MAIL_SEND_REQUESTED, payload);
    match answer.results.first() {
        Some(result) if result.get("error").is_none() => {
            let job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if job_id.is_empty() {
                return Err(UnifiedError::new(
                    "Mail service queued the message but returned no job id",
                    503,
                ));
            }
            Ok(job_id)
        }
        Some(result) => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("mail send was refused");
            Err(UnifiedError::new(
                format!("Mail service refused the message: {reason}"),
                503,
            ))
        }
        None => Err(UnifiedError::new(
            "Mail service did not answer the mail.send_requested event: no mail handler is registered",
            503,
        )),
    }
}

// ---------------------------------------------------------------------------
// Lead scoring.
// ---------------------------------------------------------------------------

/// Effective points of one score event, applying the owning rule's
/// linear decay. An event whose rule was deleted keeps full points.
fn effective_event_points(state: &MarketingState, event: &ScoreEvent) -> i64 {
    let Some(rule_id) = &event.rule_id else {
        return event.points;
    };
    let Some(rule) = state.score_rules.get(rule_id).map(|entry| entry.value().clone()) else {
        return event.points;
    };
    let Some(decay_days) = rule.decay_days.filter(|days| *days > 0) else {
        return event.points;
    };
    let Ok(at) = DateTime::parse_from_rfc3339(&event.at) else {
        return event.points;
    };
    let age_days = (Utc::now() - at.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0;
    if age_days <= 0.0 {
        return event.points;
    }
    let remaining = 1.0 - age_days / decay_days as f64;
    if remaining <= 0.0 {
        return 0;
    }
    (event.points as f64 * remaining).round() as i64
}

/// Recompute a contact score from its events (decay applied) and
/// refresh the warm/hot flags. Fires the hot-lead handoff event
/// exactly once per crossing.
fn refresh_score(state: &MarketingState, contact_id: &str) {
    let settings = state.settings_snapshot();
    let Some(mut record) = state.scores.get_mut(contact_id) else {
        return;
    };
    let total: i64 = record
        .events
        .iter()
        .map(|event| effective_event_points(state, event))
        .sum();
    let was_hot = record.hot;
    record.score = total;
    record.warm = total >= settings.warm_threshold;
    record.hot = total >= settings.hot_threshold;
    record.updated_at = now_string();
    let (score, hot, already_notified) = (
        record.score,
        record.hot,
        record.hot_notified_at.is_some(),
    );
    if hot && !was_hot && !already_notified {
        record.hot_notified_at = Some(now_string());
    }
    let email = record.email.clone();
    drop(record);
    state.emit(
        EVENT_MARKETING_SCORE_UPDATED,
        json!({ "contact_id": contact_id, "email": email, "score": score, "hot": hot }),
    );
    if hot && !was_hot && !already_notified {
        state.emit(
            EVENT_MARKETING_LEAD_HOT,
            json!({
                "contact_id": contact_id,
                "email": email,
                "score": score,
                "threshold": settings.hot_threshold,
            }),
        );
    }
}

/// Apply every active scoring rule matching `action` to a contact.
/// Returns the number of rules applied.
fn apply_score_action(
    state: &MarketingState,
    contact_id: &str,
    email: &str,
    action: &str,
) -> usize {
    let rules: Vec<ScoreRule> = state
        .score_rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| rule.active && rule.action == action)
        .collect();
    if rules.is_empty() {
        return 0;
    }
    let now = now_string();
    let mut record = state.scores.entry(contact_id.to_string()).or_insert_with(|| {
        ContactScore {
            contact_id: contact_id.to_string(),
            email: email.to_string(),
            score: 0,
            warm: false,
            hot: false,
            hot_notified_at: None,
            last_action_at: None,
            events: Vec::new(),
            updated_at: now.clone(),
        }
    });
    for rule in &rules {
        record.events.push(ScoreEvent {
            id: new_id(),
            action: action.to_string(),
            points: rule.points,
            rule_id: Some(rule.id.clone()),
            at: now.clone(),
        });
    }
    if record.events.len() > MAX_SCORE_EVENTS {
        let excess = record.events.len() - MAX_SCORE_EVENTS;
        record.events.drain(0..excess);
    }
    record.last_action_at = Some(now.clone());
    record.updated_at = now;
    drop(record);
    refresh_score(state, contact_id);
    rules.len()
}

// ---------------------------------------------------------------------------
// Campaign send execution.
// ---------------------------------------------------------------------------

/// Variant assignment for a recipient: the bucket is derived from
/// the recipient, so the same contact always sees the same variant.
fn pick_variant_for(
    variants: &[CampaignVariant],
    contact_id: &str,
) -> Option<CampaignVariant> {
    let total_weight: u32 = variants.iter().map(|variant| variant.weight_percent as u32).sum();
    if variants.is_empty() || total_weight == 0 {
        return None;
    }
    let bucket = (hash_contact(contact_id) % 100) as u32;
    if bucket >= total_weight {
        return None;
    }
    let mut cursor = 0u32;
    for variant in variants {
        cursor += variant.weight_percent as u32;
        if bucket < cursor {
            return Some(variant.clone());
        }
    }
    None
}

fn campaign_variants(state: &MarketingState, campaign_id: &str) -> Vec<CampaignVariant> {
    let mut items: Vec<CampaignVariant> = state
        .variants
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|variant| variant.campaign_id == campaign_id)
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    items
}

/// Send one campaign to its segment members. Returns
/// (queued, skipped, failed). Every recipient passes the
/// suppression and consent gates first; skips are audited.
fn execute_campaign_send(
    state: &Arc<MarketingState>,
    campaign_id: &str,
) -> UnifiedResult<Value> {
    let mut campaign = state
        .campaigns
        .get(campaign_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
    if campaign.status == "sending" {
        return Err(UnifiedError::conflict("Campaign is already sending"));
    }
    if campaign.status == "sent" {
        return Err(UnifiedError::conflict(
            "Campaign was already sent; duplicate it to send again",
        ));
    }
    if campaign.status == "paused" {
        return Err(UnifiedError::conflict(
            "Campaign is paused; resume it before sending",
        ));
    }
    let Some(segment_id) = campaign.segment_id.clone() else {
        return Err(UnifiedError::unprocessable(
            "Campaign has no segment; assign a segment before sending",
        ));
    };
    let Some(segment) = state
        .segments
        .get(&segment_id)
        .map(|entry| entry.value().clone())
    else {
        return Err(UnifiedError::not_found("Campaign segment not found"));
    };
    let (members, evaluated) = evaluate_segment(state, &segment);
    record_segment_evaluation(state, &segment_id, &members, evaluated);
    let variants = campaign_variants(state, campaign_id);
    let settings = state.settings_snapshot();
    let cap = settings.max_sends_per_run as usize;

    campaign.status = "sending".to_string();
    campaign.updated_at = now_string();
    state.campaigns.insert(campaign.id.clone(), campaign.clone());

    let mut queued = 0u64;
    let mut skipped = 0u64;
    let mut failed = 0u64;
    for contact_id in members.iter().take(cap) {
        let Some(contact) = state
            .audience
            .get(contact_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        if is_suppressed(state, &contact.email, Some(campaign_id)) {
            record_skipped_send(
                state,
                Some(campaign_id),
                None,
                None,
                None,
                &contact.contact_id,
                &contact.email,
                None,
                "unsubscribed",
            );
            skipped += 1;
            continue;
        }
        if let Err(reason) = consent_gate(state, &contact.contact_id, &contact.email) {
            record_skipped_send(
                state,
                Some(campaign_id),
                None,
                None,
                None,
                &contact.contact_id,
                &contact.email,
                None,
                &reason,
            );
            skipped += 1;
            continue;
        }
        let variant = pick_variant_for(&variants, &contact.contact_id);
        let subject = variant
            .as_ref()
            .map(|item| item.subject.clone())
            .unwrap_or_else(|| campaign.subject.clone());
        let body_text = variant
            .as_ref()
            .and_then(|item| item.body_text.clone())
            .or_else(|| campaign.body_text.clone());
        let body_html = variant
            .as_ref()
            .and_then(|item| item.body_html.clone())
            .or_else(|| campaign.body_html.clone());
        let send_id = new_id();
        let mail_payload = json!({
            "to": contact.email,
            "subject": subject,
            "body_text": body_text,
            "body_html": body_html,
            "template_id": campaign.template_id,
            "variables": {
                "contact_id": contact.contact_id,
                "campaign_id": campaign.id,
                "send_id": send_id,
            },
        });
        match request_mail_send(state, mail_payload) {
            Ok(job_id) => {
                state.sends.insert(
                    send_id.clone(),
                    MarketingSend {
                        id: send_id,
                        campaign_id: Some(campaign.id.clone()),
                        sequence_id: None,
                        step_id: None,
                        enrollment_id: None,
                        contact_id: contact.contact_id.clone(),
                        email: contact.email.clone(),
                        variant_id: variant.as_ref().map(|item| item.id.clone()),
                        status: "queued".to_string(),
                        mail_job_id: Some(job_id),
                        skip_reason: None,
                        created_at: now_string(),
                        updated_at: now_string(),
                    },
                );
                if let Some(variant) = &variant {
                    if let Some(mut stored) = state.variants.get_mut(&variant.id) {
                        stored.sent += 1;
                    }
                }
                queued += 1;
            }
            Err(error) => {
                state.sends.insert(
                    send_id.clone(),
                    MarketingSend {
                        id: send_id,
                        campaign_id: Some(campaign.id.clone()),
                        sequence_id: None,
                        step_id: None,
                        enrollment_id: None,
                        contact_id: contact.contact_id.clone(),
                        email: contact.email.clone(),
                        variant_id: variant.as_ref().map(|item| item.id.clone()),
                        status: "failed".to_string(),
                        mail_job_id: None,
                        skip_reason: Some(error.message.clone()),
                        created_at: now_string(),
                        updated_at: now_string(),
                    },
                );
                failed += 1;
            }
        }
    }

    if let Some(mut stored) = state.campaigns.get_mut(campaign_id) {
        stored.stats.queued += queued;
        stored.stats.failed += failed;
        stored.sent_at = Some(now_string());
        stored.updated_at = now_string();
        if failed > 0 && queued == 0 {
            stored.status = "draft".to_string();
        } else {
            stored.status = "sent".to_string();
        }
        campaign = stored.clone();
    }
    if campaign.status == "sent" {
        state.emit(
            EVENT_MARKETING_CAMPAIGN_SENT,
            json!({
                "campaign_id": campaign.id,
                "queued": queued,
                "skipped": skipped,
                "failed": failed,
                "evaluated_contacts": evaluated,
                "sent_at": campaign.sent_at,
            }),
        );
        Ok(json!({
            "campaign_id": campaign.id,
            "queued": queued,
            "skipped": skipped,
            "failed": failed,
            "recipients": members.len(),
            "evaluated_contacts": evaluated,
            "status": campaign.status,
        }))
    } else {
        Err(UnifiedError::new(
            "No mail handler queued any message: the mail.send_requested event went unanswered",
            503,
        ))
    }
}

// ---------------------------------------------------------------------------
// Campaign handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CampaignInput {
    name: String,
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    template_id: Option<String>,
    segment_id: Option<String>,
    newsletter_list_id: Option<String>,
    ab_enabled: Option<bool>,
    ab_winner_metric: Option<String>,
    ab_min_sample: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CampaignUpdateInput {
    name: Option<String>,
    subject: Option<String>,
    body_text: Option<String>,
    body_html: Option<String>,
    template_id: Option<String>,
    segment_id: Option<String>,
    newsletter_list_id: Option<String>,
    ab_enabled: Option<bool>,
    ab_winner_metric: Option<String>,
    ab_min_sample: Option<u64>,
}

fn validate_campaign_input(
    state: &MarketingState,
    segment_id: &Option<String>,
    ab_winner_metric: &Option<String>,
) -> UnifiedResult<()> {
    if let Some(segment_id) = segment_id {
        let id = parse_uuid(segment_id, "segment")?;
        if !state.segments.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Segment not found"));
        }
    }
    if let Some(metric) = ab_winner_metric {
        if metric != "open_rate" && metric != "click_rate" {
            return Err(UnifiedError::bad_request(
                "ab_winner_metric must be \"open_rate\" or \"click_rate\"",
            ));
        }
    }
    Ok(())
}

async fn list_campaigns(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<MarketingCampaign>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<MarketingCampaign> = state
            .campaigns
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|campaign| {
                matches_search(&format!("{} {}", campaign.name, campaign.subject), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| campaign.status == *status)
                    && params
                        .filters
                        .get("segment_id")
                        .is_none_or(|segment| campaign.segment_id.as_deref() == Some(segment.as_str()))
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<CampaignInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 200)?;
        let subject = require_text(&input.subject, "subject", 500)?;
        validate_campaign_input(&state, &input.segment_id, &input.ab_winner_metric)?;
        if input.body_text.is_none() && input.body_html.is_none() && input.template_id.is_none() {
            return Err(UnifiedError::bad_request(
                "Provide body_text or body_html (or a template_id)",
            ));
        }
        let now = now_string();
        let campaign = MarketingCampaign {
            id: new_id(),
            name,
            subject,
            body_text: input.body_text,
            body_html: input.body_html,
            template_id: input.template_id.filter(|value| !value.trim().is_empty()),
            segment_id: input.segment_id.filter(|value| !value.trim().is_empty()),
            newsletter_list_id: input
                .newsletter_list_id
                .filter(|value| !value.trim().is_empty()),
            status: "draft".to_string(),
            scheduled_at: None,
            sent_at: None,
            ab_enabled: input.ab_enabled.unwrap_or(false),
            ab_winner_metric: input
                .ab_winner_metric
                .unwrap_or_else(|| state.settings_snapshot().ab_winner_metric),
            ab_min_sample: input.ab_min_sample,
            winning_variant_id: None,
            stats: CampaignStats::default(),
            created_by: caller.actor.clone(),
            created_at: now.clone(),
            updated_at: now,
        };
        state.campaigns.insert(campaign.id.clone(), campaign.clone());
        state.emit(
            EVENT_MARKETING_CAMPAIGN_CREATED,
            json!({ "campaign_id": campaign.id, "name": campaign.name, "created_by": caller.actor }),
        );
        Ok(campaign)
    })();
    match result {
        Ok(campaign) => created(campaign),
        Err(error) => err(error, debug),
    }
}

async fn get_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))
    })();
    match result {
        Ok(campaign) => ok(campaign),
        Err(error) => err(error, debug),
    }
}

async fn update_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CampaignUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let input = body.into_inner();
        validate_campaign_input(&state, &input.segment_id, &input.ab_winner_metric)?;
        let mut campaign = state
            .campaigns
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only draft campaigns can be updated",
            ));
        }
        if let Some(name) = input.name {
            campaign.name = require_text(&name, "name", 200)?;
        }
        if let Some(subject) = input.subject {
            campaign.subject = require_text(&subject, "subject", 500)?;
        }
        if let Some(body_text) = input.body_text {
            campaign.body_text = Some(body_text);
        }
        if let Some(body_html) = input.body_html {
            campaign.body_html = Some(body_html);
        }
        if let Some(template_id) = input.template_id {
            campaign.template_id =
                if template_id.trim().is_empty() { None } else { Some(template_id) };
        }
        if let Some(segment_id) = input.segment_id {
            campaign.segment_id =
                if segment_id.trim().is_empty() { None } else { Some(segment_id) };
        }
        if let Some(list_id) = input.newsletter_list_id {
            campaign.newsletter_list_id =
                if list_id.trim().is_empty() { None } else { Some(list_id) };
        }
        if let Some(ab_enabled) = input.ab_enabled {
            campaign.ab_enabled = ab_enabled;
        }
        if let Some(metric) = input.ab_winner_metric {
            campaign.ab_winner_metric = metric;
        }
        if let Some(min_sample) = input.ab_min_sample {
            campaign.ab_min_sample = Some(min_sample);
        }
        campaign.updated_at = now_string();
        Ok(campaign.clone())
    })();
    match result {
        Ok(campaign) => ok(campaign),
        Err(error) => err(error, debug),
    }
}

async fn delete_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let campaign = state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only draft campaigns can be deleted",
            ));
        }
        state.campaigns.remove(&id.to_string());
        let variant_ids: Vec<String> = state
            .variants
            .iter()
            .filter(|entry| entry.value().campaign_id == id.to_string())
            .map(|entry| entry.key().clone())
            .collect();
        for variant_id in &variant_ids {
            state.variants.remove(variant_id);
        }
        Ok(json!({ "deleted": true, "id": id.to_string(), "removed_variants": variant_ids.len() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
struct ScheduleInput {
    scheduled_at: String,
}

async fn schedule_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ScheduleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let input = body.into_inner();
        let when = parse_datetime(&input.scheduled_at, "scheduled_at")?;
        if when <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "scheduled_at must be in the future",
            ));
        }
        let mut campaign = state
            .campaigns
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" && campaign.status != "paused" {
            return Err(UnifiedError::conflict(
                "Only draft or paused campaigns can be scheduled",
            ));
        }
        if campaign.segment_id.is_none() {
            return Err(UnifiedError::unprocessable(
                "Campaign has no segment; assign a segment before scheduling",
            ));
        }
        campaign.status = "scheduled".to_string();
        campaign.scheduled_at = Some(when.to_rfc3339());
        campaign.updated_at = now_string();
        let scheduled_at = campaign.scheduled_at.clone();
        drop(campaign);
        state.emit(
            EVENT_SCHEDULER_ENQUEUE,
            json!({
                "job_id": new_id(),
                "queue": "marketing",
                "event": EVENT_MARKETING_CAMPAIGN_SEND,
                "payload": { "campaign_id": id.to_string() },
                "run_at": scheduled_at,
            }),
        );
        Ok(state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .expect("campaign still present"))
    })();
    match result {
        Ok(campaign) => ok(campaign),
        Err(error) => err(error, debug),
    }
}

async fn send_campaign_now(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        if !state.campaigns.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Campaign not found"));
        }
        execute_campaign_send(&state, &id.to_string())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn pause_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let mut campaign = state
            .campaigns
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "scheduled" && campaign.status != "sending" {
            return Err(UnifiedError::conflict(
                "Only scheduled or sending campaigns can be paused",
            ));
        }
        campaign.status = "paused".to_string();
        campaign.updated_at = now_string();
        Ok(campaign.clone())
    })();
    match result {
        Ok(campaign) => ok(campaign),
        Err(error) => err(error, debug),
    }
}

async fn resume_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let mut campaign = state
            .campaigns
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "paused" {
            return Err(UnifiedError::conflict("Only paused campaigns can be resumed"));
        }
        campaign.status = if campaign.scheduled_at.is_some() {
            "scheduled".to_string()
        } else {
            "draft".to_string()
        };
        campaign.updated_at = now_string();
        Ok(campaign.clone())
    })();
    match result {
        Ok(campaign) => ok(campaign),
        Err(error) => err(error, debug),
    }
}

async fn get_campaign_stats(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let campaign = state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        // Recompute open/click counts from the tracking events
        // actually received, so the stats never claim tracking the
        // module did not get. The stored counters are not added on
        // top: tracking events are the single source of truth here.
        let mut stats = campaign.stats.clone();
        stats.opened = 0;
        stats.clicked = 0;
        let mut unique_openers: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut unique_clickers: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in state.tracking_events.iter() {
            let event = entry.value();
            if event.campaign_id != id.to_string() {
                continue;
            }
            if event.kind == "open" {
                stats.opened += 1;
                unique_openers.insert(event.contact_ref.clone());
            } else if event.kind == "click" {
                stats.clicked += 1;
                unique_clickers.insert(event.contact_ref.clone());
            }
        }
        stats.unique_opened = unique_openers.len() as u64;
        stats.unique_clicked = unique_clickers.len() as u64;
        let variants = campaign_variants(&state, &id.to_string());
        Ok(json!({
            "campaign_id": campaign.id,
            "status": campaign.status,
            "stats": stats.view(),
            "winning_variant_id": campaign.winning_variant_id,
            "variants": variants,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// A/B variant handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct VariantInput {
    name: String,
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    weight_percent: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct VariantUpdateInput {
    name: Option<String>,
    subject: Option<String>,
    body_text: Option<String>,
    body_html: Option<String>,
    weight_percent: Option<u8>,
}

fn variant_weight_total(state: &MarketingState, campaign_id: &str, except: Option<&str>) -> u32 {
    state
        .variants
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|variant| {
            variant.campaign_id == campaign_id
                && Some(variant.id.as_str()) != except
        })
        .map(|variant| variant.weight_percent as u32)
        .sum()
}

async fn list_variants(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<CampaignVariant>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        if !state.campaigns.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Campaign not found"));
        }
        Ok(campaign_variants(&state, &id.to_string()))
    })();
    match result {
        Ok(variants) => ok(variants),
        Err(error) => err(error, debug),
    }
}

async fn create_variant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<VariantInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CampaignVariant> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let campaign = state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" {
            return Err(UnifiedError::conflict(
                "Variants can only be added to draft campaigns",
            ));
        }
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 200)?;
        let subject = require_text(&input.subject, "subject", 500)?;
        if input.body_text.is_none() && input.body_html.is_none() {
            return Err(UnifiedError::bad_request(
                "Provide body_text or body_html for the variant",
            ));
        }
        let weight = input
            .weight_percent
            .unwrap_or_else(|| state.settings_snapshot().default_split_percent_a);
        if weight == 0 || weight > 100 {
            return Err(UnifiedError::bad_request(
                "weight_percent must be between 1 and 100",
            ));
        }
        if variant_weight_total(&state, &id.to_string(), None) + weight as u32 > 100 {
            return Err(UnifiedError::bad_request(
                "Variant weights must sum to at most 100 (the remainder goes to the base content)",
            ));
        }
        let now = now_string();
        let variant = CampaignVariant {
            id: new_id(),
            campaign_id: id.to_string(),
            name,
            subject,
            body_text: input.body_text,
            body_html: input.body_html,
            weight_percent: weight,
            sent: 0,
            opened: 0,
            clicked: 0,
            created_at: now,
        };
        state.variants.insert(variant.id.clone(), variant.clone());
        Ok(variant)
    })();
    match result {
        Ok(variant) => created(variant),
        Err(error) => err(error, debug),
    }
}

async fn update_variant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<VariantUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CampaignVariant> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let (campaign_id, variant_id) = path.into_inner();
        let campaign_uuid = parse_uuid(&campaign_id, "campaign")?;
        let variant_uuid = parse_uuid(&variant_id, "variant")?;
        let campaign = state
            .campaigns
            .get(&campaign_uuid.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" {
            return Err(UnifiedError::conflict(
                "Variants can only be changed on draft campaigns",
            ));
        }
        let input = body.into_inner();
        if let Some(weight) = input.weight_percent {
            if weight == 0 || weight > 100 {
                return Err(UnifiedError::bad_request(
                    "weight_percent must be between 1 and 100",
                ));
            }
            let others =
                variant_weight_total(&state, &campaign_uuid.to_string(), Some(&variant_uuid.to_string()));
            if others + weight as u32 > 100 {
                return Err(UnifiedError::bad_request(
                    "Variant weights must sum to at most 100",
                ));
            }
        }
        let mut variant = state
            .variants
            .get_mut(&variant_uuid.to_string())
            .ok_or_else(|| UnifiedError::not_found("Variant not found"))?;
        if variant.campaign_id != campaign_uuid.to_string() {
            return Err(UnifiedError::not_found("Variant not found"));
        }
        if let Some(name) = input.name {
            variant.name = require_text(&name, "name", 200)?;
        }
        if let Some(subject) = input.subject {
            variant.subject = require_text(&subject, "subject", 500)?;
        }
        if let Some(body_text) = input.body_text {
            variant.body_text = Some(body_text);
        }
        if let Some(body_html) = input.body_html {
            variant.body_html = Some(body_html);
        }
        if let Some(weight) = input.weight_percent {
            variant.weight_percent = weight;
        }
        Ok(variant.clone())
    })();
    match result {
        Ok(variant) => ok(variant),
        Err(error) => err(error, debug),
    }
}

async fn delete_variant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let (campaign_id, variant_id) = path.into_inner();
        let campaign_uuid = parse_uuid(&campaign_id, "campaign")?;
        let variant_uuid = parse_uuid(&variant_id, "variant")?;
        let campaign = state
            .campaigns
            .get(&campaign_uuid.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" {
            return Err(UnifiedError::conflict(
                "Variants can only be removed from draft campaigns",
            ));
        }
        let variant = state
            .variants
            .get(&variant_uuid.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Variant not found"))?;
        if variant.campaign_id != campaign_uuid.to_string() {
            return Err(UnifiedError::not_found("Variant not found"));
        }
        state.variants.remove(&variant_uuid.to_string());
        Ok(json!({ "deleted": true, "id": variant_uuid.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
struct AbDecideInput {
    metric: Option<String>,
    min_sample: Option<u64>,
}

/// Pick the A/B winner from real send and tracking data. Refuses
/// (422) when any variant is below the minimum sample or when no
/// tracking event exists for the chosen metric: a winner picked
/// from no data would be fabricated.
async fn ab_decide(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AbDecideInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let input = body.into_inner();
        let campaign = state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if !campaign.ab_enabled {
            return Err(UnifiedError::unprocessable(
                "A/B testing is not enabled for this campaign",
            ));
        }
        let metric = input.metric.unwrap_or_else(|| campaign.ab_winner_metric.clone());
        if metric != "open_rate" && metric != "click_rate" {
            return Err(UnifiedError::bad_request(
                "metric must be \"open_rate\" or \"click_rate\"",
            ));
        }
        let min_sample = input
            .min_sample
            .or(campaign.ab_min_sample)
            .unwrap_or_else(|| state.settings_snapshot().ab_min_sample_per_variant);
        let variants = campaign_variants(&state, &id.to_string());
        if variants.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Campaign has no A/B variants to compare",
            ));
        }
        // Base content counts as a candidate too, using the sends
        // recorded without a variant id. Base opens/clicks are the
        // tracking events whose reference identifies one of those
        // base sends (by contact id or email).
        let base_sends: Vec<MarketingSend> = state
            .sends
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|send| {
                send.campaign_id.as_deref() == Some(id.as_str())
                    && send.variant_id.is_none()
                    && send.status != "skipped"
            })
            .collect();
        let base_sent = base_sends.len() as u64;
        let base_refs: std::collections::HashSet<String> = base_sends
            .iter()
            .flat_map(|send| [send.contact_id.clone(), send.email.clone()])
            .collect();
        let base_opened = state
            .tracking_events
            .iter()
            .filter(|entry| {
                entry.value().campaign_id == id.to_string()
                    && entry.value().kind == "open"
                    && base_refs.contains(&entry.value().contact_ref)
            })
            .count() as u64;
        let base_clicked = state
            .tracking_events
            .iter()
            .filter(|entry| {
                entry.value().campaign_id == id.to_string()
                    && entry.value().kind == "click"
                    && base_refs.contains(&entry.value().contact_ref)
            })
            .count() as u64;
        let mut candidates: Vec<(String, String, u64, u64)> = Vec::new();
        for variant in &variants {
            let rate_count = if metric == "click_rate" { variant.clicked } else { variant.opened };
            candidates.push((
                variant.id.clone(),
                variant.name.clone(),
                variant.sent,
                rate_count,
            ));
        }
        candidates.push((
            "base".to_string(),
            "base".to_string(),
            base_sent,
            if metric == "click_rate" { base_clicked } else { base_opened },
        ));
        let below_min: Vec<&(String, String, u64, u64)> = candidates
            .iter()
            .filter(|candidate| candidate.2 < min_sample)
            .collect();
        if !below_min.is_empty() {
            return Err(UnifiedError::unprocessable(format!(
                "Not enough data: every variant needs at least {min_sample} sends before a winner can be picked ({} below minimum)",
                below_min.len()
            )));
        }
        let total_events: u64 = candidates.iter().map(|candidate| candidate.3).sum();
        if total_events == 0 {
            return Err(UnifiedError::unprocessable(format!(
                "No {metric} tracking events have been received for this campaign, so no winner can be picked from real data"
            )));
        }
        let mut best: Option<&(String, String, u64, u64)> = None;
        let mut best_rate = -1.0f64;
        for candidate in &candidates {
            let rate = candidate.3 as f64 / candidate.2 as f64;
            if rate > best_rate {
                best_rate = rate;
                best = Some(candidate);
            }
        }
        let winner = best.expect("at least one candidate exists");
        if let Some(mut stored) = state.campaigns.get_mut(&id.to_string()) {
            stored.winning_variant_id = if winner.0 == "base" {
                None
            } else {
                Some(winner.0.clone())
            };
            stored.updated_at = now_string();
        }
        state.emit(
            EVENT_MARKETING_CAMPAIGN_AB_WINNER,
            json!({
                "campaign_id": id.to_string(),
                "metric": metric,
                "min_sample": min_sample,
                "winner_variant_id": winner.0,
                "winner_name": winner.1,
                "winner_rate": best_rate,
            }),
        );
        Ok(json!({
            "campaign_id": id.to_string(),
            "metric": metric,
            "min_sample": min_sample,
            "winner_variant_id": winner.0,
            "winner_name": winner.1,
            "winner_rate": (best_rate * 10000.0).round() / 100.0,
            "candidates": candidates.iter().map(|candidate| json!({
                "variant_id": candidate.0,
                "name": candidate.1,
                "sent": candidate.2,
                "events": candidate.3,
                "rate": if candidate.2 == 0 { 0.0 } else { (candidate.3 as f64 / candidate.2 as f64 * 10000.0).round() / 100.0 },
            })).collect::<Vec<Value>>(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Segment handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SegmentInput {
    name: String,
    description: Option<String>,
    rule_tree: Value,
}

#[derive(Debug, Deserialize)]
struct SegmentUpdateInput {
    name: Option<String>,
    description: Option<String>,
    rule_tree: Option<Value>,
    status: Option<String>,
}

async fn list_segments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Segment>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<Segment> = state
            .segments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|segment| {
                matches_search(&format!("{} {}", segment.name, segment.description), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| segment.status == *status)
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_segment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<SegmentInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Segment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 200)?;
        let settings = state.settings_snapshot();
        validate_rule_tree(&input.rule_tree, &settings, 0)?;
        let now = now_string();
        let segment = Segment {
            id: new_id(),
            name,
            description: input.description.unwrap_or_default(),
            rule_tree: input.rule_tree,
            status: "active".to_string(),
            last_evaluated_at: None,
            last_size: 0,
            created_at: now.clone(),
            updated_at: now,
        };
        state.segments.insert(segment.id.clone(), segment.clone());
        Ok(segment)
    })();
    match result {
        Ok(segment) => created(segment),
        Err(error) => err(error, debug),
    }
}

async fn get_segment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Segment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        state
            .segments
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Segment not found"))
    })();
    match result {
        Ok(segment) => ok(segment),
        Err(error) => err(error, debug),
    }
}

async fn update_segment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SegmentUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Segment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        let input = body.into_inner();
        if let Some(rule_tree) = &input.rule_tree {
            let settings = state.settings_snapshot();
            validate_rule_tree(rule_tree, &settings, 0)?;
        }
        if let Some(status) = &input.status {
            if status != "active" && status != "archived" {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\" or \"archived\"",
                ));
            }
        }
        let mut segment = state
            .segments
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Segment not found"))?;
        if let Some(name) = input.name {
            segment.name = require_text(&name, "name", 200)?;
        }
        if let Some(description) = input.description {
            segment.description = description;
        }
        if let Some(rule_tree) = input.rule_tree {
            segment.rule_tree = rule_tree;
        }
        if let Some(status) = input.status {
            segment.status = status;
        }
        segment.updated_at = now_string();
        Ok(segment.clone())
    })();
    match result {
        Ok(segment) => ok(segment),
        Err(error) => err(error, debug),
    }
}

async fn delete_segment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        if state
            .campaigns
            .iter()
            .any(|entry| entry.value().segment_id.as_deref() == Some(id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Segment is used by a campaign; remove it from the campaign first",
            ));
        }
        match state.segments.remove(&id.to_string()) {
            Some(_) => {
                state.segment_history.remove(&id.to_string());
                state.segment_members.remove(&id.to_string());
                Ok(json!({ "deleted": true, "id": id.to_string() }))
            }
            None => Err(UnifiedError::not_found("Segment not found")),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn evaluate_segment_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        let segment = state
            .segments
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Segment not found"))?;
        let (members, evaluated) = evaluate_segment(&state, &segment);
        record_segment_evaluation(&state, &id.to_string(), &members, evaluated);
        state.emit(
            EVENT_MARKETING_SEGMENT_EVALUATED,
            json!({
                "segment_id": id.to_string(),
                "size": members.len(),
                "evaluated_contacts": evaluated,
            }),
        );
        Ok(json!({
            "segment_id": id.to_string(),
            "size": members.len(),
            "evaluated_contacts": evaluated,
            "member_contact_ids": members,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_segment_members(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AudienceContact>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        if !state.segments.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Segment not found"));
        }
        let params = query.into_inner();
        let member_ids: Vec<String> = state
            .segment_members
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let mut items: Vec<AudienceContact> = member_ids
            .iter()
            .filter_map(|contact_id| {
                state
                    .audience
                    .get(contact_id)
                    .map(|entry| entry.value().clone())
            })
            .filter(|contact| matches_search(&format!("{} {}", contact.name, contact.email), &params))
            .collect();
        items.sort_by(|a, b| a.email.cmp(&b.email));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn list_segment_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<SegmentSizeSnapshot>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        if !state.segments.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Segment not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<SegmentSizeSnapshot> = state
            .segment_history
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        items.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Sequence step execution.
// ---------------------------------------------------------------------------

fn sequence_steps(state: &MarketingState, sequence_id: &str) -> Vec<SequenceStep> {
    let mut items: Vec<SequenceStep> = state
        .steps
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|step| step.sequence_id == sequence_id)
        .collect();
    items.sort_by(|a, b| a.order.cmp(&b.order));
    items
}

/// Evaluate one step condition for an enrollment. Returns
/// `StepDecision::Ready` when the step may run now, `Waiting` when
/// the condition window has not expired yet, and `Skip` when the
/// window expired without the condition ever holding.
enum StepDecision {
    Ready,
    Waiting,
    Skip,
}

fn decide_step(state: &MarketingState, enrollment: &Enrollment, step: &SequenceStep) -> StepDecision {
    let condition = &step.condition;
    let condition_type = condition
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("always");
    if condition_type == "always" {
        return StepDecision::Ready;
    }
    let Some(ref_step_id) = condition
        .get("step_id")
        .and_then(|value| value.as_str())
    else {
        // A branching condition without a referenced step can never
        // be satisfied; treat it as a configuration error and skip
        // the step rather than blocking the enrollment forever.
        return StepDecision::Skip;
    };
    let window_hours = condition
        .get("window_hours")
        .and_then(|value| value.as_u64())
        .unwrap_or(72);
    let ref_send = state
        .sends
        .iter()
        .map(|entry| entry.value().clone())
        .find(|send| {
            send.enrollment_id.as_deref() == Some(enrollment.id.as_str())
                && send.step_id.as_deref() == Some(ref_step_id)
        });
    let Some(ref_send) = ref_send else {
        return StepDecision::Skip;
    };
    let sent_at = DateTime::parse_from_rfc3339(&ref_send.created_at)
        .map(|parsed| parsed.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let window_end = sent_at + chrono::Duration::hours(window_hours as i64);
    // A click implies an open, but an open does not imply a click:
    // the two conditions must be evaluated separately.
    let opened = ref_send.status == "opened" || ref_send.status == "clicked";
    let clicked = ref_send.status == "clicked";
    let event_happened = match condition_type {
        "opened" => opened,
        "clicked" => clicked,
        "did_not_open" => !opened,
        "did_not_click" => !clicked,
        _ => return StepDecision::Skip,
    };
    match condition_type {
        "opened" | "clicked" => {
            if event_happened {
                StepDecision::Ready
            } else if Utc::now() >= window_end {
                StepDecision::Skip
            } else {
                StepDecision::Waiting
            }
        }
        _ => {
            if Utc::now() >= window_end {
                if event_happened {
                    StepDecision::Ready
                } else {
                    StepDecision::Skip
                }
            } else {
                StepDecision::Waiting
            }
        }
    }
}

fn push_enrollment_history(
    enrollment: &mut Enrollment,
    step: &SequenceStep,
    action: &str,
    detail: &str,
) {
    enrollment.history.push(EnrollmentStepEvent {
        step_id: step.id.clone(),
        step_order: step.order,
        action: action.to_string(),
        detail: detail.to_string(),
        at: now_string(),
    });
}

/// Run the next due step of one enrollment. Returns true when the
/// enrollment advanced (sent or skipped a step).
fn advance_enrollment(state: &Arc<MarketingState>, enrollment_id: &str) -> UnifiedResult<bool> {
    let mut enrollment = state
        .enrollments
        .get(enrollment_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
    if enrollment.status != "active" {
        return Ok(false);
    }
    if let Some(next_at) = &enrollment.next_step_at {
        if let Ok(when) = DateTime::parse_from_rfc3339(next_at) {
            if when.with_timezone(&Utc) > Utc::now() {
                return Ok(false);
            }
        }
    }
    let steps = sequence_steps(state, &enrollment.sequence_id);
    let Some(step) = steps
        .iter()
        .find(|candidate| candidate.order == enrollment.next_step)
        .cloned()
    else {
        enrollment.status = "completed".to_string();
        enrollment.next_step = 0;
        enrollment.next_step_at = None;
        enrollment.updated_at = now_string();
        state
            .enrollments
            .insert(enrollment.id.clone(), enrollment.clone());
        state.emit(
            EVENT_MARKETING_SEQUENCE_COMPLETED,
            json!({
                "enrollment_id": enrollment.id,
                "sequence_id": enrollment.sequence_id,
                "contact_id": enrollment.contact_id,
            }),
        );
        apply_score_action(state, &enrollment.contact_id, &enrollment.email, "sequence.completed");
        return Ok(true);
    };

    match decide_step(state, &enrollment, &step) {
        StepDecision::Waiting => {
            // Recheck in one hour; the condition window may still close.
            enrollment.next_step_at =
                Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339());
            enrollment.updated_at = now_string();
            state
                .enrollments
                .insert(enrollment.id.clone(), enrollment);
            Ok(false)
        }
        StepDecision::Skip => {
            push_enrollment_history(
                &mut enrollment,
                &step,
                "skipped",
                "Step condition was not met within its window",
            );
            enrollment.next_step = step.order + 1;
            let next = steps.iter().find(|candidate| candidate.order == enrollment.next_step);
            enrollment.next_step_at = next.map(|next_step| {
                (Utc::now() + chrono::Duration::seconds(next_step.delay_seconds)).to_rfc3339()
            });
            enrollment.updated_at = now_string();
            state
                .enrollments
                .insert(enrollment.id.clone(), enrollment);
            Ok(true)
        }
        StepDecision::Ready => {
            if is_suppressed(state, &enrollment.email, None) {
                record_skipped_send(
                    state,
                    None,
                    Some(&enrollment.sequence_id),
                    Some(&step.id),
                    Some(&enrollment.id),
                    &enrollment.contact_id,
                    &enrollment.email,
                    None,
                    "unsubscribed",
                );
                push_enrollment_history(
                    &mut enrollment,
                    &step,
                    "skipped",
                    "Contact unsubscribed; step skipped and enrollment stopped",
                );
                enrollment.status = "unsubscribed".to_string();
                enrollment.next_step_at = None;
                enrollment.updated_at = now_string();
                state
                    .enrollments
                    .insert(enrollment.id.clone(), enrollment);
                return Ok(true);
            }
            if let Err(reason) = consent_gate(state, &enrollment.contact_id, &enrollment.email) {
                record_skipped_send(
                    state,
                    None,
                    Some(&enrollment.sequence_id),
                    Some(&step.id),
                    Some(&enrollment.id),
                    &enrollment.contact_id,
                    &enrollment.email,
                    None,
                    &reason,
                );
                push_enrollment_history(
                    &mut enrollment,
                    &step,
                    "skipped",
                    &format!("Send skipped: {reason}"),
                );
                enrollment.next_step = step.order + 1;
                let next = steps.iter().find(|candidate| candidate.order == enrollment.next_step);
                enrollment.next_step_at = next.map(|next_step| {
                    (Utc::now() + chrono::Duration::seconds(next_step.delay_seconds)).to_rfc3339()
                });
                if next.is_none() {
                    enrollment.status = "completed".to_string();
                    enrollment.next_step = 0;
                }
                enrollment.updated_at = now_string();
                state
                    .enrollments
                    .insert(enrollment.id.clone(), enrollment);
                return Ok(true);
            }
            let send_id = new_id();
            let mail_payload = json!({
                "to": enrollment.email,
                "subject": step.subject,
                "body_text": step.body_text,
                "body_html": step.body_html,
                "template_id": step.template_id,
                "variables": {
                    "contact_id": enrollment.contact_id,
                    "sequence_id": enrollment.sequence_id,
                    "step_id": step.id,
                    "send_id": send_id,
                },
            });
            match request_mail_send(state, mail_payload) {
                Ok(job_id) => {
                    state.sends.insert(
                        send_id.clone(),
                        MarketingSend {
                            id: send_id,
                            campaign_id: None,
                            sequence_id: Some(enrollment.sequence_id.clone()),
                            step_id: Some(step.id.clone()),
                            enrollment_id: Some(enrollment.id.clone()),
                            contact_id: enrollment.contact_id.clone(),
                            email: enrollment.email.clone(),
                            variant_id: None,
                            status: "queued".to_string(),
                            mail_job_id: Some(job_id),
                            skip_reason: None,
                            created_at: now_string(),
                            updated_at: now_string(),
                        },
                    );
                    push_enrollment_history(&mut enrollment, &step, "sent", "Step message queued with the mail service");
                    enrollment.next_step = step.order + 1;
                    let next = steps.iter().find(|candidate| candidate.order == enrollment.next_step);
                    enrollment.next_step_at = next.map(|next_step| {
                        (Utc::now() + chrono::Duration::seconds(next_step.delay_seconds))
                            .to_rfc3339()
                    });
                    if next.is_none() {
                        enrollment.status = "completed".to_string();
                        enrollment.next_step = 0;
                        enrollment.next_step_at = None;
                    }
                    enrollment.updated_at = now_string();
                    state
                        .enrollments
                        .insert(enrollment.id.clone(), enrollment.clone());
                    state.emit(
                        EVENT_MARKETING_SEQUENCE_STEP_SENT,
                        json!({
                            "enrollment_id": enrollment.id,
                            "sequence_id": enrollment.sequence_id,
                            "step_id": step.id,
                            "contact_id": enrollment.contact_id,
                        }),
                    );
                    if enrollment.status == "completed" {
                        state.emit(
                            EVENT_MARKETING_SEQUENCE_COMPLETED,
                            json!({
                                "enrollment_id": enrollment.id,
                                "sequence_id": enrollment.sequence_id,
                                "contact_id": enrollment.contact_id,
                            }),
                        );
                        apply_score_action(
                            state,
                            &enrollment.contact_id,
                            &enrollment.email,
                            "sequence.completed",
                        );
                    }
                    Ok(true)
                }
                Err(error) => Err(UnifiedError::new(
                    format!("Mail service refused the step message: {}", error.message),
                    503,
                )),
            }
        }
    }
}

/// Run every due step of every active enrollment. Returns
/// (advanced, waiting, errors).
fn process_due_enrollments(state: &Arc<MarketingState>) -> (u64, u64, u64) {
    let due_ids: Vec<String> = state
        .enrollments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|enrollment| {
            enrollment.status == "active"
                && enrollment
                    .next_step_at
                    .as_deref()
                    .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                    .is_some_and(|when| when.with_timezone(&Utc) <= Utc::now())
        })
        .map(|enrollment| enrollment.id)
        .collect();
    let mut advanced = 0u64;
    let mut waiting = 0u64;
    let mut errors = 0u64;
    for id in due_ids {
        match advance_enrollment(state, &id) {
            Ok(true) => advanced += 1,
            Ok(false) => waiting += 1,
            Err(_) => errors += 1,
        }
    }
    (advanced, waiting, errors)
}

/// Enroll one contact in a sequence. Returns the enrollment, or
/// None when the contact is already enrolled and active.
fn enroll_contact(
    state: &Arc<MarketingState>,
    sequence_id: &str,
    contact_id: &str,
    email: &str,
) -> UnifiedResult<Option<Enrollment>> {
    let sequence = state
        .sequences
        .get(sequence_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Sequence not found"))?;
    if sequence.status != "active" {
        return Err(UnifiedError::conflict(
            "Sequence is not active; only active sequences accept enrollments",
        ));
    }
    if state
        .enrollments
        .iter()
        .any(|entry| {
            entry.value().sequence_id == sequence_id
                && entry.value().contact_id == contact_id
                && entry.value().status == "active"
        })
    {
        return Ok(None);
    }
    if is_suppressed(state, email, None) {
        return Err(UnifiedError::conflict(
            "Contact is unsubscribed and cannot be enrolled",
        ));
    }
    let steps = sequence_steps(state, sequence_id);
    let first_delay = steps.first().map(|step| step.delay_seconds).unwrap_or(0);
    let now = now_string();
    let enrollment = Enrollment {
        id: new_id(),
        sequence_id: sequence_id.to_string(),
        contact_id: contact_id.to_string(),
        email: email.to_string(),
        next_step: 1,
        status: "active".to_string(),
        next_step_at: Some(
            (Utc::now() + chrono::Duration::seconds(first_delay)).to_rfc3339(),
        ),
        history: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    state
        .enrollments
        .insert(enrollment.id.clone(), enrollment.clone());
    state.emit(
        EVENT_MARKETING_SEQUENCE_ENROLLED,
        json!({
            "enrollment_id": enrollment.id,
            "sequence_id": sequence_id,
            "contact_id": contact_id,
            "email": email,
        }),
    );
    Ok(Some(enrollment))
}

// ---------------------------------------------------------------------------
// Sequence handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SequenceInput {
    name: String,
    description: Option<String>,
    trigger_kind: String,
    trigger_form_id: Option<String>,
    trigger_tag: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SequenceUpdateInput {
    name: Option<String>,
    description: Option<String>,
    trigger_kind: Option<String>,
    trigger_form_id: Option<String>,
    trigger_tag: Option<String>,
    status: Option<String>,
}

fn validate_trigger_kind(kind: &str) -> UnifiedResult<()> {
    if matches!(kind, "manual" | "form_submitted" | "purchase" | "tag_added") {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(
            "trigger_kind must be one of: manual, form_submitted, purchase, tag_added",
        ))
    }
}

async fn list_sequences(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Sequence>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<Sequence> = state
            .sequences
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sequence| {
                matches_search(&format!("{} {}", sequence.name, sequence.description), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| sequence.status == *status)
                    && params
                        .filters
                        .get("trigger_kind")
                        .is_none_or(|kind| sequence.trigger_kind == *kind)
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn create_sequence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<SequenceInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Sequence> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 200)?;
        validate_trigger_kind(&input.trigger_kind)?;
        if input.trigger_kind == "form_submitted"
            && input.trigger_form_id.as_deref().unwrap_or("").trim().is_empty()
        {
            return Err(UnifiedError::bad_request(
                "trigger_form_id is required when trigger_kind is form_submitted",
            ));
        }
        if input.trigger_kind == "tag_added"
            && input.trigger_tag.as_deref().unwrap_or("").trim().is_empty()
        {
            return Err(UnifiedError::bad_request(
                "trigger_tag is required when trigger_kind is tag_added",
            ));
        }
        let now = now_string();
        let sequence = Sequence {
            id: new_id(),
            name,
            description: input.description.unwrap_or_default(),
            trigger_kind: input.trigger_kind,
            trigger_form_id: input.trigger_form_id.filter(|value| !value.trim().is_empty()),
            trigger_tag: input.trigger_tag.filter(|value| !value.trim().is_empty()),
            status: "active".to_string(),
            created_at: now.clone(),
            updated_at: now,
        };
        state.sequences.insert(sequence.id.clone(), sequence.clone());
        Ok(sequence)
    })();
    match result {
        Ok(sequence) => created(sequence),
        Err(error) => err(error, debug),
    }
}

async fn get_sequence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Sequence> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        state
            .sequences
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Sequence not found"))
    })();
    match result {
        Ok(sequence) => ok(sequence),
        Err(error) => err(error, debug),
    }
}

async fn update_sequence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SequenceUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Sequence> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        let input = body.into_inner();
        if let Some(kind) = &input.trigger_kind {
            validate_trigger_kind(kind)?;
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "paused" | "archived") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, paused, archived",
                ));
            }
        }
        let mut sequence = state
            .sequences
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Sequence not found"))?;
        if let Some(name) = input.name {
            sequence.name = require_text(&name, "name", 200)?;
        }
        if let Some(description) = input.description {
            sequence.description = description;
        }
        if let Some(kind) = input.trigger_kind {
            sequence.trigger_kind = kind;
        }
        if let Some(form_id) = input.trigger_form_id {
            sequence.trigger_form_id =
                if form_id.trim().is_empty() { None } else { Some(form_id) };
        }
        if let Some(tag) = input.trigger_tag {
            sequence.trigger_tag = if tag.trim().is_empty() { None } else { Some(tag) };
        }
        if let Some(status) = input.status {
            sequence.status = status;
        }
        sequence.updated_at = now_string();
        Ok(sequence.clone())
    })();
    match result {
        Ok(sequence) => ok(sequence),
        Err(error) => err(error, debug),
    }
}

async fn delete_sequence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        if state
            .enrollments
            .iter()
            .any(|entry| entry.value().sequence_id == id.to_string() && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Sequence has active enrollments; pause it instead of deleting",
            ));
        }
        match state.sequences.remove(&id.to_string()) {
            Some(_) => {
                let step_ids: Vec<String> = state
                    .steps
                    .iter()
                    .filter(|entry| entry.value().sequence_id == id.to_string())
                    .map(|entry| entry.key().clone())
                    .collect();
                for step_id in &step_ids {
                    state.steps.remove(step_id);
                }
                Ok(json!({ "deleted": true, "id": id.to_string(), "removed_steps": step_ids.len() }))
            }
            None => Err(UnifiedError::not_found("Sequence not found")),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// --- Steps ---

#[derive(Debug, Deserialize)]
struct StepInput {
    order: Option<i64>,
    delay_seconds: Option<i64>,
    condition: Option<Value>,
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    template_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StepUpdateInput {
    order: Option<i64>,
    delay_seconds: Option<i64>,
    condition: Option<Value>,
    subject: Option<String>,
    body_text: Option<String>,
    body_html: Option<String>,
    template_id: Option<String>,
}

fn validate_step_condition(condition: &Value) -> UnifiedResult<()> {
    let condition_type = condition
        .get("type")
        .and_then(|value| value.as_str())
        .ok_or_else(|| UnifiedError::bad_request("Step condition needs a \"type\""))?;
    if !matches!(
        condition_type,
        "always" | "opened" | "clicked" | "did_not_open" | "did_not_click"
    ) {
        return Err(UnifiedError::bad_request(
            "Step condition type must be one of: always, opened, clicked, did_not_open, did_not_click",
        ));
    }
    if condition_type != "always" {
        let step_id = condition
            .get("step_id")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if step_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Step condition needs a \"step_id\" for branching conditions",
            ));
        }
        parse_uuid(step_id, "condition step")?;
        if let Some(window) = condition.get("window_hours") {
            let hours = window.as_u64().ok_or_else(|| {
                UnifiedError::bad_request("Step condition \"window_hours\" must be a positive integer")
            })?;
            if hours == 0 || hours > 24 * 90 {
                return Err(UnifiedError::bad_request(
                    "Step condition \"window_hours\" must be between 1 and 2160",
                ));
            }
        }
    }
    Ok(())
}

async fn list_steps(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<SequenceStep>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        if !state.sequences.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Sequence not found"));
        }
        Ok(sequence_steps(&state, &id.to_string()))
    })();
    match result {
        Ok(steps) => ok(steps),
        Err(error) => err(error, debug),
    }
}

async fn create_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<StepInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SequenceStep> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        if !state.sequences.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Sequence not found"));
        }
        let input = body.into_inner();
        let subject = require_text(&input.subject, "subject", 500)?;
        if input.body_text.is_none() && input.body_html.is_none() && input.template_id.is_none() {
            return Err(UnifiedError::bad_request(
                "Provide body_text or body_html (or a template_id)",
            ));
        }
        let condition = input.condition.unwrap_or_else(|| json!({ "type": "always" }));
        validate_step_condition(&condition)?;
        let delay_seconds = input.delay_seconds.unwrap_or(0);
        if delay_seconds < 0 || delay_seconds > 366 * 86_400 {
            return Err(UnifiedError::bad_request(
                "delay_seconds must be between 0 and 31622400 (one year)",
            ));
        }
        let order = match input.order {
            Some(order) => {
                if order < 1 {
                    return Err(UnifiedError::bad_request("order must be at least 1"));
                }
                if sequence_steps(&state, &id.to_string())
                    .iter()
                    .any(|step| step.order == order)
                {
                    return Err(UnifiedError::conflict(format!(
                        "Step order {order} is already used in this sequence"
                    )));
                }
                order
            }
            None => {
                sequence_steps(&state, &id.to_string())
                    .iter()
                    .map(|step| step.order)
                    .max()
                    .unwrap_or(0)
                    + 1
            }
        };
        let now = now_string();
        let step = SequenceStep {
            id: new_id(),
            sequence_id: id.to_string(),
            order,
            delay_seconds,
            condition,
            subject,
            body_text: input.body_text,
            body_html: input.body_html,
            template_id: input.template_id.filter(|value| !value.trim().is_empty()),
            created_at: now.clone(),
            updated_at: now,
        };
        state.steps.insert(step.id.clone(), step.clone());
        Ok(step)
    })();
    match result {
        Ok(step) => created(step),
        Err(error) => err(error, debug),
    }
}

async fn update_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<StepUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SequenceStep> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let (sequence_id, step_id) = path.into_inner();
        let sequence_uuid = parse_uuid(&sequence_id, "sequence")?;
        let step_uuid = parse_uuid(&step_id, "step")?;
        if !state.sequences.contains_key(&sequence_uuid.to_string()) {
            return Err(UnifiedError::not_found("Sequence not found"));
        }
        let input = body.into_inner();
        if let Some(condition) = &input.condition {
            validate_step_condition(condition)?;
        }
        if let Some(delay) = input.delay_seconds {
            if delay < 0 || delay > 366 * 86_400 {
                return Err(UnifiedError::bad_request(
                    "delay_seconds must be between 0 and 31622400 (one year)",
                ));
            }
        }
        if let Some(order) = input.order {
            if order < 1 {
                return Err(UnifiedError::bad_request("order must be at least 1"));
            }
            if sequence_steps(&state, &sequence_uuid.to_string())
                .iter()
                .any(|step| step.order == order && step.id != step_uuid.to_string())
            {
                return Err(UnifiedError::conflict(format!(
                    "Step order {order} is already used in this sequence"
                )));
            }
        }
        let mut step = state
            .steps
            .get_mut(&step_uuid.to_string())
            .ok_or_else(|| UnifiedError::not_found("Step not found"))?;
        if step.sequence_id != sequence_uuid.to_string() {
            return Err(UnifiedError::not_found("Step not found"));
        }
        if let Some(order) = input.order {
            step.order = order;
        }
        if let Some(delay) = input.delay_seconds {
            step.delay_seconds = delay;
        }
        if let Some(condition) = input.condition {
            step.condition = condition;
        }
        if let Some(subject) = input.subject {
            step.subject = require_text(&subject, "subject", 500)?;
        }
        if let Some(body_text) = input.body_text {
            step.body_text = Some(body_text);
        }
        if let Some(body_html) = input.body_html {
            step.body_html = Some(body_html);
        }
        if let Some(template_id) = input.template_id {
            step.template_id =
                if template_id.trim().is_empty() { None } else { Some(template_id) };
        }
        step.updated_at = now_string();
        Ok(step.clone())
    })();
    match result {
        Ok(step) => ok(step),
        Err(error) => err(error, debug),
    }
}

async fn delete_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let (sequence_id, step_id) = path.into_inner();
        let sequence_uuid = parse_uuid(&sequence_id, "sequence")?;
        let step_uuid = parse_uuid(&step_id, "step")?;
        let step = state
            .steps
            .get(&step_uuid.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Step not found"))?;
        if step.sequence_id != sequence_uuid.to_string() {
            return Err(UnifiedError::not_found("Step not found"));
        }
        if state
            .sends
            .iter()
            .any(|entry| entry.value().step_id.as_deref() == Some(step_uuid.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Step already has sends recorded; it cannot be deleted",
            ));
        }
        state.steps.remove(&step_uuid.to_string());
        Ok(json!({ "deleted": true, "id": step_uuid.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// --- Enrollment ---

#[derive(Debug, Deserialize)]
struct EnrollInput {
    contact_id: String,
    email: String,
}

async fn enroll_in_sequence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EnrollInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        let input = body.into_inner();
        let contact_id = require_text(&input.contact_id, "contact_id", 200)?;
        if !valid_email(&input.email) {
            return Err(UnifiedError::bad_request("email is not a valid email address"));
        }
        match enroll_contact(&state, &id.to_string(), &contact_id, input.email.trim())? {
            Some(enrollment) => Ok(json!({ "enrolled": true, "enrollment": enrollment })),
            None => Ok(json!({
                "enrolled": false,
                "already_enrolled": true,
                "sequence_id": id.to_string(),
                "contact_id": contact_id,
            })),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_enrollments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Enrollment>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        if !state.sequences.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Sequence not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| {
                enrollment.sequence_id == id.to_string()
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| enrollment.status == *status)
                    && matches_search(&enrollment.email, &params)
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn sequence_funnel(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "sequence")?;
        if !state.sequences.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Sequence not found"));
        }
        let steps = sequence_steps(&state, &id.to_string());
        let enrollments: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| enrollment.sequence_id == id.to_string())
            .collect();
        let total = enrollments.len() as u64;
        let mut funnel: Vec<Value> = Vec::new();
        for step in &steps {
            let sent = state
                .sends
                .iter()
                .filter(|entry| {
                    entry.value().sequence_id.as_deref() == Some(id.as_str())
                        && entry.value().step_id.as_deref() == Some(step.id.as_str())
                        && entry.value().status != "skipped"
                })
                .count() as u64;
            let opened = state
                .sends
                .iter()
                .filter(|entry| {
                    entry.value().sequence_id.as_deref() == Some(id.as_str())
                        && entry.value().step_id.as_deref() == Some(step.id.as_str())
                        && (entry.value().status == "opened" || entry.value().status == "clicked")
                })
                .count() as u64;
            let skipped = state
                .sends
                .iter()
                .filter(|entry| {
                    entry.value().sequence_id.as_deref() == Some(id.as_str())
                        && entry.value().step_id.as_deref() == Some(step.id.as_str())
                        && entry.value().status == "skipped"
                })
                .count() as u64;
            funnel.push(json!({
                "step_id": step.id,
                "step_order": step.order,
                "subject": step.subject,
                "sent": sent,
                "opened": opened,
                "skipped": skipped,
                "conversion_from_enrolled": if total == 0 { 0.0 } else { (sent as f64 / total as f64 * 10000.0).round() / 100.0 },
            }));
        }
        let completed = enrollments
            .iter()
            .filter(|enrollment| enrollment.status == "completed")
            .count() as u64;
        let unsubscribed = enrollments
            .iter()
            .filter(|enrollment| enrollment.status == "unsubscribed")
            .count() as u64;
        Ok(json!({
            "sequence_id": id.to_string(),
            "total_enrolled": total,
            "completed": completed,
            "unsubscribed": unsubscribed,
            "active": total - completed - unsubscribed,
            "steps": funnel,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn process_due_sequences(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let (advanced, waiting, errors) = process_due_enrollments(&state);
        Ok(json!({ "advanced": advanced, "waiting": waiting, "errors": errors }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn advance_enrollment_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let id = parse_uuid(path.as_str(), "enrollment")?;
        let advanced = advance_enrollment(&state, &id.to_string())?;
        let enrollment = state
            .enrollments
            .get(&id.to_string())
            .map(|entry| entry.value().clone());
        Ok(json!({ "advanced": advanced, "enrollment": enrollment }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Scoring handlers.
// ---------------------------------------------------------------------------

async fn list_scores(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ContactScore>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<ContactScore> = state
            .scores
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&format!("{} {}", record.email, record.contact_id), &params)
                    && params.filters.get("hot").is_none_or(|wanted| {
                        record.hot == (wanted == "true")
                    })
                    && params.filters.get("warm").is_none_or(|wanted| {
                        record.warm == (wanted == "true")
                    })
            })
            .collect();
        // Refresh each visible score so decay is applied at read time.
        for record in &mut items {
            let total: i64 = record
                .events
                .iter()
                .map(|event| effective_event_points(&state, event))
                .sum();
            record.score = total;
        }
        items.sort_by(|a, b| b.score.cmp(&a.score));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn get_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ContactScore> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let contact_id = require_text(path.as_str(), "contact_id", 200)?;
        refresh_score(&state, &contact_id);
        state
            .scores
            .get(&contact_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("No score recorded for this contact"))
    })();
    match result {
        Ok(score) => ok(score),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
struct RecordScoreInput {
    action: String,
    email: Option<String>,
}

async fn record_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RecordScoreInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let contact_id = require_text(path.as_str(), "contact_id", 200)?;
        let input = body.into_inner();
        let action = require_text(&input.action, "action", 200)?;
        let email = match &input.email {
            Some(email) => {
                if !valid_email(email) {
                    return Err(UnifiedError::bad_request("email is not a valid email address"));
                }
                email.trim().to_string()
            }
            None => state
                .audience
                .get(&contact_id)
                .map(|entry| entry.value().email.clone())
                .or_else(|| {
                    state
                        .scores
                        .get(&contact_id)
                        .map(|entry| entry.value().email.clone())
                })
                .ok_or_else(|| {
                    UnifiedError::bad_request(
                        "email is required when the contact is not in the known audience",
                    )
                })?,
        };
        let applied = apply_score_action(&state, &contact_id, &email, &action);
        let record = state
            .scores
            .get(&contact_id)
            .map(|entry| entry.value().clone());
        Ok(json!({
            "contact_id": contact_id,
            "action": action,
            "rules_applied": applied,
            "score": record,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_score_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ScoreRule>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<ScoreRule> = state
            .score_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                matches_search(&rule.action, &params)
                    && params.filters.get("action").is_none_or(|action| rule.action == *action)
                    && params.filters.get("active").is_none_or(|wanted| {
                        rule.active == (wanted == "true")
                    })
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
struct ScoreRuleInput {
    action: String,
    points: i64,
    decay_days: Option<u64>,
    active: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ScoreRuleUpdateInput {
    action: Option<String>,
    points: Option<i64>,
    decay_days: Option<u64>,
    active: Option<bool>,
}

async fn create_score_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<ScoreRuleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ScoreRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        let action = require_text(&input.action, "action", 200)?;
        if input.points == 0 || input.points < -1000 || input.points > 1000 {
            return Err(UnifiedError::bad_request(
                "points must be between -1000 and 1000, excluding 0",
            ));
        }
        if let Some(days) = input.decay_days {
            if days == 0 || days > 3650 {
                return Err(UnifiedError::bad_request(
                    "decay_days must be between 1 and 3650",
                ));
            }
        }
        if state
            .score_rules
            .iter()
            .any(|entry| entry.value().action == action && entry.value().active)
            && input.active.unwrap_or(true)
        {
            return Err(UnifiedError::conflict(format!(
                "An active scoring rule already exists for action \"{action}\""
            )));
        }
        let rule = ScoreRule {
            id: new_id(),
            action,
            points: input.points,
            decay_days: input.decay_days.or_else(|| {
                state.settings_snapshot().default_score_decay_days
            }),
            active: input.active.unwrap_or(true),
            created_at: now_string(),
        };
        state.score_rules.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(rule) => created(rule),
        Err(error) => err(error, debug),
    }
}

async fn update_score_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ScoreRuleUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ScoreRule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "score rule")?;
        let input = body.into_inner();
        if let Some(points) = input.points {
            if points == 0 || points < -1000 || points > 1000 {
                return Err(UnifiedError::bad_request(
                    "points must be between -1000 and 1000, excluding 0",
                ));
            }
        }
        if let Some(days) = input.decay_days {
            if days == 0 || days > 3650 {
                return Err(UnifiedError::bad_request(
                    "decay_days must be between 1 and 3650",
                ));
            }
        }
        let mut rule = state
            .score_rules
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Score rule not found"))?;
        if let Some(action) = input.action {
            rule.action = require_text(&action, "action", 200)?;
        }
        if let Some(points) = input.points {
            rule.points = points;
        }
        if let Some(days) = input.decay_days {
            rule.decay_days = Some(days);
        }
        if let Some(active) = input.active {
            rule.active = active;
        }
        Ok(rule.clone())
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}

async fn delete_score_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "score rule")?;
        match state.score_rules.remove(&id.to_string()) {
            Some(_) => Ok(json!({ "deleted": true, "id": id.to_string() })),
            None => Err(UnifiedError::not_found("Score rule not found")),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn apply_decay(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let contact_ids: Vec<String> = state
            .scores
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        for contact_id in &contact_ids {
            refresh_score(&state, contact_id);
        }
        Ok(json!({ "recomputed": contact_ids.len() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Audience, unsubscribe, suppressions, sends, settings.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AudienceSyncInput {
    contact_id: String,
    email: String,
    name: String,
    company: Option<String>,
    phone: Option<String>,
    tags: Option<Vec<String>>,
    source: Option<String>,
    fields: Option<HashMap<String, Value>>,
    /// Consent signal carried by the sync payload: when present it
    /// seeds the consent cache for the marketing purpose.
    consented: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AudienceSyncBody {
    contacts: Vec<AudienceSyncInput>,
}

fn upsert_audience_contact(state: &MarketingState, input: AudienceSyncInput) -> UnifiedResult<AudienceContact> {
    let contact_id = require_text(&input.contact_id, "contact_id", 200)?;
    if !valid_email(&input.email) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address for contact {contact_id}"
        )));
    }
    let name = require_text(&input.name, "name", 200)?;
    let mut tags: Vec<String> = input
        .tags
        .unwrap_or_default()
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    tags.sort();
    tags.dedup();
    if tags.len() > 100 {
        return Err(UnifiedError::bad_request("A contact may have at most 100 tags"));
    }
    let email = input.email.trim().to_lowercase();
    if let Some(granted) = input.consented {
        let settings = state.settings_snapshot();
        state.consent_cache.insert(
            email.clone(),
            ConsentCacheEntry {
                key: email.clone(),
                contact_id: Some(contact_id.clone()),
                granted,
                purpose: settings.consent_purpose,
                source: EVENT_MARKETING_CONTACTS_SYNC.to_string(),
                updated_at: now_string(),
            },
        );
    }
    let contact = AudienceContact {
        contact_id: contact_id.clone(),
        email,
        name,
        company: input.company.filter(|value| !value.trim().is_empty()),
        phone: input.phone.filter(|value| !value.trim().is_empty()),
        tags,
        source: input.source.filter(|value| !value.trim().is_empty()),
        fields: input.fields.unwrap_or_default(),
        updated_at: now_string(),
    };
    state.audience.insert(contact_id, contact.clone());
    Ok(contact)
}

async fn sync_audience(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<AudienceSyncBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        if input.contacts.is_empty() || input.contacts.len() > 1000 {
            return Err(UnifiedError::bad_request(
                "Provide 1 to 1000 contacts per sync request",
            ));
        }
        let mut upserted = 0u64;
        let mut errors: Vec<String> = Vec::new();
        for contact in input.contacts {
            match upsert_audience_contact(&state, contact) {
                Ok(_) => upserted += 1,
                Err(error) => errors.push(error.message),
            }
        }
        Ok(json!({ "upserted": upserted, "errors": errors }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_audience(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AudienceContact>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<AudienceContact> = state
            .audience
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|contact| {
                matches_search(
                    &format!("{} {} {}", contact.name, contact.email, contact.company.clone().unwrap_or_default()),
                    &params,
                ) && params.filters.get("tag").is_none_or(|tag| {
                    contact.tags.iter().any(|item| item == tag)
                }) && params.filters.get("source").is_none_or(|source| {
                    contact.source.as_deref() == Some(source.as_str())
                })
            })
            .collect();
        items.sort_by(|a, b| a.email.cmp(&b.email));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
struct UnsubscribeInput {
    email: String,
    campaign_id: Option<String>,
    reason: Option<String>,
}

/// Record an unsubscribe. Without a campaign id it is global and
/// blocks every future send; with one it blocks only that campaign.
fn record_unsubscribe(
    state: &MarketingState,
    email: &str,
    campaign_id: Option<&str>,
    reason: Option<&str>,
) -> UnifiedResult<Suppression> {
    if !valid_email(email) {
        return Err(UnifiedError::bad_request("email is not a valid email address"));
    }
    let email = email.trim().to_lowercase();
    if let Some(campaign_id) = campaign_id {
        parse_uuid(campaign_id, "campaign")?;
        if !state.campaigns.contains_key(campaign_id) {
            return Err(UnifiedError::not_found("Campaign not found"));
        }
    }
    let record = Suppression {
        id: new_id(),
        email: email.clone(),
        scope: if campaign_id.is_some() { "campaign".to_string() } else { "global".to_string() },
        campaign_id: campaign_id.map(str::to_string),
        reason: reason.map(str::to_string),
        created_at: now_string(),
    };
    state.suppressions.insert(record.id.clone(), record.clone());
    // An unsubscribe also withdraws the cached marketing consent.
    let settings = state.settings_snapshot();
    state.consent_cache.insert(
        email.clone(),
        ConsentCacheEntry {
            key: email.clone(),
            contact_id: None,
            granted: false,
            purpose: settings.consent_purpose,
            source: EVENT_MARKETING_UNSUBSCRIBED.to_string(),
            updated_at: now_string(),
        },
    );
    // Stop any active enrollments for this address immediately.
    let enrollment_ids: Vec<String> = state
        .enrollments
        .iter()
        .filter(|entry| {
            entry.value().email.to_lowercase() == email && entry.value().status == "active"
        })
        .map(|entry| entry.key().clone())
        .collect();
    for enrollment_id in enrollment_ids {
        if let Some(mut enrollment) = state.enrollments.get_mut(&enrollment_id) {
            enrollment.status = "unsubscribed".to_string();
            enrollment.next_step_at = None;
            enrollment.updated_at = now_string();
        }
    }
    state.emit(
        EVENT_MARKETING_UNSUBSCRIBED,
        json!({
            "email": email,
            "campaign_id": campaign_id,
            "reason": reason,
            "suppression_id": record.id,
        }),
    );
    Ok(record)
}

async fn unsubscribe(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<UnsubscribeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Suppression> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        record_unsubscribe(
            &state,
            &input.email,
            input.campaign_id.as_deref(),
            input.reason.as_deref(),
        )
    })();
    match result {
        Ok(record) => created(record),
        Err(error) => err(error, debug),
    }
}

async fn list_suppressions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Suppression>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<Suppression> = state
            .suppressions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&record.email, &params)
                    && params
                        .filters
                        .get("scope")
                        .is_none_or(|scope| record.scope == *scope)
                    && params
                        .filters
                        .get("campaign_id")
                        .is_none_or(|campaign| record.campaign_id.as_deref() == Some(campaign.as_str()))
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn delete_suppression(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "suppression")?;
        match state.suppressions.remove(&id.to_string()) {
            Some((_, record)) => Ok(json!({
                "deleted": true,
                "id": id.to_string(),
                "email": record.email,
            })),
            None => Err(UnifiedError::not_found("Suppression record not found")),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_sends(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<MarketingSend>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<MarketingSend> = state
            .sends
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|send| {
                matches_search(&format!("{} {}", send.email, send.contact_id), &params)
                    && params.filters.get("status").is_none_or(|status| send.status == *status)
                    && params.filters.get("campaign_id").is_none_or(|campaign| {
                        send.campaign_id.as_deref() == Some(campaign.as_str())
                    })
                    && params.filters.get("sequence_id").is_none_or(|sequence| {
                        send.sequence_id.as_deref() == Some(sequence.as_str())
                    })
                    && params.filters.get("contact_id").is_none_or(|contact| {
                        send.contact_id == *contact
                    })
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

async fn get_send(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingSend> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "send")?;
        state
            .sends
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Send record not found"))
    })();
    match result {
        Ok(send) => ok(send),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
struct SettingsInput {
    default_split_percent_a: Option<u8>,
    ab_min_sample_per_variant: Option<u64>,
    ab_winner_metric: Option<String>,
    warm_threshold: Option<i64>,
    hot_threshold: Option<i64>,
    default_score_decay_days: Option<u64>,
    max_sends_per_run: Option<u64>,
    segment_fields: Option<Vec<String>>,
    segment_operators: Option<Vec<String>>,
    score_actions: Option<Vec<String>>,
    consent_purpose: Option<String>,
}

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        Ok(state.settings_snapshot())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<SettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        if let Some(split) = input.default_split_percent_a {
            if split == 0 || split > 100 {
                return Err(UnifiedError::bad_request(
                    "default_split_percent_a must be between 1 and 100",
                ));
            }
        }
        if let Some(min_sample) = input.ab_min_sample_per_variant {
            if min_sample == 0 || min_sample > 100_000 {
                return Err(UnifiedError::bad_request(
                    "ab_min_sample_per_variant must be between 1 and 100000",
                ));
            }
        }
        if let Some(metric) = &input.ab_winner_metric {
            if metric != "open_rate" && metric != "click_rate" {
                return Err(UnifiedError::bad_request(
                    "ab_winner_metric must be \"open_rate\" or \"click_rate\"",
                ));
            }
        }
        if let (Some(warm), Some(hot)) = (input.warm_threshold, input.hot_threshold) {
            if warm >= hot {
                return Err(UnifiedError::bad_request(
                    "warm_threshold must be below hot_threshold",
                ));
            }
        }
        if let Some(max) = input.max_sends_per_run {
            if max == 0 || max > 100_000 {
                return Err(UnifiedError::bad_request(
                    "max_sends_per_run must be between 1 and 100000",
                ));
            }
        }
        if let Some(fields) = &input.segment_fields {
            if fields.is_empty() || fields.len() > 100 {
                return Err(UnifiedError::bad_request(
                    "segment_fields must contain 1 to 100 field names",
                ));
            }
        }
        if let Some(operators) = &input.segment_operators {
            if operators.is_empty() || operators.len() > 50 {
                return Err(UnifiedError::bad_request(
                    "segment_operators must contain 1 to 50 operators",
                ));
            }
        }
        let mut settings = state.settings_snapshot();
        if let Some(split) = input.default_split_percent_a {
            settings.default_split_percent_a = split;
        }
        if let Some(min_sample) = input.ab_min_sample_per_variant {
            settings.ab_min_sample_per_variant = min_sample;
        }
        if let Some(metric) = input.ab_winner_metric {
            settings.ab_winner_metric = metric;
        }
        if let Some(warm) = input.warm_threshold {
            settings.warm_threshold = warm;
        }
        if let Some(hot) = input.hot_threshold {
            settings.hot_threshold = hot;
        }
        if let Some(days) = input.default_score_decay_days {
            settings.default_score_decay_days = Some(days);
        }
        if let Some(max) = input.max_sends_per_run {
            settings.max_sends_per_run = max;
        }
        if let Some(fields) = input.segment_fields {
            settings.segment_fields = fields;
        }
        if let Some(operators) = input.segment_operators {
            settings.segment_operators = operators;
        }
        if let Some(actions) = input.score_actions {
            settings.score_actions = actions;
        }
        if let Some(purpose) = input.consent_purpose {
            settings.consent_purpose = require_text(&purpose, "consent_purpose", 100)?;
        }
        if settings.warm_threshold >= settings.hot_threshold {
            return Err(UnifiedError::bad_request(
                "warm_threshold must be below hot_threshold",
            ));
        }
        {
            let mut stored = state
                .settings
                .write()
                .expect("marketing settings lock poisoned");
            *stored = settings.clone();
        }
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Hook listeners.
// ---------------------------------------------------------------------------

fn hook_mail_outcome(state: &Arc<MarketingState>, ctx: &mut HookContext, delivered: bool) {
    let job_id = ctx
        .get("job_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if job_id.is_empty() {
        return;
    }
    let mut updated: Vec<String> = Vec::new();
    for entry in state.sends.iter() {
        let send = entry.value();
        if send.mail_job_id.as_deref() == Some(job_id.as_str()) && send.status == "queued" {
            updated.push(send.id.clone());
        }
    }
    for send_id in updated {
        let new_status = if delivered { "sent" } else { "failed" };
        let campaign_id = state
            .sends
            .get(&send_id)
            .and_then(|entry| entry.value().campaign_id.clone());
        if let Some(mut send) = state.sends.get_mut(&send_id) {
            send.status = new_status.to_string();
            send.updated_at = now_string();
        }
        if let Some(campaign_id) = campaign_id {
            if let Some(mut campaign) = state.campaigns.get_mut(&campaign_id) {
                if delivered {
                    campaign.stats.sent += 1;
                    campaign.stats.delivered += 1;
                } else {
                    campaign.stats.failed += 1;
                }
            }
        }
    }
}

fn hook_tracking(state: &Arc<MarketingState>, ctx: &mut HookContext, kind: &str) {
    let campaign_id = ctx
        .get("campaign_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if campaign_id.is_empty() || !state.campaigns.contains_key(&campaign_id) {
        return;
    }
    let contact_ref = ctx
        .get("subscriber_id")
        .or_else(|| ctx.get("contact_id"))
        .or_else(|| ctx.get("email"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let url = ctx
        .get("url")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let event = TrackingEvent {
        id: new_id(),
        campaign_id: campaign_id.clone(),
        contact_ref: contact_ref.clone(),
        kind: kind.to_string(),
        url,
        created_at: now_string(),
    };
    state
        .tracking_events
        .insert(event.id.clone(), event.clone());
    // Mark the matching send record opened/clicked when the tracking
    // reference identifies one (newsletter subscriber ids do not map
    // to marketing sends, so this is best-effort only). The match is
    // collected first and mutated afterwards: holding a read guard
    // on the sends map while taking a write guard on the same shard
    // would deadlock.
    let matched_send: Option<MarketingSend> = state
        .sends
        .iter()
        .map(|entry| entry.value().clone())
        .find(|send| {
            send.campaign_id.as_deref() == Some(campaign_id.as_str())
                && (send.contact_id == contact_ref || send.email == contact_ref)
                && (send.status == "sent" || send.status == "queued")
        });
    if let Some(send) = matched_send {
        if let Some(mut stored) = state.sends.get_mut(&send.id) {
            stored.status = kind.to_string();
            stored.updated_at = now_string();
        }
        if let Some(variant_id) = &send.variant_id {
            if let Some(mut variant) = state.variants.get_mut(variant_id) {
                if kind == "open" {
                    variant.opened += 1;
                } else {
                    variant.clicked += 1;
                }
            }
        }
    }
    // Campaign open/click counters are not incremented here: the
    // stats endpoint recomputes them from the stored tracking
    // events, which are the single source of truth.
    // Score the engagement only when the tracking reference is an
    // email address. Newsletter subscriber ids do not map to a
    // marketing contact, and inventing a score record for one would
    // pollute the leaderboard with an unreachable identity.
    if contact_ref.contains('@') {
        apply_score_action(
            state,
            &contact_ref,
            &contact_ref,
            if kind == "open" { "email.opened" } else { "email.clicked" },
        );
    }
}

fn hook_consent(state: &Arc<MarketingState>, ctx: &mut HookContext, granted: bool, source: &str) {
    let settings = state.settings_snapshot();
    let email = ctx
        .get("email")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let contact_id = ctx
        .get("contact_id")
        .or_else(|| ctx.get("user_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let purpose = ctx
        .get("purpose")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if !purpose.is_empty() && purpose != settings.consent_purpose {
        return;
    }
    if email.is_empty() && contact_id.is_none() {
        return;
    }
    let key = if email.is_empty() {
        contact_id.clone().unwrap_or_default().to_lowercase()
    } else {
        email.to_lowercase()
    };
    state.consent_cache.insert(
        key.clone(),
        ConsentCacheEntry {
            key,
            contact_id,
            granted,
            purpose: settings.consent_purpose,
            source: source.to_string(),
            updated_at: now_string(),
        },
    );
}

/// Enrollment triggers from other modules. The payloads are read
/// defensively because each source carries different fields:
/// - `forms.submission.received` carries only `form_id`,
///   `submission_id` and `slug`: it names no submitter, so a form
///   trigger can only enroll when the payload (or a later
///   `marketing.sequence.enroll` event, or the manual enroll
///   endpoint) supplies the contact. The listener still records the
///   form-submitted score action when the contact is known.
/// - `store.order.completed` carries the order itself (`user_id`,
///   no email): the email is resolved from the known audience.
/// - `tag_added` triggers arrive through `marketing.sequence.enroll`
///   with an explicit tag; no module currently emits a tag event.
fn hook_sequence_trigger(state: &Arc<MarketingState>, ctx: &HookContext, trigger: &str) {
    let order = ctx
        .get("order")
        .cloned()
        .unwrap_or_else(|| ctx.data.clone());
    let email = ctx
        .get("email")
        .or_else(|| order.get("email"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let contact_id = ctx
        .get("contact_id")
        .or_else(|| ctx.get("user_id"))
        .or_else(|| order.get("user_id"))
        .or_else(|| order.get("customer_user_id"))
        .or_else(|| order.get("contact_id"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let form_id = ctx
        .get("form_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let tag = ctx
        .get("tag")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    // Resolve the email from the known audience when only the
    // contact id arrived (the store order case).
    let email = if email.is_empty() && !contact_id.is_empty() {
        state
            .audience
            .get(&contact_id)
            .map(|entry| entry.value().email.clone())
            .unwrap_or_default()
    } else {
        email
    };
    if email.is_empty() || contact_id.is_empty() || !valid_email(&email) {
        return;
    }
    // Keep a snapshot of the contact so segments can see them.
    let name = ctx
        .get("name")
        .or_else(|| order.get("name"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if !name.is_empty() {
        let _ = upsert_audience_contact(
            state,
            AudienceSyncInput {
                contact_id: contact_id.clone(),
                email: email.clone(),
                name,
                company: ctx
                    .get("company")
                    .or_else(|| order.get("company"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                phone: None,
                tags: None,
                source: Some(trigger.to_string()),
                fields: None,
                consented: None,
            },
        );
    }
    let sequences: Vec<Sequence> = state
        .sequences
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|sequence| {
            sequence.status == "active"
                && match sequence.trigger_kind.as_str() {
                    "form_submitted" => {
                        trigger == "form_submitted"
                            && sequence.trigger_form_id.as_deref() == Some(form_id.as_str())
                    }
                    "purchase" => trigger == "purchase",
                    "tag_added" => {
                        trigger == "tag_added"
                            && sequence.trigger_tag.as_deref() == Some(tag.as_str())
                    }
                    _ => false,
                }
        })
        .collect();
    for sequence in sequences {
        let _ = enroll_contact(state, &sequence.id, &contact_id, &email);
    }
    if trigger == "purchase" {
        apply_score_action(state, &contact_id, &email, "purchase.completed");
    } else if trigger == "form_submitted" {
        apply_score_action(state, &contact_id, &email, "form.submitted");
    }
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct MarketingModule {
    state: Arc<MarketingState>,
}

impl MarketingModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(MarketingState::new()),
        }
    }
}

impl Default for MarketingModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(MarketingModule::new()));
}

impl Module for MarketingModule {
    fn id(&self) -> &'static str {
        "marketing"
    }

    fn name(&self) -> &'static str {
        "Marketing Automation"
    }

    fn description(&self) -> &'static str {
        "Marketing automation: campaigns with A/B testing, dynamic segments over CRM contacts, drip sequences with branching, lead scoring with decay and hot-lead handoff, send analytics, and fail-closed consent and unsubscribe handling."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["mail", "newsletter", "crm", "compliance"]
    }

    fn requires(&self) -> &[&'static str] {
        &["mail", "newsletter", "crm", "compliance"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["dashmap", "serde_json", "chrono", "uuid"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_MAIL_SENT,
            "marketing",
            Arc::new(move |ctx| hook_mail_outcome(&state, ctx, true)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MAIL_FAILED,
            "marketing",
            Arc::new(move |ctx| hook_mail_outcome(&state, ctx, false)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_NEWSLETTER_OPEN,
            "marketing",
            Arc::new(move |ctx| hook_tracking(&state, ctx, "open")),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_NEWSLETTER_CLICK,
            "marketing",
            Arc::new(move |ctx| hook_tracking(&state, ctx, "click")),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_NEWSLETTER_UNSUBSCRIBED,
            "marketing",
            Arc::new(move |ctx| {
                let email = ctx
                    .get("email")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if !email.is_empty() {
                    let _ = record_unsubscribe(&state, &email, None, Some("newsletter unsubscribe"));
                }
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_COMPLIANCE_CONSENT_RECORDED,
            "marketing",
            Arc::new(move |ctx| {
                // The compliance module records consent with this
                // event name and carries the decision in `granted`.
                let granted = ctx
                    .get("granted")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true);
                hook_consent(&state, ctx, granted, EVENT_COMPLIANCE_CONSENT_RECORDED);
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_COMPLIANCE_CONSENT_GRANTED,
            "marketing",
            Arc::new(move |ctx| hook_consent(&state, ctx, true, EVENT_COMPLIANCE_CONSENT_GRANTED)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_COMPLIANCE_CONSENT_WITHDRAWN,
            "marketing",
            Arc::new(move |ctx| hook_consent(&state, ctx, false, EVENT_COMPLIANCE_CONSENT_WITHDRAWN)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_PRIVACY_CONSENT_CHANGED,
            "marketing",
            Arc::new(move |ctx| {
                let granted = ctx
                    .get("granted")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                hook_consent(&state, ctx, granted, EVENT_PRIVACY_CONSENT_CHANGED);
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_FORMS_SUBMISSION_RECEIVED,
            "marketing",
            Arc::new(move |ctx| hook_sequence_trigger(&state, ctx, "form_submitted")),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_STORE_ORDER_COMPLETED,
            "marketing",
            Arc::new(move |ctx| hook_sequence_trigger(&state, ctx, "purchase")),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_CAMPAIGN_SEND,
            "marketing",
            Arc::new(move |ctx| {
                let campaign_id = ctx
                    .get("campaign_id")
                    .and_then(|value| value.as_str())
                    .or_else(|| {
                        ctx.get("payload")
                            .and_then(|payload| payload.get("campaign_id"))
                            .and_then(|value| value.as_str())
                    })
                    .unwrap_or("")
                    .to_string();
                if campaign_id.is_empty() {
                    ctx.add_result(json!({ "error": "marketing.campaign.send requires 'campaign_id'" }));
                    return;
                }
                match execute_campaign_send(&state, &campaign_id) {
                    Ok(result) => ctx.add_result(result),
                    Err(error) => ctx.add_result(json!({ "error": error.message })),
                }
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_SEQUENCE_PROCESS_DUE,
            "marketing",
            Arc::new(move |ctx| {
                let (advanced, waiting, errors) = process_due_enrollments(&state);
                ctx.add_result(json!({ "advanced": advanced, "waiting": waiting, "errors": errors }));
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_SEQUENCE_ENROLL,
            "marketing",
            Arc::new(move |ctx| {
                let sequence_id = ctx
                    .get("sequence_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let contact_id = ctx
                    .get("contact_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let email = ctx
                    .get("email")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if sequence_id.is_empty() || contact_id.is_empty() || email.is_empty() {
                    ctx.add_result(json!({
                        "error": "marketing.sequence.enroll requires 'sequence_id', 'contact_id' and 'email'"
                    }));
                    return;
                }
                match enroll_contact(&state, &sequence_id, &contact_id, &email) {
                    Ok(Some(enrollment)) => {
                        ctx.add_result(json!({ "enrolled": true, "enrollment_id": enrollment.id }));
                    }
                    Ok(None) => {
                        ctx.add_result(json!({ "enrolled": false, "already_enrolled": true }));
                    }
                    Err(error) => ctx.add_result(json!({ "error": error.message })),
                }
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_CONTACTS_SYNC,
            "marketing",
            Arc::new(move |ctx| {
                let contacts = ctx
                    .get("contacts")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mut upserted = 0u64;
                for raw in contacts {
                    if let Ok(input) = serde_json::from_value::<AudienceSyncInput>(raw) {
                        if upsert_audience_contact(&state, input).is_ok() {
                            upserted += 1;
                        }
                    }
                }
                ctx.add_result(json!({ "upserted": upserted }));
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_UNSUBSCRIBE,
            "marketing",
            Arc::new(move |ctx| {
                let email = ctx
                    .get("email")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if email.is_empty() {
                    ctx.add_result(json!({ "error": "marketing.unsubscribe requires 'email'" }));
                    return;
                }
                let campaign_id = ctx
                    .get("campaign_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                match record_unsubscribe(&state, &email, campaign_id.as_deref(), None) {
                    Ok(record) => ctx.add_result(json!({ "unsubscribed": true, "suppression_id": record.id })),
                    Err(error) => ctx.add_result(json!({ "error": error.message })),
                }
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_SCORE_RECORD,
            "marketing",
            Arc::new(move |ctx| {
                let contact_id = ctx
                    .get("contact_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let email = ctx
                    .get("email")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let action = ctx
                    .get("action")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if contact_id.is_empty() || email.is_empty() || action.is_empty() {
                    ctx.add_result(json!({
                        "error": "marketing.score.record requires 'contact_id', 'email' and 'action'"
                    }));
                    return;
                }
                let applied = apply_score_action(&state, &contact_id, &email, &action);
                ctx.add_result(json!({ "rules_applied": applied }));
            }),
        );
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/marketing/campaigns", owner: "marketing", description: "List campaigns" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns", owner: "marketing", description: "Create campaign" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/campaigns/{id}", owner: "marketing", description: "Get campaign" },
            RouteInfo { method: "PUT", path: "/api/v1/marketing/campaigns/{id}", owner: "marketing", description: "Update draft campaign" },
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/campaigns/{id}", owner: "marketing", description: "Delete draft campaign" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/schedule", owner: "marketing", description: "Schedule campaign" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/send-now", owner: "marketing", description: "Send campaign now" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/pause", owner: "marketing", description: "Pause campaign" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/resume", owner: "marketing", description: "Resume campaign" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/campaigns/{id}/stats", owner: "marketing", description: "Campaign stats" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/campaigns/{id}/variants", owner: "marketing", description: "List A/B variants" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/variants", owner: "marketing", description: "Create A/B variant" },
            RouteInfo { method: "PUT", path: "/api/v1/marketing/campaigns/{id}/variants/{variant_id}", owner: "marketing", description: "Update A/B variant" },
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/campaigns/{id}/variants/{variant_id}", owner: "marketing", description: "Delete A/B variant" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/ab-decide", owner: "marketing", description: "Pick A/B winner from real data" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/segments", owner: "marketing", description: "List segments" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/segments", owner: "marketing", description: "Create segment" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/segments/{id}", owner: "marketing", description: "Get segment" },
            RouteInfo { method: "PUT", path: "/api/v1/marketing/segments/{id}", owner: "marketing", description: "Update segment" },
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/segments/{id}", owner: "marketing", description: "Delete segment" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/segments/{id}/evaluate", owner: "marketing", description: "Evaluate segment membership" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/segments/{id}/members", owner: "marketing", description: "List segment members" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/segments/{id}/history", owner: "marketing", description: "Segment size history" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/sequences", owner: "marketing", description: "List sequences" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/sequences", owner: "marketing", description: "Create sequence" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/sequences/{id}", owner: "marketing", description: "Get sequence" },
            RouteInfo { method: "PUT", path: "/api/v1/marketing/sequences/{id}", owner: "marketing", description: "Update sequence" },
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/sequences/{id}", owner: "marketing", description: "Delete sequence" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/sequences/{id}/steps", owner: "marketing", description: "List sequence steps" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/sequences/{id}/steps", owner: "marketing", description: "Add sequence step" },
            RouteInfo { method: "PUT", path: "/api/v1/marketing/sequences/{id}/steps/{step_id}", owner: "marketing", description: "Update sequence step" },
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/sequences/{id}/steps/{step_id}", owner: "marketing", description: "Delete sequence step" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/sequences/{id}/enroll", owner: "marketing", description: "Enroll contact in sequence" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/sequences/{id}/enrollments", owner: "marketing", description: "List sequence enrollments" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/sequences/{id}/funnel", owner: "marketing", description: "Sequence funnel" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/sequences/process-due", owner: "marketing", description: "Run due sequence steps" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/enrollments/{id}/advance", owner: "marketing", description: "Advance one enrollment" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/scores", owner: "marketing", description: "List contact scores" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/scores/{contact_id}", owner: "marketing", description: "Get contact score" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/scores/{contact_id}/record", owner: "marketing", description: "Record scoring action" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/score-rules", owner: "marketing", description: "List scoring rules" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/score-rules", owner: "marketing", description: "Create scoring rule" },
            RouteInfo { method: "PUT", path: "/api/v1/marketing/score-rules/{id}", owner: "marketing", description: "Update scoring rule" },
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/score-rules/{id}", owner: "marketing", description: "Delete scoring rule" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/scores/apply-decay", owner: "marketing", description: "Recompute scores with decay" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/audience/sync", owner: "marketing", description: "Sync audience contacts" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/audience", owner: "marketing", description: "List audience contacts" },
            RouteInfo { method: "POST", path: "/api/v1/marketing/unsubscribe", owner: "marketing", description: "Record unsubscribe" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/suppressions", owner: "marketing", description: "List suppressions" },
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/suppressions/{id}", owner: "marketing", description: "Delete suppression" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/sends", owner: "marketing", description: "List send audit trail" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/sends/{id}", owner: "marketing", description: "Get send record" },
            RouteInfo { method: "GET", path: "/api/v1/marketing/settings", owner: "marketing", description: "Get marketing settings" },
            RouteInfo { method: "PUT", path: "/api/v1/marketing/settings", owner: "marketing", description: "Update marketing settings" },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/marketing")
                .app_data(web::Data::new(state))
                .route("/campaigns", web::get().to(list_campaigns))
                .route("/campaigns", web::post().to(create_campaign))
                .route("/campaigns/{id}", web::get().to(get_campaign))
                .route("/campaigns/{id}", web::put().to(update_campaign))
                .route("/campaigns/{id}", web::delete().to(delete_campaign))
                .route("/campaigns/{id}/schedule", web::post().to(schedule_campaign))
                .route("/campaigns/{id}/send-now", web::post().to(send_campaign_now))
                .route("/campaigns/{id}/pause", web::post().to(pause_campaign))
                .route("/campaigns/{id}/resume", web::post().to(resume_campaign))
                .route("/campaigns/{id}/stats", web::get().to(get_campaign_stats))
                .route("/campaigns/{id}/variants", web::get().to(list_variants))
                .route("/campaigns/{id}/variants", web::post().to(create_variant))
                .route("/campaigns/{id}/variants/{variant_id}", web::put().to(update_variant))
                .route("/campaigns/{id}/variants/{variant_id}", web::delete().to(delete_variant))
                .route("/campaigns/{id}/ab-decide", web::post().to(ab_decide))
                .route("/segments", web::get().to(list_segments))
                .route("/segments", web::post().to(create_segment))
                .route("/segments/{id}", web::get().to(get_segment))
                .route("/segments/{id}", web::put().to(update_segment))
                .route("/segments/{id}", web::delete().to(delete_segment))
                .route("/segments/{id}/evaluate", web::post().to(evaluate_segment_endpoint))
                .route("/segments/{id}/members", web::get().to(list_segment_members))
                .route("/segments/{id}/history", web::get().to(list_segment_history))
                .route("/sequences", web::get().to(list_sequences))
                .route("/sequences", web::post().to(create_sequence))
                .route("/sequences/{id}", web::get().to(get_sequence))
                .route("/sequences/{id}", web::put().to(update_sequence))
                .route("/sequences/{id}", web::delete().to(delete_sequence))
                .route("/sequences/{id}/steps", web::get().to(list_steps))
                .route("/sequences/{id}/steps", web::post().to(create_step))
                .route("/sequences/{id}/steps/{step_id}", web::put().to(update_step))
                .route("/sequences/{id}/steps/{step_id}", web::delete().to(delete_step))
                .route("/sequences/{id}/enroll", web::post().to(enroll_in_sequence))
                .route("/sequences/{id}/enrollments", web::get().to(list_enrollments))
                .route("/sequences/{id}/funnel", web::get().to(sequence_funnel))
                .route("/sequences/process-due", web::post().to(process_due_sequences))
                .route("/enrollments/{id}/advance", web::post().to(advance_enrollment_endpoint))
                .route("/scores", web::get().to(list_scores))
                .route("/scores/{contact_id}", web::get().to(get_score))
                .route("/scores/{contact_id}/record", web::post().to(record_score))
                .route("/score-rules", web::get().to(list_score_rules))
                .route("/score-rules", web::post().to(create_score_rule))
                .route("/score-rules/{id}", web::put().to(update_score_rule))
                .route("/score-rules/{id}", web::delete().to(delete_score_rule))
                .route("/scores/apply-decay", web::post().to(apply_decay))
                .route("/audience/sync", web::post().to(sync_audience))
                .route("/audience", web::get().to(list_audience))
                .route("/unsubscribe", web::post().to(unsubscribe))
                .route("/suppressions", web::get().to(list_suppressions))
                .route("/suppressions/{id}", web::delete().to(delete_suppression))
                .route("/sends", web::get().to(list_sends))
                .route("/sends/{id}", web::get().to(get_send))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }
}
