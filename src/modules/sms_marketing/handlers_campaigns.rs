//! HTTP handlers for SMS campaigns: create / list / get /
//! update / delete, the status machine (draft -> scheduled
//! -> sending -> sent, with paused / cancelled), scheduling,
//! and the send pass. The send pass resolves the audience,
//! renders the template per recipient, blocks opted-out
//! numbers, enforces the per-tenant and per-provider
//! per-minute limits at enqueue time (over-limit recipients
//! stay queued, never dropped), and dispatches the rest
//! through the provider adapter.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::adapter::{SmsSendRequest, dispatch};
use super::crypto::{decrypt_credential, encryption_secret};
use super::helpers::{
    caller, caller_company, campaign_is_editable, created, err, load_campaign, load_provider,
    load_template, ok, paginate, parse_send_at, record_audit, refresh_campaign_stats,
    require_caller_company, require_manage, require_tenant, require_text, require_view,
    resolve_recipients, validate_list_ids, validate_recipient_numbers, validate_segment_filter,
    validate_variables,
};
use super::state::SmsMarketingState;
use super::types::{
    CampaignStatus, CreateCampaignRequest, DEFAULT_TENANT_SENDS_PER_MINUTE, EVENT_CAMPAIGN_CREATED,
    EVENT_CAMPAIGN_SCHEDULED, EVENT_CAMPAIGN_SENT, EVENT_MESSAGE_FAILED, EVENT_MESSAGE_SENT,
    MAX_NAME_LEN, MessageStatus, ScheduleCampaignRequest, SmsCampaign, SmsMessage,
    UpdateCampaignRequest, campaign_can_transition, remaining_sends, segment_info,
};

