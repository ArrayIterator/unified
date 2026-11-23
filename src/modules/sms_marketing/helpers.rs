//! Caller, permission, loading, validation, audit, and
//! recipient-resolution helpers.

use super::state::SmsMarketingState;
use super::types::{
    AuditRecord, AuthType, CampaignStatus, KeywordKind, MAX_CAMPAIGN_LISTS, MAX_CAMPAIGN_NUMBERS,
    MAX_CONTACT_TAGS, MAX_DESCRIPTION_LEN, MAX_KEYWORD_LEN, MAX_NAME_LEN, MAX_PHONE_LEN,
    MAX_REFERENCE_LEN, MAX_SENDER_ID_LEN, MAX_TEMPLATE_BODY_LEN, MAX_URL_LEN, MessageStatus,
    PERM_SMS_MANAGE, PERM_SMS_VIEW, ProviderKind, SmsCampaign, SmsContact, SmsContactList,
    SmsMessage, SmsOptOut, SmsOptOutKeyword, SmsProvider, SmsTemplate,
};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the sign module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

pub(crate) fn require_view(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    require_permission(ctx, role, PERM_SMS_VIEW)
}

pub(crate) fn require_manage(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    require_permission(ctx, role, PERM_SMS_MANAGE)
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, group E).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
/// It replaces the old `resolve_tenant` helper, which returned only
/// the tenant id string: handlers now keep the full context so the
/// company scope can be resolved from it.
///
/// Records in this module carry `tenant_id`, not `company_id`, and
/// every state accessor / loader takes that tenant id, so the
/// tenant gate is also the record partition. The company scope
/// below is enforced as a mismatch check on reads and as a
/// presence check on writes; there is no per-record company field
/// to filter on (a C11 follow-up if company partitioning is
/// wanted).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// -- a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified),
    }
}

/// Writes require a resolved company: a mutating handler with no
/// resolvable company scope is refused (403). Reads proceed on the
/// tenant gate plus the mismatch check above.
pub(crate) fn require_caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?
        .ok_or_else(|| UnifiedError::forbidden("A write requires a resolved company scope"))
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(
    raw: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match raw {
        Some(text) => Ok(Some(require_text(text, field, max)?)),
        None => Ok(None),
    }
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
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

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a phone number: normalize to E.164, rejecting
/// numbers without a country code.
pub(crate) fn require_phone(raw: &str) -> UnifiedResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_PHONE_LEN {
        return Err(UnifiedError::bad_request(format!(
            "phone_number must be 1-{MAX_PHONE_LEN} characters"
        )));
    }
    super::types::normalize_phone(trimmed).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "phone_number must be E.164 with a country code (e.g. +6281234567890): {trimmed}"
        ))
    })
}

/// Validate a provider kind string.
pub(crate) fn validate_provider_kind(raw: &str) -> UnifiedResult<ProviderKind> {
    ProviderKind::from_str(raw.trim())
        .ok_or_else(|| UnifiedError::bad_request("kind must be http or log_only".to_string()))
}

/// Validate an auth type string.
pub(crate) fn validate_auth_type(raw: &str) -> UnifiedResult<AuthType> {
    AuthType::from_str(raw.trim()).ok_or_else(|| {
        UnifiedError::bad_request(
            "auth_type must be none, bearer, api_key_header, or basic".to_string(),
        )
    })
}

/// Validate a keyword kind string.
pub(crate) fn validate_keyword_kind(raw: &str) -> UnifiedResult<KeywordKind> {
    KeywordKind::from_str(raw.trim())
        .ok_or_else(|| UnifiedError::bad_request("kind must be opt_out or opt_in".to_string()))
}

/// Validate an opt-out keyword: upper-case, letters only,
/// at most the module bound.
pub(crate) fn validate_keyword(raw: &str) -> UnifiedResult<String> {
    let keyword = require_text(raw, "keyword", MAX_KEYWORD_LEN)?.to_ascii_uppercase();
    if !keyword.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "keyword must contain letters only".to_string(),
        ));
    }
    Ok(keyword)
}

