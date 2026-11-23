// Newsletter module - campaign sending.
//
// Split from `src/modules/newsletter.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::constants::{EVENT_NEWSLETTER_CAMPAIGN_COMPLETED, EVENT_NEWSLETTER_CAMPAIGN_SENT, STALE_SENDING_TIMEOUT_SECS, UNSUBSCRIBE_TOKEN_TTL_SECS};
use super::helpers::{confirmed_subscribers, list_base_url, now_string, personalize_html, render_template, request_send, unsubscribe_url};
use super::state::{NewsletterState, campaigns_store, lists_store};
use super::types::{Campaign, NewsletterList};

/// Atomically claim a campaign for sending. The status check and the
/// transition to `sending` happen while holding the entry lock for
/// that campaign, so two concurrent sends cannot both proceed: the
/// second one sees `sending` and gets a conflict instead of
/// double-mailing. A `sending` campaign whose last update is older
/// than the stale timeout is treated as abandoned by a crashed
/// sender and is reclaimed.
pub(crate) fn claim_campaign_for_sending(campaign_id: &str) -> UnifiedResult<Campaign> {
    let Some(mut campaign) = campaigns_store().get_mut(campaign_id) else {
        return Err(UnifiedError::not_found("Campaign not found"));
    };
    if campaign.status == "sending" && !is_stale_sending(&campaign.updated_at) {
        return Err(UnifiedError::conflict("Campaign is already sending"));
    }
    if campaign.status == "completed" || campaign.status == "sent" {
        return Err(UnifiedError::conflict(
            "Campaign was already sent; duplicate it to send again",
        ));
    }
    campaign.status = "sending".to_string();
    campaign.updated_at = now_string();
    Ok(campaign.clone())
}

/// True when a `sending` campaign's last update is older than the
/// stale timeout, or unparsable so freshness cannot be proven.
pub(crate) fn is_stale_sending(updated_at: &str) -> bool {
    let Ok(when) = DateTime::parse_from_rfc3339(updated_at) else {
        return true;
    };
    Utc::now().signed_duration_since(when.with_timezone(&Utc))
        > chrono::Duration::seconds(STALE_SENDING_TIMEOUT_SECS)
}

/// Reset every stale `sending` campaign to `failed`, so a crashed
/// send no longer leaves a campaign stuck in `sending` forever.
/// Failed campaigns are never silently re-sent: an operator
/// duplicates the campaign to send again. Returns how many were reset.
pub(crate) fn recover_stale_sending_campaigns() -> u64 {
    let mut recovered = 0u64;
    for mut entry in campaigns_store().iter_mut() {
        if entry.status == "sending" && is_stale_sending(&entry.updated_at) {
            entry.status = "failed".to_string();
            entry.updated_at = now_string();
            recovered += 1;
        }
    }
    recovered
}

/// Mark a campaign `failed` when it is still in the `sending` state
/// left behind by a failed send attempt (the claim holder's cleanup).
pub(crate) fn mark_campaign_failed_if_sending(campaign_id: &str) {
    if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
        if campaign.status == "sending" {
            campaign.status = "failed".to_string();
            campaign.updated_at = now_string();
        }
    }
}

/// Mark a campaign `failed`, unless another sender currently holds
/// its `sending` claim: a worker must never fail a campaign that is
/// still being sent by someone else.
pub(crate) fn mark_campaign_failed_if_not_sending(campaign_id: &str) {
    if let Some(mut campaign) = campaigns_store().get_mut(campaign_id) {
        if campaign.status != "sending" {
            campaign.status = "failed".to_string();
            campaign.updated_at = now_string();
        }
    }
}

/// Send a campaign to every confirmed subscriber of its list.
/// Returns (queued, failed). The campaign is marked `completed` once
/// the send phase finished; delivery confirmations arrive later
/// through provider webhooks.
pub(crate) async fn send_campaign(state: &Arc<NewsletterState>, campaign_id: &str) -> UnifiedResult<Value> {
    let mut campaign = claim_campaign_for_sending(campaign_id)?;
    let list = match lists_store()
        .get(&campaign.list_id)
        .map(|entry| entry.value().clone())
    {
        Some(list) => list,
        None => {
            mark_campaign_failed_if_sending(campaign_id);
            return Err(UnifiedError::not_found("Campaign list not found"));
        }
    };
    match send_campaign_after_claim(state, &mut campaign, &list).await {
        Ok(value) => Ok(value),
        Err(error) => {
            // A failure after the claim (for example the template
            // render failing) must not leave the campaign stuck in
            // `sending`: release the claim as `failed`.
            mark_campaign_failed_if_sending(campaign_id);
            Err(error)
        }
    }
}

