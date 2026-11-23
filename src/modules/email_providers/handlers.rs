//! email_providers - handlers.

use super::{
    adapters::*, credentials::*, crypto::*, events::*, hooks::*, http::*, module::*,
    provider_kinds::*, registry::*, store::*, worker::*,
};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json as api_json;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// HTTP handlers.
// ---------------------------------------------------------------------------
pub struct EmailProviderState {
    pub(crate) hooks: RwLock<Option<Arc<HookBus>>>,
    pub(crate) debug: AtomicBool,
}

impl EmailProviderState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("email hooks lock poisoned") = Some(hooks.clone());
        *email_hooks_store()
            .write()
            .expect("email hooks lock poisoned") = Some(hooks);
    }

    pub(crate) fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(hooks) = self
            .hooks
            .read()
            .expect("email hooks lock poisoned")
            .clone()
        {
            hooks.emit(event, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Caller identity and capability checks (same trusted-parameter
// pattern as the tenants module: X-User-Id plus X-Role headers,
// `role.check` hook, fail closed). Every mutating or sending
// handler below requires `email_providers:manage`; read views
// return masked connections only, so they stay open.
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, group B).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only: no per-record company check is applied.
/// Real partitioning needs a `tenant_id` on the records, a C11
/// follow-up.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// -- a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

pub(crate) const PERM_EMAIL_PROVIDERS_MANAGE: &str = "email_providers:manage";

pub(crate) struct EmailCaller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<EmailCaller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(EmailCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no hook bus, or no answer,
/// means not allowed.
pub(crate) fn check_capability(state: &EmailProviderState, role: &str, permission: &str) -> bool {
    let hooks = state
        .hooks
        .read()
        .expect("email hooks lock poisoned")
        .clone()
        .or_else(email_hooks);
    let Some(hooks) = hooks else {
        return false;
    };
    let context = hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    state: &EmailProviderState,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<()> {
    let caller = caller(req)?;
    if check_capability(state, &caller.role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{permission}\"",
            caller.role
        )))
    }
}

pub(crate) async fn list_catalog() -> HttpResponse {
    let catalog: Vec<Value> = EmailProviderKind::all()
        .iter()
        .map(|kind| {
            json!({
                "kind": kind.as_str(),
                "name": kind.display_name(),
                "logo": kind.logo(),
                "capabilities": capabilities_for(*kind),
                "settings_fields": settings_fields_for_catalog(*kind),
                "domain_notes": kind.domain_notes(),
                "webhook_notes": kind.webhook_notes(),
            })
        })
        .collect();
    api_json::success_response(json!({ "providers": catalog }), 200)
}

/// Settings fields for one provider kind, after the
/// `email_provider.settings_fields` hook has had its say (task 350,
/// addition A).
///
/// Contract: the catalog builds the base list from the registry
/// definition, then emits `email_provider.settings_fields` with
/// `{ kind, fields }`. A hook handler may contribute one of:
/// - `{ "add": [field, ...] }` — fields appended to the base list
///   (a field whose `key` already exists replaces the base field);
/// - `{ "remove": ["key", ...] }` — base fields dropped by key;
/// - `{ "fields": [field, ...] }` — the whole list replaced.
/// Malformed contributions are ignored, never fatal: a broken hook
/// must not break the catalog. Field objects use the same shape as
/// `SettingsFieldDef` (`key`, `label`, `type`, `required`, `secret`,
/// `placeholder`, `help`). With no hook bus (unit tests, module not
/// initialized) the base list is returned unchanged.
pub(crate) fn settings_fields_for_catalog(kind: EmailProviderKind) -> Vec<SettingsFieldDef> {
    let mut fields = settings_fields(kind);
    let Some(hooks) = email_hooks() else {
        return fields;
    };
    let emitted = hooks.emit(
        EVENT_EMAIL_PROVIDER_SETTINGS_FIELDS,
        json!({ "kind": kind.as_str(), "fields": fields }),
    );
    for result in &emitted.results {
        if let Some(replacement) = result.get("fields").and_then(|value| value.as_array()) {
            let parsed: Vec<SettingsFieldDef> = replacement
                .iter()
                .filter_map(|value| serde_json::from_value(value.clone()).ok())
                .collect();
            if !parsed.is_empty() {
                fields = parsed;
            }
        }
        if let Some(remove) = result.get("remove").and_then(|value| value.as_array()) {
            let keys: Vec<String> = remove
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect();
            fields.retain(|field| !keys.contains(&field.key));
        }
        if let Some(add) = result.get("add").and_then(|value| value.as_array()) {
            for value in add {
                let Ok(field) = serde_json::from_value::<SettingsFieldDef>(value.clone()) else {
                    continue;
                };
                if field.key.trim().is_empty() {
                    continue;
                }
                if let Some(existing) = fields.iter_mut().find(|item| item.key == field.key) {
                    *existing = field;
                } else {
                    fields.push(field);
                }
            }
        }
    }
    fields
}

/// Encrypt one OAuth secret for storage, when a value was supplied.
/// Empty means "not set". When no `app.secret_key` is configured the
/// write is refused (fail closed) instead of storing plaintext.
pub(crate) fn encrypt_stored_secret(value: &Option<String>) -> UnifiedResult<Option<String>> {
    match value.as_deref().filter(|value| !value.trim().is_empty()) {
        None => Ok(None),
        Some(plaintext) => encrypt_secret(plaintext).map(Some),
    }
}

pub(crate) fn paginate_connections(
    mut items: Vec<EmailConnectionView>,
    query: &PaginationParams,
) -> HttpResponse {
    let pagination = query.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<EmailConnectionView> = items
        .drain(..)
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(
        PageResult::new(page_items, total, &pagination).to_json(),
        200,
    )
}

pub(crate) async fn list_connections(
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, false),
    };
    let params = query.into_inner();
    let mut items: Vec<EmailConnection> = connections_store()
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(kind) = params.filters.get("kind") {
        items.retain(|connection| connection.kind.as_str() == kind);
    }
    if let Some(enabled) = params.filters.get("enabled") {
        let wanted = enabled == "true" || enabled == "1";
        items.retain(|connection| connection.enabled == wanted);
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|connection| connection.name.to_lowercase().contains(&needle));
    }
    let sort = params.allowed_sort(&["name", "kind", "created_at"], "created_at");
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "kind" => items.sort_by(|a, b| a.kind.as_str().cmp(b.kind.as_str())),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let views: Vec<EmailConnectionView> = items.into_iter().map(|c| c.view()).collect();
    paginate_connections(views, &params)
}

