//! HTTP handlers for report review, the moderation audit log,
//! forum settings, and reputation.
//!
//! Moderation endpoints require the moderate capability (managers
//! hold it implicitly through `can_moderate`); reputation reads
//! are open to any authenticated caller.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;

use super::helpers::{
    caller, can_moderate, err, load_post, load_report, load_thread, ok, paginate,
    recompute_reputation, record_moderation, require_text, require_tenant, resolve_tenant,
};
use super::state::ForumState;
use super::types::{ForumSettings, MAX_REASON_LEN, ReviewReportRequest, UpdateSettingsRequest};

// Reports

pub(crate) async fn list_reports(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        if !can_moderate(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(
                "Role does not hold permission \"forums.moderate\"",
            ));
        }
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Value> = state
            .reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|report| {
                report.tenant_id == tenant_id
                    && status_filter
                        .as_ref()
                        .map(|status| report.status == *status)
                        .unwrap_or(true)
            })
            .map(|report| serde_json::to_value(&report).unwrap_or(Value::Null))
            .collect();
        // Open reports first, then newest.
        items.sort_by(|a, b| {
            let open_a = a.get("status").and_then(Value::as_str) == Some("open");
            let open_b = b.get("status").and_then(Value::as_str) == Some("open");
            open_b.cmp(&open_a).then(
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

pub(crate) async fn review_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        if !can_moderate(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(
                "Role does not hold permission \"forums.moderate\"",
            ));
        }
        let mut report = load_report(&state, &tenant_id, &path.into_inner())?;
        if report.status != "open" {
            return Err(UnifiedError::conflict(format!(
                "Only an open report can be reviewed (status is \"{}\")",
                report.status
            )));
        }
        let input = body.into_inner();
        let action = input.action.trim().to_lowercase();
        if action != "dismiss" && action != "hide" && action != "delete" {
            return Err(UnifiedError::bad_request(
                "action must be \"dismiss\", \"hide\", or \"delete\"",
            ));
        }
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let mut target_hidden = false;
        let mut target_deleted = false;
        match action.as_str() {
            "dismiss" => {
                report.status = "dismissed".to_string();
                record_moderation(
                    &state,
                    &tenant_id,
                    "report",
                    report.id,
                    "dismiss",
                    &caller.actor,
                    reason.clone(),
                );
            }
            "hide" => {
                if report.target_type == "thread" {
                    if let Ok(thread) =
                        load_thread(&state, &tenant_id, &report.target_id.to_string())
                    {
                        if !thread.locked {
                            let mut locked = thread.clone();
                            locked.locked = true;
                            locked.updated_at = now;
                            state.threads.insert(locked.id, locked);
                            target_hidden = true;
                        }
                    }
                } else {
                    if let Ok(post) = load_post(&state, &tenant_id, &report.target_id.to_string()) {
                        if !post.hidden {
                            let mut hidden = post.clone();
                            hidden.hidden = true;
                            hidden.updated_at = now;
                            state.posts.insert(hidden.id, hidden);
                            target_hidden = true;
                        }
                    }
                }
                report.status = "actioned".to_string();
                record_moderation(
                    &state,
                    &tenant_id,
                    &report.target_type,
                    report.target_id,
                    "hide",
                    &caller.actor,
                    reason.clone(),
                );
            }
            "delete" => {
                if report.target_type == "thread" {
                    if let Ok(thread) =
                        load_thread(&state, &tenant_id, &report.target_id.to_string())
                    {
                        delete_thread_cascade(&state, &thread.id);
                        target_deleted = true;
                    }
                } else if let Ok(post) =
                    load_post(&state, &tenant_id, &report.target_id.to_string())
                {
                    delete_post_cascade(&state, &post.id);
                    target_deleted = true;
                }
                report.status = "actioned".to_string();
                record_moderation(
                    &state,
                    &tenant_id,
                    &report.target_type,
                    report.target_id,
                    "delete",
                    &caller.actor,
                    reason.clone(),
                );
            }
            _ => unreachable!("action validated above"),
        }
        report.reviewed_by = Some(caller.actor.clone());
        report.reviewed_at = Some(now);
        report.updated_at = now;
        state.reports.insert(report.id, report.clone());
        state.emit(
            "forums.report.reviewed",
            json!({
                "report_id": report.id.to_string(),
                "target_type": report.target_type,
                "target_id": report.target_id.to_string(),
                "action": action,
                "status": report.status,
                "reviewed_by": caller.actor,
                "target_hidden": target_hidden,
                "target_deleted": target_deleted,
            }),
        );
        Ok(json!({
            "report_id": report.id.to_string(),
            "target_type": report.target_type,
            "target_id": report.target_id.to_string(),
            "action": action,
            "status": report.status,
            "target_hidden": target_hidden,
            "target_deleted": target_deleted,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a thread and everything attached to it: its posts (and
/// their votes), its poll (options and votes), and its
/// subscriptions.
fn delete_thread_cascade(state: &ForumState, thread_id: &uuid::Uuid) {
    let post_ids: Vec<uuid::Uuid> = state
        .posts
        .iter()
        .filter(|entry| entry.value().thread_id == *thread_id)
        .map(|entry| *entry.key())
        .collect();
    for post_id in post_ids {
        delete_post_cascade(state, &post_id);
    }
    let poll_ids: Vec<uuid::Uuid> = state
        .polls
        .iter()
        .filter(|entry| entry.value().thread_id == *thread_id)
        .map(|entry| *entry.key())
        .collect();
    for poll_id in poll_ids {
        let option_ids: Vec<uuid::Uuid> = state
            .poll_options
            .iter()
            .filter(|entry| entry.value().poll_id == poll_id)
            .map(|entry| *entry.key())
            .collect();
        for option_id in option_ids {
            state.poll_options.remove(&option_id);
        }
        let vote_ids: Vec<uuid::Uuid> = state
            .poll_votes
            .iter()
            .filter(|entry| entry.value().poll_id == poll_id)
            .map(|entry| *entry.key())
            .collect();
        for vote_id in vote_ids {
            state.poll_votes.remove(&vote_id);
        }
        state.polls.remove(&poll_id);
    }
    let subscription_ids: Vec<uuid::Uuid> = state
        .subscriptions
        .iter()
        .filter(|entry| {
            entry.value().target_type == "thread" && entry.value().target_id == *thread_id
        })
        .map(|entry| *entry.key())
        .collect();
    for subscription_id in subscription_ids {
        state.subscriptions.remove(&subscription_id);
    }
    state.threads.remove(thread_id);
}

/// Delete a post and the votes cast on it.
fn delete_post_cascade(state: &ForumState, post_id: &uuid::Uuid) {
    let vote_ids: Vec<uuid::Uuid> = state
        .votes
        .iter()
        .filter(|entry| entry.value().post_id == *post_id)
        .map(|entry| *entry.key())
        .collect();
    for vote_id in vote_ids {
        state.votes.remove(&vote_id);
    }
    state.posts.remove(post_id);
}

// Moderation audit log

pub(crate) async fn list_moderation_actions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        if !can_moderate(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(
                "Role does not hold permission \"forums.moderate\"",
            ));
        }
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .moderation_actions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|action| action.tenant_id == tenant_id)
            .map(|action| serde_json::to_value(&action).unwrap_or(Value::Null))
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

// Settings

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ForumSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _ = tenant_id;
        let caller = caller(&req)?;
        if !can_moderate(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(
                "Role does not hold permission \"forums.moderate\"",
            ));
        }
        Ok(state.current_settings())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ForumSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _ = tenant_id;
        let caller = caller(&req)?;
        if !can_moderate(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(
                "Role does not hold permission \"forums.moderate\"",
            ));
        }
        let input = body.into_inner();
        let mut settings = state.current_settings();
        if let Some(value) = input.max_threads_per_hour {
            if value <= 0 {
                return Err(UnifiedError::bad_request(
                    "max_threads_per_hour must be greater than 0",
                ));
            }
            settings.max_threads_per_hour = value;
        }
        if let Some(value) = input.max_posts_per_hour {
            if value <= 0 {
                return Err(UnifiedError::bad_request(
                    "max_posts_per_hour must be greater than 0",
                ));
            }
            settings.max_posts_per_hour = value;
        }
        if let Some(value) = input.spam_report_threshold {
            if value <= 0 {
                return Err(UnifiedError::bad_request(
                    "spam_report_threshold must be greater than 0",
                ));
            }
            settings.spam_report_threshold = value;
        }
        if let Some(value) = input.new_user_link_limit {
            if value < 0 {
                return Err(UnifiedError::bad_request(
                    "new_user_link_limit must be 0 or greater",
                ));
            }
            settings.new_user_link_limit = value;
        }
        if let Some(value) = input.new_user_age_hours {
            if value < 0 {
                return Err(UnifiedError::bad_request(
                    "new_user_age_hours must be 0 or greater",
                ));
            }
            settings.new_user_age_hours = value;
        }
        if let Some(value) = input.max_attachments_per_post {
            if value < 0 {
                return Err(UnifiedError::bad_request(
                    "max_attachments_per_post must be 0 or greater",
                ));
            }
            settings.max_attachments_per_post = value;
        }
        *state.settings.write().expect("settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Reputation

#[derive(Debug, Deserialize)]
struct LeaderboardQuery {
    #[serde(default)]
    limit: Option<i64>,
}

pub(crate) async fn get_reputation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let user_id = path.into_inner();
        // A user with no authored threads or posts in this tenant
        // has no forum activity, so there is no reputation to show.
        let has_activity = state.threads.iter().any(|entry| {
            entry.value().tenant_id == tenant_id && entry.value().author_id == user_id
        }) || state.posts.iter().any(|entry| {
            entry.value().tenant_id == tenant_id && entry.value().author_id == user_id
        });
        if !has_activity {
            return Err(UnifiedError::not_found(format!(
                "No forum reputation for user {user_id}"
            )));
        }
        recompute_reputation(&state, &tenant_id, &user_id);
        let row = state
            .reputations
            .iter()
            .map(|entry| entry.value().clone())
            .find(|row| row.tenant_id == tenant_id && row.user_id == user_id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("No forum reputation for user {user_id}"))
            })?;
        Ok(serde_json::to_value(&row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_reputation_leaderboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    query: web::Query<LeaderboardQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let limit = query.into_inner().limit.unwrap_or(20).clamp(1, 100);
        let mut rows: Vec<Value> = state
            .reputations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.tenant_id == tenant_id)
            .map(|row| serde_json::to_value(&row).unwrap_or(Value::Null))
            .collect();
        rows.sort_by(|a, b| {
            let score_a = a.get("score").and_then(Value::as_i64).unwrap_or(0);
            let score_b = b.get("score").and_then(Value::as_i64).unwrap_or(0);
            score_b.cmp(&score_a).then(
                a.get("user_id")
                    .and_then(Value::as_str)
                    .cmp(&b.get("user_id").and_then(Value::as_str)),
            )
        });
        rows.truncate(limit as usize);
        Ok(json!(rows))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
