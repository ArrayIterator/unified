// todos - access.

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
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::validation::*;
use super::service::*;
use super::handlers_lists::*;
use super::handlers_todos::*;
use super::handlers_subtasks::*;
use super::handlers_ai::*;
use super::module::*;

// Helpers: access control (fail closed, 404 - never 403 - for objects
// the caller does not own and has no share for).

pub(crate) fn share_key(list_id: Uuid, user_id: &str) -> String {
    format!("{list_id}:{user_id}")
}

pub(crate) fn share_for(state: &TodosState, list_id: Uuid, user_id: &str) -> Option<TodoShare> {
    state
        .shares
        .get(&share_key(list_id, user_id))
        .map(|entry| entry.value().clone())
}

/// True when `user_id` may read the list (owner, or any share).
pub(crate) fn can_read_list(state: &TodosState, list: &TodoList, user_id: &str) -> bool {
    list.owner_id == user_id || share_for(state, list.id, user_id).is_some()
}

/// True when `user_id` may change the list and its todos (owner, or
/// an edit share). A read share is read-only.
pub(crate) fn can_edit_list(state: &TodosState, list: &TodoList, user_id: &str) -> bool {
    list.owner_id == user_id
        || share_for(state, list.id, user_id)
            .is_some_and(|share| share.permission == SharePermission::Edit)
}

pub(crate) fn not_found_list(id: Uuid) -> UnifiedError {
    UnifiedError::not_found(format!("Todo list not found: {id}"))
}

pub(crate) fn not_found_todo(id: Uuid) -> UnifiedError {
    UnifiedError::not_found(format!("Todo not found: {id}"))
}

pub(crate) fn not_found_share(list_id: Uuid, user_id: &str) -> UnifiedError {
    UnifiedError::not_found(format!(
        "Share not found for user {user_id} on list {list_id}"
    ))
}

pub(crate) fn load_list(state: &TodosState, id: Uuid) -> UnifiedResult<TodoList> {
    state
        .lists
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| not_found_list(id))
}

pub(crate) fn load_list_for_read(
    state: &TodosState,
    id: Uuid,
    user_id: &str,
) -> UnifiedResult<TodoList> {
    let list = load_list(state, id)?;
    if can_read_list(state, &list, user_id) {
        Ok(list)
    } else {
        Err(not_found_list(id))
    }
}

pub(crate) fn load_list_for_edit(
    state: &TodosState,
    id: Uuid,
    user_id: &str,
) -> UnifiedResult<TodoList> {
    let list = load_list(state, id)?;
    if can_edit_list(state, &list, user_id) {
        Ok(list)
    } else {
        Err(not_found_list(id))
    }
}

pub(crate) fn load_list_owned(state: &TodosState, id: Uuid, user_id: &str) -> UnifiedResult<TodoList> {
    let list = load_list(state, id)?;
    if list.owner_id == user_id {
        Ok(list)
    } else {
        Err(not_found_list(id))
    }
}

pub(crate) fn load_todo(state: &TodosState, id: Uuid) -> UnifiedResult<Todo> {
    state
        .todos
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| not_found_todo(id))
}

/// Load a todo the caller may read (owner, or a share on its list).
/// Returns the todo plus whether the caller may edit it.
pub(crate) fn load_todo_for_read(
    state: &TodosState,
    id: Uuid,
    user_id: &str,
) -> UnifiedResult<(Todo, bool)> {
    let todo = load_todo(state, id)?;
    let list = load_list(state, todo.list_id)?;
    if can_read_list(state, &list, user_id) {
        Ok((todo, can_edit_list(state, &list, user_id)))
    } else {
        Err(not_found_todo(id))
    }
}

/// Load a todo the caller may change. The todo still belongs to its
/// owner: a shared editor changes the owner's todo, never their own
/// copy (there is no copy).
pub(crate) fn load_todo_for_edit(
    state: &TodosState,
    id: Uuid,
    user_id: &str,
) -> UnifiedResult<Todo> {
    let todo = load_todo(state, id)?;
    let list = load_list(state, todo.list_id)?;
    if can_edit_list(state, &list, user_id) {
        Ok(todo)
    } else {
        Err(not_found_todo(id))
    }
}

