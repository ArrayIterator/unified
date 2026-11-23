//! chat - support sessions.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



// ---------------------------------------------------------------------------
// Live support: sessions
// ---------------------------------------------------------------------------
pub(crate) async fn list_support_sessions(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<SupportSession>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let is_roster_agent = state.agents.contains_key(&actor);
        refresh_queue_positions(&state);
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let agent_filter = params.filters.get("agent_id").cloned();
        let visitor_filter = params.filters.get("visitor_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<SupportSession> = state
            .sessions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|session| can_read_support_session(session, &actor, is_roster_agent))
            .filter(|session| in_company_scope(&session.company_id, &_scope))
            .filter(|session| {
                if let Some(status) = &status_filter {
                    let expected = format!("{:?}", session.status).to_lowercase();
                    if &expected != status {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if session.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(agent) = &agent_filter {
                    if session.agent_id.as_deref() != Some(agent.as_str()) {
                        return false;
                    }
                }
                if let Some(visitor) = &visitor_filter {
                    if &session.visitor_id != visitor {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if session.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !session.subject.to_lowercase().contains(&search)
                    && !session.visitor_id.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "updated_at", "subject"], "created_at");
        // Newest first unless the caller asks for ascending order.
        let descending = params.normalized().order != "asc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "subject" => a.subject.cmp(&b.subject),
                "updated_at" => a.updated_at.cmp(&b.updated_at),
                _ => a.created_at.cmp(&b.created_at),
            };
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


pub(crate) async fn create_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<CreateSupportSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<WidgetBootstrap> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        let body = body.into_inner();
        let visitor_id = match &body.visitor_id {
            Some(raw) => require_text(raw, "visitor_id", MAX_DEPARTMENT_LEN)?,
            None => {
                return Err(UnifiedError::bad_request(
                    "visitor_id is required for staff-created sessions",
                ))
            }
        };
        let visitor_name =
            clean_optional_text(&body.visitor_name, "visitor_name", MAX_VISITOR_NAME_LEN)?;
        let visitor_email = match &body.visitor_email {
            Some(raw) => Some(require_email(raw)?),
            None => None,
        };
        let department_id =
            clean_optional_text(&body.department_id, "department_id", MAX_DEPARTMENT_LEN)?;
        let subject = match &body.subject {
            Some(raw) => require_text(raw, "subject", MAX_SUBJECT_LEN)?,
            None => "Website chat".to_string(),
        };
        let company_id = require_company_match(
            &_tenant,
            &req,
            &clean_optional_text(&body.company_id, "company_id", MAX_DEPARTMENT_LEN)?,
        )?;
        let session = create_support_session_inner(
            &state,
            visitor_id,
            visitor_name,
            visitor_email,
            department_id,
            subject,
            company_id,
        )?;
        Ok(WidgetBootstrap {
            channel_id: session.channel_id,
            queue_position: session.queue_position,
            agents_available: agents_available(&state, session.department_id.as_deref()),
            session,
        })
    })();
    match result {
        Ok(bootstrap) => created(bootstrap),
        Err(error) => err(error),
    }
}


pub(crate) async fn get_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SupportSession> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        refresh_queue_positions(&state);
        let session = load_session(&state, &path.into_inner())?;
        require_record_in_company_scope(&session.company_id, &_scope)?;
        if !can_read_support_session(&session, &actor, state.agents.contains_key(&actor)) {
            return Err(UnifiedError::forbidden(
                "This support session is not visible to this user",
            ));
        }
        Ok(session)
    })();
    match result {
        Ok(session) => ok(session),
        Err(error) => err(error),
    }
}


