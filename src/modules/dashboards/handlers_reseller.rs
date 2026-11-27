// Dashboards - Reseller pages, view-as and settings handlers.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json::{error_response, success_response};
use unified_core::pagination::PaginationParams;
use unified_core::db::models::User;
use crate::modules::roles::permission_allowed;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use diesel::prelude::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::{CAP_ADMIN_ACCESS, CAP_RESELLER_ACCESS, CAP_RESELLER_CLIENTS_MANAGE,
    CAP_RESELLER_CLIENTS_VIEW, CAP_SETTINGS_MANAGE, CAP_VIEW_AS_USER, CallerIdentity,
    DashboardsState, EVENT_RESELLER_CLIENT_LINKED, EVENT_RESELLER_CLIENT_UNLINKED,
    EVENT_VIEW_AS_ENDED, EVENT_VIEW_AS_EXPIRED, EVENT_VIEW_AS_STARTED, LinkClientInput,
    MAX_VIEW_AS_TTL_MINUTES, MIN_VIEW_AS_TTL_MINUTES, ResellerLink, StartViewAsInput,
    UpdateDashboardSettingsInput, UserResponseView, ViewAsSession, dashboard_settings_json,
    linked_client_ids, paginate_vec, require_capability, require_capability_write,
    require_real_capability, require_real_capability_write, reseller_overview_data, run_blocking};

// Reseller handlers

pub(crate) async fn get_reseller_overview(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_RESELLER_ACCESS).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match reseller_overview_data(&ctx, &state, &caller).await {
        Ok(data) => success_response(data, 200),
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn get_reseller_clients(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_RESELLER_CLIENTS_VIEW).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let params = query.into_inner();
    let links: Vec<ResellerLink> = linked_client_ids(&state, caller.identity.user_id)
        .into_iter()
        .filter_map(|client_id| {
            state
                .reseller_links
                .get(&(caller.identity.user_id, client_id))
                .map(|entry| entry.value().clone())
        })
        .collect();
    // Attach the client profile from the database when available; a
    // link without a database row still lists, with `profile: null`.
    let pool = ctx.db.read().expect("db lock poisoned").clone();
    let profiles: HashMap<Uuid, UserResponseView> = match &pool {
        Some(pool) if !links.is_empty() => {
            let ids: Vec<Uuid> = links.iter().map(|link| link.client_id).collect();
            let pool = pool.clone();
            run_blocking(pool, move |connection| {
                use unified_core::db::schema::users::dsl as users_dsl;
                let users = users_dsl::users
                    .filter(users_dsl::id.eq_any(&ids))
                    .select(User::as_select())
                    .load::<User>(connection)
                    .map_err(UnifiedError::from)?;
                let mut map = HashMap::new();
                for user in users {
                    map.insert(user.id, UserResponseView::from(&user));
                }
                Ok(map)
            })
            .await
            .unwrap_or_default()
        }
        _ => HashMap::new(),
    };
    let items: Vec<Value> = links
        .iter()
        .map(|link| {
            json!({
                "reseller_id": link.reseller_id.to_string(),
                "client_id": link.client_id.to_string(),
                "verified": link.verified,
                "created_at": link.created_at.to_rfc3339(),
                "profile": profiles.get(&link.client_id),
            })
        })
        .collect();
    success_response(paginate_vec(items, &params).to_json(), 200)
}

