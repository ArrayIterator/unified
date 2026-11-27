//! chat - helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{Utc};
use regex::Regex;
use serde::{Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::OnceLock;
use uuid::Uuid;
use super::{limits::*, types::*, hub::*, state::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



// Helpers
pub(crate) fn actor_id(req: &HttpRequest) -> UnifiedResult<String> {
    Ok(unified_core::caller::require_caller(req)?.user_id)
}


/// Roles of the server-verified caller, resolved by the auth
/// middleware (never a header). A role-restricted public channel is
/// still a convenience filter only, never access control for
/// sensitive content: sensitive conversations belong in private or DM
/// channels, where membership - not a role - decides visibility. See
/// also the module docs in `module.rs`.
pub(crate) fn caller_roles(req: &HttpRequest) -> Vec<String> {
    unified_core::caller::caller_from(req).roles
}


/// Resolve the tenant for this request, failing closed when the
/// tenant-resolution middleware attached no tenant context (401)
/// or the resolved tenant id is not a uuidv7 UUID (403). Every
/// tenant-data handler in this module calls this FIRST, before
/// caller identity or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant. See
/// `unified_core::tenancy` (`TENANT_SCOPE_CONTRACT`).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// - a request must not carry one company in its tenant context and
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

/// Resolve the caller's company for a WRITE (C3, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}


/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known (neither the tenant context nor
/// the verified caller carries a company), a body company is refused:
/// an unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3):
/// a write requires a resolved company scope.
pub(crate) fn require_company_match(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    let caller = caller_company(tenant, req)?;
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body != &caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(body.clone())),
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, Some(caller)) => Ok(Some(caller)),
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records carrying
/// exactly that company match, plus unscoped (global/legacy) records
/// (C3, rule A); an unresolved caller still sees global rows only.
pub(crate) fn in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.is_none()
                || record_company_id.as_deref() == Some(company.as_str()),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope FOR A WRITE (C3, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records - global (unscoped) records are platform-only
/// - and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(crate) fn require_record_writable_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) if record == caller => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}



pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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


pub(crate) fn clean_members(members: Option<Vec<String>>, owner: &str) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for member in members.unwrap_or_default() {
        let cleaned = require_text(&member, "member user id", 128)?;
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    if !out.contains(&owner.to_string()) {
        out.push(owner.to_string());
    }
    if out.len() > MAX_MEMBERS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_MEMBERS} members are allowed"
        )));
    }
    Ok(out)
}


pub(crate) fn clean_roles(roles: Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for role in roles.unwrap_or_default() {
        let cleaned = require_text(&role, "role", 64)?;
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    Ok(out)
}


pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}


pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}


pub(crate) fn err(error: UnifiedError) -> HttpResponse {
    json::error_response(&error, false)
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


/// True when the caller may see (and post in) the channel: members always,
/// everyone for unrestricted public channels, role holders for restricted
/// public channels, nobody else for private and DM channels.
///
/// The role arm trusts `caller_roles` (the self-asserted
/// `X-User-Roles` header): role-restricted public channels are
/// not a security boundary - see `caller_roles`.
pub(crate) fn can_view(channel: &Channel, user: &str, roles: &[String]) -> bool {
    if channel.members.iter().any(|member| member == user) {
        return true;
    }
    match channel.kind {
        ChannelKind::Public => {
            if channel.roles.is_empty() {
                true
            } else {
                channel.roles.iter().any(|role| roles.contains(role))
            }
        }
        ChannelKind::Private | ChannelKind::Dm => false,
    }
}


pub(crate) fn load_channel(state: &ChatState, raw_id: &str) -> UnifiedResult<Channel> {
    let id = parse_uuid(raw_id, "channel")?;
    state
        .channels
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Channel not found: {id}")))
}


pub(crate) fn load_message(state: &ChatState, channel_id: Uuid, raw_id: &str) -> UnifiedResult<ChatMessage> {
    let id = parse_uuid(raw_id, "message")?;
    let message = state
        .messages
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Message not found: {id}")))?;
    if message.channel_id != channel_id {
        return Err(UnifiedError::not_found(format!("Message not found: {id}")));
    }
    Ok(message)
}


/// `@user` mention pattern, compiled once and shared by every
/// `find_mentions` call instead of being recompiled per message.
fn mention_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"@([A-Za-z0-9][A-Za-z0-9_-]*)").expect("mention regex is valid")
    })
}

