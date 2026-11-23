//! email_providers - OAuth2 for Gmail, Yahoo and Outlook.
//!
//! The three account providers send through their SMTP servers with
//! XOAUTH2 (`user=<email>`, `auth=Bearer <access_token>`), the same
//! mechanism lettre already implements (`Mechanism::Xoauth2`), so no
//! provider SDK or Gmail API dependency is needed. The auth manager's
//! Google OAuth client (`auth_manager/google.rs`) was NOT reused: it
//! is a sign-in client bound to the auth manager's own accounts and
//! redirect URIs, while these flows belong to an email connection
//! (client id/secret on the connection, redirect back to this
//! module's callback). The patterns are the same — PKCE (S256),
//! state token, 600s pending-flow TTL, 60s refresh skew —
//! re-implemented here in the email module's own terms.
//!
//! Flow: `GET /connections/{id}/oauth/authorize` returns the provider
//! authorization URL and stores a one-time pending flow keyed by the
//! state token (10 minutes). The provider redirects the admin's
//! browser to `GET /oauth/callback?code=...&state=...`; the callback
//! exchanges the code for tokens, encrypts them (see `crypto.rs`),
//! and stores them on the connection. The adapter refreshes an
//! expired access token from the stored refresh token on first use
//! and persists the rotated tokens. Revocation clears the stored
//! tokens; Google and Yahoo also get a server-side revoke call —
//! Microsoft has no token-revocation endpoint, so clearing the
//! stored tokens IS the revocation there.
//!
//! Security: the token endpoint host is allow-listed per provider
//! (`check_allowlist` against the registry definition), the
//! redirect URI must be https or http-on-localhost, the state token
//! is single-use, and no token is ever logged or returned by any
//! API response. The callback handler is the one route in this
//! module exempt from the tenant gate: the provider redirects the
//! admin's browser there, and the single-use state token (plus the
//! 10-minute expiry) is the authorization for that call.

use super::{
    adapters::*, credentials::*, events::*, handlers::*, hooks::*, http::*, module::*,
    provider_kinds::*, registry::*, store::*, worker::*,
};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::OnceLock;
use unified_core::error::{UnifiedError, UnifiedResult};
use url::Url;
use uuid::Uuid;

/// Static OAuth2 configuration for one account provider: endpoints,
/// SMTP server, and the scopes the authorization URL requests.
/// All values are constants — no per-provider code path reads
/// anything from user input for the endpoint hosts.
#[derive(Debug, Clone, Copy)]
pub struct OAuthProviderConfig {
    pub kind: EmailProviderKind,
    pub auth_url: &'static str,
    pub token_url: &'static str,
    /// Server-side token revocation endpoint, when the provider has
    /// one (Google, Yahoo). `None` means revocation is local-only
    /// (Microsoft: clearing the stored tokens).
    pub revoke_url: Option<&'static str>,
    pub smtp_host: &'static str,
    pub smtp_port: u16,
    pub scopes: &'static [&'static str],
    /// Extra authorization-URL parameters (Google: offline access +
    /// consent so a refresh token is issued).
    pub extra_auth_params: &'static [(&'static str, &'static str)],
}

pub(crate) const GMAIL_CONFIG: OAuthProviderConfig = OAuthProviderConfig {
    kind: EmailProviderKind::Gmail,
    auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
    token_url: "https://oauth2.googleapis.com/token",
    revoke_url: Some("https://oauth2.googleapis.com/revoke"),
    smtp_host: "smtp.gmail.com",
    smtp_port: 587,
    scopes: &["https://mail.google.com/"],
    extra_auth_params: &[("access_type", "offline"), ("prompt", "consent")],
};

pub(crate) const YAHOO_CONFIG: OAuthProviderConfig = OAuthProviderConfig {
    kind: EmailProviderKind::Yahoo,
    auth_url: "https://api.login.yahoo.com/oauth2/request_auth",
    token_url: "https://api.login.yahoo.com/oauth2/get_token",
    revoke_url: Some("https://api.login.yahoo.com/oauth2/revoke"),
    smtp_host: "smtp.mail.yahoo.com",
    smtp_port: 587,
    scopes: &["mail-w"],
    extra_auth_params: &[],
};

pub(crate) const OUTLOOK_CONFIG: OAuthProviderConfig = OAuthProviderConfig {
    kind: EmailProviderKind::Outlook,
    auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
    token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
    revoke_url: None,
    smtp_host: "smtp.office365.com",
    smtp_port: 587,
    scopes: &["https://outlook.office365.com/SMTP.Send", "offline_access"],
    extra_auth_params: &[],
};

