//! HTTP handlers for threads: list, create (with opening post),
//! read, update, delete, pin, lock, solve, view counting, report,
//! and authenticated search.

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
    Caller, caller, can_manage, can_moderate, check_capability, check_thread_rate_limit, created,
    err, forum_visible_to, index_post, index_thread, load_category, load_forum, load_post,
    load_thread, notify_subscribers, ok, paginate, recompute_reputation, record_moderation,
    require_permission, require_text, require_tenant, resolve_tenant, thread_view,
};
use super::state::ForumState;
use super::types::{
    CreateThreadRequest, Forum, ForumPost, ForumReport, ForumThread, ForumVisibility, MAX_BODY_LEN,
    MAX_REASON_LEN, MAX_TAG_LEN, MAX_TAGS, MAX_TITLE_LEN, ModerationReasonRequest,
    PERM_FORUMS_MODERATE, PERM_FORUMS_VIEW, ReportRequest, SolveThreadRequest, UpdateThreadRequest,
};

/// Path carrying a forum id.
#[derive(Debug, Deserialize)]
struct ForumPath {
    forum_id: String,
}

/// Path carrying a thread id.
#[derive(Debug, Deserialize)]
struct ThreadPath {
    thread_id: String,
}

/// Query for the authenticated search route: `q` plus paging and
/// optional `forum_id` / `author_id` filters.
#[derive(Debug, Deserialize)]
struct SearchForumsQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    forum_id: Option<String>,
    #[serde(default)]
    author_id: Option<String>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    per_page: Option<u32>,
}

fn pagination_from(page: Option<u32>, per_page: Option<u32>) -> PaginationParams {
    PaginationParams {
        page,
        per_page,
        sort: None,
        order: None,
        search: None,
        filters: std::collections::HashMap::new(),
    }
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

/// Load a forum in this tenant and apply the visibility gate.
fn load_visible_forum(
    state: &ForumState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &Caller,
    forum_id: &str,
) -> UnifiedResult<Forum> {
    let forum = load_forum(state, tenant_id, forum_id)?;
    check_forum_visible(ctx, caller, &forum)?;
    Ok(forum)
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

/// True when the caller may edit a thread: its author, a
/// moderator, or a manager.
fn can_edit_thread(ctx: &AppContext, caller: &Caller, thread: &ForumThread) -> bool {
    thread.author_id == caller.actor || can_moderate(ctx, &caller.role)
}

/// Validate tags: trimmed, non-empty, at most `MAX_TAGS` tags of
/// at most `MAX_TAG_LEN` characters each, de-duplicated.
fn validate_tags(raw_tags: &[String]) -> UnifiedResult<Vec<String>> {
    if raw_tags.len() > MAX_TAGS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_TAGS} tags are allowed"
        )));
    }
    let mut tags: Vec<String> = Vec::new();
    for raw in raw_tags {
        let tag = require_text(raw, "tag", MAX_TAG_LEN)?;
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    Ok(tags)
}

/// Delete every stored row that belongs to a thread: its posts
/// (with their edits, votes, and reports), its poll (options and
/// votes), its subscriptions, and reports raised against the
/// thread itself.
fn delete_thread_rows(state: &ForumState, thread: &ForumThread) {
    let post_ids: Vec<Uuid> = state
        .posts
        .iter()
        .filter(|entry| entry.value().thread_id == thread.id)
        .map(|entry| *entry.key())
        .collect();
    for post_id in &post_ids {
        let vote_ids: Vec<Uuid> = state
            .votes
            .iter()
            .filter(|entry| entry.value().post_id == *post_id)
            .map(|entry| *entry.key())
            .collect();
        for vote_id in vote_ids {
            state.votes.remove(&vote_id);
        }
        let edit_ids: Vec<Uuid> = state
            .post_edits
            .iter()
            .filter(|entry| entry.value().post_id == *post_id)
            .map(|entry| *entry.key())
            .collect();
        for edit_id in edit_ids {
            state.post_edits.remove(&edit_id);
        }
        let report_ids: Vec<Uuid> = state
            .reports
            .iter()
            .filter(|entry| {
                entry.value().target_type == "post" && entry.value().target_id == *post_id
            })
            .map(|entry| *entry.key())
            .collect();
        for report_id in report_ids {
            state.reports.remove(&report_id);
        }
        state.posts.remove(post_id);
    }
    let poll_ids: Vec<Uuid> = state
        .polls
        .iter()
        .filter(|entry| entry.value().thread_id == thread.id)
        .map(|entry| *entry.key())
        .collect();
    for poll_id in &poll_ids {
        let option_ids: Vec<Uuid> = state
            .poll_options
            .iter()
            .filter(|entry| entry.value().poll_id == *poll_id)
            .map(|entry| *entry.key())
            .collect();
        for option_id in option_ids {
            state.poll_options.remove(&option_id);
        }
        let poll_vote_ids: Vec<Uuid> = state
            .poll_votes
            .iter()
            .filter(|entry| entry.value().poll_id == *poll_id)
            .map(|entry| *entry.key())
            .collect();
        for poll_vote_id in poll_vote_ids {
            state.poll_votes.remove(&poll_vote_id);
        }
        state.polls.remove(poll_id);
    }
    let subscription_ids: Vec<Uuid> = state
        .subscriptions
        .iter()
        .filter(|entry| {
            entry.value().target_type == "thread" && entry.value().target_id == thread.id
        })
        .map(|entry| *entry.key())
        .collect();
    for subscription_id in subscription_ids {
        state.subscriptions.remove(&subscription_id);
    }
    let report_ids: Vec<Uuid> = state
        .reports
        .iter()
        .filter(|entry| {
            entry.value().target_type == "thread" && entry.value().target_id == thread.id
        })
        .map(|entry| *entry.key())
        .collect();
    for report_id in report_ids {
        state.reports.remove(&report_id);
    }
}