/// Find `@user` mentions in a message body. The captured name is treated
/// as a user id; the notifications module resolves it.
pub(crate) fn find_mentions(body: &str) -> Vec<String> {
    let pattern = mention_regex();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for capture in pattern.captures_iter(body) {
        let name = capture[1].to_string();
        if seen.insert(name.clone()) {
            out.push(name);
        }
    }
    out
}


pub(crate) fn require_email(value: &str) -> UnifiedResult<String> {
    let cleaned = require_text(value, "email", MAX_EMAIL_LEN)?;
    // Strict shape: exactly one `@`, a non-empty local part, and a
    // domain of at least two non-empty labels. Checking only
    // `contains('@')` and `contains('.')` accepted `@.`,
    // `user@.com`, and `user@example.` as valid addresses.
    if cleaned.chars().any(char::is_whitespace) {
        return Err(UnifiedError::bad_request("email must be a valid email address"));
    }
    let (local, domain) = cleaned.split_once('@').ok_or_else(|| {
        UnifiedError::bad_request("email must be a valid email address")
    })?;
    if local.is_empty() || domain.contains('@') {
        return Err(UnifiedError::bad_request("email must be a valid email address"));
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|label| label.is_empty()) {
        return Err(UnifiedError::bad_request("email must be a valid email address"));
    }
    Ok(cleaned)
}


pub(crate) fn clean_department_ids(departments: Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for department in departments.unwrap_or_default() {
        let cleaned = require_text(&department, "department_id", MAX_DEPARTMENT_LEN)?;
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    Ok(out)
}


pub(crate) fn clean_optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}


pub(crate) fn valid_ticket_priority(priority: &str) -> bool {
    matches!(priority, "low" | "normal" | "high" | "urgent")
}


pub(crate) fn load_session(state: &ChatState, raw_id: &str) -> UnifiedResult<SupportSession> {
    let id = parse_uuid(raw_id, "session")?;
    state
        .sessions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Support session not found: {id}")))
}


/// Recompute the 1-based queue position of every waiting session
/// (oldest waiting visitor first). Sessions that are not waiting get 0.
pub(crate) fn refresh_queue_positions(state: &ChatState) {
    let mut waiting: Vec<SupportSession> = state
        .sessions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|session| session.status == SupportSessionStatus::Waiting)
        .collect();
    waiting.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    let mut positions: std::collections::HashMap<Uuid, u32> = std::collections::HashMap::new();
    for (index, session) in waiting.iter().enumerate() {
        positions.insert(session.id, (index + 1) as u32);
    }
    // Snapshot current positions first, then write, so iteration and
    // mutation of the same map never overlap.
    let current: Vec<(Uuid, u32)> = state
        .sessions
        .iter()
        .map(|entry| (entry.value().id, entry.value().queue_position))
        .collect();
    let updates: Vec<(Uuid, u32)> = current
        .into_iter()
        .map(|(id, current_position)| {
            (id, positions.get(&id).copied().unwrap_or(0), current_position)
        })
        .filter(|(_, position, current_position)| position != current_position)
        .map(|(id, position, _)| (id, position))
        .collect();
    for (id, position) in updates {
        if let Some(mut entry) = state.sessions.get_mut(&id) {
            entry.queue_position = position;
        }
    }
}


/// Effective availability of a roster agent. `None` means the agent
/// cannot take chats right now; `Some(rank)` ranks candidates for
/// routing (online first, then agents with no presence entry, then away).
/// An offline or busy presence always wins over the agent's own toggle.
pub(crate) fn agent_availability_rank(state: &ChatState, agent: &SupportAgent) -> Option<u8> {
    if !agent.available {
        return None;
    }
    presence_rank(state, &agent.user_id)
}


