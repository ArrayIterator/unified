//! Social marketing types: admin-defined networks,
//! connected accounts, scheduled posts, campaigns, and
//! analytics metrics, plus the pure functions (the post
//! and campaign status machines, schedule validation,
//! hashtag / media / metric validation, token-expiry math,
//! and the metric rollup math).
//!
//! Nothing about any social network is hard-coded:
//! networks (API base, auth type, publish path, supported
//! post types, text and media limits, rate limit), accounts
//! (handle, write-only encrypted credential, token expiry),
//! posts (type, text, media references, link, hashtags),
//! campaigns (goal as data), and metric names are all data
//! rows an administrator enters. No real network name or
//! credential is baked in — the only provider kinds are
//! the generic `http` adapter (POST JSON to the configured
//! endpoint with the configured auth) and the `log_only`
//! test provider, which records a publish without any
//! network call. Live publishing depends entirely on a
//! configured network with real credentials; this module
//! seeds no network, no account, and no credential. No
//! money is stored or returned anywhere in this module, so
//! `f64` never appears.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_URL_LEN: usize = 2000;

pub(crate) const MAX_HANDLE_LEN: usize = 100;

pub(crate) const MAX_SLUG_LEN: usize = 100;

pub(crate) const MAX_POST_TEXT_LEN: usize = 100_000;

pub(crate) const MAX_HASHTAG_LEN: usize = 50;

pub(crate) const MAX_HASHTAGS_PER_POST: usize = 30;

pub(crate) const MAX_MEDIA_PER_POST: usize = 10;

pub(crate) const MAX_REFERENCE_LEN: usize = 200;

pub(crate) const MAX_METRIC_NAME_LEN: usize = 64;

pub(crate) const MAX_ACCOUNTS_PER_POST: usize = 50;

/// Rate-limit window in seconds (one minute).
pub(crate) const RATE_LIMIT_WINDOW_SECS: i64 = 60;

/// Default per-network publishes per minute on create.
pub(crate) const DEFAULT_NETWORK_POSTS_PER_MINUTE: i64 = 30;

/// Default maximum post text length on a network row when
/// the admin leaves it unset.
pub(crate) const DEFAULT_MAX_TEXT_LENGTH: i64 = 500;

/// Default maximum media items per post on a network row.
pub(crate) const DEFAULT_MAX_MEDIA_ITEMS: i64 = 4;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_SOCIAL_VIEW: &str = "social_marketing.view";

pub(crate) const PERM_SOCIAL_MANAGE: &str = "social_marketing.manage";

/// Hook event names this module emits.
pub(crate) const EVENT_ACCOUNT_CONNECTED: &str = "social.account.connected";

pub(crate) const EVENT_ACCOUNT_DISCONNECTED: &str = "social.account.disconnected";

pub(crate) const EVENT_POST_SCHEDULED: &str = "social.post.scheduled";

pub(crate) const EVENT_POST_PUBLISHED: &str = "social.post.published";

pub(crate) const EVENT_POST_FAILED: &str = "social.post.failed";

pub(crate) const EVENT_CAMPAIGN_CREATED: &str = "social.campaign.created";

/// Hook event names this module listens to.
pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

pub(crate) const EVENT_MARKETING_CAMPAIGN_CREATED: &str = "marketing.campaign.created";

pub(crate) const EVENT_MARKETING_CAMPAIGN_SENT: &str = "marketing.campaign.sent";

/// Marker written into `created_by` when the owning user
/// is deleted. Rows are kept; only the identity is
/// anonymized.
pub(crate) const DELETED_USER_ID: &str = "deleted-user";

/// Metric source recorded for rows that arrive through the
/// provider verification-token webhook.
pub(crate) const METRIC_SOURCE_WEBHOOK: &str = "webhook";

/// Metric source recorded for rows entered by hand through
/// the authenticated API.
pub(crate) const METRIC_SOURCE_MANUAL: &str = "manual";

/// The conventional per-post metric names. These are the
/// shape of the rollup struct, not a closed vocabulary:
/// metric rows may carry any slug name, and names outside
/// this list roll up into `other`.
pub(crate) const KNOWN_METRICS: &[&str] = &["impressions", "likes", "shares", "comments", "clicks"];

// ---------------------------------------------------------------------------
// Status machines (pure functions, unit-tested below)
// ---------------------------------------------------------------------------

