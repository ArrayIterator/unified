//! HTTP handlers for IVR menus (key-press trees as data),
//! IVR nodes, auto-response scripts (as data), and the
//! auto-response endpoint (template match, then the AI
//! capability for routine caller questions).

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, created, err, load_menu, load_menu_by_code, load_node, load_script, ok, paginate,
    require_code, require_greeting, require_ivr_key, require_label, require_permission,
    require_template, require_text, resolve_tenant,
};
use super::state::PhoneState;
use super::types::{
    resolve_ivr_key, AutoResponseRequest, AutoResponseScript, CreateIvrMenuRequest,
    CreateIvrNodeRequest, CreateScriptRequest, IvrAction, IvrMenu, IvrNode, ResolveIvrRequest,
    UpdateIvrMenuRequest, UpdateScriptRequest, EVENT_IVR_RESOLVED, MAX_LABEL_LEN,
    PERM_PHONE_MANAGE,
};
use crate::modules::ai;

/// Path carrying a menu id.
#[derive(Debug, Deserialize)]
struct MenuPath {
    menu_id: String,
}

/// Path carrying a menu id and a node id.
#[derive(Debug, Deserialize)]
struct NodePath {
    menu_id: String,
    node_id: String,
}

/// Path carrying a script id.
#[derive(Debug, Deserialize)]
struct ScriptPath {
    script_id: String,
}

fn menu_view(state: &PhoneState, menu: &IvrMenu) -> Value {
    let mut value = serde_json::to_value(menu).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let nodes = state.menu_nodes(menu.id);
        map.insert(
            "nodes".to_string(),
            serde_json::to_value(nodes).unwrap_or(Value::Null),
        );
    }
    value
}

fn require_ivr_action(raw: &str) -> UnifiedResult<IvrAction> {
    IvrAction::from_str(raw).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "Unknown IVR action: {raw} (expected transfer_queue, transfer_user, play_message, run_script, go_to_menu, repeat_menu, or hangup)"
        ))
    })
}