/// Presence-only availability rank for a user id: online first, then
/// users with no presence entry, then away. Offline or busy means
/// unavailable.
pub(crate) fn presence_rank(state: &ChatState, user_id: &str) -> Option<u8> {
    match state.presence.get(user_id) {
        Some(entry) => match entry.status {
            PresenceStatus::Online => Some(0),
            PresenceStatus::Away => Some(2),
            PresenceStatus::Busy | PresenceStatus::Offline => None,
        },
        None => Some(1),
    }
}


/// Ask the Departments module for a department's agent user ids
/// (`department.agents_requested`, see `tickets.rs`,
/// `agents_for_department`). Empty when the module is not registered.
pub(crate) fn department_agent_ids(state: &ChatState, department_id: &str) -> Vec<String> {
    let context = state.emit_ctx(
        "department.agents_requested",
        json!({ "department_id": department_id }),
    );
    for result in &context.results {
        if let Some(ids) = result.get("agent_ids").and_then(|value| value.as_array()) {
            return ids
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect();
        }
    }
    Vec::new()
}


/// The best agent for a department: first the agents the Departments
/// module reports for it (roster record supplies the availability
/// toggle and concurrent-chat limit; agents without a chat roster
/// record count as available with the default limit), then - when the
/// Departments module does not answer - chat's own roster for the
/// department. `department_id` of `None` matches any roster agent.
/// Fewest active chats first, then best presence rank.
pub(crate) fn best_available_agent(state: &ChatState, department_id: Option<&str>) -> Option<String> {
    if let Some(department) = department_id {
        let mut scored: Vec<(u64, u8, String)> = Vec::new();
        for agent_id in department_agent_ids(state, department) {
            let (available, max_concurrent) = match state.agents.get(&agent_id) {
                Some(entry) => (entry.available, entry.max_concurrent),
                None => (true, DEFAULT_MAX_CONCURRENT_CHATS),
            };
            if !available {
                continue;
            }
            let Some(rank) = presence_rank(state, &agent_id) else {
                continue;
            };
            let active = active_chat_count(state, &agent_id);
            if active >= max_concurrent as u64 {
                continue;
            }
            scored.push((active, rank, agent_id));
        }
        if !scored.is_empty() {
            scored.sort();
            return scored.into_iter().next().map(|(_, _, user_id)| user_id);
        }
    }
    let mut candidates: Vec<(u64, u8, String)> = state
        .agents
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|agent| match department_id {
            Some(department) => agent.department_ids.iter().any(|id| id == department),
            None => true,
        })
        .filter_map(|agent| {
            let rank = agent_availability_rank(state, &agent)?;
            let active = active_chat_count(state, &agent.user_id);
            if active >= agent.max_concurrent as u64 {
                return None;
            }
            Some((active, rank, agent.user_id))
        })
        .collect();
    candidates.sort();
    candidates.into_iter().next().map(|(_, _, user_id)| user_id)
}


pub(crate) fn agents_available(state: &ChatState, department_id: Option<&str>) -> bool {
    best_available_agent(state, department_id).is_some()
}


/// Add the assigned agent to the session's backing channel so they can
/// read and post in it.
pub(crate) fn add_agent_to_channel(state: &ChatState, session: &SupportSession) {
    let Some(agent_id) = &session.agent_id else {
        return;
    };
    if let Some(mut channel) = state.channels.get_mut(&session.channel_id) {
        if !channel.members.contains(agent_id) {
            channel.members.push(agent_id.clone());
            channel.updated_at = Utc::now();
        }
    }
}


/// Number of active sessions currently assigned to an agent.
pub(crate) fn active_chat_count(state: &ChatState, agent_id: &str) -> u64 {
    state
        .sessions
        .iter()
        .filter(|entry| {
            let session = entry.value();
            session.status == SupportSessionStatus::Active
                && session.agent_id.as_deref() == Some(agent_id)
        })
        .count() as u64
}


