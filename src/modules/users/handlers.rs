// users module - User handlers.
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

use super::types::{StoredUser, UserResponse, UsersStore};
use super::requests::{AssignRoleInput, CreateUserInput, UpdateUserInput, UserRolesResponse};
use super::validation::{db_pool, hash_password, is_unique_violation, validate_email, validate_name, validate_password};
use super::hooks::db_role_ids;

// Caller identity and capabilities.
//
// Every handler below resolves the caller from the `X-User-Id` /
// `X-Role` headers and enforces a capability through the
// `role.check` hook, exactly like the field_service and admissions
// modules. The check is fail-closed: the hook payload presets
// `"allowed": false`, and only an explicit `true` answer grants
// access, so a missing roles handler (or a silent one) denies.

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_USERS_VIEW: &str = "users.view";
pub(crate) const PERM_USERS_MANAGE: &str = "users.manage";

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// True only when the `role.check` answer explicitly allows: the
/// preset is fail-closed (`"allowed": false`), so a missing, false,
/// or non-boolean answer denies. The same shape as
/// `check_capability` in the field_service and admissions modules.
pub(crate) fn role_check_allowed(answer: &HookContext) -> bool {
    answer
        .get("allowed")
        .and_then(|value| value.as_bool())
        == Some(true)
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let answer = ctx.hooks.emit(
        "role.check",
        serde_json::json!({ "role": role, "permission": permission, "allowed": false }),
    );
    role_check_allowed(&answer)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Resolve the caller and require `permission` in one step.
pub(crate) fn require_caller_permission(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let caller = caller(req)?;
    require_permission(ctx, &caller.role, permission)?;
    Ok(caller)
}

/// True when the caller is acting on their own user record: the
/// `X-User-Id` header carries the caller's user id.
pub(crate) fn is_self(caller: &Caller, user_id: Uuid) -> bool {
    caller.actor == user_id.to_string()
}

/// True when the caller may read one user record: staff with
/// `users.view`, or the user themself.
pub(crate) fn can_read_user(ctx: &AppContext, caller: &Caller, user_id: Uuid) -> bool {
    check_capability(ctx, &caller.role, PERM_USERS_VIEW) || is_self(caller, user_id)
}

/// Authorize an update: managers may update any record; a user may
/// update only their own record, and only their profile fields
/// (name, email, password) - never `is_active` or `company_id`.
pub(crate) fn authorize_update(
    ctx: &AppContext,
    caller: &Caller,
    user_id: Uuid,
    input: &UpdateUserInput,
) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, PERM_USERS_MANAGE) {
        return Ok(());
    }
    if !is_self(caller, user_id) {
        return Err(UnifiedError::forbidden(
            "Only a manager may update another user",
        ));
    }
    if input.is_active.is_some() || input.company_id.is_some() {
        return Err(UnifiedError::forbidden(
            "Only a manager may change is_active or company_id",
        ));
    }
    Ok(())
}