/// Post status: `draft` -> `scheduled` -> `publishing` ->
/// `published`, with `failed` and `cancelled` as side
/// states. A failed post may be retried (back to
/// `publishing`); a published or cancelled post is final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostStatus {
    Draft,
    Scheduled,
    Publishing,
    Published,
    Failed,
    Cancelled,
}

impl PostStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Scheduled => "scheduled",
            Self::Publishing => "publishing",
            Self::Published => "published",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "scheduled" => Some(Self::Scheduled),
            "publishing" => Some(Self::Publishing),
            "published" => Some(Self::Published),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Allowed post transitions. Scheduling is only from draft;
/// publishing is only from draft, scheduled, or failed
/// (retry); cancelling is only from draft or scheduled; a
/// published or cancelled post never moves again.
pub fn post_can_transition(from: PostStatus, to: PostStatus) -> bool {
    matches!(
        (from, to),
        (PostStatus::Draft, PostStatus::Scheduled)
            | (PostStatus::Draft, PostStatus::Publishing)
            | (PostStatus::Draft, PostStatus::Cancelled)
            | (PostStatus::Scheduled, PostStatus::Publishing)
            | (PostStatus::Scheduled, PostStatus::Cancelled)
            | (PostStatus::Publishing, PostStatus::Published)
            | (PostStatus::Publishing, PostStatus::Failed)
            | (PostStatus::Failed, PostStatus::Publishing)
            | (PostStatus::Failed, PostStatus::Cancelled)
    )
}

/// Campaign status: `draft` -> `active` -> `completed`,
/// with `paused` and `cancelled` as side states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignStatus {
    Draft,
    Active,
    Paused,
    Completed,
    Cancelled,
}

impl CampaignStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Allowed campaign transitions. A completed or cancelled
/// campaign is final; pausing is only from active;
/// resuming is only from paused.
pub fn campaign_can_transition(from: CampaignStatus, to: CampaignStatus) -> bool {
    matches!(
        (from, to),
        (CampaignStatus::Draft, CampaignStatus::Active)
            | (CampaignStatus::Draft, CampaignStatus::Cancelled)
            | (CampaignStatus::Active, CampaignStatus::Paused)
            | (CampaignStatus::Active, CampaignStatus::Completed)
            | (CampaignStatus::Active, CampaignStatus::Cancelled)
            | (CampaignStatus::Paused, CampaignStatus::Active)
            | (CampaignStatus::Paused, CampaignStatus::Cancelled)
    )
}

// ---------------------------------------------------------------------------
// Schedule and token validation (pure functions)
// ---------------------------------------------------------------------------

/// True when `send_at` is strictly in the future relative
/// to `now`. A scheduled post must name a future time;
/// publishing "now" goes through the publish-now endpoint
/// instead of a past or present schedule.
pub fn is_future_schedule(send_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    send_at > now
}

/// True when the account token is expired at `now`: an
/// expiry in the past (or exactly now) means expired. An
/// account with no expiry never expires.
pub fn token_expired(expires_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    match expires_at {
        Some(expires_at) => expires_at <= now,
        None => false,
    }
}

/// True when the account token expires within
/// `within_seconds` of `now` (or already expired). An
/// account with no expiry is never expiring soon.
pub fn token_expiring_within(
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    within_seconds: i64,
) -> bool {
    match expires_at {
        Some(expires_at) => expires_at <= now + chrono::Duration::seconds(within_seconds),
        None => false,
    }
}

/// True when `url` is an HTTPS URL with a non-empty host.
/// Network API bases must be HTTPS; plain HTTP and
/// host-less values are refused at create / update time.
pub fn is_https_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.strip_suffix(':').unwrap_or(host);
    !host.is_empty() && !host.contains(' ')
}

/// True when `slug` is a valid network slug: 1-100
/// characters, lowercase letters, digits, or hyphens, and
/// not starting or ending with a hyphen.
pub fn is_valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return false;
    }
    slug.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// True when `metric` is a valid metric name: 1-64
