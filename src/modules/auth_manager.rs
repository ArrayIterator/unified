// Auth Manager module.
//
// Authentication providers registry (OAuth server, OAuth login, SSO,
// Google, Facebook, biometric / WebAuthn, and JWT), per-provider
// enable/disable and namespaced settings, and a JWT manager that issues,
// verifies, and revokes HMAC-SHA256 signed JWT-like tokens.
//
// Provider settings are namespaced per provider and merged into the
// Settings module namespace through the `settings.merge` hook, which the
// Settings module can listen to.
//
// Google sign-in performs a real OAuth 2.0 authorization-code exchange
// with PKCE against Google's token endpoint (see the Google connector
// section below). Other providers keep the registry, configuration
// schema, and settings storage only; their outbound exchange is still
// a documented next milestone.

use crate::core::context::AppContext;
use crate::core::error::UnifiedError;
use crate::core::hooks::events;
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::Sha256;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Provider definitions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SettingField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub required: bool,
    pub secret: bool,
    pub default: Option<Value>,
    pub options: Option<Vec<String>>,
    pub description: String,
}

fn field(
    key: &str,
    label: &str,
    field_type: &str,
    required: bool,
    secret: bool,
    default: Option<Value>,
    options: Option<Vec<String>>,
    description: &str,
) -> SettingField {
    SettingField {
        key: key.to_string(),
        label: label.to_string(),
        field_type: field_type.to_string(),
        required,
        secret,
        default,
        options,
        description: description.to_string(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    pub enabled: bool,
    pub settings_schema: Vec<SettingField>,
    pub settings: Map<String, Value>,
}

/// Static provider catalogue with each provider's settings schema.
fn provider_definitions() -> Vec<(String, String, String, String, Vec<SettingField>)> {
    vec![
        (
            "oauth-server".to_string(),
            "OAuth 2.0 Server".to_string(),
            "oauth-server".to_string(),
            "Issue authorization codes and access tokens to third-party clients.".to_string(),
            vec![
                field(
                    "issuer",
                    "Issuer URL",
                    "string",
                    true,
                    false,
                    Some(json!("https://localhost:8443")),
                    None,
                    "Base URL used as the token issuer.",
                ),
                field(
                    "access_token_ttl_seconds",
                    "Access token lifetime (seconds)",
                    "integer",
                    false,
                    false,
                    Some(json!(3600)),
                    None,
                    "How long issued access tokens stay valid.",
                ),
                field(
                    "allow_client_credentials",
                    "Allow client credentials grant",
                    "boolean",
                    false,
                    false,
                    Some(json!(true)),
                    None,
                    "Enable the client credentials grant type.",
                ),
                field(
                    "allow_authorization_code",
                    "Allow authorization code grant",
                    "boolean",
                    false,
                    false,
                    Some(json!(true)),
                    None,
                    "Enable the authorization code grant type.",
                ),
            ],
        ),
        (
            "oauth-login".to_string(),
            "OAuth Login".to_string(),
            "oauth-login".to_string(),
            "Let users sign in through an external OAuth 2.0 identity provider.".to_string(),
            vec![
                field(
                    "provider_url",
                    "Provider URL",
                    "string",
                    true,
                    false,
                    None,
                    None,
                    "Base URL of the external identity provider.",
                ),
                field(
                    "client_id",
                    "Client ID",
                    "string",
                    true,
                    false,
                    None,
                    None,
                    "OAuth client id registered at the provider.",
                ),
                field(
                    "client_secret",
                    "Client secret",
                    "secret",
                    true,
                    true,
                    None,
                    None,
                    "OAuth client secret registered at the provider.",
                ),
                field(
                    "scopes",
                    "Scopes",
                    "string",
                    false,
                    false,
                    Some(json!("openid profile email")),
                    None,
                    "Space-separated scopes requested at login.",
                ),
            ],
        ),
        (
            "sso-provider".to_string(),
            "SSO Provider".to_string(),
            "sso-provider".to_string(),
            "Single sign-on through SAML 2.0 or OpenID Connect.".to_string(),
            vec![
                field(
                    "protocol",
                    "Protocol",
                    "select",
                    true,
                    false,
                    Some(json!("oidc")),
                    Some(vec!["oidc".to_string(), "saml".to_string()]),
                    "SSO protocol used by the identity provider.",
                ),
                field(
                    "issuer",
                    "Issuer",
                    "string",
                    true,
                    false,
                    None,
                    None,
                    "Issuer identifier of the identity provider.",
                ),
                field(
                    "metadata_url",
                    "Metadata URL",
                    "string",
                    true,
                    false,
                    None,
                    None,
                    "URL of the provider metadata document.",
                ),
                field(
                    "client_secret",
                    "Client secret",
                    "secret",
                    false,
                    true,
                    None,
                    None,
                    "Client secret for OpenID Connect flows.",
                ),
            ],
        ),
        (
            "google".to_string(),
            "Google Sign-In".to_string(),
            "social".to_string(),
            "Sign in with a Google account through OAuth 2.0.".to_string(),
            vec![
                field(
                    "client_id",
                    "Client ID",
                    "string",
                    true,
                    false,
                    None,
                    None,
                    "Google OAuth client id.",
                ),
                field(
                    "client_secret",
                    "Client secret",
                    "secret",
                    true,
                    true,
                    None,
                    None,
                    "Google OAuth client secret.",
                ),
                field(
                    "hosted_domain",
                    "Hosted domain",
                    "string",
                    false,
                    false,
                    None,
                    None,
                    "Optional Google Workspace domain to restrict sign-in to.",
                ),
                field(
                    "redirect_uri",
                    "Redirect URI",
                    "string",
                    false,
                    false,
                    None,
                    None,
                    "Redirect URI registered in Google Cloud Console; used as the default for the OAuth flow.",
                ),
                field(
                    "scopes",
                    "Scopes",
                    "string",
                    false,
                    false,
                    Some(json!("openid email profile")),
                    None,
                    "Space-separated scopes requested from Google (openid, email, profile, drive.file, drive.readonly).",
                ),
            ],
        ),
        (
            "facebook".to_string(),
            "Facebook Login".to_string(),
            "social".to_string(),
            "Sign in with a Facebook account through OAuth 2.0.".to_string(),
            vec![
                field(
                    "app_id",
                    "App ID",
                    "string",
                    true,
                    false,
                    None,
                    None,
                    "Facebook app id.",
                ),
                field(
                    "app_secret",
                    "App secret",
                    "secret",
                    true,
                    true,
                    None,
                    None,
                    "Facebook app secret.",
                ),
            ],
        ),
        (
            "biometric".to_string(),
            "Biometric / WebAuthn".to_string(),
            "biometric".to_string(),
            "Passkey and platform authenticator sign-in through WebAuthn.".to_string(),
            vec![
                field(
                    "relying_party_id",
                    "Relying party ID",
                    "string",
                    true,
                    false,
                    Some(json!("localhost")),
                    None,
                    "Domain the WebAuthn credentials are bound to.",
                ),
                field(
                    "relying_party_name",
                    "Relying party name",
                    "string",
                    false,
                    false,
                    Some(json!("Unified")),
                    None,
                    "Human-readable name shown to the user.",
                ),
                field(
                    "require_user_verification",
                    "Require user verification",
                    "boolean",
                    false,
                    false,
                    Some(json!(true)),
                    None,
                    "Require biometric or PIN verification on the authenticator.",
                ),
            ],
        ),
        (
            "jwt".to_string(),
            "JWT Tokens".to_string(),
            "jwt".to_string(),
            "HMAC-SHA256 signed JSON Web Tokens issued and verified by this server.".to_string(),
            vec![
                field(
                    "default_ttl_seconds",
                    "Default token lifetime (seconds)",
                    "integer",
                    false,
                    false,
                    Some(json!(3600)),
                    None,
                    "Default lifetime of issued tokens.",
                ),
                field(
                    "issuer",
                    "Issuer",
                    "string",
                    false,
                    false,
                    Some(json!("unified")),
                    None,
                    "Issuer claim written into issued tokens.",
                ),
            ],
        ),
    ]
}

fn default_enabled(provider_id: &str) -> bool {
    // Only the built-in JWT provider is enabled out of the box; external
    // providers need credentials before they can be switched on.
    provider_id == "jwt"
}

// ---------------------------------------------------------------------------
// JWT helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JwtHeader {
    alg: String,
    typ: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JwtClaims {
    sub: String,
    iss: String,
    iat: i64,
    exp: i64,
    jti: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    scopes: Vec<String>,
}

fn base64url_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn base64url_decode(data: &str) -> Result<Vec<u8>, UnifiedError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(data)
        .map_err(|_| UnifiedError::bad_request("token is not valid base64url"))
}

fn hmac_sign(key: &[u8], message: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AuthManagerState {
    pub enabled: Arc<DashMap<String, bool>>,
    pub settings: Arc<DashMap<String, Map<String, Value>>>,
    /// Revoked token ids (jti) with the time they were revoked.
    pub revoked_tokens: Arc<DashMap<String, DateTime<Utc>>>,
    /// Google OAuth tokens, keyed by Google account id (`sub`).
    /// Tokens are never serialized or returned by any endpoint.
    pub google_tokens: Arc<DashMap<String, GoogleTokenRecord>>,
    /// Pending Google OAuth authorization flows, keyed by `state`.
    pub google_oauth_states: Arc<DashMap<String, GoogleOAuthState>>,
    /// Google Drive storage targets linked through the connector.
    pub google_drive_links: Arc<DashMap<Uuid, GoogleDriveLink>>,
    /// Two-factor recovery codes, keyed by user id. Only SHA-256
    /// hashes are stored; the plaintext codes are shown exactly once,
    /// at generation time, and never again.
    pub recovery_codes: Arc<DashMap<String, Vec<RecoveryCodeRecord>>>,
    /// Fallback signing secret, used when no application secret is
    /// configured. Random per process, so tokens do not survive a restart
    /// unless the application secret key is set.
    pub secret: Arc<[u8; 32]>,
}

impl Default for AuthManagerState {
    fn default() -> Self {
        let enabled = DashMap::new();
        for (id, _, _, _, _) in provider_definitions() {
            enabled.insert(id.clone(), default_enabled(&id));
        }
        Self {
            enabled: Arc::new(enabled),
            settings: Arc::new(DashMap::new()),
            revoked_tokens: Arc::new(DashMap::new()),
            google_tokens: Arc::new(DashMap::new()),
            google_oauth_states: Arc::new(DashMap::new()),
            google_drive_links: Arc::new(DashMap::new()),
            recovery_codes: Arc::new(DashMap::new()),
            secret: Arc::new(rand::random::<[u8; 32]>()),
        }
    }
}

impl AuthManagerState {
    /// Signing key for JWTs: the configured application secret and salt
    /// when both are set, otherwise the per-process fallback secret.
    fn signing_key(&self, ctx: &AppContext) -> Vec<u8> {
        let config = ctx.config.read().expect("config lock poisoned");
        if !config.app.secret_key.is_empty() && !config.app.salt_key.is_empty() {
            format!("{}:{}", config.app.secret_key, config.app.salt_key).into_bytes()
        } else {
            self.secret.to_vec()
        }
    }

    fn is_enabled(&self, provider_id: &str) -> bool {
        self.enabled
            .get(provider_id)
            .map(|entry| *entry.value())
            .unwrap_or(false)
    }

    fn provider_info(&self, id: &str) -> Option<ProviderInfo> {
        provider_definitions()
            .into_iter()
            .find(|(provider_id, _, _, _, _)| provider_id == id)
            .map(|(id, name, kind, description, schema)| {
                let mut settings = Map::new();
                for field in &schema {
                    if let Some(default) = &field.default {
                        settings.insert(field.key.clone(), default.clone());
                    }
                }
                if let Some(stored) = self.settings.get(&id) {
                    for (key, value) in stored.value().iter() {
                        settings.insert(key.clone(), value.clone());
                    }
                }
                ProviderInfo {
                    enabled: self.is_enabled(&id),
                    settings_schema: schema,
                    settings,
                    id,
                    name,
                    kind,
                    description,
                }
            })
    }
}

// ---------------------------------------------------------------------------
// Provider handlers
// ---------------------------------------------------------------------------

async fn list_providers(state: web::Data<AuthManagerState>) -> HttpResponse {
    let providers: Vec<ProviderInfo> = provider_definitions()
        .into_iter()
        .filter_map(|(id, _, _, _, _)| state.provider_info(&id))
        .collect();
    api_json::success_response(json!({ "providers": providers }), 200)
}

async fn set_provider_enabled(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (id, action) = path.into_inner();
    let enable = match action.as_str() {
        "enable" => true,
        "disable" => false,
        _ => {
            return api_json::error_response(
                &UnifiedError::bad_request("action must be \"enable\" or \"disable\""),
                ctx.is_debug(),
            );
        }
    };
    let Some(mut info) = state.provider_info(&id) else {
        return api_json::error_response(
            &UnifiedError::not_found(format!("provider \"{id}\" not found")),
            ctx.is_debug(),
        );
    };
    if enable {
        // A provider may only be enabled once its required settings have
        // values; otherwise the toggle would promise a working login that
        // cannot complete.
        let missing: Vec<String> = info
            .settings_schema
            .iter()
            .filter(|field| field.required)
            .filter(|field| {
                !info
                    .settings
                    .get(&field.key)
                    .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
            })
            .map(|field| field.key.clone())
            .collect();
        if !missing.is_empty() {
            return api_json::error_response(
                &UnifiedError::unprocessable(format!(
                    "cannot enable provider \"{id}\": missing required settings: {}",
                    missing.join(", ")
                ))
                .with_metadata("missing_settings", json!(missing)),
                ctx.is_debug(),
            );
        }
    }
    state.enabled.insert(id.clone(), enable);
    info.enabled = enable;
    ctx.hooks.emit(
        if enable {
            "auth.provider.enabled"
        } else {
            "auth.provider.disabled"
        },
        json!({ "provider_id": id, "enabled": enable }),
    );
    api_json::success_response(info, 200)
}

async fn get_provider_settings(
    state: web::Data<AuthManagerState>,
    id: web::Path<String>,
) -> HttpResponse {
    let id = id.into_inner();
    match state.provider_info(&id) {
        Some(info) => api_json::success_response(
            json!({
                "provider_id": info.id,
                "enabled": info.enabled,
                "settings": info.settings,
                "settings_schema": info.settings_schema,
            }),
            200,
        ),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("provider \"{id}\" not found")),
            false,
        ),
    }
}

