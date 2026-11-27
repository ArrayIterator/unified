//! Hook handlers for user deletion, the marketing
//! campaign events, and the AI grounding hooks.
//!
//! `user.deleted` carries only `{id: user_id}` (verified
//! from the users module's delete handler - no tenant, no
//! handle). This module therefore anonymizes the identity
//! fields it owns (`created_by` / `actor` become
//! `deleted-user`) and keeps every row: the publish and
//! analytics history stays auditable. Account handles are
//! the account's public handle on its network, not a user
//! id, and the payload names no handle, so no handle is
//! redacted by deletion - this listener never claims
//! otherwise.
//!
//! `marketing.campaign.created` carries `{campaign_id,
//! name, created_by}` and `marketing.campaign.sent`
//! carries `{campaign_id, queued, skipped, failed,
//! evaluated_contacts, sent_at}` with no tenant (verified
//! from the marketing handlers and service), so the
//! listeners acknowledge the events for cross-channel
//! visibility in the hook result without writing a
//! tenant-scoped row they cannot scope.

use super::handlers_ai::grounding_facts;
use super::state::SocialMarketingState;
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
pub(crate) fn on_user_deleted(state: SocialMarketingState, ctx: &mut HookContext) {
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
            json!({ "social_marketing": "skipped", "reason": "user.deleted carries no user_id" }),
        );
        return;
    };
    let now = Utc::now();
    let mut anonymized_networks = 0i64;
    for mut entry in state.networks.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_networks += 1;
        }
    }
    let mut anonymized_accounts = 0i64;
    for mut entry in state.accounts.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_accounts += 1;
        }
    }
    let mut anonymized_posts = 0i64;
    for mut entry in state.posts.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_posts += 1;
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
    let mut anonymized_audit = 0i64;
    for mut entry in state.audit.iter_mut() {
        if entry.value().actor == user_id {
            entry.value_mut().actor = DELETED_USER_ID.to_string();
            anonymized_audit += 1;
        }
    }
    ctx.add_result(json!({
        "social_marketing": "user_deleted",
        "user_id": user_id,
        "anonymized_networks": anonymized_networks,
        "anonymized_accounts": anonymized_accounts,
        "anonymized_posts": anonymized_posts,
        "anonymized_campaigns": anonymized_campaigns,
        "anonymized_audit": anonymized_audit,
        "note": "Account handles are the account's public handle on its network and user.deleted carries no handle, so no handle was redacted.",
    }));
}

/// A marketing (email) campaign was created. The payload
/// carries no tenant, so this listener records nothing
/// tenant-scoped; it acknowledges the creation so the
/// cross-channel campaign is visible in hook results.
pub(crate) fn on_marketing_campaign_created(_state: SocialMarketingState, ctx: &mut HookContext) {
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
        ctx.add_result(json!({ "social_marketing": "skipped", "reason": "marketing.campaign.created carries no campaign_id" }));
        return;
    };
    ctx.add_result(json!({
        "social_marketing": "marketing_campaign_created",
        "campaign_id": campaign_id,
        "name": ctx.get("name"),
        "note": "Acknowledged only: the marketing payload carries no tenant_id, so no tenant-scoped row was written.",
    }));
}

/// A marketing (email) campaign finished sending. The
/// payload carries no tenant, so this listener records
/// nothing tenant-scoped; it acknowledges the completion
/// with its counts so the cross-channel send is visible in
/// hook results.
pub(crate) fn on_marketing_campaign_sent(_state: SocialMarketingState, ctx: &mut HookContext) {
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
        ctx.add_result(json!({ "social_marketing": "skipped", "reason": "marketing.campaign.sent carries no campaign_id" }));
        return;
    };
    ctx.add_result(json!({
        "social_marketing": "marketing_campaign_sent",
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
pub(crate) fn on_ai_summarize_requested(state: SocialMarketingState, ctx: &mut HookContext) {
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
            json!({ "social_marketing": "skipped", "reason": "ai.summarize.requested carries no campaign_id" }),
        );
        return;
    };
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match grounding_facts(&state, tenant_id, &campaign_id) {
        Ok(facts) => ctx.add_result(json!({ "social_marketing": "summarize", "facts": facts })),
        Err(_) => {
            ctx.add_result(json!({ "social_marketing": "skipped", "reason": "campaign not found" }))
        }
    }
}

/// Answer an AI readiness request: a campaign is ready to
/// summarize when it exists and holds at least one post.
pub(crate) fn on_ai_readiness_requested(state: SocialMarketingState, ctx: &mut HookContext) {
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
            json!({ "social_marketing": "skipped", "reason": "ai.readiness.requested carries no campaign_id" }),
        );
        return;
    };
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let ready = match super::helpers::load_campaign(&state, tenant_id, &campaign_id) {
        Ok(campaign) => !state.posts_of_campaign(tenant_id, campaign.id).is_empty(),
        Err(_) => false,
    };
    ctx.add_result(json!({
        "social_marketing": "readiness",
        "campaign_id": campaign_id,
        "ready": ready,
        "checklist": ["campaign exists", "at least one post in the campaign"],
    }));
}
