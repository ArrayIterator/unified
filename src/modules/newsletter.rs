// Newsletter module.
//
// Lists, subscribers with double opt-in, and campaigns built from the
// existing GrapeJS email-template builder (templates are referenced by
// id; the builder itself is never re-implemented here). Campaigns send
// through the Email Providers module, one explicit provider connection
// per campaign (or the list's default connection), and report
// per-campaign stats. Open and click tracking are first-class: a
// tracking pixel and rewritten links emit `newsletter.open` and
// `newsletter.click` on the hook bus.
//
// Routes (all under `/api/v1/newsletter`):
//   Lists:
//     GET    /lists                       - paginated lists
//     POST   /lists                       - create a list
//     GET    /lists/{id}                  - one list
//     PUT    /lists/{id}                  - update a list
//     DELETE /lists/{id}                  - delete a list (and its subscribers)
//     GET    /lists/{id}/subscribers      - paginated subscribers of one list
//     POST   /lists/{id}/subscribe        - subscribe an email address
//   Subscribers:
//     GET    /subscribers/{id}            - one subscriber
//     PUT    /subscribers/{id}            - update a subscriber
//     DELETE /subscribers/{id}            - delete a subscriber
//     GET    /subscribers/confirm?token=  - confirm a double opt-in token
//     POST   /subscribers/confirm         - confirm a double opt-in token
//     POST   /subscribers/{id}/resend-confirm - resend the confirmation email
//     POST   /subscribers/{id}/unsubscribe - unsubscribe a subscriber
//     GET    /unsubscribe?token=          - unsubscribe through a signed link
//   Campaigns:
//     GET    /campaigns                   - paginated campaigns
//     POST   /campaigns                   - create a campaign
//     GET    /campaigns/{id}              - one campaign
//     PUT    /campaigns/{id}              - update a draft campaign
//     DELETE /campaigns/{id}              - delete a campaign
//     POST   /campaigns/{id}/schedule     - schedule a campaign
//     POST   /campaigns/{id}/send-now     - send a campaign immediately
//     POST   /campaigns/{id}/test-send    - send a test copy to one address
//     GET    /campaigns/{id}/stats        - campaign stats with rates
//   Tracking:
//     GET    /track/open/{campaign_id}/{subscriber_id}  - 1x1 tracking pixel
//     GET    /track/click/{campaign_id}/{subscriber_id}?url=... - tracked redirect
//   Webhooks:
//     POST   /webhooks/{provider}         - provider delivery/bounce/complaint webhook
//   GET    /stats                         - overall newsletter stats
//
// Hook contracts:
//   Emits `newsletter.subscribed`, `newsletter.confirmed`,
//   `newsletter.unsubscribed`, `newsletter.campaign.sent`,
//   `newsletter.campaign.completed`, `newsletter.open`,
//   `newsletter.click`.
//   Sends through `email.send_requested` (Email Providers module) and
//   renders templates through `template.render_requested` (Email
//   Templates module). Scheduling emits `scheduler.enqueue` with kind
//   `newsletter.send`; the scheduler must know that kind (see the
//   coordinator report for the snippet). The module also answers
//   `newsletter.campaign.send` itself, which is the execution entry
//   point the scheduler calls back through.
//
// Notes on honesty:
//   - Subscriber, list, and campaign records are kept in memory (same
//     pattern as the other modules in this tree); durable persistence
//     is a follow-up for the integrator (migration sketch in the
//     coordinator report).
//   - Confirmation and unsubscribe tokens are HMAC-signed with a
//     per-process secret, so they stop verifying after a restart.
//   - A campaign is `completed` once its send phase finished (every
//     confirmed subscriber was queued with the provider); delivery,
//     bounce, and complaint confirmations arrive later through the
//     provider webhooks and update the stats.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

pub const EVENT_NEWSLETTER_SUBSCRIBED: &str = "newsletter.subscribed";
pub const EVENT_NEWSLETTER_CONFIRMED: &str = "newsletter.confirmed";
pub const EVENT_NEWSLETTER_UNSUBSCRIBED: &str = "newsletter.unsubscribed";
pub const EVENT_NEWSLETTER_CAMPAIGN_SENT: &str = "newsletter.campaign.sent";
pub const EVENT_NEWSLETTER_CAMPAIGN_COMPLETED: &str = "newsletter.campaign.completed";
pub const EVENT_NEWSLETTER_OPEN: &str = "newsletter.open";
pub const EVENT_NEWSLETTER_CLICK: &str = "newsletter.click";
pub const EVENT_NEWSLETTER_CAMPAIGN_SEND: &str = "newsletter.campaign.send";
pub const EVENT_EMAIL_SEND_REQUESTED: &str = "email.send_requested";
pub const EVENT_TEMPLATE_RENDER_REQUESTED: &str = "template.render_requested";
pub const EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY: &str = "email_provider.webhook_verify";
pub const EVENT_SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";

const CONFIRM_TOKEN_TTL_SECS: i64 = 48 * 3600;
const UNSUBSCRIBE_TOKEN_TTL_SECS: i64 = 30 * 24 * 3600;

/// Transparent 1x1 GIF returned by the open-tracking endpoint.
const TRACKING_PIXEL: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsletterList {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Default email provider connection for this list's campaigns.
    /// A campaign may override it with its own connection id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_connection_id: Option<String>,
    pub double_opt_in: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_email: Option<String>,
    /// Public base URL used to build confirmation, unsubscribe, and
    /// tracking links (for example "https://example.com"). When empty,
    /// relative links are returned instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscriber {
    pub id: String,
    pub list_id: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `pending`, `confirmed`, `unsubscribed`, `bounced`, `complained`.
    pub status: String,
    pub metadata: HashMap<String, Value>,
    pub confirmed_at: Option<String>,
    pub unsubscribed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CampaignStats {
    pub sent: u64,
    pub delivered: u64,
    pub opened: u64,
    pub clicked: u64,
    pub bounced: u64,
    pub unsubscribed: u64,
    pub complaints: u64,
    /// Unique opens (one per subscriber) as opposed to total pixel hits.
    pub unique_opened: u64,
    pub unique_clicked: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignStatsView {
    #[serde(flatten)]
    pub stats: CampaignStats,
    pub open_rate: f64,
    pub click_rate: f64,
    pub bounce_rate: f64,
    pub unsubscribe_rate: f64,
    pub complaint_rate: f64,
}

impl CampaignStats {
    fn view(&self) -> CampaignStatsView {
        let rate = |count: u64| {
            if self.sent == 0 {
                0.0
            } else {
                (count as f64 / self.sent as f64 * 10000.0).round() / 100.0
            }
        };
        CampaignStatsView {
            stats: self.clone(),
            open_rate: rate(self.unique_opened),
            click_rate: rate(self.unique_clicked),
            bounce_rate: rate(self.bounced),
            unsubscribe_rate: rate(self.unsubscribed),
            complaint_rate: rate(self.complaints),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Campaign {
    pub id: String,
    pub list_id: String,
    pub name: String,
    /// GrapeJS email template id (from the Email Templates module).
    pub template_id: String,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_text: Option<String>,
    /// Explicit provider connection for this campaign. Falls back to
    /// the list's default connection, then to the global default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    /// `draft`, `scheduled`, `sending`, `sent`, `completed`, `failed`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
    pub stats: CampaignStats,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackingEvent {
    pub id: String,
    pub campaign_id: String,
    pub subscriber_id: String,
    /// `open` or `click`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// In-memory stores (same pattern as the other modules in this tree).
// ---------------------------------------------------------------------------

static LISTS: OnceLock<DashMap<String, NewsletterList>> = OnceLock::new();
static SUBSCRIBERS: OnceLock<DashMap<String, Subscriber>> = OnceLock::new();
static CAMPAIGNS: OnceLock<DashMap<String, Campaign>> = OnceLock::new();
static TRACKING_EVENTS: OnceLock<DashMap<String, TrackingEvent>> = OnceLock::new();

fn lists_store() -> &'static DashMap<String, NewsletterList> {
    LISTS.get_or_init(DashMap::new)
}

fn subscribers_store() -> &'static DashMap<String, Subscriber> {
    SUBSCRIBERS.get_or_init(DashMap::new)
}

fn campaigns_store() -> &'static DashMap<String, Campaign> {
    CAMPAIGNS.get_or_init(DashMap::new)
}

fn tracking_store() -> &'static DashMap<String, TrackingEvent> {
    TRACKING_EVENTS.get_or_init(DashMap::new)
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct NewsletterState {
    hooks: RwLock<Option<Arc<HookBus>>>,
    debug: AtomicBool,
    /// Per-process secret for signing confirmation and unsubscribe
    /// tokens. Tokens stop verifying after a restart (the stores are
    /// in-memory too, so nothing outlives the process anyway).
    token_secret: String,
}

impl NewsletterState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
            token_secret: Uuid::new_v4().to_string(),
        }
    }

    fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("newsletter hooks lock poisoned") = Some(hooks);
    }

    fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks
            .read()
            .expect("newsletter hooks lock poisoned")
            .clone()
    }

    fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(hooks) = self.hooks() {
            hooks.emit(event, data);
        }
    }

    /// Build a signed token for a subscriber: base64url of
    /// `subscriber_id:expiry_unix:purpose`, plus a hex HMAC-SHA256
    /// over that payload.
    fn sign_token(&self, subscriber_id: &str, purpose: &str, ttl_secs: i64) -> String {
        let expiry = Utc::now().timestamp() + ttl_secs;
        let payload = format!("{subscriber_id}:{expiry}:{purpose}");
        let mut mac = Hmac::<Sha256>::new_from_slice(self.token_secret.as_bytes())
            .expect("HMAC key length is always valid");
        mac.update(payload.as_bytes());
        let signature = mac.finalize().into_bytes();
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes());
        format!("{encoded}.{}", hex_encode(&signature))
    }

    /// Verify a signed token and return (subscriber_id, purpose).
    fn verify_token(&self, token: &str) -> UnifiedResult<(String, String)> {
        let (encoded, signature) = token.split_once('.').ok_or_else(|| {
            UnifiedError::bad_request("Invalid token format")
        })?;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| UnifiedError::bad_request("Invalid token encoding"))?;
        let payload = String::from_utf8(payload)
            .map_err(|_| UnifiedError::bad_request("Invalid token encoding"))?;
        let mut mac = Hmac::<Sha256>::new_from_slice(self.token_secret.as_bytes())
            .expect("HMAC key length is always valid");
        mac.update(payload.as_bytes());
        let expected = hex_encode(&mac.finalize().into_bytes());
        if !signatures_equal(signature, &expected) {
            return Err(UnifiedError::unauthorized("Invalid token signature"));
        }
        let mut parts = payload.split(':');
        let (Some(subscriber_id), Some(expiry), Some(purpose)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(UnifiedError::bad_request("Invalid token payload"));
        };
        let expiry: i64 = expiry
            .parse()
            .map_err(|_| UnifiedError::bad_request("Invalid token expiry"))?;
        if Utc::now().timestamp() > expiry {
            return Err(UnifiedError::unauthorized("Token has expired"));
        }
        Ok((subscriber_id.to_string(), purpose.to_string()))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn signatures_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn now_string() -> String {
    Utc::now().to_rfc3339()
}