async fn send_campaign_after_claim(
    state: &Arc<NewsletterState>,
    campaign: &mut Campaign,
    list: &NewsletterList,
) -> UnifiedResult<Value> {
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
pub(crate) fn spawn_campaign_worker(state: Arc<NewsletterState>, campaign_id: Option<String>) {
    // Sweep crashed sends first: a stale `sending` campaign is reset
    // to `failed` instead of blocking its slot forever.
    recover_stale_sending_campaigns();
    let run_campaign_id = campaign_id.clone();
    let run = move |state: Arc<NewsletterState>| {
        let due: Vec<String> = if let Some(id) = run_campaign_id {
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
                mark_campaign_failed_if_not_sending(&id);
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
                    mark_campaign_failed_if_not_sending(&id);
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::CampaignStats;
    use uuid::Uuid;

    fn seed_campaign(status: &str, updated_at: &str) -> String {
        let id = format!("camp-{}", Uuid::now_v7());
        campaigns_store().insert(
            id.clone(),
            Campaign {
                id: id.clone(),
                list_id: format!("list-{}", Uuid::now_v7()),
                name: "Campaign".to_string(),
                template_id: "tpl".to_string(),
                subject: String::new(),
                preview_text: None,
                connection_id: None,
                status: status.to_string(),
                scheduled_at: None,
                sent_at: None,
                stats: CampaignStats::default(),
                created_at: now_string(),
                updated_at: updated_at.to_string(),
            },
        );
        id
    }

    fn status_of(campaign_id: &str) -> String {
        campaigns_store()
            .get(campaign_id)
            .map(|entry| entry.value().status.clone())
            .unwrap_or_default()
    }

    fn hours_ago(hours: i64) -> String {
        (Utc::now() - chrono::Duration::hours(hours)).to_rfc3339()
    }

    #[test]
    fn claim_conflicts_while_a_send_is_in_flight() {
        let id = seed_campaign("draft", &now_string());
        let claimed = claim_campaign_for_sending(&id).unwrap();
        assert_eq!(claimed.status, "sending");
        assert_eq!(status_of(&id), "sending");
        // A second concurrent claim must fail instead of double-mailing.
        assert!(claim_campaign_for_sending(&id).is_err());
        assert_eq!(status_of(&id), "sending");
    }

    #[test]
    fn claim_rejects_already_sent_campaigns() {
        for status in ["completed", "sent"] {
            let id = seed_campaign(status, &now_string());
            assert!(claim_campaign_for_sending(&id).is_err());
            assert_eq!(status_of(&id), status);
        }
    }

    #[test]
    fn claim_reclaims_a_stale_sending_campaign_but_not_a_fresh_one() {
        let stale = seed_campaign("sending", &hours_ago(2));
        assert!(is_stale_sending(&hours_ago(2)));
        assert!(!is_stale_sending(&now_string()));
        assert!(is_stale_sending("not-a-timestamp"));
        assert!(claim_campaign_for_sending(&stale).is_ok());
        assert_eq!(status_of(&stale), "sending");

        let fresh = seed_campaign("sending", &now_string());
        assert!(claim_campaign_for_sending(&fresh).is_err());
        assert_eq!(status_of(&fresh), "sending");
    }

    #[test]
    fn recover_stale_sending_resets_only_stale_campaigns() {
        let stale = seed_campaign("sending", &hours_ago(2));
        let fresh = seed_campaign("sending", &now_string());
        let draft = seed_campaign("draft", &now_string());
        let recovered = recover_stale_sending_campaigns();
        assert!(recovered >= 1, "at least the stale campaign must be reset");
        assert_eq!(status_of(&stale), "failed");
        assert_eq!(status_of(&fresh), "sending");
        assert_eq!(status_of(&draft), "draft");
    }

    #[test]
    fn failed_marking_respects_the_sending_claim() {
        let sending = seed_campaign("sending", &now_string());
        let completed = seed_campaign("completed", &now_string());
        // Claim-holder cleanup: a still-sending campaign is failed.
        mark_campaign_failed_if_sending(&sending);
        assert_eq!(status_of(&sending), "failed");
        // Worker cleanup: never fail a campaign another sender holds.
        let in_flight = seed_campaign("sending", &now_string());
        mark_campaign_failed_if_not_sending(&in_flight);
        assert_eq!(status_of(&in_flight), "sending");
        // ...but a finished campaign in an error path is failed.
        mark_campaign_failed_if_not_sending(&completed);
        assert_eq!(status_of(&completed), "failed");
    }
}
