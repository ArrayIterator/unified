//! HTTP handlers for opt-outs, opt-out keywords, and the
//! provider webhooks: delivery-status callbacks and inbound
//! keyword handling. Opt-out handling mirrors the
//! newsletter module's unsubscribe conventions: the
//! opt-out list is the single source of truth, sends to a
//! listed number are blocked at send time, and removing a
//! row re-opts the number back in.
//!
//! The webhooks are public routes gated by the provider's
//! shared webhook secret (an `X-Webhook-Secret` header or
//! a `secret` query parameter, compared in constant time
//! against the stored hash). A provider with no webhook
//! secret configured rejects every callback, and an unknown
//! provider id answers 404 - the secret is the credential
//! here, exactly like the referral module's token-gated
//! public link.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::crypto::webhook_secret_matches;
use super::helpers::{
    caller, caller_company, created, err, load_keyword, load_opt_out, load_provider_for_webhook,
    ok, optional_text, paginate, record_audit, require_caller_company, require_manage,
    require_phone, require_tenant, require_view, validate_keyword, validate_keyword_kind,
};
use super::state::SmsMarketingState;
use super::types::{
    CreateKeywordRequest, CreateOptOutRequest, DEFAULT_OPTIN_KEYWORDS, DEFAULT_OPTOUT_KEYWORDS,
    DeliveryWebhookRequest, EVENT_MESSAGE_DELIVERED, EVENT_MESSAGE_FAILED, EVENT_OPTOUT_ADDED,
    InboundWebhookRequest, KeywordKind, MessageStatus, SmsOptOut, SmsOptOutKeyword,
    UpdateKeywordRequest, match_keyword, message_can_transition,
};

