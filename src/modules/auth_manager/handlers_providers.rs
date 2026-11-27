//! Auth Manager - HTTP handlers for providers and their settings.

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
use super::{providers::*, jwt::*, state::*, login::*, tokens::*, recovery::*, totp::*, google::*, module::*};

// Provider handlers
//
// Provider administration is admin-only: every mutating handler and
// the settings read require a caller whose token carries an admin
// capability (`*`, `admin:access`, or `users:manage`). Secret-typed
// settings are never returned: callers see the non-secret settings
// plus a `secrets_configured` map (`setting key -> configured?`)
// instead of the stored secret values.

/// Require an authenticated caller with an admin capability. Returns
/// the caller id and scopes, or the error response to return.
async fn require_admin_caller(
    state: &AuthManagerState,
    ctx: &AppContext,
    req: &HttpRequest,
) -> Result<(String, Vec<String>), HttpResponse> {
    let debug = ctx.is_debug();
    let (caller_id, scopes) = match caller_from_request(state, ctx, req).await {
        Ok(caller) => caller,
        Err(error) => return Err(api_json::error_response(&error, debug)),
    };
    if !caller_is_admin(&scopes) {
        return Err(api_json::error_response(
            &UnifiedError::forbidden("Provider administration requires an admin scope"),
            debug,
        ));
    }
    Ok((caller_id, scopes))
}

/// As `require_admin_caller`, plus a CSRF-token check when the
/// caller authenticated with the session cookie. Used by the
/// state-changing provider endpoints (enable/disable, settings
/// update); the settings read uses `require_admin_caller`.
async fn require_admin_caller_write(
    state: &AuthManagerState,
    ctx: &AppContext,
    req: &HttpRequest,
) -> Result<(String, Vec<String>), HttpResponse> {
    let debug = ctx.is_debug();
    let (caller_id, scopes) = match caller_from_request_write(state, ctx, req).await {
        Ok(caller) => caller,
        Err(error) => return Err(api_json::error_response(&error, debug)),
    };
    if !caller_is_admin(&scopes) {
        return Err(api_json::error_response(
            &UnifiedError::forbidden("Provider administration requires an admin scope"),
            debug,
        ));
    }
    Ok((caller_id, scopes))
}

/// True when `value` has the JSON type a settings field of
/// `field_type` accepts. `text` is a free-form string, exactly like
/// `string`; unknown field types accept any value so a future field
/// type is not rejected by an outdated check.
pub(crate) fn setting_value_matches_type(field_type: &str, value: &Value) -> bool {
    match field_type {
        "string" | "secret" | "text" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "select" => value.is_string(),
        _ => true,
    }
}

/// Split a provider's merged settings into the values safe to return
/// and a `secrets_configured` map. A secret-typed field (or a field
/// flagged `secret` in the schema) never appears in the first map:
/// the second map only records whether a non-empty value is stored.
pub(crate) fn sanitized_provider_settings(
    info: &ProviderInfo,
) -> (Map<String, Value>, Map<String, Value>) {
    let mut settings = Map::new();
    let mut secrets_configured = Map::new();
    for field in &info.settings_schema {
        let configured = info
            .settings
            .get(&field.key)
            .is_some_and(|value| !value.is_null() && value.as_str() != Some(""));
        if field.secret || field.field_type == "secret" {
            secrets_configured.insert(field.key.clone(), Value::Bool(configured));
        } else if let Some(value) = info.settings.get(&field.key) {
            settings.insert(field.key.clone(), value.clone());
        }
    }
    // Any stored key outside the schema is returned as-is only when
    // it is not secret-shaped; unknown keys are never assumed safe.
    for (key, value) in &info.settings {
        if !settings.contains_key(key)
            && !secrets_configured.contains_key(key)
            && !info.settings_schema.iter().any(|field| &field.key == key)
        {
            settings.insert(key.clone(), value.clone());
        }
    }
    (settings, secrets_configured)
}

/// A copy of `info` whose settings contain no secret values.
fn sanitized_provider_info(info: &ProviderInfo) -> ProviderInfo {
    let (settings, _) = sanitized_provider_settings(info);
    let mut info = info.clone();
    info.settings = settings;
    info
}

pub(crate) async fn list_providers(state: web::Data<AuthManagerState>) -> HttpResponse {
    let providers: Vec<ProviderInfo> = provider_definitions()
        .into_iter()
        .filter_map(|(id, _, _, _, _)| state.provider_info(&id))
        .map(|info| sanitized_provider_info(&info))
        .collect();
    api_json::success_response(json!({ "providers": providers }), 200)
}

pub(crate) async fn set_provider_enabled(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    if let Err(response) = require_admin_caller_write(&state, &ctx, &req).await {
        return response;
    }
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
    api_json::success_response(sanitized_provider_info(&info), 200)
}

