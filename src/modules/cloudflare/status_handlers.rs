//! Cloudflare connector module. - Handlers: status, settings, connection test.
//!
//! Split from `src/modules/cloudflare.rs`; behaviour unchanged.

use super::accounts_zones::*;
use super::cache_handlers::*;
use super::client::*;
use super::constants::*;
use super::dns_handlers::*;
use super::dns_validation::*;
use super::helpers::*;
use super::module::*;
use super::requests::*;
use super::state::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use reqwest::Method;
use serde_json::{json, Value};
use std::sync::Arc;

pub(super) async fn get_status(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_CLOUDFLARE_VIEW) {
        return json::error_response(&error, debug);
    }
    let last_test = state
        .last_test
        .read()
        .expect("last test lock poisoned")
        .clone();
    json::success_response(
        json!({
            "configured": state.api_token().is_some(),
            "api_token_configured": state.api_token().is_some(),
            "account_id": state.account_id(),
            "default_zone_id": state.default_zone_id(),
            "connected": last_test.as_ref().is_some_and(|test| test.ok),
            "last_test": last_test,
            "rate_limit": state.rate_limit_view(),
        }),
        200,
    )
}

pub(super) async fn update_settings(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<UpdateSettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        authorize(&ctx, &req, PERM_CLOUDFLARE_MANAGE)?;
        let input = input.into_inner();
        if let Some(token) = &input.api_token {
            let token = token.trim();
            if token.is_empty() || token.len() > 512 {
                return Err(UnifiedError::bad_request(
                    "api_token must be 1-512 characters",
                ));
            }
            state
                .settings
                .insert("api_token".to_string(), json!(token));
        }
        if let Some(account_id) = &input.account_id {
            validate_cf_id(account_id.trim(), "account")?;
            state
                .settings
                .insert("account_id".to_string(), json!(account_id.trim()));
        }
        if let Some(zone_id) = &input.default_zone_id {
            validate_cf_id(zone_id.trim(), "zone")?;
            state
                .settings
                .insert("default_zone_id".to_string(), json!(zone_id.trim()));
        }
        // Hand only flags and non-secret identifiers to the Settings
        // module for persistence under the `cloudflare` namespace.
        // The raw API token is never put in a hook payload: it stays
        // in this module's own in-memory store (the VCS pattern), so
        // no `settings.merge` listener can observe the credential.
        ctx.hooks
            .emit("settings.merge", state.settings_merge_payload());
        let preview = state
            .api_token()
            .map(|token| mask_token(&token));
        Ok(json!({
            "configured": state.api_token().is_some(),
            "api_token_configured": state.api_token().is_some(),
            "api_token_preview": preview,
            "account_id": state.account_id(),
            "default_zone_id": state.default_zone_id(),
        }))
    })();
    json::respond(result, debug)
}

pub(super) async fn test_connection(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_MANAGE)?;
        let envelope = cf_call(
            &state,
            Method::GET,
            "/user/tokens/verify",
            None,
            &[],
        )
        .await?;
        let result = cf_result(&envelope);
        let token_status = result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        if token_status != "active" {
            return Err(UnifiedError::unauthorized(format!(
                "Cloudflare API token status is \"{token_status}\""
            )));
        }
        let accounts_envelope = cf_call(
            &state,
            Method::GET,
            "/accounts",
            None,
            &[("page".to_string(), "1".to_string()), ("per_page".to_string(), "1".to_string())],
        )
        .await?;
        let accounts_total = accounts_envelope
            .get("result_info")
            .and_then(|info| info.get("total_count"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let first_account = cf_result(&accounts_envelope)
            .as_array()
            .and_then(|accounts| accounts.first().cloned())
            .and_then(|account| {
                account
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        let account_id = state.account_id().or(first_account);
        let test = CfTestResult {
            ok: true,
            token_status: Some(token_status.clone()),
            account_id: account_id.clone(),
            checked_at: Utc::now(),
        };
        *state.last_test.write().expect("last test lock poisoned") = Some(test.clone());
        ctx.hooks.emit(
            "cloudflare.connected",
            json!({
                "account_id": account_id,
                "token_status": token_status,
                "accounts_total": accounts_total,
            }),
        );
        Ok(json!({
            "ok": true,
            "token_status": token_status,
            "account_id": account_id,
            "accounts_total": accounts_total,
            "checked_at": test.checked_at,
        }))
    })()
    .await;
    json::respond(result, debug)
}
