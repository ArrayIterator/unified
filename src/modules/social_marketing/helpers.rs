//! Caller, permission, loading, validation, and audit
//! helpers.

use super::state::SocialMarketingState;
use super::types::{
    AuditRecord, AuthType, CampaignStatus, MAX_ACCOUNTS_PER_POST, MAX_HANDLE_LEN, MAX_HASHTAG_LEN,
    MAX_HASHTAGS_PER_POST, MAX_MEDIA_PER_POST, MAX_NAME_LEN, MAX_REFERENCE_LEN, MAX_SLUG_LEN,
    MAX_URL_LEN, PERM_SOCIAL_MANAGE, PERM_SOCIAL_VIEW, PostStatus, ProviderKind, SocialAccount,
    SocialCampaign, SocialMetric, SocialNetwork, SocialPost,
};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;
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
    require_permission(ctx, role, PERM_SOCIAL_VIEW)
}

pub(crate) fn require_manage(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    require_permission(ctx, role, PERM_SOCIAL_MANAGE)
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
        (None, verified) => Ok(verified.clone()),
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

/// Validate a network slug: lowercase letters, digits, and
/// hyphens, never starting or ending with a hyphen.
pub(crate) fn validate_slug(raw: &str) -> UnifiedResult<String> {
    let slug = require_text(raw, "slug", MAX_SLUG_LEN)?;
    if !super::types::is_valid_slug(&slug) {
        return Err(UnifiedError::bad_request(
            "slug must be lowercase letters, digits, or hyphens, and must not start or end with a hyphen".to_string(),
        ));
    }
    Ok(slug)
}

/// Validate a network API base URL: HTTPS with a host.
/// Log-only networks carry no API base.
pub(crate) fn validate_api_base_url(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    match raw {
        Some(url) => {
            let url = require_text(url, "api_base_url", MAX_URL_LEN)?;
            if !super::types::is_https_url(&url) {
                return Err(UnifiedError::bad_request(
                    "api_base_url must be an HTTPS URL with a host".to_string(),
                ));
            }
            Ok(Some(url))
        }
        None => Ok(None),
    }
}

/// Validate a publish path: it must start with `/` and
/// hold no whitespace.
pub(crate) fn validate_publish_path(raw: &str) -> UnifiedResult<String> {
    let path = require_text(raw, "publish_path", MAX_URL_LEN)?;
    if !path.starts_with('/') || path.bytes().any(|b| (b as char).is_whitespace()) {
        return Err(UnifiedError::bad_request(
            "publish_path must start with / and contain no whitespace".to_string(),
        ));
    }
    Ok(path)
}

/// Validate a social handle: 1-100 characters, no
/// whitespace. The leading `@` is optional and stripped.
pub(crate) fn validate_handle(raw: &str) -> UnifiedResult<String> {
    let handle = require_text(raw, "handle", MAX_HANDLE_LEN)?;
    let handle = handle.trim_start_matches('@');
    if handle.is_empty() {
        return Err(UnifiedError::bad_request(
            "handle must not be empty".to_string(),
        ));
    }
    if handle.bytes().any(|b| (b as char).is_whitespace()) {
        return Err(UnifiedError::bad_request(
            "handle must not contain whitespace".to_string(),
        ));
    }
    Ok(handle.to_string())
}

/// Validate a posts-per-minute value: 0-100000. Zero means
/// the limit blocks every publish (a deliberate off switch).
pub(crate) fn validate_posts_per_minute(value: i64) -> UnifiedResult<i64> {
    if !(0..=100_000).contains(&value) {
        return Err(UnifiedError::bad_request(
            "posts_per_minute must be between 0 and 100000".to_string(),
        ));
    }
    Ok(value)
}

/// Validate a per-network text limit: 1-100000 characters.
pub(crate) fn validate_max_text_length(value: i64) -> UnifiedResult<i64> {
    if !(1..=super::types::MAX_POST_TEXT_LEN as i64).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "max_text_length must be between 1 and {}",
            super::types::MAX_POST_TEXT_LEN
        )));
    }
    Ok(value)
}

