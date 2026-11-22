// Community / Forum module — discussion categories, topics, posts with
// one-level nested replies, votes, flags and moderation, a member
// directory, announcements, and badge counts fed by the gamification
// module.
//
// Routes live under `/api/v1/community`:
//
//   Categories:
//     GET/POST /categories — list categories / create a category.
//     GET/PATCH/DELETE /categories/{id}.
//   Topics:
//     GET/POST /topics — list topics / create a topic.
//     GET/PATCH/DELETE /topics/{id}.
//     POST /topics/{id}/pin, /topics/{id}/lock.
//   Posts and replies:
//     GET/POST /topics/{id}/posts — list posts of a topic / reply
//       to the topic (a reply names `parent_post_id` to answer one
//       post; replies are one level deep only).
//     GET/PATCH/DELETE /posts/{id}.
//     POST /posts/{id}/vote — up (+1) or down (-1); one vote per
//       user per post, changing a vote updates it.
//     GET  /posts/{id}/votes — vote totals plus the caller's vote.
//     POST /posts/{id}/flag — flag a post with a reason.
//     POST /posts/{id}/hide, /posts/{id}/unhide (moderators).
//   Flags and moderation audit:
//     GET  /flags — open flags first (moderators).
//     POST /flags/{id}/review — dismiss a flag or hide the post.
//     GET  /moderation-actions — audited moderator actions.
//   Members:
//     GET  /members — member directory (display name, join date,
//       badge counts; never emails or private data).
//     POST /members — join the community (one row per user).
//     GET  /members/{user_id} — one member.
//   Announcements:
//     GET/POST /announcements — list / create (draft until
//       published).
//     GET/PATCH/DELETE /announcements/{id}.
//     POST /announcements/{id}/publish — publish + notify members.
//   Settings:
//     GET  /settings — rate limits and spam threshold.
//     PATCH /settings (moderators).
//   Public (no auth; see the public-access note below):
//     GET  /public/topics — topics in public categories.
//     GET  /public/topics/{id} — one public topic with its posts.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory category, topic, post, vote, flag, announcement,
// member, moderation-action, and post-edit-history management;
// one vote per user per post enforced by upsert; edit history kept
// on every post edit (the previous body is stored, never silently
// overwritten); per-user hourly rate limits for topics and posts
// (defaults 10 topics / 30 posts, configurable); a spam-flag
// threshold that auto-hides a post pending review (default 5
// flags); visibility rules (public / members-only / staff-only
// categories, staff-only content answers 404 to anyone without the
// manage capability); and announcement publishing that notifies
// every member through `notification.requested`. What is NOT built
// here: badge definitions and badge awarding (the gamification
// module owns badges — this module only listens to
// `gamification.badge.awarded` to keep member badge counts, and
// emits community events so gamification can award points for
// community help); actual email / push delivery (the notifications
// module owns delivery — this module only emits
// `notification.requested`); full-text search (list endpoints
// filter by exact fields and substring-match title/body only);
// and durable storage (the DashMap stores are authoritative;
// migration `2026-11-22-000056_community` is the persistence
// target).
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler — the
// check lives in the API Manager middleware / `resolve_public`
// path — so the public routes below rely on that gate: an
// administrator must create the matching `ApiAccessRule` rows for
// `GET /api/v1/community/public/*`. The routes are registered in
// `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks them up.
// Do NOT treat the absence of a capability check on a public route
// as an open door: without the rule the API Manager denies the
// call.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `community.category.created`, `community.category.updated`,
//     `community.category.deleted`
//   - `community.topic.created`, `community.topic.updated`,
//     `community.topic.deleted`, `community.topic.pinned`,
//     `community.topic.locked`
//   - `community.post.created`, `community.post.replied`,
//     `community.post.updated`, `community.post.deleted`,
//     `community.post.hidden`, `community.post.unhidden`
//   - `community.vote.cast`
//   - `community.post.flagged`
//   - `community.flag.reviewed`
//   - `community.announcement.created`,
//     `community.announcement.published`
//   - `community.member.joined`
//   - `notification.requested` (announcement published, in the
//     notifications module's payload shape).
// Events this module listens to:
//   - `gamification.badge.awarded` — increments the member's
//     badge count (badges themselves are owned by gamification;
//     this module never awards a badge).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_TITLE_LEN: usize = 200;
const MAX_BODY_LEN: usize = 20000;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_REASON_LEN: usize = 1000;
const MAX_SLUG_LEN: usize = 128;
const MAX_TAG_LEN: usize = 64;
const MAX_TAGS: usize = 20;
/// Default rate limits: topics and posts per user per hour.
const DEFAULT_MAX_TOPICS_PER_HOUR: i64 = 10;
const DEFAULT_MAX_POSTS_PER_HOUR: i64 = 30;
/// Default number of open flags that auto-hides a post.
const DEFAULT_SPAM_FLAG_THRESHOLD: i64 = 5;

/// Capability names checked through the `role.check` hook.
const PERM_COMMUNITY_VIEW: &str = "community.view";
const PERM_COMMUNITY_MANAGE: &str = "community.manage";

/// Hook event name this module listens to.
const GAMIFICATION_BADGE_AWARDED: &str = "gamification.badge.awarded";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Who may see a category. `public` categories appear on the public
/// routes; `members_only` categories need any authenticated member
/// (the view capability); `staff_only` categories need the manage
/// capability and answer 404 to everyone else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryVisibility {
    Public,
    MembersOnly,
    StaffOnly,
}

impl Default for CategoryVisibility {
    fn default() -> Self {
        Self::Public
    }
}