fn validate_email_address(input: &str) -> UnifiedResult<()> {
    let address = input.trim();
    if address.is_empty() || !address.contains('@') {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address: {input}"
        )));
    }
    let mut parts = address.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address: {input}"
        )));
    };
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address: {input}"
        )));
    }
    Ok(())
}

fn validate_connection_id(value: &Option<String>) -> UnifiedResult<()> {
    if let Some(raw) = value {
        if !raw.trim().is_empty() {
            Uuid::parse_str(raw).map_err(|_| {
                UnifiedError::bad_request(format!("Invalid connection id \"{raw}\""))
            })?;
        }
    }
    Ok(())
}

fn list_base_url(list: &NewsletterList) -> String {
    list.base_url
        .clone()
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string()
}

fn confirm_url(list: &NewsletterList, token: &str) -> String {
    format!(
        "{}/api/v1/newsletter/subscribers/confirm?token={token}",
        list_base_url(list)
    )
}

fn unsubscribe_url(list: &NewsletterList, token: &str) -> String {
    format!("{}/api/v1/newsletter/unsubscribe?token={token}", list_base_url(list))
}

fn href_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"href="(https?://[^"]+)""#).expect("href regex must compile")
    })
}

/// Append the open-tracking pixel and rewrite links to the click
/// tracker for one subscriber. The unsubscribe placeholder token is
/// replaced with the subscriber's signed unsubscribe link.
fn personalize_html(
    html: &str,
    base_url: &str,
    campaign_id: &str,
    subscriber_id: &str,
    unsubscribe_link: &str,
) -> String {
    let mut out = html.replace("__UNSUBSCRIBE_URL__", unsubscribe_link);
    let pixel = format!(
        "{base_url}/api/v1/newsletter/track/open/{campaign_id}/{subscriber_id}"
    );
    let pixel_tag = format!("<img src=\"{pixel}\" width=\"1\" height=\"1\" alt=\"\" />");
    if let Some(position) = out.to_lowercase().rfind("</body>") {
        out.insert_str(position, &pixel_tag);
    } else {
        out.push_str(&pixel_tag);
    }
    let rewritten = href_regex()
        .replace_all(&out, |caps: &regex::Captures| {
            let original = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            // Never rewrite the tracking pixel itself or a link that
            // already points at this application.
            if original.contains("/api/v1/newsletter/track/") {
                return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
            }
            let encoded = url::form_urlencoded::byte_serialize(original.as_bytes()).collect::<String>();
            format!(
                "href=\"{base_url}/api/v1/newsletter/track/click/{campaign_id}/{subscriber_id}?url={encoded}\""
            )
        })
        .into_owned();
    rewritten
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

// ---------------------------------------------------------------------------
// Shared helpers.
// ---------------------------------------------------------------------------

/// Render a GrapeJS template through the Email Templates module.
fn render_template(
    state: &NewsletterState,
    template_id: &str,
    variables: &HashMap<String, Value>,
) -> UnifiedResult<(String, String)> {
    let Some(hooks) = state.hooks() else {
        return Err(UnifiedError::new(
            "Email template service is unavailable (no hook bus)",
            503,
        ));
    };
    let rendered = hooks.emit(
        EVENT_TEMPLATE_RENDER_REQUESTED,
        json!({ "template_id": template_id, "variables": variables }),
    );
    match rendered.results.first() {
        Some(result) if result.get("error").is_none() => {
            let html = result
                .get("rendered_html")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let subject = result
                .get("subject")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            Ok((html, subject))
        }
        Some(result) => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("template render failed");
            Err(UnifiedError::new(
                format!("Template render failed: {reason}"),
                404,
            ))
        }
        None => Err(UnifiedError::new(
            "Email template service did not answer the template.render_requested event",
            503,
        )),
    }
}

/// Ask the Email Providers module to send one message. Returns true
/// when a handler queued it.
fn request_send(state: &NewsletterState, payload: Value) -> bool {
    let Some(hooks) = state.hooks() else {
        return false;
    };
    let answer = hooks.emit(EVENT_EMAIL_SEND_REQUESTED, payload);
    match answer.results.first() {
        Some(result) => result.get("error").is_none(),
        None => false,
    }
}

fn confirmed_subscribers(list_id: &str) -> Vec<Subscriber> {
    let mut items: Vec<Subscriber> = subscribers_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|subscriber| subscriber.list_id == list_id && subscriber.status == "confirmed")
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    items
}

fn find_subscriber(list_id: &str, email: &str) -> Option<Subscriber> {
    let needle = email.trim().to_lowercase();
    subscribers_store()
        .iter()
        .map(|entry| entry.value().clone())
        .find(|subscriber| {
            subscriber.list_id == list_id && subscriber.email.to_lowercase() == needle
        })
}

fn find_subscriber_by_email(email: &str) -> Option<Subscriber> {
    let needle = email.trim().to_lowercase();
    subscribers_store()
        .iter()
        .map(|entry| entry.value().clone())
        .find(|subscriber| subscriber.email.to_lowercase() == needle)
}

