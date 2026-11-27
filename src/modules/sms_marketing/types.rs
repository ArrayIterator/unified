//! SMS marketing types: providers, plain-text templates,
//! contact lists, campaigns, messages, opt-outs, rate
//! limits, and the audit trail, plus the pure functions
//! (segment counting, template substitution, the status
//! machines, opt-out keyword matching, segment filters,
//! rate-limit math, phone normalization).
//!
//! Nothing about the SMS content is hard-coded: providers
//! (endpoint, auth type, credentials, sender id, rate
//! limit), templates, contact lists, opt-out keywords, and
//! rate limits are all data rows an administrator enters.
//! No real provider name or credential is baked in - the
//! only provider kinds are the generic `http` adapter
//! (POST JSON to the configured endpoint with the
//! configured auth) and the `log_only` test provider, which
//! records a send without any network call. Live delivery
//! depends entirely on a configured provider with real
//! credentials; this module seeds no provider and no
//! credential. Money never appears here: no price, cost,
//! or charge is stored or returned, so `f64` is never used
//! for a stored or returned number anywhere in this module.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_URL_LEN: usize = 2000;

pub(crate) const MAX_SENDER_ID_LEN: usize = 20;

pub(crate) const MAX_PHONE_LEN: usize = 20;

pub(crate) const MAX_TEMPLATE_BODY_LEN: usize = 5000;

pub(crate) const MAX_VARIABLE_NAME_LEN: usize = 64;

pub(crate) const MAX_KEYWORD_LEN: usize = 32;

pub(crate) const MAX_REFERENCE_LEN: usize = 200;

/// Maximum contact lists referenced by one campaign.
pub(crate) const MAX_CAMPAIGN_LISTS: usize = 50;

/// Maximum explicit recipient numbers on one campaign.
pub(crate) const MAX_CAMPAIGN_NUMBERS: usize = 10_000;

/// Maximum contacts on one list (enforced at insert).
pub(crate) const MAX_LIST_CONTACTS: usize = 100_000;

/// Maximum tags on one contact.
pub(crate) const MAX_CONTACT_TAGS: usize = 50;

/// Rate-limit window in seconds (one minute).
pub(crate) const RATE_LIMIT_WINDOW_SECS: i64 = 60;

/// Default per-tenant sends per minute when no settings
/// row exists yet.
pub(crate) const DEFAULT_TENANT_SENDS_PER_MINUTE: i64 = 60;

/// Default per-provider sends per minute on create.
pub(crate) const DEFAULT_PROVIDER_SENDS_PER_MINUTE: i64 = 60;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_SMS_VIEW: &str = "sms_marketing.view";

pub(crate) const PERM_SMS_MANAGE: &str = "sms_marketing.manage";

/// Hook event names this module emits.
pub(crate) const EVENT_CAMPAIGN_CREATED: &str = "sms.campaign.created";

pub(crate) const EVENT_CAMPAIGN_SCHEDULED: &str = "sms.campaign.scheduled";

pub(crate) const EVENT_CAMPAIGN_SENT: &str = "sms.campaign.sent";

pub(crate) const EVENT_MESSAGE_SENT: &str = "sms.message.sent";

pub(crate) const EVENT_MESSAGE_DELIVERED: &str = "sms.message.delivered";

pub(crate) const EVENT_MESSAGE_FAILED: &str = "sms.message.failed";

pub(crate) const EVENT_OPTOUT_ADDED: &str = "sms.optout.added";

pub(crate) const EVENT_PROVIDER_CREATED: &str = "sms.provider.created";

/// Hook event names this module listens to.
pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

pub(crate) const EVENT_MARKETING_CAMPAIGN_SENT: &str = "marketing.campaign.sent";

/// Marker written into `created_by` when the owning user
/// is deleted. Rows are kept; only the identity is
/// anonymized.
pub(crate) const DELETED_USER_ID: &str = "deleted-user";

/// Default opt-out keywords seeded as data rows for a
/// tenant the first time its keyword list is read while
/// empty. These are the conventional English opt-out words;
/// a tenant edits them like any other data row, and nothing
/// about them is hard-coded into matching (matching reads
/// the rows).
pub(crate) const DEFAULT_OPTOUT_KEYWORDS: &[&str] = &["STOP", "UNSUBSCRIBE", "QUIT", "END"];

/// Default opt-in keywords seeded alongside the opt-out
/// ones: an inbound message with one of these words removes
/// the sender from the opt-out list.
pub(crate) const DEFAULT_OPTIN_KEYWORDS: &[&str] = &["START", "SUBSCRIBE", "YES"];

// GSM-7 / UCS-2 segment math (pure functions, unit-tested below)