/// Validate a per-network media limit: 0-10 items.
pub(crate) fn validate_max_media_items(value: i64) -> UnifiedResult<i64> {
    if !(0..=MAX_MEDIA_PER_POST as i64).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "max_media_items must be between 0 and {MAX_MEDIA_PER_POST}"
        )));
    }
    Ok(value)
}

/// Validate a post-type list: each entry is a short slug
/// (letters, digits, underscores, hyphens), no repeats, at
/// most 50 entries. The entries are data — no type name is
/// checked against a hard-coded list.
pub(crate) fn validate_post_types(types: &[String]) -> UnifiedResult<Vec<String>> {
    if types.len() > 50 {
        return Err(UnifiedError::bad_request(
            "At most 50 post types per network".to_string(),
        ));
    }
    let mut seen = HashSet::new();
    let mut clean = Vec::new();
    for post_type in types {
        let post_type = require_text(post_type, "post_type", MAX_NAME_LEN)?;
        if !post_type
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(UnifiedError::bad_request(format!(
                "post_type \"{post_type}\" must contain letters, digits, underscores, or hyphens only"
            )));
        }
        if !seen.insert(post_type.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "post_types must not repeat \"{post_type}\""
            )));
        }
        clean.push(post_type);
    }
    Ok(clean)
}

/// Validate a post text against the network's text limit.
pub(crate) fn validate_post_text(raw: &str, max_len: i64) -> UnifiedResult<String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(UnifiedError::bad_request(
            "text must not be empty".to_string(),
        ));
    }
    if !super::types::text_fits_limit(text, max_len) {
        return Err(UnifiedError::bad_request(format!(
            "text must be at most {max_len} characters for this network"
        )));
    }
    Ok(text.to_string())
}

/// Validate a hashtag list: at most the per-post bound,
/// each tag normalized (a leading `#` is stripped, case is
/// folded), no invalid tag.
pub(crate) fn validate_hashtags(tags: &[String]) -> UnifiedResult<Vec<String>> {
    if tags.len() > MAX_HASHTAGS_PER_POST {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_HASHTAGS_PER_POST} hashtags per post"
        )));
    }
    super::types::normalize_hashtags(tags).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "Each hashtag must be 1-{MAX_HASHTAG_LEN} characters of letters, digits, or underscores"
        ))
    })
}

/// Validate media references: at most the network's media
/// bound, each a UUID string (a document / storage id).
pub(crate) fn validate_media_ids(ids: &[String], max_items: i64) -> UnifiedResult<Vec<String>> {
    if ids.len() > MAX_MEDIA_PER_POST {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_MEDIA_PER_POST} media items per post"
        )));
    }
    if !super::types::media_ids_valid(ids, max_items) {
        return Err(UnifiedError::bad_request(
            "media_ids must be document / storage ids (UUIDs) within the network's media limit"
                .to_string(),
        ));
    }
    let mut seen = HashSet::new();
    let mut clean = Vec::new();
    for id in ids {
        if !seen.insert(id.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "media_ids must not repeat \"{id}\""
            )));
        }
        clean.push(id.clone());
    }
    Ok(clean)
}

/// Validate target account ids: at most the per-post bound,
/// each a valid UUID string, no repeats.
pub(crate) fn validate_account_ids(ids: &[String]) -> UnifiedResult<Vec<String>> {
    if ids.len() > MAX_ACCOUNTS_PER_POST {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_ACCOUNTS_PER_POST} accounts per post"
        )));
    }
    if ids.is_empty() {
        return Err(UnifiedError::bad_request(
            "A post needs at least one target account".to_string(),
        ));
    }
    let mut seen = HashSet::new();
    let mut clean = Vec::new();
    for id in ids {
        let id = require_text(id, "account_id", MAX_REFERENCE_LEN)?;
        parse_uuid(&id, "account")?;
        if !seen.insert(id.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "account_ids must not repeat \"{id}\""
            )));
        }
        clean.push(id);
    }
    Ok(clean)
}