/// Send a campaign to every confirmed subscriber of its list.
/// Returns (queued, failed). The campaign is marked `completed` once
/// the send phase finished; delivery confirmations arrive later
/// through provider webhooks.
async fn send_campaign(state: &Arc<NewsletterState>, campaign_id: &str) -> UnifiedResult<Value> {
    let mut campaign = campaigns_store()
        .get(campaign_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
    let list = lists_store()
        .get(&campaign.list_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Campaign list not found"))?;
    if campaign.status == "sending" {
        return Err(UnifiedError::conflict("Campaign is already sending"));
    }
    if campaign.status == "completed" || campaign.status == "sent" {
        return Err(UnifiedError::conflict(
            "Campaign was already sent; duplicate it to send again",
        ));
    }
    campaign.status = "sending".to_string();
    campaign.updated_at = now_string();
    campaigns_store().insert(campaign.id.clone(), campaign.clone());

    let connection_id = campaign
        .connection_id
        .clone()
        .or_else(|| list.default_connection_id.clone());
    let (rendered_html, rendered_subject) =
        render_template(state, &campaign.template_id, &HashMap::new())?;
    let subject = if campaign.subject.trim().is_empty() {
        rendered_subject
    } else {
        campaign.subject.clone()
    };
    let base_url = list_base_url(&list);
    let recipients = confirmed_subscribers(&list.id);
    let mut queued = 0u64;
    let mut failed = 0u64;
    for subscriber in &recipients {
        let unsubscribe_token = state.sign_token(&subscriber.id, "unsubscribe", UNSUBSCRIBE_TOKEN_TTL_SECS);
        let html = personalize_html(
            &rendered_html,
            &base_url,
            &campaign.id,
            &subscriber.id,
            &unsubscribe_url(&list, &unsubscribe_token),
        );
        let sent = request_send(
            state,
            json!({
                "connection_id": connection_id,
                "to": subscriber.email,
                "from": list.sender_email,
                "from_name": list.sender_name,
                "subject": subject,
                "body_html": html,
                "metadata": {
                    "campaign_id": campaign.id,
                    "subscriber_id": subscriber.id,
                },
            }),
        );
        if sent {
            queued += 1;
        } else {
            failed += 1;
        }
    }
    campaign.stats.sent += queued;
    campaign.sent_at = Some(now_string());
    campaign.updated_at = now_string();
    if failed > 0 && queued == 0 {
        campaign.status = "failed".to_string();
        campaigns_store().insert(campaign.id.clone(), campaign.clone());
        return Err(UnifiedError::new(
            "No email provider answered the email.send_requested event: no provider handler is registered",
            503,
        ));
    }
    campaign.status = "completed".to_string();
    campaigns_store().insert(campaign.id.clone(), campaign.clone());
    state.emit(
        EVENT_NEWSLETTER_CAMPAIGN_SENT,
        json!({
            "campaign_id": campaign.id,
            "list_id": campaign.list_id,
            "queued": queued,
            "failed": failed,
            "sent_at": campaign.sent_at,
        }),
    );
    state.emit(
        EVENT_NEWSLETTER_CAMPAIGN_COMPLETED,
        json!({
            "campaign_id": campaign.id,
            "list_id": campaign.list_id,
            "queued": queued,
            "failed": failed,
        }),
    );
    Ok(json!({
        "campaign_id": campaign.id,
        "queued": queued,
        "failed": failed,
        "recipients": recipients.len(),
        "status": campaign.status,
    }))
}

/// Send due scheduled campaigns (or one named campaign) from a
/// background worker. Hook handlers are synchronous, so this runs on a
/// dedicated thread with its own runtime when no runtime is current.
fn spawn_campaign_worker(state: Arc<NewsletterState>, campaign_id: Option<String>) {
    let run = move |state: Arc<NewsletterState>| {
        let due: Vec<String> = if let Some(id) = campaign_id {
            vec![id]
        } else {
            let now = Utc::now();
            campaigns_store()
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|campaign| {
                    campaign.status == "scheduled"
                        && campaign
                            .scheduled_at
                            .as_deref()
                            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                            .is_some_and(|when| when.with_timezone(&Utc) <= now)
                })
                .map(|campaign| campaign.id)
                .collect()
        };
        for id in due {
            let worker_state = state.clone();
            let result = (|| -> UnifiedResult<Value> {
                if tokio::runtime::Handle::try_current().is_ok() {
                    // Already on a runtime thread (tokio::spawn path):
                    // block_on would deadlock, so this branch is only
                    // reached from the thread path below.
                    Err(UnifiedError::internal("campaign worker started on a runtime thread"))
                } else {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| {
                            UnifiedError::internal(format!("Cannot start campaign runtime: {error}"))
                        })?;
                    runtime.block_on(send_campaign(&worker_state, &id))
                }
            })();
            if let Err(error) = result {
                if let Some(mut campaign) = campaigns_store().get_mut(&id) {
                    campaign.status = "failed".to_string();
                    campaign.updated_at = now_string();
                }
                state.emit(
                    EVENT_NEWSLETTER_CAMPAIGN_COMPLETED,
                    json!({ "campaign_id": id, "failed": true, "error": error.message }),
                );
            }
        }
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        let state = state.clone();
        tokio::spawn(async move {
            // The send path needs an async context but no blocking
            // runtime calls: run it directly on this runtime.
            let due: Vec<String> = match campaign_id {
                Some(id) => vec![id],
                None => {
                    let now = Utc::now();
                    campaigns_store()
                        .iter()
                        .map(|entry| entry.value().clone())
                        .filter(|campaign| {
                            campaign.status == "scheduled"
                                && campaign
                                    .scheduled_at
                                    .as_deref()
                                    .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                                    .is_some_and(|when| when.with_timezone(&Utc) <= now)
                        })
                        .map(|campaign| campaign.id)
                        .collect()
                }
            };
            for id in due {
                if let Err(error) = send_campaign(&state, &id).await {
                    if let Some(mut campaign) = campaigns_store().get_mut(&id) {
                        campaign.status = "failed".to_string();
                        campaign.updated_at = now_string();
                    }
                    state.emit(
                        EVENT_NEWSLETTER_CAMPAIGN_COMPLETED,
                        json!({ "campaign_id": id, "failed": true, "error": error.message }),
                    );
                }
            }
        });
    } else {
        std::thread::spawn(move || run(state));
    }
}

// ---------------------------------------------------------------------------
// List handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ListInput {
    name: String,
    description: Option<String>,
    default_connection_id: Option<String>,
    double_opt_in: Option<bool>,
    sender_name: Option<String>,
    sender_email: Option<String>,
    base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListUpdateInput {
    name: Option<String>,
    description: Option<String>,
    default_connection_id: Option<String>,
    double_opt_in: Option<bool>,
    sender_name: Option<String>,
    sender_email: Option<String>,
    base_url: Option<String>,
}

async fn list_lists(query: web::Query<PaginationParams>) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<NewsletterList> = lists_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|list| matches_search(&format!("{} {}", list.name, list.description), &params))
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    api_json::success_response(page.to_json(), 200)
}

async fn create_list(
    state: web::Data<Arc<NewsletterState>>,
    body: web::Json<ListInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    if input.name.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("List name must not be empty"),
            debug,
        );
    }
    if let Err(error) = validate_connection_id(&input.default_connection_id) {
        return api_json::error_response(&error, debug);
    }
    if let Some(email) = &input.sender_email {
        if let Err(error) = validate_email_address(email) {
            return api_json::error_response(&error, debug);
        }
    }
    let now = now_string();
    let list = NewsletterList {
        id: Uuid::now_v7().to_string(),
        name: input.name.trim().to_string(),
        description: input.description.unwrap_or_default(),
        default_connection_id: input.default_connection_id.filter(|v| !v.trim().is_empty()),
        double_opt_in: input.double_opt_in.unwrap_or(true),
        sender_name: input.sender_name.filter(|v| !v.trim().is_empty()),
        sender_email: input.sender_email.filter(|v| !v.trim().is_empty()),
        base_url: input.base_url.filter(|v| !v.trim().is_empty()),
        created_at: now.clone(),
        updated_at: now,
    };
    lists_store().insert(list.id.clone(), list.clone());
    api_json::success_response(list, 201)
}

async fn get_list(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match lists_store().get(path.as_str()).map(|entry| entry.value().clone()) {
        Some(list) => api_json::success_response(list, 200),
        None => api_json::error_response(&UnifiedError::not_found("Newsletter list not found"), debug),
    }
}

async fn update_list(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<ListUpdateInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    if let Err(error) = validate_connection_id(&input.default_connection_id) {
        return api_json::error_response(&error, debug);
    }
    if let Some(email) = &input.sender_email {
        if let Err(error) = validate_email_address(email) {
            return api_json::error_response(&error, debug);
        }
    }
    let mut lists = lists_store();
    let Some(mut list) = lists.get_mut(path.as_str()) else {
        return api_json::error_response(&UnifiedError::not_found("Newsletter list not found"), debug);
    };
    if let Some(name) = input.name {
        if name.trim().is_empty() {
            return api_json::error_response(&UnifiedError::bad_request("List name must not be empty"), debug);
        }
        list.name = name.trim().to_string();
    }
    if let Some(description) = input.description {
        list.description = description;
    }
    if let Some(connection_id) = input.default_connection_id {
        list.default_connection_id = if connection_id.trim().is_empty() { None } else { Some(connection_id) };
    }
    if let Some(double_opt_in) = input.double_opt_in {
        list.double_opt_in = double_opt_in;
    }
    if let Some(sender_name) = input.sender_name {
        list.sender_name = if sender_name.trim().is_empty() { None } else { Some(sender_name) };
    }
    if let Some(sender_email) = input.sender_email {
        list.sender_email = if sender_email.trim().is_empty() { None } else { Some(sender_email) };
    }
    if let Some(base_url) = input.base_url {
        list.base_url = if base_url.trim().is_empty() { None } else { Some(base_url) };
    }
    list.updated_at = now_string();
    let list = list.clone();
    drop(lists);
    api_json::success_response(list, 200)
}

async fn delete_list(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = path.into_inner();
    match lists_store().remove(&id) {
        Some(_) => {
            let subscriber_ids: Vec<String> = subscribers_store()
                .iter()
                .filter(|entry| entry.value().list_id == id)
                .map(|entry| entry.key().clone())
                .collect();
            for subscriber_id in &subscriber_ids {
                subscribers_store().remove(subscriber_id);
            }
            let campaign_ids: Vec<String> = campaigns_store()
                .iter()
                .filter(|entry| entry.value().list_id == id)
                .map(|entry| entry.key().clone())
                .collect();
            for campaign_id in &campaign_ids {
                campaigns_store().remove(campaign_id);
            }
            api_json::success_response(
                json!({
                    "deleted": true,
                    "id": id,
                    "removed_subscribers": subscriber_ids.len(),
                    "removed_campaigns": campaign_ids.len(),
                }),
                200,
            )
        }
        None => api_json::error_response(&UnifiedError::not_found("Newsletter list not found"), debug),
    }
}

// ---------------------------------------------------------------------------
// Subscriber handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SubscribeInput {
    email: String,
    name: Option<String>,
    metadata: Option<HashMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
struct SubscriberUpdateInput {
    name: Option<String>,
    status: Option<String>,
    metadata: Option<HashMap<String, Value>>,
}

async fn list_list_subscribers(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = state.is_debug();
    let list_id = path.into_inner();
    if !lists_store().contains_key(&list_id) {
        return api_json::error_response(&UnifiedError::not_found("Newsletter list not found"), debug);
    }
    let params = query.into_inner();
    let mut items: Vec<Subscriber> = subscribers_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|subscriber| {
            subscriber.list_id == list_id
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| subscriber.status == *status)
                && matches_search(&subscriber.email, &params)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    api_json::success_response(page.to_json(), 200)
}