/// characters, lowercase letters, digits, or underscores,
/// starting with a letter or underscore.
pub fn is_valid_metric_name(metric: &str) -> bool {
    if metric.is_empty() || metric.len() > MAX_METRIC_NAME_LEN {
        return false;
    }
    let first = metric.as_bytes()[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    metric
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// True when `post_type` is one of the post types the
/// network row allows. Post types are data: the allowed
/// list comes from the network row, never from code.
pub fn post_type_allowed(post_type: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|allowed| allowed == post_type)
}

// ---------------------------------------------------------------------------
// Hashtag and media validation (pure functions)
// ---------------------------------------------------------------------------

/// Normalize one hashtag: strip a leading `#`, trim, and
/// lower-case. Returns `None` when nothing valid remains
/// or the tag holds characters outside letters, digits,
/// and underscores.
pub fn normalize_hashtag(raw: &str) -> Option<String> {
    let tag = raw
        .trim()
        .trim_start_matches('#')
        .trim()
        .to_ascii_lowercase();
    if tag.is_empty() || tag.len() > MAX_HASHTAG_LEN {
        return None;
    }
    if !tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    Some(tag)
}

/// Normalize a hashtag list: each tag normalized, empties
/// dropped, duplicates removed in first-appearance order.
/// Returns `None` when the list is over the per-post bound.
pub fn normalize_hashtags(raw: &[String]) -> Option<Vec<String>> {
    if raw.len() > MAX_HASHTAGS_PER_POST {
        return None;
    }
    let mut clean: Vec<String> = Vec::new();
    for tag in raw {
        let Some(tag) = normalize_hashtag(tag) else {
            return None;
        };
        if !clean.contains(&tag) {
            clean.push(tag);
        }
    }
    Some(clean)
}

/// True when `text` fits the network's text limit: non-empty
/// and at most `max_len` characters (counted as characters,
/// not bytes).
pub fn text_fits_limit(text: &str, max_len: i64) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && max_len > 0
        && (trimmed.chars().count() as i64) <= max_len
        && trimmed.len() <= MAX_POST_TEXT_LEN
}

/// True when every media id parses as a UUID and the list
/// fits the network's media bound. Media are references by
/// id only: this module never stores or fetches the bytes,
/// and it never checks that an id names a real document —
/// documents live in the documents / storage modules.
pub fn media_ids_valid(media_ids: &[String], max_items: i64) -> bool {
    if max_items < 0 || (media_ids.len() as i64) > max_items {
        return false;
    }
    media_ids.iter().all(|id| Uuid::parse_str(id).is_ok())
}

// ---------------------------------------------------------------------------
// Rate-limit math (pure functions, unit-tested below)
// ---------------------------------------------------------------------------

/// True when another publish fits inside the per-minute
/// limit. A limit of zero or less blocks everything.
pub fn publishes_allowed(published_in_window: i64, limit_per_minute: i64) -> bool {
    limit_per_minute > 0 && published_in_window < limit_per_minute
}

/// How many more publishes fit inside the per-minute limit
/// (zero when the window is full or the limit is not
/// positive).
pub fn remaining_publishes(published_in_window: i64, limit_per_minute: i64) -> i64 {
    if limit_per_minute <= 0 {
        return 0;
    }
    (limit_per_minute - published_in_window).max(0)
}

// ---------------------------------------------------------------------------
// Metric rollups (pure functions, unit-tested below)
// ---------------------------------------------------------------------------

/// Summed per-post metrics. The named fields cover the
/// conventional metric names; any other metric name a
/// tenant ingests rolls up into `other`, so the rollup
/// never drops data it does not recognize. All values are
/// integer counts — no money and no floating point anywhere.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricTotals {
    #[serde(default)]
    pub impressions: i64,
    #[serde(default)]
    pub likes: i64,
    #[serde(default)]
    pub shares: i64,
    #[serde(default)]
    pub comments: i64,
    #[serde(default)]
    pub clicks: i64,
    #[serde(default)]
    pub other: i64,
}

impl MetricTotals {
    /// Total engagement: every interaction metric summed
    /// (likes, shares, comments, clicks). Impressions are
    /// reach, not engagement, and are never added here.
    pub fn engagement(&self) -> i64 {
        self.likes + self.shares + self.comments + self.clicks
    }

    /// Total of every metric, including impressions and
    /// `other`.
    pub fn total(&self) -> i64 {
        self.impressions + self.engagement() + self.other
    }
}

