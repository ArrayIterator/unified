// Marketing Automation - Public record, request, and view types.

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

use super::constants::*;
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

// Types.

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
    pub(crate) fn view(&self) -> CampaignStatsView {
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
