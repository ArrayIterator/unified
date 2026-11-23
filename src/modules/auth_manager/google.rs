//! Auth Manager - Google sign-in (OAuth 2.0 with PKCE) and linked accounts.

use crate::auth::{password as auth_password, totp as auth_totp};
use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::hardening::{shared_state, trusted_client_ip_for_http_request};
use unified_core::hooks::events;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::db::models::User;
use unified_core::db::DbPool;
use actix_web::http::header;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::Sha256;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use super::{providers::*, jwt::*, state::*, handlers_providers::*, login::*, tokens::*, recovery::*, totp::*, module::*};
use unified_core::error::UnifiedResult;

// ---------------------------------------------------------------------------
// Google connector
//
// One unified Google connector built on the `google` provider above:
// social sign-in (OAuth 2.0 authorization code flow with PKCE), Google
// SSO status, token refresh and revocation, and Google Drive as a named
// backup/storage target. All outbound calls go to a fixed host
// allow-list (accounts.google.com, oauth2.googleapis.com,
// openidconnect.googleapis.com) over HTTPS only.
//
// Tokens live only in memory in `AuthManagerState`; they are never
// logged and never returned by any endpoint. Status endpoints return
// only account metadata (email, scopes, expiry). The Drive target
// itself is owned by the storage module: linking emits
// `google.drive.link` (and unlinking emits `google.drive.unlink`) so
// the storage module can register the target in its own registry; see
// the integration snippet in the coordinator report.
// ---------------------------------------------------------------------------

pub(crate) const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

pub(crate) const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub(crate) const GOOGLE_REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";

pub(crate) const GOOGLE_USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";

/// How long a pending authorization flow (`state`) stays valid.
pub(crate) const GOOGLE_OAUTH_STATE_TTL_SECS: i64 = 600;

/// A token is treated as expired this many seconds before its real
/// expiry, so a refresh happens before any request can fail on it.
pub(crate) const GOOGLE_TOKEN_REFRESH_SKEW_SECS: i64 = 60;

pub(crate) const GOOGLE_HTTP_TIMEOUT_SECS: u64 = 30;

pub(crate) const GOOGLE_USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"));

/// Scopes the connector recognises by short name. Anything else must
/// be a full `https://www.googleapis.com/auth/...` scope URL.
pub(crate) const GOOGLE_KNOWN_SCOPES: &[&str] = &[
    "openid",
    "email",
    "profile",
    "https://www.googleapis.com/auth/drive.file",
    "https://www.googleapis.com/auth/drive.readonly",
    "https://www.googleapis.com/auth/drive",
];

/// A linked Google account's tokens. Never serialized: only the
/// `GoogleAccountView` (metadata, no tokens) leaves this module.
#[derive(Debug, Clone)]
pub struct GoogleTokenRecord {
    pub account_id: String,
    pub email: String,
    pub name: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub scopes: Vec<String>,
    pub linked_at: DateTime<Utc>,
}

impl GoogleTokenRecord {
    pub(crate) fn view(&self) -> GoogleAccountView {
        GoogleAccountView {
            account_id: self.account_id.clone(),
            email: self.email.clone(),
            name: self.name.clone(),
            scopes: self.scopes.clone(),
            expires_at: self.expires_at,
            token_expired: self.expires_at
                <= Utc::now() + chrono::Duration::seconds(GOOGLE_TOKEN_REFRESH_SKEW_SECS),
            linked_at: self.linked_at,
        }
    }
}

/// Public view of a linked Google account: metadata only, no tokens.
#[derive(Debug, Clone, Serialize)]
pub struct GoogleAccountView {
    pub account_id: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub scopes: Vec<String>,
    pub expires_at: DateTime<Utc>,
    pub token_expired: bool,
    pub linked_at: DateTime<Utc>,
}

/// One pending OAuth authorization flow, keyed by `state`.
#[derive(Debug, Clone)]
pub struct GoogleOAuthState {
    pub state: String,
    pub code_verifier: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
}

