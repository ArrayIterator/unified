//! email_providers - deliverability headers.
//!
//! Shared computation for the anti-spam headers every send carries
//! (task 354). One function, used by the SMTP family (lettre raw
//! headers via `build_lettre_message`) and by every HTTP API adapter
//! through its documented custom-headers mechanism — never
//! per-adapter copies.
//!
//! Rules:
//! - `deliverability_opt_out` on the message suppresses every
//!   header for that send.
//! - Bulk/marketing sends (`bulk`) get the full set by default.
//! - Transactional sends get List-Unsubscribe only when the caller
//!   opts in (`include_unsubscribe`).
//! - The https unsubscribe URL comes from the connection's
//!   `unsubscribe_url` setting; empty means mailto only. No URL is
//!   ever invented.
//! - Feedback-ID is SMTP-only (API adapters pass `smtp: false`):
//!   the value is `1:unified:<connection-id-first-8>:<sender-domain>`.
//! - Amazon SES has no custom-header support on its SendEmail API,
//!   so SES sends carry no deliverability headers (documented in
//!   `adapters.rs`); the SES SMTP interface is the alternative.

use super::{credentials::*, provider_kinds::*, store::*};
use serde_json::{Map, Value};

/// Header names, as `&'static str` so lettre's
/// `HeaderName::new_from_ascii_str` can take them directly.
pub const HEADER_LIST_UNSUBSCRIBE: &str = "List-Unsubscribe";
pub const HEADER_LIST_UNSUBSCRIBE_POST: &str = "List-Unsubscribe-Post";
pub const HEADER_PRECEDENCE: &str = "Precedence";
pub const HEADER_AUTO_RESPONSE_SUPPRESS: &str = "X-Auto-Response-Suppress";
pub const HEADER_FEEDBACK_ID: &str = "Feedback-ID";