/// Route a session to a department agent (`best_available_agent` asks
/// the Departments module first and falls back to chat's own roster).
/// With no agent free the session waits in the queue.
pub(crate) fn route_session(state: &ChatState, session: &mut SupportSession) {
    let agent_id = best_available_agent(state, session.department_id.as_deref());
    match agent_id {
        Some(agent_id) => {
            session.agent_id = Some(agent_id.clone());
            session.status = SupportSessionStatus::Active;
            session.updated_at = Utc::now();
            add_agent_to_channel(state, session);
            state.emit(
                "chat.session.assigned",
                json!({
                    "session_id": session.id.to_string(),
                    "agent_id": agent_id,
                    "department_id": session.department_id.clone(),
                    "visitor_id": session.visitor_id.clone(),
                }),
            );
        }
        None => {
            session.status = SupportSessionStatus::Waiting;
            session.updated_at = Utc::now();
            state.emit(
                "chat.session.queued",
                json!({
                    "session_id": session.id.to_string(),
                    "department_id": session.department_id.clone(),
                    "visitor_id": session.visitor_id.clone(),
                }),
            );
        }
    }
}


/// Create the backing private channel and the session, then route it.
/// Shared by the widget bootstrap and staff-created sessions.
pub(crate) fn create_support_session_inner(
    state: &ChatState,
    visitor_id: String,
    visitor_name: Option<String>,
    visitor_email: Option<String>,
    department_id: Option<String>,
    subject: String,
    company_id: Option<String>,
) -> UnifiedResult<SupportSession> {
    let session_id = Uuid::now_v7();
    let now = Utc::now();
    let channel = Channel {
        id: Uuid::now_v7(),
        name: format!("support:{session_id}"),
        kind: ChannelKind::Private,
        company_id: company_id.clone(),
        owner_id: visitor_id.clone(),
        members: vec![visitor_id.clone()],
        roles: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    state.channels.insert(channel.id, channel.clone());
    let mut session = SupportSession {
        id: session_id,
        visitor_id: visitor_id.clone(),
        visitor_name,
        visitor_email,
        department_id,
        agent_id: None,
        status: SupportSessionStatus::Waiting,
        subject,
        channel_id: channel.id,
        company_id,
        queue_position: 0,
        escalated_ticket_id: None,
        transfer_history: Vec::new(),
        close_reason: None,
        created_at: now,
        updated_at: now,
        closed_at: None,
    };
    route_session(state, &mut session);
    state.sessions.insert(session.id, session.clone());
    refresh_queue_positions(state);
    let stored = state
        .sessions
        .get(&session.id)
        .map(|entry| entry.value().clone())
        .unwrap_or(session);
    state.emit(
        "chat.session.created",
        json!({
            "session_id": stored.id.to_string(),
            "visitor_id": stored.visitor_id.clone(),
            "department_id": stored.department_id.clone(),
            "agent_id": stored.agent_id.clone(),
            "status": format!("{:?}", stored.status).to_lowercase(),
            "channel_id": stored.channel_id.to_string(),
            "company_id": stored.company_id.clone(),
        }),
    );
    Ok(stored)
}


/// Post a system message into a channel (transfer notices, queue notes).
/// System messages are not attributed to a user.
pub(crate) fn post_system_message(state: &ChatState, channel_id: Uuid, body: String) {
    let now = Utc::now();
    let message = ChatMessage {
        id: Uuid::now_v7(),
        channel_id,
        author_id: "system".to_string(),
        body,
        edited: false,
        reactions: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    state.messages.insert(message.id, message.clone());
    state.hub.publish(ChatBroadcast {
        event: "chat.message".to_string(),
        channel_id,
        message,
    });
}


/// Render a session as plain text for ticket bodies and exports.
pub(crate) fn build_transcript_text(state: &ChatState, session: &SupportSession) -> String {
    let mut lines: Vec<String> = vec![
        "Live chat transcript".to_string(),
        format!("Session: {}", session.id),
        format!("Visitor: {}", session.visitor_id),
    ];
    if let Some(name) = &session.visitor_name {
        lines.push(format!("Visitor name: {name}"));
    }
    if let Some(email) = &session.visitor_email {
        lines.push(format!("Visitor email: {email}"));
    }
    if let Some(department) = &session.department_id {
        lines.push(format!("Department: {department}"));
    }
    if let Some(agent) = &session.agent_id {
        lines.push(format!("Agent: {agent}"));
    }
    lines.push(format!("Subject: {}", session.subject));
    lines.push(format!("Started: {}", session.created_at.to_rfc3339()));
    if let Some(closed) = session.closed_at {
        lines.push(format!("Closed: {}", closed.to_rfc3339()));
    }
    lines.push(String::new());
    let mut messages: Vec<ChatMessage> = state
        .messages
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|message| message.channel_id == session.channel_id)
        .collect();
    messages.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    for message in &messages {
        lines.push(format!(
            "[{}] {}: {}",
            message.created_at.to_rfc3339(),
            message.author_id,
            message.body
        ));
    }
    let files: Vec<ChatFile> = state
        .files
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|file| file.channel_id == session.channel_id)
        .collect();
    if !files.is_empty() {
        lines.push(String::new());
        lines.push("Shared files:".to_string());
        for file in &files {
            let reference = file
                .storage_key
                .clone()
                .or_else(|| file.url.clone())
                .unwrap_or_default();
            lines.push(format!(
                "- {} ({} bytes, {}) {}",
                file.name, file.size, file.mime, reference
            ));
        }
    }
    if !session.transfer_history.is_empty() {
        lines.push(String::new());
        lines.push("Transfers:".to_string());
        for transfer in &session.transfer_history {
            lines.push(format!(
                "- {} -> {} (by {}, {}): {}",
                transfer.from_agent_id.as_deref().unwrap_or("unassigned"),
                transfer.to_agent_id,
                transfer.transferred_by,
                transfer.created_at.to_rfc3339(),
                transfer.note
            ));
        }
    }
    lines.join("\n")
}


