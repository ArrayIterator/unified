//! chat - support agents.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_canned::*, support_offline::*};



// Live support: agents
pub(crate) async fn list_agents(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<SupportAgent>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let department_filter = params.filters.get("department_id").cloned();
        let available_filter = params.filters.get("available").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<SupportAgent> = state
            .agents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|agent| {
                if let Some(department) = &department_filter {
                    if !agent.department_ids.iter().any(|id| id == department) {
                        return false;
                    }
                }
                if let Some(available) = &available_filter {
                    let expected = available == "true";
                    let effective = agent_availability_rank(&state, agent).is_some();
                    if effective != expected {
                        return false;
                    }
                }
                if !search.is_empty() && !agent.user_id.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}


/// An agent registers (or updates) their own routing record. The caller
/// is the agent: `X-User-Id` is the roster key.
pub(crate) async fn register_agent(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<UpsertAgentRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SupportAgent> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let body = body.into_inner();
        let now = Utc::now();
        let mut agent = state
            .agents
            .get(&actor)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| SupportAgent {
                user_id: actor.clone(),
                department_ids: Vec::new(),
                available: true,
                max_concurrent: DEFAULT_MAX_CONCURRENT_CHATS,
                created_at: now,
                updated_at: now,
            });
        if let Some(departments) = body.department_ids {
            agent.department_ids = clean_department_ids(Some(departments))?;
        }
        if let Some(available) = body.available {
            agent.available = available;
        }
        if let Some(max_concurrent) = body.max_concurrent {
            if max_concurrent == 0 {
                return Err(UnifiedError::bad_request(
                    "max_concurrent must be at least 1",
                ));
            }
            agent.max_concurrent = max_concurrent;
        }
        agent.updated_at = now;
        state.agents.insert(actor, agent.clone());
        Ok(agent)
    })();
    match result {
        Ok(agent) => ok(agent),
        Err(error) => err(error),
    }
}


pub(crate) fn load_agent(state: &ChatState, user_id: &str) -> UnifiedResult<SupportAgent> {
    state
        .agents
        .get(user_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Agent not registered: {user_id}")))
}


pub(crate) async fn get_agent(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SupportAgent> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        load_agent(&state, &path.into_inner())
    })();
    match result {
        Ok(agent) => ok(agent),
        Err(error) => err(error),
    }
}


pub(crate) async fn update_agent(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAgentRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SupportAgent> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let user_id = path.into_inner();
        if actor != user_id {
            return Err(UnifiedError::forbidden(
                "Only the agent themselves can update their routing record",
            ));
        }
        let body = body.into_inner();
        let mut agent = load_agent(&state, &user_id)?;
        if let Some(departments) = body.department_ids {
            agent.department_ids = clean_department_ids(Some(departments))?;
        }
        if let Some(available) = body.available {
            agent.available = available;
        }
        if let Some(max_concurrent) = body.max_concurrent {
            if max_concurrent == 0 {
                return Err(UnifiedError::bad_request(
                    "max_concurrent must be at least 1",
                ));
            }
            agent.max_concurrent = max_concurrent;
        }
        agent.updated_at = Utc::now();
        state.agents.insert(user_id, agent.clone());
        Ok(agent)
    })();
    match result {
        Ok(agent) => ok(agent),
        Err(error) => err(error),
    }
}


pub(crate) async fn remove_agent(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let user_id = path.into_inner();
        if actor != user_id {
            return Err(UnifiedError::forbidden(
                "Only the agent themselves can remove their routing record",
            ));
        }
        let _agent = load_agent(&state, &user_id)?;
        state.agents.remove(&user_id);
        Ok(json!({ "deleted": true, "user_id": user_id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}