/// Escape the LIKE wildcards (`%`, `_`) and the escape character
/// itself (`\`) in a user-supplied search term, so the term can only
/// match literally. PostgreSQL treats backslash as the default
/// escape character in LIKE/ILIKE patterns.
pub(crate) fn escape_like_pattern(search: &str) -> String {
    let mut escaped = String::with_capacity(search.len());
    for c in search.chars() {
        if matches!(c, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Build the `%...%` search pattern for the database path: the
/// term is escaped so wildcards match literally, matching the
/// in-memory path's literal substring search.
pub(crate) fn like_search_pattern(search: &str) -> String {
    format!("%{}%", escape_like_pattern(search))
}

pub(crate) async fn list_users(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    query: web::Query<PaginationParams>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_caller_permission(&ctx, &req, PERM_USERS_VIEW) {
        return error_response(&error, debug);
    }
    let pagination = query.normalized();
    let sort = query.allowed_sort(&["name", "email", "created_at"], "created_at");
    let descending = pagination.order == "desc";
    let search = pagination.search.clone().map(|s| s.to_lowercase());
    let is_active_filter = pagination
        .filters
        .get("is_active")
        .and_then(|value| value.parse::<bool>().ok());
    let company_filter = pagination
        .filters
        .get("company_id")
        .and_then(|value| value.parse::<Uuid>().ok());

    match db_pool(&ctx) {
        Some(pool) => {
            let page = pagination.page;
            let per_page = pagination.per_page;
            let result = web::block(move || {
                use unified_core::db::schema::users::dsl as users_dsl;
                let mut connection = pool.get().map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot get database connection: {error}"
                    ))
                })?;

                // One filtered query for the count, one for the page.
                let mut count_query = users_dsl::users.into_boxed::<diesel::pg::Pg>();
                let mut page_query = users_dsl::users.into_boxed::<diesel::pg::Pg>();
                if let Some(search) = &search {
                    // Literal, case-insensitive match: wildcards in the
                    // term are escaped and ILIKE folds case, so the
                    // database path behaves like the in-memory path's
                    // lowercased substring search.
                    let pattern = like_search_pattern(search);
                    count_query = count_query.filter(
                        users_dsl::name
                            .ilike(pattern.clone())
                            .or(users_dsl::email.ilike(pattern.clone())),
                    );
                    page_query = page_query.filter(
                        users_dsl::name
                            .ilike(pattern.clone())
                            .or(users_dsl::email.ilike(pattern)),
                    );
                }
                if let Some(is_active) = is_active_filter {
                    count_query = count_query.filter(users_dsl::is_active.eq(is_active));
                    page_query = page_query.filter(users_dsl::is_active.eq(is_active));
                }
                if let Some(company_id) = company_filter {
                    count_query = count_query.filter(users_dsl::company_id.eq(company_id));
                    page_query = page_query.filter(users_dsl::company_id.eq(company_id));
                }
                let total: i64 = count_query.count().get_result(&mut connection).map_err(
                    |error| {
                        UnifiedError::internal(format!("Cannot count users: {error}"))
                            .with_trace(format!("{error:?}"))
                    },
                )?;

                match (sort.as_str(), descending) {
                    ("name", false) => page_query = page_query.order(users_dsl::name.asc()),
                    ("name", true) => page_query = page_query.order(users_dsl::name.desc()),
                    ("email", false) => page_query = page_query.order(users_dsl::email.asc()),
                    ("email", true) => page_query = page_query.order(users_dsl::email.desc()),
                    (_, true) => page_query = page_query.order(users_dsl::created_at.desc()),
                    (_, false) => page_query = page_query.order(users_dsl::created_at.asc()),
                }
                let rows: Vec<User> = page_query
                    .limit(per_page as i64)
                    // Saturating math: `page` comes from the clamped
                    // `normalized()` pagination, but the offset must
                    // never overflow even if that changes.
                    .offset(page.saturating_sub(1).saturating_mul(per_page) as i64)
                    .load::<User>(&mut connection)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot list users: {error}"))
                            .with_trace(format!("{error:?}"))
                    })?;
                let mut responses = Vec::with_capacity(rows.len());
                for user in &rows {
                    let roles = db_role_ids(&mut connection, user.id)?;
                    responses.push(UserResponse::from((user, roles)));
                }
                Ok::<(Vec<UserResponse>, i64), UnifiedError>((responses, total))
            });
            match result.await {
                Ok(Ok((items, total))) => {
                    let page_result = PageResult {
                        items,
                        pagination: unified_core::pagination::PageInfo {
                            page: pagination.page,
                            per_page: pagination.per_page,
                            total: total as u64,
                            total_pages: if total <= 0 {
                                0
                            } else {
                                ((total + pagination.per_page as i64 - 1) / pagination.per_page as i64) as u64
                            },
                        },
                    };
                    success_response(page_result.to_json(), 200)
                }
                Ok(Err(error)) => error_response(&error, debug),
                Err(error) => error_response(
                    &UnifiedError::internal(format!("User list failed: {error}")),
                    debug,
                ),
            }
        }
        None => {
            let mut records: Vec<StoredUser> = store
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|record| {
                    if let Some(search) = &search {
                        let haystack = format!("{} {}", record.name, record.email).to_lowercase();
                        if !haystack.contains(search.as_str()) {
                            return false;
                        }
                    }
                    if let Some(is_active) = is_active_filter {
                        if record.is_active != is_active {
                            return false;
                        }
                    }
                    if let Some(company_id) = company_filter {
                        if record.company_id != Some(company_id) {
                            return false;
                        }
                    }
                    true
                })
                .collect();
            records.sort_by(|a, b| match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "email" => a.email.cmp(&b.email),
                _ => a.created_at.cmp(&b.created_at),
            });
            if descending {
                records.reverse();
            }
            let total = records.len() as u64;
            let start = ((pagination.page - 1) * pagination.per_page) as usize;
            let end = (start + pagination.per_page as usize).min(records.len());
            let items: Vec<UserResponse> = if start < records.len() {
                records[start..end]
                    .iter()
                    .map(UserResponse::from)
                    .collect()
            } else {
                Vec::new()
            };
            success_response(PageResult::new(items, total, &pagination).to_json(), 200)
        }
    }
}