/// Sum `(metric, value)` pairs into a [`MetricTotals`].
/// Unknown metric names land in `other`. Negative values
/// are clamped to zero at the pair level: a metric count
/// can never be negative.
pub fn sum_metrics(pairs: &[(String, i64)]) -> MetricTotals {
    let mut totals = MetricTotals::default();
    for (metric, value) in pairs {
        let value = (*value).max(0);
        match metric.as_str() {
            "impressions" => totals.impressions += value,
            "likes" => totals.likes += value,
            "shares" => totals.shares += value,
            "comments" => totals.comments += value,
            "clicks" => totals.clicks += value,
            _ => totals.other += value,
        }
    }
    totals
}

/// Merge one totals row into another (used to combine
/// per-post rollups into account and campaign rollups).
pub fn merge_totals(into: &mut MetricTotals, other: &MetricTotals) {
    into.impressions += other.impressions;
    into.likes += other.likes;
    into.shares += other.shares;
    into.comments += other.comments;
    into.clicks += other.clicks;
    into.other += other.other;
}

/// Engagement rate in basis points (hundredths of a
/// percent): `engagement * 10000 / impressions`, integer
/// math only. Zero when there are no impressions, so a
/// post with no reach never divides by zero and never
/// reports a rate.
pub fn engagement_bps(totals: &MetricTotals) -> i64 {
    if totals.impressions <= 0 {
        return 0;
    }
    totals.engagement() * 10_000 / totals.impressions
}

// ---------------------------------------------------------------------------
// Enumerations (fixed vocabulary — the shape, not the content)
// ---------------------------------------------------------------------------

/// Provider kind: the generic HTTP adapter or the log-only
/// test provider. No real network is named anywhere — a
/// tenant points the HTTP adapter at whatever network API
/// it uses by entering the API base as data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Http,
    LogOnly,
}

impl ProviderKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::LogOnly => "log_only",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "http" => Some(Self::Http),
            "log_only" => Some(Self::LogOnly),
            _ => None,
        }
    }
}

/// Credential auth type for the HTTP adapter: no auth, a
/// bearer token, an API key sent in a named header, or HTTP
/// basic auth (username stored on the account in plain
/// text, the write-only credential is the password / token).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    None,
    Bearer,
    ApiKeyHeader,
    Basic,
}

impl AuthType {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bearer => "bearer",
            Self::ApiKeyHeader => "api_key_header",
            Self::Basic => "basic",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "none" => Some(Self::None),
            "bearer" => Some(Self::Bearer),
            "api_key_header" => Some(Self::ApiKeyHeader),
            "basic" => Some(Self::Basic),
            _ => None,
        }
    }

    /// True when this auth type needs a stored credential.
    pub(crate) fn needs_credential(&self) -> bool {
        !matches!(self, Self::None)
    }
}

// ---------------------------------------------------------------------------
// Stored types
// ---------------------------------------------------------------------------

/// One admin-defined social network. `api_base_url` is
/// empty for the log-only kind. `publish_path` is the path
/// appended to the API base for a publish call. `post_types`
/// is the data list of post types this network accepts;
/// `max_text_length` and `max_media_items` are the
/// per-network content limits enforced at post write time;
/// `posts_per_minute` is the per-network publish rate
/// limit. `request_fields` is an optional JSON object of
/// extra fields merged into every HTTP adapter request
/// body. No network name, API base, or credential is
/// seeded: a tenant with no network rows cannot publish.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialNetwork {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub slug: String,
    pub kind: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,
    pub publish_path: String,
    pub auth_type: AuthType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    #[serde(default)]
    pub post_types: Vec<String>,
    pub max_text_length: i64,
    pub max_media_items: i64,
    pub posts_per_minute: i64,
    #[serde(default)]
    pub request_fields: Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_true() -> bool {
    true
}