/// GSM 7-bit default alphabet (single-septet characters),
/// including the newline and carriage return entries of the
/// standard table. Any character outside this set and the
/// extension set below forces UCS-2 encoding.
pub const GSM7_BASIC: &str = "@\u{00a3}$\u{00a5}\u{00e8}\u{00e9}\u{00f9}\u{00ec}\u{00f2}\u{00c7}\n\u{00d8}\u{00f8}\r\u{00c5}\u{00e5}\u{0394}_\u{03a6}\u{0393}\u{039b}\u{03a9}\u{03a0}\u{03a8}\u{03a3}\u{0398}\u{039e}\u{00c6}\u{00e6}\u{00df}\u{00c9} !\"#\u{00a4}%&'()*+,-./0123456789:;<=>?\u{00a1}ABCDEFGHIJKLMNOPQRSTUVWXYZ\u{00c4}\u{00d6}\u{00d1}\u{00dc}\u{00a7}\u{00bf}abcdefghijklmnopqrstuvwxyz\u{00e4}\u{00f6}\u{00f1}\u{00fc}\u{00e0}";

/// GSM 7-bit extension-table characters: each one costs two
/// septets (an escape septet plus the character septet).
pub const GSM7_EXTENSION: &str = "^{}\\[~]|\u{20ac}";

/// Single-segment limits: 160 septets for GSM-7, 70
/// UCS-2 code units.
pub const GSM7_SINGLE_LIMIT: usize = 160;
pub const UCS2_SINGLE_LIMIT: usize = 70;

/// Concatenated-segment limits: 153 septets for GSM-7, 67
/// UCS-2 code units (the remainder is the header).
pub const GSM7_CONCAT_LIMIT: usize = 153;
pub const UCS2_CONCAT_LIMIT: usize = 67;

/// Which encoding a text needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentEncoding {
    Gsm7,
    Ucs2,
}

impl SegmentEncoding {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Gsm7 => "gsm7",
            Self::Ucs2 => "ucs2",
        }
    }
}

/// Segment count for one message body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentInfo {
    pub encoding: SegmentEncoding,
    /// Billable units: septets for GSM-7, UTF-16 code units
    /// for UCS-2.
    pub units: usize,
    pub segments: usize,
    /// Units carried by one segment of this message (160 /
    /// 153 for GSM-7, 70 / 67 for UCS-2, depending on
    /// whether the message fits in one segment).
    pub per_segment: usize,
}

/// True when every character of `text` is in the GSM 7-bit
/// default or extension alphabet.
pub fn is_gsm7(text: &str) -> bool {
    text.chars()
        .all(|c| GSM7_BASIC.contains(c) || GSM7_EXTENSION.contains(c))
}

/// Encoding a text needs: GSM-7 when every character is in
/// the GSM alphabets, UCS-2 otherwise.
pub fn encoding_of(text: &str) -> SegmentEncoding {
    if is_gsm7(text) {
        SegmentEncoding::Gsm7
    } else {
        SegmentEncoding::Ucs2
    }
}

/// Billable units of `text` under its encoding: septets for
/// GSM-7 (extension characters count two), UTF-16 code units
/// for UCS-2 (characters outside the basic multilingual
/// plane count two).
pub fn billable_units(text: &str, encoding: SegmentEncoding) -> usize {
    match encoding {
        SegmentEncoding::Gsm7 => text
            .chars()
            .map(|c| if GSM7_EXTENSION.contains(c) { 2 } else { 1 })
            .sum(),
        SegmentEncoding::Ucs2 => text.encode_utf16().count(),
    }
}

/// Segment count for `text`: the encoding it needs, its
/// billable units, and how many segments it spans. An empty
/// text is zero segments; a text that fits the single-segment
/// limit is one segment; anything longer splits at the
/// concatenated limit.
pub fn segment_info(text: &str) -> SegmentInfo {
    let encoding = encoding_of(text);
    let units = billable_units(text, encoding);
    let (single, concat) = match encoding {
        SegmentEncoding::Gsm7 => (GSM7_SINGLE_LIMIT, GSM7_CONCAT_LIMIT),
        SegmentEncoding::Ucs2 => (UCS2_SINGLE_LIMIT, UCS2_CONCAT_LIMIT),
    };
    if units == 0 {
        return SegmentInfo {
            encoding,
            units: 0,
            segments: 0,
            per_segment: single,
        };
    }
    if units <= single {
        return SegmentInfo {
            encoding,
            units,
            segments: 1,
            per_segment: single,
        };
    }
    SegmentInfo {
        encoding,
        units,
        segments: units.div_ceil(concat),
        per_segment: concat,
    }
}

// Template substitution (pure functions, unit-tested below)

