// todos — handlers lists.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::validation::*;
use super::access::*;
use super::service::*;
use super::handlers_todos::*;
use super::handlers_subtasks::*;
use super::handlers_ai::*;
use super::module::*;

// ---------------------------------------------------------------------------
// List handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn list_lists(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TodoList>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let params = query.into_inner();
        let mut items = readable_lists(&state, &caller.actor);
        if let Some(archived) = params.filters.get("archived") {
            let want = archived == "true" || archived == "1";
            items.retain(|list| list.archived == want);
        }
        if let Some(owner) = params.filters.get("owner_id") {
            items.retain(|list| &list.owner_id == owner);
        }
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        if !search.is_empty() {
            items.retain(|list| list.name.to_lowercase().contains(&search));
        }
        let sort = params.allowed_sort(&["name", "sort_order", "created_at"], "sort_order");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "created_at" => a.created_at.cmp(&b.created_at),
                _ => a.sort_order.cmp(&b.sort_order),
            };
            if descending { ordering.reverse() } else { ordering }
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    body: web::Json<CreateListRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoList> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_LIST_NAME_LEN)?;
        let color = match &input.color {
            Some(raw) => validate_color(raw)?,
            None => "blue".to_string(),
        };
        let icon = match &input.icon {
            Some(raw) => validate_icon(raw)?,
            None => "folder".to_string(),
        };
        let now = Utc::now();
        let list = TodoList {
            id: Uuid::now_v7(),
            owner_id: caller.actor.clone(),
            name,
            color,
            icon,
            archived: false,
            sort_order: input.sort_order.unwrap_or(0),
            is_default: false,
            created_at: now,
            updated_at: now,
        };
        state.lists.insert(list.id, list.clone());
        state.emit(
            EVENT_LIST_CREATED,
            json!({
                "list_id": list.id.to_string(),
                "owner_id": caller.actor,
                "name": list.name,
                "color": list.color,
                "icon": list.icon,
                "actor_id": caller.actor,
                "resource": format!("todo_list:{}", list.id),
            }),
        );
        Ok(list)
    })();
    match result {
        Ok(list) => created(list),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoList> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        load_list_for_read(&state, id.into_inner(), &caller.actor)
    })();
    match result {
        Ok(list) => ok(list),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateListRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoList> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let list_id = id.into_inner();
        let input = body.into_inner();
        let mut list = load_list_owned(&state, list_id, &caller.actor)?;
        if let Some(name) = &input.name {
            list.name = require_text(name, "name", MAX_LIST_NAME_LEN)?;
        }
        if let Some(color) = &input.color {
            list.color = validate_color(color)?;
        }
        if let Some(icon) = &input.icon {
            list.icon = validate_icon(icon)?;
        }
        if let Some(order) = input.sort_order {
            list.sort_order = order;
        }
        list.updated_at = Utc::now();
        state.lists.insert(list.id, list.clone());
        state.emit(
            EVENT_LIST_UPDATED,
            json!({
                "list_id": list.id.to_string(),
                "owner_id": list.owner_id,
                "name": list.name,
                "actor_id": caller.actor,
                "resource": format!("todo_list:{}", list.id),
            }),
        );
        Ok(list)
    })();
    match result {
        Ok(list) => ok(list),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let list_id = id.into_inner();
        let list = load_list_owned(&state, list_id, &caller.actor)?;
        if list.is_default {
            return Err(UnifiedError::bad_request(
                "The default list cannot be deleted: move its todos to another list first",
            ));
        }
        if state.todos.iter().any(|entry| entry.value().list_id == list_id) {
            return Err(UnifiedError::bad_request(
                "The list still contains todos: move or delete them first",
            ));
        }
        state.lists.remove(&list_id);
        state
            .shares
            .retain(|_, share| share.list_id != list_id);
        state.emit(
            EVENT_LIST_DELETED,
            json!({
                "list_id": list_id.to_string(),
                "owner_id": list.owner_id,
                "actor_id": caller.actor,
                "resource": format!("todo_list:{list_id}"),
            }),
        );
        Ok(json!({ "deleted": true, "id": list_id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn set_list_archived(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    archived: bool,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoList> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_WRITE)?;
        let list_id = id.into_inner();
        let mut list = load_list_owned(&state, list_id, &caller.actor)?;
        list.archived = archived;
        list.updated_at = Utc::now();
        state.lists.insert(list.id, list.clone());
        state.emit(
            if archived { EVENT_LIST_ARCHIVED } else { EVENT_LIST_UNARCHIVED },
            json!({
                "list_id": list.id.to_string(),
                "owner_id": list.owner_id,
                "archived": archived,
                "actor_id": caller.actor,
                "resource": format!("todo_list:{}", list.id),
            }),
        );
        Ok(list)
    })();
    match result {
        Ok(list) => ok(list),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn archive_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    set_list_archived(ctx, state, req, id, true).await
}

pub(crate) async fn unarchive_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    set_list_archived(ctx, state, req, id, false).await
}

pub(crate) async fn list_shares(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<TodoShare>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let list_id = id.into_inner();
        let _list = load_list_owned(&state, list_id, &caller.actor)?;
        let mut shares: Vec<TodoShare> = state
            .shares
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|share| share.list_id == list_id)
            .collect();
        shares.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(shares)
    })();
    match result {
        Ok(shares) => ok(shares),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn share_list(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<ShareListRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TodoShare> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_SHARE)?;
        let list_id = id.into_inner();
        let input = body.into_inner();
        let list = load_list_owned(&state, list_id, &caller.actor)?;
        let target = input.user_id.trim().to_string();
        if target.is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        if target == caller.actor {
            return Err(UnifiedError::bad_request(
                "A list cannot be shared with its own owner",
            ));
        }
        let permission = input.permission.unwrap_or(SharePermission::Read);
        let now = Utc::now();
        let share = TodoShare {
            id: Uuid::now_v7(),
            list_id,
            owner_id: list.owner_id.clone(),
            shared_with_user_id: target.clone(),
            permission,
            created_at: now,
            updated_at: now,
        };
        state
            .shares
            .insert(share_key(list_id, &target), share.clone());
        state.emit(
            EVENT_LIST_SHARED,
            json!({
                "list_id": list_id.to_string(),
                "owner_id": list.owner_id,
                "shared_with_user_id": target,
                "permission": permission.as_str(),
                "actor_id": caller.actor,
                "resource": format!("todo_list:{list_id}"),
            }),
        );
        Ok(share)
    })();
    match result {
        Ok(share) => created(share),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn revoke_share(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    path: web::Path<(Uuid, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_SHARE)?;
        let (list_id, target) = path.into_inner();
        let list = load_list_owned(&state, list_id, &caller.actor)?;
        let key = share_key(list_id, target.trim());
        match state.shares.remove(&key) {
            Some(_) => {
                state.emit(
                    EVENT_LIST_SHARE_REVOKED,
                    json!({
                        "list_id": list_id.to_string(),
                        "owner_id": list.owner_id,
                        "shared_with_user_id": target,
                        "actor_id": caller.actor,
                        "resource": format!("todo_list:{list_id}"),
                    }),
                );
                Ok(json!({ "revoked": true, "list_id": list_id, "user_id": target }))
            }
            None => Err(not_found_share(list_id, target.trim())),
        }
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_shared_with_me(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TodosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TODOS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .shares
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|share| share.shared_with_user_id == caller.actor)
            .filter_map(|share| {
                state.lists.get(&share.list_id).map(|entry| {
                    let list = entry.value().clone();
                    json!({
                        "list": list,
                        "permission": share.permission.as_str(),
                        "owner_id": share.owner_id,
                        "shared_at": share.created_at,
                    })
                })
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("list")
                .and_then(|list| list.get("name"))
                .and_then(Value::as_str)
                .cmp(
                    &b.get("list")
                        .and_then(|list| list.get("name"))
                        .and_then(Value::as_str),
                )
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}