/// One connected social account on a network. The
/// credential (access token) is write-only:
/// `credential_encrypted` holds the encrypted secret (never
/// serialized) and only `credential_fingerprint` (first 12
/// hex characters of the SHA-256) and `key_configured` are
/// visible. `webhook_secret_hash` is the SHA-256 hex of the
/// metrics-ingest webhook secret; the raw secret is returned
/// exactly once at connect / rotation and never stored.
/// `token_expires_at` is when the stored token stops being
/// usable; an account with no expiry never expires.
/// `connected` is false after a disconnect, which also
/// clears the stored credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialAccount {
    pub id: Uuid,
    pub tenant_id: String,
    pub network_id: Uuid,
    pub handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_username: Option<String>,
    #[serde(skip_serializing)]
    pub credential_encrypted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_fingerprint: Option<String>,
    #[serde(skip_serializing)]
    pub webhook_secret_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub connected: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SocialAccount {
    /// Masked read view: the credential and the webhook
    /// secret never appear; only whether a credential is
    /// configured and its fingerprint are shown.
    pub(crate) fn public_view(&self) -> Value {
        serde_json::json!({
            "id": self.id.to_string(),
            "tenant_id": self.tenant_id,
            "network_id": self.network_id.to_string(),
            "handle": self.handle,
            "display_name": self.display_name,
            "auth_username": self.auth_username,
            "key_configured": self.credential_encrypted.is_some(),
            "credential_fingerprint": self.credential_fingerprint,
            "webhook_secret_configured": self.webhook_secret_hash.is_some(),
            "token_expires_at": self.token_expires_at.map(|when| when.to_rfc3339()),
            "connected": self.connected,
            "enabled": self.enabled,
            "connected_at": self.connected_at.map(|when| when.to_rfc3339()),
            "created_by": self.created_by,
            "created_at": self.created_at.to_rfc3339(),
            "updated_at": self.updated_at.to_rfc3339(),
        })
    }
}

/// One publish result for one target account of a post.
/// Kept on the post row so a post's publish history stays
/// readable after the account changes. `provider_post_id`
/// is the id the network returned, when it returned one;
/// `error` holds the provider's failure text (truncated,
/// never a credential).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishResult {
    pub account_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_post_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
}

/// One social post: text, media references by document /
/// storage id only, an optional link, hashtags as data, and
/// a list of target account ids. `post_type` must be one of
/// the network row's `post_types`. `campaign_id` is an
/// optional campaign reference (no foreign key). `send_at`
/// is the scheduled publish time; `published_at` is when
/// the publish pass ran. `results` holds one publish result
/// per target account. A scheduled-post runner is NOT part
/// of this module: scheduling only records `send_at` and
/// emits `social.post.scheduled` for a scheduler to pick
/// up, and this module runs none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialPost {
    pub id: Uuid,
    pub tenant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<Uuid>,
    pub network_id: Uuid,
    #[serde(default)]
    pub account_ids: Vec<String>,
    pub post_type: String,
    pub text: String,
    #[serde(default)]
    pub media_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default)]
    pub hashtags: Vec<String>,
    pub status: PostStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub results: Vec<PublishResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Post counts for one campaign, computed live from the
/// post rows so a list call never has to trust a stale
/// counter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CampaignPostCounts {
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub draft: i64,
    #[serde(default)]
    pub scheduled: i64,
    #[serde(default)]
    pub publishing: i64,
    #[serde(default)]
    pub published: i64,
    #[serde(default)]
    pub failed: i64,
    #[serde(default)]
    pub cancelled: i64,
}

/// One social campaign: a named group of posts over a date
/// range with a goal as data. `goal` is a free-text goal
/// label; `goal_metric` names the metric the goal is
/// measured in (any metric slug, data — never a hard-coded
/// list) and `goal_target` is the integer count the
/// campaign aims for. `status` is `draft`, `active`,
/// `paused`, `completed`, or `cancelled`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialCampaign {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_metric: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_target: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<DateTime<Utc>>,
    pub status: CampaignStatus,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded metric value for one post. Metrics arrive
/// only through the provider verification-token webhook or
/// through manual entry — this module never fetches live
/// numbers from a network, so a metric row is always a
/// reported value, never a scraped one. One row per
/// (post, metric, source): re-ingesting the same metric
/// from the same source replaces the value instead of
/// double-counting it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialMetric {
    pub id: Uuid,
    pub tenant_id: String,
    pub post_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Uuid>,
    pub metric: String,
    pub value: i64,
    pub source: String,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audit row: who changed what, when, with the before
