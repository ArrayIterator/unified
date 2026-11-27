//! Caller, permission, loading, visibility and view helpers.

use super::state::ForumState;
use super::types::{
    reputation_score, Forum, ForumCategory, ForumModerationAction, ForumPoll, ForumPollOption,
    ForumPost, ForumPostEdit, ForumReport, ForumReputation, ForumSettings, ForumThread,
    ForumVisibility, MAX_SLUG_LEN, PERM_FORUMS_MANAGE, PERM_FORUMS_MODERATE,
};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the events module.
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
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

/// True when the caller holds the manage capability.
pub(crate) fn can_manage(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_FORUMS_MANAGE)
}

/// True when the caller holds the moderate capability, or the
/// manage capability (managers may do everything moderators can).
pub(crate) fn can_moderate(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_FORUMS_MODERATE) || can_manage(ctx, role)
}

/// Resolve the tenant id for this request from the middleware's
/// resolved tenant context. Fails closed: a request with no
/// resolved tenant is rejected, and there is never a fallback to
/// the control plane.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<String> {
    unified_core::tenancy::TenantContext::from_http_request(req)
        .map(|context| context.tenant_id.to_string())
}

// Tenant edge guard (C3 pilot 5, group B).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before caller identity or any capability
/// check.
///
/// The records in this module carry no company field, so the guard
/// is a fail-closed gate only: per-record company partitioning
/// needs a company scope on the record (a C11 follow-up), not a
/// filter here. Tenant partitioning itself is already enforced by
/// the `tenant_id` on every row plus the tenant-matching loaders
/// below. Forum visibility (`Public` / `MembersOnly` /
/// `StaffOnly`) is a separate, per-forum read rule and is unchanged
/// by this gate; the public-forum read handlers resolve the tenant
/// like every other handler, so they are gated, not excluded.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
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