async fn update_provider_settings(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
    input: web::Json<Map<String, Value>>,
) -> HttpResponse {
    let id = id.into_inner();
    let input = input.into_inner();
    let Some(info) = state.provider_info(&id) else {
        return api_json::error_response(
            &UnifiedError::not_found(format!("provider \"{id}\" not found")),
            ctx.is_debug(),
        );
    };
    // Validate every submitted key against the provider schema.
    for (key, value) in &input {
        let Some(field) = info.settings_schema.iter().find(|field| &field.key == key) else {
            return api_json::error_response(
                &UnifiedError::bad_request(format!(
                    "unknown setting \"{key}\" for provider \"{id}\""
                )),
                ctx.is_debug(),
            );
        };
        let type_ok = match field.field_type.as_str() {
            "string" | "secret" => value.is_string(),
            "boolean" => value.is_boolean(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "select" => value.is_string(),
            _ => true,
        };
        if !type_ok {
            return api_json::error_response(
                &UnifiedError::bad_request(format!(
                    "setting \"{key}\" must be of type {}",
                    field.field_type
                )),
                ctx.is_debug(),
            );
        }
        if field.field_type == "select" {
            if let Some(options) = &field.options {
                if let Some(value) = value.as_str() {
                    if !options.iter().any(|option| option == value) {
                        return api_json::error_response(
                            &UnifiedError::bad_request(format!(
                                "setting \"{key}\" must be one of {}",
                                options.join(", ")
                            )),
                            ctx.is_debug(),
                        );
                    }
                }
            }
        }
    }
    let mut stored = state
        .settings
        .get(&id)
        .map(|entry| entry.value().clone())
        .unwrap_or_default();
    for (key, value) in &input {
        stored.insert(key.clone(), value.clone());
    }
    state.settings.insert(id.clone(), stored.clone());
    // Merge the namespaced settings into the Settings module namespace
    // through a hook, so the Settings module can persist them under
    // `auth.<provider-id>` without a direct module dependency.
    ctx.hooks.emit(
        "settings.merge",
        json!({
            "namespace": format!("auth.{id}"),
            "settings": stored,
        }),
    );
    ctx.hooks.emit(
        "auth.provider.settings.updated",
        json!({ "provider_id": id, "settings": stored }),
    );
    api_json::success_response(
        json!({
            "provider_id": id,
            "enabled": state.is_enabled(&id),
            "settings": stored,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// JWT handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct IssueTokenInput {
    pub user_id: String,
    pub expires_in_seconds: Option<u64>,
    pub scopes: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct TokenInput {
    pub token: String,
}

async fn issue_token(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<IssueTokenInput>,
) -> HttpResponse {
    let input = input.into_inner();
    if !state.is_enabled("jwt") {
        return api_json::error_response(
            &UnifiedError::forbidden("the JWT provider is disabled"),
            ctx.is_debug(),
        );
    }
    let user_id = input.user_id.trim();
    if user_id.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("user_id must not be empty"),
            ctx.is_debug(),
        );
    }
    if user_id.chars().count() > 256 {
        return api_json::error_response(
            &UnifiedError::bad_request("user_id must be at most 256 characters"),
            ctx.is_debug(),
        );
    }
    let ttl = input.expires_in_seconds.unwrap_or(3600).clamp(60, 86_400);
    let now = Utc::now();
    let issuer = state
        .provider_info("jwt")
        .and_then(|info| {
            info.settings
                .get("issuer")
                .and_then(|value| value.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| "unified".to_string());
    let claims = JwtClaims {
        sub: user_id.to_string(),
        iss: issuer,
        iat: now.timestamp(),
        exp: (now + chrono::Duration::seconds(ttl as i64)).timestamp(),
        jti: Uuid::now_v7().to_string(),
        scopes: input.scopes.unwrap_or_default(),
    };
    let header = JwtHeader {
        alg: "HS256".to_string(),
        typ: "JWT".to_string(),
    };
    let header_part = base64url_encode(
        serde_json::to_string(&header)
            .expect("JWT header serializes")
            .as_bytes(),
    );
    let payload_part = base64url_encode(
        serde_json::to_string(&claims)
            .expect("JWT claims serialize")
            .as_bytes(),
    );
    let signing_input = format!("{header_part}.{payload_part}");
    let signature = base64url_encode(&hmac_sign(&state.signing_key(&ctx), &signing_input));
    let token = format!("{signing_input}.{signature}");
    ctx.hooks.emit(
        events::AUTH_LOGIN,
        json!({
            "user_id": user_id,
            "provider": "jwt",
            "jti": claims.jti,
        }),
    );
    api_json::success_response(
        json!({
            "token": token,
            "token_type": "Bearer",
            "expires_in": ttl,
            "expires_at": claims.exp,
            "user_id": user_id,
        }),
        200,
    )
}

/// Decode and verify a token. Returns the claims on success, or a
/// `UnifiedError` describing why the token is unusable.
fn verify_token(
    state: &AuthManagerState,
    ctx: &AppContext,
    token: &str,
) -> Result<JwtClaims, UnifiedError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(UnifiedError::bad_request(
            "token must have three base64url segments",
        ));
    }
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let provided = base64url_decode(parts[2])?;
    let mut mac = HmacSha256::new_from_slice(&state.signing_key(ctx))
        .expect("HMAC accepts any key length");
    mac.update(signing_input.as_bytes());
    if mac.verify_slice(&provided).is_err() {
        return Err(UnifiedError::unauthorized("token signature is invalid"));
    }
    let payload_bytes = base64url_decode(parts[1])?;
    let claims: JwtClaims = serde_json::from_slice(&payload_bytes).map_err(|_| {
        UnifiedError::bad_request("token payload is not valid JSON")
    })?;
    if claims.exp <= Utc::now().timestamp() {
        return Err(UnifiedError::unauthorized("token has expired"));
    }
    if state.revoked_tokens.contains_key(&claims.jti) {
        return Err(UnifiedError::unauthorized("token has been revoked"));
    }
    Ok(claims)
}

async fn verify_token_handler(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<TokenInput>,
) -> HttpResponse {
    let token = input.token.trim();
    if token.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("token must not be empty"),
            ctx.is_debug(),
        );
    }
    match verify_token(&state, &ctx, token) {
        Ok(claims) => api_json::success_response(
            json!({
                "valid": true,
                "user_id": claims.sub,
                "issuer": claims.iss,
                "issued_at": claims.iat,
                "expires_at": claims.exp,
                "scopes": claims.scopes,
            }),
            200,
        ),
        Err(error) => {
            ctx.hooks.emit(
                events::AUTH_FAILED,
                json!({ "provider": "jwt", "reason": error.message }),
            );
            api_json::success_response(
                json!({ "valid": false, "reason": error.message }),
                200,
            )
        }
    }
}

async fn revoke_token_handler(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<TokenInput>,
) -> HttpResponse {
    let token = input.token.trim();
    if token.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("token must not be empty"),
            ctx.is_debug(),
        );
    }
    // Only a genuinely signed token can be revoked; a forged token must
    // not be able to poison the revocation list.
    let claims = match verify_token(&state, &ctx, token) {
        Ok(claims) => claims,
        Err(error) => {
            // An already-revoked token is still a signed token: report it
            // as already revoked instead of failing the request.
            if error.message == "token has been revoked" {
                return api_json::success_response(
                    json!({ "revoked": true, "already_revoked": true }),
                    200,
                );
            }
            return api_json::error_response(&error, ctx.is_debug());
        }
    };
    let already_revoked = state
        .revoked_tokens
        .insert(claims.jti.clone(), Utc::now())
        .is_some();
    ctx.hooks.emit(
        "auth.token.revoked",
        json!({ "user_id": claims.sub, "jti": claims.jti }),
    );
    ctx.hooks.emit(
        events::AUTH_LOGOUT,
        json!({ "user_id": claims.sub, "provider": "jwt" }),
    );
    api_json::success_response(
        json!({
            "revoked": true,
            "already_revoked": already_revoked,
            "user_id": claims.sub,
            "jti": claims.jti,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Two-factor recovery codes
//
// A set of 10 single-use codes per user, offered as a TOTP alternative
// at login when the authenticator device is unavailable. Only SHA-256
// hashes are stored; the plaintext codes are returned exactly once, in
// the generation response, and are never logged or returned again.
// A code is consumed (marked used) the first time it verifies
// successfully, so a captured code cannot be replayed. Regenerating a
// set invalidates every previously issued code for that user.
// ---------------------------------------------------------------------------

/// How many recovery codes one generated set contains.
pub const RECOVERY_CODE_COUNT: usize = 10;

/// One stored recovery code: hash only, never the plaintext.
#[derive(Debug, Clone)]
pub struct RecoveryCodeRecord {
    pub code_hash: String,
    pub used: bool,
    pub created_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
}

/// SHA-256 hex digest of a normalized recovery code.
fn recovery_code_hash(code: &str) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(code.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Normalize a submitted code: strip dashes and whitespace, uppercase.
fn normalize_recovery_code(code: &str) -> String {
    code.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_uppercase())
        .collect()
}

/// Constant-time equality for two hash strings, so verification does
/// not leak how many leading characters matched.
fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for index in 0..left.len() {
        diff |= left[index] ^ right[index];
    }
    diff == 0
}

/// Generate one random recovery code, formatted `XXXX-XXXX-XXXX`
/// from an unambiguous alphabet (no `0`/`O`, `1`/`I`, `l`).
fn generate_recovery_code() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let bytes = rand::random::<[u8; 12]>();
    let chars: Vec<char> = bytes
        .iter()
        .map(|byte| CHARSET[(*byte as usize) % CHARSET.len()] as char)
        .collect();
    let joined: String = chars.into_iter().collect();
    format!("{}-{}-{}", &joined[0..4], &joined[4..8], &joined[8..12])
}

fn validate_recovery_user_id(user_id: &str) -> UnifiedResult<String> {
    let user_id = user_id.trim();
    if user_id.is_empty() {
        return Err(UnifiedError::bad_request("user_id must not be empty"));
    }
    if user_id.chars().count() > 256 {
        return Err(UnifiedError::bad_request(
            "user_id must be at most 256 characters",
        ));
    }
    Ok(user_id.to_string())
}

fn remaining_recovery_codes(state: &AuthManagerState, user_id: &str) -> usize {
    state
        .recovery_codes
        .get(user_id)
        .map(|entry| entry.value().iter().filter(|record| !record.used).count())
        .unwrap_or(0)
}

#[derive(Debug, Deserialize)]
struct RecoveryCodeUserInput {
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
struct VerifyRecoveryCodeInput {
    pub user_id: String,
    pub code: String,
}

#[derive(Debug, Deserialize)]
struct RecoveryCodeUserQuery {
    pub user_id: String,
}

async fn generate_recovery_codes(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<RecoveryCodeUserInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let user_id = validate_recovery_user_id(&input.user_id)?;
        let previous = state
            .recovery_codes
            .get(&user_id)
            .map(|entry| entry.value().len())
            .unwrap_or(0);
        let now = Utc::now();
        let mut plaintext = Vec::with_capacity(RECOVERY_CODE_COUNT);
        let mut records = Vec::with_capacity(RECOVERY_CODE_COUNT);
        for _ in 0..RECOVERY_CODE_COUNT {
            let code = generate_recovery_code();
            plaintext.push(code.clone());
            records.push(RecoveryCodeRecord {
                code_hash: recovery_code_hash(&normalize_recovery_code(&code)),
                used: false,
                created_at: now,
                used_at: None,
            });
        }
        state.recovery_codes.insert(user_id.clone(), records);
        ctx.hooks.emit(
            "auth.recovery_code.generated",
            json!({ "user_id": user_id, "count": RECOVERY_CODE_COUNT }),
        );
        Ok(json!({
            "user_id": user_id,
            // Shown exactly once: these codes are never stored in
            // plaintext and cannot be listed again.
            "codes": plaintext,
            "count": RECOVERY_CODE_COUNT,
            "remaining": RECOVERY_CODE_COUNT,
            "invalidated_previous": previous,
            "warning": "Store these codes in a safe place. Each code can be used once and will never be shown again.",
        }))
    })();
    match result {
        Ok(data) => api_json::success_response(data, 201),
        Err(error) => api_json::error_response(&error, debug),
    }
}

