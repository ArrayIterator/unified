// Marketing Automation - Business logic shared by handlers and hooks.

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
use super::service_sequences::*;
use super::handlers_campaigns::*;
use super::handlers_segments::*;
use super::handlers_sequences::*;
use super::handlers_scores::*;
use super::handlers_audience::*;
use super::hooks::*;
use super::module::*;

pub(crate) fn value_as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Array(items) => items
            .iter()
            .map(value_as_text)
            .collect::<Vec<String>>()
            .join(","),
        _ => String::new(),
    }
}

pub(crate) fn value_as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

pub(crate) fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

pub(crate) fn eval_leaf(field_value: &Value, op: &str, expected: &Value) -> bool {
    match op {
        "is_empty" => value_is_empty(field_value),
        "is_not_empty" => !value_is_empty(field_value),
        "eq" => value_as_text(field_value).to_lowercase() == value_as_text(expected).to_lowercase(),
        "ne" => value_as_text(field_value).to_lowercase() != value_as_text(expected).to_lowercase(),
        "contains" => value_as_text(field_value)
            .to_lowercase()
            .contains(&value_as_text(expected).to_lowercase()),
        "starts_with" => value_as_text(field_value)
            .to_lowercase()
            .starts_with(&value_as_text(expected).to_lowercase()),
        "ends_with" => value_as_text(field_value)
            .to_lowercase()
            .ends_with(&value_as_text(expected).to_lowercase()),
        "gt" | "gte" | "lt" | "lte" => {
            match (value_as_number(field_value), value_as_number(expected)) {
                (Some(actual), Some(wanted)) => match op {
                    "gt" => actual > wanted,
                    "gte" => actual >= wanted,
                    "lt" => actual < wanted,
                    _ => actual <= wanted,
                },
                _ => {
                    let actual = value_as_text(field_value);
                    let wanted = value_as_text(expected);
                    match op {
                        "gt" => actual > wanted,
                        "gte" => actual >= wanted,
                        "lt" => actual < wanted,
                        _ => actual <= wanted,
                    }
                }
            }
        }
        "in" => match expected.as_array() {
            Some(options) => {
                if let Some(items) = field_value.as_array() {
                    items.iter().any(|item| {
                        options.iter().any(|option| {
                            value_as_text(item).to_lowercase()
                                == value_as_text(option).to_lowercase()
                        })
                    })
                } else {
                    options.iter().any(|option| {
                        value_as_text(field_value).to_lowercase()
                            == value_as_text(option).to_lowercase()
                    })
                }
            }
            None => false,
        },
        "not_in" => match expected.as_array() {
            Some(options) => {
                if let Some(items) = field_value.as_array() {
                    !items.iter().any(|item| {
                        options.iter().any(|option| {
                            value_as_text(item).to_lowercase()
                                == value_as_text(option).to_lowercase()
                        })
                    })
                } else {
                    !options.iter().any(|option| {
                        value_as_text(field_value).to_lowercase()
                            == value_as_text(option).to_lowercase()
                    })
                }
            }
            None => false,
        },
        _ => false,
    }
}

pub(crate) fn eval_rule_tree(tree: &Value, contact: &Value) -> bool {
    let Some(object) = tree.as_object() else {
        return false;
    };
    if let Some(rules) = object.get("rules").and_then(|value| value.as_array()) {
        let operator = object
            .get("operator")
            .and_then(|value| value.as_str())
            .unwrap_or("and");
        if operator == "or" {
            rules.iter().any(|rule| eval_rule_tree(rule, contact))
        } else {
            rules.iter().all(|rule| eval_rule_tree(rule, contact))
        }
    } else {
        let Some(field) = object.get("field").and_then(|value| value.as_str()) else {
            return false;
        };
        let Some(op) = object.get("op").and_then(|value| value.as_str()) else {
            return false;
        };
        // Unknown fields never match: a rule may only match data
        // that is actually present on the contact snapshot.
        let Some(field_value) = contact.get(field) else {
            return false;
        };
        let expected = object.get("value").cloned().unwrap_or(Value::Null);
        eval_leaf(field_value, op, &expected)
    }
}