/// Emit `ticket.create_requested` and read the answer. The tickets
/// module's handler (`tickets.rs`, `on_ticket_create_requested`) adds the
/// created ticket as a result, or `{"error": ...}` when validation
/// failed. Returns the created ticket id, or `None` when no tickets
/// handler answered (ticket creation is best-effort from chat's side).
pub(crate) fn request_ticket(state: &ChatState, payload: Value) -> UnifiedResult<Option<String>> {
    let context = state.emit_ctx("ticket.create_requested", payload);
    for result in &context.results {
        if let Some(error) = result.get("error").and_then(|value| value.as_str()) {
            return Err(UnifiedError::unprocessable(format!(
                "Ticket creation failed: {error}"
            )));
        }
        if let Some(id) = result.get("id").and_then(|value| value.as_str()) {
            return Ok(Some(id.to_string()));
        }
    }
    Ok(None)
}


pub(crate) fn default_widget_config(company_id: Option<String>) -> WidgetConfig {
    let now = Utc::now();
    WidgetConfig {
        id: Uuid::nil(),
        company_id,
        name: "Live chat".to_string(),
        greeting: "Hello! How can we help you today?".to_string(),
        offline_message: "No agents are available right now. Leave a message and we will get back to you.".to_string(),
        primary_color: "#0B6B3A".to_string(),
        departments: Vec::new(),
        enabled: true,
        created_at: now,
        updated_at: now,
    }
}
#[cfg(test)]
mod tests {

    /// The tenant context the tenant-resolution middleware would
    /// attach for a request on a tenant host
    /// (`unified_core::middleware::TenantContext`, the exact struct
    /// `tenancy::TenantContext::from_http_request` reads from the
    /// request extensions). The company is empty, so the resolved
    /// scope is unscoped: existing tests that seed company-less
    /// records keep passing, while the tenant gate itself (401
    /// without any context) is exercised.
    fn tenant_context() -> unified_core::middleware::TenantContext {
        unified_core::middleware::TenantContext {
            tenant_id: uuid::Uuid::now_v7().to_string(),
            company_id: String::new(),
            site_id: String::new(),
            kind: unified_core::middleware::TenantKind::Subdomain,
            host: "tenant.example.test".to_string(),
            scope: None,
            product_id: None,
            package_id: None,
            product_subdomain_id: None,
        }
    }

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(tenant_context());
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(tenant_context());
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