/// Validate a sender id: 1-20 characters, no whitespace.
pub(crate) fn validate_sender_id(raw: &str) -> UnifiedResult<String> {
    let sender = require_text(raw, "sender_id", MAX_SENDER_ID_LEN)?;
    if sender.bytes().any(|b| (b as char).is_whitespace()) {
        return Err(UnifiedError::bad_request(
            "sender_id must not contain whitespace".to_string(),
        ));
    }
    Ok(sender)
}

/// Validate a sends-per-minute value: 0-100000. Zero means
/// the limit blocks everything (a deliberate off switch).
pub(crate) fn validate_sends_per_minute(value: i64) -> UnifiedResult<i64> {
    if !(0..=100_000).contains(&value) {
        return Err(UnifiedError::bad_request(
            "sends_per_minute must be between 0 and 100000".to_string(),
        ));
    }
    Ok(value)
}

/// Validate a template body: plain text, non-empty, at most
/// the module bound.
pub(crate) fn validate_template_body(raw: &str) -> UnifiedResult<String> {
    let body = raw.trim();
    if body.is_empty() {
        return Err(UnifiedError::bad_request(
            "body must not be empty".to_string(),
        ));
    }
    if body.len() > MAX_TEMPLATE_BODY_LEN {
        return Err(UnifiedError::bad_request(format!(
            "body must be at most {MAX_TEMPLATE_BODY_LEN} characters"
        )));
    }
    Ok(body.to_string())
}

/// Validate a contact-list reference list: at most the
/// module bound, no repeats, each a valid UUID string.
pub(crate) fn validate_list_ids(ids: &[String]) -> UnifiedResult<Vec<String>> {
    if ids.len() > MAX_CAMPAIGN_LISTS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_CAMPAIGN_LISTS} contact lists per campaign"
        )));
    }
    let mut seen = HashSet::new();
    let mut clean = Vec::new();
    for id in ids {
        let id = require_text(id, "contact_list_id", MAX_REFERENCE_LEN)?;
        parse_uuid(&id, "contact list")?;
        if !seen.insert(id.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "contact_list_ids must not repeat \"{id}\""
            )));
        }
        clean.push(id);
    }
    Ok(clean)
}

/// Validate explicit recipient numbers: at most the module
/// bound, each normalized to E.164, no repeats.
pub(crate) fn validate_recipient_numbers(numbers: &[String]) -> UnifiedResult<Vec<String>> {
    if numbers.len() > MAX_CAMPAIGN_NUMBERS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_CAMPAIGN_NUMBERS} recipient numbers per campaign"
        )));
    }
    let mut seen = HashSet::new();
    let mut clean = Vec::new();
    for number in numbers {
        let number = require_phone(number)?;
        if !seen.insert(number.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "recipient_numbers must not repeat \"{number}\""
            )));
        }
        clean.push(number);
    }
    Ok(clean)
}

/// Validate contact tags: at most the module bound, no
/// repeats, no empties.
pub(crate) fn validate_tags(tags: &[String]) -> UnifiedResult<Vec<String>> {
    if tags.len() > MAX_CONTACT_TAGS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_CONTACT_TAGS} tags per contact"
        )));
    }
    let mut seen = HashSet::new();
    let mut clean = Vec::new();
    for tag in tags {
        let tag = require_text(tag, "tag", MAX_NAME_LEN)?;
        if !seen.insert(tag.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "tags must not repeat \"{tag}\""
            )));
        }
        clean.push(tag);
    }
    Ok(clean)
}