/// A Google Drive folder linked as a named storage target. The
/// connector only records the link and announces it through the hook
/// bus; the storage module owns the actual Drive provider.
#[derive(Debug, Clone, Serialize)]
pub struct GoogleDriveLink {
    pub id: Uuid,
    pub name: String,
    pub account_id: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GoogleTokenResponse {
    pub(crate) access_token: String,
    #[serde(default)]
    pub(crate) refresh_token: Option<String>,
    #[serde(default)]
    pub(crate) expires_in: Option<i64>,
    #[serde(default)]
    pub(crate) scope: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) token_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GoogleUserInfo {
    pub(crate) sub: String,
    #[serde(default)]
    pub(crate) email: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GoogleAuthorizeQuery {
    #[serde(default)]
    pub(crate) redirect_uri: Option<String>,
    #[serde(default)]
    pub(crate) scopes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GoogleCallbackInput {
    pub(crate) code: String,
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GoogleAccountInput {
    pub(crate) account_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GoogleDriveLinkInput {
    pub(crate) account_id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) folder_id: Option<String>,
}

/// Resolve the redirect URI for a Google OAuth flow against the
/// configured redirect URI, which is the only accepted value. A
/// caller-supplied URI is accepted only when it exactly matches the
/// configured one (after URL normalization); anything else is
/// rejected, so an authorization code can never be delivered to an
/// attacker-controlled host. When no URI is configured, no
/// caller-supplied URI is accepted at all.
pub(crate) fn resolve_google_redirect_uri(
    configured: &str,
    requested: Option<&str>,
) -> UnifiedResult<String> {
    let allowed = if configured.trim().is_empty() {
        None
    } else {
        Some(validate_google_redirect_uri(configured)?)
    };
    match (requested, allowed) {
        (Some(uri), Some(allowed)) => {
            let normalized = validate_google_redirect_uri(uri)?;
            if normalized == allowed {
                Ok(normalized)
            } else {
                Err(UnifiedError::bad_request(
                    "redirect_uri does not match the configured Google redirect URI",
                ))
            }
        }
        (Some(_), None) => Err(UnifiedError::bad_request(
            "redirect_uri is not configured for the Google provider; a caller-supplied redirect URI is not accepted",
        )),
        (None, Some(allowed)) => Ok(allowed),
        (None, None) => Err(UnifiedError::bad_request(
            "redirect_uri is required (query parameter or google provider setting)",
        )),
    }
}

/// True when `email` belongs to `hosted_domain`. The check is
/// case-insensitive and requires the `@domain` suffix, so
/// `user@evil-example.com` never matches `example.com`. An empty
/// domain imposes no restriction.
pub(crate) fn email_matches_hosted_domain(email: &str, hosted_domain: &str) -> bool {
    let domain = hosted_domain
        .trim()
        .trim_start_matches('@')
        .to_lowercase();
    if domain.is_empty() {
        return true;
    }
    email
        .trim()
        .to_lowercase()
        .ends_with(&format!("@{domain}"))
}

/// Require an authenticated caller for Google account management.
/// Linked Google accounts carry no local-user linkage, so
/// per-account ownership cannot be checked here; authentication is
/// the available bar, and the endpoints never return tokens.
async fn require_google_caller(
    state: &AuthManagerState,
    ctx: &AppContext,
    req: &HttpRequest,
) -> Result<(String, Vec<String>), HttpResponse> {
    let debug = ctx.is_debug();
    caller_from_request(state, ctx, req)
        .await
        .map_err(|error| api_json::error_response(&error, debug))
}

/// As `require_google_caller`, plus a CSRF-token check when the
/// caller authenticated with the session cookie. Used by the
/// state-changing Google endpoints (refresh, revoke, drive link /
/// unlink); read-only endpoints use `require_google_caller`.
async fn require_google_caller_write(
    state: &AuthManagerState,
    ctx: &AppContext,
    req: &HttpRequest,
) -> Result<(String, Vec<String>), HttpResponse> {
    let debug = ctx.is_debug();
    caller_from_request_write(state, ctx, req)
        .await
        .map_err(|error| api_json::error_response(&error, debug))
}

pub(crate) fn google_random_token() -> String {
    base64url_encode(&rand::random::<[u8; 32]>())
}

pub(crate) fn google_pkce_verifier() -> String {
    // Two 32-byte draws: `rand::random` for byte arrays is only
    // guaranteed up to 32 bytes, so build the 64-byte verifier from
    // two draws instead of one 64-byte draw.
    let mut bytes = [0u8; 64];
    bytes[..32].copy_from_slice(&rand::random::<[u8; 32]>());
    bytes[32..].copy_from_slice(&rand::random::<[u8; 32]>());
    base64url_encode(&bytes)
}

pub(crate) fn google_pkce_challenge(verifier: &str) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(verifier.as_bytes());
    base64url_encode(&hasher.finalize())
}

pub(crate) fn google_http_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(GOOGLE_USER_AGENT)
        .timeout(Duration::from_secs(GOOGLE_HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Validate a redirect URI: HTTPS, or plain HTTP only for localhost.
pub(crate) fn validate_google_redirect_uri(uri: &str) -> UnifiedResult<String> {
    let parsed = url::Url::parse(uri.trim()).map_err(|_| {
        UnifiedError::bad_request(format!("Invalid redirect URI \"{uri}\""))
    })?;
    let host_ok = match parsed.scheme() {
        "https" => parsed.host_str().is_some(),
        "http" => matches!(parsed.host_str(), Some("localhost") | Some("127.0.0.1")),
        _ => false,
    };
    if !host_ok {
        return Err(UnifiedError::bad_request(
            "Redirect URI must use https, or http on localhost",
        ));
    }
    Ok(parsed.to_string())
}

/// Parse and validate a space-separated scope list.
pub(crate) fn parse_google_scopes(raw: &str) -> UnifiedResult<Vec<String>> {
    let scopes: Vec<String> = raw
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if scopes.is_empty() {
        return Err(UnifiedError::bad_request("At least one scope is required"));
    }
    for scope in &scopes {
        let known = GOOGLE_KNOWN_SCOPES.contains(&scope.as_str());
        let googleapis = scope.starts_with("https://www.googleapis.com/auth/")
            && scope.len() > "https://www.googleapis.com/auth/".len();
        if !known && !googleapis {
            return Err(UnifiedError::bad_request(format!(
                "Unsupported Google scope \"{scope}\""
            )));
        }
    }
    Ok(scopes)
}

/// Exchange a form-encoded token request with Google and parse the
/// token response. Google error bodies (`error_description`) are
/// surfaced instead of the raw HTTP status text.
pub(crate) async fn google_token_request(
    client: &reqwest::Client,
    form: &[(String, String)],
) -> UnifiedResult<GoogleTokenResponse> {
    let response = client
        .post(GOOGLE_TOKEN_URL)
        .form(form)
        .send()
        .await
        .map_err(|error| {
            UnifiedError::internal(format!("Google token request failed: {error}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        UnifiedError::internal(format!("Cannot read Google token response: {error}"))
    })?;
    if !status.is_success() {
        let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let detail = parsed
            .get("error_description")
            .or_else(|| parsed.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("Google token request failed");
        return Err(UnifiedError::new(
            format!("Google token request failed: {detail}"),
            status.as_u16(),
        ));
    }
    serde_json::from_str(&body).map_err(|error| {
        UnifiedError::internal(format!("Cannot parse Google token response: {error}"))
    })
}

/// Fetch the Google user profile for an access token.
pub(crate) async fn google_userinfo(
    client: &reqwest::Client,
    access_token: &str,
) -> UnifiedResult<GoogleUserInfo> {
    let response = client
        .get(GOOGLE_USERINFO_URL)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| {
            UnifiedError::internal(format!("Google userinfo request failed: {error}"))
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(UnifiedError::new(
            format!("Google userinfo request failed with HTTP {status}"),
            status.as_u16(),
        ));
    }
    response.json::<GoogleUserInfo>().await.map_err(|error| {
        UnifiedError::internal(format!("Cannot parse Google userinfo response: {error}"))
    })
}

/// Refresh one linked account's access token using its refresh token
/// and store the new token. Returns the updated account view.
pub(crate) async fn refresh_google_token(
    state: &AuthManagerState,
    ctx: &AppContext,
    account_id: &str,
) -> UnifiedResult<GoogleAccountView> {
    let record = state
        .google_tokens
        .get(account_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Google account not found"))?;
    let refresh_token = record.refresh_token.clone().ok_or_else(|| {
        UnifiedError::unprocessable(
            "Google account has no refresh token; sign in again to link it",
        )
    })?;
    let (client_id, client_secret, _, _, _) = state.google_config();
    if client_id.is_empty() || client_secret.is_empty() {
        return Err(UnifiedError::unprocessable(
            "Google client credentials are not configured",
        ));
    }
    let client = google_http_client()?;
    let form = vec![
        ("grant_type".to_string(), "refresh_token".to_string()),
        ("refresh_token".to_string(), refresh_token),
        ("client_id".to_string(), client_id),
        ("client_secret".to_string(), client_secret),
    ];
    let token = google_token_request(&client, &form).await?;
    let mut updated = record.clone();
    updated.access_token = token.access_token;
    updated.expires_at =
        Utc::now() + chrono::Duration::seconds(token.expires_in.unwrap_or(3600).max(60));
    if let Some(scope) = token.scope {
        updated.scopes = scope.split_whitespace().map(str::to_string).collect();
    }
    // Google only returns a new refresh token when the user re-consents;
    // keep the old one otherwise.
    if let Some(refresh) = token.refresh_token {
        updated.refresh_token = Some(refresh);
    }
    state.google_tokens.insert(account_id.to_string(), updated.clone());
    ctx.hooks.emit(
        "google.token.refreshed",
        json!({
            "account_id": account_id,
            "email": updated.email,
            "expires_at": updated.expires_at,
        }),
    );
    Ok(updated.view())
}

pub(crate) fn paginate_google<T: Serialize + Clone>(items: Vec<T>, params: &unified_core::pagination::PaginationParams) -> Value {
    use unified_core::pagination::PageResult;
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination).to_json()
}

// ---------------------------------------------------------------------------
// Google handlers
// ---------------------------------------------------------------------------

/// Google status: which accounts are linked. Authenticated callers
/// only: the account list contains user emails. The configured
/// redirect URI and scopes are never returned, only whether they are
/// configured, so an unauthenticated probe learns nothing about the
/// OAuth setup.
pub(crate) async fn google_status(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    if let Err(response) = require_google_caller(&state, &ctx, &req).await {
        return response;
    }
    let (client_id, _, redirect_uri, scopes, _) = state.google_config();
    let mut accounts: Vec<GoogleAccountView> = state
        .google_tokens
        .iter()
        .map(|entry| entry.value().view())
        .collect();
    accounts.sort_by(|a, b| a.email.cmp(&b.email));
    api_json::success_response(
        json!({
            "enabled": state.is_enabled("google"),
            "configured": state.google_configured(),
            "client_id_configured": !client_id.is_empty(),
            "redirect_uri_configured": !redirect_uri.is_empty(),
            "scopes_configured": !scopes.is_empty(),
            "connected": !accounts.is_empty(),
            "accounts": accounts,
        }),
        200,
    )
}

/// SSO flow status: whether a Google sign-in can start right now.
/// This stays reachable without a token (the login page needs it
/// before anyone has signed in), but it returns no configured
/// values: only whether scopes are configured, never the scope list
/// or the redirect URI.
pub(crate) async fn google_sso_status(state: web::Data<AuthManagerState>) -> HttpResponse {
    let (_, _, _, scopes, _) = state.google_config();
    let linked = state.google_tokens.len() as u64;
    api_json::success_response(
        json!({
            "provider": "google",
            "enabled": state.is_enabled("google"),
            "configured": state.google_configured(),
            "sso_ready": state.is_enabled("google") && state.google_configured(),
            "linked_accounts": linked,
            "scopes_configured": !scopes.is_empty(),
            "authorize_url": "/api/v1/auth-manager/google/authorize",
        }),
        200,
    )
}

pub(crate) async fn google_authorize(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<GoogleAuthorizeQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        if !state.is_enabled("google") {
            return Err(UnifiedError::forbidden("the Google provider is disabled"));
        }
        let (client_id, _, default_redirect, default_scopes, hosted_domain) =
            state.google_config();
        if client_id.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Google client_id is not configured",
            ));
        }
        let redirect_uri =
            resolve_google_redirect_uri(&default_redirect, query.redirect_uri.as_deref())?;
        let scopes = match &query.scopes {
            Some(raw) => parse_google_scopes(raw)?,
            None => default_scopes,
        };
        let oauth_state = google_random_token();
        let verifier = google_pkce_verifier();
        let challenge = google_pkce_challenge(&verifier);
        state.google_oauth_states.insert(
            oauth_state.clone(),
            GoogleOAuthState {
                state: oauth_state.clone(),
                code_verifier: verifier,
                redirect_uri: redirect_uri.clone(),
                scopes: scopes.clone(),
                created_at: Utc::now(),
            },
        );
        let mut url = url::Url::parse(GOOGLE_AUTH_URL)
            .expect("Google authorization URL is a constant");
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("client_id", &client_id);
            pairs.append_pair("redirect_uri", &redirect_uri);
            pairs.append_pair("response_type", "code");
            pairs.append_pair("scope", &scopes.join(" "));
            pairs.append_pair("state", &oauth_state);
            pairs.append_pair("code_challenge", &challenge);
            pairs.append_pair("code_challenge_method", "S256");
            pairs.append_pair("access_type", "offline");
            pairs.append_pair("prompt", "consent");
            if let Some(domain) = hosted_domain {
                pairs.append_pair("hd", &domain);
            }
        }
        Ok(json!({
            "authorization_url": url.to_string(),
            "state": oauth_state,
            "expires_in": GOOGLE_OAUTH_STATE_TTL_SECS,
        }))
    })();
    api_json::respond(result, debug)
}

pub(crate) async fn google_callback(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<GoogleCallbackInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GoogleAccountView> = (|| async {
        if !state.is_enabled("google") {
            return Err(UnifiedError::forbidden("the Google provider is disabled"));
        }
        let input = input.into_inner();
        // The state is single-use: remove it even when the flow fails,
        // so a captured state value cannot be replayed.
        let oauth = state
            .google_oauth_states
            .remove(&input.state)
            .map(|(_, value)| value)
            .ok_or_else(|| {
                UnifiedError::bad_request("Unknown or expired OAuth state")
            })?;
        if Utc::now()
            .signed_duration_since(oauth.created_at)
            .num_seconds()
            > GOOGLE_OAUTH_STATE_TTL_SECS
        {
            return Err(UnifiedError::bad_request(
                "OAuth state has expired; start the flow again",
            ));
        }
        if input.code.trim().is_empty() {
            return Err(UnifiedError::bad_request("code must not be empty"));
        }
        let (client_id, client_secret, _, _, hosted_domain) = state.google_config();
        if client_id.is_empty() || client_secret.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Google client credentials are not configured",
            ));
        }
        // The callback must complete the flow started by authorize:
        // a caller-supplied redirect URI is accepted only when it is
        // exactly the one recorded in the pending OAuth state.
        let redirect_uri = match input.redirect_uri {
            Some(uri) => {
                let normalized = validate_google_redirect_uri(&uri)?;
                if normalized != oauth.redirect_uri {
                    return Err(UnifiedError::bad_request(
                        "redirect_uri does not match the redirect URI of the started OAuth flow",
                    ));
                }
                normalized
            }
            None => oauth.redirect_uri.clone(),
        };
        let client = google_http_client()?;
        let form = vec![
            ("code".to_string(), input.code.trim().to_string()),
            ("client_id".to_string(), client_id),
            ("client_secret".to_string(), client_secret),
            ("redirect_uri".to_string(), redirect_uri),
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("code_verifier".to_string(), oauth.code_verifier.clone()),
        ];
        let token = google_token_request(&client, &form).await?;
        let userinfo = google_userinfo(&client, &token.access_token).await?;
        // `hd` is only a hint sent to Google: enforce the hosted
        // domain here, on the verified userinfo email, and reject any
        // account outside the configured domain.
        if let Some(domain) = &hosted_domain {
            let email = userinfo.email.clone().unwrap_or_default();
            if !email_matches_hosted_domain(&email, domain) {
                return Err(UnifiedError::forbidden(
                    "Google sign-in is restricted to the configured hosted domain",
                ));
            }
        }
        let scopes = token
            .scope
            .as_deref()
            .map(|scope| scope.split_whitespace().map(str::to_string).collect())
            .unwrap_or(oauth.scopes);
        let record = GoogleTokenRecord {
            account_id: userinfo.sub.clone(),
            email: userinfo.email.clone().unwrap_or_default(),
            name: userinfo.name.clone(),
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_at: Utc::now()
                + chrono::Duration::seconds(token.expires_in.unwrap_or(3600).max(60)),
            scopes: scopes.clone(),
            linked_at: Utc::now(),
        };
        state
            .google_tokens
            .insert(record.account_id.clone(), record.clone());
        ctx.hooks.emit(
            "google.sso.completed",
            json!({
                "account_id": record.account_id,
                "email": record.email,
                "name": record.name,
                "scopes": scopes,
            }),
        );
        ctx.hooks.emit(
            events::AUTH_LOGIN,
            json!({
                "user_id": record.account_id,
                "provider": "google",
                "email": record.email,
            }),
        );
        ctx.hooks.emit(
            "google.connected",
            json!({
                "account_id": record.account_id,
                "email": record.email,
            }),
        );
        Ok(record.view())
    })()
    .await;
    api_json::respond(result, debug)
}