/// Validate a node's target for its action: transfer targets
/// must exist as data (a queue code, a menu code, or a script
/// code); a user target must be a non-empty user id; a
/// play-message node needs a message.
fn validate_node_target(
    state: &PhoneState,
    tenant_id: &str,
    action: IvrAction,
    target_id: &Option<String>,
    message: &Option<String>,
) -> UnifiedResult<()> {
    match action {
        IvrAction::TransferQueue => {
            let Some(code) = target_id else {
                return Err(UnifiedError::bad_request(
                    "transfer_queue requires a target queue code",
                ));
            };
            super::helpers::load_queue_by_code(state, tenant_id, code)?;
            Ok(())
        }
        IvrAction::TransferUser => {
            let Some(user_id) = target_id else {
                return Err(UnifiedError::bad_request(
                    "transfer_user requires a target user id",
                ));
            };
            require_text(user_id, "target_id", MAX_LABEL_LEN)?;
            Ok(())
        }
        IvrAction::PlayMessage => {
            if message.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(UnifiedError::bad_request("play_message requires a message"));
            }
            Ok(())
        }
        IvrAction::RunScript => {
            let Some(code) = target_id else {
                return Err(UnifiedError::bad_request(
                    "run_script requires a target script code",
                ));
            };
            let found = state
                .scripts
                .iter()
                .any(|entry| entry.value().tenant_id == tenant_id && entry.value().code == *code);
            if !found {
                return Err(UnifiedError::not_found(format!(
                    "Auto-response script not found: {code}"
                )));
            }
            Ok(())
        }
        IvrAction::GoToMenu => {
            let Some(code) = target_id else {
                return Err(UnifiedError::bad_request(
                    "go_to_menu requires a target menu code",
                ));
            };
            load_menu_by_code(state, tenant_id, code)?;
            Ok(())
        }
        IvrAction::RepeatMenu | IvrAction::Hangup => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// IVR menu handlers (manager for writes)
// ---------------------------------------------------------------------------

pub(crate) async fn list_ivr_menus(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let active_filter = params.filters.get("active").map(|raw| raw == "true");
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<IvrMenu> = state
            .ivr_menus
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|menu| {
                if menu.tenant_id != tenant_id {
                    return false;
                }
                if let Some(active) = active_filter {
                    if menu.active != active {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    if !menu.code.to_lowercase().contains(needle.as_str())
                        && !menu.label.to_lowercase().contains(needle.as_str())
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        let views: Vec<Value> = items.iter().map(|menu| menu_view(&state, menu)).collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_ivr_menu(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateIvrMenuRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let input = body.into_inner();
        let code = require_code(&input.code, "code")?;
        let label = require_label(&input.label)?;
        let greeting = require_greeting(&input.greeting)?;
        let greeting_audio_ref = match &input.greeting_audio_ref {
            Some(raw) => Some(require_text(raw, "greeting_audio_ref", MAX_LABEL_LEN)?),
            None => None,
        };
        if state.menu_by_code(&tenant_id, &code).is_some() {
            return Err(UnifiedError::conflict(format!(
                "IVR menu code \"{code}\" already exists for this tenant"
            )));
        }
        // Validate every node before inserting anything, so a bad
        // menu never lands half-built.
        let mut validated: Vec<(String, IvrAction, Option<String>, Option<String>)> =
            Vec::with_capacity(input.nodes.len());
        let mut seen_keys: Vec<String> = Vec::new();
        for raw_node in &input.nodes {
            let key = require_ivr_key(&raw_node.key)?;
            if seen_keys.contains(&key) {
                return Err(UnifiedError::bad_request(format!(
                    "IVR menu has two nodes for key \"{key}\""
                )));
            }
            seen_keys.push(key.clone());
            let action = require_ivr_action(&raw_node.action)?;
            let target_id = match &raw_node.target_id {
                Some(raw) => Some(require_text(raw, "target_id", MAX_LABEL_LEN)?),
                None => None,
            };
            let message = match &raw_node.message {
                Some(raw) => Some(require_text(raw, "message", 5000)?),
                None => None,
            };
            validate_node_target(&state, &tenant_id, action, &target_id, &message)?;
            validated.push((key, action, target_id, message));
        }
        let now = Utc::now();
        let menu = IvrMenu {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            code,
            label,
            greeting,
            greeting_audio_ref,
            active: input.active,
            created_at: now,
            updated_at: now,
        };
        state.ivr_menus.insert(menu.id, menu.clone());
        for (key, action, target_id, message) in validated {
            let node = IvrNode {
                id: Uuid::now_v7(),
                tenant_id: tenant_id.clone(),
                menu_id: menu.id,
                key,
                action,
                target_id,
                message,
                created_at: now,
                updated_at: now,
            };
            state.ivr_nodes.insert(node.id, node);
        }
        Ok(menu_view(&state, &menu))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_ivr_menu(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<MenuPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let menu = load_menu(&state, &tenant_id, &path.menu_id)?;
        Ok(menu_view(&state, &menu))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_ivr_menu(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<MenuPath>,
    body: web::Json<UpdateIvrMenuRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let input = body.into_inner();
        let mut menu = load_menu(&state, &tenant_id, &path.menu_id)?;
        if let Some(label) = &input.label {
            menu.label = require_label(label)?;
        }
        if let Some(greeting) = &input.greeting {
            menu.greeting = require_greeting(greeting)?;
        }
        if let Some(audio_ref) = &input.greeting_audio_ref {
            menu.greeting_audio_ref = match audio_ref {
                Some(raw) => Some(require_text(raw, "greeting_audio_ref", MAX_LABEL_LEN)?),
                None => None,
            };
        }
        if let Some(active) = input.active {
            menu.active = active;
        }
        menu.updated_at = Utc::now();
        state.ivr_menus.insert(menu.id, menu.clone());
        Ok(menu_view(&state, &menu))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_ivr_menu(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<MenuPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let menu = load_menu(&state, &tenant_id, &path.menu_id)?;
        state.ivr_menus.remove(&menu.id);
        let node_ids: Vec<Uuid> = state
            .menu_nodes(menu.id)
            .iter()
            .map(|node| node.id)
            .collect();
        for id in node_ids {
            state.ivr_nodes.remove(&id);
        }
        Ok(json!({ "deleted": menu.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// IVR nodes
// ---------------------------------------------------------------------------

pub(crate) async fn list_ivr_nodes(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<MenuPath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let menu = load_menu(&state, &tenant_id, &path.menu_id)?;
        let items = state.menu_nodes(menu.id);
        let views: Vec<Value> = items
            .iter()
            .map(|node| serde_json::to_value(node).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn add_ivr_node(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<MenuPath>,
    body: web::Json<CreateIvrNodeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let input = body.into_inner();
        let menu = load_menu(&state, &tenant_id, &path.menu_id)?;
        let key = require_ivr_key(&input.key)?;
        let action = require_ivr_action(&input.action)?;
        let target_id = match &input.target_id {
            Some(raw) => Some(require_text(raw, "target_id", MAX_LABEL_LEN)?),
            None => None,
        };
        let message = match &input.message {
            Some(raw) => Some(require_text(raw, "message", 5000)?),
            None => None,
        };
        validate_node_target(&state, &tenant_id, action, &target_id, &message)?;
        if state
            .menu_nodes(menu.id)
            .iter()
            .any(|node| node.key.to_lowercase() == key.to_lowercase())
        {
            return Err(UnifiedError::conflict(format!(
                "Menu already has a node for key \"{key}\""
            )));
        }
        let now = Utc::now();
        let node = IvrNode {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            menu_id: menu.id,
            key,
            action,
            target_id,
            message,
            created_at: now,
            updated_at: now,
        };
        state.ivr_nodes.insert(node.id, node.clone());
        Ok(json!(node))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_ivr_node(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NodePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let menu = load_menu(&state, &tenant_id, &path.menu_id)?;
        let node = load_node(&state, &tenant_id, &path.node_id)?;
        if node.menu_id != menu.id {
            return Err(UnifiedError::not_found(format!(
                "IVR node not found: {}",
                node.id
            )));
        }
        state.ivr_nodes.remove(&node.id);
        Ok(json!({ "deleted": node.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Resolve one key press against a menu: returns the node's
/// action and target, plus the next menu when the action is
/// `go_to_menu`, and the queue ring order when the action is
/// `transfer_queue`. An unknown key is not an error — the
/// caller hears the menu again.
pub(crate) async fn resolve_ivr(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<MenuPath>,
    body: web::Json<ResolveIvrRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let menu = load_menu(&state, &tenant_id, &path.menu_id)?;
        if !menu.active {
            return Err(UnifiedError::bad_request(format!(
                "IVR menu \"{}\" is not active",
                menu.code
            )));
        }
        let key = require_ivr_key(&input.key)?;
        let nodes = state.menu_nodes(menu.id);
        let resolved = resolve_ivr_key(&nodes, &key);
        let response = match resolved {
            Some(node) => {
                let mut payload = json!({
                    "menu_id": menu.id.to_string(),
                    "menu_code": menu.code,
                    "key": key,
                    "matched": true,
                    "action": node.action.as_str(),
                    "target_id": node.target_id,
                    "message": node.message,
                });
                if node.action == IvrAction::GoToMenu {
                    if let Some(code) = &node.target_id {
                        if let Some(next) = state.menu_by_code(&tenant_id, code) {
                            if let Some(map) = payload.as_object_mut() {
                                map.insert("next_menu".to_string(), menu_view(&state, &next));
                            }
                        }
                    }
                }
                if node.action == IvrAction::TransferQueue {
                    if let Some(code) = &node.target_id {
                        if let Some(queue) = state.queue_by_code(&tenant_id, code) {
                            if let Some(map) = payload.as_object_mut() {
                                map.insert(
                                    "ring_order".to_string(),
                                    serde_json::to_value(super::types::next_ring_targets(
                                        &state.queue_members(queue.id),
                                        queue.ring_strategy,
                                    ))
                                    .unwrap_or(Value::Null),
                                );
                            }
                        }
                    }
                }
                payload
            }
            None => json!({
                "menu_id": menu.id.to_string(),
                "menu_code": menu.code,
                "key": key,
                "matched": false,
                "action": "repeat_menu",
                "target_id": Value::Null,
                "message": menu.greeting,
            }),
        };
        state.emit(
            EVENT_IVR_RESOLVED,
            json!({
                "tenant_id": tenant_id,
                "menu_id": menu.id.to_string(),
                "menu_code": menu.code,
                "key": key,
                "matched": resolved.is_some(),
                "action": response.get("action").and_then(|v| v.as_str()),
                "caller_id": input.caller_id,
                "actor_id": caller.actor,
            }),
        );
        Ok(response)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Auto-response scripts (manager)
// ---------------------------------------------------------------------------

pub(crate) async fn list_scripts(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let active_filter = params.filters.get("active").map(|raw| raw == "true");
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<AutoResponseScript> = state
            .scripts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|script| {
                if script.tenant_id != tenant_id {
                    return false;
                }
                if let Some(active) = active_filter {
                    if script.active != active {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    if !script.code.to_lowercase().contains(needle.as_str())
                        && !script.label.to_lowercase().contains(needle.as_str())
                        && !script.trigger.to_lowercase().contains(needle.as_str())
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|script| script.priority);
        let views: Vec<Value> = items
            .iter()
            .map(|script| serde_json::to_value(script).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_script(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateScriptRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let input = body.into_inner();
        let code = require_code(&input.code, "code")?;
        let label = require_label(&input.label)?;
        let trigger = require_text(&input.trigger, "trigger", MAX_LABEL_LEN)?;
        let response_template = require_template(&input.response_template)?;
        let ai_capability = match &input.ai_capability {
            Some(raw) => Some(require_text(raw, "ai_capability", MAX_LABEL_LEN)?),
            None => None,
        };
        if response_template.is_none() && ai_capability.is_none() {
            return Err(UnifiedError::bad_request(
                "A script needs either a response_template or an ai_capability",
            ));
        }
        if let Some(capability) = &ai_capability {
            if !crate::modules::phone::ai_capabilities::AI_CAPABILITIES
                .iter()
                .any(|item| item.id == capability)
            {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown phone AI capability: {capability}"
                )));
            }
        }
        if state
            .scripts
            .iter()
            .any(|entry| entry.value().tenant_id == tenant_id && entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Auto-response script code \"{code}\" already exists for this tenant"
            )));
        }
        let now = Utc::now();
        let script = AutoResponseScript {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            code,
            label,
            trigger,
            response_template,
            ai_capability,
            priority: input.priority,
            active: input.active,
            created_at: now,
            updated_at: now,
        };
        state.scripts.insert(script.id, script.clone());
        Ok(json!(script))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_script(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<ScriptPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let script = load_script(&state, &tenant_id, &path.script_id)?;
        Ok(json!(script))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_script(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<ScriptPath>,
    body: web::Json<UpdateScriptRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let input = body.into_inner();
        let mut script = load_script(&state, &tenant_id, &path.script_id)?;
        if let Some(label) = &input.label {
            script.label = require_label(label)?;
        }
        if let Some(trigger) = &input.trigger {
            script.trigger = require_text(trigger, "trigger", MAX_LABEL_LEN)?;
        }
        if let Some(template) = &input.response_template {
            script.response_template = require_template(template)?;
        }
        if let Some(capability) = &input.ai_capability {
            script.ai_capability = match capability {
                Some(raw) => Some(require_text(raw, "ai_capability", MAX_LABEL_LEN)?),
                None => None,
            };
            if let Some(name) = &script.ai_capability {
                if !crate::modules::phone::ai_capabilities::AI_CAPABILITIES
                    .iter()
                    .any(|item| item.id == name)
                {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown phone AI capability: {name}"
                    )));
                }
            }
        }
        if let Some(priority) = input.priority {
            script.priority = priority;
        }
        if let Some(active) = input.active {
            script.active = active;
        }
        if script.response_template.is_none() && script.ai_capability.is_none() {
            return Err(UnifiedError::bad_request(
                "A script needs either a response_template or an ai_capability",
            ));
        }
        script.updated_at = Utc::now();
        state.scripts.insert(script.id, script.clone());
        Ok(json!(script))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_script(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<ScriptPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PHONE_MANAGE)?;
        let script = load_script(&state, &tenant_id, &path.script_id)?;
        state.scripts.remove(&script.id);
        Ok(json!({ "deleted": script.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Auto-response: answer a routine caller question
// ---------------------------------------------------------------------------

/// Answer a routine caller question. The tenant's active
/// scripts are tried first, in priority order: a trigger
/// match answers from its template, or hands the question to
/// the script's AI capability. When no script matches, the
/// `answer_caller_question` capability answers directly (the
/// grounding hook supplies the active menu and scripts). The
/// AI only answers and may suggest a route or escalation — it
/// never books, charges, or changes anything on its own.
pub(crate) async fn auto_response(
    state: web::Data<PhoneState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<AutoResponseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let question = match require_text(&input.question, "question", 5000) {
        Ok(question) => question,
        Err(error) => return err(error, debug),
    };
    let tenant_id = match resolve_tenant(&req) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    let caller_id = match require_text(&input.caller_id, "caller_id", MAX_LABEL_LEN) {
        Ok(caller_id) => caller_id,
        Err(error) => return err(error, debug),
    };
    let needle = question.to_lowercase();
    let matched = state
        .active_scripts(&tenant_id)
        .into_iter()
        .find(|script| needle.contains(&script.trigger.to_lowercase()));
    if let Some(script) = matched {
        if let Some(template) = &script.response_template {
            return ok(json!({
                "source": "script",
                "script_code": script.code,
                "answer": template,
                "caller_id": caller_id,
            }));
        }
        if let Some(capability) = &script.ai_capability {
            return run_auto_response_capability(
                &state,
                &ctx,
                &req,
                &caller.actor,
                &tenant_id,
                capability,
                &caller_id,
                &question,
                input.call_id,
                input.menu_code.clone(),
                Some(script.code.clone()),
            )
            .await;
        }
    }
    run_auto_response_capability(
        &state,
        &ctx,
        &req,
        &caller.actor,
        &tenant_id,
        "answer_caller_question",
        &caller_id,
        &question,
        input.call_id,
        input.menu_code,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_auto_response_capability(
    _state: &PhoneState,
    ctx: &web::Data<Arc<AppContext>>,
    req: &HttpRequest,
    _actor: &str,
    tenant_id: &str,
    capability: &str,
    caller_id: &str,
    question: &str,
    call_id: Option<Uuid>,
    menu_code: Option<String>,
    script_code: Option<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let ai_caller = match ai::caller(req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = ai::require_permission(ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return err(error, debug);
    }
    let company_id = ai::resolve_capability_company(ctx, req, &ai_caller);
    let input = json!({
        "tenant_id": tenant_id,
        "caller_id": caller_id,
        "question": question,
        "call_id": call_id.map(|id| id.to_string()),
        "menu_code": menu_code,
        "script_code": script_code,
    });
    match ai::run_ai_capability(ctx, &ai_caller, company_id, "phone", capability, input).await {
        Ok(output) => ok(json!({
            "source": "ai",
            "capability": capability,
            "script_code": script_code,
            "caller_id": caller_id,
            "output": output,
        })),
        Err(error) => err(error, debug),
    }
}