/// and after values as JSON. Written for network, account,
/// post, campaign, and metric changes. Audit rows are never
/// updated or deleted through the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub id: Uuid,
    pub tenant_id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub action: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateNetworkRequest {
    pub name: String,
    pub slug: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_path: Option<String>,
    #[serde(default)]
    pub auth_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    #[serde(default)]
    pub post_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_text_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_media_items: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posts_per_minute: Option<i64>,
    #[serde(default)]
    pub request_fields: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateNetworkRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_text_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_media_items: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posts_per_minute: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_fields: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAccountRequest {
    pub network_id: String,
    pub handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAccountRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_username: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_expires_at: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ConnectAccountRequest {
    pub credential: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePostRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    pub network_id: String,
    #[serde(default)]
    pub account_ids: Vec<String>,
    pub post_type: String,
    pub text: String,
    #[serde(default)]
    pub media_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default)]
    pub hashtags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePostRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hashtags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_at: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct SchedulePostRequest {
    pub send_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateCampaignRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_metric: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_target: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCampaignRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_metric: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_target: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RecordMetricRequest {
    pub metric: String,
    pub value: i64,
}

#[derive(Debug, Deserialize)]
pub struct MetricIngestItem {
    pub metric: String,
    pub value: i64,
}

#[derive(Debug, Deserialize)]
pub struct MetricsIngestRequest {
    pub post_id: String,
    pub metrics: Vec<MetricIngestItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_status_machine() {
        use PostStatus::*;
        assert!(post_can_transition(Draft, Scheduled));
        assert!(post_can_transition(Draft, Publishing));
        assert!(post_can_transition(Draft, Cancelled));
        assert!(post_can_transition(Scheduled, Publishing));
        assert!(post_can_transition(Scheduled, Cancelled));
        assert!(post_can_transition(Publishing, Published));
        assert!(post_can_transition(Publishing, Failed));
        assert!(post_can_transition(Failed, Publishing));
        assert!(post_can_transition(Failed, Cancelled));
        assert!(!post_can_transition(Draft, Published));
        assert!(!post_can_transition(Draft, Failed));
        assert!(!post_can_transition(Scheduled, Published));
        assert!(!post_can_transition(Scheduled, Failed));
        assert!(!post_can_transition(Published, Failed));
        assert!(!post_can_transition(Published, Draft));
        assert!(!post_can_transition(Cancelled, Draft));
        assert!(!post_can_transition(Failed, Published));
        assert!(!post_can_transition(Failed, Scheduled));
    }

    #[test]
    fn campaign_status_machine() {
        use CampaignStatus::*;
        assert!(campaign_can_transition(Draft, Active));
        assert!(campaign_can_transition(Draft, Cancelled));
        assert!(campaign_can_transition(Active, Paused));
        assert!(campaign_can_transition(Active, Completed));
        assert!(campaign_can_transition(Active, Cancelled));
        assert!(campaign_can_transition(Paused, Active));
        assert!(campaign_can_transition(Paused, Cancelled));
        assert!(!campaign_can_transition(Draft, Paused));
        assert!(!campaign_can_transition(Draft, Completed));
        assert!(!campaign_can_transition(Active, Draft));
        assert!(!campaign_can_transition(Paused, Completed));
        assert!(!campaign_can_transition(Completed, Active));
        assert!(!campaign_can_transition(Cancelled, Draft));
    }

    #[test]
    fn schedule_must_be_future() {
        let now = Utc::now();
        assert!(is_future_schedule(now + chrono::Duration::seconds(60), now));
        assert!(!is_future_schedule(now, now));
        assert!(!is_future_schedule(now - chrono::Duration::seconds(1), now));
    }

    #[test]
    fn token_expiry_math() {
        let now = Utc::now();
        assert!(!token_expired(None, now));
        assert!(!token_expiring_within(None, now, 3600));
        let future = now + chrono::Duration::seconds(3600);
        assert!(!token_expired(Some(future), now));
        assert!(token_expiring_within(Some(future), now, 3600));
        assert!(!token_expiring_within(Some(future), now, 3599));
        let past = now - chrono::Duration::seconds(10);
        assert!(token_expired(Some(past), now));
        assert!(token_expiring_within(Some(past), now, 3600));
        assert!(token_expired(Some(now), now));
    }

    #[test]
    fn slug_and_url_and_metric_validation() {
        assert!(is_valid_slug("my-network"));
        assert!(is_valid_slug("net2"));
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("My-Network"));
        assert!(!is_valid_slug("-net"));
        assert!(!is_valid_slug("net-"));
        assert!(!is_valid_slug("net work"));
        assert!(is_https_url("https://api.example.com/v1"));
        assert!(is_https_url("https://api.example.com:8443/v1"));
        assert!(!is_https_url("http://api.example.com/v1"));
        assert!(!is_https_url("https://"));
        assert!(is_valid_metric_name("impressions"));
        assert!(is_valid_metric_name("custom_metric_1"));
        assert!(!is_valid_metric_name(""));
        assert!(!is_valid_metric_name("9lives"));
        assert!(!is_valid_metric_name("with-dash"));
        assert!(!is_valid_metric_name("with space"));
    }

    #[test]
    fn hashtag_normalization() {
        assert_eq!(
            normalize_hashtag("#LaunchDay"),
            Some("launchday".to_string())
        );
        assert_eq!(
            normalize_hashtag("  sale_2026 "),
            Some("sale_2026".to_string())
        );
        assert_eq!(normalize_hashtag("#"), None);
        assert_eq!(normalize_hashtag("has space"), None);
        assert_eq!(normalize_hashtag("with-dash"), None);
        assert_eq!(
            normalize_hashtags(&["#A".to_string(), "a".to_string(), "#B".to_string()]),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(normalize_hashtags(&["bad tag".to_string()]), None);
        assert_eq!(normalize_hashtags(&vec!["ok".to_string(); 31]), None);
    }

    #[test]
    fn text_and_media_limits() {
        assert!(text_fits_limit("Hello", 500));
        assert!(!text_fits_limit("", 500));
        assert!(!text_fits_limit("   ", 500));
        assert!(!text_fits_limit("Hello", 4));
        assert!(!text_fits_limit("Hello", 0));
        let id = Uuid::new_v4().to_string();
        assert!(media_ids_valid(&[id.clone()], 4));
        assert!(!media_ids_valid(&[id.clone()], 0));
        assert!(!media_ids_valid(&["not-a-uuid".to_string()], 4));
        assert!(!media_ids_valid(&[id.clone(), id], 1));
        assert!(post_type_allowed(
            "announcement",
            &["announcement".to_string(), "update".to_string()]
        ));
        assert!(!post_type_allowed("video", &["announcement".to_string()]));
    }

    #[test]
    fn rate_limit_math() {
        assert!(publishes_allowed(0, 30));
        assert!(publishes_allowed(29, 30));
        assert!(!publishes_allowed(30, 30));
        assert!(!publishes_allowed(0, 0));
        assert!(!publishes_allowed(0, -1));
        assert_eq!(remaining_publishes(0, 30), 30);
        assert_eq!(remaining_publishes(29, 30), 1);
        assert_eq!(remaining_publishes(30, 30), 0);
        assert_eq!(remaining_publishes(100, 30), 0);
    }

    #[test]
    fn metric_rollup_math() {
        let pairs = vec![
            ("impressions".to_string(), 1000),
            ("likes".to_string(), 40),
            ("shares".to_string(), 5),
            ("comments".to_string(), 3),
            ("clicks".to_string(), 12),
            ("saves".to_string(), 7),
            ("likes".to_string(), -5),
        ];
        let totals = sum_metrics(&pairs);
        assert_eq!(totals.impressions, 1000);
        assert_eq!(totals.likes, 40);
        assert_eq!(totals.shares, 5);
        assert_eq!(totals.comments, 3);
        assert_eq!(totals.clicks, 12);
        assert_eq!(totals.other, 7);
        assert_eq!(totals.engagement(), 60);
        assert_eq!(totals.total(), 1067);
        // 60 * 10000 / 1000 = 600 basis points (6 percent).
        assert_eq!(engagement_bps(&totals), 600);
        // No impressions: no division, rate is zero.
        let empty = MetricTotals::default();
        assert_eq!(engagement_bps(&empty), 0);
        assert_eq!(empty.engagement(), 0);
        // Merging combines two rollups.
        let mut merged = totals.clone();
        merge_totals(
            &mut merged,
            &MetricTotals {
                impressions: 500,
                likes: 10,
                ..Default::default()
            },
        );
        assert_eq!(merged.impressions, 1500);
        assert_eq!(merged.likes, 50);
        assert_eq!(merged.engagement(), 70);
    }
}
