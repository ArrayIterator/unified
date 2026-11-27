//! HTTP handlers for posts: list, create (nested replies),
//! read, update, delete, votes, and reports.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    Caller, caller, can_manage, can_moderate, check_capability, check_new_user_links,
    check_post_rate_limit, created, err, forum_visible_to, index_post, load_forum, load_post,
    load_thread, notify_subscribers, ok, paginate, post_view, post_visible_to,
    recompute_reputation, require_text, require_tenant, resolve_tenant, vote_totals,
};
use super::state::ForumState;
use super::types::{
    CreatePostRequest, Forum, ForumPost, ForumPostEdit, ForumReport, ForumThread, ForumVisibility,
    ForumVote, MAX_BODY_LEN, MAX_REASON_LEN, PERM_FORUMS_VIEW, ReportRequest, UpdatePostRequest,
    VoteRequest,
};

/// Path carrying a thread id.
#[derive(Debug, Deserialize)]
struct ThreadPath {
    thread_id: String,
}

/// Path carrying a thread id and a post id.
#[derive(Debug, Deserialize)]
struct PostPath {
    thread_id: String,
    post_id: String,
}

/// Visibility gate for one forum on an authenticated route.
/// Members-only forums need the `forums.view` capability checked
/// here at the route layer, before the foundation helper is
/// called. A forum the caller may not see reads as 404.
fn check_forum_visible(ctx: &AppContext, caller: &Caller, forum: &Forum) -> UnifiedResult<()> {
    let is_manager = can_manage(ctx, &caller.role);
    let visible = match forum.visibility {
        ForumVisibility::MembersOnly => {
            check_capability(ctx, &caller.role, PERM_FORUMS_VIEW)
                && forum_visible_to(forum, true, is_manager)
        }
        _ => forum_visible_to(forum, true, is_manager),
    };
    if visible {
        Ok(())
    } else {
        Err(UnifiedError::not_found(format!(
            "Forum not found: {}",
            forum.id
        )))
    }
}

/// Load a thread in this tenant and apply its forum's visibility
/// gate. A thread in a forum the caller may not see reads as 404.
fn load_visible_thread(
    state: &ForumState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &Caller,
    thread_id: &str,
) -> UnifiedResult<ForumThread> {
    let thread = load_thread(state, tenant_id, thread_id)?;
    let forum = load_forum(state, tenant_id, &thread.forum_id.to_string())?;
    check_forum_visible(ctx, caller, &forum)?;
    Ok(thread)
}

/// Load a post in this tenant, confirm it belongs to the given
/// thread, and apply the thread's forum visibility gate. A post
/// of another thread reads as 404.
fn load_visible_post(
    state: &ForumState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &Caller,
    thread: &ForumThread,
    post_id: &str,
) -> UnifiedResult<ForumPost> {
    let post = load_post(state, tenant_id, post_id)?;
    if post.thread_id != thread.id {
        return Err(UnifiedError::not_found(format!(
            "Post not found: {}",
            post.id
        )));
    }
    let forum = load_forum(state, tenant_id, &thread.forum_id.to_string())?;
    check_forum_visible(ctx, caller, &forum)?;
    Ok(post)
}

/// True when the caller may edit a post: its author, a moderator,
/// or a manager.
fn can_edit_post(ctx: &AppContext, caller: &Caller, post: &ForumPost) -> bool {
    post.author_id == caller.actor || can_moderate(ctx, &caller.role)
}

/// Collect a post and all of its descendants (replies to replies)
/// by walking the parent chain. Used for cascading deletes.
fn collect_post_subtree(state: &ForumState, root_id: Uuid) -> Vec<Uuid> {
    let mut collected = vec![root_id];
    let mut index = 0;
    while index < collected.len() {
        let parent = collected[index];
        index += 1;
        for entry in state.posts.iter() {
            let post = entry.value();
            if post.parent_post_id == Some(parent) && !collected.contains(&post.id) {
                collected.push(post.id);
            }
        }
    }
    collected
}

/// Delete a post's votes, edit history, and reports.
fn delete_post_rows(state: &ForumState, post_id: Uuid) {
    let vote_ids: Vec<Uuid> = state
        .votes
        .iter()
        .filter(|entry| entry.value().post_id == post_id)
        .map(|entry| *entry.key())
        .collect();
    for vote_id in vote_ids {
        state.votes.remove(&vote_id);
    }
    let edit_ids: Vec<Uuid> = state
        .post_edits
        .iter()
        .filter(|entry| entry.value().post_id == post_id)
        .map(|entry| *entry.key())
        .collect();
    for edit_id in edit_ids {
        state.post_edits.remove(&edit_id);
    }
    let report_ids: Vec<Uuid> = state
        .reports
        .iter()
        .filter(|entry| entry.value().target_type == "post" && entry.value().target_id == post_id)
        .map(|entry| *entry.key())
        .collect();
    for report_id in report_ids {
        state.reports.remove(&report_id);
    }
    state.posts.remove(&post_id);
}

