// users module — Module definition and registration.
//
// Split from the former single-file module `src/modules/users.rs`.
// The module root (`users.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json::{error_response, success_response};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{NewUser, User};
use unified_core::db::DbPool;
use crate::modules::security::PasswordPolicy;
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, LazyLock, OnceLock};
use uuid::Uuid;

use super::types::UsersStore;
use super::requests::CreateUserInput;
use super::validation::db_pool;
use super::hooks::{USERS_CTX, on_user_company_requested, on_user_create_requested, stash_ctx};
use super::handlers::{assign_user_role, create_user_db, create_user_memory, delete_user, get_user, list_user_roles, list_users, update_user};
use super::ai_capabilities::AI_CAPABILITIES;

pub struct UsersModule {
    pub(crate) store: UsersStore,
}

impl UsersModule {
    pub fn new() -> Self {
        Self {
            store: Arc::new(DashMap::new()),
        }
    }
}

impl Default for UsersModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(UsersModule::new()));
}

// Module implementation (trait).
impl Module for UsersModule {
    fn id(&self) -> &'static str {
        "users"
    }

    fn name(&self) -> &'static str {
        "Users"
    }

    fn description(&self) -> &'static str {
        "User management: CRUD with pagination, search and role assignment. Persists to the database when a pool is attached, otherwise uses an in-memory store."
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("users", AI_CAPABILITIES);
        let _ = USERS_CTX.set(stash_ctx(ctx));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let store = self.store.clone();
        bus.register(
            "user.create_requested",
            "users",
            Arc::new(move |ctx: &mut HookContext| {
                on_user_create_requested(store.clone(), ctx)
            }),
        );
        let store = self.store.clone();
        bus.register(
            "user.company_requested",
            "users",
            Arc::new(move |ctx: &mut HookContext| {
                on_user_company_requested(store.clone(), ctx)
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/users", owner: "users", description: "List users", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/users/{id}", owner: "users", description: "Get user", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/users/{id}", owner: "users", description: "Update user", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/users/{id}", owner: "users", description: "Delete user", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/users/{id}/roles", owner: "users", description: "List user roles", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/users/{id}/roles", owner: "users", description: "Assign user role", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/users", owner: "users", description: "Create user", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        // The POST handler dispatches on pool presence: the database
        // branch needs async blocking calls, the memory branch does not.
        let store = self.store.clone();
        cfg.service(
            web::scope("/api/v1/users")
                .app_data(web::Data::new(store))
                .route("", web::get().to(list_users))
                .route(
                    "",
                    web::post().to(
                        |ctx: web::Data<Arc<AppContext>>,
                         store: web::Data<UsersStore>,
                         body: web::Json<CreateUserInput>,
                         req: HttpRequest| async move {
                            if db_pool(&ctx).is_some() {
                                create_user_db(ctx, body, req).await
                            } else {
                                create_user_memory(ctx, store, body, req)
                            }
                        },
                    ),
                )
                .route("/{id}", web::get().to(get_user))
                .route("/{id}", web::put().to(update_user))
                .route("/{id}", web::delete().to(delete_user))
                .route("/{id}/roles", web::get().to(list_user_roles))
                .route("/{id}/roles", web::post().to(assign_user_role)),
        );
    }
}
