//! Portals - menu handlers.
//!
//! Part of the `portals` module split; see `src/modules/portals.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use chrono::Utc;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::constants::MAX_LABEL_LEN;
use super::constants::MAX_MENUS_PER_PORTAL;
use super::constants::PERM_PORTALS_MANAGE;
use super::constants::PERM_PORTALS_VIEW;
use super::helpers::caller;
use super::helpers::caller_company_scope;
use super::helpers::created;
use super::helpers::err;
use super::helpers::load_menu;
use super::helpers::load_portal;
use super::helpers::menus_for;
use super::helpers::ok;
use super::helpers::paginate;
use super::helpers::require_permission;
use super::helpers::require_record_in_company_scope_str;
use super::helpers::require_text;
use super::helpers::require_tenant;
use super::helpers::validate_menu_target;
use super::helpers::validate_roles;
use super::state::PortalsState;
use super::types::CreateMenuRequest;
use super::types::PortalMenu;
use super::types::ReorderMenusRequest;
use super::types::UpdateMenuRequest;

// ---------------------------------------------------------------------------
// Menu handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_menus(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let portal = load_portal(&state, &path.into_inner())?;
        // Menus carry no company of their own: they are scoped
        // through their parent portal.
        require_record_in_company_scope_str(&portal.company_id, &scope)?;
        let params = query.into_inner();
        let items = menus_for(&state, portal.id);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_menu(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateMenuRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PortalMenu> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let portal = load_portal(&state, &path.into_inner())?;
        // Menus carry no company of their own: they are scoped
        // through their parent portal.
        require_record_in_company_scope_str(&portal.company_id, &scope)?;
        let input = body.into_inner();
        if menus_for(&state, portal.id).len() >= MAX_MENUS_PER_PORTAL {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_MENUS_PER_PORTAL} menu entries are allowed per portal"
            )));
        }
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        let target_kind = input.target_kind.trim().to_string();
        let target = input.target.trim().to_string();
        validate_menu_target(&target_kind, &target)?;
        validate_roles(&input.roles)?;
        let roles: Vec<String> = input
            .roles
            .iter()
            .map(|role| role.trim().to_string())
            .collect();
        let position = match input.position {
            Some(position) if position >= 0 => position,
            Some(_) => return Err(UnifiedError::bad_request("menu position must be >= 0")),
            None => {
                menus_for(&state, portal.id)
                    .iter()
                    .map(|menu| menu.position)
                    .max()
                    .unwrap_or(-1)
                    + 1
            }
        };
        let now = Utc::now();
        let menu = PortalMenu {
            id: Uuid::now_v7(),
            portal_id: portal.id,
            label,
            target_kind,
            target,
            position,
            visible: input.visible,
            roles,
            created_at: now,
            updated_at: now,
        };
        state.menus.insert(menu.id, menu.clone());
        state.emit(
            "portals.menu.updated",
            json!({
                "portal_id": portal.id.to_string(),
                "menu_id": menu.id.to_string(),
                "action": "created",
            }),
        );
        Ok(menu)
    })();
    match result {
        Ok(menu) => created(menu),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_menu(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PortalMenu> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let (portal_id, menu_id) = path.into_inner();
        let portal = load_portal(&state, &portal_id)?;
        // Menus carry no company of their own: they are scoped
        // through their parent portal.
        require_record_in_company_scope_str(&portal.company_id, &scope)?;
        load_menu(&state, portal.id, &menu_id)
    })();
    match result {
        Ok(menu) => ok(menu),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_menu(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateMenuRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PortalMenu> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let (portal_id, menu_id) = path.into_inner();
        let portal = load_portal(&state, &portal_id)?;
        // Menus carry no company of their own: they are scoped
        // through their parent portal.
        require_record_in_company_scope_str(&portal.company_id, &scope)?;
        let mut menu = load_menu(&state, portal.id, &menu_id)?;
        let input = body.into_inner();
        if let Some(label) = &input.label {
            menu.label = require_text(label, "label", MAX_LABEL_LEN)?;
        }
        if let Some(target_kind) = &input.target_kind {
            menu.target_kind = target_kind.trim().to_string();
        }
        if let Some(target) = &input.target {
            menu.target = target.trim().to_string();
        }
        validate_menu_target(&menu.target_kind, &menu.target)?;
        if let Some(position) = input.position {
            if position < 0 {
                return Err(UnifiedError::bad_request("menu position must be >= 0"));
            }
            menu.position = position;
        }
        if let Some(visible) = input.visible {
            menu.visible = visible;
        }
        if let Some(roles) = &input.roles {
            validate_roles(roles)?;
            menu.roles = roles.iter().map(|role| role.trim().to_string()).collect();
        }
        menu.updated_at = Utc::now();
        state.menus.insert(menu.id, menu.clone());
        state.emit(
            "portals.menu.updated",
            json!({
                "portal_id": portal.id.to_string(),
                "menu_id": menu.id.to_string(),
                "action": "updated",
            }),
        );
        Ok(menu)
    })();
    match result {
        Ok(menu) => ok(menu),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_menu(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let (portal_id, menu_id) = path.into_inner();
        let portal = load_portal(&state, &portal_id)?;
        // Menus carry no company of their own: they are scoped
        // through their parent portal.
        require_record_in_company_scope_str(&portal.company_id, &scope)?;
        let menu = load_menu(&state, portal.id, &menu_id)?;
        state.menus.remove(&menu.id);
        state.emit(
            "portals.menu.updated",
            json!({
                "portal_id": portal.id.to_string(),
                "menu_id": menu.id.to_string(),
                "action": "deleted",
            }),
        );
        Ok(json!({ "deleted": true, "menu_id": menu.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reorder_menus(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReorderMenusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<PortalMenu>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let scope = caller_company_scope(&_tenant, &req)?;
        let portal = load_portal(&state, &path.into_inner())?;
        // Menus carry no company of their own: they are scoped
        // through their parent portal.
        require_record_in_company_scope_str(&portal.company_id, &scope)?;
        let input = body.into_inner();
        let current = menus_for(&state, portal.id);
        if input.menu_ids.len() != current.len() {
            return Err(UnifiedError::bad_request(
                "menu_ids must list every menu entry of the portal exactly once",
            ));
        }
        let mut seen: Vec<Uuid> = Vec::new();
        for id in &input.menu_ids {
            if seen.contains(id) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate menu id in reorder list: {id}"
                )));
            }
            seen.push(*id);
            if !current.iter().any(|menu| menu.id == *id) {
                return Err(UnifiedError::bad_request(format!(
                    "Menu id does not belong to this portal: {id}"
                )));
            }
        }
        let now = Utc::now();
        let mut updated: Vec<PortalMenu> = Vec::new();
        for (position, id) in input.menu_ids.iter().enumerate() {
            if let Some(mut menu) = state.menus.get_mut(id) {
                menu.position = position as i64;
                menu.updated_at = now;
                updated.push(menu.value().clone());
            }
        }
        updated.sort_by_key(|menu| menu.position);
        state.emit(
            "portals.menu.updated",
            json!({
                "portal_id": portal.id.to_string(),
                "action": "reordered",
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(menus) => ok(menus),
        Err(error) => err(error, debug),
    }
}
