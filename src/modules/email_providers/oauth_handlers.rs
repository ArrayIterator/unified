//! email_providers - OAuth handlers.
//!
//! The account-linking endpoints for Gmail, Yahoo and Outlook:
//! authorize (returns the provider authorization URL), callback
//! (the provider redirects the admin's browser here with the code),
//! refresh, and revoke. See `oauth.rs` for the flow itself.
//!
//! The callback is the one route in this module exempt from the
//! tenant gate and the capability check: the provider redirects the
//! admin's browser to it, and the single-use state token (10-minute
//! expiry, bound to one connection) is the authorization for that
//! call. Every other endpoint here requires the tenant gate and the
//! `email_providers:manage` capability like the rest of the module.

use super::{
    adapters::*, credentials::*, crypto::*, events::*, handlers::*, hooks::*, http::*, module::*,
    oauth::*, provider_kinds::*, registry::*, store::*, worker::*,
};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json as api_json;

/// Start an OAuth authorization: returns the provider authorization
/// URL the admin opens in a browser, plus the state token. The
/// pending flow lives 10 minutes; the provider redirects back to
/// `GET /oauth/callback` with the code and state.
///
/// Query: `redirect_uri` (required, https or http-on-localhost —
/// it must match the URI registered at the provider),
/// `scopes` (optional, comma-separated; defaults to the provider's
/// configured scopes).
pub(crate) async fn oauth_authorize(
    state: web::Data<Arc<EmailProviderState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<HashMap<String, String>>,
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
    if !connection.kind.is_oauth() {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Provider \"{}\" does not use OAuth2",
                connection.kind.as_str()
            )),
            debug,
        );
    }
    let client_id = match connection
        .credentials
        .client_id
        .clone()
        .filter(|value| !value.trim().is_empty())
    {
        Some(client_id) => client_id,
        None => {
            return api_json::error_response(
                &UnifiedError::bad_request(format!(
                    "Connection \"{}\" has no client_id configured",
                    connection.name
                )),
                debug,
            );
        }
    };
    let redirect_uri = query.get("redirect_uri").cloned().unwrap_or_default();
    if redirect_uri.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request(
                "redirect_uri query parameter is required (it must match the URI registered at the provider)",
            ),
            debug,
        );
    }
    let scopes: Option<Vec<String>> = query.get("scopes").map(|raw| {
        raw.split(',')
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect()
    });
    match authorization_url(
        connection.kind,
        connection.id,
        &client_id,
        &redirect_uri,
        scopes.as_deref(),
    ) {
        Ok((url, state_token)) => api_json::success_response(
            json!({
                "connection_id": connection.id.to_string(),
                "kind": connection.kind.as_str(),
                "authorization_url": url,
                "state": state_token,
                "expires_in": PENDING_FLOW_TTL_SECS,
            }),
            200,
        ),
        Err(error) => api_json::error_response(&error, debug),
    }
}

/// The OAuth callback: the provider redirects the admin's browser
/// here with `?code=...&state=...`. The state token resolves the
/// pending flow (single-use, 10 minutes); the code is exchanged for
/// tokens, which are encrypted and stored on the connection. No
/// token is ever returned in the response — only the linked account
/// address and the expiry.
pub(crate) async fn oauth_callback(
    state: web::Data<Arc<EmailProviderState>>,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = state.is_debug();
    if let Some(error) = query.get("error") {
        let detail = query
            .get("error_description")
            .map(|value| format!(": {value}"))
            .unwrap_or_default();
        return api_json::error_response(
            &UnifiedError::bad_request(format!("OAuth authorization failed: {error}{detail}")),
            debug,
        );
    }
    let code = query.get("code").cloned().unwrap_or_default();
    let state_token = query.get("state").cloned().unwrap_or_default();
    if code.trim().is_empty() || state_token.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("OAuth callback requires code and state query parameters"),
            debug,
        );
    }
    let Some(flow) = take_pending_flow(&state_token) else {
        return api_json::error_response(
            &UnifiedError::bad_request(
                "Unknown or expired OAuth state token; start the authorization again",
            ),
            debug,
        );
    };
    let Some(connection) = get_connection(&flow.connection_id) else {
        return api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        );
    };
    let result: UnifiedResult<(String, Option<String>, DateTime<Utc>)> = (|| async {
        let client_id = connection.credentials.client_id.clone().unwrap_or_default();
        let client_secret =
            decrypt_optional(&connection.credentials.client_secret)?.unwrap_or_default();
        let tokens = exchange_code(
            connection.kind,
            &client_id,
            &client_secret,
            &code,
            &flow.code_verifier,
            &flow.redirect_uri,
        )
        .await?;
        let encrypted_access = encrypt_secret(&tokens.access_token)?;
        let encrypted_refresh = match &tokens.refresh_token {
            Some(token) => Some(encrypt_secret(token)?),
            None => None,
        };
        Ok((encrypted_access, encrypted_refresh, tokens.expires_at()))
    })()
    .await;
    let (encrypted_access, encrypted_refresh, expires_at) = match result {
        Ok(values) => values,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Some(mut stored) = connections_store().get_mut(&connection.id) {
        stored.credentials.access_token = Some(encrypted_access);
        if let Some(refresh) = encrypted_refresh {
            stored.credentials.refresh_token = Some(refresh);
        }
        stored.credentials.access_token_expires_at = Some(expires_at);
        stored.updated_at = Utc::now();
    }
    state.emit(
        EVENT_EMAIL_PROVIDER_OAUTH_LINKED,
        json!({
            "connection_id": connection.id.to_string(),
            "kind": connection.kind.as_str(),
            "account": connection.sender_email,
            "expires_at": expires_at.to_rfc3339(),
        }),
    );
    api_json::success_response(
        json!({
            "linked": true,
            "connection_id": connection.id.to_string(),
            "kind": connection.kind.as_str(),
            "account": connection.sender_email,
            "expires_at": expires_at.to_rfc3339(),
        }),
        200,
    )
}