pub(crate) async fn accept_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SupportSession> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        if !state.agents.contains_key(&actor) {
            return Err(UnifiedError::not_found(format!(
                "Agent not registered: {actor}"
            )));
        }
        let mut session = load_session(&state, &path.into_inner())?;
        require_record_in_company_scope(&session.company_id, &_scope)?;
        if session.status != SupportSessionStatus::Waiting {
            return Err(UnifiedError::conflict(format!(
                "Only a waiting session can be accepted (status is {:?})",
                session.status
            )));
        }
        if let Some(department) = &session.department_id {
            let serves = state
                .agents
                .get(&actor)
                .map(|entry| entry.department_ids.iter().any(|id| id == department))
                .unwrap_or(false);
            if !serves {
                return Err(UnifiedError::forbidden(
                    "This agent does not serve the session department",
                ));
            }
        }
        session.agent_id = Some(actor.clone());
        session.status = SupportSessionStatus::Active;
        session.updated_at = Utc::now();
        add_agent_to_channel(&state, &session);
        state.sessions.insert(session.id, session.clone());
        refresh_queue_positions(&state);
        state.emit(
            "chat.session.accepted",
            json!({
                "session_id": session.id.to_string(),
                "agent_id": actor.clone(),
                "visitor_id": session.visitor_id.clone(),
                "department_id": session.department_id.clone(),
            }),
        );
        state.emit(
            "chat.session.assigned",
            json!({
                "session_id": session.id.to_string(),
                "agent_id": actor,
                "department_id": session.department_id.clone(),
                "visitor_id": session.visitor_id.clone(),
            }),
        );
        Ok(session)
    })();
    match result {
        Ok(session) => ok(session),
        Err(error) => err(error),
    }
}


pub(crate) async fn close_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CloseSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SupportSession> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        if !state.agents.contains_key(&actor) {
            return Err(UnifiedError::not_found(format!(
                "Agent not registered: {actor}"
            )));
        }
        let mut session = load_session(&state, &path.into_inner())?;
        require_record_in_company_scope(&session.company_id, &_scope)?;
        if !can_manage_support_session(&session, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the assigned agent can close this session",
            ));
        }
        if session.status != SupportSessionStatus::Active
            && session.status != SupportSessionStatus::Waiting
        {
            return Err(UnifiedError::conflict(format!(
                "Only an active or waiting session can be closed (status is {:?})",
                session.status
            )));
        }
        // A waiting session that never reached an agent is missed, not
        // closed: it tells reporting the visitor was never served.
        if session.status == SupportSessionStatus::Waiting {
            session.status = SupportSessionStatus::Missed;
        } else {
            session.status = SupportSessionStatus::Closed;
        }
        session.close_reason = match &body.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_NOTE_LEN)?),
            None => None,
        };
        session.closed_at = Some(Utc::now());
        session.updated_at = Utc::now();
        state.sessions.insert(session.id, session.clone());
        refresh_queue_positions(&state);
        state.emit(
            "chat.session.closed",
            json!({
                "session_id": session.id.to_string(),
                "agent_id": session.agent_id.clone(),
                "visitor_id": session.visitor_id.clone(),
                "department_id": session.department_id.clone(),
                "status": format!("{:?}", session.status).to_lowercase(),
                "closed_by": actor,
            }),
        );
        Ok(session)
    })();
    match result {
        Ok(session) => ok(session),
        Err(error) => err(error),
    }
}


pub(crate) async fn transfer_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TransferSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SupportSession> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        if !state.agents.contains_key(&actor) {
            return Err(UnifiedError::not_found(format!(
                "Agent not registered: {actor}"
            )));
        }
        let to_agent_id = require_text(&body.to_agent_id, "to_agent_id", MAX_DEPARTMENT_LEN)?;
        if !state.agents.contains_key(&to_agent_id) {
            return Err(UnifiedError::not_found(format!(
                "Target agent not registered: {to_agent_id}"
            )));
        }
        let note = match &body.note {
            Some(raw) => require_text(raw, "note", MAX_NOTE_LEN)?,
            None => String::new(),
        };
        let mut session = load_session(&state, &path.into_inner())?;
        require_record_in_company_scope(&session.company_id, &_scope)?;
        if !can_manage_support_session(&session, &actor) {
            return Err(UnifiedError::forbidden(
                "Only the assigned agent can transfer this session",
            ));
        }
        if session.status != SupportSessionStatus::Active
            && session.status != SupportSessionStatus::Waiting
        {
            return Err(UnifiedError::conflict(format!(
                "Only an active or waiting session can be transferred (status is {:?})",
                session.status
            )));
        }
        if session.agent_id.as_deref() == Some(to_agent_id.as_str()) {
            return Err(UnifiedError::conflict(
                "The session is already assigned to this agent",
            ));
        }
        let from_agent_id = session.agent_id.clone();
        session.transfer_history.push(TransferRecord {
            from_agent_id: from_agent_id.clone(),
            to_agent_id: to_agent_id.clone(),
            note: note.clone(),
            transferred_by: actor.clone(),
            created_at: Utc::now(),
        });
        session.agent_id = Some(to_agent_id.clone());
        session.status = SupportSessionStatus::Active;
        session.updated_at = Utc::now();
        add_agent_to_channel(&state, &session);
        state.sessions.insert(session.id, session.clone());
        refresh_queue_positions(&state);
        let handover = if note.is_empty() {
            format!(
                "Chat transferred from {} to {}.",
                from_agent_id.as_deref().unwrap_or("unassigned"),
                to_agent_id
            )
        } else {
            format!(
                "Chat transferred from {} to {}: {note}",
                from_agent_id.as_deref().unwrap_or("unassigned"),
                to_agent_id
            )
        };
        post_system_message(&state, session.channel_id, handover);
        state.emit(
            "chat.session.transferred",
            json!({
                "session_id": session.id.to_string(),
                "from_agent_id": from_agent_id,
                "to_agent_id": to_agent_id,
                "transferred_by": actor,
                "note": note,
                "visitor_id": session.visitor_id.clone(),
                "department_id": session.department_id.clone(),
            }),
        );
        Ok(session)
    })();
    match result {
        Ok(session) => ok(session),
        Err(error) => err(error),
    }
}