pub(crate) async fn create_connection(
    state: web::Data<Arc<EmailProviderState>>,
    req: HttpRequest,
    body: web::Json<CreateConnectionRequest>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&state, &req, PERM_EMAIL_PROVIDERS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    let kind = match validate_connection(&input) {
        Ok(kind) => kind,
        Err(error) => return api_json::error_response(&error, debug),
    };
    // OAuth secrets are encrypted at rest (crypto.rs). When no
    // app.secret_key is configured the create is refused instead of
    // storing them in plaintext.
    let credentials = match (|| -> UnifiedResult<EmailCredentials> {
        Ok(EmailCredentials {
            api_key: input
                .api_key
                .clone()
                .filter(|value| !value.trim().is_empty()),
            access_key_id: input
                .access_key_id
                .clone()
                .filter(|value| !value.trim().is_empty()),
            secret_access_key: input
                .secret_access_key
                .clone()
                .filter(|value| !value.trim().is_empty()),
            webhook_secret: input
                .webhook_secret
                .clone()
                .filter(|value| !value.trim().is_empty()),
            password: encrypt_stored_secret(&input.password)?,
            client_id: input
                .client_id
                .clone()
                .filter(|value| !value.trim().is_empty()),
            client_secret: encrypt_stored_secret(&input.client_secret)?,
            refresh_token: encrypt_stored_secret(&input.refresh_token)?,
            access_token: encrypt_stored_secret(&input.access_token)?,
            access_token_expires_at: input.access_token_expires_at,
        })
    })() {
        Ok(credentials) => credentials,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let now = Utc::now();
    let connection = EmailConnection {
        id: Uuid::now_v7(),
        name: input.name.trim().to_string(),
        kind,
        domain: input.domain.filter(|value| !value.trim().is_empty()),
        region: input.region.filter(|value| !value.trim().is_empty()),
        sender_email: input.sender_email.filter(|value| !value.trim().is_empty()),
        sender_name: input.sender_name.filter(|value| !value.trim().is_empty()),
        webhook_url: input.webhook_url.filter(|value| !value.trim().is_empty()),
        smtp_host: input.smtp_host.filter(|value| !value.trim().is_empty()),
        smtp_port: input.smtp_port,
        smtp_tls: input.smtp_tls.filter(|value| !value.trim().is_empty()),
        smtp_username: input.smtp_username.filter(|value| !value.trim().is_empty()),
        credentials,
        test_mode: input.test_mode.unwrap_or(false),
        enabled: input.enabled.unwrap_or(true),
        last_tested_at: None,
        last_status: None,
        created_at: now,
        updated_at: now,
    };
    connections_store().insert(connection.id, connection.clone());
    state.emit(
        EVENT_EMAIL_PROVIDER_CONNECTION_CREATED,
        json!({
            "connection_id": connection.id.to_string(),
            "kind": connection.kind.as_str(),
            "name": connection.name,
        }),
    );
    api_json::success_response(connection.view(), 201)
}

pub(crate) fn parse_connection_id(path: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(path)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid connection id \"{path}\"")))
}

