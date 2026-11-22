// Users module.
//
// Full user CRUD under `/api/v1/users`, with pagination, filtering and
// search. Passwords are hashed with `crate::auth::password::hash` and the
// password policy from the security module is enforced on create and on
// password change.
//
// Persistence: when a database pool is attached to the `AppContext`, users
// are stored in the Diesel `users` table (and role assignments in the
// `user_roles` table). When no pool is present, users live in an in-memory
// store instead — the fallback is documented here and in the module
// description so a deployment without a database still works, while a
// deployment with a database never silently loses data.
//
// Interconnection: `user.created` and `user.updated` hooks are emitted on
// create/update (and `user.deleted` on delete); assigning a role emits a
// `role.check` hook so the roles module can validate the role.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::db::models::{NewUser, User};
use crate::db::DbPool;
use crate::modules::security::PasswordPolicy;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, LazyLock, OnceLock};
use uuid::Uuid;

static EMAIL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}$")
        .expect("email validation pattern is valid")
});

/// In-memory user record. Mirrors the Diesel `User` model plus the role
/// assignments, which live in `user_roles` when a database is present.
#[derive(Debug, Clone, Serialize)]
pub struct StoredUser {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub is_active: bool,
    pub company_id: Option<Uuid>,
    pub roles: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public user representation: the password hash is never serialized.
#[derive(Debug, Clone, Serialize)]
pub struct UserResponse {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub is_active: bool,
    pub company_id: Option<Uuid>,
    pub roles: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&StoredUser> for UserResponse {
    fn from(record: &StoredUser) -> Self {
        Self {
            id: record.id,
            email: record.email.clone(),
            name: record.name.clone(),
            is_active: record.is_active,
            company_id: record.company_id,
            roles: record.roles.clone(),
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

impl From<(&User, Vec<Uuid>)> for UserResponse {
    fn from((user, roles): (&User, Vec<Uuid>)) -> Self {
        Self {
            id: user.id,
            email: user.email.clone(),
            name: user.name.clone(),
            is_active: user.is_active,
            company_id: user.company_id,
            roles,
            created_at: user.created_at,
            updated_at: user.updated_at,
        }
    }
}

pub type UsersStore = Arc<DashMap<Uuid, StoredUser>>;

pub struct UsersModule {
    store: UsersStore,
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

#[derive(Debug, Clone, Deserialize)]
pub struct CreateUserInput {
    pub email: String,
    pub name: String,
    pub password: String,
    #[serde(default)]
    pub company_id: Option<Uuid>,
    #[serde(default = "default_active")]
    pub is_active: bool,
}

fn default_active() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateUserInput {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub is_active: Option<bool>,
    /// `Some(Some(id))` sets the company, `Some(None)` clears it,
    /// `None` (field absent) leaves it unchanged.
    #[serde(default)]
    pub company_id: Option<Option<Uuid>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssignRoleInput {
    pub role_id: Uuid,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserRolesResponse {
    pub user_id: Uuid,
    pub roles: Vec<Uuid>,
}

fn validate_email(email: &str) -> UnifiedResult<()> {
    let email = email.trim();
    if email.is_empty() || !EMAIL_PATTERN.is_match(email) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address: \"{email}\""
        )));
    }
    Ok(())
}

fn validate_name(name: &str) -> UnifiedResult<()> {
    if name.trim().is_empty() {
        return Err(UnifiedError::bad_request("Name must not be empty"));
    }
    if name.chars().count() > 255 {
        return Err(UnifiedError::bad_request(
            "Name must be at most 255 characters long",
        ));
    }
    Ok(())
}

fn validate_password(password: &str) -> UnifiedResult<()> {
    let problems = PasswordPolicy::default().validate(password);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(UnifiedError::bad_request("Password does not meet the policy")
            .with_metadata("problems", serde_json::json!(problems)))
    }
}

fn hash_password(password: &str) -> UnifiedResult<String> {
    crate::auth::password::hash(password)
        .map_err(|error| UnifiedError::internal(format!("Password hashing failed: {error}")))
}

fn db_pool(ctx: &AppContext) -> Option<DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

// ---------------------------------------------------------------------------
// Hook support: `init` stashes the shared application context so the
// synchronous `user.create_requested` handler below can use the
// database and emit events. The handler reads it lazily, so
// registration order (hooks before `init`) does not matter. The
// stashed context shares the live pool/config locks; only the uptime
// and pause flags are snapshots.
// ---------------------------------------------------------------------------

static USERS_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        start_time: std::time::Instant::now(),
        paused: std::sync::atomic::AtomicBool::new(ctx.is_paused()),
        maintenance: std::sync::atomic::AtomicBool::new(ctx.is_maintenance()),
    })
}

fn hook_ctx() -> Option<Arc<AppContext>> {
    USERS_CTX.get().cloned()
}

