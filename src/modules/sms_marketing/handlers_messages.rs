//! HTTP handlers for messages: the per-recipient send
//! records of campaigns, with status filtering and a retry
//! for failed, undelivered, or rate-limited queued rows.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;

use super::adapter::{SmsSendRequest, dispatch};
use super::crypto::{decrypt_credential, encryption_secret};
use super::helpers::{
    caller, caller_company, err, load_message, load_provider, ok, paginate, record_audit,
    refresh_campaign_stats, require_caller_company, require_manage, require_tenant, require_view,
};
use super::state::SmsMarketingState;
use super::types::{EVENT_MESSAGE_FAILED, EVENT_MESSAGE_SENT, MessageStatus, remaining_sends};

/// List messages (paginated, filter by provider, status,
/// campaign; search by phone number / provider message id).
pub(crate) async fn list_messages(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let provider_filter = params.filters.get("provider").cloned();
        let status_filter = params.filters.get("status").cloned();
        let campaign_filter = params.filters.get("campaign").cloned();
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.messages_of(&tenant_id);
        items.retain(|message| {
            if let Some(provider) = &provider_filter
                && message.provider_id.to_string() != *provider
            {
                return false;
            }
            if let Some(status) = &status_filter
                && message.status.as_str() != status
            {
                return false;
            }
            if let Some(campaign) = &campaign_filter
                && message.campaign_id.to_string() != *campaign
            {
                return false;
            }
            if let Some(needle) = &search
                && !message.phone_number.to_lowercase().contains(needle)
                && !message
                    .provider_message_id
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(needle)
            {
                return false;
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Get one message.
pub(crate) async fn get_message(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let message = load_message(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&message).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Retry one message (manager): failed, undelivered, and
/// rate-limited queued rows may be retried. The opt-out
/// check and the rate-limit check run again - a number that
/// opted out since the first attempt is skipped instead of
/// sent, and a retry over the limit stays queued. The
/// stored rendered body is reused, so a template edit after
/// the first send does not change what the retry sends.
pub(crate) async fn retry_message(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let mut message = load_message(&state, &tenant_id, &path.into_inner())?;
        if !matches!(
            message.status,
            MessageStatus::Failed | MessageStatus::Undelivered | MessageStatus::Queued
        ) {
            return Err(UnifiedError::conflict(format!(
                "Message in status {} cannot be retried",
                message.status.as_str()
            )));
        }
        if state.is_opted_out(&tenant_id, &message.phone_number) {
            message.status = MessageStatus::Skipped;
            message.queued_reason = Some("opted_out".to_string());
            message.updated_at = Utc::now();
            state.messages.insert(message.id, message.clone());
            refresh_campaign_stats(&state, message.campaign_id);
            return Ok::<Value, UnifiedError>(
                serde_json::to_value(&message).unwrap_or(Value::Null),
            );
        }
        let provider = load_provider(&state, &tenant_id, &message.provider_id.to_string())?;
        if !provider.enabled {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" is disabled",
                provider.name
            )));
        }
        let tenant_limit = state
            .rate_limit_of(&tenant_id)
            .map(|row| row.sends_per_minute)
            .unwrap_or(super::types::DEFAULT_TENANT_SENDS_PER_MINUTE);
        let tenant_remaining =
            remaining_sends(state.sends_in_last_minute(&tenant_id), tenant_limit);
        let provider_remaining = remaining_sends(
            state.provider_sends_in_last_minute(&tenant_id, provider.id),
            provider.sends_per_minute,
        );
        if tenant_remaining.min(provider_remaining) <= 0 {
            message.status = MessageStatus::Queued;
            message.queued_reason = Some("rate_limited".to_string());
            message.updated_at = Utc::now();
            state.messages.insert(message.id, message.clone());
            refresh_campaign_stats(&state, message.campaign_id);
            return Ok::<Value, UnifiedError>(
                serde_json::to_value(&message).unwrap_or(Value::Null),
            );
        }
        let credential: Option<String> = if provider.auth_type.needs_credential() {
            let Some(stored) = provider.credential_encrypted.as_deref() else {
                return Err(UnifiedError::bad_request(
                    "Provider needs a credential; none is stored".to_string(),
                ));
            };
            let secret = encryption_secret(&ctx)?;
            Some(decrypt_credential(&secret, stored)?)
        } else {
            None
        };
        // A failed or undelivered row is re-queued first
        // (failed -> queued is the allowed transition), and
        // the dispatch below moves it queued -> sent or
        // queued -> failed.
        message.status = MessageStatus::Queued;
        message.queued_reason = None;
        message.error = None;
        message.updated_at = Utc::now();
        let outcome = dispatch(
            &provider,
            credential.as_deref(),
            &SmsSendRequest {
                to: message.phone_number.clone(),
                from: provider.sender_id.clone(),
                body: message.body.clone(),
            },
        )
        .await;
        if outcome.accepted {
            message.status = MessageStatus::Sent;
            message.sent_at = Some(Utc::now());
            message.provider_message_id = outcome.provider_message_id.clone();
            message.error = None;
        } else {
            message.status = MessageStatus::Failed;
            message.error = outcome.error.clone();
        }
        message.updated_at = Utc::now();
        state.messages.insert(message.id, message.clone());
        refresh_campaign_stats(&state, message.campaign_id);
        record_audit(
            &state,
            &tenant_id,
            "message",
            &message.id.to_string(),
            "retry",
            &caller.actor,
            None,
            Some(serde_json::to_value(&message).unwrap_or(Value::Null)),
        );
        if message.status == MessageStatus::Sent {
            state.emit(
                EVENT_MESSAGE_SENT,
                json!({
                    "tenant_id": tenant_id,
                    "campaign_id": message.campaign_id.to_string(),
                    "message_id": message.id.to_string(),
                    "provider_id": provider.id.to_string(),
                    "phone_number": message.phone_number,
                    "segments": message.segments,
                    "provider_message_id": message.provider_message_id,
                }),
            );
        } else {
            state.emit(
                EVENT_MESSAGE_FAILED,
                json!({
                    "tenant_id": tenant_id,
                    "campaign_id": message.campaign_id.to_string(),
                    "message_id": message.id.to_string(),
                    "provider_id": provider.id.to_string(),
                    "phone_number": message.phone_number,
                    "error": message.error,
                }),
            );
        }
        Ok(serde_json::to_value(&message).unwrap_or(Value::Null))
    })()
    .await;
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
