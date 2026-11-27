//! HTTP handlers for social posts: compose, schedule,
//! publish now, retry, cancel, and read. A post targets
//! one network and a list of that network's connected
//! accounts; publishing dispatches once per target account
//! through the network's provider adapter and stores one
//! publish result per account on the post row. Scheduling
//! only records `send_at` and emits `social.post.scheduled`
//! for a scheduler to pick up - this module runs no
//! scheduled-post runner.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::adapter::{SocialPublishRequest, dispatch};
use super::crypto::{decrypt_credential, encryption_secret};
use super::helpers::{
    caller, caller_company, created, err, load_account, load_campaign, load_network, load_post, ok,
    paginate, parse_date_filter, parse_optional_timestamp, parse_timestamp, post_is_editable,
    record_audit, require_caller_company, require_manage, require_tenant, require_text,
    require_view, validate_account_ids, validate_hashtags, validate_link, validate_media_ids,
    validate_post_text,
};
use super::state::SocialMarketingState;
use super::types::{
    CreatePostRequest, EVENT_POST_FAILED, EVENT_POST_PUBLISHED, EVENT_POST_SCHEDULED, PostStatus,
    PublishResult, SchedulePostRequest, SocialPost, UpdatePostRequest, post_can_transition,
    publishes_allowed, remaining_publishes,
};

