// Auditor handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::sharing::{
    generate_auditor_password,
    sharing_events,
    AuditorScopeEntry,
    MIN_AUDITOR_TTL_HOURS,
};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Auditor handlers
// ---------------------------------------------------------------------------

pub(crate) async fn create_auditor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    body: web::Json<CreateAuditorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<AuditorCreated> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_CREATE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let settings = state.settings();
        if !settings.auditors_enabled {
            return Err(UnifiedError::forbidden(
                "Auditor accounts are disabled by configuration",
            ));
        }
        let input = body.into_inner();
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        if !valid_email(&input.email) {
            return Err(UnifiedError::bad_request("email must be a valid email address"));
        }
        if input.expires_in_hours < MIN_AUDITOR_TTL_HOURS
            || input.expires_in_hours > settings.max_auditor_ttl_hours
        {
            return Err(UnifiedError::bad_request(format!(
                "expires_in_hours must be between {} and {}",
                MIN_AUDITOR_TTL_HOURS, settings.max_auditor_ttl_hours
            )));
        }
        if input.scopes.is_empty() {
            return Err(UnifiedError::bad_request(
                "scopes must contain at least one entry; an auditor with no scope sees nothing",
            ));
        }
        if input.scopes.len() > MAX_SCOPE_ENTRIES {
            return Err(UnifiedError::bad_request(format!(
                "scopes must contain at most {MAX_SCOPE_ENTRIES} entries"
            )));
        }
        let mut scopes: Vec<AuditorScopeEntry> = Vec::new();
        for entry in &input.scopes {
            let validated = validate_scope_input(entry)?;
            // A scoped caller may only grant scopes naming their
            // own company: a global scope would grant audit power
            // over every company. An unresolved caller (the
            // platform control plane) may grant any scope.
            require_scope_entry_company(&scope, &validated.company_id)?;
            scopes.push(validated);
        }
        scopes.sort_by(|a, b| {
            a.module
                .cmp(&b.module)
                .then(a.resource_kind.cmp(&b.resource_kind))
                .then(a.resource_id.cmp(&b.resource_id))
        });
        scopes.dedup_by(|a, b| {
            a.module == b.module
                && a.resource_kind == b.resource_kind
                && a.resource_id == b.resource_id
                && a.company_id == b.company_id
                && a.allow_export == b.allow_export
        });
        let now = Utc::now();
        let expires_at = now + chrono::Duration::hours(input.expires_in_hours);
        // One email, one active auditor account: a second active
        // account for the same email (compared case-insensitively)
        // is a conflict. Revoked or expired accounts do not block
        // a fresh account for the email.
        let email_normalized = input.email.trim().to_lowercase();
        if state.auditors.iter().any(|entry| {
            entry.value().is_usable(now)
                && entry.value().email.trim().to_lowercase() == email_normalized
        }) {
            return Err(UnifiedError::conflict(
                "An active auditor account already exists for this email",
            ));
        }
        // One-time password: generated here, hashed with Argon2id,
        // raw value returned exactly once in this response.
        let one_time_password = generate_auditor_password();
        let password_hash = crate::auth::password::hash(&one_time_password)
            .map_err(|error| UnifiedError::internal(format!("Password hashing failed: {error}")))?;
        let mut account = AuditorAccount {
            id: Uuid::now_v7(),
            user_id: Uuid::now_v7().to_string(),
            label,
            email: input.email.trim().to_string(),
            is_auditor: true,
            auditor_of: caller.actor.clone(),
            status: AuditorStatus::Active,
            expires_at,
            password_hash,
            revoked_at: None,
            revoked_by: None,
            revoke_reason: None,
            scheduler_status: "declined".to_string(),
            scheduler_job_id: None,
            created_at: now,
            updated_at: now,
        };
        let (scheduler_status, scheduler_job_id) = schedule_expiry(&state, &account);
        account.scheduler_status = scheduler_status;
        account.scheduler_job_id = scheduler_job_id;
        state.auditors.insert(account.id, account.clone());
        state.scopes.insert(account.id, scopes.clone());
        state.log_auditor_action(
            &caller.actor,
            Some(account.id),
            &format!("auditors:{}", account.id),
            "auditors.created",
            true,
        );
        state.emit(
            sharing_events::AUDITOR_CREATED,
            json!({
                "auditor_id": account.id.to_string(),
                "user_id": account.user_id,
                "auditor_of": account.auditor_of,
                "expires_at": account.expires_at.to_rfc3339(),
            }),
        );
        state.notify(
            "auditors.created",
            &caller.actor,
            "Auditor account created",
            &format!(
                "Auditor \"{}\" was created by {} and expires at {}.",
                account.label,
                caller.actor,
                account.expires_at.to_rfc3339()
            ),
            Some(format!("/api/v1/auditors/{}", account.id)),
        );
        Ok(AuditorCreated {
            account: AuditorAccountView {
                account,
                scopes,
            },
            one_time_password,
        })
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_auditors(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<PageResult<AuditorAccount>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let params = query.into_inner();
        let now = Utc::now();
        let status_filter = params.filters.get("status").cloned();
        let auditor_of_filter = params.filters.get("auditor_of").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let mut items: Vec<AuditorAccount> = state
            .auditors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|account| {
                // Company scope first: an account is visible only
                // when at least one of its scope entries names a
                // company in the caller's scope.
                if !account_in_scope(&state, account.id, &scope) {
                    return false;
                }
                if let Some(wanted) = &status_filter {
                    if account.effective_status(now).as_str() != wanted.as_str() {
                        return false;
                    }
                }
                if let Some(wanted) = &auditor_of_filter {
                    if &account.auditor_of != wanted {
                        return false;
                    }
                }
                if let Some(wanted) = &company_filter {
                    let has_company = state
                        .scopes
                        .get(&account.id)
                        .map(|entries| {
                            entries.iter().any(|entry| {
                                entry
                                    .company_id
                                    .is_some_and(|id| id.to_string() == *wanted)
                            })
                        })
                        .unwrap_or(false);
                    if !has_company {
                        return false;
                    }
                }
                if let Some(search) = &params.search {
                    let needle = search.to_lowercase();
                    if !account.label.to_lowercase().contains(&needle)
                        && !account.email.to_lowercase().contains(&needle)
                        && !account.user_id.to_lowercase().contains(&needle)
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_auditor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<AuditorAccountView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let account = load_auditor(&state, &path.into_inner())?;
        require_account_in_scope(&state, account.id, &scope)?;
        let scopes = state.scope_of(account.id).entries;
        Ok(AuditorAccountView { account, scopes })
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn revoke_auditor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RevokeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<AuditorAccountView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_REVOKE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(value) => Some(require_text(value, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let id = parse_uuid(&path.into_inner(), "auditor")?;
        // Scope check before the write guard below: the account
        // must be in the caller's company scope to mutate it.
        let account = load_auditor(&state, &id.to_string())?;
        require_account_in_scope(&state, account.id, &scope)?;
        let now = Utc::now();
        // Scope the write guard: it must be released before any
        // later access to the same map (load_auditor below re-locks
        // the same shard, and a held RefMut guard would deadlock).
        // Never shadow the guard with a clone and drop the clone:
        // that drops the owned copy, not the guard.
        {
            let mut account = state
                .auditors
                .get_mut(&id)
                .ok_or_else(|| UnifiedError::not_found(format!("Auditor not found: {id}")))?;
            if account.status == AuditorStatus::Revoked {
                return Err(UnifiedError::conflict("Auditor account is already revoked"));
            }
            account.status = AuditorStatus::Revoked;
            account.revoked_at = Some(now);
            account.revoked_by = Some(caller.actor.clone());
            account.revoke_reason = reason.clone();
            account.updated_at = now;
        }
        // One-click revoke kills live sessions immediately.
        let killed = state.kill_sessions(id, &caller.actor);
        state.log_auditor_action(
            &caller.actor,
            Some(id),
            &format!("auditors:{id}"),
            "auditors.revoked",
            true,
        );
        state.emit(
            sharing_events::AUDITOR_REVOKED,
            json!({
                "auditor_id": id.to_string(),
                "revoked_by": caller.actor,
                "reason": reason,
                "sessions_killed": killed,
            }),
        );
        state.notify(
            "auditors.revoked",
            &caller.actor,
            "Auditor account revoked",
            &format!("Auditor account {id} was revoked by {}. {killed} live session(s) killed.", caller.actor),
            Some(format!("/api/v1/auditors/{id}")),
        );
        let account = load_auditor(&state, &id.to_string())?;
        let scopes = state.scope_of(id).entries;
        Ok(AuditorAccountView { account, scopes })
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn extend_auditor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ExtendAuditorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<AuditorAccountView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_MANAGE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let input = body.into_inner();
        if input.additional_hours < 1 {
            return Err(UnifiedError::bad_request("additional_hours must be at least 1"));
        }
        let settings = state.settings();
        let id = parse_uuid(&path.into_inner(), "auditor")?;
        // Scope check before the write guard below: the account
        // must be in the caller's company scope to mutate it.
        let account = load_auditor(&state, &id.to_string())?;
        require_account_in_scope(&state, account.id, &scope)?;
        let now = Utc::now();
        let max_expiry = now + chrono::Duration::hours(settings.max_auditor_ttl_hours);
        // Scope the write guard: it must be released before the
        // load_auditor / get_mut calls below re-lock the same shard.
        // Never shadow the guard with a clone and drop the clone:
        // that drops the owned copy, not the guard.
        {
            let mut account = state
                .auditors
                .get_mut(&id)
                .ok_or_else(|| UnifiedError::not_found(format!("Auditor not found: {id}")))?;
            if account.status == AuditorStatus::Revoked {
                return Err(UnifiedError::conflict("A revoked auditor account cannot be extended"));
            }
            // An already-expired account must not be extended: adding
            // hours to a past expiry can still land in the past, so
            // the extension would "succeed" while the account stays
            // expired.
            if account.expires_at <= now {
                return Err(UnifiedError::conflict(
                    "An expired auditor account cannot be extended",
                ));
            }
            let new_expiry = account.expires_at + chrono::Duration::hours(input.additional_hours);
            if new_expiry <= now {
                return Err(UnifiedError::conflict(
                    "An expired auditor account cannot be extended",
                ));
            }
            if new_expiry > max_expiry {
                return Err(UnifiedError::bad_request(format!(
                    "Extension would exceed the maximum auditor lifetime (now + {} hours)",
                    settings.max_auditor_ttl_hours
                )));
            }
            account.expires_at = new_expiry;
            account.updated_at = now;
        }
        state.log_auditor_action(
            &caller.actor,
            Some(id),
            &format!("auditors:{id}"),
            "auditors.extended",
            true,
        );
        // Re-queue the expiry job for the new expiry time.
        let account = load_auditor(&state, &id.to_string())?;
        let (scheduler_status, scheduler_job_id) = schedule_expiry(&state, &account);
        if let Some(mut stored) = state.auditors.get_mut(&id) {
            stored.scheduler_status = scheduler_status;
            stored.scheduler_job_id = scheduler_job_id;
        }
        let account = load_auditor(&state, &id.to_string())?;
        let scopes = state.scope_of(id).entries;
        Ok(AuditorAccountView { account, scopes })
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_auditor_sessions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<PageResult<AuditorSession>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let params = query.into_inner();
        let account = load_auditor(&state, &path.into_inner())?;
        require_account_in_scope(&state, account.id, &scope)?;
        let live_filter = params.filters.get("live").cloned();
        let mut items: Vec<AuditorSession> = state
            .sessions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|session| session.auditor_id == account.id)
            .filter(|session| match live_filter.as_deref() {
                Some("true") => session.is_live(account.is_usable(Utc::now())),
                Some("false") => !session.is_live(account.is_usable(Utc::now())),
                _ => true,
            })
            .collect();
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn revoke_auditor_session(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<AuditorSession> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_REVOKE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let (auditor_raw, session_raw) = path.into_inner();
        let auditor_id = parse_uuid(&auditor_raw, "auditor")?;
        let session_id = parse_uuid(&session_raw, "session")?;
        let account = load_auditor(&state, &auditor_raw)?;
        require_account_in_scope(&state, account.id, &scope)?;
        let _ = auditor_id;
        let now = Utc::now();
        // Scope the write guard: it must be released before the
        // sessions map is read again below. Never shadow the guard
        // with a clone and drop the clone: that drops the owned
        // copy, not the guard. The scoped block returns an owned
        // clone, which is the revoked session to return.
        let session = {
            let mut session = state
                .sessions
                .get_mut(&session_id)
                .filter(|entry| entry.auditor_id == account.id)
                .ok_or_else(|| UnifiedError::not_found(format!("Session not found: {session_id}")))?;
            if session.revoked_at.is_some() {
                return Err(UnifiedError::conflict("Session is already revoked"));
            }
            session.revoked_at = Some(now);
            session.revoked_by = Some(caller.actor.clone());
            session.clone()
        };
        state.log_auditor_action(
            &caller.actor,
            Some(account.id),
            &format!("auditors:{}/sessions:{session_id}", account.id),
            "auditors.session.revoked",
            true,
        );
        state.emit(
            sharing_events::AUDITOR_SESSION_REVOKED,
            json!({
                "auditor_id": account.id.to_string(),
                "session_id": session_id.to_string(),
                "revoked_by": caller.actor,
            }),
        );
        // The guard is already released, so this owned clone is
        // the revoked session; no second map read is needed.
        Ok(session)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_auditor_activity(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<PageResult<AuditorActionLogEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let params = query.into_inner();
        let account = load_auditor(&state, &path.into_inner())?;
        require_account_in_scope(&state, account.id, &scope)?;
        let action_filter = params.filters.get("action").cloned();
        let allowed_filter = params.filters.get("allowed").cloned();
        let mut items: Vec<AuditorActionLogEntry> = state
            .action_log
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.auditor_id == Some(account.id))
            .filter(|entry| match action_filter.as_deref() {
                Some(wanted) => entry.action == wanted,
                None => true,
            })
            .filter(|entry| match allowed_filter.as_deref() {
                Some("true") => entry.allowed,
                Some("false") => !entry.allowed,
                _ => true,
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_active_auditor_sessions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<PageResult<ActiveAuditorSessionView>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_VIEW)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let scope = caller_company(&_tenant, &req)?;
        let params = query.into_inner();
        let now = Utc::now();
        let mut items: Vec<ActiveAuditorSessionView> = Vec::new();
        for session in state.sessions.iter().map(|entry| entry.value().clone()) {
            let Some(account) = state.auditors.get(&session.auditor_id).map(|entry| entry.value().clone()) else {
                continue;
            };
            // Sessions are scoped through their parent account:
            // only sessions of in-scope accounts are listed.
            if !account_in_scope(&state, account.id, &scope) {
                continue;
            }
            if !session.is_live(account.is_usable(now)) {
                continue;
            }
            items.push(ActiveAuditorSessionView {
                session_id: session.id,
                auditor_id: account.id,
                user_id: account.user_id.clone(),
                label: account.label.clone(),
                auditor_of: account.auditor_of.clone(),
                started_at: session.started_at,
                last_seen_at: session.last_seen_at,
                expires_at: account.expires_at,
                scopes: state.scope_of(account.id).entries,
            });
        }
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_my_scope(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        // Self-only read for an auditor caller: no role capability
        // is checked here, because auditors hold no roles. The
        // caller must be an active auditor account; anyone else
        // gets a 403, and an expired or revoked auditor gets a 403
        // too (never a scope).
        let actor = unified_core::caller::require_caller(&req)?.user_id;
        let account = state
            .auditor_for_user(&actor)
            .ok_or_else(|| UnifiedError::forbidden("Caller is not an auditor account"))?;
        if !account.is_usable(Utc::now()) {
            state.log_auditor_action(
                &actor,
                Some(account.id),
                &format!("auditors:{}/scope", account.id),
                "auditors.scope.read",
                false,
            );
            return Err(UnifiedError::forbidden(
                "Auditor account is expired or revoked",
            ));
        }
        state.log_auditor_action(
            &actor,
            Some(account.id),
            &format!("auditors:{}/scope", account.id),
            "auditors.scope.read",
            true,
        );
        Ok(json!({
            "auditor_id": account.id,
            "user_id": account.user_id,
            "label": account.label,
            "auditor_of": account.auditor_of,
            "expires_at": account.expires_at,
            "is_auditor": true,
            "read_only": true,
            "scopes": state.scope_of(account.id).entries,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn expire_sweep(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AuditorsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AUDITORS_MANAGE)?;
        reject_auditor_caller(&state, &caller.actor)?;
        let expired = expire_due_auditors(&state, Utc::now());
        Ok(json!({
            "expired": expired.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
            "expired_count": expired.len(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );

        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;
    use chrono::{DateTime, Utc};

    /// Context plus state sharing one hook bus whose `role.check`
    /// answer allows every permission, so the handlers run end to
    /// end. No scheduler listener is registered, so expiry jobs
    /// report `declined` (the honest path) without affecting the
    /// handler under test.
    fn test_env() -> (web::Data<Arc<AppContext>>, web::Data<AuditorsState>) {
        let bus = Arc::new(HookBus::new());
        bus.register(
            "role.check",
            "test",
            Arc::new(|ctx: &mut HookContext| {
                ctx.set("allowed", serde_json::json!(true));
            }),
        );
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::clone(&bus),
            Arc::new(ModuleRegistry::new()),
        );
        let state = AuditorsState::default();
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(bus);
        (web::Data::new(Arc::new(ctx)), web::Data::new(state))
    }

    fn admin_request() -> HttpRequest {
        verified_request("admin-1", "admin")
    }

    fn seed_account(
        state: &AuditorsState,
        email: &str,
        status: AuditorStatus,
        expires_at: DateTime<Utc>,
    ) -> AuditorAccount {
        let account = AuditorAccount {
            id: Uuid::now_v7(),
            user_id: format!("user-{}", Uuid::now_v7()),
            label: "Test auditor".to_string(),
            email: email.to_string(),
            is_auditor: true,
            auditor_of: "admin-1".to_string(),
            status,
            expires_at,
            password_hash: "hash".to_string(),
            revoked_at: None,
            revoked_by: None,
            revoke_reason: None,
            scheduler_status: "declined".to_string(),
            scheduler_job_id: None,
            created_at: expires_at,
            updated_at: expires_at,
        };
        state.auditors.insert(account.id, account.clone());
        // A global scope entry (no company): the unresolved test
        // admin sees the account through the account-scope check.
        state.scopes.insert(
            account.id,
            vec![unified_core::sharing::AuditorScopeEntry {
                module: "billing".to_string(),
                resource_kind: "invoice".to_string(),
                resource_id: None,
                company_id: None,
                allow_export: false,
            }],
        );
        account
    }

    fn seed_session(state: &AuditorsState, account: &AuditorAccount) -> AuditorSession {
        let now = Utc::now();
        let session = AuditorSession {
            id: Uuid::now_v7(),
            auditor_id: account.id,
            user_id: account.user_id.clone(),
            started_at: now,
            last_seen_at: now,
            revoked_at: None,
            revoked_by: None,
        };
        state.sessions.insert(session.id, session.clone());
        session
    }

    fn create_body(email: &str) -> web::Json<CreateAuditorRequest> {
        web::Json(CreateAuditorRequest {
            label: "Test auditor".to_string(),
            email: email.to_string(),
            expires_in_hours: 24,
            scopes: vec![AuditorScopeInput {
                module: "billing".to_string(),
                resource_kind: "invoice".to_string(),
                resource_id: None,
                company_id: None,
                allow_export: false,
            }],
        })
    }

    #[tokio::test]
    async fn revoke_auditor_completes_and_revokes_account() {
        // Regression test for the shadowed-clone deadlock: the old
        // code held the auditors write guard while load_auditor
        // re-locked the same shard, so this call hung forever.
        let (ctx, state) = test_env();
        let account = seed_account(
            &state,
            "revoke@example.test",
            AuditorStatus::Active,
            Utc::now() + chrono::Duration::hours(1),
        );
        let response = revoke_auditor(
            ctx,
            state.clone(),
            admin_request(),
            web::Path::from(account.id.to_string()),
            web::Json(RevokeRequest {
                reason: Some("no longer needed".to_string()),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let stored = state
            .auditors
            .get(&account.id)
            .expect("account kept")
            .value()
            .clone();
        assert_eq!(stored.status, AuditorStatus::Revoked);
        assert!(stored.revoked_at.is_some());
        assert_eq!(
            stored.revoked_by.as_deref(),
            Some(test_user_uuid("admin-1").to_string().as_str())
        );
    }

    #[tokio::test]
    async fn revoke_auditor_twice_conflicts() {
        let (ctx, state) = test_env();
        let account = seed_account(
            &state,
            "revoke-twice@example.test",
            AuditorStatus::Active,
            Utc::now() + chrono::Duration::hours(1),
        );
        let first = revoke_auditor(
            ctx.clone(),
            state.clone(),
            admin_request(),
            web::Path::from(account.id.to_string()),
            web::Json(RevokeRequest { reason: None }),
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let second = revoke_auditor(
            ctx,
            state,
            admin_request(),
            web::Path::from(account.id.to_string()),
            web::Json(RevokeRequest { reason: None }),
        )
        .await;
        assert_eq!(second.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn extend_auditor_completes_and_extends_expiry() {
        // Regression test for the shadowed-clone deadlock: the old
        // code held the auditors write guard while load_auditor
        // re-locked the same shard, so this call hung forever.
        let (ctx, state) = test_env();
        let before = Utc::now() + chrono::Duration::hours(1);
        let account = seed_account(
            &state,
            "extend@example.test",
            AuditorStatus::Active,
            before,
        );
        let response = extend_auditor(
            ctx,
            state.clone(),
            admin_request(),
            web::Path::from(account.id.to_string()),
            web::Json(ExtendAuditorRequest {
                additional_hours: 2,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let stored = state
            .auditors
            .get(&account.id)
            .expect("account kept")
            .value()
            .clone();
        assert!(stored.expires_at > before);
        assert_eq!(stored.status, AuditorStatus::Active);
    }

    #[tokio::test]
    async fn extend_auditor_rejects_expired_account() {
        // Adding hours to an already-past expiry can still land in
        // the past, so the extension must be refused instead of
        // "succeeding" while the account stays expired. Here the
        // account expired 2 hours ago and 1 extra hour still
        // leaves the new expiry in the past.
        let (ctx, state) = test_env();
        let expired_at = Utc::now() - chrono::Duration::hours(2);
        let account = seed_account(
            &state,
            "expired@example.test",
            AuditorStatus::Active,
            expired_at,
        );
        let response = extend_auditor(
            ctx,
            state.clone(),
            admin_request(),
            web::Path::from(account.id.to_string()),
            web::Json(ExtendAuditorRequest {
                additional_hours: 1,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let stored = state
            .auditors
            .get(&account.id)
            .expect("account kept")
            .value()
            .clone();
        assert_eq!(stored.expires_at, expired_at);
    }

    #[tokio::test]
    async fn revoke_auditor_session_completes_and_revokes_session() {
        // Regression test for the shadowed-clone deadlock: the old
        // code held the sessions write guard while the same map
        // was read again to build the response, so this call hung
        // forever.
        let (ctx, state) = test_env();
        let account = seed_account(
            &state,
            "session@example.test",
            AuditorStatus::Active,
            Utc::now() + chrono::Duration::hours(1),
        );
        let session = seed_session(&state, &account);
        let response = revoke_auditor_session(
            ctx,
            state.clone(),
            admin_request(),
            web::Path::from((account.id.to_string(), session.id.to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let stored = state
            .sessions
            .get(&session.id)
            .expect("session kept")
            .value()
            .clone();
        assert!(stored.revoked_at.is_some());
        assert_eq!(
            stored.revoked_by.as_deref(),
            Some(test_user_uuid("admin-1").to_string().as_str())
        );
    }

    #[tokio::test]
    async fn create_auditor_rejects_duplicate_active_email() {
        let (ctx, state) = test_env();
        let first = create_auditor(
            ctx.clone(),
            state.clone(),
            admin_request(),
            create_body("dup@example.test"),
        )
        .await;
        assert_eq!(first.status(), StatusCode::CREATED);
        // Same email with different casing is still a duplicate.
        let second = create_auditor(
            ctx,
            state,
            admin_request(),
            create_body("DUP@Example.test"),
        )
        .await;
        assert_eq!(second.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn create_auditor_allows_same_email_after_revoke() {
        // Only an *active* account blocks the email; a revoked
        // account does not.
        let (ctx, state) = test_env();
        seed_account(
            &state,
            "reused@example.test",
            AuditorStatus::Revoked,
            Utc::now() + chrono::Duration::hours(1),
        );
        let response = create_auditor(
            ctx,
            state,
            admin_request(),
            create_body("reused@example.test"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_auditor_allows_same_email_after_expiry() {
        // An account whose expiry has passed is not active, so the
        // email is free for a fresh account.
        let (ctx, state) = test_env();
        seed_account(
            &state,
            "reused-expired@example.test",
            AuditorStatus::Active,
            Utc::now() - chrono::Duration::hours(1),
        );
        let response = create_auditor(
            ctx,
            state,
            admin_request(),
            create_body("reused-expired@example.test"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
    }
}