pub(crate) async fn escalate_support_session(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EscalateSessionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let mut session = load_session(&state, &path.into_inner())?;
        require_record_in_company_scope(&session.company_id, &_scope)?;
        if session.escalated_ticket_id.is_some() {
            return Err(UnifiedError::conflict(
                "This session was already escalated to a ticket",
            ));
        }
        let title = match &body.title {
            Some(raw) => require_text(raw, "title", MAX_NAME_LEN)?,
            None => format!("Chat transcript: {}", session.subject),
        };
        let priority = match &body.priority {
            Some(raw) => {
                let cleaned = require_text(raw, "priority", 32)?;
                if !valid_ticket_priority(&cleaned) {
                    return Err(UnifiedError::bad_request(
                        "priority must be one of: low, normal, high, urgent",
                    ));
                }
                cleaned
            }
            None => "normal".to_string(),
        };
        let category = match &body.category {
            Some(raw) => require_text(raw, "category", MAX_NAME_LEN)?,
            None => "chat".to_string(),
        };
        // The full transcript becomes the ticket body so the agent who
        // picks the ticket up sees the whole conversation.
        let transcript = build_transcript_text(&state, &session);
        let ticket_id = request_ticket(
            &state,
            json!({
                "source": "chat",
                "session_id": session.id.to_string(),
                "channel_id": session.channel_id.to_string(),
                "user_id": session.visitor_id.clone(),
                "requested_by": actor.clone(),
                "title": title.clone(),
                "body": transcript.clone(),
                "description": format!(
                    "Chat session {} escalated by {}",
                    session.id, actor
                ),
                "company_id": session.company_id.clone(),
                "department_id": session.department_id.clone(),
                "priority": priority,
                "category": category,
            }),
        )?;
        session.escalated_ticket_id = ticket_id.clone();
        session.updated_at = Utc::now();
        state.sessions.insert(session.id, session.clone());
        state.emit(
            "chat.session.escalated",
            json!({
                "session_id": session.id.to_string(),
                "ticket_id": ticket_id,
                "escalated_by": actor,
                "visitor_id": session.visitor_id.clone(),
                "department_id": session.department_id.clone(),
            }),
        );
        Ok(json!({
            "ticket_requested": true,
            "ticket_id": ticket_id,
            "session_id": session.id.to_string(),
            "title": title,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}


pub(crate) async fn get_session_transcript(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<TranscriptQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let session = load_session(&state, &path.into_inner())?;
        require_record_in_company_scope(&session.company_id, &_scope)?;
        if !can_read_support_session(&session, &actor, state.agents.contains_key(&actor)) {
            return Err(UnifiedError::forbidden(
                "This support session is not visible to this user",
            ));
        }
        let mut messages: Vec<ChatMessage> = state
            .messages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|message| message.channel_id == session.channel_id)
            .collect();
        messages.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        let mut files: Vec<ChatFile> = state
            .files
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|file| file.channel_id == session.channel_id)
            .collect();
        files.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let rating = state
            .ratings
            .iter()
            .map(|entry| entry.value().clone())
            .find(|rating| rating.session_id == session.id);
        let format = query
            .into_inner()
            .format
            .unwrap_or_else(|| "json".to_string());
        if format == "text" {
            return Ok(json!({
                "session_id": session.id.to_string(),
                "format": "text",
                "transcript": build_transcript_text(&state, &session),
            }));
        }
        let transcript = SessionTranscript {
            session,
            messages,
            files,
            rating,
        };
        serde_json::to_value(transcript)
            .map_err(|_| UnifiedError::bad_request("Transcript could not be serialized"))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}


pub(crate) async fn get_queue_position(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        refresh_queue_positions(&state);
        let session = load_session(&state, &path.into_inner())?;
        require_record_in_company_scope(&session.company_id, &_scope)?;
        let waiting_ahead = if session.status == SupportSessionStatus::Waiting {
            session.queue_position.saturating_sub(1)
        } else {
            0
        };
        Ok(json!({
            "session_id": session.id.to_string(),
            "status": format!("{:?}", session.status).to_lowercase(),
            "position": session.queue_position,
            "waiting_ahead": waiting_ahead,
            "agents_available": agents_available(&state, session.department_id.as_deref()),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}


/// Read scope for a support session: its visitor, its assigned
/// agent, or any agent on the support roster. Anyone else — even
/// authenticated — sees nothing: no list rows, no session, no
/// transcript.
pub(crate) fn can_read_support_session(
    session: &SupportSession,
    actor: &str,
    is_roster_agent: bool,
) -> bool {
    session.visitor_id == actor
        || session.agent_id.as_deref() == Some(actor)
        || is_roster_agent
}


/// Close/transfer scope for a support session: only the assigned
/// agent. A session with no agent yet is unassigned queue work,
/// so any roster agent may close it (it becomes `Missed`) or
/// transfer it (which routes it); the handler still requires the
/// caller to be on the agent roster. There is no supervisor role
/// in this codebase to exempt, so none is checked here.
pub(crate) fn can_manage_support_session(session: &SupportSession, actor: &str) -> bool {
    match &session.agent_id {
        Some(assigned) => assigned == actor,
        None => session.status == SupportSessionStatus::Waiting,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn session(visitor_id: &str, agent_id: Option<&str>, status: SupportSessionStatus) -> SupportSession {
        let now = Utc::now();
        SupportSession {
            id: Uuid::now_v7(),
            visitor_id: visitor_id.to_string(),
            visitor_name: None,
            visitor_email: None,
            department_id: None,
            agent_id: agent_id.map(str::to_string),
            status,
            subject: "Help".to_string(),
            channel_id: Uuid::now_v7(),
            company_id: None,
            queue_position: 0,
            escalated_ticket_id: None,
            transfer_history: Vec::new(),
            close_reason: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
        }
    }

    #[test]
    fn session_reads_are_scoped_to_visitor_or_roster_agent() {
        let session = session("visitor-1", Some("agent-1"), SupportSessionStatus::Active);
        assert!(can_read_support_session(&session, "visitor-1", false));
        assert!(can_read_support_session(&session, "agent-1", false));
        assert!(can_read_support_session(&session, "agent-2", true));
        // Any other authenticated caller — another visitor, a
        // stranger — sees nothing.
        assert!(!can_read_support_session(&session, "visitor-2", false));
        assert!(!can_read_support_session(&session, "stranger", false));
    }

    #[test]
    fn only_the_assigned_agent_may_close_or_transfer() {
        let active = session("visitor-1", Some("agent-1"), SupportSessionStatus::Active);
        assert!(can_manage_support_session(&active, "agent-1"));
        assert!(!can_manage_support_session(&active, "agent-2"));
        assert!(!can_manage_support_session(&active, "visitor-1"));
        // Unassigned waiting sessions are queue work: any roster
        // agent may close (missed) or transfer (route) them.
        let waiting = session("visitor-1", None, SupportSessionStatus::Waiting);
        assert!(can_manage_support_session(&waiting, "agent-2"));
        // But an unassigned session that is no longer waiting
        // belongs to nobody.
        let closed = session("visitor-1", None, SupportSessionStatus::Closed);
        assert!(!can_manage_support_session(&closed, "agent-2"));
    }
}