/// Create a user in the in-memory store. The database path lives in
/// `create_user_db`; the route handler dispatches between the two based on
/// whether a pool is attached.
pub(crate) fn create_user_memory(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    body: web::Json<CreateUserInput>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_caller_permission(&ctx, &req, PERM_USERS_MANAGE) {
        return error_response(&error, debug);
    }
    let input = body.into_inner();
    let result: UnifiedResult<UserResponse> = (|| {
        validate_email(&input.email)?;
        validate_name(&input.name)?;
        validate_password(&input.password)?;
        let email = input.email.trim().to_lowercase();
        let password_hash = hash_password(&input.password)?;
        let now = Utc::now();
        if store
            .iter()
            .any(|entry| entry.value().email.eq_ignore_ascii_case(&email))
        {
            return Err(UnifiedError::conflict(format!(
                "A user with email \"{email}\" already exists"
            )));
        }
        let record = StoredUser {
            id: Uuid::now_v7(),
            email,
            name: input.name.trim().to_string(),
            password_hash,
            is_active: input.is_active,
            company_id: input.company_id,
            roles: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        let response = UserResponse::from(&record);
        store.insert(record.id, record);
        Ok(response)
    })();
    match result {
        Ok(response) => {
            ctx.hooks.emit(
                events::USER_CREATED,
                serde_json::json!({ "id": response.id, "email": response.email, "name": response.name }),
            );
            success_response(response, 201)
        }
        Err(error) => error_response(&error, debug),
    }
}

pub(crate) async fn create_user_db(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateUserInput>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = require_caller_permission(&ctx, &req, PERM_USERS_MANAGE) {
        return error_response(&error, debug);
    }
    let input = body.into_inner();
    let outcome: UnifiedResult<()> = (|| {
        validate_email(&input.email)?;
        validate_name(&input.name)?;
        validate_password(&input.password)?;
        Ok(())
    })();
    if let Err(error) = outcome {
        return error_response(&error, debug);
    }
    let email = input.email.trim().to_lowercase();
    let password_hash = match hash_password(&input.password) {
        Ok(hash) => hash,
        Err(error) => return error_response(&error, debug),
    };
    let pool = match db_pool(&ctx) {
        Some(pool) => pool,
        None => {
            return error_response(
                &UnifiedError::internal("No database pool attached"),
                debug,
            )
        }
    };
    let new_user = NewUser {
        id: Uuid::now_v7(),
        email: email.clone(),
        name: input.name.trim().to_string(),
        password_hash,
        company_id: input.company_id,
    };
    let is_active = input.is_active;
    let created = web::block(move || {
        use unified_core::db::schema::users::dsl as users_dsl;
        let mut connection = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        let exists: i64 = users_dsl::users
            .filter(users_dsl::email.eq(&new_user.email))
            .count()
            .get_result(&mut connection)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot check duplicate email: {error}"))
            })?;
        if exists > 0 {
            return Err(UnifiedError::conflict(format!(
                "A user with email \"{}\" already exists",
                new_user.email
            )));
        }
        let mut user = diesel::insert_into(users_dsl::users)
            .values(&new_user)
            .get_result::<User>(&mut connection)
            .map_err(|error| {
                // The pre-check above can race with a concurrent
                // insert: the unique constraint is the authority, so
                // a unique violation is a 409 conflict, not a 500.
                if is_unique_violation(&error) {
                    UnifiedError::conflict(format!(
                        "A user with email \"{}\" already exists",
                        new_user.email
                    ))
                } else {
                    UnifiedError::internal(format!("Cannot create user: {error}"))
                        .with_trace(format!("{error:?}"))
                }
            })?;
        // `is_active` is not part of `NewUser` (the column defaults to
        // true); apply an explicit false when requested.
        if !is_active {
            user = diesel::update(users_dsl::users.find(user.id))
                .set(users_dsl::is_active.eq(false))
                .get_result::<User>(&mut connection)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot update user: {error}"))
                        .with_trace(format!("{error:?}"))
                })?;
        }
        Ok::<User, UnifiedError>(user)
    })
    .await;
    match created {
        Ok(Ok(user)) => {
            let response = UserResponse::from((&user, Vec::new()));
            ctx.hooks.emit(
                events::USER_CREATED,
                serde_json::json!({ "id": response.id, "email": response.email, "name": response.name }),
            );
            success_response(response, 201)
        }
        Ok(Err(error)) => error_response(&error, debug),
        Err(error) => error_response(
            &UnifiedError::internal(format!("User creation failed: {error}")),
            debug,
        ),
    }
}