/// List posts (paginated, filter by network, account,
/// status, campaign, post type; search by text/hashtags;
/// filter by created date range with `from` / `to`).
pub(crate) async fn list_posts(
    state: web::Data<SocialMarketingState>,
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
        let network_filter = params.filters.get("network_id").cloned();
        let account_filter = params.filters.get("account_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let campaign_filter = params.filters.get("campaign_id").cloned();
        let type_filter = params.filters.get("post_type").cloned();
        let from_filter = match params.filters.get("from") {
            Some(raw) => Some(parse_date_filter(raw, "from")?),
            None => None,
        };
        let to_filter = match params.filters.get("to") {
            Some(raw) => Some(parse_date_filter(raw, "to")?),
            None => None,
        };
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.posts_of(&tenant_id);
        items.retain(|post| {
            if let Some(network_id) = &network_filter
                && post.network_id.to_string() != *network_id
            {
                return false;
            }
            if let Some(account_id) = &account_filter
                && !post.account_ids.contains(account_id)
            {
                return false;
            }
            if let Some(status) = &status_filter
                && post.status.as_str() != status
            {
                return false;
            }
            if let Some(campaign_id) = &campaign_filter {
                match &post.campaign_id {
                    Some(id) if id.to_string() == *campaign_id => {}
                    _ => return false,
                }
            }
            if let Some(post_type) = &type_filter
                && post.post_type != *post_type
            {
                return false;
            }
            if let Some(from) = from_filter
                && post.created_at < from
            {
                return false;
            }
            if let Some(to) = to_filter
                && post.created_at > to
            {
                return false;
            }
            if let Some(needle) = &search
                && !post.text.to_lowercase().contains(needle)
                && !post.hashtags.iter().any(|tag| tag.contains(needle))
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

/// Create a post (manager). The post starts as a draft; a
/// `send_at` in the future starts it as scheduled instead.
/// Every target account must belong to the post's network,
/// and the post type must be one of the network row's post
/// types. Media are references by document / storage id
/// only - this module never stores or fetches the bytes.
pub(crate) async fn create_post(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreatePostRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let network = load_network(&state, &tenant_id, &input.network_id)?;
        let campaign_id = match &input.campaign_id {
            Some(raw) => Some(load_campaign(&state, &tenant_id, raw)?.id),
            None => None,
        };
        let account_ids = validate_account_ids(&input.account_ids)?;
        for account_id in &account_ids {
            let account = load_account(&state, &tenant_id, account_id)?;
            if account.network_id != network.id {
                return Err(UnifiedError::bad_request(format!(
                    "Account \"{}\" does not belong to this network",
                    account.handle
                )));
            }
        }
        let post_type = require_text(&input.post_type, "post_type", 200)?;
        if !super::types::post_type_allowed(&post_type, &network.post_types) {
            return Err(UnifiedError::bad_request(format!(
                "post_type \"{post_type}\" is not one of this network's post types"
            )));
        }
        let text = validate_post_text(&input.text, network.max_text_length)?;
        let media_ids = validate_media_ids(&input.media_ids, network.max_media_items)?;
        let link = validate_link(&input.link)?;
        let hashtags = validate_hashtags(&input.hashtags)?;
        let send_at = parse_optional_timestamp(&input.send_at, "send_at")?;
        let now = Utc::now();
        let status = match send_at {
            Some(when) => {
                if !super::types::is_future_schedule(when, now) {
                    return Err(UnifiedError::bad_request(
                        "send_at must be in the future; publish now through the publish-now endpoint instead".to_string(),
                    ));
                }
                PostStatus::Scheduled
            }
            None => PostStatus::Draft,
        };
        let post = SocialPost {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            campaign_id,
            network_id: network.id,
            account_ids,
            post_type,
            text,
            media_ids,
            link,
            hashtags,
            status,
            send_at,
            published_at: None,
            results: Vec::new(),
            error: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        let view = serde_json::to_value(&post).unwrap_or(Value::Null);
        state.posts.insert(post.id, post.clone());
        record_audit(
            &state,
            &tenant_id,
            "post",
            &post.id.to_string(),
            "create",
            &caller.actor,
            None,
            Some(view.clone()),
        );
        if status == PostStatus::Scheduled {
            state.emit(
                EVENT_POST_SCHEDULED,
                json!({
                    "tenant_id": tenant_id,
                    "post_id": post.id.to_string(),
                    "network_id": post.network_id.to_string(),
                    "send_at": post.send_at.map(|when| when.to_rfc3339()),
                    "created_by": caller.actor,
                    "note": "No runner in this module: a scheduler picks this post up at send_at.",
                }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one post with its per-account publish results and
/// its summed metrics.
pub(crate) async fn get_post(
    state: web::Data<SocialMarketingState>,
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
        let post = load_post(&state, &tenant_id, &path.into_inner())?;
        let totals = state.rollup_of_post(&tenant_id, post.id);
        let mut view = serde_json::to_value(&post).unwrap_or(Value::Null);
        if let Some(object) = view.as_object_mut() {
            object.insert(
                "metrics".to_string(),
                serde_json::to_value(&totals).unwrap_or(Value::Null),
            );
            object.insert(
                "engagement_bps".to_string(),
                Value::from(super::types::engagement_bps(&totals)),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a post (manager). Only drafts and cancelled posts
/// are editable; a scheduled post must be cancelled first.
pub(crate) async fn update_post(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdatePostRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut post = load_post(&state, &tenant_id, &path.into_inner())?;
        if !post_is_editable(post.status) {
            return Err(UnifiedError::conflict(format!(
                "Post in status {} cannot be edited",
                post.status.as_str()
            )));
        }
        let network = load_network(&state, &tenant_id, &post.network_id.to_string())?;
        let before = serde_json::to_value(&post).unwrap_or(Value::Null);
        if let Some(campaign_id) = &input.campaign_id {
            post.campaign_id = match campaign_id {
                Some(raw) => Some(load_campaign(&state, &tenant_id, raw)?.id),
                None => None,
            };
        }
        if let Some(account_ids) = &input.account_ids {
            let account_ids = validate_account_ids(account_ids)?;
            for account_id in &account_ids {
                let account = load_account(&state, &tenant_id, account_id)?;
                if account.network_id != network.id {
                    return Err(UnifiedError::bad_request(format!(
                        "Account \"{}\" does not belong to this network",
                        account.handle
                    )));
                }
            }
            post.account_ids = account_ids;
        }
        if let Some(post_type) = &input.post_type {
            let post_type = require_text(post_type, "post_type", 200)?;
            if !super::types::post_type_allowed(&post_type, &network.post_types) {
                return Err(UnifiedError::bad_request(format!(
                    "post_type \"{post_type}\" is not one of this network's post types"
                )));
            }
            post.post_type = post_type;
        }
        if let Some(text) = &input.text {
            post.text = validate_post_text(text, network.max_text_length)?;
        }
        if let Some(media_ids) = &input.media_ids {
            post.media_ids = validate_media_ids(media_ids, network.max_media_items)?;
        }
        if let Some(link) = &input.link {
            post.link = match link {
                Some(url) => validate_link(&Some(url.clone()))?,
                None => None,
            };
        }
        if let Some(hashtags) = &input.hashtags {
            post.hashtags = validate_hashtags(hashtags)?;
        }
        if let Some(send_at) = &input.send_at {
            post.send_at = match send_at {
                Some(raw) => {
                    let when = parse_timestamp(raw, "send_at")?;
                    if !super::types::is_future_schedule(when, Utc::now()) {
                        return Err(UnifiedError::bad_request(
                            "send_at must be in the future".to_string(),
                        ));
                    }
                    Some(when)
                }
                None => None,
            };
        }
        post.updated_at = Utc::now();
        let view = serde_json::to_value(&post).unwrap_or(Value::Null);
        state.posts.insert(post.id, post.clone());
        record_audit(
            &state,
            &tenant_id,
            "post",
            &post.id.to_string(),
            "update",
            &caller.actor,
            Some(before),
            Some(view.clone()),
        );
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a post (manager). Only drafts and cancelled posts
/// can be deleted; metric rows that name the post are kept
/// as analytics history.
pub(crate) async fn delete_post(
    state: web::Data<SocialMarketingState>,
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
        let post = load_post(&state, &tenant_id, &path.into_inner())?;
        if !post_is_editable(post.status) {
            return Err(UnifiedError::conflict(format!(
                "Post in status {} cannot be deleted",
                post.status.as_str()
            )));
        }
        state.posts.remove(&post.id);
        record_audit(
            &state,
            &tenant_id,
            "post",
            &post.id.to_string(),
            "delete",
            &caller.actor,
            Some(serde_json::to_value(&post).unwrap_or(Value::Null)),
            None,
        );
        Ok(json!({ "deleted": true, "post_id": post.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Schedule a post for a future publish time (manager).
/// Records `send_at`, moves the post to `scheduled`, and
/// emits `social.post.scheduled` for a scheduler to pick
/// up. This module runs no runner: nothing publishes the
/// post at `send_at` on its own.
pub(crate) async fn schedule_post(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<SchedulePostRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut post = load_post(&state, &tenant_id, &path.into_inner())?;
        if !post_can_transition(post.status, PostStatus::Scheduled) {
            return Err(UnifiedError::conflict(format!(
                "Post in status {} cannot be scheduled",
                post.status.as_str()
            )));
        }
        let send_at = parse_timestamp(&input.send_at, "send_at")?;
        if !super::types::is_future_schedule(send_at, Utc::now()) {
            return Err(UnifiedError::bad_request(
                "send_at must be in the future".to_string(),
            ));
        }
        let before = serde_json::to_value(&post).unwrap_or(Value::Null);
        post.status = PostStatus::Scheduled;
        post.send_at = Some(send_at);
        post.updated_at = Utc::now();
        let view = serde_json::to_value(&post).unwrap_or(Value::Null);
        state.posts.insert(post.id, post.clone());
        record_audit(
            &state,
            &tenant_id,
            "post",
            &post.id.to_string(),
            "schedule",
            &caller.actor,
            Some(before),
            Some(view.clone()),
        );
        state.emit(
            EVENT_POST_SCHEDULED,
            json!({
                "tenant_id": tenant_id,
                "post_id": post.id.to_string(),
                "network_id": post.network_id.to_string(),
                "send_at": send_at.to_rfc3339(),
                "scheduled_by": caller.actor,
                "note": "No runner in this module: a scheduler picks this post up at send_at.",
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Cancel a post (manager): a draft, scheduled, or failed
/// post becomes cancelled. Published posts cannot be
/// cancelled - unpublishing is the network's own action,
/// not this module's.
pub(crate) async fn cancel_post(
    state: web::Data<SocialMarketingState>,
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
        let mut post = load_post(&state, &tenant_id, &path.into_inner())?;
        if !post_can_transition(post.status, PostStatus::Cancelled) {
            return Err(UnifiedError::conflict(format!(
                "Post in status {} cannot be cancelled",
                post.status.as_str()
            )));
        }
        let before = serde_json::to_value(&post).unwrap_or(Value::Null);
        post.status = PostStatus::Cancelled;
        post.updated_at = Utc::now();
        let view = serde_json::to_value(&post).unwrap_or(Value::Null);
        state.posts.insert(post.id, post.clone());
        record_audit(
            &state,
            &tenant_id,
            "post",
            &post.id.to_string(),
            "cancel",
            &caller.actor,
            Some(before),
            Some(view.clone()),
        );
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Publish a post now (manager): dispatch once per target
/// account through the network's provider adapter and store
/// one result per account. The post must be a draft, a
/// scheduled post, or a failed post (retry). Every target
/// account must be connected, enabled, and hold an
/// unexpired token; the network must be enabled; the
/// per-network per-minute limit is enforced before any
/// publish, and a full window refuses the publish instead
/// of silently dropping accounts.
pub(crate) async fn publish_post_now(
    state: web::Data<SocialMarketingState>,
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
        let post = load_post(&state, &tenant_id, &path.into_inner())?;
        publish_post(&state, &ctx, &tenant_id, &caller.actor, post).await
    })()
    .await;
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Retry a failed post (manager): the same publish pass as
/// publish-now, restricted to failed posts.
pub(crate) async fn retry_post(
    state: web::Data<SocialMarketingState>,
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
        let post = load_post(&state, &tenant_id, &path.into_inner())?;
        if post.status != PostStatus::Failed {
            return Err(UnifiedError::conflict(format!(
                "Only a failed post can be retried; this post is {}",
                post.status.as_str()
            )));
        }
        publish_post(&state, &ctx, &tenant_id, &caller.actor, post).await
    })()
    .await;
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// The shared publish pass for publish-now and retry.
/// Loads the network, checks every target account, enforces
/// the per-network rate limit, decrypts each account's
/// credential once, dispatches per account, and stores the
/// results on the post row. All accounts accepted means
/// `published`; any failure means `failed` (the per-account
/// results name the accounts that failed, and the post can
/// be retried).
async fn publish_post(
    state: &SocialMarketingState,
    ctx: &Arc<AppContext>,
    tenant_id: &str,
    actor: &str,
    mut post: SocialPost,
) -> UnifiedResult<Value> {
    if !post_can_transition(post.status, PostStatus::Publishing) {
        return Err(UnifiedError::conflict(format!(
            "Post in status {} cannot be published",
            post.status.as_str()
        )));
    }
    let network = load_network(state, tenant_id, &post.network_id.to_string())?;
    if !network.enabled {
        return Err(UnifiedError::bad_request(format!(
            "Network \"{}\" is disabled",
            network.name
        )));
    }
    // Rate-limit budget for this pass: the remaining
    // publishes for this network in the last minute. One
    // publish per target account is charged.
    let published_in_window = state.publishes_in_last_minute(tenant_id, network.id);
    if !publishes_allowed(published_in_window, network.posts_per_minute) {
        return Err(UnifiedError::new(
            format!(
                "Network \"{}\" rate limit reached: {} publishes in the last minute (limit {})",
                network.name, published_in_window, network.posts_per_minute
            ),
            429,
        ));
    }
    let budget = remaining_publishes(published_in_window, network.posts_per_minute);
    if (post.account_ids.len() as i64) > budget {
        return Err(UnifiedError::new(
            format!(
                "Network \"{}\" has {budget} publishes left this minute but this post targets {} accounts",
                network.name,
                post.account_ids.len()
            ),
            429,
        ));
    }
    // Every target account must be usable before any
    // publish happens: connected, enabled, unexpired token,
    // and a decryptable credential when the network's auth
    // needs one.
    let now = Utc::now();
    let mut accounts = Vec::new();
    for account_id in &post.account_ids {
        let account = load_account(state, tenant_id, account_id)?;
        if account.network_id != network.id {
            return Err(UnifiedError::bad_request(format!(
                "Account \"{}\" does not belong to this network",
                account.handle
            )));
        }
        if !account.enabled {
            return Err(UnifiedError::bad_request(format!(
                "Account \"{}\" is disabled",
                account.handle
            )));
        }
        if !account.connected {
            return Err(UnifiedError::bad_request(format!(
                "Account \"{}\" is not connected",
                account.handle
            )));
        }
        if super::types::token_expired(account.token_expires_at, now) {
            return Err(UnifiedError::bad_request(format!(
                "Account \"{}\" token is expired; reconnect it first",
                account.handle
            )));
        }
        let credential: Option<String> = if network.auth_type.needs_credential() {
            let Some(stored) = account.credential_encrypted.as_deref() else {
                return Err(UnifiedError::bad_request(format!(
                    "Account \"{}\" needs a credential; none is stored",
                    account.handle
                )));
            };
            let secret = encryption_secret(ctx)?;
            Some(decrypt_credential(&secret, stored)?)
        } else {
            None
        };
        accounts.push((account, credential));
    }
    let before = serde_json::to_value(&post).unwrap_or(Value::Null);
    post.status = PostStatus::Publishing;
    post.published_at = Some(now);
    post.updated_at = now;
    state.posts.insert(post.id, post.clone());
    let mut results: Vec<PublishResult> = Vec::new();
    let mut accepted: i64 = 0;
    let mut failed: i64 = 0;
    for (account, credential) in &accounts {
        let outcome = dispatch(
            &network,
            account,
            credential.as_deref(),
            &SocialPublishRequest {
                account_handle: account.handle.clone(),
                post_type: post.post_type.clone(),
                text: post.text.clone(),
                media_ids: post.media_ids.clone(),
                link: post.link.clone(),
                hashtags: post.hashtags.clone(),
            },
        )
        .await;
        if outcome.accepted {
            accepted += 1;
        } else {
            failed += 1;
        }
        results.push(PublishResult {
            account_id: account.id,
            provider_post_id: outcome.provider_post_id,
            status: if outcome.accepted {
                "published".to_string()
            } else {
                "failed".to_string()
            },
            error: outcome.error,
            published_at: if outcome.accepted {
                Some(Utc::now())
            } else {
                None
            },
        });
    }
    post.results = results;
    post.status = if failed == 0 {
        PostStatus::Published
    } else {
        PostStatus::Failed
    };
    if failed > 0 {
        let first_error = post
            .results
            .iter()
            .find_map(|result| result.error.clone())
            .unwrap_or_else(|| "Publish failed".to_string());
        post.error = Some(first_error);
    } else {
        post.error = None;
    }
    post.updated_at = Utc::now();
    let view = serde_json::to_value(&post).unwrap_or(Value::Null);
    state.posts.insert(post.id, post.clone());
    record_audit(
        state,
        tenant_id,
        "post",
        &post.id.to_string(),
        "publish",
        actor,
        Some(before),
        Some(view.clone()),
    );
    if post.status == PostStatus::Published {
        state.emit(
            EVENT_POST_PUBLISHED,
            json!({
                "tenant_id": tenant_id,
                "post_id": post.id.to_string(),
                "network_id": post.network_id.to_string(),
                "account_ids": post.account_ids,
                "accepted": accepted,
                "published_at": post.published_at.map(|when| when.to_rfc3339()),
                "published_by": actor,
            }),
        );
    } else {
        state.emit(
            EVENT_POST_FAILED,
            json!({
                "tenant_id": tenant_id,
                "post_id": post.id.to_string(),
                "network_id": post.network_id.to_string(),
                "account_ids": post.account_ids,
                "accepted": accepted,
                "failed": failed,
                "error": post.error,
                "published_by": actor,
            }),
        );
    }
    Ok(view)
}