pub(crate) async fn google_refresh(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<GoogleAccountInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(response) = require_google_caller_write(&state, &ctx, &req).await {
        return response;
    }
    let result = refresh_google_token(&state, &ctx, input.account_id.trim()).await;
    api_json::respond(result, debug)
}

pub(crate) async fn google_revoke(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<GoogleAccountInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(response) = require_google_caller_write(&state, &ctx, &req).await {
        return response;
    }
    let result: UnifiedResult<Value> = (|| async {
        let account_id = input.account_id.trim().to_string();
        let record = state
            .google_tokens
            .get(&account_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Google account not found"))?;
        // Best effort: tell Google to invalidate the token, but the
        // local record is removed either way so a failed remote call
        // cannot leave a token behind.
        let mut remote_revoked = false;
        if let Ok(client) = google_http_client() {
            let token = record
                .refresh_token
                .clone()
                .unwrap_or_else(|| record.access_token.clone());
            if let Ok(response) = client
                .post(GOOGLE_REVOKE_URL)
                .form(&[("token", token)])
                .send()
                .await
            {
                remote_revoked = response.status().is_success();
            }
        }
        state.google_tokens.remove(&account_id);
        // Drive links owned by this account stop working without the
        // token, so drop them and announce the unlink.
        let linked: Vec<Uuid> = state
            .google_drive_links
            .iter()
            .filter(|entry| entry.value().account_id == account_id)
            .map(|entry| *entry.key())
            .collect();
        for id in linked {
            state.google_drive_links.remove(&id);
        }
        ctx.hooks.emit(
            "google.token.revoked",
            json!({
                "account_id": account_id,
                "email": record.email,
                "remote_revoked": remote_revoked,
            }),
        );
        ctx.hooks.emit(
            events::AUTH_LOGOUT,
            json!({ "user_id": account_id, "provider": "google" }),
        );
        Ok(json!({
            "revoked": true,
            "remote_revoked": remote_revoked,
            "account_id": account_id,
        }))
    })()
    .await;
    api_json::respond(result, debug)
}

pub(crate) async fn google_accounts(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<unified_core::pagination::PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_google_caller(&state, &ctx, &req).await {
        return response;
    }
    let mut accounts: Vec<GoogleAccountView> = state
        .google_tokens
        .iter()
        .map(|entry| entry.value().view())
        .collect();
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        accounts.retain(|account| account.email.to_lowercase().contains(&needle));
    }
    accounts.sort_by(|a, b| a.email.cmp(&b.email));
    if params.normalized().order == "desc" {
        accounts.reverse();
    }
    api_json::success_response(paginate_google(accounts, &params), 200)
}