pub(crate) async fn get_user(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    id: web::Path<Uuid>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let user_id = id.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    // Hide the record from callers who may not read it (same shape
    // as the admissions module): they get 404, so user ids cannot
    // be probed.
    if !can_read_user(&ctx, &caller, user_id) {
        return error_response(
            &UnifiedError::not_found(format!("User {user_id} not found")),
            debug,
        );
    }
    match db_pool(&ctx) {
        Some(pool) => {
            let found = web::block(move || {
                use unified_core::db::schema::users::dsl as users_dsl;
                let mut connection = pool.get().map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot get database connection: {error}"
                    ))
                })?;
                let user = users_dsl::users
                    .find(user_id)
                    .first::<User>(&mut connection)
                    .optional()
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot load user: {error}"))
                            .with_trace(format!("{error:?}"))
                    })?;
                match user {
                    Some(user) => {
                        let roles = db_role_ids(&mut connection, user.id)?;
                        Ok::<Option<UserResponse>, UnifiedError>(Some(UserResponse::from((
                            &user, roles,
                        ))))
                    }
                    None => Ok(None),
                }
            })
            .await;
            match found {
                Ok(Ok(Some(response))) => success_response(response, 200),
                Ok(Ok(None)) => error_response(
                    &UnifiedError::not_found(format!("User {user_id} not found")),
                    debug,
                ),
                Ok(Err(error)) => error_response(&error, debug),
                Err(error) => error_response(
                    &UnifiedError::internal(format!("User lookup failed: {error}")),
                    debug,
                ),
            }
        }
        None => match store.get(&user_id) {
            Some(record) => success_response(UserResponse::from(record.value()), 200),
            None => error_response(
                &UnifiedError::not_found(format!("User {user_id} not found")),
                debug,
            ),
        },
    }
}