/// The OAuth config for a kind, or `None` when the kind is not an
/// OAuth provider (API providers, SMTP, sendmail).
pub(crate) fn config_for(kind: EmailProviderKind) -> Option<&'static OAuthProviderConfig> {
    match kind {
        EmailProviderKind::Gmail => Some(&GMAIL_CONFIG),
        EmailProviderKind::Yahoo => Some(&YAHOO_CONFIG),
        EmailProviderKind::Outlook => Some(&OUTLOOK_CONFIG),
        _ => None,
    }
}

/// True when the kind authenticates through OAuth2.
pub(crate) fn is_oauth_kind(kind: EmailProviderKind) -> bool {
    config_for(kind).is_some()
}

// ---------------------------------------------------------------------------
// PKCE and state tokens.
// ---------------------------------------------------------------------------

/// A PKCE code verifier: 43 URL-safe characters (RFC 7636).
pub(crate) fn new_code_verifier() -> String {
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The S256 code challenge for a verifier (SHA-256, base64url, no
/// padding). Verified against the RFC 7636 test vector in the tests.
pub(crate) fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// An opaque state token for one authorization flow (32 random
/// bytes, hex). Single-use, 10 minutes, bound to one connection.
pub(crate) fn new_state_token() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// How long a pending authorization flow lives (seconds).
pub(crate) const PENDING_FLOW_TTL_SECS: i64 = 600;

/// One pending authorization flow, keyed by its state token.
#[derive(Debug, Clone)]
pub struct PendingOAuthFlow {
    pub connection_id: Uuid,
    pub code_verifier: String,
    pub redirect_uri: String,
    pub created_at: DateTime<Utc>,
}

static PENDING_FLOWS: OnceLock<DashMap<String, PendingOAuthFlow>> = OnceLock::new();

fn pending_flows() -> &'static DashMap<String, PendingOAuthFlow> {
    PENDING_FLOWS.get_or_init(DashMap::new)
}

pub(crate) fn store_pending_flow(state: String, flow: PendingOAuthFlow) {
    pending_flows().insert(state, flow);
}

/// Take a pending flow exactly once. Returns `None` when the state
/// token is unknown, already used, or expired (10 minutes).
pub(crate) fn take_pending_flow(state: &str) -> Option<PendingOAuthFlow> {
    let (_, flow) = pending_flows().remove(state)?;
    let age = Utc::now()
        .signed_duration_since(flow.created_at)
        .num_seconds();
    if age > PENDING_FLOW_TTL_SECS {
        return None;
    }
    Some(flow)
}

// ---------------------------------------------------------------------------
// Redirect URI validation.
// ---------------------------------------------------------------------------

/// Validate the redirect URI the admin registered at the provider:
/// https anywhere, or http only on localhost (the local-admin
/// development case). Anything else — plain http to a public host,
/// or a non-http(s) scheme — is refused, so a stolen client id
/// cannot exfiltrate the authorization code to an attacker host.
pub(crate) fn validate_redirect_uri(raw: &str) -> UnifiedResult<Url> {
    let url = Url::parse(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid OAuth redirect URI \"{raw}\"")))?;
    match url.scheme() {
        "https" => Ok(url),
        "http" => {
            let host = url.host_str().unwrap_or("");
            if matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") {
                Ok(url)
            } else {
                Err(UnifiedError::bad_request(
                    "OAuth redirect URI must use https, or http on localhost",
                ))
            }
        }
        _ => Err(UnifiedError::bad_request(
            "OAuth redirect URI must use https, or http on localhost",
        )),
    }
}

// ---------------------------------------------------------------------------
// Authorization URL.
// ---------------------------------------------------------------------------