/// Build the JSON document a segment rule evaluates against for one
/// audience contact: snapshot fields plus the live lead score.
pub(crate) fn contact_document(state: &MarketingState, contact: &AudienceContact) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("contact_id".to_string(), json!(contact.contact_id));
    map.insert("name".to_string(), json!(contact.name));
    map.insert("email".to_string(), json!(contact.email));
    map.insert(
        "company".to_string(),
        contact.company.clone().map(Value::String).unwrap_or(Value::Null),
    );
    map.insert(
        "phone".to_string(),
        contact.phone.clone().map(Value::String).unwrap_or(Value::Null),
    );
    map.insert("tags".to_string(), json!(contact.tags));
    map.insert(
        "source".to_string(),
        contact.source.clone().map(Value::String).unwrap_or(Value::Null),
    );
    map.insert("created_at".to_string(), json!(contact.updated_at));
    let score = state
        .scores
        .get(&contact.contact_id)
        .map(|entry| entry.score)
        .unwrap_or(0);
    map.insert("score".to_string(), json!(score));
    for (key, value) in &contact.fields {
        map.insert(key.clone(), value.clone());
    }
    Value::Object(map)
}

/// Evaluate a segment against the known audience. Returns the
/// matching contact ids and how many contacts were evaluated.
pub(crate) fn evaluate_segment(state: &MarketingState, segment: &Segment) -> (Vec<String>, u64) {
    let mut members = Vec::new();
    let mut evaluated = 0u64;
    for entry in state.audience.iter() {
        let contact = entry.value();
        evaluated += 1;
        let document = contact_document(state, contact);
        if eval_rule_tree(&segment.rule_tree, &document) {
            members.push(contact.contact_id.clone());
        }
    }
    members.sort();
    (members, evaluated)
}

pub(crate) fn record_segment_evaluation(
    state: &MarketingState,
    segment_id: &str,
    members: &[String],
    evaluated: u64,
) {
    let snapshot = SegmentSizeSnapshot {
        id: new_id(),
        segment_id: segment_id.to_string(),
        size: members.len() as u64,
        evaluated_contacts: evaluated,
        recorded_at: now_string(),
    };
    let mut history = state
        .segment_history
        .entry(segment_id.to_string())
        .or_default();
    history.push(snapshot);
    if history.len() > MAX_SEGMENT_HISTORY {
        let excess = history.len() - MAX_SEGMENT_HISTORY;
        history.drain(0..excess);
    }
    state
        .segment_members
        .insert(segment_id.to_string(), members.to_vec());
    if let Some(mut segment) = state.segments.get_mut(segment_id) {
        segment.last_evaluated_at = Some(now_string());
        segment.last_size = members.len() as u64;
        segment.updated_at = now_string();
    }
}

// Consent, suppression, and the send gate.

/// True when the contact is suppressed globally, or for this
/// campaign. Suppression is always honoured before any send.
pub(crate) fn is_suppressed(state: &MarketingState, email: &str, campaign_id: Option<&str>) -> bool {
    let needle = email.trim().to_lowercase();
    state.suppressions.iter().any(|entry| {
        let record = entry.value();
        if record.email.to_lowercase() != needle {
            return false;
        }
        if record.scope == "global" {
            return true;
        }
        match (record.campaign_id.as_deref(), campaign_id) {
            (Some(scope_campaign), Some(wanted)) => scope_campaign == wanted,
            _ => false,
        }
    })
}