pub(crate) async fn update_user(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateUserInput>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let user_id = id.into_inner();
    let input = body.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = authorize_update(&ctx, &caller, user_id, &input) {
        return error_response(&error, debug);
    }
    let outcome: UnifiedResult<()> = (|| {
        if let Some(email) = &input.email {
            validate_email(email)?;
        }
        if let Some(name) = &input.name {
            validate_name(name)?;
        }
        if let Some(password) = &input.password {
            validate_password(password)?;
        }
        Ok(())
    })();
    if let Err(error) = outcome {
        return error_response(&error, debug);
    }

    match db_pool(&ctx) {
        Some(pool) => {
            let new_hash = match &input.password {
                Some(password) => match hash_password(password) {
                    Ok(hash) => Some(hash),
                    Err(error) => return error_response(&error, debug),
                },
                None => None,
            };
            let email = input.email.as_ref().map(|e| e.trim().to_lowercase());
            let updated = web::block(move || {
                use unified_core::db::schema::users::dsl as users_dsl;
                let mut connection = pool.get().map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot get database connection: {error}"
                    ))
                })?;
                if let Some(email) = &email {
                    let clash: i64 = users_dsl::users
                        .filter(users_dsl::email.eq(email))
                        .filter(users_dsl::id.ne(user_id))
                        .count()
                        .get_result(&mut connection)
                        .map_err(|error| {
                            UnifiedError::internal(format!(
                                "Cannot check duplicate email: {error}"
                            ))
                        })?;
                    if clash > 0 {
                        return Err(UnifiedError::conflict(format!(
                            "A user with email \"{email}\" already exists"
                        )));
                    }
                }
                #[derive(AsChangeset)]
                #[diesel(table_name = unified_core::db::schema::users)]
                struct UserChangeset {
                    email: Option<String>,
                    name: Option<String>,
                    password_hash: Option<String>,
                    is_active: Option<bool>,
                    company_id: Option<Option<Uuid>>,
                    updated_at: DateTime<Utc>,
                }
                let changeset = UserChangeset {
                    email,
                    name: input.name.as_ref().map(|name| name.trim().to_string()),
                    password_hash: new_hash,
                    is_active: input.is_active,
                    company_id: input.company_id,
                    updated_at: Utc::now(),
                };
                let user = diesel::update(users_dsl::users.find(user_id))
                    .set(changeset)
                    .get_result::<User>(&mut connection)
                    .map_err(|error| {
                        if matches!(
                            error,
                            diesel::result::Error::NotFound
                        ) {
                            UnifiedError::not_found(format!("User {user_id} not found"))
                        } else {
                            UnifiedError::internal(format!("Cannot update user: {error}"))
                                .with_trace(format!("{error:?}"))
                        }
                    })?;
                let roles = db_role_ids(&mut connection, user.id)?;
                Ok::<UserResponse, UnifiedError>(UserResponse::from((&user, roles)))
            })
            .await;
            match updated {
                Ok(Ok(response)) => {
                    ctx.hooks.emit(
                        events::USER_UPDATED,
                        serde_json::json!({ "id": response.id, "email": response.email, "name": response.name }),
                    );
                    success_response(response, 200)
                }
                Ok(Err(error)) => error_response(&error, debug),
                Err(error) => error_response(
                    &UnifiedError::internal(format!("User update failed: {error}")),
                    debug,
                ),
            }
        }
        None => {
            let mut record = match store.get_mut(&user_id) {
                Some(record) => record,
                None => {
                    return error_response(
                        &UnifiedError::not_found(format!("User {user_id} not found")),
                        debug,
                    )
                }
            };
            if let Some(email) = &input.email {
                let email = email.trim().to_lowercase();
                if store
                    .iter()
                    .any(|entry| entry.key() != &user_id && entry.value().email == email)
                {
                    return error_response(
                        &UnifiedError::conflict(format!(
                            "A user with email \"{email}\" already exists"
                        )),
                        debug,
                    );
                }
                record.email = email;
            }
            if let Some(name) = &input.name {
                record.name = name.trim().to_string();
            }
            if let Some(password) = &input.password {
                match hash_password(password) {
                    Ok(hash) => record.password_hash = hash,
                    Err(error) => return error_response(&error, debug),
                }
            }
            if let Some(is_active) = input.is_active {
                record.is_active = is_active;
            }
            if let Some(company_id) = input.company_id {
                record.company_id = company_id;
            }
            record.updated_at = Utc::now();
            let response = UserResponse::from(record.value());
            drop(record);
            ctx.hooks.emit(
                events::USER_UPDATED,
                serde_json::json!({ "id": response.id, "email": response.email, "name": response.name }),
            );
            success_response(response, 200)
        }
    }
}

