// Marketing Automation - Hook-bus listeners.

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
use super::types::*;
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
use super::module::*;

// Hook listeners.

pub(crate) fn hook_mail_outcome(state: &Arc<MarketingState>, ctx: &mut HookContext, delivered: bool) {
    let job_id = ctx
        .get("job_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if job_id.is_empty() {
        return;
    }
    let mut updated: Vec<String> = Vec::new();
    for entry in state.sends.iter() {
        let send = entry.value();
        if send.mail_job_id.as_deref() == Some(job_id.as_str()) && send.status == "queued" {
            updated.push(send.id.clone());
        }
    }
    for send_id in updated {
        let new_status = if delivered { "sent" } else { "failed" };
        let campaign_id = state
            .sends
            .get(&send_id)
            .and_then(|entry| entry.value().campaign_id.clone());
        if let Some(mut send) = state.sends.get_mut(&send_id) {
            send.status = new_status.to_string();
            send.updated_at = now_string();
        }
        if let Some(campaign_id) = campaign_id {
            if let Some(mut campaign) = state.campaigns.get_mut(&campaign_id) {
                if delivered {
                    campaign.stats.sent += 1;
                    campaign.stats.delivered += 1;
                } else {
                    campaign.stats.failed += 1;
                }
            }
        }
    }
}

pub(crate) fn hook_tracking(state: &Arc<MarketingState>, ctx: &mut HookContext, kind: &str) {
    let campaign_id = ctx
        .get("campaign_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if campaign_id.is_empty() || !state.campaigns.contains_key(&campaign_id) {
        return;
    }
    let contact_ref = ctx
        .get("subscriber_id")
        .or_else(|| ctx.get("contact_id"))
        .or_else(|| ctx.get("email"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let url = ctx
        .get("url")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let event = TrackingEvent {
        id: new_id(),
        campaign_id: campaign_id.clone(),
        contact_ref: contact_ref.clone(),
        kind: kind.to_string(),
        url,
        created_at: now_string(),
    };
    state
        .tracking_events
        .insert(event.id.clone(), event.clone());
    // Mark the matching send record opened/clicked when the tracking
    // reference identifies one (newsletter subscriber ids do not map
    // to marketing sends, so this is best-effort only). The match is
    // collected first and mutated afterwards: holding a read guard
    // on the sends map while taking a write guard on the same shard
    // would deadlock.
    let matched_send: Option<MarketingSend> = state
        .sends
        .iter()
        .map(|entry| entry.value().clone())
        .find(|send| {
            send.campaign_id.as_deref() == Some(campaign_id.as_str())
                && (send.contact_id == contact_ref || send.email == contact_ref)
                && (send.status == "sent" || send.status == "queued")
        });
    if let Some(send) = matched_send {
        if let Some(mut stored) = state.sends.get_mut(&send.id) {
            stored.status = kind.to_string();
            stored.updated_at = now_string();
        }
        if let Some(variant_id) = &send.variant_id {
            if let Some(mut variant) = state.variants.get_mut(variant_id) {
                if kind == "open" {
                    variant.opened += 1;
                } else {
                    variant.clicked += 1;
                }
            }
        }
    }
    // Campaign open/click counters are not incremented here: the
    // stats endpoint recomputes them from the stored tracking
    // events, which are the single source of truth.
    // Score the engagement only when the tracking reference is an
    // email address. Newsletter subscriber ids do not map to a
    // marketing contact, and inventing a score record for one would
    // pollute the leaderboard with an unreachable identity.
    if contact_ref.contains('@') {
        apply_score_action(
            state,
            &contact_ref,
            &contact_ref,
            if kind == "open" { "email.opened" } else { "email.clicked" },
        );
    }
}

pub(crate) fn hook_consent(state: &Arc<MarketingState>, ctx: &mut HookContext, granted: bool, source: &str) {
    let settings = state.settings_snapshot();
    let email = ctx
        .get("email")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let contact_id = ctx
        .get("contact_id")
        .or_else(|| ctx.get("user_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let purpose = ctx
        .get("purpose")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if !purpose.is_empty() && purpose != settings.consent_purpose {
        return;
    }
    if email.is_empty() && contact_id.is_none() {
        return;
    }
    let key = if email.is_empty() {
        contact_id.clone().unwrap_or_default().to_lowercase()
    } else {
        email.to_lowercase()
    };
    state.consent_cache.insert(
        key.clone(),
        ConsentCacheEntry {
            key,
            contact_id,
            granted,
            purpose: settings.consent_purpose,
            source: source.to_string(),
            updated_at: now_string(),
        },
    );
}

/// Enrollment triggers from other modules. The payloads are read
/// defensively because each source carries different fields:
/// - `forms.submission.received` carries only `form_id`,
///   `submission_id` and `slug`: it names no submitter, so a form
///   trigger can only enroll when the payload (or a later
///   `marketing.sequence.enroll` event, or the manual enroll
///   endpoint) supplies the contact. The listener still records the
///   form-submitted score action when the contact is known.
/// - `store.order.completed` carries the order itself (`user_id`,
///   no email): the email is resolved from the known audience.
/// - `tag_added` triggers arrive through `marketing.sequence.enroll`
///   with an explicit tag; no module currently emits a tag event.
pub(crate) fn hook_sequence_trigger(state: &Arc<MarketingState>, ctx: &HookContext, trigger: &str) {
    let order = ctx
        .get("order")
        .cloned()
        .unwrap_or_else(|| ctx.data.clone());
    let email = ctx
        .get("email")
        .or_else(|| order.get("email"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let contact_id = ctx
        .get("contact_id")
        .or_else(|| ctx.get("user_id"))
        .or_else(|| order.get("user_id"))
        .or_else(|| order.get("customer_user_id"))
        .or_else(|| order.get("contact_id"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let form_id = ctx
        .get("form_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let tag = ctx
        .get("tag")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    // Resolve the email from the known audience when only the
    // contact id arrived (the store order case).
    let email = if email.is_empty() && !contact_id.is_empty() {
        state
            .audience
            .get(&contact_id)
            .map(|entry| entry.value().email.clone())
            .unwrap_or_default()
    } else {
        email
    };
    if email.is_empty() || contact_id.is_empty() || !valid_email(&email) {
        return;
    }
    // Keep a snapshot of the contact so segments can see them.
    let name = ctx
        .get("name")
        .or_else(|| order.get("name"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if !name.is_empty() {
        let _ = upsert_audience_contact(
            state,
            AudienceSyncInput {
                contact_id: contact_id.clone(),
                email: email.clone(),
                name,
                company: ctx
                    .get("company")
                    .or_else(|| order.get("company"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                phone: None,
                tags: None,
                source: Some(trigger.to_string()),
                fields: None,
                consented: None,
            },
        );
    }
    let sequences: Vec<Sequence> = state
        .sequences
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|sequence| {
            sequence.status == "active"
                && match sequence.trigger_kind.as_str() {
                    "form_submitted" => {
                        trigger == "form_submitted"
                            && sequence.trigger_form_id.as_deref() == Some(form_id.as_str())
                    }
                    "purchase" => trigger == "purchase",
                    "tag_added" => {
                        trigger == "tag_added"
                            && sequence.trigger_tag.as_deref() == Some(tag.as_str())
                    }
                    _ => false,
                }
        })
        .collect();
    for sequence in sequences {
        let _ = enroll_contact(state, &sequence.id, &contact_id, &email);
    }
    if trigger == "purchase" {
        apply_score_action(state, &contact_id, &email, "purchase.completed");
    } else if trigger == "form_submitted" {
        apply_score_action(state, &contact_id, &email, "form.submitted");
    }
}