async fn subscribe(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<SubscribeInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let list_id = path.into_inner();
    let input = body.into_inner();
    let Some(list) = lists_store().get(&list_id).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("Newsletter list not found"), debug);
    };
    if let Err(error) = validate_email_address(&input.email) {
        return api_json::error_response(&error, debug);
    }
    let email = input.email.trim().to_lowercase();
    let now = now_string();
    if let Some(existing) = find_subscriber(&list_id, &email) {
        // Re-subscribing an unsubscribed or bounced address starts the
        // flow again instead of silently reviving the old record.
        if existing.status == "confirmed" {
            return api_json::error_response(
                &UnifiedError::conflict("This email address is already subscribed to the list"),
                debug,
            );
        }
        let mut updated = existing.clone();
        updated.status = if list.double_opt_in { "pending".to_string() } else { "confirmed".to_string() };
        updated.name = input.name.or(updated.name);
        updated.updated_at = now;
        if updated.status == "confirmed" {
            updated.confirmed_at = Some(now.clone());
        }
        subscribers_store().insert(updated.id.clone(), updated.clone());
        return subscribe_response(&state, &list, updated);
    }
    let subscriber = Subscriber {
        id: Uuid::now_v7().to_string(),
        list_id: list.id.clone(),
        email,
        name: input.name,
        status: if list.double_opt_in { "pending".to_string() } else { "confirmed".to_string() },
        metadata: input.metadata.unwrap_or_default(),
        confirmed_at: if list.double_opt_in { None } else { Some(now.clone()) },
        unsubscribed_at: None,
        created_at: now.clone(),
        updated_at: now,
    };
    subscribers_store().insert(subscriber.id.clone(), subscriber.clone());
    state.emit(
        EVENT_NEWSLETTER_SUBSCRIBED,
        json!({
            "subscriber_id": subscriber.id,
            "list_id": list.id,
            "email": subscriber.email,
            "status": subscriber.status,
        }),
    );
    subscribe_response(&state, &list, subscriber)
}

/// Shared subscribe response: for double opt-in, build the signed
/// confirmation link and email it through the list's provider
/// connection (or the global default). The link is also returned so
/// an administrator can deliver it another way when no provider is
/// configured.
fn subscribe_response(
    state: &Arc<NewsletterState>,
    list: &NewsletterList,
    subscriber: Subscriber,
) -> HttpResponse {
    if !list.double_opt_in {
        state.emit(
            EVENT_NEWSLETTER_CONFIRMED,
            json!({
                "subscriber_id": subscriber.id,
                "list_id": list.id,
                "email": subscriber.email,
            }),
        );
        return api_json::success_response(
            json!({ "subscriber": subscriber, "confirmation_email_sent": false, "confirm_url": null }),
            201,
        );
    }
    let token = state.sign_token(&subscriber.id, "confirm", CONFIRM_TOKEN_TTL_SECS);
    let url = confirm_url(list, &token);
    let sent = request_send(
        state,
        json!({
            "connection_id": list.default_connection_id,
            "to": subscriber.email,
            "from": list.sender_email,
            "from_name": list.sender_name,
            "subject": format!("Please confirm your subscription to {}", list.name),
            "body_text": format!(
                "Please confirm your subscription to {} by opening this link (valid for 48 hours): {url}\n\nIf you did not subscribe, ignore this message.",
                list.name
            ),
            "body_html": format!(
                "<p>Please confirm your subscription to {} by clicking the link below (valid for 48 hours):</p><p><a href=\"{url}\">Confirm subscription</a></p><p>If you did not subscribe, ignore this message.</p>",
                list.name, url = url
            ),
        }),
    );
    api_json::success_response(
        json!({
            "subscriber": subscriber,
            "confirmation_email_sent": sent,
            "confirm_url": url,
        }),
        201,
    )
}

async fn get_subscriber(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match subscribers_store().get(path.as_str()).map(|entry| entry.value().clone()) {
        Some(subscriber) => api_json::success_response(subscriber, 200),
        None => api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug),
    }
}

async fn update_subscriber(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<SubscriberUpdateInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    if let Some(status) = &input.status {
        if !matches!(status.as_str(), "pending" | "confirmed" | "unsubscribed" | "bounced" | "complained") {
            return api_json::error_response(
                &UnifiedError::bad_request("status must be one of: pending, confirmed, unsubscribed, bounced, complained"),
                debug,
            );
        }
    }
    let mut store = subscribers_store();
    let Some(mut subscriber) = store.get_mut(path.as_str()) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug);
    };
    if let Some(name) = input.name {
        subscriber.name = if name.trim().is_empty() { None } else { Some(name) };
    }
    if let Some(status) = input.status {
        subscriber.status = status;
        if subscriber.status == "confirmed" && subscriber.confirmed_at.is_none() {
            subscriber.confirmed_at = Some(now_string());
        }
        if subscriber.status == "unsubscribed" {
            subscriber.unsubscribed_at = Some(now_string());
        }
    }
    if let Some(metadata) = input.metadata {
        subscriber.metadata = metadata;
    }
    subscriber.updated_at = now_string();
    let subscriber = subscriber.clone();
    drop(store);
    api_json::success_response(subscriber, 200)
}

async fn delete_subscriber(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match subscribers_store().remove(path.as_str()) {
        Some(_) => api_json::success_response(
            json!({ "deleted": true, "id": path.as_str() }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug),
    }
}

async fn confirm_subscriber(
    state: web::Data<Arc<NewsletterState>>,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = state.is_debug();
    let Some(token) = query.get("token") else {
        return api_json::error_response(
            &UnifiedError::bad_request("Query parameter token is required"),
            debug,
        );
    };
    confirm_with_token(&state, token)
}

#[derive(Debug, Deserialize)]
struct ConfirmInput {
    token: String,
}

async fn confirm_subscriber_post(
    state: web::Data<Arc<NewsletterState>>,
    body: web::Json<ConfirmInput>,
) -> HttpResponse {
    confirm_with_token(&state, &body.into_inner().token)
}

fn confirm_with_token(state: &Arc<NewsletterState>, token: &str) -> HttpResponse {
    let debug = state.is_debug();
    let (subscriber_id, purpose) = match state.verify_token(token) {
        Ok(pair) => pair,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if purpose != "confirm" {
        return api_json::error_response(
            &UnifiedError::bad_request("This token is not a confirmation token"),
            debug,
        );
    }
    let mut store = subscribers_store();
    let Some(mut subscriber) = store.get_mut(&subscriber_id) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug);
    };
    if subscriber.status == "confirmed" {
        let subscriber = subscriber.clone();
        drop(store);
        return api_json::success_response(
            json!({ "subscriber": subscriber, "already_confirmed": true }),
            200,
        );
    }
    if subscriber.status == "unsubscribed" || subscriber.status == "bounced" || subscriber.status == "complained" {
        return api_json::error_response(
            &UnifiedError::conflict(format!(
                "Subscriber cannot be confirmed while status is \"{}\"",
                subscriber.status
            )),
            debug,
        );
    }
    subscriber.status = "confirmed".to_string();
    subscriber.confirmed_at = Some(now_string());
    subscriber.updated_at = now_string();
    let subscriber = subscriber.clone();
    drop(store);
    state.emit(
        EVENT_NEWSLETTER_CONFIRMED,
        json!({
            "subscriber_id": subscriber.id,
            "list_id": subscriber.list_id,
            "email": subscriber.email,
        }),
    );
    api_json::success_response(
        json!({ "subscriber": subscriber, "confirmed": true }),
        200,
    )
}

async fn resend_confirm(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let Some(subscriber) = subscribers_store().get(path.as_str()).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug);
    };
    if subscriber.status == "confirmed" {
        return api_json::error_response(
            &UnifiedError::conflict("Subscriber is already confirmed"),
            debug,
        );
    }
    let Some(list) = lists_store().get(&subscriber.list_id).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber list not found"), debug);
    };
    let token = state.sign_token(&subscriber.id, "confirm", CONFIRM_TOKEN_TTL_SECS);
    let url = confirm_url(&list, &token);
    let sent = request_send(
        state,
        json!({
            "connection_id": list.default_connection_id,
            "to": subscriber.email,
            "from": list.sender_email,
            "from_name": list.sender_name,
            "subject": format!("Please confirm your subscription to {}", list.name),
            "body_text": format!(
                "Please confirm your subscription to {} by opening this link (valid for 48 hours): {url}",
                list.name
            ),
            "body_html": format!(
                "<p>Please confirm your subscription to {} by clicking the link below (valid for 48 hours):</p><p><a href=\"{url}\">Confirm subscription</a></p>",
                list.name, url = url
            ),
        }),
    );
    api_json::success_response(
        json!({ "subscriber_id": subscriber.id, "confirmation_email_sent": sent, "confirm_url": url }),
        200,
    )
}

async fn unsubscribe_subscriber(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let mut store = subscribers_store();
    let Some(mut subscriber) = store.get_mut(path.as_str()) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug);
    };
    subscriber.status = "unsubscribed".to_string();
    subscriber.unsubscribed_at = Some(now_string());
    subscriber.updated_at = now_string();
    let subscriber = subscriber.clone();
    drop(store);
    state.emit(
        EVENT_NEWSLETTER_UNSUBSCRIBED,
        json!({
            "subscriber_id": subscriber.id,
            "list_id": subscriber.list_id,
            "email": subscriber.email,
            "via": "admin",
        }),
    );
    api_json::success_response(json!({ "subscriber": subscriber, "unsubscribed": true }), 200)
}