pub(crate) async fn get_connection_handler(
    state: web::Data<Arc<EmailProviderState>>,
    path: web::Path<String>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let id = match parse_connection_id(&path.into_inner()) {
        Ok(id) => id,
        Err(error) => return api_json::error_response(&error, debug),
    };
    match get_connection(&id) {
        Some(connection) => api_json::success_response(connection.view(), 200),
        None => api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        ),
    }
}

pub(crate) async fn update_connection(
    state: web::Data<Arc<EmailProviderState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateConnectionRequest>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&state, &req, PERM_EMAIL_PROVIDERS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = match parse_connection_id(&path.into_inner()) {
        Ok(id) => id,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let mut connections = connections_store();
    let Some(mut connection) = connections.get_mut(&id) else {
        return api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        );
    };
    // Re-validate the merged record so a partial update cannot drop a
    // credential the provider kind requires.
    let merged = CreateConnectionRequest {
        name: input
            .name
            .clone()
            .unwrap_or_else(|| connection.name.clone()),
        kind: connection.kind.as_str().to_string(),
        domain: input.domain.clone().or_else(|| connection.domain.clone()),
        region: input.region.clone().or_else(|| connection.region.clone()),
        sender_email: input
            .sender_email
            .clone()
            .or_else(|| connection.sender_email.clone()),
        sender_name: input
            .sender_name
            .clone()
            .or_else(|| connection.sender_name.clone()),
        webhook_url: input
            .webhook_url
            .clone()
            .or_else(|| connection.webhook_url.clone()),
        smtp_host: input
            .smtp_host
            .clone()
            .or_else(|| connection.smtp_host.clone()),
        smtp_port: input.smtp_port.or(connection.smtp_port),
        smtp_tls: input
            .smtp_tls
            .clone()
            .or_else(|| connection.smtp_tls.clone()),
        smtp_username: input
            .smtp_username
            .clone()
            .or_else(|| connection.smtp_username.clone()),
        api_key: input
            .api_key
            .clone()
            .or_else(|| connection.credentials.api_key.clone()),
        access_key_id: input
            .access_key_id
            .clone()
            .or_else(|| connection.credentials.access_key_id.clone()),
        secret_access_key: input
            .secret_access_key
            .clone()
            .or_else(|| connection.credentials.secret_access_key.clone()),
        webhook_secret: input
            .webhook_secret
            .clone()
            .or_else(|| connection.credentials.webhook_secret.clone()),
        password: input
            .password
            .clone()
            .or_else(|| connection.credentials.password.clone()),
        client_id: input
            .client_id
            .clone()
            .or_else(|| connection.credentials.client_id.clone()),
        client_secret: input
            .client_secret
            .clone()
            .or_else(|| connection.credentials.client_secret.clone()),
        refresh_token: input
            .refresh_token
            .clone()
            .or_else(|| connection.credentials.refresh_token.clone()),
        access_token: input
            .access_token
            .clone()
            .or_else(|| connection.credentials.access_token.clone()),
        access_token_expires_at: input
            .access_token_expires_at
            .or(connection.credentials.access_token_expires_at),
        test_mode: input.test_mode.or(Some(connection.test_mode)),
        enabled: input.enabled.or(Some(connection.enabled)),
    };
    if let Err(error) = validate_connection(&merged) {
        return api_json::error_response(&error, debug);
    }
    if let Some(name) = input.name {
        connection.name = name.trim().to_string();
    }
    if let Some(domain) = input.domain {
        connection.domain = if domain.trim().is_empty() {
            None
        } else {
            Some(domain)
        };
    }
    if let Some(region) = input.region {
        connection.region = if region.trim().is_empty() {
            None
        } else {
            Some(region)
        };
    }
    if let Some(sender_email) = input.sender_email {
        connection.sender_email = if sender_email.trim().is_empty() {
            None
        } else {
            Some(sender_email)
        };
    }
    if let Some(sender_name) = input.sender_name {
        connection.sender_name = if sender_name.trim().is_empty() {
            None
        } else {
            Some(sender_name)
        };
    }
    if let Some(webhook_url) = input.webhook_url {
        connection.webhook_url = if webhook_url.trim().is_empty() {
            None
        } else {
            Some(webhook_url)
        };
    }
    // Secrets are write-only: an omitted field keeps the stored value,
    // an empty string clears it.
    if let Some(api_key) = input.api_key {
        connection.credentials.api_key = if api_key.is_empty() {
            None
        } else {
            Some(api_key)
        };
    }
    if let Some(access_key_id) = input.access_key_id {
        connection.credentials.access_key_id = if access_key_id.is_empty() {
            None
        } else {
            Some(access_key_id)
        };
    }
    if let Some(secret_access_key) = input.secret_access_key {
        connection.credentials.secret_access_key = if secret_access_key.is_empty() {
            None
        } else {
            Some(secret_access_key)
        };
    }
    if let Some(webhook_secret) = input.webhook_secret {
        connection.credentials.webhook_secret = if webhook_secret.is_empty() {
            None
        } else {
            Some(webhook_secret)
        };
    }
    if let Some(smtp_host) = input.smtp_host {
        connection.smtp_host = if smtp_host.trim().is_empty() {
            None
        } else {
            Some(smtp_host)
        };
    }
    if let Some(smtp_port) = input.smtp_port {
        connection.smtp_port = Some(smtp_port);
    }
    if let Some(smtp_tls) = input.smtp_tls {
        connection.smtp_tls = if smtp_tls.trim().is_empty() {
            None
        } else {
            Some(smtp_tls)
        };
    }
    if let Some(smtp_username) = input.smtp_username {
        connection.smtp_username = if smtp_username.trim().is_empty() {
            None
        } else {
            Some(smtp_username)
        };
    }
    // OAuth secrets are write-only and encrypted at rest: an omitted
    // field keeps the stored value, an empty string clears it, a new
    // value is encrypted (refused when no app.secret_key is set).
    // Pre-encrypt first so a refused write changes nothing.
    let new_password = match encrypt_stored_secret(&input.password) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let new_client_secret = match encrypt_stored_secret(&input.client_secret) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let new_refresh_token = match encrypt_stored_secret(&input.refresh_token) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let new_access_token = match encrypt_stored_secret(&input.access_token) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if input.password.is_some() {
        connection.credentials.password = new_password;
    }
    if let Some(client_id) = input.client_id {
        connection.credentials.client_id = if client_id.trim().is_empty() {
            None
        } else {
            Some(client_id)
        };
    }
    if input.client_secret.is_some() {
        connection.credentials.client_secret = new_client_secret;
    }
    if input.refresh_token.is_some() {
        connection.credentials.refresh_token = new_refresh_token;
    }
    if input.access_token.is_some() {
        connection.credentials.access_token = new_access_token;
    }
    if input.access_token_expires_at.is_some() {
        connection.credentials.access_token_expires_at = input.access_token_expires_at;
    }
    if let Some(test_mode) = input.test_mode {
        connection.test_mode = test_mode;
    }
    if let Some(enabled) = input.enabled {
        connection.enabled = enabled;
    }
    connection.updated_at = Utc::now();
    let view = connection.view();
    drop(connections);
    api_json::success_response(view, 200)
}