pub(crate) async fn delete_user(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    id: web::Path<Uuid>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let user_id = id.into_inner();
    if let Err(error) = require_caller_permission(&ctx, &req, PERM_USERS_MANAGE) {
        return error_response(&error, debug);
    }
    match db_pool(&ctx) {
        Some(pool) => {
            let deleted = web::block(move || {
                use unified_core::db::schema::user_roles::dsl as user_roles_dsl;
                use unified_core::db::schema::users::dsl as users_dsl;
                let mut connection = pool.get().map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot get database connection: {error}"
                    ))
                })?;
                // One transaction: the role rows and the user row are
                // deleted together, so a failure cannot leave the
                // user alive with roles silently lost (or vice versa).
                connection.transaction(|conn| {
                    diesel::delete(
                        user_roles_dsl::user_roles
                            .filter(user_roles_dsl::user_id.eq(user_id)),
                    )
                    .execute(conn)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot delete user roles: {error}"))
                            .with_trace(format!("{error:?}"))
                    })?;
                    let rows = diesel::delete(users_dsl::users.find(user_id))
                        .execute(conn)
                        .map_err(|error| {
                            UnifiedError::internal(format!("Cannot delete user: {error}"))
                                .with_trace(format!("{error:?}"))
                        })?;
                    Ok::<usize, UnifiedError>(rows)
                })
            })
            .await;
            match deleted {
                Ok(Ok(0)) => error_response(
                    &UnifiedError::not_found(format!("User {user_id} not found")),
                    debug,
                ),
                Ok(Ok(_)) => {
                    ctx.hooks.emit(
                        "user.deleted",
                        serde_json::json!({ "id": user_id }),
                    );
                    success_response(
                        serde_json::json!({ "deleted": true, "id": user_id }),
                        200,
                    )
                }
                Ok(Err(error)) => error_response(&error, debug),
                Err(error) => error_response(
                    &UnifiedError::internal(format!("User delete failed: {error}")),
                    debug,
                ),
            }
        }
        None => match store.remove(&user_id) {
            Some(_) => {
                ctx.hooks.emit(
                    "user.deleted",
                    serde_json::json!({ "id": user_id }),
                );
                success_response(
                    serde_json::json!({ "deleted": true, "id": user_id }),
                    200,
                )
            }
            None => error_response(
                &UnifiedError::not_found(format!("User {user_id} not found")),
                debug,
            ),
        },
    }
}

pub(crate) async fn list_user_roles(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    id: web::Path<Uuid>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let user_id = id.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    if !can_read_user(&ctx, &caller, user_id) {
        return error_response(
            &UnifiedError::not_found(format!("User {user_id} not found")),
            debug,
        );
    }
    match db_pool(&ctx) {
        Some(pool) => {
            let found = web::block(move || {
                use unified_core::db::schema::users::dsl as users_dsl;
                let mut connection = pool.get().map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot get database connection: {error}"
                    ))
                })?;
                let exists: i64 = users_dsl::users
                    .filter(users_dsl::id.eq(user_id))
                    .count()
                    .get_result(&mut connection)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot load user: {error}"))
                    })?;
                if exists == 0 {
                    return Ok::<Option<Vec<Uuid>>, UnifiedError>(None);
                }
                let roles = db_role_ids(&mut connection, user_id)?;
                Ok(Some(roles))
            })
            .await;
            match found {
                Ok(Ok(Some(roles))) => success_response(
                    UserRolesResponse {
                        user_id,
                        roles,
                    },
                    200,
                ),
                Ok(Ok(None)) => error_response(
                    &UnifiedError::not_found(format!("User {user_id} not found")),
                    debug,
                ),
                Ok(Err(error)) => error_response(&error, debug),
                Err(error) => error_response(
                    &UnifiedError::internal(format!("User roles lookup failed: {error}")),
                    debug,
                ),
            }
        }
        None => match store.get(&user_id) {
            Some(record) => success_response(
                UserRolesResponse {
                    user_id,
                    roles: record.roles.clone(),
                },
                200,
            ),
            None => error_response(
                &UnifiedError::not_found(format!("User {user_id} not found")),
                debug,
            ),
        },
    }
}

