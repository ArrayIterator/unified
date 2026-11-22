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
// Real OAuth token exchange with external providers is a documented next
// milestone: the provider registry, configuration schema, and settings
// storage are real, but no outbound exchange is performed yet.

use crate::core::context::AppContext;
use crate::core::error::UnifiedError;
use crate::core::hooks::events;
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::Sha256;
use std::sync::Arc;
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
        "Authentication providers, per-provider settings, and JWT issue, verify, and revoke."
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
                .route("/jwt/revoke", web::post().to(revoke_token_handler)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AuthManagerModule::new()));
}