async fn verify_recovery_code(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<VerifyRecoveryCodeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let user_id = validate_recovery_user_id(&input.user_id)?;
        let normalized = normalize_recovery_code(&input.code);
        if normalized.len() != 12 {
            return Err(UnifiedError::unauthorized(
                "Recovery code is invalid or has already been used",
            ));
        }
        let candidate_hash = recovery_code_hash(&normalized);
        let mut records = state
            .recovery_codes
            .get_mut(&user_id)
            .ok_or_else(|| {
                UnifiedError::unauthorized("Recovery code is invalid or has already been used")
            })?;
        let mut matched = false;
        for record in records.iter_mut() {
            if !record.used && constant_time_eq(&record.code_hash, &candidate_hash) {
                record.used = true;
                record.used_at = Some(Utc::now());
                matched = true;
                break;
            }
        }
        if !matched {
            return Err(UnifiedError::unauthorized(
                "Recovery code is invalid or has already been used",
            ));
        }
        let remaining = records.iter().filter(|record| !record.used).count();
        drop(records);
        ctx.hooks.emit(
            "auth.recovery_code.used",
            json!({ "user_id": user_id, "remaining": remaining }),
        );
        ctx.hooks.emit(
            events::AUTH_LOGIN,
            json!({ "user_id": user_id, "provider": "recovery-code" }),
        );
        Ok(json!({
            "valid": true,
            "user_id": user_id,
            "remaining": remaining,
        }))
    })();
    match result {
        Ok(data) => api_json::success_response(data, 200),
        Err(error) => {
            ctx.hooks.emit(
                events::AUTH_FAILED,
                json!({ "provider": "recovery-code", "reason": error.message }),
            );
            api_json::error_response(&error, debug)
        }
    }
}

