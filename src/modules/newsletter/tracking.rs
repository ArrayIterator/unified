// Newsletter module - open and click tracking handlers.
//
// Split from `src/modules/newsletter.rs`; the module root re-exports the public API of this module.

use unified_core::error::UnifiedError;
use unified_core::json as api_json;
use actix_web::{web, HttpResponse};
use dashmap::mapref::entry::Entry;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use url::Url;
use uuid::Uuid;

use super::constants::{EVENT_NEWSLETTER_CLICK, EVENT_NEWSLETTER_OPEN, TRACKING_PIXEL};
use super::helpers::{now_string};
use super::state::{NewsletterState, campaigns_store, click_urls_store, lists_store, seen_tracking_store, subscribers_store, tracking_store};
use super::types::{TrackingEvent};

// ---------------------------------------------------------------------------
// Tracking.
// ---------------------------------------------------------------------------

/// Record one open/click event and update the campaign stats.
/// Returns false (recording nothing) when the campaign does not
/// exist, the subscriber does not exist, or the subscriber belongs
/// to a different list than the campaign, so forged tracking URLs
/// cannot inflate stats for arbitrary ids.
pub(crate) fn record_tracking(
    state: &NewsletterState,
    campaign_id: &str,
    subscriber_id: &str,
    kind: &str,
    url: Option<String>,
) -> bool {
    let Some(campaign) = campaigns_store()
        .get(campaign_id)
        .map(|entry| entry.value().clone())
    else {
        return false;
    };
    let Some(subscriber) = subscribers_store()
        .get(subscriber_id)
        .map(|entry| entry.value().clone())
    else {
        return false;
    };
    if subscriber.list_id != campaign.list_id {
        return false;
    }
    let event = TrackingEvent {
        id: Uuid::now_v7().to_string(),
        campaign_id: campaign_id.to_string(),
        subscriber_id: subscriber_id.to_string(),
        kind: kind.to_string(),
        url: url.clone(),
        created_at: now_string(),
    };
    tracking_store().insert(event.id.clone(), event);
    // Unique counts: one per subscriber per campaign per kind. The
    // claim is atomic through the map entry API, so two concurrent
    // duplicate events cannot both count as the first.
    let seen_key = format!("{campaign_id}\0{subscriber_id}\0{kind}");
    let is_first = match seen_tracking_store().entry(seen_key) {
        Entry::Occupied(_) => false,
        Entry::Vacant(slot) => {
            slot.insert(());
            true
        }
    };
    if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
        match kind {
            "open" => {
                campaign.stats.opened += 1;
                if is_first {
                    campaign.stats.unique_opened += 1;
                }
            }
            "click" => {
                campaign.stats.clicked += 1;
                if is_first {
                    campaign.stats.unique_clicked += 1;
                }
            }
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
    true
}

/// A click target is allowed only when it was recorded for this
/// campaign while its HTML was personalized, or when its host is the
/// campaign list's own base-URL host (the allowlisted domain set).
/// Anything else is an open redirect and is refused.
pub(crate) fn is_allowed_click_url(campaign_id: &str, url: &Url) -> bool {
    if let Some(recorded) = click_urls_store().get(campaign_id) {
        if recorded.contains(&url.to_string()) {
            return true;
        }
    }
    let Some(campaign) = campaigns_store()
        .get(campaign_id)
        .map(|entry| entry.value().clone())
    else {
        return false;
    };
    let Some(list) = lists_store()
        .get(&campaign.list_id)
        .map(|entry| entry.value().clone())
    else {
        return false;
    };
    let Some(base) = list.base_url.as_deref().filter(|raw| !raw.trim().is_empty()) else {
        return false;
    };
    let Ok(base_url) = Url::parse(base) else {
        return false;
    };
    match (base_url.host_str(), url.host_str()) {
        (Some(allowed), Some(host)) => allowed.eq_ignore_ascii_case(host),
        _ => false,
    }
}

pub(crate) async fn track_open(
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (campaign_id, subscriber_id) = path.into_inner();
    record_tracking(&state, &campaign_id, &subscriber_id, "open", None);
    HttpResponse::Ok()
        .content_type("image/gif")
        .body(TRACKING_PIXEL)
}

pub(crate) async fn track_click(
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
    // Open-redirect guard: refuse any target that was not recorded
    // for this campaign and is not on the campaign list's own domain.
    if !is_allowed_click_url(&campaign_id, &url) {
        return api_json::error_response(
            &UnifiedError::bad_request("url is not a link recorded for this campaign"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::helpers::record_click_url;
    use super::types::{Campaign, CampaignStats, NewsletterList, Subscriber};

    fn seed_campaign(list_base_url: Option<&str>) -> (String, String, String) {
        let list_id = format!("list-{}", Uuid::now_v7());
        lists_store().insert(
            list_id.clone(),
            NewsletterList {
                id: list_id.clone(),
                name: "List".to_string(),
                description: String::new(),
                default_connection_id: None,
                double_opt_in: true,
                sender_name: None,
                sender_email: None,
                base_url: list_base_url.map(str::to_string),
                created_at: String::new(),
                updated_at: String::new(),
            },
        );
        let campaign_id = format!("camp-{}", Uuid::now_v7());
        campaigns_store().insert(
            campaign_id.clone(),
            Campaign {
                id: campaign_id.clone(),
                list_id: list_id.clone(),
                name: "Campaign".to_string(),
                template_id: "tpl".to_string(),
                subject: String::new(),
                preview_text: None,
                connection_id: None,
                status: "draft".to_string(),
                scheduled_at: None,
                sent_at: None,
                stats: CampaignStats::default(),
                created_at: String::new(),
                updated_at: String::new(),
            },
        );
        let subscriber_id = format!("sub-{}", Uuid::now_v7());
        subscribers_store().insert(
            subscriber_id.clone(),
            Subscriber {
                id: subscriber_id.clone(),
                list_id: list_id.clone(),
                email: "a@b.com".to_string(),
                name: None,
                status: "confirmed".to_string(),
                metadata: HashMap::new(),
                confirmed_at: None,
                unsubscribed_at: None,
                created_at: String::new(),
                updated_at: String::new(),
            },
        );
        (list_id, campaign_id, subscriber_id)
    }

    fn campaign_stats(campaign_id: &str) -> CampaignStats {
        campaigns_store()
            .get(campaign_id)
            .map(|entry| entry.value().stats.clone())
            .unwrap_or_default()
    }

    #[test]
    fn record_tracking_ignores_unknown_campaign_and_subscriber() {
        let state = NewsletterState::new();
        let before = tracking_store().len();
        let ghost_campaign = format!("ghost-camp-{}", Uuid::now_v7());
        let ghost_subscriber = format!("ghost-sub-{}", Uuid::now_v7());
        assert!(!record_tracking(&state, &ghost_campaign, "also-ghost", "open", None));
        assert!(!record_tracking(&state, "also-ghost", &ghost_subscriber, "open", None));
        assert_eq!(tracking_store().len(), before, "no event may be stored");
        let (_, campaign_id, _) = seed_campaign(None);
        assert!(!record_tracking(&state, &campaign_id, &ghost_subscriber, "open", None));
        assert_eq!(campaign_stats(&campaign_id).opened, 0);
    }

    #[test]
    fn record_tracking_counts_unique_opens_once() {
        let state = NewsletterState::new();
        let (_, campaign_id, subscriber_id) = seed_campaign(None);
        assert!(record_tracking(&state, &campaign_id, &subscriber_id, "open", None));
        assert!(record_tracking(&state, &campaign_id, &subscriber_id, "open", None));
        assert!(record_tracking(&state, &campaign_id, &subscriber_id, "click", None));
        let stats = campaign_stats(&campaign_id);
        assert_eq!(stats.opened, 2, "totals count every hit");
        assert_eq!(stats.unique_opened, 1, "unique counts the first hit only");
        assert_eq!(stats.clicked, 1);
        assert_eq!(stats.unique_clicked, 1);
    }

    #[test]
    fn record_tracking_rejects_a_subscriber_from_another_list() {
        let state = NewsletterState::new();
        let (_, campaign_id, _) = seed_campaign(None);
        let (_, _, other_subscriber) = seed_campaign(None);
        assert!(!record_tracking(&state, &campaign_id, &other_subscriber, "open", None));
        assert_eq!(campaign_stats(&campaign_id).opened, 0);
    }

    #[test]
    fn is_allowed_click_url_allows_recorded_and_own_domain_only() {
        let (_, campaign_id, _) = seed_campaign(Some("https://app.example.com"));
        let recorded = Url::parse("https://example.com/landing").unwrap();
        record_click_url(&campaign_id, "https://example.com/landing");
        assert!(is_allowed_click_url(&campaign_id, &recorded));
        let own_domain = Url::parse("https://app.example.com/help").unwrap();
        assert!(is_allowed_click_url(&campaign_id, &own_domain));
        let evil = Url::parse("https://evil.example.com/phish").unwrap();
        assert!(!is_allowed_click_url(&campaign_id, &evil));
        let unrecorded = Url::parse("https://other.example.com/page").unwrap();
        assert!(!is_allowed_click_url(&campaign_id, &unrecorded));
        let ghost = format!("ghost-camp-{}", Uuid::now_v7());
        assert!(!is_allowed_click_url(&ghost, &recorded));
    }
}
