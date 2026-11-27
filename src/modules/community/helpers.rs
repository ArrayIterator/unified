//! Caller, permission, loading, visibility and view helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use serde::{Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use super::types::{CategoryVisibility, CommunityAnnouncement, CommunityCategory, CommunityFlag, CommunityMember, CommunityModerationAction, CommunityPost, CommunityPostEdit, CommunityTopic, MAX_SLUG_LEN, PERM_COMMUNITY_MANAGE};
use super::state::CommunityState;

// Helpers

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

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// Tenant edge guard (C3 pilot 4, group B).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only: no per-record company check is applied.
/// Real partitioning needs a `tenant_id` on the records, a C11
/// follow-up.
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
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// True when the caller holds the manage capability.
pub(crate) fn can_manage(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_COMMUNITY_MANAGE)
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
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

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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
        out.push_str("category");
    }
    out.truncate(MAX_SLUG_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

pub(crate) fn load_category(state: &CommunityState, raw_id: &str) -> UnifiedResult<CommunityCategory> {
    let id = parse_uuid(raw_id, "category")?;
    state
        .categories
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Category not found: {id}")))
}

pub(crate) fn load_topic(state: &CommunityState, raw_id: &str) -> UnifiedResult<CommunityTopic> {
    let id = parse_uuid(raw_id, "topic")?;
    state
        .topics
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Topic not found: {id}")))
}

pub(crate) fn load_post(state: &CommunityState, raw_id: &str) -> UnifiedResult<CommunityPost> {
    let id = parse_uuid(raw_id, "post")?;
    state
        .posts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Post not found: {id}")))
}

pub(crate) fn load_flag(state: &CommunityState, raw_id: &str) -> UnifiedResult<CommunityFlag> {
    let id = parse_uuid(raw_id, "flag")?;
    state
        .flags
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Flag not found: {id}")))
}

pub(crate) fn load_announcement(
    state: &CommunityState,
    raw_id: &str,
) -> UnifiedResult<CommunityAnnouncement> {
    let id = parse_uuid(raw_id, "announcement")?;
    state
        .announcements
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Announcement not found: {id}")))
}

/// Visibility gate for a category: staff-only categories are
/// invisible (404, never 403, so their existence does not leak) to
/// anyone without the manage capability. Members-only categories
/// are invisible the same way to anyone who is not a community
/// member: the view capability alone is not enough, since any
/// caller can self-assert the `X-User-Id` / `X-Role` headers, so
/// membership is checked against the stored member list.
pub(crate) fn check_category_visible(
    state: &CommunityState,
    ctx: &AppContext,
    actor: &str,
    role: &str,
    category: &CommunityCategory,
) -> UnifiedResult<()> {
    if category.visibility == CategoryVisibility::StaffOnly && !can_manage(ctx, role) {
        return Err(UnifiedError::not_found(format!(
            "Category not found: {}",
            category.id
        )));
    }
    if category.visibility == CategoryVisibility::MembersOnly
        && !state
            .members
            .iter()
            .any(|entry| entry.value().user_id == actor)
    {
        return Err(UnifiedError::not_found(format!(
            "Category not found: {}",
            category.id
        )));
    }
    Ok(())
}

/// A post is readable by its author, by anyone when it is not
/// hidden, and by moderators even when hidden.
pub(crate) fn post_visible_to(post: &CommunityPost, actor: &str, is_manager: bool) -> bool {
    !post.hidden || post.author_id == actor || is_manager
}

/// Record an audited moderator action.
pub(crate) fn record_moderation(
    state: &CommunityState,
    target_type: &str,
    target_id: Uuid,
    action: &str,
    moderator_id: &str,
    reason: Option<String>,
) {
    let entry = CommunityModerationAction {
        id: Uuid::now_v7(),
        target_type: target_type.to_string(),
        target_id,
        action: action.to_string(),
        moderator_id: moderator_id.to_string(),
        reason,
        created_at: Utc::now(),
    };
    state.moderation_actions.insert(entry.id, entry);
}

/// How many topics / posts the user created in the last hour, for
/// the rate limits. Counts only, never the bodies.
pub(crate) fn count_recent_topics(state: &CommunityState, author_id: &str, since: DateTime<Utc>) -> i64 {
    state
        .topics
        .iter()
        .filter(|entry| entry.value().author_id == author_id && entry.value().created_at >= since)
        .count() as i64
}

pub(crate) fn count_recent_posts(state: &CommunityState, author_id: &str, since: DateTime<Utc>) -> i64 {
    state
        .posts
        .iter()
        .filter(|entry| entry.value().author_id == author_id && entry.value().created_at >= since)
        .count() as i64
}

pub(crate) fn check_topic_rate_limit(state: &CommunityState, author_id: &str) -> UnifiedResult<()> {
    let limit = state.current_settings().max_topics_per_hour;
    let since = Utc::now() - Duration::hours(1);
    if count_recent_topics(state, author_id, since) >= limit {
        return Err(UnifiedError::bad_request(format!(
            "Topic rate limit reached: at most {limit} topics per hour"
        )));
    }
    Ok(())
}

pub(crate) fn check_post_rate_limit(state: &CommunityState, author_id: &str) -> UnifiedResult<()> {
    let limit = state.current_settings().max_posts_per_hour;
    let since = Utc::now() - Duration::hours(1);
    if count_recent_posts(state, author_id, since) >= limit {
        return Err(UnifiedError::bad_request(format!(
            "Post rate limit reached: at most {limit} posts per hour"
        )));
    }
    Ok(())
}

pub(crate) fn open_flag_count(state: &CommunityState, post_id: Uuid) -> i64 {
    state
        .flags
        .iter()
        .filter(|entry| entry.value().post_id == post_id && entry.value().status == "open")
        .count() as i64
}

pub(crate) fn vote_totals(state: &CommunityState, post_id: Uuid) -> (i64, i64) {
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

pub(crate) fn member_view(state: &CommunityState, member: &CommunityMember) -> Value {
    let topic_count = state
        .topics
        .iter()
        .filter(|entry| entry.value().author_id == member.user_id)
        .count();
    let post_count = state
        .posts
        .iter()
        .filter(|entry| entry.value().author_id == member.user_id)
        .count();
    json!({
        "id": member.id.to_string(),
        "user_id": member.user_id,
        "display_name": member.display_name,
        "badge_count": member.badge_count,
        "topic_count": topic_count,
        "post_count": post_count,
        "joined_at": member.joined_at,
    })
}

pub(crate) fn topic_view(state: &CommunityState, topic: &CommunityTopic, actor: &str, is_manager: bool) -> Value {
    let mut value = serde_json::to_value(topic).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let post_count = state
            .posts
            .iter()
            .filter(|entry| {
                entry.value().topic_id == topic.id
                    && post_visible_to(entry.value(), actor, is_manager)
            })
            .count();
        map.insert("post_count".to_string(), Value::from(post_count));
    }
    value
}

pub(crate) fn post_view(state: &CommunityState, post: &CommunityPost) -> Value {
    let (up, down) = vote_totals(state, post.id);
    let mut value = serde_json::to_value(post).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("up_votes".to_string(), Value::from(up));
        map.insert("down_votes".to_string(), Value::from(down));
        map.insert("score".to_string(), Value::from(up - down));
        let edits: Vec<CommunityPostEdit> = {
            let mut rows: Vec<CommunityPostEdit> = state
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_post(hidden: bool, author: &str) -> CommunityPost {
        CommunityPost {
            id: Uuid::new_v4(),
            topic_id: Uuid::new_v4(),
            parent_post_id: None,
            author_id: author.to_string(),
            body: "Hello".to_string(),
            hidden,
            edited: false,
            edited_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
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
        assert_eq!(slugify(""), "category");
        assert_eq!(slugify("!!!"), "category");
    }

    #[test]
    fn require_text_trims_and_enforces_limits() {
        assert_eq!(require_text("  hi  ", "name", 10).unwrap(), "hi");
        assert!(require_text("", "name", 10).is_err());
        assert!(require_text("   ", "name", 10).is_err());
        assert!(require_text("toolong", "name", 3).is_err());
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

    fn test_context() -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        use std::sync::Arc;
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn make_category(visibility: CategoryVisibility) -> CommunityCategory {
        CommunityCategory {
            id: Uuid::new_v4(),
            name: "General".to_string(),
            description: None,
            slug: "general".to_string(),
            sort_order: 0,
            visibility,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn make_member(user_id: &str) -> CommunityMember {
        let now = Utc::now();
        CommunityMember {
            id: Uuid::new_v4(),
            user_id: user_id.to_string(),
            display_name: "Member".to_string(),
            badge_count: 0,
            joined_at: now,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn members_only_categories_require_a_stored_membership() {
        let state = CommunityState::default();
        let ctx = test_context();
        let member = make_member("user-1");
        state.members.insert(member.id, member);
        let members_only = make_category(CategoryVisibility::MembersOnly);
        assert!(check_category_visible(&state, &ctx, "user-1", "member", &members_only).is_ok());
        assert!(check_category_visible(&state, &ctx, "user-2", "member", &members_only).is_err());
        let public = make_category(CategoryVisibility::Public);
        assert!(check_category_visible(&state, &ctx, "user-2", "member", &public).is_ok());
        let staff_only = make_category(CategoryVisibility::StaffOnly);
        assert!(check_category_visible(&state, &ctx, "user-1", "member", &staff_only).is_err());
    }
}