/// Refresh the stored OAuth tokens for a connection and persist the
/// rotated values (encrypted). Shared by the refresh endpoint; the
/// adapter has its own inline refresh for the send path.
pub(crate) async fn refresh_oauth_tokens(
    connection: &EmailConnection,
) -> UnifiedResult<TokenResponse> {
    let refresh_token = decrypt_optional(&connection.credentials.refresh_token)?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            UnifiedError::unprocessable(format!(
                "Connection \"{}\" is not linked to a {} account yet; link it from the OAuth authorize endpoint",
                connection.name,
                connection.kind.as_str()
            ))
        })?;
    let client_id = connection.credentials.client_id.clone().unwrap_or_default();
    let client_secret =
        decrypt_optional(&connection.credentials.client_secret)?.unwrap_or_default();
    let tokens =
        refresh_access_token(connection.kind, &client_id, &client_secret, &refresh_token).await?;
    let encrypted_access = encrypt_secret(&tokens.access_token)?;
    let encrypted_refresh = match &tokens.refresh_token {
        Some(token) => Some(encrypt_secret(token)?),
        None => None,
    };
    let expires_at = tokens.expires_at();
    if let Some(mut stored) = connections_store().get_mut(&connection.id) {
        stored.credentials.access_token = Some(encrypted_access);
        if let Some(refresh) = encrypted_refresh {
            stored.credentials.refresh_token = Some(refresh);
        }
        stored.credentials.access_token_expires_at = Some(expires_at);
        stored.updated_at = Utc::now();
    }
    emit_event(
        EVENT_EMAIL_PROVIDER_OAUTH_REFRESHED,
        json!({
            "connection_id": connection.id.to_string(),
            "kind": connection.kind.as_str(),
            "expires_at": expires_at.to_rfc3339(),
        }),
    );
    Ok(tokens)
}

/// Manually refresh a connection's OAuth tokens (also happens
/// automatically on first use when the access token is expired).
pub(crate) async fn oauth_refresh(
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
    if !connection.kind.is_oauth() {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Provider \"{}\" does not use OAuth2",
                connection.kind.as_str()
            )),
            debug,
        );
    }
    match refresh_oauth_tokens(&connection).await {
        Ok(tokens) => api_json::success_response(
            json!({
                "refreshed": true,
                "connection_id": connection.id.to_string(),
                "kind": connection.kind.as_str(),
                "expires_at": tokens.expires_at().to_rfc3339(),
            }),
            200,
        ),
        Err(error) => api_json::error_response(&error, debug),
    }
}

/// Unlink an OAuth account: revoke the tokens server-side where the
/// provider supports it (Google, Yahoo) and clear the stored tokens
/// either way. Microsoft has no token-revocation endpoint, so
/// clearing the stored tokens IS the revocation there
/// (`server_revoked: false` says so honestly).
pub(crate) async fn oauth_revoke(
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
    if !connection.kind.is_oauth() {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Provider \"{}\" does not use OAuth2",
                connection.kind.as_str()
            )),
            debug,
        );
    }
    let client_id = connection.credentials.client_id.clone().unwrap_or_default();
    // A token that cannot be decrypted (secret rotated or missing)
    // is already unusable: the unlink below still clears it, so a
    // decrypt failure here must not block the revoke.
    let client_secret = decrypt_optional(&connection.credentials.client_secret)
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut server_revoked = false;
    for token in [
        decrypt_optional(&connection.credentials.access_token)
            .ok()
            .flatten(),
        decrypt_optional(&connection.credentials.refresh_token)
            .ok()
            .flatten(),
    ] {
        let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        if revoke_token(connection.kind, &client_id, &client_secret, &token)
            .await
            .unwrap_or(false)
        {
            server_revoked = true;
        }
    }
    if let Some(mut stored) = connections_store().get_mut(&connection.id) {
        stored.credentials.access_token = None;
        stored.credentials.refresh_token = None;
        stored.credentials.access_token_expires_at = None;
        stored.updated_at = Utc::now();
    }
    api_json::success_response(
        json!({
            "unlinked": true,
            "server_revoked": server_revoked,
            "connection_id": connection.id.to_string(),
            "kind": connection.kind.as_str(),
        }),
        200,
    )
}