/// Variable names referenced by `{{name}}` placeholders in
/// `body`, in first-appearance order, deduplicated. A
/// placeholder is `{{`, an optional run of whitespace, a
/// name of letters, digits, or underscores starting with a
/// letter or underscore, optional whitespace, `}}`.
/// Anything else that looks like braces is left alone.
pub fn template_variables(body: &str) -> Vec<String> {
    let mut names = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            let start = j;
            while j < bytes.len()
                && ((bytes[j] as char).is_ascii_alphanumeric() || bytes[j] == b'_')
            {
                j += 1;
            }
            let name = &body[start..j];
            let mut k = j;
            while k < bytes.len() && (bytes[k] as char).is_whitespace() {
                k += 1;
            }
            if k + 1 < bytes.len()
                && bytes[k] == b'}'
                && bytes[k + 1] == b'}'
                && !name.is_empty()
                && (name.as_bytes()[0].is_ascii_alphabetic() || name.as_bytes()[0] == b'_')
            {
                let name = name.to_string();
                if !names.contains(&name) {
                    names.push(name);
                }
                i = k + 2;
                continue;
            }
        }
        i += 1;
    }
    names
}

/// Render `body` by replacing each `{{name}}` placeholder
/// with the matching value. Returns the rendered text plus
/// the names that had no value: a missing variable leaves
/// its placeholder in place and is reported, never silently
/// blanked. Values are inserted verbatim.
pub fn render_template(body: &str, variables: &HashMap<String, String>) -> (String, Vec<String>) {
    let mut missing: Vec<String> = Vec::new();
    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            let start = j;
            while j < bytes.len()
                && ((bytes[j] as char).is_ascii_alphanumeric() || bytes[j] == b'_')
            {
                j += 1;
            }
            let name = &body[start..j];
            let mut k = j;
            while k < bytes.len() && (bytes[k] as char).is_whitespace() {
                k += 1;
            }
            if k + 1 < bytes.len()
                && bytes[k] == b'}'
                && bytes[k + 1] == b'}'
                && !name.is_empty()
                && (name.as_bytes()[0].is_ascii_alphabetic() || name.as_bytes()[0] == b'_')
            {
                match variables.get(name) {
                    Some(value) => out.push_str(value),
                    None => {
                        out.push_str(&body[i..k + 2]);
                        if !missing.contains(&name.to_string()) {
                            missing.push(name.to_string());
                        }
                    }
                }
                i = k + 2;
                continue;
            }
        }
        // Copy one UTF-8 character verbatim.
        let ch_len = utf8_len(bytes[i]);
        let end = (i + ch_len).min(bytes.len());
        out.push_str(&body[i..end]);
        i = end;
    }
    (out, missing)
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

// Phone normalization (pure function, unit-tested below)

/// Normalize a phone number to E.164: strip spaces, dashes,
/// parentheses, and dots, then require a leading `+` and
/// 8-15 digits. Returns `None` for anything else - a number
/// without a country code, a short code, or text that is not
/// a number never becomes a recipient.
pub fn normalize_phone(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '(' | ')' | '.'))
        .collect();
    if !cleaned.starts_with('+') {
        return None;
    }
    let digits = &cleaned[1..];
    if !(8..=15).contains(&digits.len()) {
        return None;
    }
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(cleaned)
}

// Status machines (pure functions, unit-tested below)

/// Campaign status: `draft` -> `scheduled` -> `sending` ->
/// `sent`, with `paused` and `cancelled` as side states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignStatus {
    Draft,
    Scheduled,
    Sending,
    Sent,
    Paused,
    Cancelled,
}

impl CampaignStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Scheduled => "scheduled",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Paused => "paused",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "scheduled" => Some(Self::Scheduled),
            "sending" => Some(Self::Sending),
            "sent" => Some(Self::Sent),
            "paused" => Some(Self::Paused),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Allowed campaign transitions. A sent campaign is final;
/// a cancelled campaign is final; scheduling is only from
/// draft or paused; sending is only from draft, scheduled,
/// or paused; pausing is only from scheduled or sending.
pub fn campaign_can_transition(from: CampaignStatus, to: CampaignStatus) -> bool {
    matches!(
        (from, to),
        (CampaignStatus::Draft, CampaignStatus::Scheduled)
            | (CampaignStatus::Draft, CampaignStatus::Sending)
            | (CampaignStatus::Draft, CampaignStatus::Cancelled)
            | (CampaignStatus::Scheduled, CampaignStatus::Sending)
            | (CampaignStatus::Scheduled, CampaignStatus::Paused)
            | (CampaignStatus::Scheduled, CampaignStatus::Cancelled)
            | (CampaignStatus::Sending, CampaignStatus::Sent)
            | (CampaignStatus::Sending, CampaignStatus::Paused)
            | (CampaignStatus::Sending, CampaignStatus::Cancelled)
            | (CampaignStatus::Paused, CampaignStatus::Scheduled)
            | (CampaignStatus::Paused, CampaignStatus::Sending)
            | (CampaignStatus::Paused, CampaignStatus::Cancelled)
    )
}

