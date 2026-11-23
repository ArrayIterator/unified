// Newsletter module - shared helpers.
//
// Split from `src/modules/newsletter.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use chrono::Utc;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;
use uuid::Uuid;

use super::constants::{EVENT_EMAIL_SEND_REQUESTED, EVENT_TEMPLATE_RENDER_REQUESTED, MAX_TRACKED_URLS_PER_CAMPAIGN};
use super::state::{NewsletterState, click_urls_store, subscribers_store};
use super::types::{NewsletterList, Subscriber};

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn signatures_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn now_string() -> String {
    Utc::now().to_rfc3339()
}

pub(crate) fn validate_email_address(input: &str) -> UnifiedResult<()> {
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

pub(crate) fn validate_connection_id(value: &Option<String>) -> UnifiedResult<()> {
    if let Some(raw) = value {
        if !raw.trim().is_empty() {
            Uuid::parse_str(raw).map_err(|_| {
                UnifiedError::bad_request(format!("Invalid connection id \"{raw}\""))
            })?;
        }
    }
    Ok(())
}

pub(crate) fn list_base_url(list: &NewsletterList) -> String {
    list.base_url
        .clone()
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string()
}

pub(crate) fn confirm_url(list: &NewsletterList, token: &str) -> String {
    format!(
        "{}/api/v1/newsletter/subscribers/confirm?token={token}",
        list_base_url(list)
    )
}

pub(crate) fn unsubscribe_url(list: &NewsletterList, token: &str) -> String {
    format!("{}/api/v1/newsletter/unsubscribe?token={token}", list_base_url(list))
}

/// Record a click-target URL as belonging to a campaign, so the
/// click tracker only redirects to URLs the campaign actually sent.
/// The stored form is the normalized URL string; at most
/// `MAX_TRACKED_URLS_PER_CAMPAIGN` distinct URLs are kept per
/// campaign.
pub(crate) fn record_click_url(campaign_id: &str, url: &str) {
    let Ok(parsed) = url::Url::parse(url) else {
        return;
    };
    let normalized = parsed.to_string();
    let mut entry = click_urls_store()
        .entry(campaign_id.to_string())
        .or_default();
    if entry.len() < MAX_TRACKED_URLS_PER_CAMPAIGN {
        entry.insert(normalized);
    }
}

/// First hook result without an `error` key, if any handler
/// succeeded. Callers must not consult only `results.first()`: a
/// first handler's error masks a later handler's success and vice
/// versa.
pub(crate) fn pick_hook_success(results: &[Value]) -> Option<&Value> {
    results.iter().find(|result| result.get("error").is_none())
}

/// Error messages from every failed hook result, for aggregation.
pub(crate) fn hook_errors(results: &[Value]) -> Vec<String> {
    results
        .iter()
        .filter_map(|result| {
            result
                .get("error")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .collect()
}
/// Compiled regex matching absolute http(s) `href` values, for click tracking.
pub(crate) fn href_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"href="(https?://[^"]+)""#).expect("href regex must compile")
    })
}

/// Append the open-tracking pixel and rewrite links to the click
/// tracker for one subscriber. The unsubscribe placeholder token is
/// replaced with the subscriber's signed unsubscribe link.
pub(crate) fn personalize_html(
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
            // Never rewrite the tracking pixel itself, a link that
            // already points at this application, or the unsubscribe
            // link: the unsubscribe URL must stay a direct signed
            // link, not a tracked click redirect.
            if original.contains("/api/v1/newsletter/track/")
                || original.contains("/api/v1/newsletter/unsubscribe")
            {
                return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
            }
            record_click_url(campaign_id, original);
            let encoded = url::form_urlencoded::byte_serialize(original.as_bytes()).collect::<String>();
            format!(
                "href=\"{base_url}/api/v1/newsletter/track/click/{campaign_id}/{subscriber_id}?url={encoded}\""
            )
        })
        .into_owned();
    rewritten
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
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
pub(crate) fn render_template(
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
    // Succeed when any handler succeeds; only when every handler
    // failed do the aggregated errors become the failure.
    match pick_hook_success(&rendered.results) {
        Some(result) => {
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
        None => {
            if rendered.results.is_empty() {
                return Err(UnifiedError::new(
                    "Email template service did not answer the template.render_requested event",
                    503,
                ));
            }
            let reasons = hook_errors(&rendered.results);
            Err(UnifiedError::new(
                format!(
                    "Template render failed: {}",
                    if reasons.is_empty() {
                        "template render failed".to_string()
                    } else {
                        reasons.join("; ")
                    }
                ),
                404,
            ))
        }
    }
}

/// Ask the Email Providers module to send one message. Returns true
/// when any handler queued it.
pub(crate) fn request_send(state: &NewsletterState, payload: Value) -> bool {
    let Some(hooks) = state.hooks() else {
        return false;
    };
    let answer = hooks.emit(EVENT_EMAIL_SEND_REQUESTED, payload);
    pick_hook_success(&answer.results).is_some()
}

pub(crate) fn confirmed_subscribers(list_id: &str) -> Vec<Subscriber> {
    let mut items: Vec<Subscriber> = subscribers_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|subscriber| subscriber.list_id == list_id && subscriber.status == "confirmed")
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    items
}

