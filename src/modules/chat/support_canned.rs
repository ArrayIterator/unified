//! chat - support canned.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use uuid::Uuid;
use super::{limits::*, types::*, hub::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_offline::*};



// Live support: canned responses
pub(crate) async fn list_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<CannedResponse>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        let params = query.into_inner();
        let category_filter = params.filters.get("category").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<CannedResponse> = state
            .canned
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|canned| {
                if let Some(category) = &category_filter {
                    if &canned.category != category {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if canned.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !canned.title.to_lowercase().contains(&search)
                    && !canned.body.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "category", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.cmp(&b.title),
                "category" => a.category.cmp(&b.category),
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


pub(crate) async fn create_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    body: web::Json<CreateCannedRequest>,
) -> HttpResponse {
    let result: UnifiedResult<CannedResponse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let body = body.into_inner();
        let now = Utc::now();
        let canned = CannedResponse {
            id: Uuid::now_v7(),
            title: require_text(&body.title, "title", MAX_CANNED_TITLE_LEN)?,
            body: require_text(&body.body, "body", MAX_CANNED_BODY_LEN)?,
            category: match &body.category {
                Some(raw) => require_text(raw, "category", MAX_NAME_LEN)?,
                None => "general".to_string(),
            },
            department_id: clean_optional_text(
                &body.department_id,
                "department_id",
                MAX_DEPARTMENT_LEN,
            )?,
            created_by: actor.clone(),
            usage_count: 0,
            created_at: now,
            updated_at: now,
        };
        state.canned.insert(canned.id, canned.clone());
        state.emit(
            "chat.canned_response.created",
            json!({
                "canned_response_id": canned.id.to_string(),
                "title": canned.title.clone(),
                "category": canned.category.clone(),
                "department_id": canned.department_id.clone(),
                "created_by": actor,
            }),
        );
        Ok(canned)
    })();
    match result {
        Ok(canned) => created(canned),
        Err(error) => err(error),
    }
}


pub(crate) fn load_canned(state: &ChatState, raw_id: &str) -> UnifiedResult<CannedResponse> {
    let id = parse_uuid(raw_id, "canned response")?;
    state
        .canned
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Canned response not found: {id}")))
}


pub(crate) async fn get_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<CannedResponse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _actor = actor_id(&req)?;
        load_canned(&state, &path.into_inner())
    })();
    match result {
        Ok(canned) => ok(canned),
        Err(error) => err(error),
    }
}


pub(crate) async fn update_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCannedRequest>,
) -> HttpResponse {
    let result: UnifiedResult<CannedResponse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let body = body.into_inner();
        let mut canned = load_canned(&state, &path.into_inner())?;
        if canned.created_by != actor {
            return Err(UnifiedError::forbidden(
                "Only the canned response author can update it",
            ));
        }
        if let Some(title) = &body.title {
            canned.title = require_text(title, "title", MAX_CANNED_TITLE_LEN)?;
        }
        if let Some(body_text) = &body.body {
            canned.body = require_text(body_text, "body", MAX_CANNED_BODY_LEN)?;
        }
        if let Some(category) = &body.category {
            canned.category = require_text(category, "category", MAX_NAME_LEN)?;
        }
        if let Some(department_id) = &body.department_id {
            canned.department_id = match department_id {
                Some(raw) => Some(require_text(raw, "department_id", MAX_DEPARTMENT_LEN)?),
                None => None,
            };
        }
        canned.updated_at = Utc::now();
        state.canned.insert(canned.id, canned.clone());
        state.emit(
            "chat.canned_response.updated",
            json!({
                "canned_response_id": canned.id.to_string(),
                "updated_by": actor,
            }),
        );
        Ok(canned)
    })();
    match result {
        Ok(canned) => ok(canned),
        Err(error) => err(error),
    }
}


pub(crate) async fn delete_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let canned = load_canned(&state, &path.into_inner())?;
        if canned.created_by != actor {
            return Err(UnifiedError::forbidden(
                "Only the canned response author can delete it",
            ));
        }
        state.canned.remove(&canned.id);
        state.emit(
            "chat.canned_response.deleted",
            json!({
                "canned_response_id": canned.id.to_string(),
                "deleted_by": actor,
            }),
        );
        Ok(json!({ "deleted": true, "canned_response_id": canned.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error),
    }
}


/// Use a canned response: counts the use (so teams can see which replies
/// actually help) and returns the body for the agent to send.
pub(crate) async fn use_canned(
    state: web::Data<ChatState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UseCannedRequest>,
) -> HttpResponse {
    let result: UnifiedResult<CannedResponse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let actor = actor_id(&req)?;
        let mut canned = load_canned(&state, &path.into_inner())?;
        canned.usage_count = canned.usage_count.saturating_add(1);
        canned.updated_at = Utc::now();
        state.canned.insert(canned.id, canned.clone());
        state.emit(
            "chat.canned_response.used",
            json!({
                "canned_response_id": canned.id.to_string(),
                "used_by": actor,
                "session_id": body.session_id.clone(),
            }),
        );
        Ok(canned)
    })();
    match result {
        Ok(canned) => ok(canned),
        Err(error) => err(error),
    }
}