/// Per-message delivery status. `queued` is the starting
/// state for every accepted recipient; `skipped` is a
/// recipient excluded before any send attempt (opted out or
/// invalid); `undelivered` is a provider-reported
/// non-delivery that is not a hard failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Queued,
    Sent,
    Delivered,
    Failed,
    Undelivered,
    Skipped,
}

impl MessageStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sent => "sent",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Undelivered => "undelivered",
            Self::Skipped => "skipped",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "queued" => Some(Self::Queued),
            "sent" => Some(Self::Sent),
            "delivered" => Some(Self::Delivered),
            "failed" => Some(Self::Failed),
            "undelivered" => Some(Self::Undelivered),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

/// Allowed message transitions. Delivery updates only move
/// forward from `sent`; a failed or undelivered message may
/// be re-queued for retry; a skipped message is final.
pub fn message_can_transition(from: MessageStatus, to: MessageStatus) -> bool {
    matches!(
        (from, to),
        (MessageStatus::Queued, MessageStatus::Sent)
            | (MessageStatus::Queued, MessageStatus::Failed)
            | (MessageStatus::Queued, MessageStatus::Skipped)
            | (MessageStatus::Sent, MessageStatus::Delivered)
            | (MessageStatus::Sent, MessageStatus::Failed)
            | (MessageStatus::Sent, MessageStatus::Undelivered)
            | (MessageStatus::Failed, MessageStatus::Queued)
            | (MessageStatus::Undelivered, MessageStatus::Queued)
            | (MessageStatus::Delivered, MessageStatus::Delivered)
    )
}

// Opt-out keyword matching and segment filters (pure functions)

/// Match an inbound message body against the tenant's
/// opt-out / opt-in keyword rows. The body is trimmed and
/// upper-cased; a keyword matches when the body equals it or
/// starts with it followed by whitespace (`STOP` matches
/// `STOP`, `stop`, and `STOP please`, but not `STOPPED`).
/// Returns the matched keyword in its stored form.
pub fn match_keyword(body: &str, keywords: &[String]) -> Option<String> {
    let normalized = body.trim().to_ascii_uppercase();
    if normalized.is_empty() {
        return None;
    }
    for keyword in keywords {
        let key = keyword.trim().to_ascii_uppercase();
        if key.is_empty() {
            continue;
        }
        if normalized == key || normalized.starts_with(&format!("{key} ")) {
            return Some(keyword.clone());
        }
    }
    None
}

/// Evaluate a campaign segment filter against one contact.
/// The filter is a JSON object with any of:
/// - `tags_all`: every listed tag must be on the contact.
/// - `tags_any`: at least one listed tag must be on the
///   contact.
/// - `variables`: an object of `name: value` pairs; every
///   pair must equal the contact's stored variable (compared
///   as strings).
/// An empty or absent filter matches every contact.
pub fn segment_filter_matches(
    contact_tags: &[String],
    contact_variables: &HashMap<String, String>,
    filter: &Value,
) -> bool {
    let Some(filter) = filter.as_object() else {
        return true;
    };
    if let Some(tags_all) = filter.get("tags_all").and_then(Value::as_array) {
        for tag in tags_all {
            let Some(tag) = tag.as_str() else {
                return false;
            };
            if !contact_tags.iter().any(|t| t == tag) {
                return false;
            }
        }
    }
    if let Some(tags_any) = filter.get("tags_any").and_then(Value::as_array) {
        if !tags_any.is_empty() {
            let mut any = false;
            for tag in tags_any {
                if let Some(tag) = tag.as_str()
                    && contact_tags.iter().any(|t| t == tag)
                {
                    any = true;
                    break;
                }
            }
            if !any {
                return false;
            }
        }
    }
    if let Some(variables) = filter.get("variables").and_then(Value::as_object) {
        for (key, expected) in variables {
            let expected = match expected.as_str() {
                Some(text) => text.to_string(),
                None => expected.to_string(),
            };
            match contact_variables.get(key) {
                Some(actual) if actual == &expected => {}
                _ => return false,
            }
        }
    }
    true
}

// Rate-limit math (pure functions, unit-tested below)

/// True when another send fits inside the per-minute limit:
/// `sent_in_window` sends already happened in the last
/// minute and `limit_per_minute` is the configured cap. A
/// limit of zero or less blocks everything.
pub fn sends_allowed(sent_in_window: i64, limit_per_minute: i64) -> bool {
    limit_per_minute > 0 && sent_in_window < limit_per_minute
}

/// How many more sends fit inside the per-minute limit
/// (zero when the window is full or the limit is not
/// positive).
pub fn remaining_sends(sent_in_window: i64, limit_per_minute: i64) -> i64 {
    if limit_per_minute <= 0 {
        return 0;
    }
    (limit_per_minute - sent_in_window).max(0)
}