/// Compute the deliverability headers for one send, in wire order.
/// Returns an empty list when the send opts out, or when neither
/// bulk nor include_unsubscribe asks for List-Unsubscribe (a plain
/// transactional send carries no deliverability headers).
///
/// Callers must have validated the sender address first (the
/// adapters do); an empty sender simply yields no List-Unsubscribe
/// value. Header values are validated to be single-line: a value
/// containing CR or LF is dropped (it cannot occur — the sender
/// address is validated and the unsubscribe URL is validated as an
/// http(s) URL at connection write time — but a header injection
/// must never depend on a caller elsewhere getting that right).
pub fn deliverability_headers(
    message: &EmailMessage,
    connection: &EmailConnection,
    smtp: bool,
) -> Vec<(&'static str, String)> {
    if message.deliverability_opt_out {
        return Vec::new();
    }
    let mut headers: Vec<(&'static str, String)> = Vec::new();
    if message.bulk || message.include_unsubscribe {
        let (from, _) = message.sender(connection);
        if !from.trim().is_empty() {
            let mut value = format!("<mailto:{}>", from.trim());
            if let Some(url) = connection
                .unsubscribe_url
                .as_deref()
                .filter(|url| !url.trim().is_empty())
            {
                value.push_str(&format!(", <{}>", url.trim()));
            }
            headers.push((HEADER_LIST_UNSUBSCRIBE, value));
            headers.push((
                HEADER_LIST_UNSUBSCRIBE_POST,
                "List-Unsubscribe=One-Click".to_string(),
            ));
        }
    }
    if message.bulk {
        headers.push((HEADER_PRECEDENCE, "bulk".to_string()));
        headers.push((HEADER_AUTO_RESPONSE_SUPPRESS, "All".to_string()));
        if smtp {
            let (from, _) = message.sender(connection);
            let domain = from.split('@').nth(1).unwrap_or("");
            let connection_id = connection.id.to_string();
            let short = &connection_id[..8.min(connection_id.len())];
            headers.push((
                HEADER_FEEDBACK_ID,
                format!("1:unified:{short}:{domain}"),
            ));
        }
    }
    headers.retain(|(_, value)| !value.contains(['\r', '\n']));
    headers
}

/// The same headers as a JSON object (name -> value), for API
/// adapters whose custom-headers mechanism takes an object
/// (Brevo, SendGrid, Resend, SparkPost, Mandrill).
pub fn deliverability_headers_object(
    message: &EmailMessage,
    connection: &EmailConnection,
) -> Value {
    let mut object = Map::new();
    for (name, value) in deliverability_headers(message, connection, false) {
        object.insert(name.to_string(), Value::String(value));
    }
    Value::Object(object)
}

/// The same headers as a Postmark-style array
/// (`[{ "Name": ..., "Value": ... }]`).
pub fn deliverability_headers_array(
    message: &EmailMessage,
    connection: &EmailConnection,
) -> Value {
    Value::Array(
        deliverability_headers(message, connection, false)
            .into_iter()
            .map(|(name, value)| {
                serde_json::json!({ "Name": name, "Value": value })
            })
            .collect(),
    )
}

/// Merge `overlay` headers into a JSON headers object, with the
/// overlay winning on collision (the anti-spam contract beats
/// caller metadata). A non-object base is treated as empty.
pub fn merge_headers_object(base: &Value, overlay: &Value) -> Value {
    let mut object = base.as_object().cloned().unwrap_or_default();
    if let Some(overlay) = overlay.as_object() {
        for (key, value) in overlay {
            object.insert(key.clone(), value.clone());
        }
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn connection() -> EmailConnection {
        EmailConnection {
            id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
            name: "Test".to_string(),
            kind: EmailProviderKind::Smtp,
            domain: None,
            region: None,
            sender_email: Some("news@example.com".to_string()),
            sender_name: None,
            webhook_url: None,
            unsubscribe_url: None,
            smtp_host: None,
            smtp_port: None,
            smtp_tls: None,
            smtp_username: None,
            credentials: EmailCredentials::default(),
            test_mode: false,
            enabled: true,
            last_tested_at: None,
            last_status: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn message() -> EmailMessage {
        EmailMessage {
            to: "reader@example.com".to_string(),
            from: None,
            from_name: None,
            subject: "Test".to_string(),
            body_text: Some("Hello".to_string()),
            body_html: None,
            reply_to: None,
            template_id: None,
            variables: HashMap::new(),
            metadata: HashMap::new(),
            test_mode: false,
            bulk: false,
            include_unsubscribe: false,
            deliverability_opt_out: false,
        }
    }

    fn names(headers: &[(&'static str, String)]) -> Vec<&'static str> {
        headers.iter().map(|(name, _)| *name).collect()
    }

    #[test]
    fn plain_transactional_send_carries_no_headers() {
        assert!(deliverability_headers(&message(), &connection(), true).is_empty());
    }

    #[test]
    fn opt_out_suppresses_bulk_headers() {
        let mut bulk = message();
        bulk.bulk = true;
        bulk.deliverability_opt_out = true;
        assert!(deliverability_headers(&bulk, &connection(), true).is_empty());
    }

    #[test]
    fn transactional_opt_in_gets_unsubscribe_only() {
        let mut transactional = message();
        transactional.include_unsubscribe = true;
        let headers = deliverability_headers(&transactional, &connection(), true);
        assert_eq!(
            names(&headers),
            vec![HEADER_LIST_UNSUBSCRIBE, HEADER_LIST_UNSUBSCRIBE_POST]
        );
        assert_eq!(
            headers[0].1,
            "<mailto:news@example.com>".to_string()
        );
    }

    #[test]
    fn bulk_gets_the_full_set_with_feedback_id_on_smtp_only() {
        let mut bulk = message();
        bulk.bulk = true;
        let smtp = deliverability_headers(&bulk, &connection(), true);
        assert_eq!(
            names(&smtp),
            vec![
                HEADER_LIST_UNSUBSCRIBE,
                HEADER_LIST_UNSUBSCRIBE_POST,
                HEADER_PRECEDENCE,
                HEADER_AUTO_RESPONSE_SUPPRESS,
                HEADER_FEEDBACK_ID,
            ]
        );
        assert_eq!(smtp[4].1, "1:unified:11111111:example.com".to_string());
        let api = deliverability_headers(&bulk, &connection(), false);
        assert!(!names(&api).contains(&HEADER_FEEDBACK_ID));
    }

    #[test]
    fn unsubscribe_url_adds_the_https_option() {
        let mut with_url = connection();
        with_url.unsubscribe_url =
            Some("https://example.com/unsubscribe".to_string());
        let mut bulk = message();
        bulk.bulk = true;
        let headers = deliverability_headers(&bulk, &with_url, true);
        assert_eq!(
            headers[0].1,
            "<mailto:news@example.com>, <https://example.com/unsubscribe>".to_string()
        );
    }

    #[test]
    fn merge_prefers_the_deliverability_value() {
        let base = serde_json::json!({ "List-Unsubscribe": "<mailto:old@example.com>", "X-Custom": "keep" });
        let mut bulk = message();
        bulk.bulk = true;
        let overlay = deliverability_headers_object(&bulk, &connection());
        let merged = merge_headers_object(&base, &overlay);
        assert_eq!(
            merged.get("List-Unsubscribe").and_then(Value::as_str),
            Some("<mailto:news@example.com>")
        );
        assert_eq!(
            merged.get("X-Custom").and_then(Value::as_str),
            Some("keep")
        );
    }
}
