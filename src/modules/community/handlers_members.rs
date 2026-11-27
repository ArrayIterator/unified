//! HTTP handlers for members, announcements, settings and public topics.

use super::helpers::{
    caller, caller_company, can_manage, created, err, load_announcement, load_topic, member_view,
    ok, paginate, post_view, require_permission, require_tenant, require_text,
};
use super::state::CommunityState;
use super::types::{
    CategoryVisibility, CommunityAnnouncement, CommunityMember, CommunityPost, CommunitySettings,
    CreateAnnouncementRequest, JoinRequest, MAX_BODY_LEN, MAX_NAME_LEN, MAX_TITLE_LEN,
    PERM_COMMUNITY_MANAGE, PERM_COMMUNITY_VIEW, UpdateAnnouncementRequest, UpdateSettingsRequest,
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

// Member handlers

pub(crate) async fn list_members(
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

pub(crate) async fn join_community(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    body: web::Json<JoinRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn get_member(
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

// Announcement handlers

pub(crate) async fn list_announcements(
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

pub(crate) async fn create_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    body: web::Json<CreateAnnouncementRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityAnnouncement> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn get_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityAnnouncement> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn update_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAnnouncementRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityAnnouncement> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

pub(crate) async fn delete_announcement(
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

pub(crate) async fn publish_announcement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunityAnnouncement> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

// Settings handlers

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunitySettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMMUNITY_MANAGE)?;
        Ok(state.current_settings())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CommunityState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommunitySettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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

// Public handlers (no auth - gated by the API Manager's ApiAccessRule
// for each path; default deny. See the module header.)

pub(crate) async fn list_public_topics(
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

pub(crate) async fn get_public_topic(state: web::Data<CommunityState>, path: web::Path<String>) -> HttpResponse {
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
