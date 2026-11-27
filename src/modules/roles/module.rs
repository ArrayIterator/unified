//! roles - module.

// Roles module - the Role Manager.
//
// Static default roles (admin, editor, author, member, customer,
// reseller, finance) are
// seeded in memory with permission sets; dynamic roles can be created,
// updated and deleted through the API. Permissions are a JSON map of
// resource -> actions, for example `{"posts": ["create", "read"]}`.
//
// The role factory is `POST /api/v1/roles/check`: it answers whether a
// role is allowed a permission (`resource:action`, or `*` for any). The
// check is hookable: the module itself answers `role.check` hook events,
// and any other module may override the result by setting `allowed` on
// the hook payload.

use unified_core::hooks::{HookBus};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::{web};
use chrono::{Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{store::*, types::*, capabilities::*, seeds::*, validation::*, handlers::*};


pub struct RolesModule {
    pub(crate) store: RolesStore,
}


impl RolesModule {
    pub fn new() -> Self {
        let store: RolesStore = Arc::new(DashMap::new());
        for (name, slug, description, permissions) in default_roles() {
            let record = RoleRecord {
                id: Uuid::now_v7(),
                name: name.to_string(),
                slug: slug.to_string(),
                description: Some(description.to_string()),
                is_system: true,
                permissions,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            store.insert(record.id, record);
        }
        Self { store }
    }
}


impl Default for RolesModule {
    fn default() -> Self {
        Self::new()
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(RolesModule::new()));
}


impl Module for RolesModule {
    fn id(&self) -> &'static str {
        "roles"
    }

    fn name(&self) -> &'static str {
        "Roles"
    }

    fn description(&self) -> &'static str {
        "Role manager: static default roles, dynamic role CRUD, and a hookable permission check factory."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let store = self.store.clone();
        // Default implementation of the `role.check` hook: answer from the
        // role store. Other modules may register their own handlers and
        // override `allowed` on the payload.
        bus.register("role.check", "roles", Arc::new(move |context| {
            let permission = context
                .get("permission")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            // A `role_id` payload (from the users module) asks whether the
            // role exists at all; existence alone is enough there, even
            // for a role with an empty permission set.
            if let Some(role_id) = context
                .get("role_id")
                .and_then(|value| value.as_str())
            {
                let exists = find_role(&store, role_id).is_some();
                context.set("allowed", Value::Bool(exists));
                context.add_result(json!({
                    "role": role_id,
                    "permission": permission,
                    "allowed": exists,
                }));
                return;
            }
            let role_key = context
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            match check_role(&store, &role_key, &permission) {
                Some(allowed) => {
                    context.set("allowed", Value::Bool(allowed));
                    context.add_result(json!({
                        "role": role_key,
                        "permission": permission,
                        "allowed": allowed,
                    }));
                }
                None => {
                    context.set("allowed", Value::Bool(false));
                    context.add_result(json!({
                        "role": role_key,
                        "permission": permission,
                        "allowed": false,
                    }));
                }
            }
        }));
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/roles", owner: "roles", description: "List roles", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/roles", owner: "roles", description: "Create role", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/roles/check", owner: "roles", description: "Check permission", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/roles/{id}", owner: "roles", description: "Get role", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/roles/{id}", owner: "roles", description: "Update role", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/roles/{id}", owner: "roles", description: "Delete role", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/roles/{id}/permissions", owner: "roles", description: "Get role permissions", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/roles")
                .app_data(web::Data::new(self.store.clone()))
                .route("", web::get().to(list_roles))
                .route("", web::post().to(create_role))
                .route("/check", web::post().to(check_permission))
                .route("/{id}", web::get().to(get_role))
                .route("/{id}", web::put().to(update_role))
                .route("/{id}", web::delete().to(delete_role))
                .route("/{id}/permissions", web::get().to(get_role_permissions)),
        );
    }
}