/// Validate an optional link: HTTPS with a host.
pub(crate) fn validate_link(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    match raw {
        Some(url) => {
            let url = require_text(url, "link", MAX_URL_LEN)?;
            if !super::types::is_https_url(&url) {
                return Err(UnifiedError::bad_request(
                    "link must be an HTTPS URL with a host".to_string(),
                ));
            }
            Ok(Some(url))
        }
        None => Ok(None),
    }
}

/// Validate a metric name: a lowercase slug.
pub(crate) fn validate_metric_name(raw: &str) -> UnifiedResult<String> {
    let metric =
        require_text(raw, "metric", super::types::MAX_METRIC_NAME_LEN)?.to_ascii_lowercase();
    if !super::types::is_valid_metric_name(&metric) {
        return Err(UnifiedError::bad_request(
            "metric must be a lowercase slug of letters, digits, or underscores".to_string(),
        ));
    }
    Ok(metric)
}

/// Validate a metric value: a count can never be negative.
pub(crate) fn validate_metric_value(value: i64) -> UnifiedResult<i64> {
    if value < 0 {
        return Err(UnifiedError::bad_request(
            "metric value must not be negative".to_string(),
        ));
    }
    Ok(value)
}

/// Validate a goal target: a non-negative integer count.
pub(crate) fn validate_goal_target(value: i64) -> UnifiedResult<i64> {
    if value < 0 {
        return Err(UnifiedError::bad_request(
            "goal_target must not be negative".to_string(),
        ));
    }
    Ok(value)
}

/// Parse an RFC 3339 timestamp.
pub(crate) fn parse_timestamp(raw: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| {
            UnifiedError::bad_request(format!("{field} must be an RFC 3339 timestamp: {raw}"))
        })
}

/// Parse an optional RFC 3339 timestamp (a token expiry or
/// a campaign bound).
pub(crate) fn parse_optional_timestamp(
    raw: &Option<String>,
    field: &str,
) -> UnifiedResult<Option<DateTime<Utc>>> {
    match raw {
        Some(text) => Ok(Some(parse_timestamp(text, field)?)),
        None => Ok(None),
    }
}

/// Validate a campaign date range: when both bounds are
/// present, the start must not be after the end.
pub(crate) fn validate_date_range(
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
) -> UnifiedResult<()> {
    if let (Some(starts_at), Some(ends_at)) = (starts_at, ends_at)
        && starts_at > ends_at
    {
        return Err(UnifiedError::bad_request(
            "starts_at must not be after ends_at".to_string(),
        ));
    }
    Ok(())
}

/// Validate a campaign status string.
pub(crate) fn validate_campaign_status(raw: &str) -> UnifiedResult<CampaignStatus> {
    CampaignStatus::from_str(raw.trim()).ok_or_else(|| {
        UnifiedError::bad_request(
            "status must be draft, active, paused, completed, or cancelled".to_string(),
        )
    })
}