/// Generate a random password that satisfies the default password
/// policy (at least 12 characters, with an uppercase letter, a
/// lowercase letter, a digit, and a symbol). The generated password is
/// never returned to the caller: it is only hashed and stored.
fn random_password() -> String {
    let mut rng = rand::rng();
    const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
    const DIGITS: &[u8] = b"23456789";
    const SYMBOLS: &[u8] = b"!@#$%^&*";
    const ALL: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!@#$%^&*";
    let pick = |rng: &mut rand::rngs::ThreadRng, set: &[u8]| -> char {
        set[rng.random_range(0..set.len())] as char
    };
    let mut chars: Vec<char> = vec![
        pick(&mut rng, UPPER),
        pick(&mut rng, LOWER),
        pick(&mut rng, DIGITS),
        pick(&mut rng, SYMBOLS),
    ];
    for _ in 0..12 {
        chars.push(pick(&mut rng, ALL));
    }
    for i in (1..chars.len()).rev() {
        let j = rng.random_range(0..=i);
        chars.swap(i, j);
    }
    chars.into_iter().collect()
}

/// Create a user from hook data, on the database when a pool is
/// attached and in the in-memory store otherwise. The password is
/// hashed with `crate::auth::password::hash`; the caller supplies the
/// already-resolved password (random when none was given).
fn create_user_from_hook(
    store: &UsersStore,
    app_ctx: Option<&AppContext>,
    name: &str,
    email: &str,
    password: &str,
    company_id: Option<Uuid>,
) -> UnifiedResult<UserResponse> {
    validate_email(email)?;
    validate_name(name)?;
    validate_password(password)?;
    let email = email.trim().to_lowercase();
    let password_hash = hash_password(password)?;
    let now = Utc::now();
    match app_ctx.and_then(db_pool) {
        Some(pool) => {
            let new_user = NewUser {
                id: Uuid::new_v7(),
                email: email.clone(),
                name: name.trim().to_string(),
                password_hash,
                company_id,
            };
            let mut connection = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            {
                use crate::db::schema::users::dsl as users_dsl;
                let exists: i64 = users_dsl::users
                    .filter(users_dsl::email.eq(&new_user.email))
                    .count()
                    .get_result(&mut connection)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot check duplicate email: {error}"))
                    })?;
                if exists > 0 {
                    return Err(UnifiedError::conflict(format!(
                        "A user with email \"{email}\" already exists"
                    )));
                }
                let user = diesel::insert_into(users_dsl::users)
                    .values(&new_user)
                    .get_result::<User>(&mut connection)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot create user: {error}"))
                            .with_trace(format!("{error:?}"))
                    })?;
                Ok(UserResponse::from((&user, Vec::new())))
            }
        }
        None => {
            if store
                .iter()
                .any(|entry| entry.value().email.eq_ignore_ascii_case(&email))
            {
                return Err(UnifiedError::conflict(format!(
                    "A user with email \"{email}\" already exists"
                )));
            }
            let record = StoredUser {
                id: Uuid::new_v7(),
                email,
                name: name.trim().to_string(),
                password_hash,
                is_active: true,
                company_id,
                roles: Vec::new(),
                created_at: now,
                updated_at: now,
            };
            let response = UserResponse::from(&record);
            store.insert(record.id, record);
            Ok(response)
        }
    }
}

/// Create the user requested by a CRM lead conversion
/// (`user.create_requested`, payload `{ "name", "email",
/// "password"?, "company_id"? }`). A missing password is replaced by
/// a random one, which is hashed and never included in the result.
fn on_user_create_requested(store: UsersStore, ctx: &mut HookContext) {
    let app_ctx = hook_ctx();
    let name = ctx
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let email = ctx
        .get("email")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if name.trim().is_empty() || email.trim().is_empty() {
        ctx.add_result(serde_json::json!({
            "error": "user.create_requested requires 'name' and 'email'",
        }));
        return;
    }
    // The CRM sends the company as a name, not an id; only a UUID
    // `company_id` can be attached to the new user.
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<Uuid>().ok());
    let password = ctx
        .get("password")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(random_password);
    match create_user_from_hook(
        &store,
        app_ctx.as_deref(),
        &name,
        &email,
        &password,
        company_id,
    ) {
        Ok(response) => {
            if let Some(app_ctx) = &app_ctx {
                app_ctx.hooks.emit(
                    events::USER_CREATED,
                    serde_json::json!({ "id": response.id, "email": response.email, "name": response.name }),
                );
            }
            ctx.add_result(serde_json::json!({ "user_id": response.id }));
        }
        Err(error) => {
            ctx.add_result(serde_json::json!({ "error": error.message }));
        }
    }
}