        /// A request with a tenant context carrying `tenant_company`
    /// (empty = no tenant company), a verified caller carrying
    /// `verified_company` (empty = no company resolved), and a
    /// forged `X-Company-Id` header when `forged_header` is
    /// non-empty. The header must never affect the scope.
    fn scoped_request(
        tenant_company: &str,
        verified_company: &str,
        forged_header: &str,
    ) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let mut req = actix_web::test::TestRequest::default();
        if !forged_header.is_empty() {
            req = req.insert_header(("x-company-id", forged_header));
        }
        let req = req.to_http_request();
        if !verified_company.is_empty() {
            req.extensions_mut().insert(
                unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                    .with_company(verified_company.to_string()),
            );
        }
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: tenant_company.to_string(),
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
    fn caller_company_prefers_tenant_company_over_verified_caller() {
        let req = scoped_request("company-a", "company-a", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-a".to_string())
        );
    }

    #[test]
    fn caller_company_uses_verified_caller_when_tenant_has_none() {
        let req = scoped_request("", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-b".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = scoped_request("company-a", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert!(super::caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_a_forged_company_header() {
        let req = scoped_request("", "", "company-forged");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(super::caller_company(&tenant, &req).unwrap(), None);
    }

#[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = verified_request("user-1", "admin");
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;

    #[test]
    fn parse_uuid_accepts_valid_and_rejects_invalid() {
        let id = parse_uuid("550e8400-e29b-41d4-a716-446655440000", "channel").unwrap();
        assert_eq!(id.to_string(), "550e8400-e29b-41d4-a716-446655440000");
        assert!(parse_uuid("not-a-uuid", "channel").is_err());
        assert!(parse_uuid("", "channel").is_err());
    }

    #[test]
    fn require_text_trims_and_enforces_limits() {
        assert_eq!(require_text("  hello  ", "name", 10).unwrap().as_str(), "hello");
        assert!(require_text("   ", "name", 10).is_err());
        assert!(require_text("", "name", 10).is_err());
        assert!(require_text("toolong", "name", 3).is_err());
        assert!(require_text("exact", "name", 5).is_ok());
    }

    #[test]
    fn clean_members_deduplicates_and_appends_owner() {
        let members = clean_members(
            Some(vec!["alice".to_string(), "bob".to_string(), "alice".to_string()]),
            "owner",
        )
        .unwrap();
        assert_eq!(
            members,
            vec!["alice".to_string(), "bob".to_string(), "owner".to_string()]
        );
        assert_eq!(
            clean_members(None, "owner").unwrap(),
            vec!["owner".to_string()]
        );
        assert_eq!(
            clean_members(Some(vec!["owner".to_string()]), "owner").unwrap(),
            vec!["owner".to_string()]
        );
    }

    #[test]
    fn clean_roles_deduplicates_and_trims() {
        let roles = clean_roles(Some(vec!["admin".to_string(), " admin ".to_string()])).unwrap();
        assert_eq!(roles, vec!["admin".to_string()]);
        assert!(clean_roles(None).unwrap().is_empty());
    }

    #[test]
    fn find_mentions_collects_unique_user_ids() {
        assert_eq!(
            find_mentions("@alice hello @bob and @alice again"),
            vec!["alice".to_string(), "bob".to_string()]
        );
        assert!(find_mentions("no mentions here").is_empty());
        assert!(find_mentions("@-bad @ ok").is_empty());
    }

    #[test]
    fn find_mentions_is_stable_across_repeated_calls() {
        // The mention pattern is compiled once and shared: repeated
        // calls must agree exactly with the first call.
        let body = "@alice and @bob-2, then @alice again";
        let first = find_mentions(body);
        for _ in 0..3 {
            assert_eq!(find_mentions(body), first);
        }
        assert_eq!(first, vec!["alice".to_string(), "bob-2".to_string()]);
    }

    #[test]
    fn require_email_accepts_simple_addresses() {
        assert!(require_email("user@example.com").is_ok());
        assert_eq!(
            require_email("  user@example.com  ").unwrap().as_str(),
            "user@example.com"
        );
        assert!(require_email("plainaddress").is_err());
        assert!(require_email("user@example").is_err());
        assert!(require_email("").is_err());
    }

    #[test]
    fn require_email_rejects_empty_local_or_domain_labels() {
        // The old `contains('@') && contains('.')` check accepted
        // every one of these.
        for bad in [
            "@.",
            "@example.com",
            "user@",
            "user@.com",
            "user@example.",
            "user@example..com",
            "user@@example.com",
            "user@exam ple.com",
            "us er@example.com",
        ] {
            assert!(require_email(bad).is_err(), "{bad} must be rejected");
        }
        assert!(require_email("first.last@mail.example.co.uk").is_ok());
    }

    #[test]
    fn caller_roles_come_from_the_verified_caller_not_the_header() {
        // The C6 regression note is closed: roles come from the
        // server-verified caller (resolved from the database at
        // authentication time), never from a self-asserted header.
        // A request carrying only the old `x-user-roles` header
        // has no roles at all.
        use actix_web::test::TestRequest;
        let req = {
            use actix_web::HttpMessage;
            let req = TestRequest::default().to_http_request();
            req.extensions_mut().insert(
                unified_core::auth::Caller::verified(test_user_uuid("user-1"), Vec::new())
                    .with_roles(vec!["admin".to_string(), "editor".to_string()]),
            );
            req
        };
        assert_eq!(caller_roles(&req), vec!["admin".to_string(), "editor".to_string()]);
        let header_only = TestRequest::default()
            .insert_header(("x-user-roles", "admin, editor"))
            .to_http_request();
        assert!(caller_roles(&header_only).is_empty());
        let bare = TestRequest::default().to_http_request();
        assert!(caller_roles(&bare).is_empty());
    }

    #[test]
    fn valid_ticket_priority_accepts_closed_vocabulary() {
        for priority in ["low", "normal", "high", "urgent"] {
            assert!(valid_ticket_priority(priority));
        }
        assert!(!valid_ticket_priority("critical"));
        assert!(!valid_ticket_priority(""));
    }

    #[test]
    fn clean_optional_text_passes_none_and_validates_some() {
        assert_eq!(clean_optional_text(&None, "note", 100).unwrap(), None);
        assert_eq!(
            clean_optional_text(&Some("  hi  ".to_string()), "note", 100).unwrap(),
            Some("hi".to_string())
        );
        assert!(clean_optional_text(&Some("".to_string()), "note", 100).is_err());
    }

    #[test]
    fn paginate_returns_the_requested_page() {
        let items: Vec<u32> = (1..=25).collect();
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(10),
            ..Default::default()
        };
        let page = paginate(items, &params);
        assert_eq!(
            page.items,
            vec![11, 12, 13, 14, 15, 16, 17, 18, 19, 20]
        );
        assert_eq!(page.pagination.total, 25);
        assert_eq!(page.pagination.page, 2);
    }

    #[test]
    fn in_company_scope_denies_company_records_to_unresolved_callers() {
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("c1".to_string()), &None));
        assert!(in_company_scope(
            &Some("c1".to_string()),
            &Some("c1".to_string())
        ));
        assert!(!in_company_scope(
            &Some("c2".to_string()),
            &Some("c1".to_string())
        ));
    }

    #[test]
    fn require_company_match_refuses_body_company_from_unresolved_callers() {
        let req = verified_request("user-1", "admin");
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_company_match(&tenant, &req, &Some("c1".to_string())).is_err());
        assert!(require_company_match(&tenant, &req, &None)
            .unwrap()
            .is_none());
    }
}