/// Parse an optional date filter (`from` / `to` query
/// filters): an invalid value fails the list instead of
/// silently widening it.
pub(crate) fn parse_date_filter(raw: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    parse_timestamp(raw, field)
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

pub(crate) fn load_network(
    state: &SocialMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SocialNetwork> {
    let id = parse_uuid(raw_id, "network")?;
    let network = state
        .networks
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Network not found: {id}")))?;
    tenant_match(&network.tenant_id, tenant_id, id, "Network")?;
    Ok(network)
}

pub(crate) fn load_account(
    state: &SocialMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SocialAccount> {
    let id = parse_uuid(raw_id, "account")?;
    let account = state
        .accounts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Account not found: {id}")))?;
    tenant_match(&account.tenant_id, tenant_id, id, "Account")?;
    Ok(account)
}

/// Load an account by id without a tenant filter: the
/// metrics-ingest webhook authenticates with the account's
/// shared secret, not with a tenant session, so the account
/// row itself carries the tenant.
pub(crate) fn load_account_for_webhook(
    state: &SocialMarketingState,
    raw_id: &str,
) -> UnifiedResult<SocialAccount> {
    let id = parse_uuid(raw_id, "account")?;
    state
        .accounts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Account not found: {id}")))
}

pub(crate) fn load_post(
    state: &SocialMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SocialPost> {
    let id = parse_uuid(raw_id, "post")?;
    let post = state
        .posts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Post not found: {id}")))?;
    tenant_match(&post.tenant_id, tenant_id, id, "Post")?;
    Ok(post)
}

pub(crate) fn load_campaign(
    state: &SocialMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SocialCampaign> {
    let id = parse_uuid(raw_id, "campaign")?;
    let campaign = state
        .campaigns
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Campaign not found: {id}")))?;
    tenant_match(&campaign.tenant_id, tenant_id, id, "Campaign")?;
    Ok(campaign)
}

pub(crate) fn load_metric(
    state: &SocialMarketingState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SocialMetric> {
    let id = parse_uuid(raw_id, "metric")?;
    let metric = state
        .metrics
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Metric not found: {id}")))?;
    tenant_match(&metric.tenant_id, tenant_id, id, "Metric")?;
    Ok(metric)
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

/// Append one audit row for a change. `before` / `after` are
/// the serialized rows (or `None` for creates / deletes).
pub(crate) fn record_audit(
    state: &SocialMarketingState,
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

/// True when a post may still be edited or deleted: only
/// drafts and cancelled posts.
pub(crate) fn post_is_editable(status: PostStatus) -> bool {
    matches!(status, PostStatus::Draft | PostStatus::Cancelled)
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
        assert!(validate_slug("My-Net").is_err());
        assert!(validate_slug("-net").is_err());
        assert_eq!(validate_slug("my-net").unwrap(), "my-net");
        assert!(validate_api_base_url(&Some("http://x.example".to_string())).is_err());
        assert!(validate_api_base_url(&Some("https://x.example".to_string())).is_ok());
        assert!(validate_publish_path("no-slash").is_err());
        assert!(validate_publish_path("/v1/publish").is_ok());
        assert_eq!(validate_handle("@brand").unwrap(), "brand");
        assert!(validate_handle("has space").is_err());
        assert!(validate_posts_per_minute(-1).is_err());
        assert!(validate_posts_per_minute(100_001).is_err());
        assert!(validate_posts_per_minute(0).is_ok());
        assert!(validate_max_text_length(0).is_err());
        assert!(validate_max_text_length(500).is_ok());
        assert!(validate_post_types(&["bad type".to_string()]).is_err());
        assert!(
            validate_post_types(&["announcement".to_string(), "announcement".to_string()]).is_err()
        );
        assert!(validate_post_text("", 500).is_err());
        assert!(validate_post_text("Hello", 4).is_err());
        assert!(validate_post_text("Hello", 500).is_ok());
        assert!(validate_hashtags(&["bad tag".to_string()]).is_err());
        assert_eq!(
            validate_hashtags(&["#Launch".to_string()]).unwrap(),
            vec!["launch".to_string()]
        );
        assert!(validate_media_ids(&["not-a-uuid".to_string()], 4).is_err());
        assert!(validate_account_ids(&[]).is_err());
        assert!(validate_account_ids(&["not-a-uuid".to_string()]).is_err());
        assert!(validate_link(&Some("http://x.example".to_string())).is_err());
        assert!(validate_metric_name("With-Dash").is_err());
        assert_eq!(validate_metric_name("Impressions").unwrap(), "impressions");
        assert!(validate_metric_value(-1).is_err());
        assert!(validate_goal_target(-1).is_err());
        assert!(parse_timestamp("2026-12-01T10:00:00Z", "send_at").is_ok());
        assert!(parse_timestamp("tomorrow", "send_at").is_err());
        assert!(
            validate_date_range(
                Some(Utc::now() + chrono::Duration::days(2)),
                Some(Utc::now() + chrono::Duration::days(1))
            )
            .is_err()
        );
        assert!(validate_campaign_status("bogus").is_err());
        assert!(post_is_editable(PostStatus::Draft));
        assert!(!post_is_editable(PostStatus::Published));
        assert!(campaign_is_editable(CampaignStatus::Draft));
        assert!(!campaign_is_editable(CampaignStatus::Active));
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
