//! WhatsApp external module (split from lib.rs; moved verbatim).

use super::*;

/// HMAC-SHA256, used to verify the Meta webhook signature.
pub(crate) type HmacSha256 = Hmac<Sha256>;

/// The hook event a store/commerce module is expected to emit when an
/// order is paid. Verified 2026-11-22: this event does NOT exist in
/// `unified_core::hooks::events` (the core defines no order events at
/// all), and no core module emits it. The hook bus accepts arbitrary
/// event names, so the listener is registered on the literal name and
/// stays dormant until a store module emits it. The listener only
/// records a notification intent; it never sends a message.
pub const ORDER_PAID_EVENT: &str = "store.order.paid";

/// Hook event emitted when an inbound WhatsApp message arrives.
pub const MESSAGE_RECEIVED_EVENT: &str = "whatsapp.message.received";

/// Hook event emitted when a delivery status update arrives.
pub const MESSAGE_STATUS_EVENT: &str = "whatsapp.message.status";

/// Maximum inbound webhook body accepted (1 MiB).
pub(crate) const MAX_WEBHOOK_BODY_BYTES: usize = 1_048_576;

/// Maximum outbound text body (Meta allows 4096 characters).
pub(crate) const MAX_TEXT_LEN: usize = 4096;

/// Maximum number of variables a template may declare.
pub(crate) const MAX_TEMPLATE_VARIABLES: usize = 20;

/// Tenant id used for inbound webhook messages that cannot be
/// attributed to a tenant from prior outbound traffic. Such records
/// are only visible when explicitly filtering `tenant_id=unassigned`;
/// they never leak into another tenant's list.
pub(crate) const UNASSIGNED_TENANT: &str = "unassigned";

/// Default template language when the caller does not specify one.
pub(crate) const DEFAULT_TEMPLATE_LANGUAGE: &str = "en_US";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// WhatsApp configuration, read from the environment. Secrets are held
/// here only so the delivery worker and webhook handlers can use them;
/// they are never logged and never serialised into a response.
#[derive(Debug, Clone)]
pub struct WhatsAppConfig {
    pub access_token: Option<String>,
    pub phone_number_id: Option<String>,
    pub verify_token: Option<String>,
    pub app_secret: Option<String>,
    pub rate_limit_per_minute: u32,
    pub graph_api_base: String,
    pub graph_api_version: String,
}

impl WhatsAppConfig {
    /// Read configuration from the environment. Empty values count as
    /// unset. The rate limit defaults to 30 and is clamped to 1..=1000.
    pub fn from_env() -> Self {
        let rate_limit_per_minute = std::env::var("WHATSAPP_RATE_LIMIT_PER_MINUTE")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(30)
            .clamp(1, 1000);
        Self {
            access_token: env_non_empty("WHATSAPP_ACCESS_TOKEN"),
            phone_number_id: env_non_empty("WHATSAPP_PHONE_NUMBER_ID"),
            verify_token: env_non_empty("WHATSAPP_VERIFY_TOKEN"),
            app_secret: env_non_empty("WHATSAPP_APP_SECRET"),
            rate_limit_per_minute,
            graph_api_base: std::env::var("WHATSAPP_GRAPH_API_BASE")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "https://graph.facebook.com".to_string()),
            graph_api_version: std::env::var("WHATSAPP_GRAPH_API_VERSION")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "v19.0".to_string()),
        }
    }

    /// True when outbound delivery can be attempted: both the access
    /// token and the phone number id are configured.
    pub fn is_delivery_configured(&self) -> bool {
        self.access_token.is_some() && self.phone_number_id.is_some()
    }
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------------------
// Validation helpers (pure functions, unit-tested)
// ---------------------------------------------------------------------------

/// True when `phone` is a valid E.164 number: a leading `+`, then 8-15
/// digits, the first of which is not `0`.
pub fn is_valid_e164(phone: &str) -> bool {
    let Some(digits) = phone.strip_prefix('+') else {
        return false;
    };
    (8..=15).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0')
}

/// True when `id` is an acceptable tenant id: non-empty, at most 128
/// characters, letters, digits, `-`, `_`, `.` only.
pub fn is_valid_tenant_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// True when `name` is an acceptable Meta template name: lowercase
/// letters, digits and `_` only, at most 64 characters.
pub fn is_valid_template_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// True when `code` is a language code of the form `ll_CC`
/// (for example `en_US`, `id_ID`).
pub fn is_valid_language_code(code: &str) -> bool {
    let mut parts = code.split('_');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(lang), Some(region), None) => {
            lang.len() == 2
                && lang.bytes().all(|c| c.is_ascii_lowercase())
                && region.len() == 2
                && region.bytes().all(|c| c.is_ascii_uppercase())
        }
        _ => false,
    }
}

/// Count the `{{n}}` placeholders in a template body. Placeholders
/// must be numbered consecutively from `{{1}}` with no gaps and no
/// `{{0}}`; a malformed, unclosed, or non-numeric placeholder is an
/// error. Returns the number of distinct placeholders (0 when the body
/// has none).
pub fn count_template_variables(body: &str) -> Result<usize, String> {
    let mut indices: Vec<u32> = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err("unclosed template placeholder '{{' in template body".to_string());
        };
        let inner = after[..end].trim();
        let n: u32 = inner.parse().map_err(|_| {
            format!("invalid template placeholder '{{{{{inner}}}}}' in template body")
        })?;
        if n == 0 {
            return Err("template placeholders start at {{1}}; {{0}} is invalid".to_string());
        }
        indices.push(n);
        rest = &after[end + 2..];
    }
    indices.sort_unstable();
    indices.dedup();
    for (i, n) in indices.iter().enumerate() {
        if *n as usize != i + 1 {
            return Err(format!(
                "template placeholders must be numbered consecutively from {{{{1}}}}; found {{{{{n}}}}} without {{{{{}}}}}",
                i + 1
            ));
        }
    }
    Ok(indices.len())
}

/// Constant-time byte comparison. Lengths are compared first (length
/// is not secret for the values compared here); the byte loop itself
/// does not short-circuit.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Lowercase hex encoding of `bytes`.
pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Verify an HMAC-SHA256 hex signature over `body` with `secret`.
/// The comparison is constant-time; a wrong key, wrong body, or
/// malformed signature all return `false`.
pub fn verify_hmac_sha256_hex(secret: &[u8], body: &[u8], signature_hex: &str) -> bool {
    let mut mac = match HmacSha256::new_from_slice(secret) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(body);
    let expected = to_hex(&mac.finalize().into_bytes());
    constant_time_eq(
        expected.as_bytes(),
        signature_hex.trim().to_lowercase().as_bytes(),
    )
}

/// Current UTC time as an RFC 3339 string with second precision.
pub(crate) fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Generate a WhatsApp-style message id (`wamid.<uuid>`).
pub(crate) fn new_wamid() -> String {
    format!("wamid.{}", Uuid::new_v4())
}