/// A slug is lowercase letters, digits, and single hyphens only,
/// never leading or trailing with a hyphen, at most 128 chars.
pub(crate) fn valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("forum");
    }
    out.truncate(MAX_SLUG_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

// Tenant-scoped loaders
//
// Every loader checks the row's `tenant_id` against the caller's
// tenant. A row from another tenant reads as 404, never 403, so
// its existence does not leak across tenants.

fn tenant_match(row_tenant_id: &str, tenant_id: &str, id: Uuid, what: &str) -> UnifiedResult<()> {
    if row_tenant_id != tenant_id {
        return Err(UnifiedError::not_found(format!("{what} not found: {id}")));
    }
    Ok(())
}

pub(crate) fn load_forum(
    state: &ForumState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<Forum> {
    let id = parse_uuid(raw_id, "forum")?;
    let forum = state
        .forums
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Forum not found: {id}")))?;
    tenant_match(&forum.tenant_id, tenant_id, id, "Forum")?;
    Ok(forum)
}

pub(crate) fn load_forum_by_slug(
    state: &ForumState,
    tenant_id: &str,
    slug: &str,
) -> UnifiedResult<Forum> {
    state
        .forums
        .iter()
        .map(|entry| entry.value().clone())
        .find(|forum| forum.tenant_id == tenant_id && forum.slug == slug)
        .ok_or_else(|| UnifiedError::not_found(format!("Forum not found: {slug}")))
}

pub(crate) fn load_category(
    state: &ForumState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<ForumCategory> {
    let id = parse_uuid(raw_id, "category")?;
    let category = state
        .categories
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Category not found: {id}")))?;
    tenant_match(&category.tenant_id, tenant_id, id, "Category")?;
    Ok(category)
}

pub(crate) fn load_thread(
    state: &ForumState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<ForumThread> {
    let id = parse_uuid(raw_id, "thread")?;
    let thread = state
        .threads
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Thread not found: {id}")))?;
    tenant_match(&thread.tenant_id, tenant_id, id, "Thread")?;
    Ok(thread)
}

pub(crate) fn load_post(
    state: &ForumState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<ForumPost> {
    let id = parse_uuid(raw_id, "post")?;
    let post = state
        .posts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Post not found: {id}")))?;
    tenant_match(&post.tenant_id, tenant_id, id, "Post")?;
    Ok(post)
}

pub(crate) fn load_poll(
    state: &ForumState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<ForumPoll> {
    let id = parse_uuid(raw_id, "poll")?;
    let poll = state
        .polls
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Poll not found: {id}")))?;
    tenant_match(&poll.tenant_id, tenant_id, id, "Poll")?;
    Ok(poll)
}

pub(crate) fn load_report(
    state: &ForumState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<ForumReport> {
    let id = parse_uuid(raw_id, "report")?;
    let report = state
        .reports
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Report not found: {id}")))?;
    tenant_match(&report.tenant_id, tenant_id, id, "Report")?;
    Ok(report)
}

// Visibility

/// Visibility gate for a forum: public forums are visible to
/// everyone; members-only forums need an authenticated caller
/// (the `forums.view` capability itself is checked at the route
/// layer before this helper is called, since this helper carries
/// no role); staff-only forums need a manager and answer false to
/// everyone else, so handlers return 404 instead of 403.
pub(crate) fn forum_visible_to(forum: &Forum, is_authenticated: bool, is_manager: bool) -> bool {
    match forum.visibility {
        ForumVisibility::Public => true,
        ForumVisibility::MembersOnly => is_authenticated,
        ForumVisibility::StaffOnly => is_manager,
    }
}

/// A post is readable by its author, by anyone when it is not
/// hidden, and by moderators even when hidden.
pub(crate) fn post_visible_to(post: &ForumPost, actor: &str, is_manager: bool) -> bool {
    !post.hidden || post.author_id == actor || is_manager
}

// Moderation audit

/// Record an audited moderator action.
pub(crate) fn record_moderation(
    state: &ForumState,
    tenant_id: &str,
    target_type: &str,
    target_id: Uuid,
    action: &str,
    moderator_id: &str,
    reason: Option<String>,
) {
    let entry = ForumModerationAction {
        id: Uuid::now_v7(),
        tenant_id: tenant_id.to_string(),
        target_type: target_type.to_string(),
        target_id,
        action: action.to_string(),
        moderator_id: moderator_id.to_string(),
        reason,
        created_at: Utc::now(),
    };
    state.moderation_actions.insert(entry.id, entry);
}

// Rate limits

/// How many threads / posts the user created in the last hour in
/// this tenant, for the rate limits. Counts only, never the bodies.
pub(crate) fn count_recent_threads(
    state: &ForumState,
    tenant_id: &str,
    author_id: &str,
    since: DateTime<Utc>,
) -> i64 {
    state
        .threads
        .iter()
        .filter(|entry| {
            let thread = entry.value();
            thread.tenant_id == tenant_id
                && thread.author_id == author_id
                && thread.created_at >= since
        })
        .count() as i64
}

pub(crate) fn count_recent_posts(
    state: &ForumState,
    tenant_id: &str,
    author_id: &str,
    since: DateTime<Utc>,
) -> i64 {
    state
        .posts
        .iter()
        .filter(|entry| {
            let post = entry.value();
            post.tenant_id == tenant_id && post.author_id == author_id && post.created_at >= since
        })
        .count() as i64
}

pub(crate) fn check_thread_rate_limit(
    state: &ForumState,
    tenant_id: &str,
    author_id: &str,
) -> UnifiedResult<()> {
    let limit = state.current_settings().max_threads_per_hour;
    let since = Utc::now() - Duration::hours(1);
    if count_recent_threads(state, tenant_id, author_id, since) >= limit {
        return Err(UnifiedError::bad_request(format!(
            "Thread rate limit reached: at most {limit} threads per hour"
        )));
    }
    Ok(())
}

pub(crate) fn check_post_rate_limit(
    state: &ForumState,
    tenant_id: &str,
    author_id: &str,
) -> UnifiedResult<()> {
    let limit = state.current_settings().max_posts_per_hour;
    let since = Utc::now() - Duration::hours(1);
    if count_recent_posts(state, tenant_id, author_id, since) >= limit {
        return Err(UnifiedError::bad_request(format!(
            "Post rate limit reached: at most {limit} posts per hour"
        )));
    }
    Ok(())
}

// Votes and reputation

pub(crate) fn vote_totals(state: &ForumState, post_id: Uuid) -> (i64, i64) {
    let mut up = 0i64;
    let mut down = 0i64;
    for entry in state.votes.iter() {
        let vote = entry.value();
        if vote.post_id == post_id {
            if vote.value > 0 {
                up += 1;
            } else if vote.value < 0 {
                down += 1;
            }
        }
    }
    (up, down)
}

/// Recount a user's reputation from the stored rows: upvotes and
/// downvotes received on their posts in this tenant, plus best
/// answers (threads marked solved whose best-answer post is one
/// of their posts). Upserts the `ForumReputation` row, keeping
/// the existing `badge_count` (badges are owned by gamification).
pub(crate) fn recompute_reputation(state: &ForumState, tenant_id: &str, user_id: &str) {
    let mut upvotes = 0i64;
    let mut downvotes = 0i64;
    for entry in state.votes.iter() {
        let vote = entry.value();
        if vote.value > 0 || vote.value < 0 {
            if let Some(post) = state.posts.get(&vote.post_id) {
                if post.tenant_id == tenant_id && post.author_id == user_id {
                    if vote.value > 0 {
                        upvotes += 1;
                    } else {
                        downvotes += 1;
                    }
                }
            }
        }
    }
    let best_answers = state
        .threads
        .iter()
        .filter(|entry| {
            let thread = entry.value();
            thread.tenant_id == tenant_id && thread.solved
        })
        .filter(|entry| {
            let Some(best_id) = entry.value().best_answer_post_id else {
                return false;
            };
            state
                .posts
                .get(&best_id)
                .map(|post| post.author_id == user_id)
                .unwrap_or(false)
        })
        .count() as i64;
    let score = reputation_score(upvotes, downvotes, best_answers);
    let existing = state
        .reputations
        .iter()
        .find(|entry| entry.value().tenant_id == tenant_id && entry.value().user_id == user_id)
        .map(|entry| (*entry.key(), entry.value().badge_count));
    match existing {
        Some((id, badge_count)) => {
            if let Some(mut row) = state.reputations.get_mut(&id) {
                row.score = score;
                row.upvotes_received = upvotes;
                row.downvotes_received = downvotes;
                row.best_answers = best_answers;
                row.badge_count = badge_count;
                row.updated_at = Utc::now();
            }
        }
        None => {
            let row = ForumReputation {
                id: Uuid::now_v7(),
                tenant_id: tenant_id.to_string(),
                user_id: user_id.to_string(),
                score,
                upvotes_received: upvotes,
                downvotes_received: downvotes,
                best_answers,
                badge_count: 0,
                updated_at: Utc::now(),
            };
            state.reputations.insert(row.id, row);
        }
    }
}

// New-user link limits

/// Count links in a body: occurrences of `http://` plus
/// occurrences of `https://`.
pub(crate) fn count_links(body: &str) -> i64 {
    (body.matches("http://").count() + body.matches("https://").count()) as i64
}

/// Reject a post body that carries more links than
/// `new_user_link_limit` when the author is a new user. A new
/// user is a user with zero existing non-hidden posts in this
/// tenant. `new_user_age_hours` is not consulted here: the
/// zero-post rule is the definition of new for this check, and
/// the age setting is kept for a future account-age check.
pub(crate) fn check_new_user_links(
    state: &ForumState,
    tenant_id: &str,
    author_id: &str,
    body: &str,
    settings: &ForumSettings,
) -> UnifiedResult<()> {
    let has_posts = state.posts.iter().any(|entry| {
        let post = entry.value();
        post.tenant_id == tenant_id && post.author_id == author_id && !post.hidden
    });
    if has_posts {
        return Ok(());
    }
    let links = count_links(body);
    if links > settings.new_user_link_limit {
        return Err(UnifiedError::bad_request(format!(
            "New users may include at most {} link(s) in a post",
            settings.new_user_link_limit
        )));
    }
    Ok(())
}

// Views

pub(crate) fn thread_view(
    state: &ForumState,
    thread: &ForumThread,
    actor: &str,
    is_manager: bool,
) -> Value {
    let mut value = serde_json::to_value(thread).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let post_count = state
            .posts
            .iter()
            .filter(|entry| {
                entry.value().thread_id == thread.id
                    && post_visible_to(entry.value(), actor, is_manager)
            })
            .count();
        map.insert("post_count".to_string(), Value::from(post_count));
    }
    value
}

pub(crate) fn post_view(state: &ForumState, post: &ForumPost) -> Value {
    let (up, down) = vote_totals(state, post.id);
    let mut value = serde_json::to_value(post).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("up_votes".to_string(), Value::from(up));
        map.insert("down_votes".to_string(), Value::from(down));
        map.insert("score".to_string(), Value::from(up - down));
        let edits: Vec<ForumPostEdit> = {
            let mut rows: Vec<ForumPostEdit> = state
                .post_edits
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|edit| edit.post_id == post.id)
                .collect();
            rows.sort_by_key(|edit| edit.edited_at);
            rows
        };
        map.insert(
            "edit_history".to_string(),
            serde_json::to_value(edits).unwrap_or(Value::Null),
        );
    }
    value
}

/// A poll with its options (each carrying its vote count) and the
/// caller's own vote, if any, as the voted option id.
pub(crate) fn poll_view(state: &ForumState, poll: &ForumPoll, caller_user_id: &str) -> Value {
    let mut options: Vec<ForumPollOption> = state
        .poll_options
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|option| option.poll_id == poll.id)
        .collect();
    options.sort_by_key(|option| option.sort_order);
    let option_values: Vec<Value> = options
        .iter()
        .map(|option| {
            let votes = state
                .poll_votes
                .iter()
                .filter(|entry| entry.value().option_id == option.id)
                .count() as i64;
            json!({
                "id": option.id.to_string(),
                "text": option.text,
                "sort_order": option.sort_order,
                "vote_count": votes,
            })
        })
        .collect();
    let caller_vote = state
        .poll_votes
        .iter()
        .find(|entry| entry.value().poll_id == poll.id && entry.value().user_id == caller_user_id)
        .map(|entry| entry.value().option_id.to_string());
    let mut value = serde_json::to_value(poll).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("options".to_string(), Value::Array(option_values));
        map.insert(
            "caller_vote_option_id".to_string(),
            caller_vote.map(Value::String).unwrap_or(Value::Null),
        );
    }
    value
}

// Search indexing and subscriptions

fn snippet(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(max_chars).collect();
        out.push_str("...");
        out
    }
}

pub(crate) fn index_thread(state: &ForumState, thread: &ForumThread) {
    let opening = state
        .posts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|post| post.thread_id == thread.id && post.depth == 0)
        .min_by_key(|post| post.created_at)
        .map(|post| snippet(&post.body, 200));
    state.emit(
        "search.index",
        json!({
            "id": thread.id.to_string(),
            "kind": "forums.thread",
            "title": thread.title,
            "snippet": opening,
            "url": format!("/forums/threads/{}", thread.id),
            "updated_at": thread.updated_at.to_rfc3339(),
        }),
    );
}