pub(crate) async fn delete_connection(
    state: web::Data<Arc<EmailProviderState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&state, &req, PERM_EMAIL_PROVIDERS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = match parse_connection_id(&path.into_inner()) {
        Ok(id) => id,
        Err(error) => return api_json::error_response(&error, debug),
    };
    match connections_store().remove(&id) {
        Some((_, connection)) => {
            {
                let mut default = default_connection_store()
                    .write()
                    .expect("default connection lock poisoned");
                if *default == Some(id) {
                    *default = None;
                }
            }
            state.emit(
                EVENT_EMAIL_PROVIDER_CONNECTION_DELETED,
                json!({
                    "connection_id": id.to_string(),
                    "kind": connection.kind.as_str(),
                    "name": connection.name,
                }),
            );
            api_json::success_response(json!({ "deleted": true, "id": id.to_string() }), 200)
        }
        None => api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        ),
    }
}

pub(crate) async fn test_connection_handler(
    state: web::Data<Arc<EmailProviderState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&state, &req, PERM_EMAIL_PROVIDERS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = match parse_connection_id(&path.into_inner()) {
        Ok(id) => id,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let Some(connection) = get_connection(&id) else {
        return api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        );
    };
    let adapter = match email_provider_registry().build(&connection) {
        Ok(adapter) => adapter,
        Err(error) => return api_json::error_response(&error, debug),
    };
    match adapter.test_connection().await {
        Ok(result) => {
            if let Some(mut stored) = connections_store().get_mut(&id) {
                stored.last_tested_at = Some(Utc::now());
                stored.last_status = Some(if result.reachable {
                    "ok".to_string()
                } else {
                    "failed".to_string()
                });
            }
            state.emit(
                EVENT_EMAIL_PROVIDER_CONNECTED,
                json!({
                    "connection_id": id.to_string(),
                    "kind": connection.kind.as_str(),
                    "reachable": result.reachable,
                    "latency_ms": result.latency_ms,
                }),
            );
            api_json::success_response(result, 200)
        }
        Err(error) => {
            if let Some(mut stored) = connections_store().get_mut(&id) {
                stored.last_tested_at = Some(Utc::now());
                stored.last_status = Some("failed".to_string());
            }
            api_json::error_response(&error, debug)
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SendInput {
    pub(crate) connection_id: Option<String>,
    pub(crate) to: String,
    pub(crate) from: Option<String>,
    pub(crate) from_name: Option<String>,
    pub(crate) subject: Option<String>,
    pub(crate) body_text: Option<String>,
    pub(crate) body_html: Option<String>,
    pub(crate) reply_to: Option<String>,
    pub(crate) template_id: Option<String>,
    pub(crate) variables: Option<HashMap<String, Value>>,
    pub(crate) metadata: Option<HashMap<String, String>>,
    pub(crate) test_mode: Option<bool>,
}

pub(crate) fn send_input_to_message(
    input: &SendInput,
    connection: &EmailConnection,
) -> UnifiedResult<EmailMessage> {
    validate_email_address(&input.to)?;
    let subject = input.subject.clone().unwrap_or_default();
    if subject.trim().is_empty() && input.template_id.is_none() {
        return Err(UnifiedError::bad_request(
            "Provide a subject or a template_id that renders one",
        ));
    }
    if input.body_text.is_none() && input.body_html.is_none() && input.template_id.is_none() {
        return Err(UnifiedError::bad_request(
            "Provide body_text or body_html (or a template_id)",
        ));
    }
    Ok(EmailMessage {
        to: input.to.clone(),
        from: input.from.clone(),
        from_name: input.from_name.clone(),
        subject,
        body_text: input.body_text.clone(),
        body_html: input.body_html.clone(),
        reply_to: input.reply_to.clone(),
        template_id: input.template_id.clone(),
        variables: input.variables.clone().unwrap_or_default(),
        metadata: input.metadata.clone().unwrap_or_default(),
        test_mode: input.test_mode.unwrap_or(connection.test_mode),
    })
}

pub(crate) async fn send_mail_handler(
    state: web::Data<Arc<EmailProviderState>>,
    req: HttpRequest,
    body: web::Json<SendInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&state, &req, PERM_EMAIL_PROVIDERS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    let connection = match resolve_connection(input.connection_id.as_deref()) {
        Ok(connection) => connection,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if !connection.enabled {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Email provider connection \"{}\" is disabled",
                connection.name
            )),
            debug,
        );
    }
    let mut message = match send_input_to_message(&input, &connection) {
        Ok(message) => message,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Some(template_id) = &message.template_id {
        match render_template(template_id, &message.variables) {
            Ok((html, subject)) => {
                if message.body_html.is_none() {
                    message.body_html = Some(html);
                }
                if message.subject.trim().is_empty() {
                    message.subject = subject;
                }
            }
            Err(error) => return api_json::error_response(&error, debug),
        }
    }
    let adapter = match email_provider_registry().build(&connection) {
        Ok(adapter) => adapter,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let job_id = Uuid::now_v7().to_string();
    append_log(SendLogEntry {
        id: job_id.clone(),
        connection_id: connection.id.to_string(),
        provider: connection.kind.as_str().to_string(),
        to: message.to.clone(),
        subject: message.subject.clone(),
        status: "queued".to_string(),
        provider_message_id: None,
        test_mode: message.test_mode,
        error: None,
        created_at: Utc::now().to_rfc3339(),
    });
    match adapter.send(&message).await {
        Ok(result) => {
            update_log(
                &job_id,
                if result.test_mode { "test" } else { "sent" },
                result.provider_message_id.clone(),
                None,
            );
            state.emit(
                EVENT_EMAIL_SENT,
                json!({
                    "job_id": job_id,
                    "to": message.to,
                    "connection_id": connection.id.to_string(),
                    "provider": connection.kind.as_str(),
                    "provider_message_id": result.provider_message_id,
                    "test_mode": result.test_mode,
                }),
            );
            api_json::success_response(
                json!({ "job_id": job_id, "connection_id": connection.id.to_string(), "result": result }),
                200,
            )
        }
        Err(error) => {
            update_log(&job_id, "failed", None, Some(error.message.clone()));
            state.emit(
                EVENT_EMAIL_FAILED,
                json!({
                    "job_id": job_id,
                    "to": message.to,
                    "connection_id": connection.id.to_string(),
                    "provider": connection.kind.as_str(),
                    "error": error.message,
                }),
            );
            api_json::error_response(&error, debug)
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SendBatchInput {
    pub(crate) connection_id: Option<String>,
    pub(crate) recipients: Vec<String>,
    pub(crate) from: Option<String>,
    pub(crate) from_name: Option<String>,
    pub(crate) subject: Option<String>,
    pub(crate) body_text: Option<String>,
    pub(crate) body_html: Option<String>,
    pub(crate) reply_to: Option<String>,
    pub(crate) template_id: Option<String>,
    pub(crate) variables: Option<HashMap<String, Value>>,
    pub(crate) metadata: Option<HashMap<String, String>>,
    pub(crate) test_mode: Option<bool>,
}

pub(crate) async fn send_batch_handler(
    state: web::Data<Arc<EmailProviderState>>,
    req: HttpRequest,
    body: web::Json<SendBatchInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&state, &req, PERM_EMAIL_PROVIDERS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    if input.recipients.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("recipients must not be empty"),
            debug,
        );
    }
    if input.recipients.len() > MAX_BATCH_RECIPIENTS {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "recipients must not exceed {MAX_BATCH_RECIPIENTS} per request"
            )),
            debug,
        );
    }
    let connection = match resolve_connection(input.connection_id.as_deref()) {
        Ok(connection) => connection,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if !connection.enabled {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Email provider connection \"{}\" is disabled",
                connection.name
            )),
            debug,
        );
    }
    let base = SendInput {
        connection_id: input.connection_id.clone(),
        to: input.recipients[0].clone(),
        from: input.from.clone(),
        from_name: input.from_name.clone(),
        subject: input.subject.clone(),
        body_text: input.body_text.clone(),
        body_html: input.body_html.clone(),
        reply_to: input.reply_to.clone(),
        template_id: input.template_id.clone(),
        variables: input.variables.clone(),
        metadata: input.metadata.clone(),
        test_mode: input.test_mode,
    };
    let mut message = match send_input_to_message(&base, &connection) {
        Ok(message) => message,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Some(template_id) = &message.template_id {
        match render_template(template_id, &message.variables) {
            Ok((html, subject)) => {
                if message.body_html.is_none() {
                    message.body_html = Some(html);
                }
                if message.subject.trim().is_empty() {
                    message.subject = subject;
                }
            }
            Err(error) => return api_json::error_response(&error, debug),
        }
    }
    let adapter = match email_provider_registry().build(&connection) {
        Ok(adapter) => adapter,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let mut results = Vec::with_capacity(input.recipients.len());
    let mut failures = 0u64;
    for recipient in &input.recipients {
        if let Err(error) = validate_email_address(recipient) {
            results.push(json!({ "to": recipient, "accepted": false, "error": error.message }));
            failures += 1;
            continue;
        }
        let mut single = message.clone();
        single.to = recipient.clone();
        match adapter.send(&single).await {
            Ok(result) => {
                append_log(SendLogEntry {
                    id: Uuid::now_v7().to_string(),
                    connection_id: connection.id.to_string(),
                    provider: connection.kind.as_str().to_string(),
                    to: recipient.clone(),
                    subject: single.subject.clone(),
                    status: if result.test_mode { "test" } else { "sent" }.to_string(),
                    provider_message_id: result.provider_message_id.clone(),
                    test_mode: result.test_mode,
                    error: None,
                    created_at: Utc::now().to_rfc3339(),
                });
                results.push(json!({ "to": recipient, "accepted": true, "result": result }));
            }
            Err(error) => {
                failures += 1;
                append_log(SendLogEntry {
                    id: Uuid::now_v7().to_string(),
                    connection_id: connection.id.to_string(),
                    provider: connection.kind.as_str().to_string(),
                    to: recipient.clone(),
                    subject: single.subject.clone(),
                    status: "failed".to_string(),
                    provider_message_id: None,
                    test_mode: single.test_mode,
                    error: Some(error.message.clone()),
                    created_at: Utc::now().to_rfc3339(),
                });
                results.push(json!({ "to": recipient, "accepted": false, "error": error.message }));
            }
        }
    }
    let sent = (input.recipients.len() as u64) - failures;
    api_json::success_response(
        json!({
            "connection_id": connection.id.to_string(),
            "provider": connection.kind.as_str(),
            "sent": sent,
            "failed": failures,
            "results": results,
        }),
        200,
    )
}

pub(crate) async fn list_send_log(
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, false),
    };
    let params = query.into_inner();
    let pagination = params.normalized();
    let mut entries = send_log_store()
        .read()
        .expect("send log lock poisoned")
        .clone();
    if let Some(status) = pagination.filters.get("status") {
        entries.retain(|entry| entry.status == *status);
    }
    if let Some(provider) = pagination.filters.get("provider") {
        entries.retain(|entry| entry.provider == *provider);
    }
    if let Some(connection_id) = pagination.filters.get("connection_id") {
        entries.retain(|entry| entry.connection_id == *connection_id);
    }
    if let Some(search) = &pagination.search {
        let needle = search.to_lowercase();
        entries.retain(|entry| {
            entry.to.to_lowercase().contains(&needle)
                || entry.subject.to_lowercase().contains(&needle)
        });
    }
    let sort = params.allowed_sort(&["created_at", "to", "subject", "status"], "created_at");
    match sort.as_str() {
        "to" => entries.sort_by(|a, b| a.to.cmp(&b.to)),
        "subject" => entries.sort_by(|a, b| a.subject.cmp(&b.subject)),
        "status" => entries.sort_by(|a, b| a.status.cmp(&b.status)),
        _ => entries.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if pagination.order == "desc" {
        entries.reverse();
    }
    let total = entries.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<SendLogEntry> = entries
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(
        PageResult::new(page_items, total, &pagination).to_json(),
        200,
    )
}

