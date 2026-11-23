//! Hook handlers for user deletion, the marketing
//! campaign completion event, and the AI grounding hooks.
//!
//! `user.deleted` carries only `{id: user_id}` (verified
//! from the users module's delete handler — no tenant, no
//! phone number). This module therefore anonymizes the
//! identity fields it owns (`created_by` / `updated_by` /
//! `actor` become `deleted-user`) and keeps every row: the
//! delivery history stays auditable. Recipient phone
//! numbers cannot be attributed to the deleted user from
//! this payload — no row in this module links a user id to
//! a phone number — so no recipient row is redacted, and
//! this listener never claims otherwise.
//!
//! `marketing.campaign.sent` carries `{campaign_id,
//! queued, skipped, failed, evaluated_contacts, sent_at}`
//! with no tenant (verified from the marketing service), so
//! the listener acknowledges the event for cross-channel
//! visibility in the hook result without writing a
//! tenant-scoped row it cannot scope.

use super::handlers_ai::grounding_facts;
use super::state::SmsMarketingState;
use super::types::DELETED_USER_ID;
use chrono::Utc;
use serde_json::json;
use unified_core::hooks::HookContext;

/// The AI module's runner emits this event for the summary
/// capability; this module answers with the campaign's
/// stored facts.
pub(crate) fn ai_summarize_event() -> &'static str {
    "ai.summarize.requested"
}

/// The AI module's runner emits this event for the readiness
/// capability; this module answers with a readiness checklist.
pub(crate) fn ai_readiness_event() -> &'static str {
    "ai.readiness.requested"
}

/// `user.deleted` carries `user_id` (falling back to `id`,
/// the same shape the sign and spreadsheets modules read).
/// Every identity field this module owns is anonymized to
/// `deleted-user`; rows are never deleted.
pub(crate) fn on_user_deleted(state: SmsMarketingState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(user_id) = user_id else {
        ctx.add_result(
            json!({ "sms_marketing": "skipped", "reason": "user.deleted carries no user_id" }),
        );
        return;
    };
    let now = Utc::now();
    let mut anonymized_providers = 0i64;
    for mut entry in state.providers.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_providers += 1;
        }
    }
    let mut anonymized_templates = 0i64;
    for mut entry in state.templates.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_templates += 1;
        }
    }
    let mut anonymized_lists = 0i64;
    for mut entry in state.contact_lists.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_lists += 1;
        }
    }
    let mut anonymized_contacts = 0i64;
    for mut entry in state.contacts.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_contacts += 1;
        }
    }
    let mut anonymized_campaigns = 0i64;
    for mut entry in state.campaigns.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_campaigns += 1;
        }
    }
    let mut anonymized_opt_outs = 0i64;
    for mut entry in state.opt_outs.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_opt_outs += 1;
        }
    }
    let mut anonymized_keywords = 0i64;
    for mut entry in state.opt_out_keywords.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_keywords += 1;
        }
    }
    let mut anonymized_rate_limits = 0i64;
    for mut entry in state.rate_limits.iter_mut() {
        if entry.value().updated_by == user_id {
            entry.value_mut().updated_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_rate_limits += 1;
        }
    }
    let mut anonymized_audit = 0i64;
    for mut entry in state.audit.iter_mut() {
        if entry.value().actor == user_id {
            entry.value_mut().actor = DELETED_USER_ID.to_string();
            anonymized_audit += 1;
        }
    }
    ctx.add_result(json!({
        "sms_marketing": "user_deleted",
        "user_id": user_id,
        "anonymized_providers": anonymized_providers,
        "anonymized_templates": anonymized_templates,
        "anonymized_lists": anonymized_lists,
        "anonymized_contacts": anonymized_contacts,
        "anonymized_campaigns": anonymized_campaigns,
        "anonymized_opt_outs": anonymized_opt_outs,
        "anonymized_keywords": anonymized_keywords,
        "anonymized_rate_limits": anonymized_rate_limits,
        "anonymized_audit": anonymized_audit,
        "note": "Recipient phone numbers are not linked to a user id in this module and user.deleted carries no phone number, so no recipient row was redacted.",
    }));
}

/// A marketing (email) campaign finished sending. The
/// payload carries no tenant, so this listener records
/// nothing tenant-scoped; it acknowledges the completion
/// with its counts so the cross-channel send is visible in
/// hook results.
pub(crate) fn on_marketing_campaign_sent(_state: SmsMarketingState, ctx: &mut HookContext) {
    let campaign_id = ctx
        .get("campaign_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(campaign_id) = campaign_id else {
        ctx.add_result(json!({ "sms_marketing": "skipped", "reason": "marketing.campaign.sent carries no campaign_id" }));
        return;
    };
    ctx.add_result(json!({
        "sms_marketing": "marketing_campaign_sent",
        "campaign_id": campaign_id,
        "queued": ctx.get("queued"),
        "skipped": ctx.get("skipped"),
        "failed": ctx.get("failed"),
        "note": "Acknowledged only: the marketing payload carries no tenant_id, so no tenant-scoped row was written.",
    }));
}

/// Answer an AI summary request with the campaign's stored
/// facts. The payload names the campaign under
/// `campaign_id` (falling back to `id`, the shape other
/// modules use).
pub(crate) fn on_ai_summarize_requested(state: SmsMarketingState, ctx: &mut HookContext) {
    let campaign_id = ctx
        .get("campaign_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(campaign_id) = campaign_id else {
        ctx.add_result(
            json!({ "sms_marketing": "skipped", "reason": "ai.summarize.requested carries no campaign_id" }),
        );
        return;
    };
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match grounding_facts(&state, tenant_id, &campaign_id) {
        Ok(facts) => ctx.add_result(json!({ "sms_marketing": "summarize", "facts": facts })),
        Err(_) => {
            ctx.add_result(json!({ "sms_marketing": "skipped", "reason": "campaign not found" }))
        }
    }
}

/// Answer an AI readiness request: a campaign is ready to
/// summarize when it exists and resolves to at least one
/// recipient.
pub(crate) fn on_ai_readiness_requested(state: SmsMarketingState, ctx: &mut HookContext) {
    let campaign_id = ctx
        .get("campaign_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(campaign_id) = campaign_id else {
        ctx.add_result(
            json!({ "sms_marketing": "skipped", "reason": "ai.readiness.requested carries no campaign_id" }),
        );
        return;
    };
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let ready = match super::helpers::load_campaign(&state, tenant_id, &campaign_id) {
        Ok(campaign) => !super::helpers::resolve_recipients(&state, &campaign).is_empty(),
        Err(_) => false,
    };
    ctx.add_result(json!({
        "sms_marketing": "readiness",
        "campaign_id": campaign_id,
        "ready": ready,
        "checklist": ["campaign exists", "at least one recipient resolves"],
    }));
}