pub(crate) async fn link_reseller_client(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    body: web::Json<LinkClientInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller =
        match require_capability_write(&req, &ctx, &state, CAP_RESELLER_CLIENTS_MANAGE).await {
            Ok(caller) => caller,
            Err(response) => return response,
        };
    let client_id = match Uuid::parse_str(body.client_id.trim()) {
        Ok(id) => id,
        Err(_) => {
            return error_response(
                &UnifiedError::bad_request("client_id must be a valid UUID"),
                debug,
            )
        }
    };
    if client_id == caller.identity.user_id {
        return error_response(
            &UnifiedError::bad_request("A reseller cannot link themselves as a client"),
            debug,
        );
    }
    if state
        .reseller_links
        .contains_key(&(caller.identity.user_id, client_id))
    {
        return error_response(
            &UnifiedError::conflict(format!(
                "Client \"{client_id}\" is already linked to this reseller"
            )),
            debug,
        );
    }
    // Verify the client exists when a database is available; without
    // one the link is recorded as unverified instead of refused.
    let verified = match ctx.db.read().expect("db lock poisoned").clone() {
        Some(pool) => {
            let found = run_blocking(pool, move |connection| {
                use unified_core::db::schema::users::dsl as users_dsl;
                let found = users_dsl::users
                    .find(client_id)
                    .select(User::as_select())
                    .first::<User>(connection)
                    .optional()
                    .map_err(UnifiedError::from)?;
                Ok(found.is_some())
            })
            .await;
            match found {
                Ok(true) => true,
                Ok(false) => {
                    return error_response(
                        &UnifiedError::not_found(format!(
                            "Client user \"{client_id}\" does not exist"
                        )),
                        debug,
                    )
                }
                Err(error) => return error_response(&error, debug),
            }
        }
        None => false,
    };
    let link = ResellerLink {
        reseller_id: caller.identity.user_id,
        client_id,
        verified,
        created_at: Utc::now(),
    };
    state
        .reseller_links
        .insert((caller.identity.user_id, client_id), link.clone());
    ctx.hooks.emit(
        EVENT_RESELLER_CLIENT_LINKED,
        json!({
            "actor_id": caller.identity.user_id.to_string(),
            "reseller_id": caller.identity.user_id.to_string(),
            "client_id": client_id.to_string(),
            "verified": verified,
            "resource": format!("reseller:{}:client:{client_id}", caller.identity.user_id),
        }),
    );
    success_response(link, 201)
}

pub(crate) async fn unlink_reseller_client(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller =
        match require_capability_write(&req, &ctx, &state, CAP_RESELLER_CLIENTS_MANAGE).await {
            Ok(caller) => caller,
            Err(response) => return response,
        };
    let client_id = match Uuid::parse_str(path.into_inner().trim()) {
        Ok(id) => id,
        Err(_) => {
            return error_response(
                &UnifiedError::bad_request("client_id must be a valid UUID"),
                debug,
            )
        }
    };
    match state
        .reseller_links
        .remove(&(caller.identity.user_id, client_id))
    {
        Some((_, link)) => {
            ctx.hooks.emit(
                EVENT_RESELLER_CLIENT_UNLINKED,
                json!({
                    "actor_id": caller.identity.user_id.to_string(),
                    "reseller_id": caller.identity.user_id.to_string(),
                    "client_id": client_id.to_string(),
                    "resource": format!("reseller:{}:client:{client_id}", caller.identity.user_id),
                }),
            );
            let _ = link;
            success_response(json!({ "unlinked": true, "client_id": client_id.to_string() }), 200)
        }
        None => error_response(
            &UnifiedError::not_found(format!(
                "Client \"{client_id}\" is not linked to this reseller"
            )),
            debug,
        ),
    }
}

/// The key a view-as session is stored under in
/// `DashboardsState::view_as`: always the REAL admin id, never the
/// effective (possibly impersonated) id. `authenticate` rewrites
/// `identity.user_id` to the view-as target while impersonating, so
/// keying by it would store a nested session under the target's id
/// and record the target as the admin. Keying by the real id also
/// means starting a second session while one is active hits the
/// already-active conflict below instead of corrupting bookkeeping.
pub(crate) fn view_as_key(identity: &CallerIdentity) -> Uuid {
    identity.real_user_id
}

/// True when any of the target's roles is an admin role: either the
/// role slug is `admin` or its permissions grant `admin:access`.
/// The capability check also covers the wildcard `"*": ["*"]`
/// admin grant, since `permission_allowed` returns true for any
/// permission on such a map. Only non-admin users may be view-as
/// targets: impersonating a fellow admin would hand over that
/// admin's capabilities.
pub(crate) fn target_holds_admin_role(roles: &[(String, Value)]) -> bool {
    roles.iter().any(|(slug, permissions)| {
        slug == "admin" || permission_allowed(permissions, CAP_ADMIN_ACCESS)
    })
}