pub(crate) async fn google_drive_link(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<GoogleDriveLinkInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(response) = require_google_caller_write(&state, &ctx, &req).await {
        return response;
    }
    let result: UnifiedResult<GoogleDriveLink> = (|| async {
        let input = input.into_inner();
        let name = input.name.trim();
        if name.is_empty() || name.chars().count() > 128 {
            return Err(UnifiedError::bad_request(
                "name must be 1-128 characters",
            ));
        }
        if let Some(folder_id) = &input.folder_id {
            let folder_id = folder_id.trim();
            if folder_id.is_empty()
                || folder_id.len() > 128
                || !folder_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(UnifiedError::bad_request(
                    "folder_id must be a Google Drive folder id (letters, digits, '-' or '_')",
                ));
            }
        }
        let account = state
            .google_tokens
            .get(input.account_id.trim())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Google account not found"))?;
        // The Drive provider needs a working token: refresh it first
        // when it is (nearly) expired.
        if account.expires_at
            <= Utc::now() + chrono::Duration::seconds(GOOGLE_TOKEN_REFRESH_SKEW_SECS)
        {
            refresh_google_token(&state, &ctx, &account.account_id).await?;
        }
        let link = GoogleDriveLink {
            id: Uuid::now_v7(),
            name: name.to_string(),
            account_id: account.account_id.clone(),
            email: account.email.clone(),
            folder_id: input.folder_id.map(|value| value.trim().to_string()),
            status: "linked".to_string(),
            created_at: Utc::now(),
        };
        state.google_drive_links.insert(link.id, link.clone());
        // The storage module owns the Drive provider: it registers
        // this target in its registry from this event.
        ctx.hooks.emit(
            "google.drive.link",
            json!({
                "link_id": link.id,
                "name": link.name,
                "account_id": link.account_id,
                "email": link.email,
                "folder_id": link.folder_id,
                "provider": "google-drive",
            }),
        );
        Ok(link)
    })()
    .await;
    match result {
        Ok(link) => api_json::success_response(link, 201),
        Err(error) => api_json::error_response(&error, debug),
    }
}