/// True when `url` is an HTTPS URL with a non-empty host.
/// Provider endpoints must be HTTPS; plain HTTP and
/// host-less values are refused at create / update time.
pub fn is_https_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.strip_suffix(':').unwrap_or(host);
    !host.is_empty() && !host.contains(' ')
}

// Enumerations (fixed vocabulary - the shape, not the content)

/// Provider kind: the generic HTTP adapter or the log-only
/// test provider. No real provider is named anywhere - a
/// tenant points the HTTP adapter at whatever SMS gateway
/// it uses by entering the endpoint as data.
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
/// basic auth (username stored in plain text, password is
/// the write-only credential).
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

/// What an opt-out keyword row does: `opt_out` adds the
/// sender to the opt-out list, `opt_in` removes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeywordKind {
    OptOut,
    OptIn,
}

impl KeywordKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::OptOut => "opt_out",
            Self::OptIn => "opt_in",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "opt_out" => Some(Self::OptOut),
            "opt_in" => Some(Self::OptIn),
            _ => None,
        }
    }
}

// Stored types

/// One admin-configured SMS provider. `endpoint_url` is
/// empty for the log-only kind. The credential is write-only:
/// `credential_encrypted` holds the encrypted secret (never
/// serialized) and only `credential_fingerprint` (first 12
/// hex characters of the SHA-256) and `key_configured` are
/// visible. `webhook_secret_hash` is the SHA-256 hex of the
/// delivery / inbound webhook secret; the raw secret is
/// returned exactly once at creation / rotation and never
/// stored. `request_fields` is an optional JSON object of
/// extra fields merged into every HTTP adapter request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsProvider {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub kind: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    pub auth_type: AuthType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_username: Option<String>,
    #[serde(skip_serializing)]
    pub credential_encrypted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_fingerprint: Option<String>,
    #[serde(skip_serializing)]
    pub webhook_secret_hash: Option<String>,
    pub sender_id: String,
    pub sends_per_minute: i64,
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

impl SmsProvider {
    /// Masked read view: the credential and the webhook
    /// secret never appear; only whether a credential is
    /// configured and its fingerprint are shown.
    pub(crate) fn public_view(&self) -> Value {
        serde_json::json!({
            "id": self.id.to_string(),
            "tenant_id": self.tenant_id,
            "name": self.name,
            "kind": self.kind.as_str(),
            "endpoint_url": self.endpoint_url,
            "auth_type": self.auth_type.as_str(),
            "auth_header": self.auth_header,
            "auth_username": self.auth_username,
            "key_configured": self.credential_encrypted.is_some(),
            "credential_fingerprint": self.credential_fingerprint,
            "webhook_secret_configured": self.webhook_secret_hash.is_some(),
            "sender_id": self.sender_id,
            "sends_per_minute": self.sends_per_minute,
            "request_fields": self.request_fields,
            "enabled": self.enabled,
            "created_by": self.created_by,
            "created_at": self.created_at.to_rfc3339(),
            "updated_at": self.updated_at.to_rfc3339(),
        })
    }
}

/// One plain-text SMS template with `{{variable}}`
/// placeholders. Bodies are plain text only - no HTML, no
/// attachments. `variables` lists the placeholder names the
/// body references (derived at write time).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsTemplate {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub body: String,
    #[serde(default)]
    pub variables: Vec<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One contact list: a named audience. Members live in
/// `SmsContact` rows that reference the list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsContactList {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One contact on a list: a normalized E.164 number, an
/// optional display name, tags for segment filters, and
/// free-form variables (an object of string values) used
/// for template substitution. One number per list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsContact {
    pub id: Uuid,
    pub tenant_id: String,
    pub list_id: Uuid,
    pub phone_number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub variables: HashMap<String, String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Send counts for one campaign, kept on the campaign row
/// so a list call does not have to scan messages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CampaignStats {
    #[serde(default)]
    pub queued: i64,
    #[serde(default)]
    pub sent: i64,
    #[serde(default)]
    pub delivered: i64,
    #[serde(default)]
    pub failed: i64,
    #[serde(default)]
    pub skipped: i64,
}

/// One SMS campaign: a template, a provider, an audience,
/// and a status. The audience is contact lists by reference
/// plus explicit recipient numbers plus a segment filter
/// (a JSON object evaluated against contact tags and
/// variables). `variables` holds campaign-level template
/// defaults; per-contact variables win over them.
/// `send_at` is the scheduled send time; `sent_at` is when
/// the send pass ran. A campaign never stores message
/// bodies itself - bodies live on the message rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsCampaign {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub template_id: Uuid,
    pub provider_id: Uuid,
    #[serde(default)]
    pub contact_list_ids: Vec<String>,
    #[serde(default)]
    pub recipient_numbers: Vec<String>,
    #[serde(default)]
    pub segment_filter: Value,
    #[serde(default)]
    pub variables: HashMap<String, String>,
    pub status: CampaignStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub stats: CampaignStats,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One outbound SMS: a single recipient of a campaign. The