/// Validate contact variables: keys are variable names
/// (letters, digits, underscores, starting with a letter or
/// underscore), values are plain strings.
pub(crate) fn validate_variables(
    variables: &HashMap<String, String>,
) -> UnifiedResult<HashMap<String, String>> {
    let mut clean = HashMap::new();
    for (key, value) in variables {
        let key = key.trim();
        if key.is_empty()
            || key.len() > super::types::MAX_VARIABLE_NAME_LEN
            || !(key.as_bytes()[0].is_ascii_alphabetic() || key.as_bytes()[0] == b'_')
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(UnifiedError::bad_request(format!(
                "variable name \"{key}\" is not valid"
            )));
        }
        if value.len() > MAX_DESCRIPTION_LEN {
            return Err(UnifiedError::bad_request(format!(
                "variable \"{key}\" must be at most {MAX_DESCRIPTION_LEN} characters"
            )));
        }
        clean.insert(key.to_string(), value.clone());
    }
    Ok(clean)
}

/// Validate a segment filter: it must be a JSON object when
/// present, and its `tags_all` / `tags_any` entries must be
/// strings. The filter itself is data, so no tag or variable
/// name is checked against stored rows here.
pub(crate) fn validate_segment_filter(filter: &Value) -> UnifiedResult<Value> {
    if filter.is_null() {
        return Ok(json!({}));
    }
    let Some(object) = filter.as_object() else {
        return Err(UnifiedError::bad_request(
            "segment_filter must be a JSON object".to_string(),
        ));
    };
    for key in ["tags_all", "tags_any"] {
        if let Some(tags) = object.get(key) {
            let Some(tags) = tags.as_array() else {
                return Err(UnifiedError::bad_request(format!(
                    "segment_filter.{key} must be an array of strings"
                )));
            };
            if tags.iter().any(|tag| !tag.is_string()) {
                return Err(UnifiedError::bad_request(format!(
                    "segment_filter.{key} must be an array of strings"
                )));
            }
        }
    }
    if let Some(variables) = object.get("variables")
        && !variables.is_object()
    {
        return Err(UnifiedError::bad_request(
            "segment_filter.variables must be an object".to_string(),
        ));
    }
    Ok(filter.clone())
}

/// Parse an RFC 3339 timestamp (a scheduled send time).
pub(crate) fn parse_send_at(raw: &str) -> UnifiedResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| {
            UnifiedError::bad_request(format!("send_at must be an RFC 3339 timestamp: {raw}"))
        })
}

