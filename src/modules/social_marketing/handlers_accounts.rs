//! HTTP handlers for social accounts: connected accounts
//! on a network (handle, write-only encrypted credential,
//! token expiry, enabled flag). The credential is
//! write-only and encrypted; reads show only whether a
//! credential is configured and its fingerprint. The
//! metrics-ingest webhook secret is write-only too: the raw
//! secret is returned exactly once at connect / rotation,
//! only its hash is stored.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::crypto::{
    credential_fingerprint, decrypt_credential, encrypt_credential, encryption_secret,
    generate_webhook_secret, hash_webhook_secret,
};
use super::helpers::{
    caller, caller_company, created, err, load_account, load_network, ok, optional_text, paginate,
    parse_date_filter, parse_optional_timestamp, parse_timestamp, record_audit,
    require_caller_company, require_manage, require_tenant, require_text, require_view,
    validate_handle,
};
use super::state::SocialMarketingState;
use super::types::{
    ConnectAccountRequest, CreateAccountRequest, EVENT_ACCOUNT_CONNECTED,
    EVENT_ACCOUNT_DISCONNECTED, MAX_NAME_LEN, SocialAccount, UpdateAccountRequest,
};

/// List accounts (paginated, filter by network, connected,
/// enabled, token-expired; search by handle/display name;
/// filter by created date range with `from` / `to`). The
/// credential never appears in the list view.
pub(crate) async fn list_accounts(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let network_filter = params.filters.get("network_id").cloned();
        let connected_filter = params
            .filters
            .get("connected")
            .and_then(|raw| match raw.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        let enabled_filter = params
            .filters
            .get("enabled")
            .and_then(|raw| match raw.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        let expired_filter =
            params
                .filters
                .get("token_expired")
                .and_then(|raw| match raw.as_str() {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                });
        let from_filter = match params.filters.get("from") {
            Some(raw) => Some(parse_date_filter(raw, "from")?),
            None => None,
        };
        let to_filter = match params.filters.get("to") {
            Some(raw) => Some(parse_date_filter(raw, "to")?),
            None => None,
        };
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let now = Utc::now();
        let mut items = state.accounts_of(&tenant_id);
        items.retain(|account| {
            if let Some(network_id) = &network_filter
                && account.network_id.to_string() != *network_id
            {
                return false;
            }
            if let Some(connected) = connected_filter
                && account.connected != connected
            {
                return false;
            }
            if let Some(enabled) = enabled_filter
                && account.enabled != enabled
            {
                return false;
            }
            if let Some(expired) = expired_filter
                && super::types::token_expired(account.token_expires_at, now) != expired
            {
                return false;
            }
            if let Some(from) = from_filter
                && account.created_at < from
            {
                return false;
            }
            if let Some(to) = to_filter
                && account.created_at > to
            {
                return false;
            }
            if let Some(needle) = &search
                && !account.handle.to_lowercase().contains(needle)
                && !account
                    .display_name
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(needle)
            {
                return false;
            }
            true
        });
        let views: Vec<Value> = items.iter().map(|item| item.public_view()).collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Create an account row (manager). The account starts
/// disconnected: connecting is a separate step that stores
/// the credential. A webhook secret is generated at create
/// time and returned once, so metrics ingestion is
/// secret-gated from the start.
pub(crate) async fn create_account(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateAccountRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let network = load_network(&state, &tenant_id, &input.network_id)?;
        let handle = validate_handle(&input.handle)?;
        if state.accounts_of(&tenant_id).iter().any(|account| {
            account.network_id == network.id
                && account.handle.to_lowercase() == handle.to_lowercase()
        }) {
            return Err(UnifiedError::conflict(format!(
                "An account with handle \"{handle}\" already exists on this network"
            )));
        }
        let display_name = optional_text(&input.display_name, "display_name", MAX_NAME_LEN)?;
        let auth_username = optional_text(&input.auth_username, "auth_username", MAX_NAME_LEN)?;
        if network.auth_type == super::types::AuthType::Basic
            && auth_username.as_deref().unwrap_or("").is_empty()
        {
            return Err(UnifiedError::bad_request(
                "auth_username is required for basic auth".to_string(),
            ));
        }
        let token_expires_at =
            parse_optional_timestamp(&input.token_expires_at, "token_expires_at")?;
        let now = Utc::now();
        let mut account = SocialAccount {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            network_id: network.id,
            handle,
            display_name,
            auth_username,
            credential_encrypted: None,
            credential_fingerprint: None,
            webhook_secret_hash: None,
            token_expires_at,
            connected: false,
            enabled: input.enabled.unwrap_or(true),
            connected_at: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        // A credential supplied at create time is encrypted
        // at once, but the account still starts disconnected
        // until an explicit connect call confirms it.
        if let Some(credential) = input.credential.as_deref().filter(|key| !key.is_empty()) {
            if !network.auth_type.needs_credential() {
                return Err(UnifiedError::bad_request(
                    "This network auth type takes no credential".to_string(),
                ));
            }
            let secret = encryption_secret(&ctx)?;
            account.credential_encrypted = Some(encrypt_credential(&secret, credential));
            account.credential_fingerprint = Some(credential_fingerprint(credential));
        } else if network.auth_type.needs_credential() {
            // No credential yet: the account can be created,
            // but connect will refuse until one is supplied.
        }
        // The webhook secret is always set: supplied or fresh.
        // The raw value is returned exactly once, below.
        let webhook_secret = match input.webhook_secret.as_deref().filter(|s| !s.is_empty()) {
            Some(secret) => secret.to_string(),
            None => generate_webhook_secret(),
        };
        account.webhook_secret_hash = Some(hash_webhook_secret(&webhook_secret));
        let view = account.public_view();
        state.accounts.insert(account.id, account.clone());
        record_audit(
            &state,
            &tenant_id,
            "account",
            &account.id.to_string(),
            "create",
            &caller.actor,
            None,
            Some(view.clone()),
        );
        let mut response = view;
        if let Some(object) = response.as_object_mut() {
            object.insert("webhook_secret".to_string(), Value::String(webhook_secret));
        }
        Ok(response)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one account (masked view — the credential and the
/// webhook secret never appear).
pub(crate) async fn get_account(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let account = load_account(&state, &tenant_id, &path.into_inner())?;
        Ok(account.public_view())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update an account (manager). The credential is never
/// changed here — use connect to store a fresh one.
pub(crate) async fn update_account(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateAccountRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut account = load_account(&state, &tenant_id, &path.into_inner())?;
        let before = account.public_view();
        if let Some(handle) = &input.handle {
            let handle = validate_handle(handle)?;
            if handle.to_lowercase() != account.handle.to_lowercase()
                && state.accounts_of(&tenant_id).iter().any(|other| {
                    other.network_id == account.network_id
                        && other.handle.to_lowercase() == handle.to_lowercase()
                })
            {
                return Err(UnifiedError::conflict(format!(
                    "An account with handle \"{handle}\" already exists on this network"
                )));
            }
            account.handle = handle;
        }
        if let Some(display_name) = &input.display_name {
            account.display_name = match display_name {
                Some(text) => Some(require_text(text, "display_name", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(auth_username) = &input.auth_username {
            account.auth_username = match auth_username {
                Some(text) => Some(require_text(text, "auth_username", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(token_expires_at) = &input.token_expires_at {
            account.token_expires_at = match token_expires_at {
                Some(text) => Some(parse_timestamp(text, "token_expires_at")?),
                None => None,
            };
        }
        if let Some(enabled) = input.enabled {
            account.enabled = enabled;
        }
        account.updated_at = Utc::now();
        let view = account.public_view();
        state.accounts.insert(account.id, account.clone());
        record_audit(
            &state,
            &tenant_id,
            "account",
            &account.id.to_string(),
            "update",
            &caller.actor,
            Some(before),
            Some(view.clone()),
        );
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete an account (manager). Refused while any post
/// targets it; metric rows that name it are kept. The
/// stored credential and webhook secret die with the row.
pub(crate) async fn delete_account(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let account = load_account(&state, &tenant_id, &path.into_inner())?;
        let needle = account.id.to_string();
        let referenced = state.posts.iter().any(|entry| {
            entry.value().tenant_id == tenant_id && entry.value().account_ids.contains(&needle)
        });
        if referenced {
            return Err(UnifiedError::conflict(
                "Account is targeted by a post; delete or re-point the post first",
            ));
        }
        state.accounts.remove(&account.id);
        record_audit(
            &state,
            &tenant_id,
            "account",
            &account.id.to_string(),
            "delete",
            &caller.actor,
            Some(account.public_view()),
            None,
        );
        Ok(json!({ "deleted": true, "account_id": account.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Connect an account (manager): store the credential
/// write-only encrypted, mark the account connected, and
/// record the connect time. When no webhook secret is
/// stored yet (or a fresh one is supplied), the raw secret
/// is returned exactly once. A token expiry in the past is
/// refused: a connected account must hold a usable token.
pub(crate) async fn connect_account(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<ConnectAccountRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut account = load_account(&state, &tenant_id, &path.into_inner())?;
        let network = load_network(&state, &tenant_id, &account.network_id.to_string())?;
        if !network.enabled {
            return Err(UnifiedError::bad_request(format!(
                "Network \"{}\" is disabled",
                network.name
            )));
        }
        if input.credential.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "credential must not be empty".to_string(),
            ));
        }
        if !network.auth_type.needs_credential() {
            return Err(UnifiedError::bad_request(
                "This network auth type takes no credential".to_string(),
            ));
        }
        let token_expires_at =
            parse_optional_timestamp(&input.token_expires_at, "token_expires_at")?;
        let now = Utc::now();
        if super::types::token_expired(token_expires_at, now) {
            return Err(UnifiedError::bad_request(
                "token_expires_at is in the past; supply a usable token".to_string(),
            ));
        }
        let secret = encryption_secret(&ctx)?;
        account.credential_encrypted = Some(encrypt_credential(&secret, &input.credential));
        account.credential_fingerprint = Some(credential_fingerprint(&input.credential));
        if let Some(expires_at) = token_expires_at {
            account.token_expires_at = Some(expires_at);
        }
        account.connected = true;
        account.connected_at = Some(now);
        account.updated_at = now;
        // The webhook secret is set when missing, or
        // replaced when a fresh one is supplied. The raw
        // value is returned exactly once, below.
        let mut new_webhook_secret: Option<String> = None;
        match input.webhook_secret.as_deref().filter(|s| !s.is_empty()) {
            Some(secret) => {
                account.webhook_secret_hash = Some(hash_webhook_secret(secret));
                new_webhook_secret = Some(secret.to_string());
            }
            None => {
                if account.webhook_secret_hash.is_none() {
                    let fresh = generate_webhook_secret();
                    account.webhook_secret_hash = Some(hash_webhook_secret(&fresh));
                    new_webhook_secret = Some(fresh);
                }
            }
        }
        let view = account.public_view();
        state.accounts.insert(account.id, account.clone());
        record_audit(
            &state,
            &tenant_id,
            "account",
            &account.id.to_string(),
            "connect",
            &caller.actor,
            None,
            Some(view.clone()),
        );
        state.emit(
            EVENT_ACCOUNT_CONNECTED,
            json!({
                "tenant_id": tenant_id,
                "account_id": account.id.to_string(),
                "network_id": account.network_id.to_string(),
                "handle": account.handle,
                "connected_by": caller.actor,
            }),
        );
        let mut response = view;
        if let Some(secret) = new_webhook_secret
            && let Some(object) = response.as_object_mut()
        {
            object.insert("webhook_secret".to_string(), Value::String(secret));
        }
        Ok(response)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Disconnect an account (manager): mark it disconnected
/// and clear the stored credential, so a stale token can
/// never publish again. The webhook secret is kept: metrics
/// already reported stay verifiable, and a fresh connect
/// reuses it unless a new one is supplied.
pub(crate) async fn disconnect_account(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let mut account = load_account(&state, &tenant_id, &path.into_inner())?;
        let before = account.public_view();
        account.connected = false;
        account.credential_encrypted = None;
        account.credential_fingerprint = None;
        account.updated_at = Utc::now();
        let view = account.public_view();
        state.accounts.insert(account.id, account.clone());
        record_audit(
            &state,
            &tenant_id,
            "account",
            &account.id.to_string(),
            "disconnect",
            &caller.actor,
            Some(before),
            Some(view.clone()),
        );
        state.emit(
            EVENT_ACCOUNT_DISCONNECTED,
            json!({
                "tenant_id": tenant_id,
                "account_id": account.id.to_string(),
                "network_id": account.network_id.to_string(),
                "handle": account.handle,
                "disconnected_by": caller.actor,
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Rotate an account's metrics-ingest webhook secret
/// (manager): generates a fresh secret, stores only its
/// hash, and returns the raw secret exactly once. The old
/// secret stops working at once.
pub(crate) async fn rotate_account_webhook_secret(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let mut account = load_account(&state, &tenant_id, &path.into_inner())?;
        let secret = generate_webhook_secret();
        account.webhook_secret_hash = Some(hash_webhook_secret(&secret));
        account.updated_at = Utc::now();
        state.accounts.insert(account.id, account.clone());
        record_audit(
            &state,
            &tenant_id,
            "account",
            &account.id.to_string(),
            "rotate_webhook_secret",
            &caller.actor,
            None,
            Some(account.public_view()),
        );
        Ok(json!({
            "account_id": account.id.to_string(),
            "webhook_secret": secret,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Dry-run check of an account's configuration (manager):
/// validates the network, the connection, the token expiry,
/// and whether a credential is stored. This makes NO
/// network call and publishes NOTHING — a live probe would
/// publish a real post, so reachability is proven by the
/// first real publish, not here.
pub(crate) async fn test_account(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let account = load_account(&state, &tenant_id, &path.into_inner())?;
        let network = load_network(&state, &tenant_id, &account.network_id.to_string())?;
        let now = Utc::now();
        let mut problems: Vec<String> = Vec::new();
        if !network.enabled {
            problems.push("network is disabled".to_string());
        }
        if !account.enabled {
            problems.push("account is disabled".to_string());
        }
        if !account.connected {
            problems.push("account is not connected".to_string());
        }
        if super::types::token_expired(account.token_expires_at, now) {
            problems.push("stored token is expired".to_string());
        } else if super::types::token_expiring_within(account.token_expires_at, now, 7 * 24 * 3600)
        {
            problems.push("stored token expires within 7 days".to_string());
        }
        if network.kind == super::types::ProviderKind::Http {
            match &network.api_base_url {
                Some(url) if super::types::is_https_url(url) => {}
                _ => problems.push("api_base_url must be an HTTPS URL".to_string()),
            }
        }
        if network.auth_type.needs_credential() && account.credential_encrypted.is_none() {
            problems.push("auth type needs a credential; none is stored".to_string());
        }
        if network.auth_type == super::types::AuthType::Basic
            && account.auth_username.as_deref().unwrap_or("").is_empty()
        {
            problems.push("basic auth needs auth_username".to_string());
        }
        // A stored credential must still decrypt with the
        // current secret; a rotated app secret breaks old
        // rows, and that must surface here, not at publish
        // time.
        let credential_readable = match &account.credential_encrypted {
            Some(stored) => match encryption_secret(&ctx) {
                Ok(secret) => decrypt_credential(&secret, stored).is_ok(),
                Err(_) => false,
            },
            None => true,
        };
        if !credential_readable {
            problems.push(
                "stored credential cannot be decrypted with the current app secret".to_string(),
            );
        }
        Ok(json!({
            "account_id": account.id.to_string(),
            "network_id": network.id.to_string(),
            "handle": account.handle,
            "kind": network.kind.as_str(),
            "connected": account.connected,
            "key_configured": account.credential_encrypted.is_some(),
            "credential_readable": credential_readable,
            "token_expired": super::types::token_expired(account.token_expires_at, now),
            "token_expires_at": account.token_expires_at.map(|when| when.to_rfc3339()),
            "webhook_secret_configured": account.webhook_secret_hash.is_some(),
            "ready": problems.is_empty(),
            "problems": problems,
            "note": "Dry-run only: no network call was made and nothing was published.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