/// Seed a tenant's default keyword rows the first time
/// they are needed: the conventional opt-out words as
/// `opt_out` rows and the opt-in words as `opt_in` rows.
/// These are data rows like any other - a tenant edits or
/// deletes them, and matching always reads the rows, never
/// a hard-coded list.
pub(crate) fn ensure_default_keywords(state: &SmsMarketingState, tenant_id: &str) {
    if !state.keywords_of(tenant_id).is_empty() {
        return;
    }
    let now = Utc::now();
    for keyword in DEFAULT_OPTOUT_KEYWORDS {
        let row = SmsOptOutKeyword {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.to_string(),
            keyword: keyword.to_string(),
            kind: KeywordKind::OptOut,
            enabled: true,
            created_by: "system".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.opt_out_keywords.insert(row.id, row);
    }
    for keyword in DEFAULT_OPTIN_KEYWORDS {
        let row = SmsOptOutKeyword {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.to_string(),
            keyword: keyword.to_string(),
            kind: KeywordKind::OptIn,
            enabled: true,
            created_by: "system".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.opt_out_keywords.insert(row.id, row);
    }
}

/// List opt-outs (paginated, filter by source; search by
/// phone number).
pub(crate) async fn list_opt_outs(
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
        let source_filter = params.filters.get("source").cloned();
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.opt_outs_of(&tenant_id);
        items.retain(|row| {
            if let Some(source) = &source_filter
                && row.source != *source
            {
                return false;
            }
            if let Some(needle) = &search
                && !row.phone_number.to_lowercase().contains(needle)
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

/// Add a number to the opt-out list by hand (manager). One
/// row per number per tenant; adding a number that is
/// already listed is a 409.
pub(crate) async fn create_opt_out(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateOptOutRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let phone_number = require_phone(&input.phone_number)?;
        let keyword = match &input.keyword {
            Some(raw) => validate_keyword(raw)?,
            None => "manual".to_string(),
        };
        if state.is_opted_out(&tenant_id, &phone_number) {
            return Err(UnifiedError::conflict(format!(
                "Number {phone_number} is already opted out"
            )));
        }
        let now = Utc::now();
        let row = SmsOptOut {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            phone_number,
            keyword,
            source: "manual".to_string(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.opt_outs.insert(row.id, row.clone());
        record_audit(
            &state,
            &tenant_id,
            "opt_out",
            &row.id.to_string(),
            "create",
            &caller.actor,
            None,
            Some(serde_json::to_value(&row).unwrap_or(Value::Null)),
        );
        state.emit(
            EVENT_OPTOUT_ADDED,
            json!({
                "tenant_id": tenant_id,
                "phone_number": row.phone_number,
                "keyword": row.keyword,
                "source": row.source,
            }),
        );
        Ok(serde_json::to_value(&row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Remove a number from the opt-out list (manager): the
/// re-opt-in path. Sends to the number are allowed again
/// from the next send pass.
pub(crate) async fn delete_opt_out(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let row = load_opt_out(&state, &tenant_id, &path.into_inner())?;
        state.opt_outs.remove(&row.id);
        record_audit(
            &state,
            &tenant_id,
            "opt_out",
            &row.id.to_string(),
            "delete",
            &caller.actor,
            Some(serde_json::to_value(&row).unwrap_or(Value::Null)),
            None,
        );
        Ok(json!({ "deleted": true, "opt_out_id": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List keyword rows (paginated, filter by kind, enabled).
/// The first read for a tenant seeds the default opt-out /
/// opt-in keywords as data rows.
pub(crate) async fn list_keywords(
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
        ensure_default_keywords(&state, &tenant_id);
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let enabled_filter = params
            .filters
            .get("enabled")
            .and_then(|raw| match raw.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.keywords_of(&tenant_id);
        items.retain(|row| {
            if let Some(kind) = &kind_filter
                && row.kind.as_str() != kind
            {
                return false;
            }
            if let Some(enabled) = enabled_filter
                && row.enabled != enabled
            {
                return false;
            }
            if let Some(needle) = &search
                && !row.keyword.to_lowercase().contains(needle)
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

/// Create a keyword row (manager). One row per keyword per
/// kind per tenant; a duplicate is a 409.
pub(crate) async fn create_keyword(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateKeywordRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        ensure_default_keywords(&state, &tenant_id);
        let input = body.into_inner();
        let keyword = validate_keyword(&input.keyword)?;
        let kind = match &input.kind {
            Some(raw) => validate_keyword_kind(raw)?,
            None => KeywordKind::OptOut,
        };
        if state
            .keywords_of(&tenant_id)
            .iter()
            .any(|row| row.keyword == keyword && row.kind == kind)
        {
            return Err(UnifiedError::conflict(format!(
                "Keyword {keyword} already exists for {}",
                kind.as_str()
            )));
        }
        let now = Utc::now();
        let row = SmsOptOutKeyword {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            keyword,
            kind,
            enabled: true,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.opt_out_keywords.insert(row.id, row.clone());
        record_audit(
            &state,
            &tenant_id,
            "opt_out_keyword",
            &row.id.to_string(),
            "create",
            &caller.actor,
            None,
            Some(serde_json::to_value(&row).unwrap_or(Value::Null)),
        );
        Ok(serde_json::to_value(&row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Update a keyword row (manager).
pub(crate) async fn update_keyword(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateKeywordRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut row = load_keyword(&state, &tenant_id, &path.into_inner())?;
        let before = serde_json::to_value(&row).unwrap_or(Value::Null);
        if let Some(keyword) = &input.keyword {
            row.keyword = validate_keyword(keyword)?;
        }
        if let Some(kind) = &input.kind {
            row.kind = validate_keyword_kind(kind)?;
        }
        if let Some(enabled) = input.enabled {
            row.enabled = enabled;
        }
        if state.keywords_of(&tenant_id).iter().any(|other| {
            other.id != row.id && other.keyword == row.keyword && other.kind == row.kind
        }) {
            return Err(UnifiedError::conflict(format!(
                "Keyword {} already exists for {}",
                row.keyword,
                row.kind.as_str()
            )));
        }
        row.updated_at = Utc::now();
        state.opt_out_keywords.insert(row.id, row.clone());
        record_audit(
            &state,
            &tenant_id,
            "opt_out_keyword",
            &row.id.to_string(),
            "update",
            &caller.actor,
            Some(before),
            Some(serde_json::to_value(&row).unwrap_or(Value::Null)),
        );
        Ok(serde_json::to_value(&row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a keyword row (manager).
pub(crate) async fn delete_keyword(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let row = load_keyword(&state, &tenant_id, &path.into_inner())?;
        state.opt_out_keywords.remove(&row.id);
        record_audit(
            &state,
            &tenant_id,
            "opt_out_keyword",
            &row.id.to_string(),
            "delete",
            &caller.actor,
            Some(serde_json::to_value(&row).unwrap_or(Value::Null)),
            None,
        );
        Ok(json!({ "deleted": true, "keyword_id": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Webhooks (public, shared-secret gated)

/// Read the presented webhook secret from the
/// `X-Webhook-Secret` header or the `secret` query
/// parameter. Either may carry it; neither is logged.
fn presented_secret(req: &HttpRequest, query: &HashMap<String, String>) -> Option<String> {
    req.headers()
        .get("X-Webhook-Secret")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .or_else(|| query.get("secret").cloned())
}

fn require_webhook_secret(
    provider_hash: Option<&str>,
    req: &HttpRequest,
    query: &HashMap<String, String>,
) -> UnifiedResult<()> {
    match presented_secret(req, query) {
        Some(secret) if webhook_secret_matches(provider_hash, &secret) => Ok(()),
        _ => Err(UnifiedError::unauthorized(
            "Invalid or missing webhook secret".to_string(),
        )),
    }
}

/// Delivery-status webhook: the provider posts back per
/// message with `{message_id | provider_message_id, status,
/// error?}`. `status` maps to `sent`, `delivered`,
/// `failed`, or `undelivered`. The message is matched by
/// provider message id first, then by this module's message
/// id, inside the provider's tenant. A callback for a
/// transition the status machine does not allow (for
/// example a delivery report for an already-failed message)
/// is acknowledged and ignored, never an error - providers
/// retry webhooks that answer with an error. Emits
/// `sms.message.delivered` / `sms.message.failed`.
pub(crate) async fn delivery_webhook(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    query: web::Query<HashMap<String, String>>,
    body: web::Json<DeliveryWebhookRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let provider = load_provider_for_webhook(&state, &path.into_inner())?;
        require_webhook_secret(provider.webhook_secret_hash.as_deref(), &req, &query)?;
        let input = body.into_inner();
        let target = match input.status.trim().to_ascii_lowercase().as_str() {
            "sent" => MessageStatus::Sent,
            "delivered" => MessageStatus::Delivered,
            "failed" => MessageStatus::Failed,
            "undelivered" => MessageStatus::Undelivered,
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "status must be sent, delivered, failed, or undelivered: {other}"
                )));
            }
        };
        let tenant_id = provider.tenant_id.clone();
        let message = state
            .messages
            .iter()
            .map(|entry| entry.value().clone())
            .find(|message| {
                message.tenant_id == tenant_id
                    && message.provider_id == provider.id
                    && (input
                        .provider_message_id
                        .as_deref()
                        .is_some_and(|id| message.provider_message_id.as_deref() == Some(id))
                        || input
                            .message_id
                            .as_deref()
                            .is_some_and(|id| message.id.to_string() == id))
            });
        let Some(mut message) = message else {
            return Err(UnifiedError::not_found(
                "Message not found for this provider".to_string(),
            ));
        };
        if !message_can_transition(message.status, target) {
            return Ok(json!({
                "acknowledged": true,
                "applied": false,
                "reason": format!(
                    "message in status {} cannot move to {}",
                    message.status.as_str(),
                    target.as_str()
                ),
            }));
        }
        let from = message.status;
        message.status = target;
        match target {
            MessageStatus::Sent => message.sent_at = Some(Utc::now()),
            MessageStatus::Delivered => message.delivered_at = Some(Utc::now()),
            MessageStatus::Failed => {
                message.error = optional_text(&input.error, "error", 500)?;
            }
            MessageStatus::Undelivered => {
                message.error = optional_text(&input.error, "error", 500)?;
            }
            _ => {}
        }
        message.updated_at = Utc::now();
        state.messages.insert(message.id, message.clone());
        super::helpers::refresh_campaign_stats(&state, message.campaign_id);
        match target {
            MessageStatus::Delivered => state.emit(
                EVENT_MESSAGE_DELIVERED,
                json!({
                    "tenant_id": tenant_id,
                    "campaign_id": message.campaign_id.to_string(),
                    "message_id": message.id.to_string(),
                    "provider_id": provider.id.to_string(),
                    "phone_number": message.phone_number,
                }),
            ),
            MessageStatus::Failed | MessageStatus::Undelivered => state.emit(
                EVENT_MESSAGE_FAILED,
                json!({
                    "tenant_id": tenant_id,
                    "campaign_id": message.campaign_id.to_string(),
                    "message_id": message.id.to_string(),
                    "provider_id": provider.id.to_string(),
                    "phone_number": message.phone_number,
                    "error": message.error,
                }),
            ),
            _ => {}
        }
        Ok(json!({
            "acknowledged": true,
            "applied": true,
            "message_id": message.id.to_string(),
            "from": from.as_str(),
            "status": target.as_str(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Inbound-message webhook: the provider posts an inbound
/// SMS with `{from, body}`. The body is matched against the
/// tenant's enabled keyword rows: an opt-out keyword adds
/// the sender to the opt-out list (idempotent) and emits
/// `sms.optout.added`; an opt-in keyword removes the sender
/// from the list. A body that matches no keyword is
/// acknowledged and ignored.
pub(crate) async fn inbound_webhook(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    query: web::Query<HashMap<String, String>>,
    body: web::Json<InboundWebhookRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let provider = load_provider_for_webhook(&state, &path.into_inner())?;
        require_webhook_secret(provider.webhook_secret_hash.as_deref(), &req, &query)?;
        let input = body.into_inner();
        let tenant_id = provider.tenant_id.clone();
        let phone_number = require_phone(&input.from)?;
        ensure_default_keywords(&state, &tenant_id);
        let keywords = state.keywords_of(&tenant_id);
        let opt_out_words: Vec<String> = keywords
            .iter()
            .filter(|row| row.enabled && row.kind == KeywordKind::OptOut)
            .map(|row| row.keyword.clone())
            .collect();
        let opt_in_words: Vec<String> = keywords
            .iter()
            .filter(|row| row.enabled && row.kind == KeywordKind::OptIn)
            .map(|row| row.keyword.clone())
            .collect();
        if let Some(keyword) = match_keyword(&input.body, &opt_out_words) {
            if !state.is_opted_out(&tenant_id, &phone_number) {
                let now = Utc::now();
                let row = SmsOptOut {
                    id: Uuid::new_v4(),
                    tenant_id: tenant_id.clone(),
                    phone_number: phone_number.clone(),
                    keyword: keyword.clone(),
                    source: "keyword".to_string(),
                    created_by: "system".to_string(),
                    created_at: now,
                    updated_at: now,
                };
                state.opt_outs.insert(row.id, row);
                state.emit(
                    EVENT_OPTOUT_ADDED,
                    json!({
                        "tenant_id": tenant_id,
                        "phone_number": phone_number,
                        "keyword": keyword,
                        "source": "keyword",
                    }),
                );
            }
            return Ok(json!({
                "acknowledged": true,
                "action": "opted_out",
                "phone_number": phone_number,
                "keyword": keyword,
            }));
        }
        if let Some(keyword) = match_keyword(&input.body, &opt_in_words) {
            if let Some(row) = state.opt_out_for(&tenant_id, &phone_number) {
                state.opt_outs.remove(&row.id);
            }
            return Ok(json!({
                "acknowledged": true,
                "action": "opted_in",
                "phone_number": phone_number,
                "keyword": keyword,
            }));
        }
        Ok(json!({
            "acknowledged": true,
            "action": "ignored",
            "phone_number": phone_number,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