pub(crate) fn find_subscriber(list_id: &str, email: &str) -> Option<Subscriber> {
    let needle = email.trim().to_lowercase();
    subscribers_store()
        .iter()
        .map(|entry| entry.value().clone())
        .find(|subscriber| {
            subscriber.list_id == list_id && subscriber.email.to_lowercase() == needle
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn list(base_url: Option<&str>) -> NewsletterList {
        NewsletterList {
            id: "l1".to_string(),
            name: "List".to_string(),
            description: String::new(),
            default_connection_id: None,
            double_opt_in: true,
            sender_name: None,
            sender_email: None,
            base_url: base_url.map(str::to_string),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn hex_encode_formats_bytes() {
        assert_eq!(hex_encode(b"abc"), "616263");
        assert_eq!(hex_encode(b""), "");
        assert_eq!(hex_encode(&[0, 255]), "00ff");
    }

    #[test]
    fn signatures_equal_compares_exactly() {
        assert!(signatures_equal("abc", "abc"));
        assert!(!signatures_equal("abc", "abd"));
        assert!(!signatures_equal("abc", "abcd"));
    }

    #[test]
    fn validate_email_address_rules() {
        assert!(validate_email_address("a@b.com").is_ok());
        assert!(validate_email_address("  a@b.com  ").is_ok());
        assert!(validate_email_address("no-at-sign").is_err());
        assert!(validate_email_address("a@b").is_err());
        assert!(validate_email_address("a@@b.com").is_err());
        assert!(validate_email_address("").is_err());
    }

    #[test]
    fn validate_connection_id_accepts_uuid_or_empty() {
        assert!(validate_connection_id(&None).is_ok());
        assert!(validate_connection_id(&Some(String::new())).is_ok());
        assert!(
            validate_connection_id(&Some(
                "123e4567-e89b-12d3-a456-426614174000".to_string()
            ))
            .is_ok()
        );
        assert!(validate_connection_id(&Some("not-a-uuid".to_string())).is_err());
    }

    #[test]
    fn list_urls_use_the_list_base_url() {
        let with_base = list(Some("https://example.com/"));
        assert_eq!(list_base_url(&with_base), "https://example.com");
        assert_eq!(
            confirm_url(&with_base, "tok-1"),
            "https://example.com/api/v1/newsletter/subscribers/confirm?token=tok-1"
        );
        assert_eq!(
            unsubscribe_url(&with_base, "tok-1"),
            "https://example.com/api/v1/newsletter/unsubscribe?token=tok-1"
        );
        let without_base = list(None);
        assert_eq!(list_base_url(&without_base), "");
        assert_eq!(
            confirm_url(&without_base, "tok-1"),
            "/api/v1/newsletter/subscribers/confirm?token=tok-1"
        );
    }

    #[test]
    fn personalize_html_rewrites_links_and_adds_pixel() {
        let html = "<html><body><a href=\"https://example.com/page\">x</a>__UNSUBSCRIBE_URL__</body></html>";
        let out = personalize_html(
            html,
            "https://app.example.com",
            "c1",
            "s1",
            "https://app.example.com/unsub",
        );
        assert!(out.contains("https://app.example.com/unsub"));
        assert!(out.contains("/api/v1/newsletter/track/open/c1/s1"));
        assert!(out.contains("/api/v1/newsletter/track/click/c1/s1?url="));
        assert!(out.find("/api/v1/newsletter/track/open").unwrap() < out.find("</body>").unwrap());
        let plain = personalize_html("<p>hi</p>", "https://app.example.com", "c1", "s1", "u");
        assert!(plain.contains("/api/v1/newsletter/track/open/c1/s1"));
    }

    #[test]
    fn personalize_html_keeps_the_unsubscribe_link_direct() {
        let unsub = "https://app.example.com/api/v1/newsletter/unsubscribe?token=tok-1";
        let html = format!(
            "<html><body><a href=\"https://example.com/page\">x</a><a href=\"{unsub}\">stop</a>__UNSUBSCRIBE_URL__</body></html>"
        );
        let out = personalize_html(&html, "https://app.example.com", "c1", "s1", unsub);
        // The unsubscribe link appears verbatim and is never wrapped
        // in a track/click redirect.
        assert!(out.contains(&format!("href=\"{unsub}\"")));
        assert!(!out.contains("track/click/c1/s1?url=https%3A%2F%2Fapp.example.com%2Fapi%2Fv1%2Fnewsletter%2Funsubscribe"));
        // The ordinary link is still rewritten to the click tracker.
        assert!(out.contains("/api/v1/newsletter/track/click/c1/s1?url="));
    }

    #[test]
    fn record_click_url_stores_the_normalized_target() {
        use super::state::click_urls_store;
        let campaign_id = format!("record-{}", Uuid::now_v7());
        record_click_url(&campaign_id, "https://EXAMPLE.com/Page?x=1");
        let stored = click_urls_store()
            .get(&campaign_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        assert!(stored.contains("https://example.com/Page?x=1"));
        assert!(!stored.contains("https://other.example.com/"));
        record_click_url(&campaign_id, "not a url");
        assert_eq!(
            click_urls_store().get(&campaign_id).unwrap().len(),
            1,
            "an unparsable URL must not be recorded"
        );
    }

    #[test]
    fn pick_hook_success_ignores_position() {
        let failed = json!({ "error": "first failed" });
        let ok = json!({ "rendered_html": "<p>hi</p>" });
        assert_eq!(pick_hook_success(&[failed.clone(), ok.clone()]), Some(&ok));
        assert_eq!(pick_hook_success(&[ok.clone(), failed.clone()]), Some(&ok));
        assert_eq!(pick_hook_success(&[failed.clone()]), None);
        assert_eq!(pick_hook_success(&[]), None);
        assert_eq!(
            hook_errors(&[failed, json!({ "error": "second failed" })]),
            vec!["first failed".to_string(), "second failed".to_string()]
        );
    }

    fn template_state(
        first_error: Option<&'static str>,
        second_error: Option<&'static str>,
    ) -> NewsletterState {
        use unified_core::hooks::{HookBus, HookContext};
        let state = NewsletterState::new();
        let bus = Arc::new(HookBus::new());
        let first = first_error.map(|message| {
            Arc::new(move |ctx: &mut HookContext| {
                ctx.add_result(json!({ "error": message }));
            }) as Arc<dyn Fn(&mut HookContext) + Send + Sync>
        });
        let second = second_error.map(|message| {
            Arc::new(move |ctx: &mut HookContext| {
                ctx.add_result(json!({ "error": message }));
            }) as Arc<dyn Fn(&mut HookContext) + Send + Sync>
        });
        // First handler runs first (higher priority); when it is a
        // success it returns rendered output instead of an error.
        if let Some(handler) = first {
            bus.register_with_priority(EVENT_TEMPLATE_RENDER_REQUESTED, "test-first", handler, 10);
        } else {
            bus.register_with_priority(
                EVENT_TEMPLATE_RENDER_REQUESTED,
                "test-first",
                Arc::new(|ctx: &mut HookContext| {
                    ctx.add_result(
                        json!({ "rendered_html": "<p>hi</p>", "subject": "Hello" }),
                    );
                }),
                10,
            );
        }
        if let Some(handler) = second {
            bus.register_with_priority(EVENT_TEMPLATE_RENDER_REQUESTED, "test-second", handler, 0);
        } else {
            bus.register_with_priority(
                EVENT_TEMPLATE_RENDER_REQUESTED,
                "test-second",
                Arc::new(|ctx: &mut HookContext| {
                    ctx.add_result(
                        json!({ "rendered_html": "<p>second</p>", "subject": "Second" }),
                    );
                }),
                0,
            );
        }
        state.set_hooks(bus);
        state
    }

    #[test]
    fn render_template_succeeds_when_any_handler_succeeds() {
        // First handler errors, second succeeds: the old
        // first-result-only code returned the error.
        let state = template_state(Some("template missing"), None);
        let (html, subject) = render_template(&state, "tpl-1", &HashMap::new()).unwrap();
        assert_eq!(html, "<p>second</p>");
        assert_eq!(subject, "Second");
        // First handler succeeds, second errors: still a success.
        let state = template_state(None, Some("late failure"));
        let (html, subject) = render_template(&state, "tpl-1", &HashMap::new()).unwrap();
        assert_eq!(html, "<p>hi</p>");
        assert_eq!(subject, "Hello");
    }

    #[test]
    fn render_template_aggregates_errors_when_all_handlers_fail() {
        let state = template_state(Some("first broke"), Some("second broke"));
        let error = render_template(&state, "tpl-1", &HashMap::new()).unwrap_err();
        assert!(error.message.contains("first broke"), "{}", error.message);
        assert!(error.message.contains("second broke"), "{}", error.message);
    }

    #[test]
    fn request_send_succeeds_when_any_handler_succeeds() {
        use unified_core::hooks::{HookBus, HookContext};
        let state = NewsletterState::new();
        let bus = Arc::new(HookBus::new());
        bus.register_with_priority(
            EVENT_EMAIL_SEND_REQUESTED,
            "test-first",
            Arc::new(|ctx: &mut HookContext| {
                ctx.add_result(json!({ "error": "provider down" }));
            }),
            10,
        );
        bus.register_with_priority(
            EVENT_EMAIL_SEND_REQUESTED,
            "test-second",
            Arc::new(|ctx: &mut HookContext| {
                ctx.add_result(json!({ "queued": true }));
            }),
            0,
        );
        state.set_hooks(bus);
        assert!(request_send(&state, json!({ "to": "a@b.com" })));
        // No handlers at all: false.
        let empty = NewsletterState::new();
        empty.set_hooks(Arc::new(HookBus::new()));
        assert!(!request_send(&empty, json!({ "to": "a@b.com" })));
    }
    #[test]
    fn hex_encode_matches_reference_for_every_byte_value() {
        // Exhaustive oracle: the lookup-table encoding must agree
        // with plain per-byte formatting for all 256 byte values.
        // (`format!` is the reference here, in test code only.)
        for byte in 0..=u8::MAX {
            assert_eq!(hex_encode(&[byte]), format!("{byte:02x}"));
        }
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

}
