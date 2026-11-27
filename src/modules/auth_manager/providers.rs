//! Auth Manager - Authentication provider catalogue and settings schema.

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
use super::{jwt::*, state::*, handlers_providers::*, login::*, tokens::*, recovery::*, totp::*, google::*, module::*};

pub(crate) type HmacSha256 = Hmac<Sha256>;

// Provider definitions

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

pub(crate) fn field(
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
pub(crate) fn provider_definitions() -> Vec<(String, String, String, String, Vec<SettingField>)> {
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

pub(crate) fn default_enabled(provider_id: &str) -> bool {
    // Only the built-in JWT provider is enabled out of the box; external
    // providers need credentials before they can be switched on.
    provider_id == "jwt"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_enabled_cases() {
        assert!(default_enabled("jwt"));
        assert!(!default_enabled("google"));
        assert!(!default_enabled("facebook"));
        assert!(!default_enabled(""));
    }

    #[test]
    fn provider_definitions_have_unique_ids() {
        let definitions = provider_definitions();
        assert!(!definitions.is_empty());
        let mut ids: Vec<&str> = definitions
            .iter()
            .map(|(id, _, _, _, _)| id.as_str())
            .collect();
        assert!(ids.iter().all(|id| !id.is_empty()));
        assert!(ids.contains(&"jwt"));
        assert!(ids.contains(&"google"));
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), definitions.len());
        for (id, name, _, _, schema) in &definitions {
            assert!(!name.is_empty(), "provider {id} has no name");
            assert!(!schema.is_empty(), "provider {id} has no settings schema");
        }
    }

    #[test]
    fn field_helper_builds_setting_field() {
        let field = field(
            "client_id",
            "Client ID",
            "text",
            true,
            false,
            None,
            None,
            "OAuth client id",
        );
        assert_eq!(field.key, "client_id");
        assert_eq!(field.label, "Client ID");
        assert_eq!(field.field_type, "text");
        assert!(field.required);
        assert!(!field.secret);
        assert_eq!(field.description, "OAuth client id");
    }
}