/// Outcome of the consent gate: `Ok(())` means the send may go
/// ahead, `Err(reason)` names why it must not.
pub(crate) fn consent_gate(
    state: &MarketingState,
    contact_id: &str,
    email: &str,
) -> Result<(), String> {
    let settings = state.settings_snapshot();
    let key = email.trim().to_lowercase();
    if let Some(entry) = state.consent_cache.get(&key) {
        if entry.purpose == settings.consent_purpose {
            return if entry.granted {
                Ok(())
            } else {
                Err("consent_withdrawn".to_string())
            };
        }
    }
    // Consent events from compliance carry a user id rather than an
    // email address, so the cache is also keyed by contact id.
    if let Some(entry) = state.consent_cache.get(contact_id) {
        if entry.purpose == settings.consent_purpose {
            return if entry.granted {
                Ok(())
            } else {
                Err("consent_withdrawn".to_string())
            };
        }
    }
    if let Some(hooks) = state.hooks() {
        let answer = hooks.emit(
            EVENT_COMPLIANCE_CONSENT_CHECK,
            json!({
                "contact_id": contact_id,
                "email": email,
                "purpose": settings.consent_purpose,
            }),
        );
        if let Some(result) = answer.results.first() {
            if result.get("error").is_none() {
                if let Some(granted) = result.get("granted").and_then(|value| value.as_bool()) {
                    state.consent_cache.insert(
                        key.clone(),
                        ConsentCacheEntry {
                            key: key.clone(),
                            contact_id: Some(contact_id.to_string()),
                            granted,
                            purpose: settings.consent_purpose.clone(),
                            source: EVENT_COMPLIANCE_CONSENT_CHECK.to_string(),
                            updated_at: now_string(),
                        },
                    );
                    return if granted {
                        Ok(())
                    } else {
                        Err("consent_not_granted".to_string())
                    };
                }
            }
        }
    }
    // Fail closed: no consent on record and no handler answered the
    // check, so the contact is never mailed.
    Err("consent_unverified".to_string())
}

pub(crate) fn record_skipped_send(
    state: &MarketingState,
    campaign_id: Option<&str>,
    sequence_id: Option<&str>,
    step_id: Option<&str>,
    enrollment_id: Option<&str>,
    contact_id: &str,
    email: &str,
    variant_id: Option<&str>,
    reason: &str,
) {
    let record = MarketingSend {
        id: new_id(),
        campaign_id: campaign_id.map(str::to_string),
        sequence_id: sequence_id.map(str::to_string),
        step_id: step_id.map(str::to_string),
        enrollment_id: enrollment_id.map(str::to_string),
        contact_id: contact_id.to_string(),
        email: email.to_string(),
        variant_id: variant_id.map(str::to_string),
        status: "skipped".to_string(),
        mail_job_id: None,
        skip_reason: Some(reason.to_string()),
        created_at: now_string(),
        updated_at: now_string(),
    };
    let send_id = record.id.clone();
    state.sends.insert(record.id.clone(), record);
    state.emit(
        EVENT_MARKETING_SEND_SKIPPED,
        json!({
            "send_id": send_id,
            "campaign_id": campaign_id,
            "sequence_id": sequence_id,
            "contact_id": contact_id,
            "email": email,
            "reason": reason,
        }),
    );
    if let Some(campaign_id) = campaign_id {
        if let Some(mut campaign) = state.campaigns.get_mut(campaign_id) {
            campaign.stats.skipped += 1;
        }
    }
}

/// Ask the Mail module to send one message. Returns the mail job id
/// when a handler queued it, or an error describing why no handler
/// answered.
pub(crate) fn request_mail_send(state: &MarketingState, payload: Value) -> UnifiedResult<String> {
    let Some(hooks) = state.hooks() else {
        return Err(UnifiedError::new(
            "Mail service is unavailable (no hook bus)",
            503,
        ));
    };
    let answer = hooks.emit(EVENT_MAIL_SEND_REQUESTED, payload);
    match answer.results.first() {
        Some(result) if result.get("error").is_none() => {
            let job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if job_id.is_empty() {
                return Err(UnifiedError::new(
                    "Mail service queued the message but returned no job id",
                    503,
                ));
            }
            Ok(job_id)
        }
        Some(result) => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("mail send was refused");
            Err(UnifiedError::new(
                format!("Mail service refused the message: {reason}"),
                503,
            ))
        }
        None => Err(UnifiedError::new(
            "Mail service did not answer the mail.send_requested event: no mail handler is registered",
            503,
        )),
    }
}