pub(crate) fn index_post(state: &ForumState, post: &ForumPost) {
    let title = state
        .threads
        .get(&post.thread_id)
        .map(|entry| entry.value().title.clone())
        .unwrap_or_default();
    state.emit(
        "search.index",
        json!({
            "id": post.id.to_string(),
            "kind": "forums.post",
            "title": title,
            "snippet": snippet(&post.body, 200),
            "url": format!("/forums/threads/{}", post.thread_id),
            "updated_at": post.updated_at.to_rfc3339(),
        }),
    );
}

/// Notify every subscriber of a thread or a forum, except the
/// user named by `exclude_user_id`.
pub(crate) fn notify_subscribers(
    state: &ForumState,
    target_type: &str,
    target_id: Uuid,
    title: &str,
    body: &str,
    link: &str,
    exclude_user_id: &str,
) {
    let subscribers: Vec<String> = state
        .subscriptions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|subscription| {
            subscription.target_type == target_type
                && subscription.target_id == target_id
                && subscription.user_id != exclude_user_id
        })
        .map(|subscription| subscription.user_id)
        .collect();
    for user_id in subscribers {
        state.notify("forums", &user_id, title, body, Some(link.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_post(hidden: bool, author: &str) -> ForumPost {
        ForumPost {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            thread_id: Uuid::new_v4(),
            parent_post_id: None,
            depth: 0,
            author_id: author.to_string(),
            body: "Hello".to_string(),
            hidden,
            edited: false,
            edited_at: None,
            quoted_post_id: None,
            attachment_ids: Vec::new(),
            mentions: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

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
    fn valid_slug_rules() {
        assert!(valid_slug("hello-world"));
        assert!(valid_slug("a"));
        assert!(!valid_slug("Hello"));
        assert!(!valid_slug("a--b"));
        assert!(!valid_slug("-a"));
        assert!(!valid_slug("a-"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("a_b"));
    }

    #[test]
    fn slugify_normalizes_names() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("Rust & Go"), "rust-go");
        assert_eq!(slugify(""), "forum");
        assert_eq!(slugify("!!!"), "forum");
    }

    #[test]
    fn reputation_score_math() {
        assert_eq!(reputation_score(0, 0, 0), 0);
        assert_eq!(reputation_score(5, 2, 1), 10 * 5 - 2 * 2 + 15 * 1);
        assert_eq!(reputation_score(0, 3, 0), -6);
    }

    #[test]
    fn count_links_counts_both_schemes() {
        assert_eq!(count_links("no links here"), 0);
        assert_eq!(count_links("see https://example.com"), 1);
        assert_eq!(count_links("http://a.example and https://b.example"), 2);
        assert_eq!(count_links("https://x https://y https://z"), 3);
    }

    #[test]
    fn post_visible_to_rules() {
        let visible = make_post(false, "author-1");
        assert!(post_visible_to(&visible, "someone-else", false));
        let hidden = make_post(true, "author-1");
        assert!(post_visible_to(&hidden, "author-1", false));
        assert!(!post_visible_to(&hidden, "someone-else", false));
        assert!(post_visible_to(&hidden, "someone-else", true));
    }

    #[test]
    fn paginate_bounds() {
        let items: Vec<i64> = (1..=5).collect();
        let first = paginate(items.clone(), &pagination_params(Some(1), Some(2)));
        assert_eq!(first.items, vec![1, 2]);
        assert_eq!(first.pagination.total, 5);
        let third = paginate(items.clone(), &pagination_params(Some(3), Some(2)));
        assert_eq!(third.items, vec![5]);
        let past_end = paginate(items.clone(), &pagination_params(Some(4), Some(2)));
        assert!(past_end.items.is_empty());
        assert_eq!(past_end.pagination.total, 5);
    }
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach.
    fn tenant_request() -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: uuid::Uuid::now_v7().to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
    }
}