/// Build the provider authorization URL for a connection and store
/// the pending flow. Returns `(authorization_url, state)`: the admin
/// opens the URL in a browser and the provider redirects back to
/// this module's callback with the state token.
pub(crate) fn authorization_url(
    kind: EmailProviderKind,
    connection_id: Uuid,
    client_id: &str,
    redirect_uri: &str,
    scopes: Option<&[String]>,
) -> UnifiedResult<(String, String)> {
    let config = config_for(kind).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "Provider \"{}\" does not use OAuth2",
            kind.as_str()
        ))
    })?;
    let redirect = validate_redirect_uri(redirect_uri)?;
    let verifier = new_code_verifier();
    let state = new_state_token();
    let scope_list: Vec<String> = match scopes {
        Some(scopes) if !scopes.is_empty() => scopes.to_vec(),
        _ => config
            .scopes
            .iter()
            .map(|scope| scope.to_string())
            .collect(),
    };
    let mut url = Url::parse(config.auth_url)
        .map_err(|_| UnifiedError::internal("Invalid OAuth authorization URL"))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", client_id);
        query.append_pair("redirect_uri", redirect.as_str());
        query.append_pair("scope", &scope_list.join(" "));
        query.append_pair("state", &state);
        query.append_pair("code_challenge", &code_challenge(&verifier));
        query.append_pair("code_challenge_method", "S256");
        for (key, value) in config.extra_auth_params {
            query.append_pair(key, value);
        }
    }
    store_pending_flow(
        state.clone(),
        PendingOAuthFlow {
            connection_id,
            code_verifier: verifier,
            redirect_uri: redirect.to_string(),
            created_at: Utc::now(),
        },
    );
    Ok((url.to_string(), state))
}

// ---------------------------------------------------------------------------
// Token exchange, refresh, revocation.
// ---------------------------------------------------------------------------

/// A token endpoint response. `expires_in` is always returned by
/// the three providers; when it is missing the token is treated as
/// expiring in one hour rather than as never expiring.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl TokenResponse {
    /// When the access token expires. Missing `expires_in` defaults
    /// to one hour, the shortest of the three providers' lifetimes.
    pub fn expires_at(&self) -> DateTime<Utc> {
        Utc::now() + chrono::Duration::seconds(self.expires_in.unwrap_or(3600) as i64)
    }
}

fn token_url_for(kind: EmailProviderKind) -> UnifiedResult<(&'static OAuthProviderConfig, Url)> {
    let config = config_for(kind).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "Provider \"{}\" does not use OAuth2",
            kind.as_str()
        ))
    })?;
    let url = Url::parse(config.token_url)
        .map_err(|_| UnifiedError::internal("Invalid OAuth token URL"))?;
    check_allowlist(&url, kind)?;
    Ok((config, url))
}

async fn post_token_form(
    kind: EmailProviderKind,
    form: &HashMap<&str, &str>,
) -> UnifiedResult<TokenResponse> {
    let (_, url) = token_url_for(kind)?;
    let client = build_client()?;
    let response =
        client.post(url).form(form).send().await.map_err(|error| {
            UnifiedError::new(format!("OAuth token request failed: {error}"), 502)
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        UnifiedError::new(format!("OAuth token response unreadable: {error}"), 502)
    })?;
    if !status.is_success() {
        return Err(UnifiedError::new(
            format!(
                "OAuth token request refused (HTTP {}): {}",
                status.as_u16(),
                body.chars().take(300).collect::<String>()
            ),
            502,
        ));
    }
    serde_json::from_str(&body).map_err(|error| {
        UnifiedError::new(
            format!("OAuth token response is not valid JSON: {error}"),
            502,
        )
    })
}