/// List campaigns (paginated, filter by status, provider,
/// template; search by name).
pub(crate) async fn list_campaigns(
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
        let status_filter = params.filters.get("status").cloned();
        let provider_filter = params.filters.get("provider").cloned();
        let template_filter = params.filters.get("template").cloned();
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.campaigns_of(&tenant_id);
        items.retain(|campaign| {
            if let Some(status) = &status_filter
                && campaign.status.as_str() != status
            {
                return false;
            }
            if let Some(provider) = &provider_filter
                && campaign.provider_id.to_string() != *provider
            {
                return false;
            }
            if let Some(template) = &template_filter
                && campaign.template_id.to_string() != *template
            {
                return false;
            }
            if let Some(needle) = &search
                && !campaign.name.to_lowercase().contains(needle)
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

/// Create a campaign (manager). A `send_at` in the future
/// starts the campaign as scheduled; without one it starts
/// as a draft. Template and provider must exist in this
/// tenant.
pub(crate) async fn create_campaign(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateCampaignRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let template = load_template(&state, &tenant_id, &input.template_id)?;
        let provider = load_provider(&state, &tenant_id, &input.provider_id)?;
        if !provider.enabled {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" is disabled",
                provider.name
            )));
        }
        let contact_list_ids = validate_list_ids(&input.contact_list_ids)?;
        let recipient_numbers = validate_recipient_numbers(&input.recipient_numbers)?;
        let segment_filter = match &input.segment_filter {
            Some(filter) => validate_segment_filter(filter)?,
            None => json!({}),
        };
        let variables = validate_variables(&input.variables)?;
        let (status, send_at) = match &input.send_at {
            Some(raw) => {
                let when = parse_send_at(raw)?;
                if when <= Utc::now() {
                    return Err(UnifiedError::bad_request(
                        "send_at must be in the future".to_string(),
                    ));
                }
                (CampaignStatus::Scheduled, Some(when))
            }
            None => (CampaignStatus::Draft, None),
        };
        let now = Utc::now();
        let campaign = SmsCampaign {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            template_id: template.id,
            provider_id: provider.id,
            contact_list_ids,
            recipient_numbers,
            segment_filter,
            variables,
            status,
            send_at,
            sent_at: None,
            stats: super::types::CampaignStats::default(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.campaigns.insert(campaign.id, campaign.clone());
        record_audit(
            &state,
            &tenant_id,
            "campaign",
            &campaign.id.to_string(),
            "create",
            &caller.actor,
            None,
            Some(serde_json::to_value(&campaign).unwrap_or(Value::Null)),
        );
        state.emit(
            EVENT_CAMPAIGN_CREATED,
            json!({
                "tenant_id": tenant_id,
                "campaign_id": campaign.id.to_string(),
                "status": campaign.status.as_str(),
                "created_by": caller.actor,
            }),
        );
        Ok(serde_json::to_value(&campaign).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one campaign.
pub(crate) async fn get_campaign(
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
        let campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&campaign).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a campaign (manager). Only drafts and cancelled
/// campaigns are editable; a campaign that already sent (or
/// is sending) keeps its record.
pub(crate) async fn update_campaign(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateCampaignRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        if !campaign_is_editable(campaign.status) {
            return Err(UnifiedError::conflict(format!(
                "Campaign in status {} cannot be edited",
                campaign.status.as_str()
            )));
        }
        let before = serde_json::to_value(&campaign).unwrap_or(Value::Null);
        if let Some(name) = &input.name {
            campaign.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(template_id) = &input.template_id {
            let template = load_template(&state, &tenant_id, template_id)?;
            campaign.template_id = template.id;
        }
        if let Some(provider_id) = &input.provider_id {
            let provider = load_provider(&state, &tenant_id, provider_id)?;
            campaign.provider_id = provider.id;
        }
        if let Some(list_ids) = &input.contact_list_ids {
            campaign.contact_list_ids = validate_list_ids(list_ids)?;
        }
        if let Some(numbers) = &input.recipient_numbers {
            campaign.recipient_numbers = validate_recipient_numbers(numbers)?;
        }
        if let Some(filter) = &input.segment_filter {
            campaign.segment_filter = validate_segment_filter(filter)?;
        }
        if let Some(variables) = &input.variables {
            campaign.variables = validate_variables(variables)?;
        }
        if let Some(send_at) = &input.send_at {
            match send_at {
                Some(raw) => {
                    let when = parse_send_at(raw)?;
                    if when <= Utc::now() {
                        return Err(UnifiedError::bad_request(
                            "send_at must be in the future".to_string(),
                        ));
                    }
                    campaign.send_at = Some(when);
                    if campaign.status == CampaignStatus::Draft {
                        campaign.status = CampaignStatus::Scheduled;
                    }
                }
                None => {
                    campaign.send_at = None;
                    if campaign.status == CampaignStatus::Scheduled {
                        campaign.status = CampaignStatus::Draft;
                    }
                }
            }
        }
        campaign.updated_at = Utc::now();
        state.campaigns.insert(campaign.id, campaign.clone());
        record_audit(
            &state,
            &tenant_id,
            "campaign",
            &campaign.id.to_string(),
            "update",
            &caller.actor,
            Some(before),
            Some(serde_json::to_value(&campaign).unwrap_or(Value::Null)),
        );
        Ok(serde_json::to_value(&campaign).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a campaign (manager). Only drafts and cancelled
/// campaigns can be deleted; its message rows are kept so
/// the delivery history stays auditable.
pub(crate) async fn delete_campaign(
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
        let campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        if !campaign_is_editable(campaign.status) {
            return Err(UnifiedError::conflict(format!(
                "Campaign in status {} cannot be deleted",
                campaign.status.as_str()
            )));
        }
        state.campaigns.remove(&campaign.id);
        record_audit(
            &state,
            &tenant_id,
            "campaign",
            &campaign.id.to_string(),
            "delete",
            &caller.actor,
            Some(serde_json::to_value(&campaign).unwrap_or(Value::Null)),
            None,
        );
        Ok(json!({ "deleted": true, "campaign_id": campaign.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Schedule a campaign (manager): draft or paused ->
/// scheduled with a future send time. Emits
/// `sms.campaign.scheduled` so a scheduler can pick the
/// campaign up at `send_at`; this module runs no scheduler
/// itself.
pub(crate) async fn schedule_campaign(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<ScheduleCampaignRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        if !campaign_can_transition(campaign.status, CampaignStatus::Scheduled) {
            return Err(UnifiedError::conflict(format!(
                "Campaign in status {} cannot be scheduled",
                campaign.status.as_str()
            )));
        }
        let when = parse_send_at(&input.send_at)?;
        if when <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "send_at must be in the future".to_string(),
            ));
        }
        campaign.status = CampaignStatus::Scheduled;
        campaign.send_at = Some(when);
        campaign.updated_at = Utc::now();
        state.campaigns.insert(campaign.id, campaign.clone());
        record_audit(
            &state,
            &tenant_id,
            "campaign",
            &campaign.id.to_string(),
            "schedule",
            &caller.actor,
            None,
            Some(serde_json::to_value(&campaign).unwrap_or(Value::Null)),
        );
        state.emit(
            EVENT_CAMPAIGN_SCHEDULED,
            json!({
                "tenant_id": tenant_id,
                "campaign_id": campaign.id.to_string(),
                "send_at": when.to_rfc3339(),
            }),
        );
        Ok(serde_json::to_value(&campaign).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Cancel a campaign (manager): draft, scheduled, or paused
/// -> cancelled. Messages already sent stay sent.
pub(crate) async fn cancel_campaign(
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
        let mut campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        if !campaign_can_transition(campaign.status, CampaignStatus::Cancelled) {
            return Err(UnifiedError::conflict(format!(
                "Campaign in status {} cannot be cancelled",
                campaign.status.as_str()
            )));
        }
        campaign.status = CampaignStatus::Cancelled;
        campaign.updated_at = Utc::now();
        state.campaigns.insert(campaign.id, campaign.clone());
        record_audit(
            &state,
            &tenant_id,
            "campaign",
            &campaign.id.to_string(),
            "cancel",
            &caller.actor,
            None,
            Some(serde_json::to_value(&campaign).unwrap_or(Value::Null)),
        );
        Ok(serde_json::to_value(&campaign).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Pause a campaign (manager): scheduled or sending ->
/// paused. A paused campaign can be scheduled again or
/// sent now.
pub(crate) async fn pause_campaign(
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
        let mut campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        if !campaign_can_transition(campaign.status, CampaignStatus::Paused) {
            return Err(UnifiedError::conflict(format!(
                "Campaign in status {} cannot be paused",
                campaign.status.as_str()
            )));
        }
        campaign.status = CampaignStatus::Paused;
        campaign.updated_at = Utc::now();
        state.campaigns.insert(campaign.id, campaign.clone());
        record_audit(
            &state,
            &tenant_id,
            "campaign",
            &campaign.id.to_string(),
            "pause",
            &caller.actor,
            None,
            Some(serde_json::to_value(&campaign).unwrap_or(Value::Null)),
        );
        Ok(serde_json::to_value(&campaign).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Send a campaign now (manager): draft, scheduled, or
/// paused -> sending -> sent. For every recipient the pass
/// renders the template, skips opted-out numbers and
/// recipients with missing variables, holds over-limit
/// recipients as queued (rate limits are enforced at
/// enqueue time, never by dropping), and dispatches the
/// rest through the provider adapter. A log-only provider
/// records the send without any network call; an HTTP
/// provider really posts to its configured endpoint, so
/// live delivery depends on a configured provider with real
/// credentials. Emits `sms.message.sent` /
/// `sms.message.failed` per attempted message and
/// `sms.campaign.sent` with the counts.
pub(crate) async fn send_campaign_now(
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
        let mut campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        if !campaign_can_transition(campaign.status, CampaignStatus::Sending) {
            return Err(UnifiedError::conflict(format!(
                "Campaign in status {} cannot be sent",
                campaign.status.as_str()
            )));
        }
        let template = load_template(&state, &tenant_id, &campaign.template_id.to_string())?;
        let provider = load_provider(&state, &tenant_id, &campaign.provider_id.to_string())?;
        if !provider.enabled {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" is disabled",
                provider.name
            )));
        }
        let recipients = resolve_recipients(&state, &campaign);
        if recipients.is_empty() {
            return Err(UnifiedError::bad_request(
                "Campaign has no recipients: add contact lists, explicit numbers, or loosen the segment filter".to_string(),
            ));
        }
        // Decrypt the credential once for the whole pass; a
        // provider whose auth needs a credential but has none
        // stored (or whose stored credential no longer
        // decrypts) fails the send before any message is
        // created.
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
        // Rate-limit budget for this pass: the tighter of the
        // per-tenant and per-provider remaining sends in the
        // last minute.
        let tenant_limit = state
            .rate_limit_of(&tenant_id)
            .map(|row| row.sends_per_minute)
            .unwrap_or(DEFAULT_TENANT_SENDS_PER_MINUTE);
        let tenant_remaining = remaining_sends(
            state.sends_in_last_minute(&tenant_id),
            tenant_limit,
        );
        let provider_remaining = remaining_sends(
            state.provider_sends_in_last_minute(&tenant_id, provider.id),
            provider.sends_per_minute,
        );
        let mut budget = tenant_remaining.min(provider_remaining);
        let now = Utc::now();
        campaign.status = CampaignStatus::Sending;
        campaign.sent_at = Some(now);
        campaign.updated_at = now;
        state.campaigns.insert(campaign.id, campaign.clone());
        let mut attempted: i64 = 0;
        let mut accepted: i64 = 0;
        let mut failed: i64 = 0;
        let mut skipped: i64 = 0;
        let mut queued: i64 = 0;
        for recipient in &recipients {
            let (rendered, missing) =
                super::types::render_template(&template.body, &recipient.variables);
            let info = segment_info(&rendered);
            let mut message = SmsMessage {
                id: Uuid::new_v4(),
                tenant_id: tenant_id.clone(),
                campaign_id: campaign.id,
                provider_id: provider.id,
                phone_number: recipient.phone_number.clone(),
                body: rendered.clone(),
                segments: info.segments as i64,
                encoding: info.encoding,
                status: MessageStatus::Queued,
                queued_reason: None,
                provider_message_id: None,
                error: None,
                sent_at: None,
                delivered_at: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            if !missing.is_empty() {
                message.status = MessageStatus::Skipped;
                message.queued_reason = Some("missing_variable".to_string());
                message.error = Some(format!(
                    "Missing template variables: {}",
                    missing.join(", ")
                ));
                skipped += 1;
            } else if state.is_opted_out(&tenant_id, &recipient.phone_number) {
                message.status = MessageStatus::Skipped;
                message.queued_reason = Some("opted_out".to_string());
                skipped += 1;
            } else if budget <= 0 {
                message.status = MessageStatus::Queued;
                message.queued_reason = Some("rate_limited".to_string());
                queued += 1;
            } else {
                budget -= 1;
                attempted += 1;
                let outcome = dispatch(
                    &provider,
                    credential.as_deref(),
                    &SmsSendRequest {
                        to: recipient.phone_number.clone(),
                        from: provider.sender_id.clone(),
                        body: rendered.clone(),
                    },
                )
                .await;
                if outcome.accepted {
                    message.status = MessageStatus::Sent;
                    message.sent_at = Some(Utc::now());
                    message.provider_message_id = outcome.provider_message_id.clone();
                    accepted += 1;
                } else {
                    message.status = MessageStatus::Failed;
                    message.error = outcome.error.clone();
                    failed += 1;
                }
                message.updated_at = Utc::now();
                state.messages.insert(message.id, message.clone());
                if message.status == MessageStatus::Sent {
                    state.emit(
                        EVENT_MESSAGE_SENT,
                        json!({
                            "tenant_id": tenant_id,
                            "campaign_id": campaign.id.to_string(),
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
                            "campaign_id": campaign.id.to_string(),
                            "message_id": message.id.to_string(),
                            "provider_id": provider.id.to_string(),
                            "phone_number": message.phone_number,
                            "error": message.error,
                        }),
                    );
                }
                continue;
            }
            state.messages.insert(message.id, message);
        }
        refresh_campaign_stats(&state, campaign.id);
        if let Some(mut stored) = state.campaigns.get_mut(&campaign.id) {
            stored.status = CampaignStatus::Sent;
            stored.updated_at = Utc::now();
        }
        let campaign = load_campaign(&state, &tenant_id, &campaign.id.to_string())?;
        record_audit(
            &state,
            &tenant_id,
            "campaign",
            &campaign.id.to_string(),
            "send",
            &caller.actor,
            None,
            Some(serde_json::to_value(&campaign).unwrap_or(Value::Null)),
        );
        state.emit(
            EVENT_CAMPAIGN_SENT,
            json!({
                "tenant_id": tenant_id,
                "campaign_id": campaign.id.to_string(),
                "recipients": recipients.len(),
                "attempted": attempted,
                "accepted": accepted,
                "failed": failed,
                "skipped": skipped,
                "queued": queued,
                "sent_at": campaign.sent_at,
            }),
        );
        Ok::<Value, UnifiedError>(json!({
            "campaign": campaign,
            "recipients": recipients.len(),
            "attempted": attempted,
            "accepted": accepted,
            "failed": failed,
            "skipped": skipped,
            "queued": queued,
        }))
    })()
    .await;
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