// Lead scoring.

/// Effective points of one score event, applying the owning rule's
/// linear decay. An event whose rule was deleted keeps full points.
pub(crate) fn effective_event_points(state: &MarketingState, event: &ScoreEvent) -> i64 {
    let Some(rule_id) = &event.rule_id else {
        return event.points;
    };
    let Some(rule) = state.score_rules.get(rule_id).map(|entry| entry.value().clone()) else {
        return event.points;
    };
    let Some(decay_days) = rule.decay_days.filter(|days| *days > 0) else {
        return event.points;
    };
    let Ok(at) = DateTime::parse_from_rfc3339(&event.at) else {
        return event.points;
    };
    let age_days = (Utc::now() - at.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0;
    if age_days <= 0.0 {
        return event.points;
    }
    let remaining = 1.0 - age_days / decay_days as f64;
    if remaining <= 0.0 {
        return 0;
    }
    (event.points as f64 * remaining).round() as i64
}

/// Recompute a contact score from its events (decay applied) and
/// refresh the warm/hot flags. Fires the hot-lead handoff event
/// exactly once per crossing.
pub(crate) fn refresh_score(state: &MarketingState, contact_id: &str) {
    let settings = state.settings_snapshot();
    let Some(mut record) = state.scores.get_mut(contact_id) else {
        return;
    };
    let total: i64 = record
        .events
        .iter()
        .map(|event| effective_event_points(state, event))
        .sum();
    let was_hot = record.hot;
    record.score = total;
    record.warm = total >= settings.warm_threshold;
    record.hot = total >= settings.hot_threshold;
    record.updated_at = now_string();
    let (score, hot, already_notified) = (
        record.score,
        record.hot,
        record.hot_notified_at.is_some(),
    );
    if hot && !was_hot && !already_notified {
        record.hot_notified_at = Some(now_string());
    }
    let email = record.email.clone();
    drop(record);
    state.emit(
        EVENT_MARKETING_SCORE_UPDATED,
        json!({ "contact_id": contact_id, "email": email, "score": score, "hot": hot }),
    );
    if hot && !was_hot && !already_notified {
        state.emit(
            EVENT_MARKETING_LEAD_HOT,
            json!({
                "contact_id": contact_id,
                "email": email,
                "score": score,
                "threshold": settings.hot_threshold,
            }),
        );
    }
}

/// Apply every active scoring rule matching `action` to a contact.
/// Returns the number of rules applied.
pub(crate) fn apply_score_action(
    state: &MarketingState,
    contact_id: &str,
    email: &str,
    action: &str,
) -> usize {
    let rules: Vec<ScoreRule> = state
        .score_rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| rule.active && rule.action == action)
        .collect();
    if rules.is_empty() {
        return 0;
    }
    let now = now_string();
    let mut record = state.scores.entry(contact_id.to_string()).or_insert_with(|| {
        ContactScore {
            contact_id: contact_id.to_string(),
            email: email.to_string(),
            score: 0,
            warm: false,
            hot: false,
            hot_notified_at: None,
            last_action_at: None,
            events: Vec::new(),
            updated_at: now.clone(),
        }
    });
    for rule in &rules {
        record.events.push(ScoreEvent {
            id: new_id(),
            action: action.to_string(),
            points: rule.points,
            rule_id: Some(rule.id.clone()),
            at: now.clone(),
        });
    }
    if record.events.len() > MAX_SCORE_EVENTS {
        let excess = record.events.len() - MAX_SCORE_EVENTS;
        record.events.drain(0..excess);
    }
    record.last_action_at = Some(now.clone());
    record.updated_at = now;
    drop(record);
    refresh_score(state, contact_id);
    rules.len()
}