pub(crate) async fn get_default_connection(req: HttpRequest) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, false),
    };
    match default_connection() {
        Some(connection) => api_json::success_response(
            json!({ "connection_id": connection.id.to_string(), "connection": connection.view() }),
            200,
        ),
        None => {
            api_json::success_response(json!({ "connection_id": null, "connection": null }), 200)
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SetDefaultInput {
    pub(crate) connection_id: Option<String>,
}

pub(crate) async fn set_default_connection(
    state: web::Data<Arc<EmailProviderState>>,
    req: HttpRequest,
    body: web::Json<SetDefaultInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&state, &req, PERM_EMAIL_PROVIDERS_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    let Some(raw) = input.connection_id else {
        let mut default = default_connection_store()
            .write()
            .expect("default connection lock poisoned");
        *default = None;
        return api_json::success_response(json!({ "connection_id": null }), 200);
    };
    let id = match Uuid::parse_str(&raw) {
        Ok(id) => id,
        Err(_) => {
            return api_json::error_response(
                &UnifiedError::bad_request(format!("Invalid connection id \"{raw}\"")),
                debug,
            );
        }
    };
    match get_connection(&id) {
        Some(connection) => {
            {
                let mut default = default_connection_store()
                    .write()
                    .expect("default connection lock poisoned");
                *default = Some(id);
            }
            api_json::success_response(
                json!({ "connection_id": id.to_string(), "connection": connection.view() }),
                200,
            )
        }
        None => api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        ),
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
    use actix_web::test::TestRequest;
    use unified_core::hooks::HookBus;

    fn test_state() -> Arc<EmailProviderState> {
        let state = Arc::new(EmailProviderState::new());
        state.set_hooks(Arc::new(HookBus::new()));
        state
    }

    fn test_connection_input() -> CreateConnectionRequest {
        CreateConnectionRequest {
            name: "test-connection".to_string(),
            kind: "smtp".to_string(),
            domain: None,
            region: None,
            sender_email: None,
            sender_name: None,
            webhook_url: None,
            smtp_host: None,
            smtp_port: None,
            smtp_tls: None,
            smtp_username: None,
            api_key: None,
            access_key_id: None,
            secret_access_key: None,
            webhook_secret: None,
            password: None,
            client_id: None,
            client_secret: None,
            refresh_token: None,
            access_token: None,
            access_token_expires_at: None,
            test_mode: None,
            enabled: None,
        }
    }

    #[test]
    fn caller_rejects_requests_without_a_verified_caller() {
        let no_user = TestRequest::default().to_http_request();
        assert!(caller(&no_user).is_err());
        let no_role = TestRequest::default().to_http_request();
        assert!(caller(&no_role).is_err());
        let both = verified_request("user-1", "admin");
        let found = caller(&both).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        let state = test_state();
        let req = verified_request("user-1", "admin");
        assert!(!check_capability(
            &state,
            "admin",
            PERM_EMAIL_PROVIDERS_MANAGE
        ));
        assert!(require_permission(&state, &req, PERM_EMAIL_PROVIDERS_MANAGE).is_err());
    }

    #[tokio::test]
    async fn create_connection_is_gated_behind_the_manage_capability() {
        let state = test_state();
        let data = web::Data::new(state.clone());
        // No identity headers at all: refused with 401 before any work.
        let anonymous = TestRequest::default().to_http_request();
        let response =
            create_connection(data.clone(), anonymous, web::Json(test_connection_input())).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::UNAUTHORIZED);
        // Identity headers present, but no `role.check` handler answers:
        // fail closed with 403.
        let unauthorised = verified_request("user-1", "viewer");
        let response = create_connection(
            data.clone(),
            unauthorised,
            web::Json(test_connection_input()),
        )
        .await;
        assert_eq!(response.status(), actix_web::http::StatusCode::FORBIDDEN);
        // A `role.check` handler that allows the capability lets the
        // request through to the normal create flow.
        state
            .hooks
            .read()
            .expect("email hooks lock poisoned")
            .as_ref()
            .expect("test hooks")
            .register(
                "role.check",
                "test",
                Arc::new(|context: &mut unified_core::hooks::HookContext| {
                    context.set("allowed", serde_json::Value::Bool(true));
                }),
            );
        let allowed = verified_request("user-1", "admin");
        let response = create_connection(data, allowed, web::Json(test_connection_input())).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::CREATED);
    }
}