pub(crate) async fn start_view_as(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    body: web::Json<StartViewAsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // Checked against the REAL admin identity (see
    // `require_real_capability`): while impersonating, the effective
    // identity is the target user, who must never be able to start a
    // view-as session of their own - only the admin can, and the
    // session is keyed by the admin's real id (see `view_as_key`).
    let caller = match require_real_capability_write(&req, &ctx, &state, CAP_VIEW_AS_USER).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let admin_id = view_as_key(&caller.identity);
    let input = body.into_inner();
    let reason = input.reason.trim().to_string();
    if reason.is_empty() || reason.chars().count() > 500 {
        return error_response(
            &UnifiedError::bad_request("reason is required and must be at most 500 characters"),
            debug,
        );
    }
    let target_id = match Uuid::parse_str(input.target_user_id.trim()) {
        Ok(id) => id,
        Err(_) => {
            return error_response(
                &UnifiedError::bad_request("target_user_id must be a valid UUID"),
                debug,
            )
        }
    };
    if target_id == admin_id {
        return error_response(
            &UnifiedError::bad_request("An admin cannot view as themselves"),
            debug,
        );
    }
    if state.view_as.contains_key(&admin_id) {
        return error_response(
            &UnifiedError::conflict(
                "A view-as session is already active for this admin; stop it first",
            ),
            debug,
        );
    }
    // The target must exist, be active, and hold no admin role when
    // a database is available; without one the session is still
    // created, and the target resolves (or fails closed) on the next
    // request. The admin-role check needs the database too: with no
    // pool there is no role store to consult.
    if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
        let found = run_blocking(pool, move |connection| {
            use unified_core::db::schema::user_roles::dsl as user_roles_dsl;
            use unified_core::db::schema::users::dsl as users_dsl;
            let found = users_dsl::users
                .find(target_id)
                .select(User::as_select())
                .first::<User>(connection)
                .optional()
                .map_err(UnifiedError::from)?;
            let Some(user) = found else {
                return Ok((None, false));
            };
            let role_ids: Vec<Uuid> = user_roles_dsl::user_roles
                .filter(user_roles_dsl::user_id.eq(target_id))
                .select(user_roles_dsl::role_id)
                .load::<Uuid>(connection)
                .map_err(UnifiedError::from)?;
            let mut roles: Vec<(String, Value)> = Vec::new();
            if !role_ids.is_empty() {
                #[derive(Debug, QueryableByName)]
                struct TargetRoleRow {
                    #[diesel(sql_type = diesel::sql_types::Text)]
                    slug: String,
                    #[diesel(sql_type = diesel::sql_types::Text)]
                    permissions: String,
                }
                let rows = diesel::sql_query(
                    "SELECT slug, permissions::text AS permissions FROM roles WHERE id = ANY($1)",
                )
                .bind::<diesel::sql_types::Array<diesel::sql_types::Uuid>, _>(role_ids)
                .load::<TargetRoleRow>(connection)
                .map_err(UnifiedError::from)?;
                for row in rows {
                    let permissions = serde_json::from_str::<Value>(&row.permissions)
                        .unwrap_or(Value::Null);
                    roles.push((row.slug, permissions));
                }
            }
            Ok((Some(user), target_holds_admin_role(&roles)))
        })
        .await;
        match found {
            Ok((Some(user), false)) if user.is_active => {}
            Ok((Some(_), true)) => {
                return error_response(
                    &UnifiedError::forbidden(format!(
                        "Target user \"{target_id}\" holds an admin role and cannot be viewed as"
                    )),
                    debug,
                )
            }
            Ok((Some(_), _)) => {
                return error_response(
                    &UnifiedError::forbidden(format!(
                        "Target user \"{target_id}\" is disabled"
                    )),
                    debug,
                )
            }
            Ok((None, _)) => {
                return error_response(
                    &UnifiedError::not_found(format!(
                        "Target user \"{target_id}\" does not exist"
                    )),
                    debug,
                )
            }
            Err(error) => return error_response(&error, debug),
        }
    }
    let now = Utc::now();
    let session = ViewAsSession {
        admin_id,
        target_user_id: target_id,
        reason: reason.clone(),
        started_at: now,
        expires_at: now + chrono::Duration::minutes(state.view_as_ttl_minutes()),
    };
    state.view_as.insert(admin_id, session.clone());
    ctx.hooks.emit(
        EVENT_VIEW_AS_STARTED,
        json!({
            "actor_id": admin_id.to_string(),
            "admin_id": admin_id.to_string(),
            "target_user_id": target_id.to_string(),
            "reason": reason,
            "resource": format!("user:{target_id}"),
            "expires_at": session.expires_at.to_rfc3339(),
        }),
    );
    success_response(session, 201)
}