async fn unsubscribe_via_link(
    state: web::Data<Arc<NewsletterState>>,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = state.is_debug();
    let Some(token) = query.get("token") else {
        return api_json::error_response(
            &UnifiedError::bad_request("Query parameter token is required"),
            debug,
        );
    };
    let (subscriber_id, purpose) = match state.verify_token(token) {
        Ok(pair) => pair,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if purpose != "unsubscribe" {
        return api_json::error_response(
            &UnifiedError::bad_request("This token is not an unsubscribe token"),
            debug,
        );
    }
    let mut store = subscribers_store();
    let Some(mut subscriber) = store.get_mut(&subscriber_id) else {
        return api_json::error_response(&UnifiedError::not_found("Subscriber not found"), debug);
    };
    subscriber.status = "unsubscribed".to_string();
    subscriber.unsubscribed_at = Some(now_string());
    subscriber.updated_at = now_string();
    let subscriber = subscriber.clone();
    drop(store);
    state.emit(
        EVENT_NEWSLETTER_UNSUBSCRIBED,
        json!({
            "subscriber_id": subscriber.id,
            "list_id": subscriber.list_id,
            "email": subscriber.email,
            "via": "link",
        }),
    );
    api_json::success_response(json!({ "subscriber": subscriber, "unsubscribed": true }), 200)
}

// ---------------------------------------------------------------------------
// Campaign handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CampaignInput {
    list_id: String,
    name: String,
    template_id: String,
    subject: Option<String>,
    preview_text: Option<String>,
    connection_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CampaignUpdateInput {
    name: Option<String>,
    template_id: Option<String>,
    subject: Option<String>,
    preview_text: Option<String>,
    connection_id: Option<String>,
}

async fn list_campaigns(query: web::Query<PaginationParams>) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Campaign> = campaigns_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|campaign| {
            matches_search(&format!("{} {}", campaign.name, campaign.subject), &params)
                && params
                    .filters
                    .get("list_id")
                    .is_none_or(|list_id| campaign.list_id == *list_id)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| campaign.status == *status)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    api_json::success_response(page.to_json(), 200)
}

async fn create_campaign(
    state: web::Data<Arc<NewsletterState>>,
    body: web::Json<CampaignInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    if input.name.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("Campaign name must not be empty"),
            debug,
        );
    }
    if input.template_id.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("template_id must not be empty (a GrapeJS email template id)"),
            debug,
        );
    }
    if let Err(error) = validate_connection_id(&input.connection_id) {
        return api_json::error_response(&error, debug);
    }
    if !lists_store().contains_key(&input.list_id) {
        return api_json::error_response(
            &UnifiedError::not_found("Newsletter list not found"),
            debug,
        );
    }
    let now = now_string();
    let campaign = Campaign {
        id: Uuid::now_v7().to_string(),
        list_id: input.list_id,
        name: input.name.trim().to_string(),
        template_id: input.template_id.trim().to_string(),
        subject: input.subject.unwrap_or_default(),
        preview_text: input.preview_text.filter(|v| !v.trim().is_empty()),
        connection_id: input.connection_id.filter(|v| !v.trim().is_empty()),
        status: "draft".to_string(),
        scheduled_at: None,
        sent_at: None,
        stats: CampaignStats::default(),
        created_at: now.clone(),
        updated_at: now,
    };
    campaigns_store().insert(campaign.id.clone(), campaign.clone());
    api_json::success_response(campaign, 201)
}

async fn get_campaign(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match campaigns_store().get(path.as_str()).map(|entry| entry.value().clone()) {
        Some(campaign) => api_json::success_response(campaign, 200),
        None => api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug),
    }
}

async fn update_campaign(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<CampaignUpdateInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    if let Err(error) = validate_connection_id(&input.connection_id) {
        return api_json::error_response(&error, debug);
    }
    let mut store = campaigns_store();
    let Some(mut campaign) = store.get_mut(path.as_str()) else {
        return api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug);
    };
    if campaign.status != "draft" && campaign.status != "scheduled" {
        return api_json::error_response(
            &UnifiedError::conflict(format!(
                "Only draft or scheduled campaigns can be updated (status is \"{}\")",
                campaign.status
            )),
            debug,
        );
    }
    if let Some(name) = input.name {
        if name.trim().is_empty() {
            return api_json::error_response(&UnifiedError::bad_request("Campaign name must not be empty"), debug);
        }
        campaign.name = name.trim().to_string();
    }
    if let Some(template_id) = input.template_id {
        campaign.template_id = template_id.trim().to_string();
    }
    if let Some(subject) = input.subject {
        campaign.subject = subject;
    }
    if let Some(preview_text) = input.preview_text {
        campaign.preview_text = if preview_text.trim().is_empty() { None } else { Some(preview_text) };
    }
    if let Some(connection_id) = input.connection_id {
        campaign.connection_id = if connection_id.trim().is_empty() { None } else { Some(connection_id) };
    }
    campaign.updated_at = now_string();
    let campaign = campaign.clone();
    drop(store);
    api_json::success_response(campaign, 200)
}

async fn delete_campaign(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match campaigns_store().remove(path.as_str()) {
        Some(_) => api_json::success_response(
            json!({ "deleted": true, "id": path.as_str() }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug),
    }
}

#[derive(Debug, Deserialize)]
struct ScheduleInput {
    scheduled_at: String,
}

async fn schedule_campaign(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<ScheduleInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    let scheduled_at = match DateTime::parse_from_rfc3339(&input.scheduled_at) {
        Ok(when) => when.with_timezone(&Utc),
        Err(_) => {
            return api_json::error_response(
                &UnifiedError::bad_request("scheduled_at must be an RFC 3339 timestamp, for example 2026-12-01T09:00:00Z"),
                debug,
            )
        }
    };
    if scheduled_at <= Utc::now() {
        return api_json::error_response(
            &UnifiedError::bad_request("scheduled_at must be in the future (use send-now for immediate delivery)"),
            debug,
        );
    }
    let mut store = campaigns_store();
    let Some(mut campaign) = store.get_mut(path.as_str()) else {
        return api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug);
    };
    if campaign.status != "draft" && campaign.status != "scheduled" {
        return api_json::error_response(
            &UnifiedError::conflict(format!(
                "Only draft campaigns can be scheduled (status is \"{}\")",
                campaign.status
            )),
            debug,
        );
    }
    campaign.status = "scheduled".to_string();
    campaign.scheduled_at = Some(scheduled_at.to_rfc3339());
    campaign.updated_at = now_string();
    let campaign = campaign.clone();
    drop(store);
    // Hand execution to the Scheduler module. The scheduler only knows
    // its built-in kinds today, so it may reject `newsletter.send`;
    // the campaign stays scheduled either way and the
    // `newsletter.campaign.send` hook is the execution entry point.
    let mut scheduler_accepted = false;
    let mut scheduler_note: Option<String> = None;
    if let Some(hooks) = state.hooks() {
        let answer = hooks.emit(
            EVENT_SCHEDULER_ENQUEUE,
            json!({
                "kind": "newsletter.send",
                "payload": { "campaign_id": campaign.id },
            }),
        );
        match answer.results.first() {
            Some(result) if result.get("error").is_none() => {
                scheduler_accepted = true;
            }
            Some(result) => {
                scheduler_note = result
                    .get("error")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
            }
            None => {
                scheduler_note = Some("Scheduler did not answer the scheduler.enqueue event".to_string());
            }
        }
    } else {
        scheduler_note = Some("No hook bus is available".to_string());
    }
    api_json::success_response(
        json!({
            "campaign": campaign,
            "scheduler_accepted": scheduler_accepted,
            "scheduler_note": scheduler_note,
        }),
        200,
    )
}

async fn send_now_campaign(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let state: Arc<NewsletterState> = state.into_inner();
    match send_campaign(&state, &path.into_inner()).await {
        Ok(result) => api_json::success_response(result, 200),
        Err(error) => api_json::error_response(&error, debug),
    }
}

#[derive(Debug, Deserialize)]
struct CampaignTestSendInput {
    to: String,
}

async fn test_send_campaign(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<CampaignTestSendInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    if let Err(error) = validate_email_address(&input.to) {
        return api_json::error_response(&error, debug);
    }
    let Some(campaign) = campaigns_store().get(path.as_str()).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug);
    };
    let Some(list) = lists_store().get(&campaign.list_id).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("Campaign list not found"), debug);
    };
    let (rendered_html, rendered_subject) = match render_template(&state, &campaign.template_id, &HashMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let subject = if campaign.subject.trim().is_empty() {
        rendered_subject
    } else {
        campaign.subject.clone()
    };
    let sent = request_send(
        &state,
        json!({
            "connection_id": campaign.connection_id.clone().or_else(|| list.default_connection_id.clone()),
            "to": input.to,
            "from": list.sender_email,
            "from_name": list.sender_name,
            "subject": format!("[TEST] {subject}"),
            "body_html": rendered_html,
            "metadata": { "campaign_id": campaign.id, "test": "true" },
        }),
    );
    if sent {
        api_json::success_response(
            json!({ "queued": true, "campaign_id": campaign.id, "to": input.to }),
            200,
        )
    } else {
        api_json::error_response(
            &UnifiedError::new(
                "No email provider answered the email.send_requested event: no provider handler is registered",
                503,
            ),
            debug,
        )
    }
}