/// Exchange an authorization code for tokens. The code verifier
/// comes from the pending flow (PKCE), never from the callback
/// query — a stolen code alone cannot be exchanged.
pub(crate) async fn exchange_code(
    kind: EmailProviderKind,
    client_id: &str,
    client_secret: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> UnifiedResult<TokenResponse> {
    let mut form: HashMap<&str, &str> = HashMap::new();
    form.insert("grant_type", "authorization_code");
    form.insert("code", code);
    form.insert("code_verifier", code_verifier);
    form.insert("client_id", client_id);
    form.insert("client_secret", client_secret);
    form.insert("redirect_uri", redirect_uri);
    post_token_form(kind, &form).await
}

/// Refresh an access token from a stored refresh token. The
/// response may rotate the refresh token; callers must persist
/// `refresh_token` when it is present.
pub(crate) async fn refresh_access_token(
    kind: EmailProviderKind,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> UnifiedResult<TokenResponse> {
    let mut form: HashMap<&str, &str> = HashMap::new();
    form.insert("grant_type", "refresh_token");
    form.insert("refresh_token", refresh_token);
    form.insert("client_id", client_id);
    form.insert("client_secret", client_secret);
    post_token_form(kind, &form).await
}

/// Revoke a token server-side. Returns `Ok(true)` when the provider
/// confirmed the revocation, `Ok(false)` when the provider has no
/// revocation endpoint (Microsoft) or answered non-2xx — the caller
/// still clears the stored tokens either way. Only a transport
/// failure is an error.
pub(crate) async fn revoke_token(
    kind: EmailProviderKind,
    client_id: &str,
    client_secret: &str,
    token: &str,
) -> UnifiedResult<bool> {
    let Some(config) = config_for(kind) else {
        return Err(UnifiedError::bad_request(format!(
            "Provider \"{}\" does not use OAuth2",
            kind.as_str()
        )));
    };
    let Some(revoke_url) = config.revoke_url else {
        return Ok(false);
    };
    let url =
        Url::parse(revoke_url).map_err(|_| UnifiedError::internal("Invalid OAuth revoke URL"))?;
    check_allowlist(&url, kind)?;
    let client = build_client()?;
    let response = client
        .post(url)
        .form(&[
            ("token", token),
            ("token_type_hint", "refresh_token"),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ])
        .send()
        .await
        .map_err(|error| UnifiedError::new(format!("OAuth revoke request failed: {error}"), 502))?;
    Ok(response.status().is_success())
}

// ---------------------------------------------------------------------------
// Expiry.
// ---------------------------------------------------------------------------

/// Refresh skew: a token expiring within 60 seconds is treated as
/// expired, matching the auth manager's Google refresh skew.
pub(crate) const REFRESH_SKEW_SECS: i64 = 60;

/// True when an access token must be refreshed: unknown expiry
/// (never linked or never recorded) counts as expired, so the
/// adapter never sends with a token of unknown age.
pub(crate) fn access_token_expired(expires_at: Option<DateTime<Utc>>) -> bool {
    match expires_at {
        None => true,
        Some(expires_at) => Utc::now() + chrono::Duration::seconds(REFRESH_SKEW_SECS) >= expires_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_the_rfc_7636_vector() {
        // RFC 7636, Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            code_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn code_verifier_is_43_url_safe_chars() {
        let verifier = new_code_verifier();
        assert_eq!(verifier.len(), 43);
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(verifier, new_code_verifier());
    }

    #[test]
    fn state_tokens_are_unique_hex() {
        let first = new_state_token();
        let second = new_state_token();
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn redirect_uri_validation() {
        assert!(validate_redirect_uri("https://unified.example/oauth/callback").is_ok());
        assert!(validate_redirect_uri("http://localhost:8080/oauth/callback").is_ok());
        assert!(validate_redirect_uri("http://127.0.0.1:8080/oauth/callback").is_ok());
        assert!(validate_redirect_uri("http://unified.example/oauth/callback").is_err());
        assert!(validate_redirect_uri("ftp://unified.example/oauth/callback").is_err());
        assert!(validate_redirect_uri("not a url").is_err());
    }

    #[test]
    fn authorization_url_carries_pkce_and_scopes() {
        let connection_id = Uuid::now_v7();
        let (url, state) = authorization_url(
            EmailProviderKind::Gmail,
            connection_id,
            "client-id-1",
            "https://unified.example/oauth/callback",
            None,
        )
        .unwrap();
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=client-id-1"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains(&format!("state={state}")));
        // The pending flow is stored and single-use.
        let flow = take_pending_flow(&state).expect("pending flow must exist");
        assert_eq!(flow.connection_id, connection_id);
        assert!(!flow.code_verifier.is_empty());
        assert!(take_pending_flow(&state).is_none());
    }

    #[test]
    fn authorization_url_rejects_non_oauth_kinds() {
        let err = authorization_url(
            EmailProviderKind::SendGrid,
            Uuid::now_v7(),
            "client-id-1",
            "https://unified.example/oauth/callback",
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not use OAuth2"));
    }

    #[test]
    fn access_token_expiry_uses_the_60s_skew() {
        assert!(access_token_expired(None));
        assert!(access_token_expired(Some(
            Utc::now() + chrono::Duration::seconds(30)
        )));
        assert!(!access_token_expired(Some(
            Utc::now() + chrono::Duration::seconds(3600)
        )));
        assert!(access_token_expired(Some(
            Utc::now() - chrono::Duration::seconds(10)
        )));
    }

    #[test]
    fn oauth_configs_cover_the_three_account_providers() {
        assert!(config_for(EmailProviderKind::Gmail).is_some());
        assert!(config_for(EmailProviderKind::Yahoo).is_some());
        assert!(config_for(EmailProviderKind::Outlook).is_some());
        assert!(config_for(EmailProviderKind::Sendmail).is_none());
        assert!(config_for(EmailProviderKind::Smtp).is_none());
        assert_eq!(GMAIL_CONFIG.smtp_port, 587);
        assert_eq!(OUTLOOK_CONFIG.revoke_url, None);
        assert!(YAHOO_CONFIG.revoke_url.is_some());
    }
}