impl CategoryVisibility {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::MembersOnly => "members_only",
            Self::StaffOnly => "staff_only",
        }
    }
}

/// One discussion category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityCategory {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub slug: String,
    pub sort_order: i64,
    pub visibility: CategoryVisibility,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One discussion topic inside a category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityTopic {
    pub id: Uuid,
    pub category_id: Uuid,
    pub title: String,
    pub author_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub pinned: bool,
    pub locked: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One post. Top-level posts have `parent_post_id` None; a reply
/// names the post it answers, and that parent must itself be a
/// top-level post, so replies are exactly one level deep.
/// `hidden` is set by moderators or by the spam-flag auto-hide;
/// hidden posts are only visible to moderators. `edited` marks a
/// post whose body changed; the previous bodies live in
/// `CommunityPostEdit` rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityPost {
    pub id: Uuid,
    pub topic_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_post_id: Option<Uuid>,
    pub author_id: String,
    pub body: String,
    pub hidden: bool,
    pub edited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One kept edit of a post: the previous body, who edited it, and
/// when. Edits append a row here; the stored body is never
/// overwritten in place without a record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityPostEdit {
    pub id: Uuid,
    pub post_id: Uuid,
    pub editor_id: String,
    pub previous_body: String,
    pub edited_at: DateTime<Utc>,
}

/// One vote on a post: +1 (up) or -1 (down). One row per
/// (post, user); changing a vote updates the row in place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityVote {
    pub id: Uuid,
    pub post_id: Uuid,
    pub user_id: String,
    pub value: i8,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One flag raised against a post. `status` is `open`, `dismissed`,
/// or `actioned` (a moderator acted on it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityFlag {
    pub id: Uuid,
    pub post_id: Uuid,
    pub reporter_id: String,
    pub reason: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audited moderator action: hide / unhide / pin / lock /
/// flag review, with who did it and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityModerationAction {
    pub id: Uuid,
    pub target_type: String,
    pub target_id: Uuid,
    pub action: String,
    pub moderator_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One community member. The directory exposes display name, join
/// date, badge counts, and activity counts only — never emails or
/// other private data. `badge_count` is maintained by the
/// `gamification.badge.awarded` listener.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityMember {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
    pub badge_count: i64,
    pub joined_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One announcement. Drafts are created unpublished; publishing
/// notifies every member and stamps `published_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityAnnouncement {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub pinned: bool,
    pub published: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
    pub author_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Rate limits and spam handling, configurable per installation.
/// The in-memory copy is authoritative until the migration lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunitySettings {
    pub max_topics_per_hour: i64,
    pub max_posts_per_hour: i64,
    pub spam_flag_threshold: i64,
}