async fn campaign_stats(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match campaigns_store().get(path.as_str()).map(|entry| entry.value().clone()) {
        Some(campaign) => api_json::success_response(
            json!({
                "campaign_id": campaign.id,
                "status": campaign.status,
                "stats": campaign.stats.view(),
            }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug),
    }
}

// ---------------------------------------------------------------------------
// Tracking.
// ---------------------------------------------------------------------------

fn record_tracking(
    state: &NewsletterState,
    campaign_id: &str,
    subscriber_id: &str,
    kind: &str,
    url: Option<String>,
) {
    let event = TrackingEvent {
        id: Uuid::now_v7().to_string(),
        campaign_id: campaign_id.to_string(),
        subscriber_id: subscriber_id.to_string(),
        kind: kind.to_string(),
        url: url.clone(),
        created_at: now_string(),
    };
    let event_id = event.id.clone();
    tracking_store().insert(event.id.clone(), event);
    // Unique counts: one per subscriber per campaign per kind.
    let is_first = !tracking_store()
        .iter()
        .any(|entry| {
            let existing = entry.value();
            existing.campaign_id == campaign_id
                && existing.subscriber_id == subscriber_id
                && existing.kind == kind
                && existing.id != event_id
        });
    if is_first {
        if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
            match kind {
                "open" => {
                    campaign.stats.opened += 1;
                    campaign.stats.unique_opened += 1;
                }
                "click" => {
                    campaign.stats.clicked += 1;
                    campaign.stats.unique_clicked += 1;
                }
                _ => {}
            }
        }
    } else if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
        match kind {
            "open" => campaign.stats.opened += 1,
            "click" => campaign.stats.clicked += 1,
            _ => {}
        }
    }
    let event_name = if kind == "open" {
        EVENT_NEWSLETTER_OPEN
    } else {
        EVENT_NEWSLETTER_CLICK
    };
    state.emit(
        event_name,
        json!({
            "campaign_id": campaign_id,
            "subscriber_id": subscriber_id,
            "url": url,
        }),
    );
}

async fn track_open(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (campaign_id, subscriber_id) = path.into_inner();
    record_tracking(&state, &campaign_id, &subscriber_id, "open", None);
    HttpResponse::Ok()
        .content_type("image/gif")
        .body(TRACKING_PIXEL)
}

async fn track_click(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<(String, String)>,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = state.is_debug();
    let (campaign_id, subscriber_id) = path.into_inner();
    let Some(target) = query.get("url") else {
        return api_json::error_response(
            &UnifiedError::bad_request("Query parameter url is required"),
            debug,
        );
    };
    let url = match Url::parse(target) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() => url,
        _ => {
            return api_json::error_response(
                &UnifiedError::bad_request("url must be an http(s) URL with a host"),
                debug,
            )
        }
    };
    if !url.username().is_empty() || url.password().is_some() {
        return api_json::error_response(
            &UnifiedError::bad_request("url must not embed credentials"),
            debug,
        );
    }
    record_tracking(
        &state,
        &campaign_id,
        &subscriber_id,
        "click",
        Some(url.to_string()),
    );
    HttpResponse::Found()
        .insert_header((actix_web::http::header::LOCATION, url.to_string()))
        .finish()
}

// ---------------------------------------------------------------------------
// Provider webhooks.
// ---------------------------------------------------------------------------

/// One normalized webhook event, mapped honestly from what each
/// provider actually delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebhookAction {
    Delivered,
    Opened,
    Clicked,
    Bounced,
    Complained,
    Unsubscribed,
    /// Seen and logged, but no subscriber or stats change (for
    /// example a soft bounce or a deferred delivery).
    LogOnly,
}

#[derive(Debug, Clone)]
struct NormalizedEvent {
    email: String,
    action: WebhookAction,
    campaign_id: Option<String>,
    subscriber_id: Option<String>,
}