/// Lists the caller may read: their own plus lists shared with them.
pub(crate) fn readable_lists(state: &TodosState, user_id: &str) -> Vec<TodoList> {
    state
        .lists
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|list| can_read_list(state, list, user_id))
        .collect()
}

/// The caller's default ("Inbox") list, creating it lazily when the
/// user has none. The default list is the quick-capture target.
///
/// Creation is serialised per user: the fast path checks first
/// without a lock, then the per-user lock is held while the check
/// runs again and the list is inserted, so two concurrent first
/// requests for one user create exactly one default list.
pub(crate) fn ensure_default_list(state: &TodosState, user_id: &str) -> TodoList {
    let find_default = |state: &TodosState| {
        state
            .lists
            .iter()
            .map(|entry| entry.value().clone())
            .find(|list| list.owner_id == user_id && list.is_default)
    };
    if let Some(list) = find_default(state) {
        return list;
    }
    let lock = {
        let entry = state
            .default_list_locks
            .entry(user_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())));
        (*entry).clone()
    };
    let _guard = lock.lock().expect("default-list lock poisoned");
    if let Some(list) = find_default(state) {
        return list;
    }
    let now = Utc::now();
    let list = TodoList {
        id: Uuid::now_v7(),
        owner_id: user_id.to_string(),
        name: DEFAULT_LIST_NAME.to_string(),
        color: "blue".to_string(),
        icon: "inbox".to_string(),
        archived: false,
        sort_order: 0,
        is_default: true,
        created_at: now,
        updated_at: now,
    };
    state.lists.insert(list.id, list.clone());
    state.emit(
        EVENT_LIST_CREATED,
        json!({
            "list_id": list.id.to_string(),
            "owner_id": user_id,
            "name": list.name,
            "is_default": true,
            "actor_id": user_id,
            "resource": format!("todo_list:{}", list.id),
        }),
    );
    list
}

pub(crate) fn next_position(state: &TodosState, list_id: Uuid) -> i32 {
    state
        .todos
        .iter()
        .filter(|entry| entry.value().list_id == list_id)
        .map(|entry| entry.value().position)
        .max()
        .map_or(0, |max| max + 1)
}

pub(crate) fn log_completion(
    state: &TodosState,
    todo_id: Uuid,
    user_id: &str,
    action: &str,
) -> TodoCompletionLog {
    let now = Utc::now();
    let entry = TodoCompletionLog {
        id: Uuid::now_v7(),
        todo_id,
        user_id: user_id.to_string(),
        action: action.to_string(),
        created_at: now,
        updated_at: now,
    };
    state.completion_log.insert(entry.id, entry.clone());
    entry
}

pub(crate) fn todo_event_payload(todo: &Todo, actor: &str) -> Value {
    json!({
        "todo_id": todo.id.to_string(),
        "id": todo.id.to_string(),
        "list_id": todo.list_id.to_string(),
        "owner_id": todo.owner_id,
        "title": todo.title,
        "status": todo.status.as_str(),
        "priority": todo.priority.as_str(),
        "due_at": todo.due_at,
        "actor_id": actor,
        "entity_type": "todo",
        "entity_id": todo.id.to_string(),
        "resource": format!("todo:{}", todo.id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_share_reports_the_share_not_the_list() {
        let list_id = Uuid::new_v4();
        let error = not_found_share(list_id, "user-2");
        assert_eq!(error.code, 404);
        assert!(error.message.contains("Share not found"));
        assert!(error.message.contains("user-2"));
        assert!(error.message.contains(&list_id.to_string()));
    }

    #[test]
    fn concurrent_first_requests_create_one_default_list() {
        let state = TodosState::default();
        let ids: Vec<Uuid> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let mut last = Uuid::new_v4();
                        for _ in 0..10 {
                            last = ensure_default_list(&state, "user-1").id;
                        }
                        last
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("worker thread panicked"))
                .collect()
        });
        assert!(ids.iter().all(|id| *id == ids[0]));
        let defaults = state
            .lists
            .iter()
            .filter(|entry| entry.value().owner_id == "user-1" && entry.value().is_default)
            .count();
        assert_eq!(defaults, 1);
    }
}
