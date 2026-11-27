//! HTTP handlers for thread polls: create, read, vote, close.
//!
//! One poll per thread. Polls are created by the thread author or
//! a moderator, and each user may cast exactly one vote per poll.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use uuid::Uuid;

use super::helpers::{
    caller, can_moderate, created, err, load_poll, load_thread, ok, poll_view, require_text,
    require_tenant, resolve_tenant,
};
use super::state::ForumState;
use super::types::{
    CreatePollRequest, ForumPoll, ForumPollOption, MAX_NAME_LEN, MAX_POLL_OPTIONS,
    MAX_POLL_QUESTION_LEN, VotePollRequest,
};

// Poll handlers

pub(crate) async fn create_poll(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePollRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let thread = load_thread(&state, &tenant_id, &path.into_inner())?;
        if thread.author_id != caller.actor && !can_moderate(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(
                "Only the thread author or a moderator may create a poll on this thread",
            ));
        }
        let input = body.into_inner();
        let question = require_text(&input.question, "question", MAX_POLL_QUESTION_LEN)?;
        if input.options.len() < 2 || input.options.len() > MAX_POLL_OPTIONS {
            return Err(UnifiedError::bad_request(format!(
                "A poll needs between 2 and {MAX_POLL_OPTIONS} options"
            )));
        }
        let mut options: Vec<String> = Vec::with_capacity(input.options.len());
        for raw in &input.options {
            options.push(require_text(raw, "option", MAX_NAME_LEN)?);
        }
        if state.polls.iter().any(|entry| {
            entry.value().tenant_id == tenant_id && entry.value().thread_id == thread.id
        }) {
            return Err(UnifiedError::bad_request("This thread already has a poll"));
        }
        let now = Utc::now();
        let poll = ForumPoll {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            thread_id: thread.id,
            question,
            closed: false,
            closed_at: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        for (index, text) in options.iter().enumerate() {
            let option = ForumPollOption {
                id: Uuid::now_v7(),
                tenant_id: tenant_id.clone(),
                poll_id: poll.id,
                text: text.clone(),
                sort_order: index as i64,
                created_at: now,
            };
            state.poll_options.insert(option.id, option);
        }
        state.polls.insert(poll.id, poll.clone());
        state.emit(
            "forums.poll.created",
            json!({
                "poll_id": poll.id.to_string(),
                "thread_id": thread.id.to_string(),
                "question": poll.question,
                "created_by": caller.actor,
            }),
        );
        Ok(poll_view(&state, &poll, &caller.actor))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_thread_poll(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let thread = load_thread(&state, &tenant_id, &path.into_inner())?;
        let poll = state
            .polls
            .iter()
            .map(|entry| entry.value().clone())
            .find(|poll| poll.tenant_id == tenant_id && poll.thread_id == thread.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Thread {} has no poll", thread.id)))?;
        Ok(poll_view(&state, &poll, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_poll(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let poll = load_poll(&state, &tenant_id, &path.into_inner())?;
        Ok(poll_view(&state, &poll, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn vote_poll(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<VotePollRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> =
        (|| {
            let _tenant = require_tenant(&req)?;
            let tenant_id = resolve_tenant(&req)?;
            let caller = caller(&req)?;
            let poll = load_poll(&state, &tenant_id, &path.into_inner())?;
            if poll.closed {
                return Err(UnifiedError::bad_request("This poll is closed"));
            }
            let input = body.into_inner();
            let option = state
                .poll_options
                .get(&input.option_id)
                .map(|entry| entry.value().clone())
                .filter(|option| option.poll_id == poll.id && option.tenant_id == tenant_id)
                .ok_or_else(|| {
                    UnifiedError::bad_request(format!(
                        "Option {} does not belong to poll {}",
                        input.option_id, poll.id
                    ))
                })?;
            if state.poll_votes.iter().any(|entry| {
                entry.value().poll_id == poll.id && entry.value().user_id == caller.actor
            }) {
                return Err(UnifiedError::bad_request(
                    "You have already voted in this poll",
                ));
            }
            let vote = super::types::ForumPollVote {
                id: Uuid::now_v7(),
                tenant_id: tenant_id.clone(),
                poll_id: poll.id,
                option_id: option.id,
                user_id: caller.actor.clone(),
                created_at: Utc::now(),
            };
            state.poll_votes.insert(vote.id, vote);
            state.emit(
                "forums.poll.voted",
                json!({
                    "poll_id": poll.id.to_string(),
                    "thread_id": poll.thread_id.to_string(),
                    "option_id": option.id.to_string(),
                    "user_id": caller.actor,
                }),
            );
            Ok(poll_view(&state, &poll, &caller.actor))
        })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn close_poll(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ForumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut poll = load_poll(&state, &tenant_id, &path.into_inner())?;
        if poll.created_by != caller.actor && !can_moderate(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(
                "Only the poll creator or a moderator may close this poll",
            ));
        }
        if poll.closed {
            return Err(UnifiedError::bad_request("This poll is already closed"));
        }
        let now = Utc::now();
        poll.closed = true;
        poll.closed_at = Some(now);
        poll.updated_at = now;
        state.polls.insert(poll.id, poll.clone());
        state.emit(
            "forums.poll.closed",
            json!({
                "poll_id": poll.id.to_string(),
                "thread_id": poll.thread_id.to_string(),
                "closed_by": caller.actor,
            }),
        );
        Ok(poll_view(&state, &poll, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