// Campaign send execution.

/// Variant assignment for a recipient: the bucket is derived from
/// the recipient, so the same contact always sees the same variant.
pub(crate) fn pick_variant_for(
    variants: &[CampaignVariant],
    contact_id: &str,
) -> Option<CampaignVariant> {
    let total_weight: u32 = variants.iter().map(|variant| variant.weight_percent as u32).sum();
    if variants.is_empty() || total_weight == 0 {
        return None;
    }
    let bucket = (hash_contact(contact_id) % 100) as u32;
    if bucket >= total_weight {
        return None;
    }
    let mut cursor = 0u32;
    for variant in variants {
        cursor += variant.weight_percent as u32;
        if bucket < cursor {
            return Some(variant.clone());
        }
    }
    None
}

pub(crate) fn campaign_variants(state: &MarketingState, campaign_id: &str) -> Vec<CampaignVariant> {
    let mut items: Vec<CampaignVariant> = state
        .variants
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|variant| variant.campaign_id == campaign_id)
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    items
}

/// Send one campaign to its segment members. Returns
/// (queued, skipped, failed). Every recipient passes the
/// suppression and consent gates first; skips are audited.
pub(crate) fn execute_campaign_send(
    state: &Arc<MarketingState>,
    campaign_id: &str,
) -> UnifiedResult<Value> {
    let mut campaign = state
        .campaigns
        .get(campaign_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
    if campaign.status == "sending" {
        return Err(UnifiedError::conflict("Campaign is already sending"));
    }
    if campaign.status == "sent" {
        return Err(UnifiedError::conflict(
            "Campaign was already sent; duplicate it to send again",
        ));
    }
    if campaign.status == "paused" {
        return Err(UnifiedError::conflict(
            "Campaign is paused; resume it before sending",
        ));
    }
    let Some(segment_id) = campaign.segment_id.clone() else {
        return Err(UnifiedError::unprocessable(
            "Campaign has no segment; assign a segment before sending",
        ));
    };
    let Some(segment) = state
        .segments
        .get(&segment_id)
        .map(|entry| entry.value().clone())
    else {
        return Err(UnifiedError::not_found("Campaign segment not found"));
    };
    let (members, evaluated) = evaluate_segment(state, &segment);
    record_segment_evaluation(state, &segment_id, &members, evaluated);
    let variants = campaign_variants(state, campaign_id);
    let settings = state.settings_snapshot();
    let cap = settings.max_sends_per_run as usize;

    campaign.status = "sending".to_string();
    campaign.updated_at = now_string();
    state.campaigns.insert(campaign.id.clone(), campaign.clone());

    let mut queued = 0u64;
    let mut skipped = 0u64;
    let mut failed = 0u64;
    for contact_id in members.iter().take(cap) {
        let Some(contact) = state
            .audience
            .get(contact_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        if is_suppressed(state, &contact.email, Some(campaign_id)) {
            record_skipped_send(
                state,
                Some(campaign_id),
                None,
                None,
                None,
                &contact.contact_id,
                &contact.email,
                None,
                "unsubscribed",
            );
            skipped += 1;
            continue;
        }
        if let Err(reason) = consent_gate(state, &contact.contact_id, &contact.email) {
            record_skipped_send(
                state,
                Some(campaign_id),
                None,
                None,
                None,
                &contact.contact_id,
                &contact.email,
                None,
                &reason,
            );
            skipped += 1;
            continue;
        }
        let variant = pick_variant_for(&variants, &contact.contact_id);
        let subject = variant
            .as_ref()
            .map(|item| item.subject.clone())
            .unwrap_or_else(|| campaign.subject.clone());
        let body_text = variant
            .as_ref()
            .and_then(|item| item.body_text.clone())
            .or_else(|| campaign.body_text.clone());
        let body_html = variant
            .as_ref()
            .and_then(|item| item.body_html.clone())
            .or_else(|| campaign.body_html.clone());
        let send_id = new_id();
        let mail_payload = json!({
            "to": contact.email,
            "subject": subject,
            "body_text": body_text,
            "body_html": body_html,
            "template_id": campaign.template_id,
            "variables": {
                "contact_id": contact.contact_id,
                "campaign_id": campaign.id,
                "send_id": send_id,
            },
        });
        match request_mail_send(state, mail_payload) {
            Ok(job_id) => {
                state.sends.insert(
                    send_id.clone(),
                    MarketingSend {
                        id: send_id,
                        campaign_id: Some(campaign.id.clone()),
                        sequence_id: None,
                        step_id: None,
                        enrollment_id: None,
                        contact_id: contact.contact_id.clone(),
                        email: contact.email.clone(),
                        variant_id: variant.as_ref().map(|item| item.id.clone()),
                        status: "queued".to_string(),
                        mail_job_id: Some(job_id),
                        skip_reason: None,
                        created_at: now_string(),
                        updated_at: now_string(),
                    },
                );
                if let Some(variant) = &variant {
                    if let Some(mut stored) = state.variants.get_mut(&variant.id) {
                        stored.sent += 1;
                    }
                }
                queued += 1;
            }
            Err(error) => {
                state.sends.insert(
                    send_id.clone(),
                    MarketingSend {
                        id: send_id,
                        campaign_id: Some(campaign.id.clone()),
                        sequence_id: None,
                        step_id: None,
                        enrollment_id: None,
                        contact_id: contact.contact_id.clone(),
                        email: contact.email.clone(),
                        variant_id: variant.as_ref().map(|item| item.id.clone()),
                        status: "failed".to_string(),
                        mail_job_id: None,
                        skip_reason: Some(error.message.clone()),
                        created_at: now_string(),
                        updated_at: now_string(),
                    },
                );
                failed += 1;
            }
        }
    }

    if let Some(mut stored) = state.campaigns.get_mut(campaign_id) {
        stored.stats.queued += queued;
        stored.stats.failed += failed;
        stored.sent_at = Some(now_string());
        stored.updated_at = now_string();
        if failed > 0 && queued == 0 {
            stored.status = "draft".to_string();
        } else {
            stored.status = "sent".to_string();
        }
        campaign = stored.clone();
    }
    if campaign.status == "sent" {
        state.emit(
            EVENT_MARKETING_CAMPAIGN_SENT,
            json!({
                "campaign_id": campaign.id,
                "queued": queued,
                "skipped": skipped,
                "failed": failed,
                "evaluated_contacts": evaluated,
                "sent_at": campaign.sent_at,
            }),
        );
        Ok(json!({
            "campaign_id": campaign.id,
            "queued": queued,
            "skipped": skipped,
            "failed": failed,
            "recipients": members.len(),
            "evaluated_contacts": evaluated,
            "status": campaign.status,
        }))
    } else {
        Err(UnifiedError::new(
            "No mail handler queued any message: the mail.send_requested event went unanswered",
            503,
        ))
    }
}

pub(crate) fn upsert_audience_contact(state: &MarketingState, input: AudienceSyncInput) -> UnifiedResult<AudienceContact> {
    let contact_id = require_text(&input.contact_id, "contact_id", 200)?;
    if !valid_email(&input.email) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address for contact {contact_id}"
        )));
    }
    let name = require_text(&input.name, "name", 200)?;
    let mut tags: Vec<String> = input
        .tags
        .unwrap_or_default()
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    tags.sort();
    tags.dedup();
    if tags.len() > 100 {
        return Err(UnifiedError::bad_request("A contact may have at most 100 tags"));
    }
    let email = input.email.trim().to_lowercase();
    if let Some(granted) = input.consented {
        let settings = state.settings_snapshot();
        state.consent_cache.insert(
            email.clone(),
            ConsentCacheEntry {
                key: email.clone(),
                contact_id: Some(contact_id.clone()),
                granted,
                purpose: settings.consent_purpose,
                source: EVENT_MARKETING_CONTACTS_SYNC.to_string(),
                updated_at: now_string(),
            },
        );
    }
    let contact = AudienceContact {
        contact_id: contact_id.clone(),
        email,
        name,
        company: input.company.filter(|value| !value.trim().is_empty()),
        phone: input.phone.filter(|value| !value.trim().is_empty()),
        tags,
        source: input.source.filter(|value| !value.trim().is_empty()),
        fields: input.fields.unwrap_or_default(),
        updated_at: now_string(),
    };
    state.audience.insert(contact_id, contact.clone());
    Ok(contact)
}