/// rendered body is stored so delivery reports stay
/// readable after the template changes. `segments` and
/// `encoding` come from the segment math at send time.
/// `queued_reason` explains a message left queued
/// (`rate_limited`) or skipped (`opted_out`,
/// `missing_variable`); `error` holds the provider's
/// failure text (truncated, never a credential).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsMessage {
    pub id: Uuid,
    pub tenant_id: String,
    pub campaign_id: Uuid,
    pub provider_id: Uuid,
    pub phone_number: String,
    pub body: String,
    pub segments: i64,
    pub encoding: SegmentEncoding,
    pub status: MessageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One opted-out number for a tenant: sends to it are
/// blocked at send time. `keyword` is the matched inbound
/// keyword, or `manual` / `api` for rows added by hand.
/// `source` names how the row arrived (`keyword`,
/// `manual`, `api`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsOptOut {
    pub id: Uuid,
    pub tenant_id: String,
    pub phone_number: String,
    pub keyword: String,
    pub source: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One opt-out / opt-in keyword row for a tenant. Matching
/// reads these rows, so a tenant with no rows has no
/// keyword handling until it adds (or seeds) some.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsOptOutKeyword {
    pub id: Uuid,
    pub tenant_id: String,
    pub keyword: String,
    pub kind: KeywordKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-tenant send rate limit: sends per minute for the
/// whole tenant, enforced at enqueue time alongside the
/// per-provider limit. One row per tenant; a tenant with no
/// row uses the default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsRateLimit {
    pub id: Uuid,
    pub tenant_id: String,
    pub sends_per_minute: i64,
    pub updated_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audit row: who changed what, when, with the before