pub(crate) async fn get_view_as(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // Checked against the REAL admin identity, like `stop_view_as`:
    // while impersonating, the effective identity is the target user,
    // and the session is keyed by the admin's real id (see
    // `view_as_key`), so looking it up by the effective id would
    // never find it.
    let caller = match require_real_capability(&req, &ctx, &state, CAP_VIEW_AS_USER).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let admin_id = view_as_key(&caller.identity);
    match state.view_as.get(&admin_id) {
        Some(entry) => {
            let session = entry.value().clone();
            if session.expires_at <= Utc::now() {
                drop(entry);
                state.view_as.remove(&admin_id);
                ctx.hooks.emit(
                    EVENT_VIEW_AS_EXPIRED,
                    json!({
                        "actor_id": admin_id.to_string(),
                        "target_user_id": session.target_user_id.to_string(),
                        "reason": session.reason,
                        "resource": format!("user:{}", session.target_user_id),
                        "expired_at": session.expires_at.to_rfc3339(),
                    }),
                );
                error_response(
                    &UnifiedError::not_found("No active view-as session"),
                    debug,
                )
            } else {
                success_response(session, 200)
            }
        }
        None => error_response(
            &UnifiedError::not_found("No active view-as session"),
            debug,
        ),
    }
}

pub(crate) async fn stop_view_as(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // Checked against the REAL admin identity (see
    // `require_real_capability`): the impersonated target must never
    // be able to end the admin's session.
    let caller = match require_real_capability_write(&req, &ctx, &state, CAP_VIEW_AS_USER).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match state.view_as.remove(&caller.identity.real_user_id) {
        Some((_, session)) => {
            ctx.hooks.emit(
                EVENT_VIEW_AS_ENDED,
                json!({
                    "actor_id": caller.identity.real_user_id.to_string(),
                    "admin_id": caller.identity.real_user_id.to_string(),
                    "target_user_id": session.target_user_id.to_string(),
                    "reason": session.reason,
                    "resource": format!("user:{}", session.target_user_id),
                }),
            );
            success_response(
                json!({
                    "stopped": true,
                    "admin_id": caller.identity.real_user_id.to_string(),
                    "target_user_id": session.target_user_id.to_string(),
                }),
                200,
            )
        }
        None => error_response(
            &UnifiedError::not_found("No active view-as session"),
            debug,
        ),
    }
}

// Admin dashboard settings (runtime settings owned by this module)

pub(crate) async fn get_dashboard_settings(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let caller = match require_capability(&req, &ctx, &state, CAP_SETTINGS_MANAGE).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let _ = caller;
    success_response(dashboard_settings_json(&ctx, &state), 200)
}

pub(crate) async fn update_dashboard_settings(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    body: web::Json<UpdateDashboardSettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability_write(&req, &ctx, &state, CAP_SETTINGS_MANAGE).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let _ = caller;
    if let Some(ttl) = body.view_as_ttl_minutes {
        if !(MIN_VIEW_AS_TTL_MINUTES..=MAX_VIEW_AS_TTL_MINUTES).contains(&ttl) {
            return error_response(
                &UnifiedError::bad_request(format!(
                    "view_as_ttl_minutes must be between {MIN_VIEW_AS_TTL_MINUTES} and {MAX_VIEW_AS_TTL_MINUTES}"
                )),
                debug,
            );
        }
        *state.view_as_ttl_minutes.write().expect("view-as ttl lock poisoned") = ttl;
    }
    success_response(dashboard_settings_json(&ctx, &state), 200)
}


