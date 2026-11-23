// users module — Hook support.
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

use super::types::{StoredUser, UserResponse, UsersStore};
use super::validation::{db_pool, hash_password, is_unique_violation, validate_email, validate_name, validate_password};

// ---------------------------------------------------------------------------
// Hook support: `init` stashes the shared application context so the
// synchronous `user.create_requested` handler below can use the
// database and emit events. The handler reads it lazily, so
// registration order (hooks before `init`) does not matter. The
// stashed context shares the live pool/config locks; only the uptime
// and pause flags are snapshots.
// ---------------------------------------------------------------------------

pub(crate) static USERS_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

pub(crate) fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        cache: ctx.cache.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        storage_registry: ctx.storage_registry.clone(),
        routes: ctx.routes.clone(),
        tls: ctx.tls.clone(),
        start_time: std::time::Instant::now(),
        paused: std::sync::atomic::AtomicBool::new(ctx.is_paused()),
        maintenance: std::sync::atomic::AtomicBool::new(ctx.is_maintenance()),
    })
}

pub(crate) fn hook_ctx() -> Option<Arc<AppContext>> {
    USERS_CTX.get().cloned()
}

/// Generate a random password that satisfies the default password
/// policy (at least 12 characters, with an uppercase letter, a
/// lowercase letter, a digit, and a symbol). The generated password is
/// never returned to the caller: it is only hashed and stored.
pub(crate) fn random_password() -> String {
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
pub(crate) fn create_user_from_hook(
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
                id: Uuid::now_v7(),
                email: email.clone(),
                name: name.trim().to_string(),
                password_hash,
                company_id,
            };
            let mut connection = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            {
                use unified_core::db::schema::users::dsl as users_dsl;
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
                        // Same race as the HTTP create path: the
                        // unique constraint is the authority, so a
                        // unique violation is a 409 conflict, not a 500.
                        if is_unique_violation(&error) {
                            UnifiedError::conflict(format!(
                                "A user with email \"{email}\" already exists"
                            ))
                        } else {
                            UnifiedError::internal(format!("Cannot create user: {error}"))
                                .with_trace(format!("{error:?}"))
                        }
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
                id: Uuid::now_v7(),
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
pub(crate) fn on_user_create_requested(store: UsersStore, ctx: &mut HookContext) {
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

/// Look a user's company up server-side: the in-memory store first,
/// then the `users` table when a database pool is attached (the same
/// pool-or-memory rule the create and read paths use). `None` when
/// the user does not exist, has no company, or the database cannot
/// be reached -- every one of those answers "no company", never an
/// invented one.
pub(crate) fn lookup_user_company(store: &UsersStore, user_id: Uuid) -> Option<Uuid> {
    if let Some(record) = store.get(&user_id) {
        return record.company_id;
    }
    let app_ctx = hook_ctx()?;
    let pool = db_pool(app_ctx.as_ref())?;
    let mut connection = pool.get().ok()?;
    use unified_core::db::schema::users::dsl as users_dsl;
    users_dsl::users
        .find(user_id)
        .select(users_dsl::company_id)
        .first::<Option<Uuid>>(&mut connection)
        .ok()
        .flatten()
}

/// Answer `user.company_requested` (`{ "user_id" }`) with the
/// caller's company, resolved server-side only.
///
/// The company id is NEVER taken from the hook payload: a
/// `company_id` carried in the request is client-asserted, and
/// trusting it would let any caller claim any tenant. Only the id
/// looked up from the users module's own store/database for the
/// requested `user_id` is returned, so a payload-carried company
/// that disagrees with the lookup is overwritten with the looked-up
/// one, and one carried for a user with no company (or an unknown
/// or malformed user id) is neutralised instead of passed through.
///
/// The answer is given on both channels the consumers read: the
/// payload gains `company_id` (and `user_id` echoing the requested
/// id, so the import/export resolver can verify the answer belongs
/// to its caller), and the same `{"user_id", "company_id"}` object
/// is added as a result. A user with no company, an unknown user,
/// or a malformed `user_id` gets no company answer at all.
pub(crate) fn on_user_company_requested(store: UsersStore, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| raw.trim().parse::<Uuid>().ok());
    let Some(user_id) = user_id else {
        neutralise_payload_company(ctx);
        return;
    };
    let Some(company_id) = lookup_user_company(&store, user_id) else {
        neutralise_payload_company(ctx);
        return;
    };
    let user_id = user_id.to_string();
    let company_id = company_id.to_string();
    ctx.set("user_id", serde_json::Value::String(user_id.clone()));
    ctx.set("company_id", serde_json::Value::String(company_id.clone()));
    ctx.add_result(serde_json::json!({
        "user_id": user_id,
        "company_id": company_id,
    }));
}

/// Drop a client-supplied `company_id` from the payload so it can
/// never be mistaken for a server-side answer when this handler has
/// no company to return. A no-op when the payload carries none.
fn neutralise_payload_company(ctx: &mut HookContext) {
    if ctx.get("company_id").is_some() {
        ctx.set("company_id", serde_json::Value::Null);
    }
}

/// Load the role ids assigned to a user from the `user_roles` table.
pub(crate) fn db_role_ids(connection: &mut PgConnection, user_id: Uuid) -> UnifiedResult<Vec<Uuid>> {
    use unified_core::db::schema::user_roles::dsl as user_roles_dsl;
    user_roles_dsl::user_roles
        .filter(user_roles_dsl::user_id.eq(user_id))
        .select(user_roles_dsl::role_id)
        .load::<Uuid>(connection)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load user roles: {error}"))
                .with_trace(format!("{error:?}"))
        })
}