fn metadata_ids(value: &Value) -> (Option<String>, Option<String>) {
    let campaign_id = value
        .get("campaign_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|v| v.get("campaign_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    let subscriber_id = value
        .get("subscriber_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|v| v.get("subscriber_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    (campaign_id, subscriber_id)
}

fn normalize_webhook_events(provider: &str, body: &Value) -> Vec<NormalizedEvent> {
    let mut events = Vec::new();
    let mut push = |events: &mut Vec<NormalizedEvent>,
                    email: &str,
                    action: WebhookAction,
                    campaign_id: Option<String>,
                    subscriber_id: Option<String>| {
        if email.trim().is_empty() {
            return;
        }
        events.push(NormalizedEvent {
            email: email.trim().to_lowercase(),
            action,
            campaign_id,
            subscriber_id,
        });
    };
    match provider {
        "sendgrid" => {
            // SendGrid posts a JSON array of event objects.
            if let Some(items) = body.as_array() {
                for item in items {
                    let email = item.get("email").and_then(|v| v.as_str()).unwrap_or("");
                    let (campaign_id, subscriber_id) = metadata_ids(item);
                    let action = match item.get("event").and_then(|v| v.as_str()) {
                        Some("delivered") => WebhookAction::Delivered,
                        Some("open") => WebhookAction::Opened,
                        Some("click") => WebhookAction::Clicked,
                        Some("bounce") | Some("dropped") => WebhookAction::Bounced,
                        Some("spamreport") => WebhookAction::Complained,
                        Some("unsubscribe") => WebhookAction::Unsubscribed,
                        _ => WebhookAction::LogOnly,
                    };
                    push(&mut events, email, action, campaign_id, subscriber_id);
                }
            }
        }
        "brevo" => {
            // Brevo posts one JSON object per event.
            let email = body.get("email").and_then(|v| v.as_str()).unwrap_or("");
            let action = match body.get("event").and_then(|v| v.as_str()) {
                Some("delivered") | Some("sent") => WebhookAction::Delivered,
                Some("opened") => WebhookAction::Opened,
                Some("clicked") => WebhookAction::Clicked,
                Some("hardBounce") | Some("invalid") | Some("blocked") => WebhookAction::Bounced,
                Some("spam") => WebhookAction::Complained,
                Some("unsubscribed") => WebhookAction::Unsubscribed,
                _ => WebhookAction::LogOnly,
            };
            push(&mut events, email, action, None, None);
        }
        "mailgun" => {
            // Mailgun posts form fields, or JSON with an `event-data`
            // object (the endpoint normalizes form bodies to JSON).
            let data = body.get("event-data").unwrap_or(body);
            let email = data
                .get("recipient")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("email").and_then(|v| v.as_str()))
                .unwrap_or("");
            let (campaign_id, subscriber_id) = metadata_ids(data);
            let action = match data.get("event").and_then(|v| v.as_str()) {
                Some("delivered") => WebhookAction::Delivered,
                Some("opened") => WebhookAction::Opened,
                Some("clicked") => WebhookAction::Clicked,
                Some("complained") => WebhookAction::Complained,
                Some("unsubscribed") => WebhookAction::Unsubscribed,
                Some("bounced") => WebhookAction::Bounced,
                Some("failed") => {
                    let permanent = data
                        .get("severity")
                        .and_then(|v| v.as_str())
                        .is_some_and(|severity| severity == "permanent");
                    if permanent {
                        WebhookAction::Bounced
                    } else {
                        WebhookAction::LogOnly
                    }
                }
                _ => WebhookAction::LogOnly,
            };
            push(&mut events, email, action, campaign_id, subscriber_id);
        }
        "postmark" => {
            // Postmark posts one JSON object per event.
            let email = body.get("Email").and_then(|v| v.as_str()).unwrap_or("");
            let metadata = body.get("Metadata").cloned().unwrap_or(Value::Null);
            let (campaign_id, subscriber_id) = metadata_ids(&metadata);
            let action = match body.get("RecordType").and_then(|v| v.as_str()) {
                Some("Delivery") => WebhookAction::Delivered,
                Some("Open") => WebhookAction::Opened,
                Some("Click") => WebhookAction::Clicked,
                Some("Bounce") => {
                    let hard = body
                        .get("Type")
                        .and_then(|v| v.as_str())
                        .is_some_and(|bounce_type| bounce_type == "HardBounce");
                    if hard {
                        WebhookAction::Bounced
                    } else {
                        WebhookAction::LogOnly
                    }
                }
                Some("SpamComplaint") => WebhookAction::Complained,
                Some("SubscriptionChange") => {
                    match body.get("SuppressionReason").and_then(|v| v.as_str()) {
                        Some("RecipientSpamComplaint") => WebhookAction::Complained,
                        Some("RecipientUnsubscribed") => WebhookAction::Unsubscribed,
                        _ => WebhookAction::LogOnly,
                    }
                }
                _ => WebhookAction::LogOnly,
            };
            push(&mut events, email, action, campaign_id, subscriber_id);
        }
        "ses" => {
            // SES delivers SNS envelopes: { "Message": "{...}" }.
            // Event-publishing messages (Open/Click) arrive as plain
            // JSON with `eventType` instead.
            if let Some(event_type) = body.get("eventType").and_then(|v| v.as_str()) {
                let email = body
                    .get("mail")
                    .and_then(|v| v.get("destination"))
                    .and_then(|v| v.as_array())
                    .and_then(|items| items.first())
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let action = match event_type {
                    "Delivery" => WebhookAction::Delivered,
                    "Open" => WebhookAction::Opened,
                    "Click" => WebhookAction::Clicked,
                    "Bounce" => WebhookAction::Bounced,
                    "Complaint" => WebhookAction::Complained,
                    _ => WebhookAction::LogOnly,
                };
                push(&mut events, email, action, None, None);
            } else if let Some(message) = body.get("Message").and_then(|v| v.as_str()) {
                if let Ok(inner) = serde_json::from_str::<Value>(message) {
                    let notification_type = inner
                        .get("notificationType")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match notification_type {
                        "Bounce" => {
                            let permanent = inner
                                .get("bounce")
                                .and_then(|v| v.get("bounceType"))
                                .and_then(|v| v.as_str())
                                .is_some_and(|bounce_type| bounce_type == "Permanent");
                            let action = if permanent {
                                WebhookAction::Bounced
                            } else {
                                WebhookAction::LogOnly
                            };
                            if let Some(recipients) = inner
                                .get("bounce")
                                .and_then(|v| v.get("bouncedRecipients"))
                                .and_then(|v| v.as_array())
                            {
                                for recipient in recipients {
                                    let email = recipient
                                        .get("emailAddress")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    push(&mut events, email, action, None, None);
                                }
                            }
                        }
                        "Complaint" => {
                            if let Some(recipients) = inner
                                .get("complaint")
                                .and_then(|v| v.get("complainedRecipients"))
                                .and_then(|v| v.as_array())
                            {
                                for recipient in recipients {
                                    let email = recipient
                                        .get("emailAddress")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    push(
                                        &mut events,
                                        email,
                                        WebhookAction::Complained,
                                        None,
                                        None,
                                    );
                                }
                            }
                        }
                        "Delivery" => {
                            if let Some(recipients) = inner
                                .get("mail")
                                .and_then(|v| v.get("destination"))
                                .and_then(|v| v.as_array())
                            {
                                for recipient in recipients {
                                    let email = recipient.as_str().unwrap_or("");
                                    push(&mut events, email, WebhookAction::Delivered, None, None);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        "resend" => {
            // Resend posts { "type": "email.delivered", "data": {...} }.
            let event_type = body.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let data = body.get("data").unwrap_or(body);
            let email = data
                .get("to")
                .and_then(|v| v.as_array())
                .and_then(|items| items.first())
                .and_then(|v| v.as_str())
                .or_else(|| data.get("email").and_then(|v| v.as_str()))
                .unwrap_or("");
            let (campaign_id, subscriber_id) = metadata_ids(data);
            let action = match event_type {
                "email.delivered" | "email.sent" => WebhookAction::Delivered,
                "email.opened" => WebhookAction::Opened,
                "email.clicked" => WebhookAction::Clicked,
                "email.bounced" => WebhookAction::Bounced,
                "email.complained" => WebhookAction::Complained,
                _ => WebhookAction::LogOnly,
            };
            push(&mut events, email, action, campaign_id, subscriber_id);
        }
        "sparkpost" => {
            // SparkPost posts a JSON array of { "msys": { "message_event": {...} } }.
            let items: Vec<&Value> = match body.as_array() {
                Some(items) => items.iter().collect(),
                None => vec![body],
            };
            for item in items {
                let event = item
                    .get("msys")
                    .and_then(|v| v.get("message_event"))
                    .unwrap_or(item);
                let email = event
                    .get("rcpt_to")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let metadata = event.get("metadata").cloned().unwrap_or(Value::Null);
                let (campaign_id, subscriber_id) = metadata_ids(&metadata);
                let action = match event.get("type").and_then(|v| v.as_str()) {
                    Some("delivery") => WebhookAction::Delivered,
                    Some("open") => WebhookAction::Opened,
                    Some("click") => WebhookAction::Clicked,
                    Some("spam_complaint") => WebhookAction::Complained,
                    Some("unsubscribe") | Some("list_unsubscribe") => {
                        WebhookAction::Unsubscribed
                    }
                    Some("bounce") => {
                        // SparkPost bounce classes 10, 30, 90 are
                        // permanent (undetermined, invalid, blocked);
                        // 20 and 40 are transient.
                        let permanent = event
                            .get("bounce_class")
                            .and_then(|v| v.as_u64())
                            .is_some_and(|class| matches!(class, 10 | 30 | 90));
                        if permanent {
                            WebhookAction::Bounced
                        } else {
                            WebhookAction::LogOnly
                        }
                    }
                    _ => WebhookAction::LogOnly,
                };
                push(&mut events, email, action, campaign_id, subscriber_id);
            }
        }
        "mailchimp" => {
            // Mandrill posts a form field `mandrill_events` holding a
            // JSON array of { "event", "msg": { "email", "metadata" } }.
            let items = body
                .get("mandrill_events")
                .and_then(|v| v.as_str())
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            for item in items {
                let msg = item.get("msg").unwrap_or(&item);
                let email = msg.get("email").and_then(|v| v.as_str()).unwrap_or("");
                let (campaign_id, subscriber_id) = metadata_ids(msg);
                let action = match item.get("event").and_then(|v| v.as_str()) {
                    Some("send") => WebhookAction::Delivered,
                    Some("open") => WebhookAction::Opened,
                    Some("click") => WebhookAction::Clicked,
                    Some("hard_bounce") => WebhookAction::Bounced,
                    Some("spam") => WebhookAction::Complained,
                    Some("unsub") => WebhookAction::Unsubscribed,
                    _ => WebhookAction::LogOnly,
                };
                push(&mut events, email, action, campaign_id, subscriber_id);
            }
        }
        _ => {}
    }
    events
}

fn apply_webhook_event(state: &NewsletterState, event: &NormalizedEvent) -> String {
    if event.action == WebhookAction::LogOnly {
        return "logged".to_string();
    }
    if let (Some(campaign_id), Some(subscriber_id)) =
        (&event.campaign_id, &event.subscriber_id)
    {
        match event.action {
            WebhookAction::Opened => {
                record_tracking(state, campaign_id, subscriber_id, "open", None);
                return "tracked_open".to_string();
            }
            WebhookAction::Clicked => {
                record_tracking(state, campaign_id, subscriber_id, "click", None);
                return "tracked_click".to_string();
            }
            _ => {}
        }
    }
    let Some(subscriber) = find_subscriber_by_email(&event.email) else {
        return "unknown_subscriber".to_string();
    };
    match event.action {
        WebhookAction::Delivered => {
            if let Some(campaign_id) = &event.campaign_id {
                if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
                    campaign.stats.delivered += 1;
                }
            }
            "delivered".to_string()
        }
        WebhookAction::Bounced => {
            apply_subscriber_status(state, &subscriber, "bounced");
            if let Some(campaign_id) = &event.campaign_id {
                if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
                    campaign.stats.bounced += 1;
                }
            }
            "bounced".to_string()
        }
        WebhookAction::Complained => {
            apply_subscriber_status(state, &subscriber, "complained");
            if let Some(campaign_id) = &event.campaign_id {
                if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
                    campaign.stats.complaints += 1;
                }
            }
            "complained".to_string()
        }
        WebhookAction::Unsubscribed => {
            apply_subscriber_status(state, &subscriber, "unsubscribed");
            if let Some(campaign_id) = &event.campaign_id {
                if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
                    campaign.stats.unsubscribed += 1;
                }
            }
            "unsubscribed".to_string()
        }
        WebhookAction::Opened | WebhookAction::Clicked => "tracked".to_string(),
        WebhookAction::LogOnly => "logged".to_string(),
    }
}

fn apply_subscriber_status(state: &NewsletterState, subscriber: &Subscriber, status: &str) {
    if let Some(mut stored) = subscribers_store().get_mut(&subscriber.id) {
        stored.status = status.to_string();
        if status == "unsubscribed" {
            stored.unsubscribed_at = Some(now_string());
        }
        stored.updated_at = now_string();
    }
    if status == "unsubscribed" {
        state.emit(
            EVENT_NEWSLETTER_UNSUBSCRIBED,
            json!({
                "subscriber_id": subscriber.id,
                "list_id": subscriber.list_id,
                "email": subscriber.email,
                "via": "webhook",
            }),
        );
    }
}

async fn provider_webhook(
    state: web::Data<Arc<NewsletterState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<HashMap<String, String>>,
    body: web::Bytes,
) -> HttpResponse {
    let debug = state.is_debug();
    let provider = path.into_inner();
    if !matches!(
        provider.as_str(),
        "mailchimp" | "brevo" | "sendgrid" | "mailgun" | "postmark" | "ses" | "resend" | "sparkpost"
    ) {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Unknown webhook provider \"{provider}\" (SMTP has no webhooks)"
            )),
            debug,
        );
    }
    // Verify the signature through the Email Providers module before
    // any processing. The raw body is passed through unchanged.
    let headers_object: HashMap<String, String> = req
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let body_text = String::from_utf8_lossy(&body).into_owned();
    let verified = match state.hooks() {
        Some(hooks) => {
            let answer = hooks.emit(
                EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY,
                json!({
                    "provider": provider,
                    "connection_id": query.get("connection_id"),
                    "headers": headers_object,
                    "body": body_text,
                }),
            );
            match answer.results.first() {
                Some(result) if result.get("error").is_none() => {
                    result.get("valid").and_then(|v| v.as_bool()).unwrap_or(false)
                }
                Some(result) => {
                    let reason = result
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("webhook verification failed");
                    return api_json::error_response(
                        &UnifiedError::unauthorized(format!("Webhook rejected: {reason}")),
                        debug,
                    );
                }
                None => {
                    return api_json::error_response(
                        &UnifiedError::new(
                            "Email provider service did not answer the email_provider.webhook_verify event",
                            503,
                        ),
                        debug,
                    )
                }
            }
        }
        None => {
            return api_json::error_response(
                &UnifiedError::new("Webhook verification is unavailable (no hook bus)", 503),
                debug,
            )
        }
    };
    if !verified {
        return api_json::error_response(
            &UnifiedError::unauthorized("Webhook signature verification failed"),
            debug,
        );
    }
    // Parse the body: JSON first, form-encoded second (Mailgun and
    // Mandrill post form bodies).
    let parsed: Value = serde_json::from_slice(&body).unwrap_or_else(|_| {
        let fields: HashMap<String, String> = url::form_urlencoded::parse(&body)
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        serde_json::to_value(fields).unwrap_or(Value::Null)
    });
    let events = normalize_webhook_events(&provider, &parsed);
    let mut outcomes: HashMap<String, u64> = HashMap::new();
    for event in &events {
        let outcome = apply_webhook_event(&state, event);
        *outcomes.entry(outcome).or_insert(0) += 1;
    }
    api_json::success_response(
        json!({
            "provider": provider,
            "received": events.len(),
            "outcomes": outcomes,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Overall stats.
// ---------------------------------------------------------------------------

async fn overall_stats() -> HttpResponse {
    let lists = lists_store().len() as u64;
    let subscribers = subscribers_store().len() as u64;
    let confirmed = subscribers_store()
        .iter()
        .filter(|entry| entry.value().status == "confirmed")
        .count() as u64;
    let campaigns = campaigns_store().len() as u64;
    let (sent, delivered, opened, clicked, bounced, unsubscribed, complaints) =
        campaigns_store().iter().fold(
            (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64),
            |acc, entry| {
                let stats = &entry.value().stats;
                (
                    acc.0 + stats.sent,
                    acc.1 + stats.delivered,
                    acc.2 + stats.opened,
                    acc.3 + stats.clicked,
                    acc.4 + stats.bounced,
                    acc.5 + stats.unsubscribed,
                    acc.6 + stats.complaints,
                )
            },
        );
    api_json::success_response(
        json!({
            "lists": lists,
            "subscribers": subscribers,
            "confirmed_subscribers": confirmed,
            "campaigns": campaigns,
            "sent": sent,
            "delivered": delivered,
            "opened": opened,
            "clicked": clicked,
            "bounced": bounced,
            "unsubscribed": unsubscribed,
            "complaints": complaints,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct NewsletterModule {
    state: Arc<NewsletterState>,
}

impl NewsletterModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(NewsletterState::new()),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(NewsletterModule::new()));
}

impl Module for NewsletterModule {
    fn id(&self) -> &'static str {
        "newsletter"
    }

    fn name(&self) -> &'static str {
        "Newsletter"
    }

    fn description(&self) -> &'static str {
        "Newsletter lists, double opt-in subscribers, and campaigns built from GrapeJS email templates, sent through the Email Providers module with open/click tracking and provider webhooks."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["email_providers", "email-templates", "scheduler"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Execution entry point for scheduled campaigns (called back
        // by the scheduler, or directly by an operator).
        let state = self.state.clone();
        bus.register(
            EVENT_NEWSLETTER_CAMPAIGN_SEND,
            "newsletter",
            Arc::new(move |ctx: &mut HookContext| {
                let campaign_id = ctx
                    .get("campaign_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                spawn_campaign_worker(state.clone(), campaign_id);
                ctx.add_result(json!({ "queued": true, "campaign_id": campaign_id }));
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/newsletter/lists", owner: "newsletter", description: "Paginated newsletter lists." },
            RouteInfo { method: "POST", path: "/api/v1/newsletter/lists", owner: "newsletter", description: "Create a newsletter list." },
            RouteInfo { method: "GET", path: "/api/v1/newsletter/lists/{id}", owner: "newsletter", description: "One newsletter list." },
            RouteInfo { method: "PUT", path: "/api/v1/newsletter/lists/{id}", owner: "newsletter", description: "Update a newsletter list." },
            RouteInfo { method: "DELETE", path: "/api/v1/newsletter/lists/{id}", owner: "newsletter", description: "Delete a newsletter list and its subscribers and campaigns." },
            RouteInfo { method: "GET", path: "/api/v1/newsletter/lists/{id}/subscribers", owner: "newsletter", description: "Paginated subscribers of one list." },
            RouteInfo { method: "POST", path: "/api/v1/newsletter/lists/{id}/subscribe", owner: "newsletter", description: "Subscribe an email address (double opt-in when the list requires it)." },
            RouteInfo { method: "GET", path: "/api/v1/newsletter/subscribers/{id}", owner: "newsletter", description: "One subscriber." },
            RouteInfo { method: "PUT", path: "/api/v1/newsletter/subscribers/{id}", owner: "newsletter", description: "Update a subscriber." },
            RouteInfo { method: "DELETE", path: "/api/v1/newsletter/subscribers/{id}", owner: "newsletter", description: "Delete a subscriber." },
            RouteInfo { method: "GET", path: "/api/v1/newsletter/subscribers/confirm", owner: "newsletter", description: "Confirm a double opt-in token." },
            RouteInfo { method: "POST", path: "/api/v1/newsletter/subscribers/confirm", owner: "newsletter", description: "Confirm a double opt-in token." },
            RouteInfo { method: "POST", path: "/api/v1/newsletter/subscribers/{id}/resend-confirm", owner: "newsletter", description: "Resend the confirmation email." },
            RouteInfo { method: "POST", path: "/api/v1/newsletter/subscribers/{id}/unsubscribe", owner: "newsletter", description: "Unsubscribe a subscriber." },
            RouteInfo { method: "GET", path: "/api/v1/newsletter/unsubscribe", owner: "newsletter", description: "Unsubscribe through a signed link." },
            RouteInfo { method: "GET", path: "/api/v1/newsletter/campaigns", owner: "newsletter", description: "Paginated campaigns." },
            RouteInfo { method: "POST", path: "/api/v1/newsletter/campaigns", owner: "newsletter", description: "Create a campaign from a GrapeJS template." },
            RouteInfo { method: "GET", path: "/api/v1/newsletter/campaigns/{id}", owner: "newsletter", description: "One campaign." },
            RouteInfo { method: "PUT", path: "/api/v1/newsletter/campaigns/{id}", owner: "newsletter", description: "Update a draft or scheduled campaign." },
            RouteInfo { method: "DELETE", path: "/api/v1/newsletter/campaigns/{id}", owner: "newsletter", description: "Delete a campaign." },
            RouteInfo { method: "POST", path: "/api/v1/newsletter/campaigns/{id}/schedule", owner: "newsletter", description: "Schedule a campaign through the scheduler." },
            RouteInfo { method: "POST", path: "/api/v1/newsletter/campaigns/{id}/send-now", owner: "newsletter", description: "Send a campaign immediately." },
            RouteInfo { method: "POST", path: "/api/v1/newsletter/campaigns/{id}/test-send", owner: "newsletter", description: "Send a test copy of a campaign to one address." },
            RouteInfo { method: "GET", path: "/api/v1/newsletter/campaigns/{id}/stats", owner: "newsletter", description: "Campaign stats with rates." },
            RouteInfo { method: "GET", path: "/api/v1/newsletter/track/open/{campaign_id}/{subscriber_id}", owner: "newsletter", description: "Open-tracking pixel." },
            RouteInfo { method: "GET", path: "/api/v1/newsletter/track/click/{campaign_id}/{subscriber_id}", owner: "newsletter", description: "Click-tracking redirect." },
            RouteInfo { method: "POST", path: "/api/v1/newsletter/webhooks/{provider}", owner: "newsletter", description: "Provider delivery/bounce/complaint webhook receiver." },
            RouteInfo { method: "GET", path: "/api/v1/newsletter/stats", owner: "newsletter", description: "Overall newsletter stats." },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/newsletter")
                .app_data(web::Data::new(state))
                .route("/stats", web::get().to(overall_stats))
                .route("/lists", web::get().to(list_lists))
                .route("/lists", web::post().to(create_list))
                .route("/lists/{id}", web::get().to(get_list))
                .route("/lists/{id}", web::put().to(update_list))
                .route("/lists/{id}", web::delete().to(delete_list))
                .route("/lists/{id}/subscribers", web::get().to(list_list_subscribers))
                .route("/lists/{id}/subscribe", web::post().to(subscribe))
                // Static segments first so `/subscribers/confirm` and
                // `/unsubscribe` are never captured by `/{id}` routes.
                .route("/subscribers/confirm", web::get().to(confirm_subscriber))
                .route("/subscribers/confirm", web::post().to(confirm_subscriber_post))
                .route("/unsubscribe", web::get().to(unsubscribe_via_link))
                .route("/subscribers/{id}", web::get().to(get_subscriber))
                .route("/subscribers/{id}", web::put().to(update_subscriber))
                .route("/subscribers/{id}", web::delete().to(delete_subscriber))
                .route("/subscribers/{id}/resend-confirm", web::post().to(resend_confirm))
                .route("/subscribers/{id}/unsubscribe", web::post().to(unsubscribe_subscriber))
                .route("/campaigns", web::get().to(list_campaigns))
                .route("/campaigns", web::post().to(create_campaign))
                .route("/campaigns/{id}", web::get().to(get_campaign))
                .route("/campaigns/{id}", web::put().to(update_campaign))
                .route("/campaigns/{id}", web::delete().to(delete_campaign))
                .route("/campaigns/{id}/schedule", web::post().to(schedule_campaign))
                .route("/campaigns/{id}/send-now", web::post().to(send_now_campaign))
                .route("/campaigns/{id}/test-send", web::post().to(test_send_campaign))
                .route("/campaigns/{id}/stats", web::get().to(campaign_stats))
                .route("/track/open/{campaign_id}/{subscriber_id}", web::get().to(track_open))
                .route("/track/click/{campaign_id}/{subscriber_id}", web::get().to(track_click))
                .route("/webhooks/{provider}", web::post().to(provider_webhook)),
        );
    }
}