/// Load the role ids assigned to a user from the `user_roles` table.
fn db_role_ids(connection: &mut PgConnection, user_id: Uuid) -> UnifiedResult<Vec<Uuid>> {
    use crate::db::schema::user_roles::dsl as user_roles_dsl;
    user_roles_dsl::user_roles
        .filter(user_roles_dsl::user_id.eq(user_id))
        .select(user_roles_dsl::role_id)
        .load::<Uuid>(connection)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load user roles: {error}"))
                .with_trace(format!("{error:?}"))
        })
}

async fn list_users(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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
                use crate::db::schema::users::dsl as users_dsl;
                let mut connection = pool.get().map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot get database connection: {error}"
                    ))
                })?;

                // One filtered query for the count, one for the page.
                let mut count_query = users_dsl::users.into_boxed::<diesel::pg::Pg>();
                let mut page_query = users_dsl::users.into_boxed::<diesel::pg::Pg>();
                if let Some(search) = &search {
                    let pattern = format!("%{search}%");
                    count_query = count_query.filter(
                        users_dsl::name
                            .like(pattern.clone())
                            .or(users_dsl::email.like(pattern.clone())),
                    );
                    page_query = page_query.filter(
                        users_dsl::name
                            .like(pattern.clone())
                            .or(users_dsl::email.like(pattern)),
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
                    .offset(((page - 1) * per_page) as i64)
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
                        pagination: crate::core::pagination::PageInfo {
                            page: pagination.page,
                            per_page: pagination.per_page,
                            total: total as u64,
                            total_pages: if total <= 0 {
                                0
                            } else {
                                total.div_ceil(pagination.per_page as i64) as u64
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
fn create_user_memory(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    body: web::Json<CreateUserInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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
            id: Uuid::new_v7(),
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

async fn create_user_db(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateUserInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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
        id: Uuid::new_v7(),
        email: email.clone(),
        name: input.name.trim().to_string(),
        password_hash,
        company_id: input.company_id,
    };
    let is_active = input.is_active;
    let created = web::block(move || {
        use crate::db::schema::users::dsl as users_dsl;
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
                UnifiedError::internal(format!("Cannot create user: {error}"))
                    .with_trace(format!("{error:?}"))
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

async fn get_user(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let user_id = id.into_inner();
    match db_pool(&ctx) {
        Some(pool) => {
            let found = web::block(move || {
                use crate::db::schema::users::dsl as users_dsl;
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

async fn update_user(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateUserInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let user_id = id.into_inner();
    let input = body.into_inner();
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
                use crate::db::schema::users::dsl as users_dsl;
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
                #[diesel(table_name = crate::db::schema::users)]
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

async fn delete_user(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let user_id = id.into_inner();
    match db_pool(&ctx) {
        Some(pool) => {
            let deleted = web::block(move || {
                use crate::db::schema::user_roles::dsl as user_roles_dsl;
                use crate::db::schema::users::dsl as users_dsl;
                let mut connection = pool.get().map_err(|error| {
                    UnifiedError::internal(format!(
                        "Cannot get database connection: {error}"
                    ))
                })?;
                diesel::delete(
                    user_roles_dsl::user_roles
                        .filter(user_roles_dsl::user_id.eq(user_id)),
                )
                .execute(&mut connection)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete user roles: {error}"))
                        .with_trace(format!("{error:?}"))
                })?;
                let rows = diesel::delete(users_dsl::users.find(user_id))
                    .execute(&mut connection)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot delete user: {error}"))
                            .with_trace(format!("{error:?}"))
                    })?;
                Ok::<usize, UnifiedError>(rows)
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

async fn list_user_roles(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let user_id = id.into_inner();
    match db_pool(&ctx) {
        Some(pool) => {
            let found = web::block(move || {
                use crate::db::schema::users::dsl as users_dsl;
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

async fn assign_user_role(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<UsersStore>,
    id: web::Path<Uuid>,
    body: web::Json<AssignRoleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let user_id = id.into_inner();
    let role_id = body.role_id;

    // Let the roles module (or any other hook handler) validate that the
    // role exists. When a handler marks the check as not allowed, the role
    // is treated as unknown.
    let check = ctx.hooks.emit(
        "role.check",
        serde_json::json!({ "role_id": role_id, "permission": "*", "allowed": true }),
    );
    if check.get("allowed").and_then(|value| value.as_bool()) == Some(false) {
        return error_response(
            &UnifiedError::not_found(format!("Role {role_id} not found")),
            debug,
        );
    }

    match db_pool(&ctx) {
        Some(pool) => {
            let assigned = web::block(move || {
                use crate::db::schema::user_roles::dsl as user_roles_dsl;
                use crate::db::schema::users::dsl as users_dsl;
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
                #[diesel(table_name = crate::db::schema::user_roles)]
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
                         body: web::Json<CreateUserInput>| async move {
                            if db_pool(&ctx).is_some() {
                                create_user_db(ctx, body).await
                            } else {
                                create_user_memory(ctx, store, body)
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
