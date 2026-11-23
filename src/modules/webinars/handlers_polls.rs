// webinars -- Poll handlers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use super::helpers::*;
use super::state::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Poll handlers
// ---------------------------------------------------------------------------

pub(crate) fn require_poll_access(
    state: &WebinarsState,
    webinar: &Webinar,
    user_id: &str,
    roles: &[String],
) -> UnifiedResult<()> {
    if is_staff(webinar, user_id, roles) {
        return Ok(());
    }
    match find_registration(state, webinar.id, user_id) {
        Some(row) if row.status == "registered" => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "Only registered attendees and webinar staff can use polls",
        )),
    }
}

pub(crate) async fn list_polls(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<PollView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let params = query.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        require_poll_access(&state, &webinar, &actor, &roles)?;
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<PollView> = state
            .polls
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|poll| {
                if poll.webinar_id != webinar.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &poll.status != status {
                        return false;
                    }
                }
                true
            })
            .map(|poll| poll_view(&state, &poll))
            .collect();
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = a.created_at.cmp(&b.created_at);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

pub(crate) async fn create_poll(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePollRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PollView> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !is_staff(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, a panelist, or an admin can create polls",
            ));
        }
        if !webinar.polls_enabled {
            return Err(UnifiedError::unprocessable(
                "Polls are disabled for this webinar",
            ));
        }
        if webinar.status == WebinarStatus::Ended || webinar.status == WebinarStatus::Cancelled {
            return Err(UnifiedError::unprocessable(
                "Polls cannot be created for an ended or cancelled webinar",
            ));
        }
        let question = require_text(&input.question, "question", MAX_QUESTION_LEN)?;
        let config = state.config();
        if input.options.len() < 2 {
            return Err(UnifiedError::bad_request("A poll needs at least 2 options"));
        }
        if input.options.len() > config.poll_max_options {
            return Err(UnifiedError::bad_request(format!(
                "A poll can have at most {} options",
                config.poll_max_options
            )));
        }
        let mut options: Vec<String> = Vec::new();
        for raw in &input.options {
            options.push(require_text(raw, "options", MAX_POLL_OPTION_LEN)?);
        }
        let now = Utc::now();
        let poll = WebinarPoll {
            id: Uuid::now_v7(),
            webinar_id: webinar.id,
            question,
            options,
            allow_multiple: input.allow_multiple.unwrap_or(false),
            status: "open".to_string(),
            created_by: actor.clone(),
            created_at: now,
            closed_at: None,
            updated_at: now,
        };
        state.polls.insert(poll.id, poll.clone());
        state.emit(
            "webinar.poll.created",
            json!({
                "webinar_id": webinar.id.to_string(),
                "poll_id": poll.id.to_string(),
                "question": poll.question,
                "options": poll.options,
                "allow_multiple": poll.allow_multiple,
                "created_by": actor,
            }),
        );
        Ok(poll_view(&state, &poll))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error),
    }
}

pub(crate) async fn get_poll(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<PollView> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, poll_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        require_poll_access(&state, &webinar, &actor, &roles)?;
        let poll = load_poll(&state, webinar.id, &poll_id)?;
        Ok(poll_view(&state, &poll))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

pub(crate) async fn vote_poll(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<VotePollRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PollView> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let (webinar_id, poll_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        require_poll_access(&state, &webinar, &actor, &roles)?;
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Polls can only be voted in while the webinar is live",
            ));
        }
        let poll = load_poll(&state, webinar.id, &poll_id)?;
        if poll.status != "open" {
            return Err(UnifiedError::unprocessable("This poll is closed"));
        }
        if input.option_indexes.is_empty() {
            return Err(UnifiedError::bad_request(
                "option_indexes must not be empty",
            ));
        }
        if !poll.allow_multiple && input.option_indexes.len() != 1 {
            return Err(UnifiedError::bad_request(
                "This poll accepts exactly one option",
            ));
        }
        let mut cleaned: Vec<usize> = Vec::new();
        for index in &input.option_indexes {
            if *index >= poll.options.len() {
                return Err(UnifiedError::bad_request(format!(
                    "Option index {index} is out of range for this poll",
                )));
            }
            if !cleaned.contains(index) {
                cleaned.push(*index);
            }
        }
        let now = Utc::now();
        let key = format!("{}:{}", poll.id, actor);
        let vote = match state.votes.get(&key) {
            Some(existing) => {
                let mut updated = existing.value().clone();
                updated.option_indexes = cleaned.clone();
                updated.updated_at = now;
                updated
            }
            None => WebinarPollVote {
                id: Uuid::now_v7(),
                poll_id: poll.id,
                webinar_id: webinar.id,
                user_id: actor.clone(),
                option_indexes: cleaned.clone(),
                created_at: now,
                updated_at: now,
            },
        };
        state.votes.insert(key, vote);
        state.emit(
            "webinar.poll.voted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "poll_id": poll.id.to_string(),
                "user_id": actor,
                "option_indexes": cleaned,
            }),
        );
        Ok(poll_view(&state, &poll))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

pub(crate) async fn close_poll(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<PollView> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, poll_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can close polls",
            ));
        }
        let mut poll = load_poll(&state, webinar.id, &poll_id)?;
        if poll.status != "open" {
            return Err(UnifiedError::unprocessable("This poll is already closed"));
        }
        let now = Utc::now();
        poll.status = "closed".to_string();
        poll.closed_at = Some(now);
        poll.updated_at = now;
        state.polls.insert(poll.id, poll.clone());
        state.emit(
            "webinar.poll.closed",
            json!({
                "webinar_id": webinar.id.to_string(),
                "poll_id": poll.id.to_string(),
                "closed_by": actor,
                "closed_at": now.to_rfc3339(),
            }),
        );
        Ok(poll_view(&state, &poll))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

pub(crate) async fn delete_poll(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, poll_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !is_host(&webinar, &actor) && !is_admin(&roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host or an admin can delete polls",
            ));
        }
        let poll = load_poll(&state, webinar.id, &poll_id)?;
        state.polls.remove(&poll.id);
        let vote_keys: Vec<String> = state
            .votes
            .iter()
            .filter(|entry| entry.value().poll_id == poll.id)
            .map(|entry| entry.key().clone())
            .collect();
        for key in vote_keys {
            state.votes.remove(&key);
        }
        state.emit(
            "webinar.poll.deleted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "poll_id": poll.id.to_string(),
                "deleted_by": actor,
            }),
        );
        Ok(json!({ "deleted": true, "poll_id": poll.id.to_string() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}
