// Diesel models for the marketing domain of the unified-db-commerce bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Marketing automation (migration 000085_marketing).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_campaigns)]
pub struct MarketingCampaign {
    pub id: Uuid,
    pub name: String,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub template_id: Option<String>,
    pub segment_id: Option<Uuid>,
    pub newsletter_list_id: Option<String>,
    pub status: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub ab_enabled: bool,
    pub ab_winner_metric: String,
    pub ab_min_sample: Option<i64>,
    pub winning_variant_id: Option<Uuid>,
    pub stats_queued: i64,
    pub stats_sent: i64,
    pub stats_delivered: i64,
    pub stats_failed: i64,
    pub stats_skipped: i64,
    pub stats_opened: i64,
    pub stats_clicked: i64,
    pub stats_unsubscribed: i64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_campaigns)]
pub struct NewMarketingCampaign {
    pub id: Uuid,
    pub name: String,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub template_id: Option<String>,
    pub segment_id: Option<Uuid>,
    pub newsletter_list_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub ab_min_sample: Option<i64>,
    pub winning_variant_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_campaign_variants)]
pub struct MarketingCampaignVariant {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub name: String,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub weight_percent: i16,
    pub sent: i64,
    pub opened: i64,
    pub clicked: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_campaign_variants)]
pub struct NewMarketingCampaignVariant {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub name: String,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_segments)]
pub struct MarketingSegment {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub rule_tree: Value,
    pub status: String,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub last_size: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_segments)]
pub struct NewMarketingSegment {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub last_evaluated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_segment_size_history)]
pub struct MarketingSegmentSizeHistory {
    pub id: Uuid,
    pub segment_id: Uuid,
    pub size: i64,
    pub evaluated_contacts: i64,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_segment_size_history)]
pub struct NewMarketingSegmentSizeHistory {
    pub id: Uuid,
    pub segment_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_sequences)]
pub struct MarketingSequence {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub trigger_kind: String,
    pub trigger_form_id: Option<String>,
    pub trigger_tag: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_sequences)]
pub struct NewMarketingSequence {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub trigger_form_id: Option<String>,
    pub trigger_tag: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_sequence_steps)]
pub struct MarketingSequenceStep {
    pub id: Uuid,
    pub sequence_id: Uuid,
    pub step_order: i64,
    pub delay_seconds: i64,
    pub condition: Value,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub template_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_sequence_steps)]
pub struct NewMarketingSequenceStep {
    pub id: Uuid,
    pub sequence_id: Uuid,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub template_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_enrollments)]
pub struct MarketingEnrollment {
    pub id: Uuid,
    pub sequence_id: Uuid,
    pub contact_id: String,
    pub email: String,
    pub next_step: i64,
    pub status: String,
    pub next_step_at: Option<DateTime<Utc>>,
    pub history: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_enrollments)]
pub struct NewMarketingEnrollment {
    pub id: Uuid,
    pub sequence_id: Uuid,
    pub contact_id: String,
    pub email: String,
    pub next_step_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_scores)]
pub struct MarketingScore {
    pub contact_id: String,
    pub email: String,
    pub score: i64,
    pub warm: bool,
    pub hot: bool,
    pub hot_notified_at: Option<DateTime<Utc>>,
    pub last_action_at: Option<DateTime<Utc>>,
    pub events: Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_scores)]
pub struct NewMarketingScore {
    pub contact_id: String,
    pub email: String,
    pub hot_notified_at: Option<DateTime<Utc>>,
    pub last_action_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_score_rules)]
pub struct MarketingScoreRule {
    pub id: Uuid,
    pub action: String,
    pub points: i64,
    pub decay_days: Option<i64>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_score_rules)]
pub struct NewMarketingScoreRule {
    pub id: Uuid,
    pub action: String,
    pub decay_days: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = marketing_sends)]
pub struct MarketingSend {
    pub id: Uuid,
    pub campaign_id: Option<Uuid>,
    pub sequence_id: Option<Uuid>,
    pub step_id: Option<Uuid>,
    pub enrollment_id: Option<Uuid>,
    pub contact_id: String,
    pub email: String,
    pub variant_id: Option<Uuid>,
    pub status: String,
    pub mail_job_id: Option<String>,
    pub skip_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = marketing_sends)]
pub struct NewMarketingSend {
    pub id: Uuid,
    pub campaign_id: Option<Uuid>,
    pub sequence_id: Option<Uuid>,
    pub step_id: Option<Uuid>,
    pub enrollment_id: Option<Uuid>,
    pub contact_id: String,
    pub email: String,
    pub variant_id: Option<Uuid>,
    pub mail_job_id: Option<String>,
    pub skip_reason: Option<String>,
}