/// Validate a provider endpoint URL: HTTPS with a host.
/// Log-only providers carry no endpoint.
pub(crate) fn validate_endpoint_url(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    match raw {
        Some(url) => {
            let url = require_text(url, "endpoint_url", MAX_URL_LEN)?;
            if !super::types::is_https_url(&url) {
                return Err(UnifiedError::bad_request(
                    "endpoint_url must be an HTTPS URL with a host".to_string(),
                ));
            }
            Ok(Some(url))
        }
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Tenant-scoped loaders (cross-tenant reads as 404, never 403)
// ---------------------------------------------------------------------------

fn tenant_match(row_tenant_id: &str, tenant_id: &str, id: Uuid, what: &str) -> UnifiedResult<()> {
    if row_tenant_id != tenant_id {
        return Err(UnifiedError::not_found(format!("{what} not found: {id}")));
    }
    Ok(())
}

pub(crate) fn load_provider(
    state: &SmsMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SmsProvider> {
    let id = parse_uuid(raw_id, "provider")?;
    let provider = state
        .providers
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Provider not found: {id}")))?;
    tenant_match(&provider.tenant_id, tenant_id, id, "Provider")?;
    Ok(provider)
}

/// Load a provider by id without a tenant filter: the
/// delivery / inbound webhooks authenticate with the
/// provider's shared secret, not with a tenant session, so
/// the provider row itself carries the tenant.
pub(crate) fn load_provider_for_webhook(
    state: &SmsMarketingState,
    raw_id: &str,
) -> UnifiedResult<SmsProvider> {
    let id = parse_uuid(raw_id, "provider")?;
    state
        .providers
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Provider not found: {id}")))
}

pub(crate) fn load_template(
    state: &SmsMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SmsTemplate> {
    let id = parse_uuid(raw_id, "template")?;
    let template = state
        .templates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Template not found: {id}")))?;
    tenant_match(&template.tenant_id, tenant_id, id, "Template")?;
    Ok(template)
}

pub(crate) fn load_contact_list(
    state: &SmsMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SmsContactList> {
    let id = parse_uuid(raw_id, "contact list")?;
    let list = state
        .contact_lists
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Contact list not found: {id}")))?;
    tenant_match(&list.tenant_id, tenant_id, id, "Contact list")?;
    Ok(list)
}

pub(crate) fn load_contact(
    state: &SmsMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SmsContact> {
    let id = parse_uuid(raw_id, "contact")?;
    let contact = state
        .contacts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Contact not found: {id}")))?;
    tenant_match(&contact.tenant_id, tenant_id, id, "Contact")?;
    Ok(contact)
}

pub(crate) fn load_campaign(
    state: &SmsMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SmsCampaign> {
    let id = parse_uuid(raw_id, "campaign")?;
    let campaign = state
        .campaigns
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Campaign not found: {id}")))?;
    tenant_match(&campaign.tenant_id, tenant_id, id, "Campaign")?;
    Ok(campaign)
}

pub(crate) fn load_message(
    state: &SmsMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SmsMessage> {
    let id = parse_uuid(raw_id, "message")?;
    let message = state
        .messages
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Message not found: {id}")))?;
    tenant_match(&message.tenant_id, tenant_id, id, "Message")?;
    Ok(message)
}

pub(crate) fn load_opt_out(
    state: &SmsMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SmsOptOut> {
    let id = parse_uuid(raw_id, "opt-out")?;
    let row = state
        .opt_outs
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Opt-out not found: {id}")))?;
    tenant_match(&row.tenant_id, tenant_id, id, "Opt-out")?;
    Ok(row)
}

pub(crate) fn load_keyword(
    state: &SmsMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SmsOptOutKeyword> {
    let id = parse_uuid(raw_id, "keyword")?;
    let row = state
        .opt_out_keywords
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Keyword not found: {id}")))?;
    tenant_match(&row.tenant_id, tenant_id, id, "Keyword")?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

/// Append one audit row for a change. `before` / `after` are
/// the serialized rows (or `None` for creates / deletes).
pub(crate) fn record_audit(
    state: &SmsMarketingState,
    tenant_id: &str,
    entity_type: &str,
    entity_id: &str,
    action: &str,
    actor: &str,
    before: Option<Value>,
    after: Option<Value>,
) {
    let row = AuditRecord {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        entity_type: entity_type.to_string(),
        entity_id: entity_id.to_string(),
        action: action.to_string(),
        actor: actor.to_string(),
        before,
        after,
        created_at: Utc::now(),
    };
    state.audit.insert(row.id, row);
}

// ---------------------------------------------------------------------------
// Recipient resolution
// ---------------------------------------------------------------------------

/// One resolved recipient: a normalized number plus the
/// template variables that apply to it (contact variables
/// merged over the campaign defaults).
#[derive(Debug, Clone)]
pub(crate) struct ResolvedRecipient {
    pub(crate) phone_number: String,
    pub(crate) variables: HashMap<String, String>,
}

/// Resolve a campaign's audience: contacts of its lists
/// that pass the segment filter, plus its explicit recipient
/// numbers. Numbers are deduplicated (a number on two lists
/// is one recipient); a contact's own variables win over the
/// campaign defaults, and an explicit number uses only the
/// campaign defaults. Lists that no longer exist are
/// skipped, never an error — deleting a list must not break
/// a campaign that referenced it.
pub(crate) fn resolve_recipients(
    state: &SmsMarketingState,
    campaign: &SmsCampaign,
) -> Vec<ResolvedRecipient> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut recipients: Vec<ResolvedRecipient> = Vec::new();
    for list_id in &campaign.contact_list_ids {
        let Ok(list_id) = Uuid::parse_str(list_id) else {
            continue;
        };
        for contact in state.contacts_of_list(&campaign.tenant_id, list_id) {
            if !super::types::segment_filter_matches(
                &contact.tags,
                &contact.variables,
                &campaign.segment_filter,
            ) {
                continue;
            }
            if !seen.insert(contact.phone_number.clone()) {
                continue;
            }
            let mut variables = campaign.variables.clone();
            variables.extend(contact.variables.clone());
            recipients.push(ResolvedRecipient {
                phone_number: contact.phone_number,
                variables,
            });
        }
    }
    for number in &campaign.recipient_numbers {
        if !seen.insert(number.clone()) {
            continue;
        }
        recipients.push(ResolvedRecipient {
            phone_number: number.clone(),
            variables: campaign.variables.clone(),
        });
    }
    recipients
}

/// Recompute a campaign's stats from its message rows and
/// store them back on the campaign row.
pub(crate) fn refresh_campaign_stats(state: &SmsMarketingState, campaign_id: Uuid) {
    let Some(mut campaign) = state.campaigns.get_mut(&campaign_id) else {
        return;
    };
    let mut stats = super::types::CampaignStats::default();
    for entry in state.messages.iter() {
        let message = entry.value();
        if message.campaign_id != campaign_id {
            continue;
        }
        match message.status {
            MessageStatus::Queued => stats.queued += 1,
            MessageStatus::Sent => stats.sent += 1,
            MessageStatus::Delivered => stats.delivered += 1,
            MessageStatus::Failed => stats.failed += 1,
            MessageStatus::Undelivered => stats.failed += 1,
            MessageStatus::Skipped => stats.skipped += 1,
        }
    }
    campaign.stats = stats;
}

/// True when a campaign may still be edited or deleted:
/// only drafts and cancelled campaigns.
pub(crate) fn campaign_is_editable(status: CampaignStatus) -> bool {
    matches!(status, CampaignStatus::Draft | CampaignStatus::Cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn pagination_params(page: Option<u32>, per_page: Option<u32>) -> PaginationParams {
        PaginationParams {
            page,
            per_page,
            sort: None,
            order: None,
            search: None,
            filters: HashMap::new(),
        }
    }

    #[test]
    fn paginate_bounds() {
        let items: Vec<i64> = (1..=5).collect();
        let first = paginate(items.clone(), &pagination_params(Some(1), Some(2)));
        assert_eq!(first.items, vec![1, 2]);
        assert_eq!(first.pagination.total, 5);
        let past_end = paginate(items.clone(), &pagination_params(Some(4), Some(2)));
        assert!(past_end.items.is_empty());
        assert_eq!(past_end.pagination.total, 5);
    }

    #[test]
    fn validation_rejects_bad_inputs() {
        assert!(require_text("", "name", 10).is_err());
        assert!(require_text("   ", "name", 10).is_err());
        assert!(require_text("a".repeat(11).as_str(), "name", 10).is_err());
        assert_eq!(require_text("  ok  ", "name", 10).unwrap(), "ok");
        assert!(validate_provider_kind("other_vendor").is_err());
        assert!(validate_provider_kind("http").is_ok());
        assert!(validate_auth_type("oauth").is_err());
        assert!(validate_keyword("stop!").is_err());
        assert_eq!(validate_keyword("stop").unwrap(), "STOP");
        assert!(validate_sender_id("has space").is_err());
        assert!(validate_sender_id("BrandSMS").is_ok());
        assert!(validate_sends_per_minute(-1).is_err());
        assert!(validate_sends_per_minute(100_001).is_err());
        assert!(validate_sends_per_minute(0).is_ok());
        assert!(validate_template_body("").is_err());
        assert!(validate_template_body("Hello").is_ok());
        assert!(validate_recipient_numbers(&["0812".to_string()]).is_err());
        assert!(validate_recipient_numbers(&["+62812".to_string(), "+62812".to_string()]).is_err());
        assert!(validate_list_ids(&["not-a-uuid".to_string()]).is_err());
        assert!(validate_segment_filter(&json!({"tags_all": "vip"})).is_err());
        assert!(validate_segment_filter(&json!({"tags_all": ["vip"]})).is_ok());
        assert!(validate_segment_filter(&json!(null)).is_ok());
        assert!(parse_send_at("2026-12-01T10:00:00Z").is_ok());
        assert!(parse_send_at("tomorrow").is_err());
        assert!(validate_endpoint_url(&Some("http://x.example".to_string())).is_err());
        assert!(validate_endpoint_url(&Some("https://x.example".to_string())).is_ok());
        assert!(validate_endpoint_url(&None).is_ok());
    }

    #[test]
    fn recipient_resolution_dedupes_and_filters() {
        let state = SmsMarketingState::default();
        let tenant_id = "tenant-a".to_string();
        let now = Utc::now();
        let list_id = Uuid::new_v4();
        state.contact_lists.insert(
            list_id,
            SmsContactList {
                id: list_id,
                tenant_id: tenant_id.clone(),
                name: "Buyers".to_string(),
                description: None,
                created_by: "user-1".to_string(),
                created_at: now,
                updated_at: now,
            },
        );
        let mut vars = HashMap::new();
        vars.insert("city".to_string(), "Padang".to_string());
        state.contacts.insert(
            Uuid::new_v4(),
            SmsContact {
                id: Uuid::new_v4(),
                tenant_id: tenant_id.clone(),
                list_id,
                phone_number: "+6281111111111".to_string(),
                display_name: None,
                tags: vec!["vip".to_string()],
                variables: vars,
                created_by: "user-1".to_string(),
                created_at: now,
                updated_at: now,
            },
        );
        state.contacts.insert(
            Uuid::new_v4(),
            SmsContact {
                id: Uuid::new_v4(),
                tenant_id: tenant_id.clone(),
                list_id,
                phone_number: "+6281222222222".to_string(),
                display_name: None,
                tags: vec!["new".to_string()],
                variables: HashMap::new(),
                created_by: "user-1".to_string(),
                created_at: now,
                updated_at: now,
            },
        );
        let campaign = SmsCampaign {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name: "Promo".to_string(),
            template_id: Uuid::new_v4(),
            provider_id: Uuid::new_v4(),
            contact_list_ids: vec![list_id.to_string()],
            // Same number as the first contact: deduped.
            recipient_numbers: vec!["+6281111111111".to_string()],
            segment_filter: json!({"tags_any": ["vip"]}),
            variables: HashMap::new(),
            status: CampaignStatus::Draft,
            send_at: None,
            sent_at: None,
            stats: super::types::CampaignStats::default(),
            created_by: "user-1".to_string(),
            created_at: now,
            updated_at: now,
        };
        let recipients = resolve_recipients(&state, &campaign);
        assert_eq!(recipients.len(), 1);
        assert_eq!(recipients[0].phone_number, "+6281111111111");
        assert_eq!(
            recipients[0].variables.get("city").map(String::as_str),
            Some("Padang")
        );
        assert!(campaign_is_editable(CampaignStatus::Draft));
        assert!(!campaign_is_editable(CampaignStatus::Sent));
    }

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let mut caller = unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new());
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_company() {
        let req = tenant_request("company-1", Some("company-1"));
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert_eq!(
            caller_company(&tenant, &req).unwrap().as_deref(),
            Some("company-1")
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("company-1", Some("company-2"));
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn require_caller_company_refuses_writes_without_a_resolved_company() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert!(require_caller_company(&tenant, &req).is_err());
        let req = tenant_request("company-1", None);
        let tenant = require_tenant(&req).expect("tenant resolves");
        assert_eq!(require_caller_company(&tenant, &req).unwrap(), "company-1");
    }
}
