//! HTTP handlers for posts, votes, flags and moderation.

use super::helpers::{
    caller, caller_company, can_manage, check_category_visible, check_post_rate_limit, created,
    err, load_flag, load_post, load_topic, ok, open_flag_count, paginate, post_view,
    post_visible_to, record_moderation, require_permission, require_tenant, require_text,
    vote_totals,
};
use super::state::CommunityState;
use super::types::{
    CommunityFlag, CommunityModerationAction, CommunityPost, CommunityPostEdit, CommunityVote,
    CreatePostRequest, FlagRequest, MAX_BODY_LEN, MAX_REASON_LEN, ModerationReasonRequest,
    PERM_COMMUNITY_MANAGE, PERM_COMMUNITY_VIEW, ReviewFlagRequest, UpdatePostRequest, VoteRequest,
};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

// Post handlers

pub(crate) async fn list_topic_posts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let topic = load_topic(&state, &path.into_inner())?;
        let category = state
            .categories
            .get(&topic.category_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Category not found: {}", topic.category_id))
            })?;
        check_category_visible(&state, &ctx, &caller.actor, &caller.role, &category)?;
        let params = query.into_inner();
        let author_filter = params.filters.get("author_id").cloned();
        let parent_filter = params
            .filters
            .get("parent_post_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .posts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|post| {
                if post.topic_id != topic.id {
                    return false;
                }
                if !post_visible_to(post, &caller.actor, is_manager) {
                    return false;
                }
                if let Some(author) = &author_filter {
                    if post.author_id != *author {
                        return false;
                    }
                }
                if let Some(parent) = parent_filter {
                    if post.parent_post_id != Some(parent) {
                        return false;
                    }
                }
                if !search.is_empty() && !post.body.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .map(|post| post_view(&state, &post))
            .collect();
        items.sort_by(|a, b| {
            a.get("created_at")
                .and_then(Value::as_str)
                .cmp(&b.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePostRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let topic = load_topic(&state, &path.into_inner())?;
        let category = state
            .categories
            .get(&topic.category_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Category not found: {}", topic.category_id))
            })?;
        check_category_visible(&state, &ctx, &caller.actor, &caller.role, &category)?;
        if topic.locked {
            return Err(UnifiedError::conflict(
                "This topic is locked; no new posts can be added until a moderator unlocks it",
            ));
        }
        check_post_rate_limit(&state, &caller.actor)?;
        let input = body.into_inner();
        let body_text = require_text(&input.body, "body", MAX_BODY_LEN)?;
        let parent_post_id = match input.parent_post_id {
            Some(parent_id) => {
                let parent = state
                    .posts
                    .get(&parent_id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| {
                        UnifiedError::not_found(format!("Parent post not found: {parent_id}"))
                    })?;
                if parent.topic_id != topic.id {
                    return Err(UnifiedError::bad_request(
                        "parent_post_id belongs to a different topic",
                    ));
                }
                if parent.parent_post_id.is_some() {
                    return Err(UnifiedError::bad_request(
                        "Replies are one level deep: a reply cannot answer another reply",
                    ));
                }
                if !post_visible_to(&parent, &caller.actor, is_manager) {
                    return Err(UnifiedError::not_found(format!(
                        "Parent post not found: {parent_id}"
                    )));
                }
                Some(parent_id)
            }
            None => None,
        };
        let now = Utc::now();
        let post = CommunityPost {
            id: Uuid::now_v7(),
            topic_id: topic.id,
            parent_post_id,
            author_id: caller.actor.clone(),
            body: body_text,
            hidden: false,
            edited: false,
            edited_at: None,
            created_at: now,
            updated_at: now,
        };
        state.posts.insert(post.id, post.clone());
        let (event_name, is_reply) = if parent_post_id.is_some() {
            ("community.post.replied", true)
        } else {
            ("community.post.created", false)
        };
        state.emit(
            event_name,
            json!({
                "post_id": post.id.to_string(),
                "topic_id": topic.id.to_string(),
                "category_id": topic.category_id.to_string(),
                "author_id": post.author_id,
                "user_id": post.author_id,
                "parent_post_id": parent_post_id.map(|id| id.to_string()),
                "is_reply": is_reply,
            }),
        );
        Ok(post_view(&state, &post))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let post = load_post(&state, &path.into_inner())?;
        if !post_visible_to(&post, &caller.actor, is_manager) {
            return Err(UnifiedError::not_found(format!("Post not found: {}", post.id)));
        }
        let topic = load_topic(&state, &post.topic_id.to_string())?;
        let category = state
            .categories
            .get(&topic.category_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Category not found: {}", topic.category_id))
            })?;
        check_category_visible(&state, &ctx, &caller.actor, &caller.role, &category)?;
        Ok(post_view(&state, &post))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePostRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let mut post = load_post(&state, &path.into_inner())?;
        if post.author_id != caller.actor && !is_manager {
            return Err(UnifiedError::forbidden(
                "Only the post author or a moderator can edit this post",
            ));
        }
        let input = body.into_inner();
        let new_body = require_text(&input.body, "body", MAX_BODY_LEN)?;
        if new_body == post.body {
            return Ok(post_view(&state, &post));
        }
        // Keep the edit history: the previous body is stored before
        // the post is overwritten, never silently replaced.
        let now = Utc::now();
        let edit = CommunityPostEdit {
            id: Uuid::now_v7(),
            post_id: post.id,
            editor_id: caller.actor.clone(),
            previous_body: post.body.clone(),
            edited_at: now,
        };
        state.post_edits.insert(edit.id, edit);
        post.body = new_body;
        post.edited = true;
        post.edited_at = Some(now);
        post.updated_at = now;
        state.posts.insert(post.id, post.clone());
        state.emit(
            "community.post.updated",
            json!({
                "post_id": post.id.to_string(),
                "topic_id": post.topic_id.to_string(),
                "editor_id": caller.actor,
            }),
        );
        Ok(post_view(&state, &post))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let post = load_post(&state, &path.into_inner())?;
        if post.author_id != caller.actor && !is_manager {
            return Err(UnifiedError::forbidden(
                "Only the post author or a moderator can delete this post",
            ));
        }
        let id = post.id;
        // Replies to this post are removed with it; their votes and
        // flags go too. Edit history is kept for the audit trail.
        let reply_ids: Vec<Uuid> = state
            .posts
            .iter()
            .filter(|entry| entry.value().parent_post_id == Some(id))
            .map(|entry| *entry.key())
            .collect();
        let mut removed: Vec<Uuid> = vec![id];
        removed.extend(reply_ids.iter().copied());
        for key in &removed {
            state.posts.remove(key);
        }
        let stale_votes: Vec<Uuid> = state
            .votes
            .iter()
            .filter(|entry| removed.contains(&entry.value().post_id))
            .map(|entry| *entry.key())
            .collect();
        for key in stale_votes {
            state.votes.remove(&key);
        }
        let stale_flags: Vec<Uuid> = state
            .flags
            .iter()
            .filter(|entry| removed.contains(&entry.value().post_id))
            .map(|entry| *entry.key())
            .collect();
        for key in stale_flags {
            state.flags.remove(&key);
        }
        state.emit(
            "community.post.deleted",
            json!({ "post_id": id.to_string(), "topic_id": post.topic_id.to_string() }),
        );
        Ok(json!({ "deleted": true, "post_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Vote handlers

pub(crate) async fn vote_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<VoteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let post = load_post(&state, &path.into_inner())?;
        if !post_visible_to(&post, &caller.actor, is_manager) {
            return Err(UnifiedError::not_found(format!("Post not found: {}", post.id)));
        }
        let value = body.into_inner().value;
        if value != 1 && value != -1 {
            return Err(UnifiedError::bad_request("value must be 1 (up) or -1 (down)"));
        }
        let now = Utc::now();
        // One vote per user per post: an existing vote is updated,
        // never duplicated.
        let existing = state
            .votes
            .iter()
            .find(|entry| entry.value().post_id == post.id && entry.value().user_id == caller.actor)
            .map(|entry| *entry.key());
        let vote = match existing {
            Some(vote_id) => {
                let mut stored = state
                    .votes
                    .get_mut(&vote_id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found("Vote not found"))?;
                stored.value = value;
                stored.updated_at = now;
                state.votes.insert(stored.id, stored.clone());
                stored
            }
            None => {
                let vote = CommunityVote {
                    id: Uuid::now_v7(),
                    post_id: post.id,
                    user_id: caller.actor.clone(),
                    value,
                    created_at: now,
                    updated_at: now,
                };
                state.votes.insert(vote.id, vote.clone());
                vote
            }
        };
        let (up, down) = vote_totals(&state, post.id);
        state.emit(
            "community.vote.cast",
            json!({
                "post_id": post.id.to_string(),
                "topic_id": post.topic_id.to_string(),
                "user_id": caller.actor,
                "value": value,
                "score": up - down,
            }),
        );
        Ok(json!({
            "post_id": post.id.to_string(),
            "vote_id": vote.id.to_string(),
            "value": vote.value,
            "up_votes": up,
            "down_votes": down,
            "score": up - down,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_post_votes(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let post = load_post(&state, &path.into_inner())?;
        if !post_visible_to(&post, &caller.actor, is_manager) {
            return Err(UnifiedError::not_found(format!("Post not found: {}", post.id)));
        }
        let (up, down) = vote_totals(&state, post.id);
        let my_vote = state
            .votes
            .iter()
            .find(|entry| entry.value().post_id == post.id && entry.value().user_id == caller.actor)
            .map(|entry| entry.value().value);
        Ok(json!({
            "post_id": post.id.to_string(),
            "up_votes": up,
            "down_votes": down,
            "score": up - down,
            "my_vote": my_vote,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Flag + moderation handlers

pub(crate) async fn flag_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FlagRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let mut post = load_post(&state, &path.into_inner())?;
        if !post_visible_to(&post, &caller.actor, is_manager) {
            return Err(UnifiedError::not_found(format!("Post not found: {}", post.id)));
        }
        let reason = require_text(&body.into_inner().reason, "reason", MAX_REASON_LEN)?;
        if state
            .flags
            .iter()
            .any(|entry| {
                entry.value().post_id == post.id
                    && entry.value().reporter_id == caller.actor
                    && entry.value().status == "open"
            })
        {
            return Err(UnifiedError::conflict(
                "You already have an open flag on this post",
            ));
        }
        let now = Utc::now();
        let flag = CommunityFlag {
            id: Uuid::now_v7(),
            post_id: post.id,
            reporter_id: caller.actor.clone(),
            reason,
            status: "open".to_string(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.flags.insert(flag.id, flag.clone());
        state.emit(
            "community.post.flagged",
            json!({
                "post_id": post.id.to_string(),
                "topic_id": post.topic_id.to_string(),
                "flag_id": flag.id.to_string(),
                "reporter_id": flag.reporter_id,
                "open_flags": open_flag_count(&state, post.id),
            }),
        );
        // Spam threshold: enough open flags auto-hide the post
        // pending review. The post is never deleted by this path.
        let threshold = state.current_settings().spam_flag_threshold;
        let mut auto_hidden = false;
        if !post.hidden && open_flag_count(&state, post.id) >= threshold {
            post.hidden = true;
            post.updated_at = now;
            state.posts.insert(post.id, post.clone());
            auto_hidden = true;
            record_moderation(
                &state,
                "post",
                post.id,
                "auto_hide",
                "system",
                Some(format!(
                    "Auto-hidden after reaching the spam threshold of {threshold} open flags"
                )),
            );
            state.emit(
                "community.post.hidden",
                json!({
                    "post_id": post.id.to_string(),
                    "topic_id": post.topic_id.to_string(),
                    "reason": "spam_threshold",
                    "auto": true,
                }),
            );
        }
        Ok(json!({
            "flag_id": flag.id.to_string(),
            "post_id": post.id.to_string(),
            "status": flag.status,
            "open_flags": open_flag_count(&state, post.id),
            "auto_hidden": auto_hidden,
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_flags(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let post_filter = params
            .filters
            .get("post_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let mut items: Vec<CommunityFlag> = state
            .flags
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|flag| {
                if let Some(status) = &status_filter {
                    if &flag.status != status {
                        return false;
                    }
                }
                if let Some(post_id) = post_filter {
                    if flag.post_id != post_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        // Open flags first, then newest first.
        items.sort_by(|a, b| {
            let open_a = a.status == "open";
            let open_b = b.status == "open";
            open_b
                .cmp(&open_a)
                .then(b.created_at.cmp(&a.created_at))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn review_flag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewFlagRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let mut flag = load_flag(&state, &path.into_inner())?;
        if flag.status != "open" {
            return Err(UnifiedError::conflict(format!(
                "Only an open flag can be reviewed (status is \"{}\")",
                flag.status
            )));
        }
        let input = body.into_inner();
        let action = input.action.trim().to_lowercase();
        if action != "dismiss" && action != "hide" {
            return Err(UnifiedError::bad_request(
                "action must be \"dismiss\" or \"hide\"",
            ));
        }
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let mut post_hidden = false;
        if action == "hide" {
            if let Some(mut post) = state.posts.get_mut(&flag.post_id) {
                if !post.hidden {
                    post.hidden = true;
                    post.updated_at = now;
                    post_hidden = true;
                }
            }
            flag.status = "actioned".to_string();
            record_moderation(
                &state,
                "post",
                flag.post_id,
                "hide",
                &caller.actor,
                reason.clone(),
            );
            state.emit(
                "community.post.hidden",
                json!({
                    "post_id": flag.post_id.to_string(),
                    "flag_id": flag.id.to_string(),
                    "reason": "flag_review",
                    "auto": false,
                }),
            );
        } else {
            flag.status = "dismissed".to_string();
            record_moderation(
                &state,
                "flag",
                flag.id,
                "dismiss",
                &caller.actor,
                reason.clone(),
            );
        }
        flag.reviewed_by = Some(caller.actor.clone());
        flag.reviewed_at = Some(now);
        flag.updated_at = now;
        state.flags.insert(flag.id, flag.clone());
        state.emit(
            "community.flag.reviewed",
            json!({
                "flag_id": flag.id.to_string(),
                "post_id": flag.post_id.to_string(),
                "action": action,
                "reviewed_by": caller.actor,
                "post_hidden": post_hidden,
            }),
        );
        Ok(json!({
            "flag_id": flag.id.to_string(),
            "post_id": flag.post_id.to_string(),
            "status": flag.status,
            "action": action,
            "post_hidden": post_hidden,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn hide_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ModerationReasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let mut post = load_post(&state, &path.into_inner())?;
        let reason = match &body.into_inner().reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        if post.hidden {
            return Err(UnifiedError::conflict("This post is already hidden"));
        }
        post.hidden = true;
        post.updated_at = Utc::now();
        state.posts.insert(post.id, post.clone());
        record_moderation(&state, "post", post.id, "hide", &caller.actor, reason);
        state.emit(
            "community.post.hidden",
            json!({
                "post_id": post.id.to_string(),
                "topic_id": post.topic_id.to_string(),
                "reason": "moderator",
                "auto": false,
            }),
        );
        Ok(post_view(&state, &post))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn unhide_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ModerationReasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let mut post = load_post(&state, &path.into_inner())?;
        let reason = match &body.into_inner().reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        if !post.hidden {
            return Err(UnifiedError::conflict("This post is not hidden"));
        }
        post.hidden = false;
        post.updated_at = Utc::now();
        state.posts.insert(post.id, post.clone());
        record_moderation(&state, "post", post.id, "unhide", &caller.actor, reason);
        state.emit(
            "community.post.unhidden",
            json!({
                "post_id": post.id.to_string(),
                "topic_id": post.topic_id.to_string(),
            }),
        );
        Ok(post_view(&state, &post))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_moderation_actions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let params = query.into_inner();
        let action_filter = params.filters.get("action").cloned();
        let moderator_filter = params.filters.get("moderator_id").cloned();
        let target_filter = params
            .filters
            .get("target_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let mut items: Vec<CommunityModerationAction> = state
            .moderation_actions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                if let Some(action) = &action_filter {
                    if &entry.action != action {
                        return false;
                    }
                }
                if let Some(moderator) = &moderator_filter {
                    if &entry.moderator_id != moderator {
                        return false;
                    }
                }
                if let Some(target) = target_filter {
                    if entry.target_id != target {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|entry| std::cmp::Reverse(entry.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