/// Record an unsubscribe. Without a campaign id it is global and
/// blocks every future send; with one it blocks only that campaign.
pub(crate) fn record_unsubscribe(
    state: &MarketingState,
    email: &str,
    campaign_id: Option<&str>,
    reason: Option<&str>,
) -> UnifiedResult<Suppression> {
    if !valid_email(email) {
        return Err(UnifiedError::bad_request("email is not a valid email address"));
    }
    let email = email.trim().to_lowercase();
    if let Some(campaign_id) = campaign_id {
        parse_uuid(campaign_id, "campaign")?;
        if !state.campaigns.contains_key(campaign_id) {
            return Err(UnifiedError::not_found("Campaign not found"));
        }
    }
    let record = Suppression {
        id: new_id(),
        email: email.clone(),
        scope: if campaign_id.is_some() { "campaign".to_string() } else { "global".to_string() },
        campaign_id: campaign_id.map(str::to_string),
        reason: reason.map(str::to_string),
        created_at: now_string(),
    };
    state.suppressions.insert(record.id.clone(), record.clone());
    // An unsubscribe also withdraws the cached marketing consent.
    let settings = state.settings_snapshot();
    state.consent_cache.insert(
        email.clone(),
        ConsentCacheEntry {
            key: email.clone(),
            contact_id: None,
            granted: false,
            purpose: settings.consent_purpose,
            source: EVENT_MARKETING_UNSUBSCRIBED.to_string(),
            updated_at: now_string(),
        },
    );
    // Stop any active enrollments for this address immediately.
    let enrollment_ids: Vec<String> = state
        .enrollments
        .iter()
        .filter(|entry| {
            entry.value().email.to_lowercase() == email && entry.value().status == "active"
        })
        .map(|entry| entry.key().clone())
        .collect();
    for enrollment_id in enrollment_ids {
        if let Some(mut enrollment) = state.enrollments.get_mut(&enrollment_id) {
            enrollment.status = "unsubscribed".to_string();
            enrollment.next_step_at = None;
            enrollment.updated_at = now_string();
        }
    }
    state.emit(
        EVENT_MARKETING_UNSUBSCRIBED,
        json!({
            "email": email,
            "campaign_id": campaign_id,
            "reason": reason,
            "suppression_id": record.id,
        }),
    );
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn eval_leaf_compares_text_case_insensitively() {
        assert!(eval_leaf(&json!("Hello"), "eq", &json!("hello")));
        assert!(!eval_leaf(&json!("Hello"), "eq", &json!("bye")));
        assert!(eval_leaf(&json!("Hello"), "ne", &json!("bye")));
        assert!(eval_leaf(&json!("Hello World"), "contains", &json!("WORLD")));
        assert!(eval_leaf(&json!("Hello"), "starts_with", &json!("he")));
        assert!(eval_leaf(&json!("Hello"), "ends_with", &json!("LO")));
        assert!(!eval_leaf(&json!("Hello"), "bogus", &json!("Hello")));
    }

    #[test]
    fn eval_leaf_compares_numbers_numerically() {
        assert!(eval_leaf(&json!(5), "gt", &json!(3)));
        assert!(!eval_leaf(&json!(3), "gt", &json!(5)));
        assert!(eval_leaf(&json!(5), "gte", &json!(5)));
        assert!(eval_leaf(&json!(2), "lt", &json!(5)));
        assert!(eval_leaf(&json!(5), "lte", &json!(5)));
        assert!(eval_leaf(&json!("b"), "gt", &json!("a")));
    }

    #[test]
    fn eval_leaf_handles_membership_and_emptiness() {
        assert!(eval_leaf(&json!("red"), "in", &json!(["red", "blue"])));
        assert!(!eval_leaf(&json!("green"), "in", &json!(["red", "blue"])));
        assert!(eval_leaf(&json!(["a", "b"]), "in", &json!(["b", "c"])));
        assert!(!eval_leaf(&json!("red"), "not_in", &json!(["red", "blue"])));
        assert!(eval_leaf(&json!("green"), "not_in", &json!(["red", "blue"])));
        assert!(eval_leaf(&json!(""), "is_empty", &json!(null)));
        assert!(eval_leaf(&json!([]), "is_empty", &json!(null)));
        assert!(eval_leaf(&json!("x"), "is_not_empty", &json!(null)));
        assert!(!eval_leaf(&json!("x"), "is_empty", &json!(null)));
    }

    #[test]
    fn eval_rule_tree_and_requires_every_rule() {
        let contact = json!({"email": "ana@example.com", "score": 40});
        let tree = json!({"operator": "and", "rules": [
            {"field": "email", "op": "contains", "value": "example"},
            {"field": "score", "op": "gte", "value": 30},
        ]});
        assert!(eval_rule_tree(&tree, &contact));
        let failing = json!({"operator": "and", "rules": [
            {"field": "email", "op": "contains", "value": "example"},
            {"field": "score", "op": "gte", "value": 100},
        ]});
        assert!(!eval_rule_tree(&failing, &contact));
    }

    #[test]
    fn eval_rule_tree_or_needs_one_rule() {
        let contact = json!({"email": "ana@example.com", "score": 40});
        let tree = json!({"operator": "or", "rules": [
            {"field": "email", "op": "eq", "value": "other@example.com"},
            {"field": "score", "op": "gte", "value": 30},
        ]});
        assert!(eval_rule_tree(&tree, &contact));
        let failing = json!({"operator": "or", "rules": [
            {"field": "email", "op": "eq", "value": "other@example.com"},
            {"field": "score", "op": "gte", "value": 100},
        ]});
        assert!(!eval_rule_tree(&failing, &contact));
    }

    #[test]
    fn eval_rule_tree_never_matches_missing_data() {
        let contact = json!({"email": "ana@example.com"});
        assert!(!eval_rule_tree(
            &json!({"field": "company", "op": "eq", "value": "Acme"}),
            &contact
        ));
        assert!(!eval_rule_tree(&json!("not an object"), &contact));
        assert!(!eval_rule_tree(&json!({}), &contact));
    }

    #[test]
    fn value_helpers_cover_text_numbers_and_emptiness() {
        assert_eq!(value_as_text(&json!("hi")), "hi");
        assert_eq!(value_as_text(&json!(42)), "42");
        assert_eq!(value_as_text(&json!(true)), "true");
        assert_eq!(value_as_number(&json!(2.5)), Some(2.5));
        assert_eq!(value_as_number(&json!("3")), Some(3.0));
        assert_eq!(value_as_number(&json!("nope")), None);
        assert!(value_is_empty(&json!(null)));
        assert!(value_is_empty(&json!("   ")));
        assert!(value_is_empty(&json!({})));
        assert!(!value_is_empty(&json!(0)));
    }
}