/// Load the role slugs assigned to a user: the role ids from
/// `user_roles`, then their slugs from `roles`. Used by the auth
/// middleware to attach server-resolved roles to the verified
/// caller, so `role.check` capability checks answer from real
/// roles instead of `"anonymous"`. Any database error fails the
/// lookup (the middleware treats that as "no roles", failing
/// closed); the error is never turned into an authentication
/// failure here.
pub(crate) fn db_role_slugs(
    connection: &mut PgConnection,
    user_id: Uuid,
) -> UnifiedResult<Vec<String>> {
    use unified_core::db::schema::roles::dsl as roles_dsl;
    let role_ids = db_role_ids(connection, user_id)?;
    roles_dsl::roles
        .filter(roles_dsl::id.eq_any(role_ids))
        .select(roles_dsl::slug)
        .load::<String>(connection)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load user role slugs: {error}"))
                .with_trace(format!("{error:?}"))
        })
}

/// Load the company id assigned to a user: the `company_id`
/// column of the `users` table, as a string. Used by the auth
/// middleware to attach the server-resolved company to the
/// verified caller, so the handler-facing view carries a real
/// company instead of `None`. `None` when the user has no
/// company or no row exists; any other database error fails the
/// lookup (the middleware treats that as "no company", failing
/// closed); the error is never turned into an authentication
/// failure here, and a company is never invented.
pub(crate) fn db_user_company(
    connection: &mut PgConnection,
    user_id: Uuid,
) -> UnifiedResult<Option<String>> {
    use unified_core::db::schema::users::dsl as users_dsl;
    users_dsl::users
        .find(user_id)
        .select(users_dsl::company_id)
        .first::<Option<Uuid>>(connection)
        .optional()
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load user company: {error}"))
                .with_trace(format!("{error:?}"))
        })
        .map(|company| company.flatten().map(|id| id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_password_is_sixteen_characters() {
        assert_eq!(random_password().chars().count(), 16);
    }

    #[test]
    fn random_password_covers_all_character_classes() {
        let password = random_password();
        assert!(password.chars().any(|c| c.is_ascii_uppercase()));
        assert!(password.chars().any(|c| c.is_ascii_lowercase()));
        assert!(password.chars().any(|c| c.is_ascii_digit()));
        assert!(password.chars().any(|c| "!@#$%^&*".contains(c)));
    }

    #[test]
    fn random_passwords_differ() {
        assert_ne!(random_password(), random_password());
    }

    fn seed_user(store: &UsersStore, company_id: Option<Uuid>) -> Uuid {
        let id = Uuid::now_v7();
        let now = Utc::now();
        store.insert(
            id,
            StoredUser {
                id,
                email: "company-user@example.com".to_string(),
                name: "Company User".to_string(),
                password_hash: "hashed".to_string(),
                is_active: true,
                company_id,
                roles: Vec::new(),
                created_at: now,
                updated_at: now,
            },
        );
        id
    }

    fn company_context(user_id: &str) -> HookContext {
        HookContext::new(
            "user.company_requested",
            serde_json::json!({ "user_id": user_id }),
        )
    }

    #[test]
    fn company_requested_answers_known_users_company() {
        let store: UsersStore = Arc::new(DashMap::new());
        let company_id = Uuid::now_v7();
        let user_id = seed_user(&store, Some(company_id));
        let mut ctx = company_context(&user_id.to_string());
        on_user_company_requested(store, &mut ctx);
        assert_eq!(
            ctx.get("company_id").and_then(|value| value.as_str()),
            Some(company_id.to_string()).as_deref()
        );
        assert_eq!(
            ctx.get("user_id").and_then(|value| value.as_str()),
            Some(user_id.to_string()).as_deref()
        );
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(
            ctx.results[0].get("company_id").and_then(|value| value.as_str()),
            Some(company_id.to_string()).as_deref()
        );
        assert_eq!(
            ctx.results[0].get("user_id").and_then(|value| value.as_str()),
            Some(user_id.to_string()).as_deref()
        );
    }

    #[test]
    fn company_requested_answers_nothing_for_unknown_user() {
        let store: UsersStore = Arc::new(DashMap::new());
        let mut ctx = company_context(&Uuid::now_v7().to_string());
        on_user_company_requested(store, &mut ctx);
        assert!(ctx.get("company_id").is_none());
        assert!(ctx.results.is_empty());
    }

    #[test]
    fn company_requested_answers_nothing_for_user_without_company() {
        let store: UsersStore = Arc::new(DashMap::new());
        let user_id = seed_user(&store, None);
        let mut ctx = company_context(&user_id.to_string());
        on_user_company_requested(store, &mut ctx);
        assert!(ctx.get("company_id").is_none());
        assert!(ctx.results.is_empty());
    }

    #[test]
    fn company_requested_ignores_payload_carried_company_id() {
        let store: UsersStore = Arc::new(DashMap::new());
        let company_id = Uuid::now_v7();
        let spoofed = Uuid::now_v7();
        assert_ne!(company_id, spoofed);
        let user_id = seed_user(&store, Some(company_id));
        let mut ctx = HookContext::new(
            "user.company_requested",
            serde_json::json!({
                "user_id": user_id.to_string(),
                "company_id": spoofed.to_string(),
            }),
        );
        on_user_company_requested(store, &mut ctx);
        assert_eq!(
            ctx.get("company_id").and_then(|value| value.as_str()),
            Some(company_id.to_string()).as_deref()
        );
        assert_eq!(
            ctx.results[0].get("company_id").and_then(|value| value.as_str()),
            Some(company_id.to_string()).as_deref()
        );
    }

    #[test]
    fn company_requested_neutralises_payload_company_for_unknown_user() {
        let store: UsersStore = Arc::new(DashMap::new());
        let spoofed = Uuid::now_v7();
        let mut ctx = HookContext::new(
            "user.company_requested",
            serde_json::json!({
                "user_id": Uuid::now_v7().to_string(),
                "company_id": spoofed.to_string(),
            }),
        );
        on_user_company_requested(store, &mut ctx);
        assert_ne!(
            ctx.get("company_id").and_then(|value| value.as_str()),
            Some(spoofed.to_string()).as_deref()
        );
        assert!(ctx.results.is_empty());
    }

    #[test]
    fn company_requested_answers_nothing_for_malformed_or_missing_user_id() {
        let store: UsersStore = Arc::new(DashMap::new());
        let mut ctx = company_context("not-a-uuid");
        on_user_company_requested(store.clone(), &mut ctx);
        assert!(ctx.get("company_id").is_none());
        assert!(ctx.results.is_empty());
        let mut ctx = HookContext::new("user.company_requested", serde_json::json!({}));
        on_user_company_requested(store, &mut ctx);
        assert!(ctx.get("company_id").is_none());
        assert!(ctx.results.is_empty());
    }
}