pub(crate) async fn get_provider_settings(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
) -> HttpResponse {
    if let Err(response) = require_admin_caller(&state, &ctx, &req).await {
        return response;
    }
    let id = id.into_inner();
    match state.provider_info(&id) {
        Some(info) => {
            let (settings, secrets_configured) = sanitized_provider_settings(&info);
            api_json::success_response(
                json!({
                    "provider_id": info.id,
                    "enabled": info.enabled,
                    "settings": settings,
                    "secrets_configured": secrets_configured,
                    "settings_schema": info.settings_schema,
                }),
                200,
            )
        }
        None => api_json::error_response(
            &UnifiedError::not_found(format!("provider \"{id}\" not found")),
            ctx.is_debug(),
        ),
    }
}

pub(crate) async fn update_provider_settings(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<String>,
    input: web::Json<Map<String, Value>>,
) -> HttpResponse {
    if let Err(response) = require_admin_caller_write(&state, &ctx, &req).await {
        return response;
    }
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
        let type_ok = setting_value_matches_type(&field.field_type, value);
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
    );    ctx.hooks.emit(
        "auth.provider.settings.updated",
        // The event carries only which keys changed, never the values:
        // secret values must not fan out to hook listeners.
        json!({
            "provider_id": id,
            "changed_settings": input.keys().collect::<Vec<&String>>(),
        }),
    );
    let info = state
        .provider_info(&id)
        .expect("provider exists: it was loaded above");
    let (settings, secrets_configured) = sanitized_provider_settings(&info);
    api_json::success_response(
        json!({
            "provider_id": id,
            "enabled": state.is_enabled(&id),
            "settings": settings,
            "secrets_configured": secrets_configured,
        }),
        200,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn google_info_with_secret() -> ProviderInfo {
        let mut settings = Map::new();
        settings.insert("client_id".to_string(), json!("google-client-id"));
        settings.insert("client_secret".to_string(), json!("super-secret-value"));
        settings.insert("redirect_uri".to_string(), json!("https://example.com/callback"));
        ProviderInfo {
            id: "google".to_string(),
            name: "Google Sign-In".to_string(),
            kind: "social".to_string(),
            description: String::new(),
            enabled: true,
            settings_schema: provider_definitions()
                .into_iter()
                .find(|(id, _, _, _, _)| id == "google")
                .map(|(_, _, _, _, schema)| schema)
                .unwrap_or_default(),
            settings,
        }
    }

    #[test]
    fn sanitized_settings_never_return_secret_values() {
        let info = google_info_with_secret();
        let (settings, secrets_configured) = sanitized_provider_settings(&info);
        // Non-secret values pass through untouched.
        assert_eq!(settings.get("client_id"), Some(&json!("google-client-id")));
        assert_eq!(
            settings.get("redirect_uri"),
            Some(&json!("https://example.com/callback"))
        );
        // The secret value itself never appears anywhere in the output.
        let rendered = serde_json::to_string(&settings).expect("settings serialize");
        assert!(!rendered.contains("super-secret-value"));
        assert!(!settings.contains_key("client_secret"));
        // Instead the caller learns only whether a secret is configured.
        assert_eq!(
            secrets_configured.get("client_secret"),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn sanitized_settings_report_unconfigured_secrets_as_false() {
        let mut info = google_info_with_secret();
        info.settings.remove("client_secret");
        let (_, secrets_configured) = sanitized_provider_settings(&info);
        assert_eq!(
            secrets_configured.get("client_secret"),
            Some(&Value::Bool(false))
        );
    }

    #[test]
    fn setting_value_matches_type_cases() {
        // "text" is a free-form string, exactly like "string".
        assert!(setting_value_matches_type("text", &json!("hello")));
        assert!(!setting_value_matches_type("text", &json!(42)));
        assert!(!setting_value_matches_type("text", &json!(true)));
        assert!(!setting_value_matches_type("text", &json!(["a"])));
        assert!(setting_value_matches_type("string", &json!("hello")));
        assert!(!setting_value_matches_type("string", &json!(42)));
        assert!(setting_value_matches_type("secret", &json!("s3cr3t")));
        assert!(!setting_value_matches_type("secret", &json!(42)));
        assert!(setting_value_matches_type("boolean", &json!(true)));
        assert!(!setting_value_matches_type("boolean", &json!("true")));
        assert!(setting_value_matches_type("integer", &json!(3600)));
        assert!(!setting_value_matches_type("integer", &json!("3600")));
        assert!(setting_value_matches_type("select", &json!("oidc")));
        assert!(!setting_value_matches_type("select", &json!(1)));
    }
}