/// Count open reports against one target.
fn count_open_reports(state: &ForumState, target_type: &str, target_id: Uuid) -> i64 {
    state
        .reports
        .iter()
        .filter(|entry| {
            let report = entry.value();
            report.target_type == target_type
                && report.target_id == target_id
                && report.status == "open"
        })
        .count() as i64
}

// Post handlers

pub(crate) async fn list_thread_posts(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ThreadPath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_moderator = can_moderate(&ctx, &caller.role);
        let thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
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
                if post.thread_id != thread.id {
                    return false;
                }
                // Hidden posts are only visible to their author
                // and to moderators (managers included).
                if !post_visible_to(post, &caller.actor, is_moderator) {
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
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ThreadPath>,
    body: web::Json<CreatePostRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_moderator = can_moderate(&ctx, &caller.role);
        let thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        if thread.locked && !is_moderator {
            return Err(UnifiedError::conflict(
                "This thread is locked; no new posts can be added until a moderator unlocks it",
            ));
        }
        check_post_rate_limit(&state, &tenant_id, &caller.actor)?;
        let input = body.into_inner();
        let body_text = require_text(&input.body, "body", MAX_BODY_LEN)?;
        // Top-level posts are only the opening post created with
        // the thread: a new post must name the post it answers.
        // Depth is capped at 2: a reply to a depth-0 post is
        // depth 1, a reply to a depth-1 post is depth 2, and a
        // reply to a depth-2 post is rejected.
        let Some(parent_id) = input.parent_post_id else {
            return Err(UnifiedError::bad_request(
                "parent_post_id is required: only the thread opening post is top-level",
            ));
        };
        let parent = load_post(&state, &tenant_id, &parent_id.to_string())?;
        if parent.thread_id != thread.id {
            return Err(UnifiedError::not_found(format!(
                "Post not found: {parent_id}"
            )));
        }
        if !post_visible_to(&parent, &caller.actor, is_moderator) {
            return Err(UnifiedError::not_found(format!(
                "Post not found: {parent_id}"
            )));
        }
        let depth: i8 = match parent.depth {
            0 => 1,
            1 => 2,
            _ => {
                return Err(UnifiedError::bad_request(
                    "Cannot reply to a depth-2 post: maximum nesting depth reached",
                ));
            }
        };
        let settings = state.current_settings();
        check_new_user_links(&state, &tenant_id, &caller.actor, &body_text, &settings)?;
        // Spam check hook: listeners may flag the body as spam.
        // No listener means the built-in rules alone apply (the
        // default payload carries `spam: false`).
        let spam_context = ctx.hooks.emit(
            "forums.spam.check_requested",
            json!({
                "tenant_id": tenant_id,
                "author_id": caller.actor,
                "body": body_text,
                "spam": false,
            }),
        );
        if spam_context
            .get("spam")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(UnifiedError::bad_request("This post was flagged as spam"));
        }
        let quoted_post_id = match input.quoted_post_id {
            Some(quoted_id) => {
                let quoted = load_post(&state, &tenant_id, &quoted_id.to_string())?;
                if quoted.thread_id != thread.id {
                    return Err(UnifiedError::bad_request(
                        "quoted_post_id must be a post of this thread",
                    ));
                }
                if !post_visible_to(&quoted, &caller.actor, is_moderator) {
                    return Err(UnifiedError::not_found(format!(
                        "Post not found: {quoted_id}"
                    )));
                }
                Some(quoted_id)
            }
            None => None,
        };
        if input.attachment_ids.len() as i64 > settings.max_attachments_per_post {
            return Err(UnifiedError::bad_request(format!(
                "at most {} attachments per post",
                settings.max_attachments_per_post
            )));
        }
        // Attachment ids are metadata only, never file bytes:
        // each must be a non-empty id string.
        let mut attachment_ids: Vec<String> = Vec::new();
        for raw in &input.attachment_ids {
            let id = raw.trim();
            if id.is_empty() {
                return Err(UnifiedError::bad_request(
                    "attachment_ids must be non-empty id strings",
                ));
            }
            attachment_ids.push(id.to_string());
        }
        let mut mentions: Vec<String> = Vec::new();
        for raw in &input.mentions {
            let mention = raw.trim();
            if !mention.is_empty() && !mentions.contains(&mention.to_string()) {
                mentions.push(mention.to_string());
            }
        }
        let now = Utc::now();
        let post = ForumPost {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            thread_id: thread.id,
            parent_post_id: Some(parent.id),
            depth,
            author_id: caller.actor.clone(),
            body: body_text,
            hidden: false,
            edited: false,
            edited_at: None,
            quoted_post_id,
            attachment_ids,
            mentions,
            created_at: now,
            updated_at: now,
        };
        state.posts.insert(post.id, post.clone());
        state.emit(
            "forums.post.created",
            json!({
                "tenant_id": tenant_id,
                "thread_id": thread.id.to_string(),
                "post_id": post.id.to_string(),
                "author_id": post.author_id,
                "parent_post_id": parent.id.to_string(),
                "depth": depth,
            }),
        );
        state.emit(
            "forums.post.replied",
            json!({
                "tenant_id": tenant_id,
                "thread_id": thread.id.to_string(),
                "post_id": post.id.to_string(),
                "author_id": post.author_id,
                "parent_post_id": parent.id.to_string(),
                "parent_author_id": parent.author_id,
            }),
        );
        index_post(&state, &post);
        let link = format!("/forums/threads/{}", thread.id);
        notify_subscribers(
            &state,
            "thread",
            thread.id,
            "New reply in a thread you follow",
            &format!("{} replied in \"{}\".", caller.actor, thread.title),
            &link,
            &caller.actor,
        );
        notify_subscribers(
            &state,
            "forum",
            thread.forum_id,
            "New reply in a forum you follow",
            &format!("{} replied in \"{}\".", caller.actor, thread.title),
            &link,
            &caller.actor,
        );
        Ok(post_view(&state, &post))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_post(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<PostPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_moderator = can_moderate(&ctx, &caller.role);
        let thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        let post = load_visible_post(&state, &ctx, &tenant_id, &caller, &thread, &path.post_id)?;
        if !post_visible_to(&post, &caller.actor, is_moderator) {
            return Err(UnifiedError::not_found(format!(
                "Post not found: {}",
                post.id
            )));
        }
        Ok(post_view(&state, &post))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_post(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<PostPath>,
    body: web::Json<UpdatePostRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        let mut post =
            load_visible_post(&state, &ctx, &tenant_id, &caller, &thread, &path.post_id)?;
        if !post_visible_to(&post, &caller.actor, can_moderate(&ctx, &caller.role)) {
            return Err(UnifiedError::not_found(format!(
                "Post not found: {}",
                post.id
            )));
        }
        if !can_edit_post(&ctx, &caller, &post) {
            return Err(UnifiedError::forbidden(
                "Only the post author or a moderator may update this post",
            ));
        }
        let input = body.into_inner();
        let body_text = require_text(&input.body, "body", MAX_BODY_LEN)?;
        // Keep the previous body in the edit history before
        // overwriting it.
        let now = Utc::now();
        let edit = ForumPostEdit {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            post_id: post.id,
            editor_id: caller.actor.clone(),
            previous_body: post.body.clone(),
            edited_at: now,
        };
        state.post_edits.insert(edit.id, edit);
        post.body = body_text;
        post.edited = true;
        post.edited_at = Some(now);
        post.updated_at = now;
        state.posts.insert(post.id, post.clone());
        state.emit(
            "forums.post.updated",
            json!({
                "tenant_id": tenant_id,
                "thread_id": thread.id.to_string(),
                "post_id": post.id.to_string(),
                "author_id": post.author_id,
                "editor_id": caller.actor,
            }),
        );
        index_post(&state, &post);
        Ok(post_view(&state, &post))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_post(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<PostPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        let post = load_visible_post(&state, &ctx, &tenant_id, &caller, &thread, &path.post_id)?;
        if !post_visible_to(&post, &caller.actor, can_moderate(&ctx, &caller.role)) {
            return Err(UnifiedError::not_found(format!(
                "Post not found: {}",
                post.id
            )));
        }
        if !can_edit_post(&ctx, &caller, &post) {
            return Err(UnifiedError::forbidden(
                "Only the post author or a moderator may delete this post",
            ));
        }
        if post.depth == 0 {
            return Err(UnifiedError::bad_request(
                "The opening post cannot be deleted on its own; delete the thread instead",
            ));
        }
        let author_id = post.author_id.clone();
        // Deleting a post deletes its replies (and their replies),
        // plus each removed post's votes, edit history, and
        // reports.
        let subtree = collect_post_subtree(&state, post.id);
        for post_id in subtree {
            delete_post_rows(&state, post_id);
        }
        recompute_reputation(&state, &tenant_id, &author_id);
        state.emit(
            "forums.post.deleted",
            json!({
                "tenant_id": tenant_id,
                "thread_id": thread.id.to_string(),
                "post_id": post.id.to_string(),
                "author_id": author_id,
            }),
        );
        Ok(json!({ "deleted": true, "post_id": post.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn vote_post(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<PostPath>,
    body: web::Json<VoteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_moderator = can_moderate(&ctx, &caller.role);
        let thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        let post = load_visible_post(&state, &ctx, &tenant_id, &caller, &thread, &path.post_id)?;
        if !post_visible_to(&post, &caller.actor, is_moderator) {
            return Err(UnifiedError::not_found(format!(
                "Post not found: {}",
                post.id
            )));
        }
        let value = body.into_inner().value;
        if value != 1 && value != -1 {
            return Err(UnifiedError::bad_request("vote value must be +1 or -1"));
        }
        // One vote per (post, user): changing a vote updates the
        // row in place.
        let now = Utc::now();
        let existing = state
            .votes
            .iter()
            .find(|entry| entry.value().post_id == post.id && entry.value().user_id == caller.actor)
            .map(|entry| *entry.key());
        match existing {
            Some(id) => {
                if let Some(mut vote) = state.votes.get_mut(&id) {
                    vote.value = value;
                    vote.updated_at = now;
                }
            }
            None => {
                let vote = ForumVote {
                    id: Uuid::now_v7(),
                    tenant_id: tenant_id.clone(),
                    post_id: post.id,
                    user_id: caller.actor.clone(),
                    value,
                    created_at: now,
                    updated_at: now,
                };
                state.votes.insert(vote.id, vote);
            }
        }
        recompute_reputation(&state, &tenant_id, &post.author_id);
        state.emit(
            "forums.vote.cast",
            json!({
                "tenant_id": tenant_id,
                "post_id": post.id.to_string(),
                "user_id": caller.actor,
                "value": value,
            }),
        );
        let (up, down) = vote_totals(&state, post.id);
        Ok(json!({
            "post_id": post.id.to_string(),
            "up_votes": up,
            "down_votes": down,
            "score": up - down,
            "caller_vote": value,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_post_votes(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<PostPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_moderator = can_moderate(&ctx, &caller.role);
        let thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        let post = load_visible_post(&state, &ctx, &tenant_id, &caller, &thread, &path.post_id)?;
        if !post_visible_to(&post, &caller.actor, is_moderator) {
            return Err(UnifiedError::not_found(format!(
                "Post not found: {}",
                post.id
            )));
        }
        let (up, down) = vote_totals(&state, post.id);
        let caller_vote = state
            .votes
            .iter()
            .find(|entry| entry.value().post_id == post.id && entry.value().user_id == caller.actor)
            .map(|entry| entry.value().value);
        Ok(json!({
            "post_id": post.id.to_string(),
            "up_votes": up,
            "down_votes": down,
            "score": up - down,
            "caller_vote": caller_vote,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn report_post(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<PostPath>,
    body: web::Json<ReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(ForumReport, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        let mut post =
            load_visible_post(&state, &ctx, &tenant_id, &caller, &thread, &path.post_id)?;
        if !post_visible_to(&post, &caller.actor, can_moderate(&ctx, &caller.role)) {
            return Err(UnifiedError::not_found(format!(
                "Post not found: {}",
                post.id
            )));
        }
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        // One open report per (reporter, target): return the
        // existing open report instead of creating a duplicate.
        if let Some(existing) = state
            .reports
            .iter()
            .map(|entry| entry.value().clone())
            .find(|report| {
                report.tenant_id == tenant_id
                    && report.target_type == "post"
                    && report.target_id == post.id
                    && report.reporter_id == caller.actor
                    && report.status == "open"
            })
        {
            return Ok((existing, false));
        }
        let now = Utc::now();
        let report = ForumReport {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            target_type: "post".to_string(),
            target_id: post.id,
            reporter_id: caller.actor.clone(),
            reason,
            status: "open".to_string(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.reports.insert(report.id, report.clone());
        // Auto-hide the post once open reports reach the
        // configured threshold.
        let threshold = state.current_settings().spam_report_threshold;
        if count_open_reports(&state, "post", post.id) >= threshold {
            post.hidden = true;
            post.updated_at = now;
            state.posts.insert(post.id, post.clone());
        }
        state.emit(
            "forums.post.reported",
            json!({
                "tenant_id": tenant_id,
                "target_type": "post",
                "target_id": post.id.to_string(),
                "report_id": report.id.to_string(),
                "reporter_id": report.reporter_id,
                "reason": report.reason,
            }),
        );
        Ok((report, true))
    })();
    match result {
        Ok((data, is_new)) => {
            if is_new {
                created(data)
            } else {
                ok(data)
            }
        }
        Err(error) => err(error, debug),
    }
}