/// and after values as JSON. Written for provider, template,
/// list, contact, campaign, message, opt-out, keyword, and
/// rate-limit changes. Audit rows are never updated or
/// deleted through the API.
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

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateProviderRequest {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    #[serde(default)]
    pub auth_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
    pub sender_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends_per_minute: Option<i64>,
    #[serde(default)]
    pub request_fields: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProviderRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_username: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends_per_minute: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_fields: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTemplateRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTemplateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PreviewTemplateRequest {
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateContactListRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateContactListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateContactRequest {
    pub phone_number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCampaignRequest {
    pub name: String,
    pub template_id: String,
    pub provider_id: String,
    #[serde(default)]
    pub contact_list_ids: Vec<String>,
    #[serde(default)]
    pub recipient_numbers: Vec<String>,
    #[serde(default)]
    pub segment_filter: Option<Value>,
    #[serde(default)]
    pub variables: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCampaignRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_list_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_numbers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_filter: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_at: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ScheduleCampaignRequest {
    pub send_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateOptOutRequest {
    pub phone_number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateKeywordRequest {
    pub keyword: String,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateKeywordRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRateLimitRequest {
    pub sends_per_minute: i64,
}

#[derive(Debug, Deserialize)]
pub struct DeliveryWebhookRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_message_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct InboundWebhookRequest {
    pub from: String,
    pub body: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn segment_info_gsm7_single_and_concat() {
        let one = segment_info("Hello world");
        assert_eq!(one.encoding, SegmentEncoding::Gsm7);
        assert_eq!(one.units, 11);
        assert_eq!(one.segments, 1);
        assert_eq!(one.per_segment, 160);
        let long = "a".repeat(160);
        assert_eq!(segment_info(&long).segments, 1);
        let longer = "a".repeat(161);
        let info = segment_info(&longer);
        assert_eq!(info.segments, 2);
        assert_eq!(info.per_segment, 153);
        // 153 * 2 = 306 fits in two concatenated segments.
        assert_eq!(segment_info(&"a".repeat(306)).segments, 2);
        assert_eq!(segment_info(&"a".repeat(307)).segments, 3);
        assert_eq!(segment_info("").segments, 0);
    }

    #[test]
    fn segment_info_extension_chars_cost_two_septets() {
        // ^ and € are extension-table characters: two septets each.
        let info = segment_info("^");
        assert_eq!(info.encoding, SegmentEncoding::Gsm7);
        assert_eq!(info.units, 2);
        assert_eq!(info.segments, 1);
        let info = segment_info(&"^".repeat(80));
        assert_eq!(info.units, 160);
        assert_eq!(info.segments, 1);
        let info = segment_info(&"^".repeat(81));
        assert_eq!(info.units, 162);
        assert_eq!(info.segments, 2);
    }

    #[test]
    fn segment_info_ucs2_for_non_gsm_chars() {
        // Cyrillic and emoji force UCS-2.
        let info = segment_info("Привет");
        assert_eq!(info.encoding, SegmentEncoding::Ucs2);
        assert_eq!(info.units, 6);
        assert_eq!(info.segments, 1);
        assert_eq!(info.per_segment, 70);
        assert_eq!(segment_info(&"я".repeat(70)).segments, 1);
        assert_eq!(segment_info(&"я".repeat(71)).segments, 2);
        assert_eq!(segment_info(&"я".repeat(134)).segments, 2);
        assert_eq!(segment_info(&"я".repeat(135)).segments, 3);
        // An astral character is two UTF-16 code units.
        let info = segment_info("😀");
        assert_eq!(info.encoding, SegmentEncoding::Ucs2);
        assert_eq!(info.units, 2);
    }

    #[test]
    fn template_variables_and_rendering() {
        let body = "Hi {{ first_name }}, your code is {{code}}. Thanks {{first_name}}!";
        assert_eq!(
            template_variables(body),
            vec!["first_name".to_string(), "code".to_string()]
        );
        let mut vars = HashMap::new();
        vars.insert("first_name".to_string(), "Ayu".to_string());
        let (rendered, missing) = render_template(body, &vars);
        assert_eq!(rendered, "Hi Ayu, your code is {{code}}. Thanks Ayu!");
        assert_eq!(missing, vec!["code".to_string()]);
        // A placeholder with no closing braces is left alone.
        assert_eq!(template_variables("Hello {{name"), Vec::<String>::new());
        assert_eq!(template_variables("No placeholders"), Vec::<String>::new());
        // Names may start with an underscore; digits alone are not a name.
        assert_eq!(
            template_variables("{{_ok}} and {{9bad}}"),
            vec!["_ok".to_string()]
        );
        // Unicode body renders verbatim around placeholders.
        let (rendered, missing) = render_template("Halo {{name}} 😀", &vars);
        assert_eq!(rendered, "Halo {{name}} 😀");
        assert_eq!(missing, vec!["name".to_string()]);
    }

    #[test]
    fn phone_normalization() {
        assert_eq!(
            normalize_phone("+62 812-3456-7890"),
            Some("+6281234567890".to_string())
        );
        assert_eq!(
            normalize_phone("+1 (415) 555.0132"),
            Some("+14155550132".to_string())
        );
        assert_eq!(normalize_phone("081234567890"), None);
        assert_eq!(normalize_phone("+62"), None);
        assert_eq!(normalize_phone("+1234567890123456"), None);
        assert_eq!(normalize_phone("+62abc"), None);
        assert_eq!(normalize_phone(""), None);
        assert_eq!(
            normalize_phone("+6281234567"),
            Some("+6281234567".to_string())
        );
    }

    #[test]
    fn campaign_status_machine() {
        use CampaignStatus::*;
        assert!(campaign_can_transition(Draft, Scheduled));
        assert!(campaign_can_transition(Draft, Sending));
        assert!(campaign_can_transition(Draft, Cancelled));
        assert!(campaign_can_transition(Scheduled, Sending));
        assert!(campaign_can_transition(Scheduled, Paused));
        assert!(campaign_can_transition(Sending, Sent));
        assert!(campaign_can_transition(Sending, Paused));
        assert!(campaign_can_transition(Paused, Scheduled));
        assert!(campaign_can_transition(Paused, Sending));
        assert!(!campaign_can_transition(Draft, Sent));
        assert!(!campaign_can_transition(Draft, Paused));
        assert!(!campaign_can_transition(Sent, Cancelled));
        assert!(!campaign_can_transition(Sent, Draft));
        assert!(!campaign_can_transition(Cancelled, Draft));
        assert!(!campaign_can_transition(Scheduled, Sent));
        assert!(!campaign_can_transition(Paused, Sent));
    }

    #[test]
    fn message_status_machine() {
        use MessageStatus::*;
        assert!(message_can_transition(Queued, Sent));
        assert!(message_can_transition(Queued, Failed));
        assert!(message_can_transition(Queued, Skipped));
        assert!(message_can_transition(Sent, Delivered));
        assert!(message_can_transition(Sent, Undelivered));
        assert!(message_can_transition(Failed, Queued));
        assert!(message_can_transition(Undelivered, Queued));
        assert!(!message_can_transition(Queued, Delivered));
        assert!(!message_can_transition(Delivered, Failed));
        assert!(!message_can_transition(Skipped, Queued));
        assert!(!message_can_transition(Failed, Delivered));
    }

    #[test]
    fn keyword_matching() {
        let keywords = vec!["STOP".to_string(), "UNSUBSCRIBE".to_string()];
        assert_eq!(match_keyword("STOP", &keywords), Some("STOP".to_string()));
        assert_eq!(
            match_keyword("  stop  ", &keywords),
            Some("STOP".to_string())
        );
        assert_eq!(
            match_keyword("STOP please", &keywords),
            Some("STOP".to_string())
        );
        assert_eq!(
            match_keyword("unsubscribe me", &keywords),
            Some("UNSUBSCRIBE".to_string())
        );
        assert_eq!(match_keyword("STOPPED", &keywords), None);
        assert_eq!(match_keyword("HELLO", &keywords), None);
        assert_eq!(match_keyword("", &keywords), None);
        assert_eq!(match_keyword("STOP", &[]), None);
    }

    #[test]
    fn segment_filter_evaluation() {
        let mut vars = HashMap::new();
        vars.insert("city".to_string(), "Padang".to_string());
        vars.insert("tier".to_string(), "gold".to_string());
        let tags = vec!["vip".to_string(), "buyer".to_string()];
        assert!(segment_filter_matches(&tags, &vars, &json!({})));
        assert!(segment_filter_matches(&tags, &vars, &json!(null)));
        assert!(segment_filter_matches(
            &tags,
            &vars,
            &json!({"tags_all": ["vip", "buyer"]})
        ));
        assert!(!segment_filter_matches(
            &tags,
            &vars,
            &json!({"tags_all": ["vip", "new"]})
        ));
        assert!(segment_filter_matches(
            &tags,
            &vars,
            &json!({"tags_any": ["new", "vip"]})
        ));
        assert!(!segment_filter_matches(
            &tags,
            &vars,
            &json!({"tags_any": ["new"]})
        ));
        assert!(segment_filter_matches(
            &tags,
            &vars,
            &json!({"variables": {"city": "Padang"}})
        ));
        assert!(!segment_filter_matches(
            &tags,
            &vars,
            &json!({"variables": {"city": "Jakarta"}})
        ));
        assert!(segment_filter_matches(
            &tags,
            &vars,
            &json!({"tags_all": ["vip"], "variables": {"tier": "gold"}})
        ));
        assert!(!segment_filter_matches(
            &tags,
            &vars,
            &json!({"tags_all": ["vip"], "variables": {"tier": "silver"}})
        ));
    }

    #[test]
    fn rate_limit_math() {
        assert!(sends_allowed(0, 60));
        assert!(sends_allowed(59, 60));
        assert!(!sends_allowed(60, 60));
        assert!(!sends_allowed(61, 60));
        assert!(!sends_allowed(0, 0));
        assert!(!sends_allowed(0, -5));
        assert_eq!(remaining_sends(0, 60), 60);
        assert_eq!(remaining_sends(59, 60), 1);
        assert_eq!(remaining_sends(60, 60), 0);
        assert_eq!(remaining_sends(100, 60), 0);
        assert_eq!(remaining_sends(0, 0), 0);
    }

    #[test]
    fn https_url_validation() {
        assert!(is_https_url("https://api.example.com/send"));
        assert!(is_https_url("https://api.example.com:8443/v1/sms"));
        assert!(!is_https_url("http://api.example.com/send"));
        assert!(!is_https_url("https://"));
        assert!(!is_https_url("https://exa mple.com"));
        assert!(!is_https_url(""));
        assert!(!is_https_url("ftp://api.example.com"));
    }

    #[test]
    fn vocabulary_parsing() {
        assert_eq!(ProviderKind::from_str("http"), Some(ProviderKind::Http));
        assert_eq!(
            ProviderKind::from_str("log_only"),
            Some(ProviderKind::LogOnly)
        );
        assert_eq!(ProviderKind::from_str("other_vendor"), None);
        assert_eq!(AuthType::from_str("bearer"), Some(AuthType::Bearer));
        assert_eq!(
            AuthType::from_str("api_key_header"),
            Some(AuthType::ApiKeyHeader)
        );
        assert_eq!(AuthType::from_str("basic"), Some(AuthType::Basic));
        assert_eq!(AuthType::from_str("oauth"), None);
        assert!(AuthType::Bearer.needs_credential());
        assert!(AuthType::Basic.needs_credential());
        assert!(!AuthType::None.needs_credential());
        assert_eq!(KeywordKind::from_str("opt_out"), Some(KeywordKind::OptOut));
        assert_eq!(KeywordKind::from_str("opt_in"), Some(KeywordKind::OptIn));
        assert_eq!(KeywordKind::from_str("block"), None);
        assert_eq!(
            CampaignStatus::from_str("scheduled"),
            Some(CampaignStatus::Scheduled)
        );
        assert_eq!(CampaignStatus::from_str("queued"), None);
        assert_eq!(
            MessageStatus::from_str("undelivered"),
            Some(MessageStatus::Undelivered)
        );
        assert_eq!(MessageStatus::from_str("draft"), None);
    }
}