#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;

    fn impersonating_identity(admin_id: Uuid, target_id: Uuid) -> CallerIdentity {
        let mut identity = CallerIdentity::direct(admin_id, Vec::new());
        identity.user_id = target_id;
        identity.impersonating = true;
        identity.impersonated_by = Some(admin_id);
        identity
    }

    fn session(admin_id: Uuid, target_id: Uuid) -> ViewAsSession {
        let now = Utc::now();
        ViewAsSession {
            admin_id,
            target_user_id: target_id,
            reason: "support check".to_string(),
            started_at: now,
            expires_at: now + chrono::Duration::minutes(30),
        }
    }

    fn context() -> Arc<AppContext> {
        Arc::new(AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        ))
    }

    #[test]
    fn view_as_key_returns_real_admin_while_impersonating() {
        let admin_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        let identity = impersonating_identity(admin_id, target_id);
        // The effective id is the target; the session key must still
        // be the real admin, or a nested start would be keyed under
        // the target and record the target as the admin.
        assert_eq!(identity.user_id, target_id);
        assert_eq!(view_as_key(&identity), admin_id);
    }

    #[test]
    fn view_as_key_returns_caller_when_not_impersonating() {
        let admin_id = Uuid::new_v4();
        let identity = CallerIdentity::direct(admin_id, Vec::new());
        assert_eq!(view_as_key(&identity), admin_id);
    }

    #[test]
    fn view_as_session_is_keyed_by_real_admin_not_target() {
        let ctx = context();
        let state = DashboardsState::new();
        let admin_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        state.view_as.insert(admin_id, session(admin_id, target_id));
        // Looked up by the target id (the effective id while
        // impersonating) there is no session; looked up by the real
        // admin id - the id `authenticate` passes in from the JWT
        // subject - the session is found.
        assert!(state.view_as.get(&target_id).is_none());
        assert!(state.view_as.get(&admin_id).is_some());
        assert!(state.take_expired_or_active_view_as(target_id, &ctx).is_none());
        let found = state
            .take_expired_or_active_view_as(admin_id, &ctx)
            .expect("session keyed by the real admin must resolve");
        assert_eq!(found.admin_id, admin_id);
        assert_eq!(found.target_user_id, target_id);
    }

    #[test]
    fn target_holds_admin_role_rejects_admin_slug() {
        // The slug alone rejects, even with weak permissions: the
        // role is an admin role by name.
        let roles = vec![("admin".to_string(), json!({ "posts": ["read"] }))];
        assert!(target_holds_admin_role(&roles));
    }

    #[test]
    fn target_holds_admin_role_rejects_wildcard_permissions() {
        // The seeded admin role carries `{"*": ["*"]}` under a
        // non-admin slug still grants everything, including admin
        // access.
        let roles = vec![("superuser".to_string(), json!({ "*": ["*"] }))];
        assert!(target_holds_admin_role(&roles));
    }

    #[test]
    fn target_holds_admin_role_rejects_admin_access_grant() {
        let roles = vec![("staff".to_string(), json!({ "admin": ["access"] }))];
        assert!(target_holds_admin_role(&roles));
    }

    #[test]
    fn target_holds_admin_role_rejects_any_admin_among_many() {
        let roles = vec![
            ("customer".to_string(), json!({ "orders": ["read"] })),
            ("admin".to_string(), json!({ "posts": ["read"] })),
        ];
        assert!(target_holds_admin_role(&roles));
    }

    #[test]
    fn target_holds_admin_role_allows_plain_customer() {
        let roles = vec![(
            "customer".to_string(),
            json!({ "store": ["read", "purchase"], "orders": ["create", "read"], "licenses": ["read"] }),
        )];
        assert!(!target_holds_admin_role(&roles));
    }

    #[test]
    fn target_holds_admin_role_allows_no_roles() {
        let roles: Vec<(String, Value)> = Vec::new();
        assert!(!target_holds_admin_role(&roles));
    }
}