fn parse_bool_filter(raw: Option<&String>) -> Option<bool> {
    raw.and_then(|value| match value.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Thread handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_forum_threads(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ForumPath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let forum = load_visible_forum(&state, &ctx, &tenant_id, &caller, &path.forum_id)?;
        let params = query.into_inner();
        let category_filter = params
            .filters
            .get("category_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let tag_filter = params.filters.get("tag").cloned();
        let pinned_filter = parse_bool_filter(params.filters.get("pinned"));
        let solved_filter = parse_bool_filter(params.filters.get("solved"));
        let author_filter = params.filters.get("author_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .threads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|thread| {
                if thread.tenant_id != tenant_id || thread.forum_id != forum.id {
                    return false;
                }
                if let Some(category_id) = category_filter {
                    if thread.category_id != Some(category_id) {
                        return false;
                    }
                }
                if let Some(tag) = &tag_filter {
                    if !thread.tags.iter().any(|entry| entry == tag) {
                        return false;
                    }
                }
                if let Some(pinned) = pinned_filter {
                    if thread.pinned != pinned {
                        return false;
                    }
                }
                if let Some(solved) = solved_filter {
                    if thread.solved != solved {
                        return false;
                    }
                }
                if let Some(author) = &author_filter {
                    if thread.author_id != *author {
                        return false;
                    }
                }
                if !search.is_empty() && !thread.title.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .map(|thread| thread_view(&state, &thread, &caller.actor, is_manager))
            .collect();
        // Pinned first, then newest.
        items.sort_by(|a, b| {
            let pinned_a = a.get("pinned").and_then(Value::as_bool).unwrap_or(false);
            let pinned_b = b.get("pinned").and_then(Value::as_bool).unwrap_or(false);
            pinned_b.cmp(&pinned_a).then(
                b.get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&a.get("created_at").and_then(Value::as_str)),
            )
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_thread(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateThreadRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let forum = load_visible_forum(
            &state,
            &ctx,
            &tenant_id,
            &caller,
            &input.forum_id.to_string(),
        )?;
        if let Some(category_id) = input.category_id {
            let category = load_category(&state, &tenant_id, &category_id.to_string())?;
            if category.forum_id != forum.id {
                return Err(UnifiedError::not_found(format!(
                    "Category not found: {category_id}"
                )));
            }
        }
        check_thread_rate_limit(&state, &tenant_id, &caller.actor)?;
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let body_text = require_text(&input.body, "body", MAX_BODY_LEN)?;
        let tags = validate_tags(&input.tags)?;
        let now = Utc::now();
        let thread = ForumThread {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            forum_id: forum.id,
            category_id: input.category_id,
            title,
            author_id: caller.actor.clone(),
            tags,
            pinned: false,
            locked: false,
            solved: false,
            best_answer_post_id: None,
            view_count: 0,
            created_at: now,
            updated_at: now,
        };
        state.threads.insert(thread.id, thread.clone());
        // The opening post carries the thread body (depth 0, no
        // parent): creating a thread always creates its first post
        // in the same step.
        let opening = ForumPost {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            thread_id: thread.id,
            parent_post_id: None,
            depth: 0,
            author_id: caller.actor.clone(),
            body: body_text,
            hidden: false,
            edited: false,
            edited_at: None,
            quoted_post_id: None,
            attachment_ids: Vec::new(),
            mentions: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        state.posts.insert(opening.id, opening.clone());
        state.emit(
            "forums.thread.created",
            json!({
                "tenant_id": tenant_id,
                "forum_id": forum.id.to_string(),
                "thread_id": thread.id.to_string(),
                "author_id": thread.author_id,
                "title": thread.title,
            }),
        );
        state.emit(
            "forums.post.created",
            json!({
                "tenant_id": tenant_id,
                "thread_id": thread.id.to_string(),
                "post_id": opening.id.to_string(),
                "author_id": opening.author_id,
                "depth": 0,
            }),
        );
        index_thread(&state, &thread);
        index_post(&state, &opening);
        let is_manager = can_manage(&ctx, &caller.role);
        Ok(thread_view(&state, &thread, &caller.actor, is_manager))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_thread(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ThreadPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        Ok(thread_view(&state, &thread, &caller.actor, is_manager))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_thread(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ThreadPath>,
    body: web::Json<UpdateThreadRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let mut thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        if !can_edit_thread(&ctx, &caller, &thread) {
            return Err(UnifiedError::forbidden(
                "Only the thread author or a moderator may update this thread",
            ));
        }
        let input = body.into_inner();
        if let Some(raw) = &input.title {
            thread.title = require_text(raw, "title", MAX_TITLE_LEN)?;
        }
        if let Some(category_id) = input.category_id {
            let category = load_category(&state, &tenant_id, &category_id.to_string())?;
            if category.forum_id != thread.forum_id {
                return Err(UnifiedError::not_found(format!(
                    "Category not found: {category_id}"
                )));
            }
            thread.category_id = Some(category_id);
        }
        if let Some(tags) = &input.tags {
            thread.tags = validate_tags(tags)?;
        }
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        state.emit(
            "forums.thread.updated",
            json!({
                "tenant_id": tenant_id,
                "thread_id": thread.id.to_string(),
                "author_id": thread.author_id,
                "title": thread.title,
            }),
        );
        index_thread(&state, &thread);
        Ok(thread_view(&state, &thread, &caller.actor, is_manager))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_thread(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ThreadPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        if !can_edit_thread(&ctx, &caller, &thread) {
            return Err(UnifiedError::forbidden(
                "Only the thread author or a moderator may delete this thread",
            ));
        }
        delete_thread_rows(&state, &thread);
        state.threads.remove(&thread.id);
        state.emit(
            "forums.thread.deleted",
            json!({
                "tenant_id": tenant_id,
                "thread_id": thread.id.to_string(),
                "author_id": thread.author_id,
                "title": thread.title,
            }),
        );
        Ok(json!({ "deleted": true, "thread_id": thread.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn pin_thread(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ThreadPath>,
    body: web::Json<ModerationReasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORUMS_MODERATE)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let mut thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        let reason = body.into_inner().reason;
        thread.pinned = true;
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        record_moderation(
            &state,
            &tenant_id,
            "thread",
            thread.id,
            "pin",
            &caller.actor,
            reason.clone(),
        );
        state.emit(
            "forums.thread.pinned",
            json!({
                "tenant_id": tenant_id,
                "thread_id": thread.id.to_string(),
                "moderator_id": caller.actor,
                "reason": reason,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor, is_manager))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn lock_thread(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ThreadPath>,
    body: web::Json<ModerationReasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FORUMS_MODERATE)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let mut thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        let reason = body.into_inner().reason;
        thread.locked = true;
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        record_moderation(
            &state,
            &tenant_id,
            "thread",
            thread.id,
            "lock",
            &caller.actor,
            reason.clone(),
        );
        state.emit(
            "forums.thread.locked",
            json!({
                "tenant_id": tenant_id,
                "thread_id": thread.id.to_string(),
                "moderator_id": caller.actor,
                "reason": reason,
            }),
        );
        Ok(thread_view(&state, &thread, &caller.actor, is_manager))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn solve_thread(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ThreadPath>,
    body: web::Json<SolveThreadRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let mut thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        if !can_edit_thread(&ctx, &caller, &thread) {
            return Err(UnifiedError::forbidden(
                "Only the thread author or a moderator may mark this thread solved",
            ));
        }
        let input = body.into_inner();
        let answer = load_post(&state, &tenant_id, &input.best_answer_post_id.to_string())?;
        if answer.thread_id != thread.id {
            return Err(UnifiedError::bad_request(
                "best_answer_post_id must be a post of this thread",
            ));
        }
        thread.solved = true;
        thread.best_answer_post_id = Some(answer.id);
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        recompute_reputation(&state, &tenant_id, &answer.author_id);
        state.emit(
            "forums.thread.solved",
            json!({
                "tenant_id": tenant_id,
                "thread_id": thread.id.to_string(),
                "best_answer_post_id": answer.id.to_string(),
                "answer_author_id": answer.author_id,
                "solved_by": caller.actor,
            }),
        );
        notify_subscribers(
            &state,
            "thread",
            thread.id,
            "A thread you follow was marked solved",
            &format!("\"{}\" now has a best answer.", thread.title),
            &format!("/forums/threads/{}", thread.id),
            &caller.actor,
        );
        Ok(thread_view(&state, &thread, &caller.actor, is_manager))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Increment a thread's view count by one. There is no per-user
/// de-duplication: every call counts, including repeat views by
/// the same caller, so the count is a raw hit count, not a unique
/// viewer count.
pub(crate) async fn view_thread(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ThreadPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let mut thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
        thread.view_count += 1;
        thread.updated_at = Utc::now();
        state.threads.insert(thread.id, thread.clone());
        Ok(thread_view(&state, &thread, &caller.actor, is_manager))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn report_thread(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<ThreadPath>,
    body: web::Json<ReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(ForumReport, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let thread = load_visible_thread(&state, &ctx, &tenant_id, &caller, &path.thread_id)?;
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
                    && report.target_type == "thread"
                    && report.target_id == thread.id
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
            target_type: "thread".to_string(),
            target_id: thread.id,
            reporter_id: caller.actor.clone(),
            reason,
            status: "open".to_string(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.reports.insert(report.id, report.clone());
        state.emit(
            "forums.post.reported",
            json!({
                "tenant_id": tenant_id,
                "target_type": "thread",
                "target_id": thread.id.to_string(),
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

pub(crate) async fn search_forums(
    state: web::Data<ForumState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<SearchForumsQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let input = query.into_inner();
        let needle = input.q.unwrap_or_default().trim().to_lowercase();
        if needle.chars().count() < 2 {
            return Err(UnifiedError::bad_request("q must be at least 2 characters"));
        }
        let forum_filter = match &input.forum_id {
            Some(raw) => {
                let forum = load_visible_forum(&state, &ctx, &tenant_id, &caller, raw)?;
                Some(forum.id)
            }
            None => None,
        };
        let author_filter = input.author_id.clone();
        let params = pagination_from(input.page, input.per_page);
        // Forums the caller can see: public ones, members-only
        // ones when the caller holds `forums.view`, staff-only
        // ones for managers only.
        let can_view_members = check_capability(&ctx, &caller.role, PERM_FORUMS_VIEW);
        let visible_forum_ids: Vec<Uuid> = state
            .forums
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|forum| {
                if forum.tenant_id != tenant_id {
                    return false;
                }
                match forum.visibility {
                    ForumVisibility::Public => true,
                    ForumVisibility::MembersOnly => can_view_members,
                    ForumVisibility::StaffOnly => is_manager,
                }
            })
            .map(|forum| forum.id)
            .collect();
        let mut items: Vec<Value> = state
            .threads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|thread| {
                if thread.tenant_id != tenant_id || !visible_forum_ids.contains(&thread.forum_id) {
                    return false;
                }
                if let Some(forum_id) = forum_filter {
                    if thread.forum_id != forum_id {
                        return false;
                    }
                }
                if let Some(author) = &author_filter {
                    if thread.author_id != *author {
                        return false;
                    }
                }
                if thread.title.to_lowercase().contains(&needle) {
                    return true;
                }
                // Substring over visible post bodies of this
                // thread (hidden posts are only searchable by
                // their author and by moderators).
                state.posts.iter().any(|entry| {
                    let post = entry.value();
                    if post.thread_id != thread.id {
                        return false;
                    }
                    let is_moderator = can_moderate(&ctx, &caller.role);
                    if post.hidden && post.author_id != caller.actor && !is_moderator {
                        return false;
                    }
                    post.body.to_lowercase().contains(&needle)
                })
            })
            .map(|thread| thread_view(&state, &thread, &caller.actor, is_manager))
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