async fn regenerate_recovery_codes(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<RecoveryCodeUserInput>,
) -> HttpResponse {
    // Regeneration is generation with invalidation: the new set
    // replaces the old one, so every previously issued code stops
    // working immediately.
    generate_recovery_codes(state, ctx, input).await
}

async fn remaining_recovery_codes_handler(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<RecoveryCodeUserQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let user_id = validate_recovery_user_id(&query.user_id)?;
        let total = state
            .recovery_codes
            .get(&user_id)
            .map(|entry| entry.value().len())
            .unwrap_or(0);
        Ok(json!({
            "user_id": user_id,
            "remaining": remaining_recovery_codes(&state, &user_id),
            "total": total,
            "has_codes": total > 0,
        }))
    })();
    api_json::respond(result, debug)
}

async fn revoke_recovery_codes(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<RecoveryCodeUserInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let user_id = validate_recovery_user_id(&input.user_id)?;
        let removed = state
            .recovery_codes
            .remove(&user_id)
            .map(|(_, records)| records.len())
            .unwrap_or(0);
        ctx.hooks.emit(
            "auth.recovery_code.revoked",
            json!({ "user_id": user_id, "revoked_count": removed }),
        );
        Ok(json!({
            "user_id": user_id,
            "revoked": true,
            "revoked_count": removed,
        }))
    })();
    api_json::respond(result, debug)
}

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

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
const GOOGLE_USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
/// How long a pending authorization flow (`state`) stays valid.
const GOOGLE_OAUTH_STATE_TTL_SECS: i64 = 600;
/// A token is treated as expired this many seconds before its real
/// expiry, so a refresh happens before any request can fail on it.
const GOOGLE_TOKEN_REFRESH_SKEW_SECS: i64 = 60;
const GOOGLE_HTTP_TIMEOUT_SECS: u64 = 30;
const GOOGLE_USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"));