pub(crate) async fn assign_user_role(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    id: web::Path<Uuid>,
    body: web::Json<AssignRoleInput>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let user_id = id.into_inner();
    let role_id = body.role_id;
    if let Err(error) = require_caller_permission(&ctx, &req, PERM_USERS_MANAGE) {
        return error_response(&error, debug);
    }

    // Let the roles module (or any other hook handler) validate that the
    // role exists. The check is fail-closed: the payload presets
    // `"allowed": false`, and only an explicit `true` answer lets the
    // assignment proceed, so a role no handler vouches for is treated
    // as unknown.
    let check = ctx.hooks.emit(
        "role.check",
        serde_json::json!({ "role_id": role_id, "permission": "*", "allowed": false }),
    );
    if !role_check_allowed(&check) {
        return error_response(
            &UnifiedError::not_found(format!("Role {role_id} not found")),
            debug,
        );
    }

    match db_pool(&ctx) {
        Some(pool) => {
            let assigned = web::block(move || {
                use unified_core::db::schema::user_roles::dsl as user_roles_dsl;
                use unified_core::db::schema::users::dsl as users_dsl;
                let mut connection = pool.get().map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot get database connection: {error}"
                    ))
                })?;
                let exists: i64 = users_dsl::users
                    .filter(users_dsl::id.eq(user_id))
                    .count()
                    .get_result(&mut connection)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot load user: {error}"))
                    })?;
                if exists == 0 {
                    return Err(UnifiedError::not_found(format!(
                        "User {user_id} not found"
                    )));
                }
                #[derive(Insertable)]
                #[diesel(table_name = unified_core::db::schema::user_roles)]
                struct NewUserRole {
                    user_id: Uuid,
                    role_id: Uuid,
                    created_at: DateTime<Utc>,
                }
                let already: i64 = user_roles_dsl::user_roles
                    .filter(user_roles_dsl::user_id.eq(user_id))
                    .filter(user_roles_dsl::role_id.eq(role_id))
                    .count()
                    .get_result(&mut connection)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot check role assignment: {error}"))
                    })?;
                if already == 0 {
                    diesel::insert_into(user_roles_dsl::user_roles)
                        .values(NewUserRole {
                            user_id,
                            role_id,
                            created_at: Utc::now(),
                        })
                        .execute(&mut connection)
                        .map_err(|error| {
                            UnifiedError::internal(format!(
                                "Cannot assign role: {error}"
                            ))
                            .with_trace(format!("{error:?}"))
                        })?;
                }
                let roles = db_role_ids(&mut connection, user_id)?;
                Ok::<Vec<Uuid>, UnifiedError>(roles)
            })
            .await;
            match assigned {
                Ok(Ok(roles)) => {
                    ctx.hooks.emit(
                        events::USER_UPDATED,
                        serde_json::json!({ "id": user_id, "role_assigned": role_id }),
                    );
                    success_response(UserRolesResponse { user_id, roles }, 200)
                }
                Ok(Err(error)) => error_response(&error, debug),
                Err(error) => error_response(
                    &UnifiedError::internal(format!("Role assignment failed: {error}")),
                    debug,
                ),
            }
        }
        None => {
            let mut record = match store.get_mut(&user_id) {
                Some(record) => record,
                None => {
                    return error_response(
                        &UnifiedError::not_found(format!("User {user_id} not found")),
                        debug,
                    )
                }
            };
            if !record.roles.contains(&role_id) {
                record.roles.push(role_id);
            }
            record.updated_at = Utc::now();
            let roles = record.roles.clone();
            drop(record);
            ctx.hooks.emit(
                events::USER_UPDATED,
                serde_json::json!({ "id": user_id, "role_assigned": role_id }),
            );
            success_response(UserRolesResponse { user_id, roles }, 200)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    fn request_with_headers(user_id: &str, role: &str) -> HttpRequest {
        TestRequest::default()
            .insert_header(("X-User-Id", user_id))
            .insert_header(("X-Role", role))
            .to_http_request()
    }

    fn role_check_answer(allowed: serde_json::Value) -> HookContext {
        HookContext::new("role.check", serde_json::json!({ "allowed": allowed }))
    }

    #[test]
    fn caller_reads_actor_and_role_headers() {
        let req = request_with_headers("user-123", "admin");
        let caller = caller(&req).unwrap();
        assert_eq!(caller.actor, "user-123");
        assert_eq!(caller.role, "admin");
    }

    #[test]
    fn caller_rejects_a_missing_user_id_header() {
        let req = TestRequest::default()
            .insert_header(("X-Role", "admin"))
            .to_http_request();
        let error = caller(&req).unwrap_err();
        assert_eq!(error.code, 401);
    }

    #[test]
    fn caller_rejects_a_missing_role_header() {
        let req = TestRequest::default()
            .insert_header(("X-User-Id", "user-123"))
            .to_http_request();
        let error = caller(&req).unwrap_err();
        assert_eq!(error.code, 401);
    }

    #[test]
    fn caller_rejects_blank_headers() {
        let req = request_with_headers("   ", "admin");
        assert!(caller(&req).is_err());
        let req = request_with_headers("user-123", "");
        assert!(caller(&req).is_err());
    }

    #[test]
    fn role_check_allowed_requires_an_explicit_true_answer() {
        // The fail-closed preset ("allowed": false) must deny.
        assert!(!role_check_allowed(&role_check_answer(serde_json::json!(false))));
        // A missing answer denies.
        let missing = HookContext::new("role.check", serde_json::json!({}));
        assert!(!role_check_allowed(&missing));
        // A non-boolean answer denies.
        assert!(!role_check_allowed(&role_check_answer(serde_json::json!("yes"))));
        assert!(!role_check_allowed(&role_check_answer(serde_json::json!(1))));
        // Only an explicit true allows.
        assert!(role_check_allowed(&role_check_answer(serde_json::json!(true))));
    }

    #[test]
    fn is_self_matches_only_the_callers_own_id() {
        let user_id = Uuid::new_v4();
        let me = Caller {
            actor: user_id.to_string(),
            role: "user".to_string(),
        };
        let other = Caller {
            actor: Uuid::new_v4().to_string(),
            role: "user".to_string(),
        };
        assert!(is_self(&me, user_id));
        assert!(!is_self(&other, user_id));
    }

    #[test]
    fn escape_like_pattern_escapes_wildcards_and_backslashes() {
        assert_eq!(escape_like_pattern("plain"), "plain");
        assert_eq!(escape_like_pattern("100%"), "100\\%");
        assert_eq!(escape_like_pattern("a_b"), "a\\_b");
        assert_eq!(escape_like_pattern("a\\b"), "a\\\\b");
        assert_eq!(escape_like_pattern("%_\\"), "\\%\\_\\\\");
        assert_eq!(escape_like_pattern(""), "");
    }

    #[test]
    fn like_search_pattern_wraps_the_escaped_term_in_wildcards() {
        assert_eq!(like_search_pattern("ada"), "%ada%");
        // A wildcard in the term must stay literal, never widen the match.
        assert_eq!(like_search_pattern("%"), "%\\%%");
        assert_eq!(like_search_pattern("a_b"), "%a\\_b%");
    }
}
