//! chat - support ratings.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json};
use uuid::Uuid;
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_agents::*, support_canned::*, support_offline::*};



// ---------------------------------------------------------------------------
// Live support: ratings
// ---------------------------------------------------------------------------
pub(crate) async fn rate_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RateSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChatRating> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let session = load_session(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&session.company_id, &_scope)?;
        rating_actor_allowed(&session, &actor)?;
        if session.status != SupportSessionStatus::Closed {
            return Err(UnifiedError::conflict(
                "Only a closed chat can be rated",
            ));
        }
        if state
            .ratings
            .iter()
            .any(|entry| entry.value().session_id == session.id)
        {
            return Err(UnifiedError::conflict(
                "This chat has already been rated",
            ));
        }
        if body.score == 0 || body.score > MAX_RATING_SCORE {
            return Err(UnifiedError::bad_request(format!(
                "score must be between 1 and {MAX_RATING_SCORE}"
            )));
        }
        let comment = match &body.comment {
            Some(raw) => Some(require_text(raw, "comment", MAX_RATING_COMMENT_LEN)?),
            None => None,
        };
        let visitor_id = match &body.visitor_id {
            Some(raw) => {
                let cleaned = require_text(raw, "visitor_id", MAX_DEPARTMENT_LEN)?;
                if cleaned != session.visitor_id || cleaned != actor {
                    return Err(UnifiedError::forbidden(
                        "Only the chat visitor can rate this chat",
                    ));
                }
                cleaned
            }
            None => actor.clone(),
        };
        let rating = ChatRating {
            id: Uuid::now_v7(),
            session_id: session.id,
            visitor_id: visitor_id.clone(),
            agent_id: session.agent_id.clone(),
            department_id: session.department_id.clone(),
            score: body.score,
            comment,
            created_at: Utc::now(),
        };
        state.ratings.insert(rating.id, rating.clone());
        state.emit(
            "chat.session.rated",
            json!({
                "session_id": session.id.to_string(),
                "rating_id": rating.id.to_string(),
                "visitor_id": visitor_id,
                "agent_id": rating.agent_id.clone(),
                "department_id": rating.department_id.clone(),
                "score": rating.score,
            }),
        );
        Ok(rating)
    })();
    match result {
        Ok(rating) => created(rating),
        Err(error) => err(error),
    }
}


pub(crate) async fn get_session_rating(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ChatRating> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        let session = load_session(&state, &path.into_inner())?;
        require_record_in_company_scope(&session.company_id, &_scope)?;
        state
            .ratings
            .iter()
            .map(|entry| entry.value().clone())
            .find(|rating| rating.session_id == session.id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No rating for support session: {}",
                    session.id
                ))
            })
    })();
    match result {
        Ok(rating) => ok(rating),
        Err(error) => err(error),
    }
}


pub(crate) async fn list_ratings(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<ChatRating>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let agent_filter = params.filters.get("agent_id").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let score_filter = params.filters.get("score").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ChatRating> = state
            .ratings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rating| {
                if let Some(agent) = &agent_filter {
                    if rating.agent_id.as_deref() != Some(agent.as_str()) {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if rating.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(score) = &score_filter {
                    if rating.score.to_string() != *score {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let comment = rating.comment.clone().unwrap_or_default().to_lowercase();
                    if !comment.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        // Newest first unless the caller asks for ascending order.
        let descending = params.normalized().order != "asc";
        items.sort_by(|a, b| {
            if descending {
                b.created_at.cmp(&a.created_at)
            } else {
                a.created_at.cmp(&b.created_at)
            }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}


/// Only the session's own visitor may rate it, and the caller
/// must authenticate as that visitor (`X-User-Id` == the
/// session's `visitor_id`). The `visitor_id` in the request body
/// is a claim, not an identity: without this check anyone could
/// rate any closed session as its visitor.
pub(crate) fn rating_actor_allowed(session: &SupportSession, actor: &str) -> UnifiedResult<()> {
    if actor != session.visitor_id {
        return Err(UnifiedError::forbidden(
            "Only the chat visitor can rate this chat",
        ));
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn session(visitor_id: &str, agent_id: Option<&str>) -> SupportSession {
        let now = Utc::now();
        SupportSession {
            id: Uuid::now_v7(),
            visitor_id: visitor_id.to_string(),
            visitor_name: None,
            visitor_email: None,
            department_id: None,
            agent_id: agent_id.map(str::to_string),
            status: SupportSessionStatus::Closed,
            subject: "Help".to_string(),
            channel_id: Uuid::now_v7(),
            company_id: None,
            queue_position: 0,
            escalated_ticket_id: None,
            transfer_history: Vec::new(),
            close_reason: None,
            created_at: now,
            updated_at: now,
            closed_at: Some(now),
        }
    }

    #[test]
    fn only_the_session_visitor_may_rate_it() {
        let session = session("visitor-1", Some("agent-1"));
        assert!(rating_actor_allowed(&session, "visitor-1").is_ok());
        // The assigned agent, another visitor, and a stranger are
        // all refused — previously any of them could rate the
        // session by asserting its visitor id in the body.
        assert!(rating_actor_allowed(&session, "agent-1").is_err());
        assert!(rating_actor_allowed(&session, "visitor-2").is_err());
        assert!(rating_actor_allowed(&session, "").is_err());
    }
}