/// Scopes the connector recognises by short name. Anything else must
/// be a full `https://www.googleapis.com/auth/...` scope URL.
const GOOGLE_KNOWN_SCOPES: &[&str] = &[
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
    fn view(&self) -> GoogleAccountView {
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
struct GoogleTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    token_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleUserInfo {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleAuthorizeQuery {
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    scopes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleCallbackInput {
    code: String,
    state: String,
    #[serde(default)]
    redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleAccountInput {
    account_id: String,
}

#[derive(Debug, Deserialize)]
struct GoogleDriveLinkInput {
    account_id: String,
    name: String,
    #[serde(default)]
    folder_id: Option<String>,
}

fn google_random_token() -> String {
    base64url_encode(&rand::random::<[u8; 32]>())
}

fn google_pkce_verifier() -> String {
    // Two 32-byte draws: `rand::random` for byte arrays is only
    // guaranteed up to 32 bytes, so build the 64-byte verifier from
    // two draws instead of one 64-byte draw.
    let mut bytes = [0u8; 64];
    bytes[..32].copy_from_slice(&rand::random::<[u8; 32]>());
    bytes[32..].copy_from_slice(&rand::random::<[u8; 32]>());
    base64url_encode(&bytes)
}

fn google_pkce_challenge(verifier: &str) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(verifier.as_bytes());
    base64url_encode(&hasher.finalize())
}

fn google_http_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(GOOGLE_USER_AGENT)
        .timeout(Duration::from_secs(GOOGLE_HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Validate a redirect URI: HTTPS, or plain HTTP only for localhost.
fn validate_google_redirect_uri(uri: &str) -> UnifiedResult<String> {
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
fn parse_google_scopes(raw: &str) -> UnifiedResult<Vec<String>> {
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

impl AuthManagerState {
    /// Google connector configuration from the `google` provider
    /// settings: (client_id, client_secret, redirect_uri, scopes,
    /// hosted_domain).
    fn google_config(&self) -> (String, String, String, Vec<String>, Option<String>) {
        let info = self.provider_info("google");
        let get = |key: &str| {
            info.as_ref()
                .and_then(|info| info.settings.get(key))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let scopes = parse_google_scopes(&get("scopes"))
            .unwrap_or_else(|_| vec!["openid".into(), "email".into(), "profile".into()]);
        let hosted_domain = get("hosted_domain");
        (
            get("client_id"),
            get("client_secret"),
            get("redirect_uri"),
            scopes,
            if hosted_domain.is_empty() {
                None
            } else {
                Some(hosted_domain)
            },
        )
    }

    fn google_configured(&self) -> bool {
        let (client_id, client_secret, _, _, _) = self.google_config();
        !client_id.is_empty() && !client_secret.is_empty()
    }
}

/// Exchange a form-encoded token request with Google and parse the
/// token response. Google error bodies (`error_description`) are
/// surfaced instead of the raw HTTP status text.
async fn google_token_request(
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
async fn google_userinfo(
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
async fn refresh_google_token(
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

fn paginate_google<T: Serialize + Clone>(items: Vec<T>, params: &crate::core::pagination::PaginationParams) -> Value {
    use crate::core::pagination::PageResult;
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

async fn google_status(state: web::Data<AuthManagerState>) -> HttpResponse {
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
            "redirect_uri": redirect_uri,
            "scopes_configured": scopes,
            "connected": !accounts.is_empty(),
            "accounts": accounts,
        }),
        200,
    )
}

/// SSO flow status: whether a Google sign-in can start right now.
async fn google_sso_status(state: web::Data<AuthManagerState>) -> HttpResponse {
    let (_, _, _, scopes, _) = state.google_config();
    let linked = state.google_tokens.len() as u64;
    api_json::success_response(
        json!({
            "provider": "google",
            "enabled": state.is_enabled("google"),
            "configured": state.google_configured(),
            "sso_ready": state.is_enabled("google") && state.google_configured(),
            "linked_accounts": linked,
            "scopes_configured": scopes,
            "authorize_url": "/api/v1/auth-manager/google/authorize",
        }),
        200,
    )
}

async fn google_authorize(
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
        let redirect_uri = match &query.redirect_uri {
            Some(uri) => validate_google_redirect_uri(uri)?,
            None => {
                if default_redirect.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "redirect_uri is required (query parameter or google provider setting)",
                    ));
                }
                validate_google_redirect_uri(&default_redirect)?
            }
        };
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

async fn google_callback(
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
        let (client_id, client_secret, _, _, _) = state.google_config();
        if client_id.is_empty() || client_secret.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Google client credentials are not configured",
            ));
        }
        let redirect_uri = match input.redirect_uri {
            Some(uri) => validate_google_redirect_uri(&uri)?,
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

async fn google_refresh(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<GoogleAccountInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = refresh_google_token(&state, &ctx, input.account_id.trim()).await;
    api_json::respond(result, debug)
}

async fn google_revoke(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<GoogleAccountInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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

async fn google_accounts(
    state: web::Data<AuthManagerState>,
    params: web::Query<crate::core::pagination::PaginationParams>,
) -> HttpResponse {
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

async fn google_drive_link(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<GoogleDriveLinkInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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

async fn google_drive_links(
    state: web::Data<AuthManagerState>,
    params: web::Query<crate::core::pagination::PaginationParams>,
) -> HttpResponse {
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

async fn google_drive_unlink(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
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

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AuthManagerModule {
    state: AuthManagerState,
}

impl AuthManagerModule {
    pub fn new() -> Self {
        Self {
            state: AuthManagerState::default(),
        }
    }
}

impl Default for AuthManagerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AuthManagerModule {
    fn id(&self) -> &'static str {
        "auth-manager"
    }

    fn name(&self) -> &'static str {
        "Auth Manager"
    }

    fn description(&self) -> &'static str {
        "Authentication providers, per-provider settings, JWT issue, verify, and revoke, and the unified Google connector (OAuth 2.0 with PKCE, SSO status, token refresh and revocation, Google Drive linking)."
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/providers", owner: "auth-manager", description: "List providers" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/providers/{id}/{action}", owner: "auth-manager", description: "Set provider enabled" },
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/providers/{id}/settings", owner: "auth-manager", description: "Get provider settings" },
            RouteInfo { method: "PUT", path: "/api/v1/auth-manager/providers/{id}/settings", owner: "auth-manager", description: "Update provider settings" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/jwt/issue", owner: "auth-manager", description: "Issue token" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/jwt/verify", owner: "auth-manager", description: "Verify token handler" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/jwt/revoke", owner: "auth-manager", description: "Revoke token handler" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/recovery-codes/generate", owner: "auth-manager", description: "Generate recovery codes" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/recovery-codes/verify", owner: "auth-manager", description: "Verify recovery code" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/recovery-codes/regenerate", owner: "auth-manager", description: "Regenerate recovery codes" },
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/recovery-codes/remaining", owner: "auth-manager", description: "Remaining recovery codes handler" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/recovery-codes/revoke", owner: "auth-manager", description: "Revoke recovery codes" },
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/google/status", owner: "auth-manager", description: "Google status" },
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/google/authorize", owner: "auth-manager", description: "Google authorize" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/google/callback", owner: "auth-manager", description: "Google callback" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/google/refresh", owner: "auth-manager", description: "Google refresh" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/google/revoke", owner: "auth-manager", description: "Google revoke" },
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/google/accounts", owner: "auth-manager", description: "Google accounts" },
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/google/sso/status", owner: "auth-manager", description: "SSO flow status: whether a Google sign-in can start right now." },
            RouteInfo { method: "GET", path: "/api/v1/auth-manager/google/drive/links", owner: "auth-manager", description: "Google drive links" },
            RouteInfo { method: "POST", path: "/api/v1/auth-manager/google/drive/links", owner: "auth-manager", description: "Google drive link" },
            RouteInfo { method: "DELETE", path: "/api/v1/auth-manager/google/drive/links/{id}", owner: "auth-manager", description: "Google drive unlink" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/auth-manager")
                .app_data(state)
                .route("/providers", web::get().to(list_providers))
                .route("/providers/{id}/{action}", web::post().to(set_provider_enabled))
                .route(
                    "/providers/{id}/settings",
                    web::get().to(get_provider_settings),
                )
                .route(
                    "/providers/{id}/settings",
                    web::put().to(update_provider_settings),
                )
                .route("/jwt/issue", web::post().to(issue_token))
                .route("/jwt/verify", web::post().to(verify_token_handler))
                .route("/jwt/revoke", web::post().to(revoke_token_handler))
                .route(
                    "/recovery-codes/generate",
                    web::post().to(generate_recovery_codes),
                )
                .route(
                    "/recovery-codes/verify",
                    web::post().to(verify_recovery_code),
                )
                .route(
                    "/recovery-codes/regenerate",
                    web::post().to(regenerate_recovery_codes),
                )
                .route(
                    "/recovery-codes/remaining",
                    web::get().to(remaining_recovery_codes_handler),
                )
                .route(
                    "/recovery-codes/revoke",
                    web::post().to(revoke_recovery_codes),
                )
                .route("/google/status", web::get().to(google_status))
                .route("/google/authorize", web::get().to(google_authorize))
                .route("/google/callback", web::post().to(google_callback))
                .route("/google/refresh", web::post().to(google_refresh))
                .route("/google/revoke", web::post().to(google_revoke))
                .route("/google/accounts", web::get().to(google_accounts))
                .route("/google/sso/status", web::get().to(google_sso_status))
                .route("/google/drive/links", web::get().to(google_drive_links))
                .route("/google/drive/links", web::post().to(google_drive_link))
                .route(
                    "/google/drive/links/{id}",
                    web::delete().to(google_drive_unlink),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AuthManagerModule::new()));
}