pub(crate) async fn google_drive_links(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<unified_core::pagination::PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_google_caller(&state, &ctx, &req).await {
        return response;
    }
    let mut links: Vec<GoogleDriveLink> = state
        .google_drive_links
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        links.retain(|link| {
            link.name.to_lowercase().contains(&needle)
                || link.email.to_lowercase().contains(&needle)
        });
    }
    if let Some(account_id) = params.filters.get("account_id") {
        links.retain(|link| link.account_id == *account_id);
    }
    links.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        links.reverse();
    }
    api_json::success_response(paginate_google(links, &params), 200)
}

pub(crate) async fn google_drive_unlink(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(response) = require_google_caller_write(&state, &ctx, &req).await {
        return response;
    }
    let id = id.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let link = state
            .google_drive_links
            .remove(&id)
            .map(|(_, value)| value)
            .ok_or_else(|| UnifiedError::not_found("Google Drive link not found"))?;
        ctx.hooks.emit(
            "google.drive.unlink",
            json!({
                "link_id": link.id,
                "name": link.name,
                "account_id": link.account_id,
                "provider": "google-drive",
            }),
        );
        Ok(json!({ "unlinked": true, "id": id }))
    })();
    api_json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_pkce_verifier_shape() {
        let verifier = google_pkce_verifier();
        // 64 random bytes, base64url without padding: 86 characters.
        assert_eq!(verifier.len(), 86);
        assert!(verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(verifier, google_pkce_verifier());
    }

    #[test]
    fn google_pkce_challenge_is_sha256_base64url() {
        assert_eq!(
            google_pkce_challenge("test-verifier"),
            "JBbiqONGWPaAmwXk_8bT6UnlPfrn65D32eZlJS-zGG0"
        );
        assert_eq!(google_pkce_challenge("test-verifier").len(), 43);
        assert_ne!(
            google_pkce_challenge("test-verifier"),
            google_pkce_challenge("other-verifier")
        );
    }

    #[test]
    fn validate_google_redirect_uri_cases() {
        assert!(validate_google_redirect_uri("https://example.com/callback").is_ok());
        assert!(validate_google_redirect_uri("http://localhost:8080/callback").is_ok());
        assert!(validate_google_redirect_uri("http://127.0.0.1/callback").is_ok());
        assert!(validate_google_redirect_uri("http://example.com/callback").is_err());
        assert!(validate_google_redirect_uri("ftp://example.com/callback").is_err());
        assert!(validate_google_redirect_uri("not a url").is_err());
        assert!(validate_google_redirect_uri("https://").is_err());
    }

    #[test]
    fn resolve_google_redirect_uri_cases() {
        let configured = "https://example.com/callback";
        // Exact match (after normalization) is accepted.
        assert_eq!(
            resolve_google_redirect_uri(configured, Some("https://example.com/callback"))
                .expect("exact match"),
            "https://example.com/callback".to_string()
        );
        // No caller value falls back to the configured URI.
        assert_eq!(
            resolve_google_redirect_uri(configured, None).expect("configured fallback"),
            "https://example.com/callback".to_string()
        );
        // A different host, path, or scheme is rejected.
        assert!(resolve_google_redirect_uri(configured, Some("https://evil.com/callback"))
            .is_err());
        assert!(resolve_google_redirect_uri(
            configured,
            Some("https://example.com/other")
        )
        .is_err());
        assert!(resolve_google_redirect_uri(configured, Some("http://example.com/callback"))
            .is_err());
        // A caller-supplied URI with nothing configured is rejected.
        assert!(resolve_google_redirect_uri(
            "",
            Some("https://example.com/callback")
        )
        .is_err());
        // Nothing configured and nothing supplied is an error.
        assert!(resolve_google_redirect_uri("", None).is_err());
        // A malformed caller URI is rejected.
        assert!(resolve_google_redirect_uri(configured, Some("not a url")).is_err());
    }

    #[test]
    fn email_matches_hosted_domain_cases() {
        assert!(email_matches_hosted_domain("user@example.com", "example.com"));
        assert!(email_matches_hosted_domain("User@Example.COM", "example.com"));
        assert!(email_matches_hosted_domain("user@example.com", "@example.com"));
        assert!(!email_matches_hosted_domain(
            "user@evil-example.com",
            "example.com"
        ));
        assert!(!email_matches_hosted_domain(
            "user@example.com.evil.com",
            "example.com"
        ));
        assert!(!email_matches_hosted_domain("user@other.com", "example.com"));
        // No email at all never matches a configured domain.
        assert!(!email_matches_hosted_domain("", "example.com"));
        // No configured domain imposes no restriction.
        assert!(email_matches_hosted_domain("user@anywhere.com", ""));
    }

    #[test]
    fn parse_google_scopes_cases() {
        assert_eq!(
            parse_google_scopes("openid email").expect("known scopes"),
            vec!["openid".to_string(), "email".to_string()]
        );
        assert_eq!(
            parse_google_scopes("openid  email").expect("extra spaces"),
            vec!["openid".to_string(), "email".to_string()]
        );
        assert!(parse_google_scopes(
            "https://www.googleapis.com/auth/drive"
        )
        .is_ok());
        assert!(parse_google_scopes("").is_err());
        assert!(parse_google_scopes("   ").is_err());
        assert!(parse_google_scopes("bogus.scope").is_err());
        assert!(parse_google_scopes("openid bogus.scope").is_err());
    }
}