impl Default for CommunitySettings {
    fn default() -> Self {
        Self {
            max_topics_per_hour: DEFAULT_MAX_TOPICS_PER_HOUR,
            max_posts_per_hour: DEFAULT_MAX_POSTS_PER_HOUR,
            spam_flag_threshold: DEFAULT_SPAM_FLAG_THRESHOLD,
        }
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateCategoryRequest {
    pub name: String,
    pub description: Option<String>,
    pub slug: Option<String>,
    pub sort_order: Option<i64>,
    pub visibility: Option<CategoryVisibility>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCategoryRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub slug: Option<String>,
    pub sort_order: Option<i64>,
    pub visibility: Option<CategoryVisibility>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTopicRequest {
    pub category_id: Uuid,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTopicRequest {
    pub title: Option<String>,
    pub category_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePostRequest {
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_post_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePostRequest {
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct VoteRequest {
    /// +1 for up, -1 for down.
    pub value: i8,
}

#[derive(Debug, Deserialize)]
pub struct FlagRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct ReviewFlagRequest {
    /// `dismiss` clears the flag; `hide` hides the post.
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JoinRequest {
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateAnnouncementRequest {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAnnouncementRequest {
    pub title: Option<String>,
    pub body: Option<String>,
    pub pinned: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub max_topics_per_hour: Option<i64>,
    pub max_posts_per_hour: Option<i64>,
    pub spam_flag_threshold: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ModerationReasonRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the community. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000056_community`; until that migration is executed,
/// community data does not survive a restart.
#[derive(Clone)]
pub struct CommunityState {
    pub categories: Arc<DashMap<Uuid, CommunityCategory>>,
    pub topics: Arc<DashMap<Uuid, CommunityTopic>>,
    pub posts: Arc<DashMap<Uuid, CommunityPost>>,
    pub post_edits: Arc<DashMap<Uuid, CommunityPostEdit>>,
    pub votes: Arc<DashMap<Uuid, CommunityVote>>,
    pub flags: Arc<DashMap<Uuid, CommunityFlag>>,
    pub moderation_actions: Arc<DashMap<Uuid, CommunityModerationAction>>,
    pub members: Arc<DashMap<Uuid, CommunityMember>>,
    pub announcements: Arc<DashMap<Uuid, CommunityAnnouncement>>,
    pub settings: Arc<RwLock<CommunitySettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for CommunityState {
    fn default() -> Self {
        Self {
            categories: Arc::new(DashMap::new()),
            topics: Arc::new(DashMap::new()),
            posts: Arc::new(DashMap::new()),
            post_edits: Arc::new(DashMap::new()),
            votes: Arc::new(DashMap::new()),
            flags: Arc::new(DashMap::new()),
            moderation_actions: Arc::new(DashMap::new()),
            members: Arc::new(DashMap::new()),
            announcements: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(CommunitySettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl CommunityState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "community",
                "category": "community",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    fn current_settings(&self) -> CommunitySettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the events module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// True when the caller holds the manage capability.
fn can_manage(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_COMMUNITY_MANAGE)
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
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

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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
fn valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn slugify(name: &str) -> String {
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

fn load_category(state: &CommunityState, raw_id: &str) -> UnifiedResult<CommunityCategory> {
    let id = parse_uuid(raw_id, "category")?;
    state
        .categories
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Category not found: {id}")))
}

fn load_topic(state: &CommunityState, raw_id: &str) -> UnifiedResult<CommunityTopic> {
    let id = parse_uuid(raw_id, "topic")?;
    state
        .topics
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Topic not found: {id}")))
}

fn load_post(state: &CommunityState, raw_id: &str) -> UnifiedResult<CommunityPost> {
    let id = parse_uuid(raw_id, "post")?;
    state
        .posts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Post not found: {id}")))
}

fn load_flag(state: &CommunityState, raw_id: &str) -> UnifiedResult<CommunityFlag> {
    let id = parse_uuid(raw_id, "flag")?;
    state
        .flags
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Flag not found: {id}")))
}

fn load_announcement(
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
/// need the view capability, which every list/get handler in this
/// module already requires.
fn check_category_visible(
    ctx: &AppContext,
    role: &str,
    category: &CommunityCategory,
) -> UnifiedResult<()> {
    if category.visibility == CategoryVisibility::StaffOnly && !can_manage(ctx, role) {
        return Err(UnifiedError::not_found(format!(
            "Category not found: {}",
            category.id
        )));
    }
    Ok(())
}

/// A post is readable by its author, by anyone when it is not
/// hidden, and by moderators even when hidden.
fn post_visible_to(post: &CommunityPost, actor: &str, is_manager: bool) -> bool {
    !post.hidden || post.author_id == actor || is_manager
}

/// Record an audited moderator action.
fn record_moderation(
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
fn count_recent_topics(state: &CommunityState, author_id: &str, since: DateTime<Utc>) -> i64 {
    state
        .topics
        .iter()
        .filter(|entry| entry.value().author_id == author_id && entry.value().created_at >= since)
        .count() as i64
}

fn count_recent_posts(state: &CommunityState, author_id: &str, since: DateTime<Utc>) -> i64 {
    state
        .posts
        .iter()
        .filter(|entry| entry.value().author_id == author_id && entry.value().created_at >= since)
        .count() as i64
}

fn check_topic_rate_limit(state: &CommunityState, author_id: &str) -> UnifiedResult<()> {
    let limit = state.current_settings().max_topics_per_hour;
    let since = Utc::now() - Duration::hours(1);
    if count_recent_topics(state, author_id, since) >= limit {
        return Err(UnifiedError::bad_request(format!(
            "Topic rate limit reached: at most {limit} topics per hour"
        )));
    }
    Ok(())
}

fn check_post_rate_limit(state: &CommunityState, author_id: &str) -> UnifiedResult<()> {
    let limit = state.current_settings().max_posts_per_hour;
    let since = Utc::now() - Duration::hours(1);
    if count_recent_posts(state, author_id, since) >= limit {
        return Err(UnifiedError::bad_request(format!(
            "Post rate limit reached: at most {limit} posts per hour"
        )));
    }
    Ok(())
}

fn open_flag_count(state: &CommunityState, post_id: Uuid) -> i64 {
    state
        .flags
        .iter()
        .filter(|entry| entry.value().post_id == post_id && entry.value().status == "open")
        .count() as i64
}

fn vote_totals(state: &CommunityState, post_id: Uuid) -> (i64, i64) {
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

fn member_view(state: &CommunityState, member: &CommunityMember) -> Value {
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

fn topic_view(state: &CommunityState, topic: &CommunityTopic, actor: &str, is_manager: bool) -> Value {
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

fn post_view(state: &CommunityState, post: &CommunityPost) -> Value {
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

// ---------------------------------------------------------------------------
// Badge listener (gamification owns badges)
// ---------------------------------------------------------------------------

/// `gamification.badge.awarded` carries `user_id` (and `badge_id`).
/// The member's badge count is incremented when the member exists;
/// a non-member is ignored (joining the community later starts
/// from zero — badge history itself lives in gamification).
fn on_badge_awarded(state: CommunityState, ctx: &mut HookContext) {
    let Some(user_id) = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({ "community": "skipped", "reason": "gamification.badge.awarded carries no user_id" }));
        return;
    };
    let member_id = state
        .members
        .iter()
        .find(|entry| entry.value().user_id == user_id)
        .map(|entry| *entry.key());
    let Some(member_id) = member_id else {
        ctx.add_result(json!({ "community": "skipped", "user_id": user_id, "reason": "user is not a community member" }));
        return;
    };
    if let Some(mut member) = state.members.get_mut(&member_id) {
        member.badge_count += 1;
        member.updated_at = Utc::now();
        let count = member.badge_count;
        ctx.add_result(json!({ "community": "badge_count_updated", "user_id": user_id, "badge_count": count }));
    }
}

// ---------------------------------------------------------------------------
// Category handlers
// ---------------------------------------------------------------------------

async fn list_categories(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let visibility_filter = params.filters.get("visibility").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<CommunityCategory> = state
            .categories
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|category| {
                if category.visibility == CategoryVisibility::StaffOnly && !is_manager {
                    return false;
                }
                if let Some(visibility) = &visibility_filter {
                    if category.visibility.as_str() != visibility {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        category.name,
                        category.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|category| (category.sort_order, category.name.clone()));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    body: web::Json<CreateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityCategory> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let slug = match &input.slug {
            Some(raw) => {
                let slug = raw.trim().to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 128 characters)",
                    ));
                }
                slug
            }
            None => slugify(&name),
        };
        if state.categories.iter().any(|entry| entry.value().slug == slug) {
            return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
        }
        let now = Utc::now();
        let category = CommunityCategory {
            id: Uuid::now_v7(),
            name,
            description,
            slug,
            sort_order: input.sort_order.unwrap_or(0),
            visibility: input.visibility.unwrap_or_default(),
            created_at: now,
            updated_at: now,
        };
        state.categories.insert(category.id, category.clone());
        state.emit(
            "community.category.created",
            json!({
                "category_id": category.id.to_string(),
                "slug": category.slug,
                "name": category.name,
                "visibility": category.visibility.as_str(),
            }),
        );
        Ok(category)
    })();
    match result {
        Ok(category) => created(category),
        Err(error) => err(error, debug),
    }
}

async fn get_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityCategory> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let category = load_category(&state, &path.into_inner())?;
        check_category_visible(&ctx, &caller.role, &category)?;
        Ok(category)
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

async fn update_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCategoryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityCategory> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let mut category = load_category(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            category.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            category.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(slug) = &input.slug {
            let slug = slug.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only",
                ));
            }
            if state
                .categories
                .iter()
                .any(|entry| entry.value().slug == slug && entry.value().id != category.id)
            {
                return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
            }
            category.slug = slug;
        }
        if let Some(sort_order) = input.sort_order {
            category.sort_order = sort_order;
        }
        if let Some(visibility) = input.visibility {
            category.visibility = visibility;
        }
        category.updated_at = Utc::now();
        state.categories.insert(category.id, category.clone());
        state.emit(
            "community.category.updated",
            json!({ "category_id": category.id.to_string(), "slug": category.slug }),
        );
        Ok(category)
    })();
    match result {
        Ok(category) => ok(category),
        Err(error) => err(error, debug),
    }
}

async fn delete_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let category = load_category(&state, &path.into_inner())?;
        if state
            .topics
            .iter()
            .any(|entry| entry.value().category_id == category.id)
        {
            return Err(UnifiedError::conflict(
                "A category with topics cannot be deleted; move or delete its topics first",
            ));
        }
        let id = category.id;
        state.categories.remove(&id);
        state.emit(
            "community.category.deleted",
            json!({ "category_id": id.to_string(), "slug": category.slug }),
        );
        Ok(json!({ "deleted": true, "category_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Topic handlers
// ---------------------------------------------------------------------------

async fn list_topics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let category_filter = params
            .filters
            .get("category_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let tag_filter = params.filters.get("tag").cloned();
        let pinned_filter = params
            .filters
            .get("pinned")
            .and_then(|raw| raw.parse::<bool>().ok());
        let author_filter = params.filters.get("author_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .topics
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|topic| {
                let Some(category) = state.categories.get(&topic.category_id) else {
                    return false;
                };
                if category.visibility == CategoryVisibility::StaffOnly && !is_manager {
                    return false;
                }
                if let Some(category_id) = category_filter {
                    if topic.category_id != category_id {
                        return false;
                    }
                }
                if let Some(tag) = &tag_filter {
                    if !topic.tags.iter().any(|entry| entry == tag) {
                        return false;
                    }
                }
                if let Some(pinned) = pinned_filter {
                    if topic.pinned != pinned {
                        return false;
                    }
                }
                if let Some(author) = &author_filter {
                    if topic.author_id != *author {
                        return false;
                    }
                }
                if !search.is_empty() && !topic.title.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .map(|topic| topic_view(&state, &topic, &caller.actor, is_manager))
            .collect();
        let sort = params.allowed_sort(&["title", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a
                    .get("title")
                    .and_then(Value::as_str)
                    .cmp(&b.get("title").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        // Pinned topics sort first within the same page order.
        items.sort_by_key(|item| {
            !item
                .get("pinned")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    body: web::Json<CreateTopicRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityTopic> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let input = body.into_inner();
        let category = state
            .categories
            .get(&input.category_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Category not found: {}", input.category_id))
            })?;
        check_category_visible(&ctx, &caller.role, &category)?;
        check_topic_rate_limit(&state, &caller.actor)?;
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        if input.tags.len() > MAX_TAGS {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_TAGS} tags are allowed"
            )));
        }
        let mut tags: Vec<String> = Vec::new();
        for raw in &input.tags {
            tags.push(require_text(raw, "tag", MAX_TAG_LEN)?);
        }
        let now = Utc::now();
        let topic = CommunityTopic {
            id: Uuid::now_v7(),
            category_id: category.id,
            title,
            author_id: caller.actor.clone(),
            tags,
            pinned: false,
            locked: false,
            created_at: now,
            updated_at: now,
        };
        state.topics.insert(topic.id, topic.clone());
        state.emit(
            "community.topic.created",
            json!({
                "topic_id": topic.id.to_string(),
                "category_id": topic.category_id.to_string(),
                "title": topic.title,
                "author_id": topic.author_id,
            }),
        );
        Ok(topic)
    })();
    match result {
        Ok(topic) => created(topic),
        Err(error) => err(error, debug),
    }
}

async fn get_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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
        check_category_visible(&ctx, &caller.role, &category)?;
        Ok(topic_view(&state, &topic, &caller.actor, is_manager))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTopicRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityTopic> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let mut topic = load_topic(&state, &path.into_inner())?;
        if topic.author_id != caller.actor && !is_manager {
            return Err(UnifiedError::forbidden(
                "Only the topic author or a moderator can update this topic",
            ));
        }
        let input = body.into_inner();
        if let Some(title) = &input.title {
            topic.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(category_id) = input.category_id {
            let category = state
                .categories
                .get(&category_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Category not found: {category_id}")))?;
            check_category_visible(&ctx, &caller.role, &category)?;
            topic.category_id = category_id;
        }
        if let Some(tags) = &input.tags {
            if tags.len() > MAX_TAGS {
                return Err(UnifiedError::bad_request(format!(
                    "at most {MAX_TAGS} tags are allowed"
                )));
            }
            let mut checked: Vec<String> = Vec::new();
            for raw in tags {
                checked.push(require_text(raw, "tag", MAX_TAG_LEN)?);
            }
            topic.tags = checked;
        }
        topic.updated_at = Utc::now();
        state.topics.insert(topic.id, topic.clone());
        state.emit(
            "community.topic.updated",
            json!({ "topic_id": topic.id.to_string(), "title": topic.title }),
        );
        Ok(topic)
    })();
    match result {
        Ok(topic) => ok(topic),
        Err(error) => err(error, debug),
    }
}

async fn delete_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let topic = load_topic(&state, &path.into_inner())?;
        if topic.author_id != caller.actor && !is_manager {
            return Err(UnifiedError::forbidden(
                "Only the topic author or a moderator can delete this topic",
            ));
        }
        let id = topic.id;
        state.topics.remove(&id);
        let stale_posts: Vec<Uuid> = state
            .posts
            .iter()
            .filter(|entry| entry.value().topic_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in &stale_posts {
            state.posts.remove(key);
        }
        let stale_votes: Vec<Uuid> = state
            .votes
            .iter()
            .filter(|entry| {
                stale_posts.contains(&entry.value().post_id)
            })
            .map(|entry| *entry.key())
            .collect();
        for key in stale_votes {
            state.votes.remove(&key);
        }
        let stale_flags: Vec<Uuid> = state
            .flags
            .iter()
            .filter(|entry| {
                stale_posts.contains(&entry.value().post_id)
            })
            .map(|entry| *entry.key())
            .collect();
        for key in stale_flags {
            state.flags.remove(&key);
        }
        state.emit(
            "community.topic.deleted",
            json!({ "topic_id": id.to_string(), "title": topic.title }),
        );
        Ok(json!({ "deleted": true, "topic_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn pin_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityTopic> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let mut topic = load_topic(&state, &path.into_inner())?;
        topic.pinned = !topic.pinned;
        topic.updated_at = Utc::now();
        state.topics.insert(topic.id, topic.clone());
        record_moderation(
            &state,
            "topic",
            topic.id,
            if topic.pinned { "pin" } else { "unpin" },
            &caller.actor,
            None,
        );
        state.emit(
            "community.topic.pinned",
            json!({ "topic_id": topic.id.to_string(), "pinned": topic.pinned }),
        );
        Ok(topic)
    })();
    match result {
        Ok(topic) => ok(topic),
        Err(error) => err(error, debug),
    }
}

async fn lock_topic(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityTopic> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let mut topic = load_topic(&state, &path.into_inner())?;
        topic.locked = !topic.locked;
        topic.updated_at = Utc::now();
        state.topics.insert(topic.id, topic.clone());
        record_moderation(
            &state,
            "topic",
            topic.id,
            if topic.locked { "lock" } else { "unlock" },
            &caller.actor,
            None,
        );
        state.emit(
            "community.topic.locked",
            json!({ "topic_id": topic.id.to_string(), "locked": topic.locked }),
        );
        Ok(topic)
    })();
    match result {
        Ok(topic) => ok(topic),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Post handlers
// ---------------------------------------------------------------------------

async fn list_topic_posts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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
        check_category_visible(&ctx, &caller.role, &category)?;
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

async fn create_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePostRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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
        check_category_visible(&ctx, &caller.role, &category)?;
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

async fn get_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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
        check_category_visible(&ctx, &caller.role, &category)?;
        Ok(post_view(&state, &post))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePostRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn delete_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

// ---------------------------------------------------------------------------
// Vote handlers
// ---------------------------------------------------------------------------

async fn vote_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<VoteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn get_post_votes(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

// ---------------------------------------------------------------------------
// Flag + moderation handlers
// ---------------------------------------------------------------------------

async fn flag_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FlagRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn list_flags(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn review_flag(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewFlagRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn hide_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ModerationReasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn unhide_post(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ModerationReasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn list_moderation_actions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

// ---------------------------------------------------------------------------
// Member handlers
// ---------------------------------------------------------------------------

async fn list_members(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .members
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|member| {
                if !search.is_empty() && !member.display_name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .map(|member| member_view(&state, &member))
            .collect();
        let sort = params.allowed_sort(&["display_name", "joined_at", "badge_count"], "joined_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "display_name" => a
                    .get("display_name")
                    .and_then(Value::as_str)
                    .cmp(&b.get("display_name").and_then(Value::as_str)),
                "badge_count" => a
                    .get("badge_count")
                    .and_then(Value::as_i64)
                    .cmp(&b.get("badge_count").and_then(Value::as_i64)),
                _ => a
                    .get("joined_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("joined_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn join_community(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    body: web::Json<JoinRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        if state
            .members
            .iter()
            .any(|entry| entry.value().user_id == caller.actor)
        {
            return Err(UnifiedError::conflict(
                "You are already a community member",
            ));
        }
        let display_name = require_text(&body.into_inner().display_name, "display_name", MAX_NAME_LEN)?;
        let now = Utc::now();
        let member = CommunityMember {
            id: Uuid::now_v7(),
            user_id: caller.actor.clone(),
            display_name,
            badge_count: 0,
            joined_at: now,
            created_at: now,
            updated_at: now,
        };
        state.members.insert(member.id, member.clone());
        state.emit(
            "community.member.joined",
            json!({
                "member_id": member.id.to_string(),
                "user_id": member.user_id,
                "display_name": member.display_name,
            }),
        );
        Ok(member_view(&state, &member))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_member(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let user_id = path.into_inner();
        let member = state
            .members
            .iter()
            .map(|entry| entry.value().clone())
            .find(|member| member.user_id == user_id)
            .ok_or_else(|| UnifiedError::not_found(format!("Member not found: {user_id}")))?;
        Ok(member_view(&state, &member))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Announcement handlers
// ---------------------------------------------------------------------------

async fn list_announcements(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let published_filter = params
            .filters
            .get("published")
            .and_then(|raw| raw.parse::<bool>().ok());
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<CommunityAnnouncement> = state
            .announcements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|announcement| {
                // Drafts are only visible to moderators.
                if !announcement.published && !is_manager {
                    return false;
                }
                if let Some(published) = published_filter {
                    if announcement.published != published {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!("{} {}", announcement.title, announcement.body)
                        .to_lowercase()
                        .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|announcement| {
            (
                std::cmp::Reverse(announcement.pinned),
                std::cmp::Reverse(announcement.created_at),
            )
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    body: web::Json<CreateAnnouncementRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityAnnouncement> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let body_text = require_text(&input.body, "body", MAX_BODY_LEN)?;
        let now = Utc::now();
        let announcement = CommunityAnnouncement {
            id: Uuid::now_v7(),
            title,
            body: body_text,
            pinned: input.pinned,
            published: false,
            published_at: None,
            author_id: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.announcements.insert(announcement.id, announcement.clone());
        state.emit(
            "community.announcement.created",
            json!({
                "announcement_id": announcement.id.to_string(),
                "title": announcement.title,
                "author_id": announcement.author_id,
            }),
        );
        Ok(announcement)
    })();
    match result {
        Ok(announcement) => created(announcement),
        Err(error) => err(error, debug),
    }
}

async fn get_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityAnnouncement> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let announcement = load_announcement(&state, &path.into_inner())?;
        if !announcement.published && !is_manager {
            return Err(UnifiedError::not_found(format!(
                "Announcement not found: {}",
                announcement.id
            )));
        }
        Ok(announcement)
    })();
    match result {
        Ok(announcement) => ok(announcement),
        Err(error) => err(error, debug),
    }
}

async fn update_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAnnouncementRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityAnnouncement> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let mut announcement = load_announcement(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            announcement.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(body_text) = &input.body {
            announcement.body = require_text(body_text, "body", MAX_BODY_LEN)?;
        }
        if let Some(pinned) = input.pinned {
            announcement.pinned = pinned;
        }
        announcement.updated_at = Utc::now();
        state
            .announcements
            .insert(announcement.id, announcement.clone());
        Ok(announcement)
    })();
    match result {
        Ok(announcement) => ok(announcement),
        Err(error) => err(error, debug),
    }
}

async fn delete_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let announcement = load_announcement(&state, &path.into_inner())?;
        let id = announcement.id;
        state.announcements.remove(&id);
        Ok(json!({ "deleted": true, "announcement_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn publish_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityAnnouncement> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let mut announcement = load_announcement(&state, &path.into_inner())?;
        if announcement.published {
            return Err(UnifiedError::conflict("This announcement is already published"));
        }
        let now = Utc::now();
        announcement.published = true;
        announcement.published_at = Some(now);
        announcement.updated_at = now;
        state
            .announcements
            .insert(announcement.id, announcement.clone());
        state.emit(
            "community.announcement.published",
            json!({
                "announcement_id": announcement.id.to_string(),
                "title": announcement.title,
                "author_id": announcement.author_id,
                "published_at": now.to_rfc3339(),
            }),
        );
        // Notify every member through the notifications module.
        let members: Vec<CommunityMember> = state
            .members
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        for member in members {
            state.notify(
                "community.announcement.published",
                &member.user_id,
                &announcement.title,
                &announcement.body,
                Some(format!("/api/v1/community/announcements/{}", announcement.id)),
            );
        }
        Ok(announcement)
    })();
    match result {
        Ok(announcement) => ok(announcement),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Settings handlers
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunitySettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        Ok(state.current_settings())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunitySettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.current_settings();
        if let Some(value) = input.max_topics_per_hour {
            if value < 1 || value > 1000 {
                return Err(UnifiedError::bad_request(
                    "max_topics_per_hour must be between 1 and 1000",
                ));
            }
            settings.max_topics_per_hour = value;
        }
        if let Some(value) = input.max_posts_per_hour {
            if value < 1 || value > 10000 {
                return Err(UnifiedError::bad_request(
                    "max_posts_per_hour must be between 1 and 10000",
                ));
            }
            settings.max_posts_per_hour = value;
        }
        if let Some(value) = input.spam_flag_threshold {
            if value < 1 || value > 100 {
                return Err(UnifiedError::bad_request(
                    "spam_flag_threshold must be between 1 and 100",
                ));
            }
            settings.spam_flag_threshold = value;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public handlers (no auth — gated by the API Manager's ApiAccessRule
// for each path; default deny. See the module header.)
// ---------------------------------------------------------------------------

async fn list_public_topics(
    state: web::Data<CommunityState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let search = params.search.clone().unwrap_or_default().to_lowercase();
    let mut items: Vec<Value> = state
        .topics
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|topic| {
            let Some(category) = state.categories.get(&topic.category_id) else {
                return false;
            };
            if category.visibility != CategoryVisibility::Public {
                return false;
            }
            if !search.is_empty() && !topic.title.to_lowercase().contains(&search) {
                return false;
            }
            true
        })
        .map(|topic| {
            let mut value = serde_json::to_value(&topic).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut value {
                let post_count = state
                    .posts
                    .iter()
                    .filter(|entry| entry.value().topic_id == topic.id && !entry.value().hidden)
                    .count();
                map.insert("post_count".to_string(), Value::from(post_count));
            }
            value
        })
        .collect();
    items.sort_by(|a, b| {
        let pinned_a = a.get("pinned").and_then(Value::as_bool).unwrap_or(false);
        let pinned_b = b.get("pinned").and_then(Value::as_bool).unwrap_or(false);
        pinned_b.cmp(&pinned_a).then(
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str)),
        )
    });
    ok(paginate(items, &params))
}

async fn get_public_topic(state: web::Data<CommunityState>, path: web::Path<String>) -> HttpResponse {
    let topic = match load_topic(&state, &path.into_inner()) {
        Ok(topic) => topic,
        Err(error) => return err(error, false),
    };
    let category = match state.categories.get(&topic.category_id) {
        Some(entry) => entry.value().clone(),
        None => {
            return err(
                UnifiedError::not_found(format!("Topic not found: {}", topic.id)),
                false,
            )
        }
    };
    if category.visibility != CategoryVisibility::Public {
        return err(
            UnifiedError::not_found(format!("Topic not found: {}", topic.id)),
            false,
        );
    }
    let mut value = serde_json::to_value(&topic).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let posts: Vec<Value> = {
            let mut rows: Vec<CommunityPost> = state
                .posts
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|post| post.topic_id == topic.id && !post.hidden)
                .collect();
            rows.sort_by_key(|post| post.created_at);
            rows.into_iter().map(|post| post_view(&state, &post)).collect()
        };
        map.insert("posts".to_string(), Value::Array(posts));
    }
    ok(value)
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct CommunityModule {
    state: CommunityState,
}

impl CommunityModule {
    pub fn new() -> Self {
        Self {
            state: CommunityState::default(),
        }
    }
}

impl Default for CommunityModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CommunityModule {
    fn id(&self) -> &'static str {
        "community"
    }

    fn name(&self) -> &'static str {
        "Community / Forum"
    }

    fn description(&self) -> &'static str {
        "Community forum with categories (public, members-only, staff-only), topics with pin and lock, posts with one-level nested replies and kept edit history, one vote per user per post, flag-based moderation with an audited action log and spam auto-hide, a member directory, announcements that notify members, and configurable rate limits. Badges are owned by the gamification module; this module only tracks badge counts."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "gamification"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "gamification"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            GAMIFICATION_BADGE_AWARDED,
            "community",
            Arc::new(move |ctx: &mut HookContext| {
                on_badge_awarded(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/community")
                .app_data(state)
                .route("/public/topics", web::get().to(list_public_topics))
                .route("/public/topics/{id}", web::get().to(get_public_topic))
                .route("/categories", web::get().to(list_categories))
                .route("/categories", web::post().to(create_category))
                .route("/categories/{id}", web::get().to(get_category))
                .route("/categories/{id}", web::patch().to(update_category))
                .route("/categories/{id}", web::delete().to(delete_category))
                .route("/topics", web::get().to(list_topics))
                .route("/topics", web::post().to(create_topic))
                .route("/topics/{id}", web::get().to(get_topic))
                .route("/topics/{id}", web::patch().to(update_topic))
                .route("/topics/{id}", web::delete().to(delete_topic))
                .route("/topics/{id}/pin", web::post().to(pin_topic))
                .route("/topics/{id}/lock", web::post().to(lock_topic))
                .route("/topics/{id}/posts", web::get().to(list_topic_posts))
                .route("/topics/{id}/posts", web::post().to(create_post))
                .route("/posts/{id}", web::get().to(get_post))
                .route("/posts/{id}", web::patch().to(update_post))
                .route("/posts/{id}", web::delete().to(delete_post))
                .route("/posts/{id}/vote", web::post().to(vote_post))
                .route("/posts/{id}/votes", web::get().to(get_post_votes))
                .route("/posts/{id}/flag", web::post().to(flag_post))
                .route("/posts/{id}/hide", web::post().to(hide_post))
                .route("/posts/{id}/unhide", web::post().to(unhide_post))
                .route("/flags", web::get().to(list_flags))
                .route("/flags/{id}/review", web::post().to(review_flag))
                .route("/moderation-actions", web::get().to(list_moderation_actions))
                .route("/members", web::get().to(list_members))
                .route("/members", web::post().to(join_community))
                .route("/members/{user_id}", web::get().to(get_member))
                .route("/announcements", web::get().to(list_announcements))
                .route("/announcements", web::post().to(create_announcement))
                .route("/announcements/{id}", web::get().to(get_announcement))
                .route("/announcements/{id}", web::patch().to(update_announcement))
                .route("/announcements/{id}", web::delete().to(delete_announcement))
                .route(
                    "/announcements/{id}/publish",
                    web::post().to(publish_announcement),
                )
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/community/categories", owner: "community", description: "List categories (paginated, filter by visibility; staff-only categories hidden without the manage capability)." },
            RouteInfo { method: "POST", path: "/api/v1/community/categories", owner: "community", description: "Create a category (order, visibility public/members-only/staff-only)." },
            RouteInfo { method: "GET", path: "/api/v1/community/categories/{id}", owner: "community", description: "Get one category." },
            RouteInfo { method: "PATCH", path: "/api/v1/community/categories/{id}", owner: "community", description: "Update a category." },
            RouteInfo { method: "DELETE", path: "/api/v1/community/categories/{id}", owner: "community", description: "Delete a category with no topics." },
            RouteInfo { method: "GET", path: "/api/v1/community/topics", owner: "community", description: "List topics (paginated, filter by category_id, tag, pinned, author_id; pinned first)." },
            RouteInfo { method: "POST", path: "/api/v1/community/topics", owner: "community", description: "Create a topic (rate-limited per user per hour)." },
            RouteInfo { method: "GET", path: "/api/v1/community/topics/{id}", owner: "community", description: "Get one topic with its visible post count." },
            RouteInfo { method: "PATCH", path: "/api/v1/community/topics/{id}", owner: "community", description: "Update a topic (author or moderator)." },
            RouteInfo { method: "DELETE", path: "/api/v1/community/topics/{id}", owner: "community", description: "Delete a topic with its posts, votes, and flags (author or moderator)." },
            RouteInfo { method: "POST", path: "/api/v1/community/topics/{id}/pin", owner: "community", description: "Toggle the pinned flag of a topic (moderator, audited)." },
            RouteInfo { method: "POST", path: "/api/v1/community/topics/{id}/lock", owner: "community", description: "Toggle the locked flag of a topic (moderator, audited)." },
            RouteInfo { method: "GET", path: "/api/v1/community/topics/{id}/posts", owner: "community", description: "List posts of a topic (paginated, filter by author_id, parent_post_id; hidden posts only for moderators and the author)." },
            RouteInfo { method: "POST", path: "/api/v1/community/topics/{id}/posts", owner: "community", description: "Create a post or a one-level reply (parent_post_id must be a top-level post; rate-limited; locked topics reject posts)." },
            RouteInfo { method: "GET", path: "/api/v1/community/posts/{id}", owner: "community", description: "Get one post with vote totals and edit history." },
            RouteInfo { method: "PATCH", path: "/api/v1/community/posts/{id}", owner: "community", description: "Edit a post (author or moderator); the previous body is kept in the edit history." },
            RouteInfo { method: "DELETE", path: "/api/v1/community/posts/{id}", owner: "community", description: "Delete a post with its replies, votes, and flags (author or moderator)." },
            RouteInfo { method: "POST", path: "/api/v1/community/posts/{id}/vote", owner: "community", description: "Vote up (+1) or down (-1) on a post; one vote per user, changing it updates the row." },
            RouteInfo { method: "GET", path: "/api/v1/community/posts/{id}/votes", owner: "community", description: "Vote totals of a post plus the caller's own vote." },
            RouteInfo { method: "POST", path: "/api/v1/community/posts/{id}/flag", owner: "community", description: "Flag a post with a reason; reaching the spam threshold auto-hides it pending review." },
            RouteInfo { method: "POST", path: "/api/v1/community/posts/{id}/hide", owner: "community", description: "Hide a post (moderator, audited)." },
            RouteInfo { method: "POST", path: "/api/v1/community/posts/{id}/unhide", owner: "community", description: "Unhide a post (moderator, audited)." },
            RouteInfo { method: "GET", path: "/api/v1/community/flags", owner: "community", description: "List flags, open first (moderator)." },
            RouteInfo { method: "POST", path: "/api/v1/community/flags/{id}/review", owner: "community", description: "Review a flag: dismiss it or hide the post (moderator, audited)." },
            RouteInfo { method: "GET", path: "/api/v1/community/moderation-actions", owner: "community", description: "Audited moderator action log (moderator)." },
            RouteInfo { method: "GET", path: "/api/v1/community/members", owner: "community", description: "Member directory: display name, join date, badge counts, activity counts (never emails or private data)." },
            RouteInfo { method: "POST", path: "/api/v1/community/members", owner: "community", description: "Join the community (one row per user)." },
            RouteInfo { method: "GET", path: "/api/v1/community/members/{user_id}", owner: "community", description: "Get one member from the directory." },
            RouteInfo { method: "GET", path: "/api/v1/community/announcements", owner: "community", description: "List announcements (drafts only for moderators; pinned first)." },
            RouteInfo { method: "POST", path: "/api/v1/community/announcements", owner: "community", description: "Create a draft announcement (moderator)." },
            RouteInfo { method: "GET", path: "/api/v1/community/announcements/{id}", owner: "community", description: "Get one announcement." },
            RouteInfo { method: "PATCH", path: "/api/v1/community/announcements/{id}", owner: "community", description: "Update a draft announcement (moderator)." },
            RouteInfo { method: "DELETE", path: "/api/v1/community/announcements/{id}", owner: "community", description: "Delete an announcement (moderator)." },
            RouteInfo { method: "POST", path: "/api/v1/community/announcements/{id}/publish", owner: "community", description: "Publish an announcement and notify every member (moderator)." },
            RouteInfo { method: "GET", path: "/api/v1/community/settings", owner: "community", description: "Get rate limits and the spam flag threshold (moderator)." },
            RouteInfo { method: "PATCH", path: "/api/v1/community/settings", owner: "community", description: "Update rate limits and the spam flag threshold (moderator)." },
            RouteInfo { method: "GET", path: "/api/v1/community/public/topics", owner: "community", description: "PUBLIC: topics in public categories (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "GET", path: "/api/v1/community/public/topics/{id}", owner: "community", description: "PUBLIC: one public topic with its visible posts (no auth; gated by the API Manager ApiAccessRule)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CommunityModule::new()));
}
