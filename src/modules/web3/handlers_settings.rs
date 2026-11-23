// web3 -- Settings handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{HttpRequest, HttpResponse, web};
use std::sync::Arc;

use super::constants::*;
use super::helpers::*;
use super::state::*;
use super::types::*;
use super::validation::*;

// ---------------------------------------------------------------------------
// Handlers: settings
// ---------------------------------------------------------------------------

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Web3Settings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_ADMIN)?;
        Ok(state.settings_snapshot())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Web3Settings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_ADMIN)?;
        let input = body.into_inner();
        let mut settings = state.settings_snapshot();
        if let Some(enabled) = input.public_ledger_enabled {
            settings.public_ledger_enabled = enabled;
        }
        if let Some(hosts) = input.allowed_private_hosts {
            let mut cleaned: Vec<String> = Vec::new();
            for host in hosts {
                let host = host.trim().to_ascii_lowercase();
                if host.is_empty() {
                    continue;
                }
                if host.chars().any(|c| c.is_control() || c == '/' || c == ':') {
                    return Err(UnifiedError::bad_request(
                        "Allowed private hosts must be plain hostnames or IP literals",
                    ));
                }
                cleaned.push(host);
            }
            cleaned.sort();
            cleaned.dedup();
            settings.allowed_private_hosts = cleaned;
        }
        if let Some(hosts) = input.allowed_extra_hosts {
            let mut cleaned: Vec<String> = Vec::new();
            for host in hosts {
                let host = host.trim().to_ascii_lowercase();
                if host.is_empty() {
                    continue;
                }
                if host.chars().any(|c| c.is_control() || c == '/' || c == ':') {
                    return Err(UnifiedError::bad_request(
                        "Allowed extra hosts must be plain hostnames",
                    ));
                }
                cleaned.push(host);
            }
            cleaned.sort();
            cleaned.dedup();
            settings.allowed_extra_hosts = cleaned;
        }
        if let Some(raw) = &input.anchoring_relay_url {
            if raw.trim().is_empty() {
                settings.anchoring_relay_url = None;
            } else {
                let url = validate_chain_endpoint_url(
                    raw,
                    &settings.allowed_extra_hosts,
                    false,
                    &settings.allowed_private_hosts,
                )?;
                settings.anchoring_relay_url = Some(url.to_string());
            }
        }
        if let Some(key) = &input.anchoring_key {
            if key.trim().is_empty() {
                settings.anchoring_key_encrypted = None;
                settings.anchoring_key_configured = false;
            } else {
                reject_private_key_like(key, "anchoring_key")?;
                // Storing a key encrypts it: a missing app secret
                // fails closed instead of using a fallback.
                let secret = encryption_secret(&ctx)?;
                settings.anchoring_key_encrypted =
                    Some(encrypt_secret(&secret, key.trim()));
                settings.anchoring_key_configured = true;
            }
        }
        if let Some(limit) = input.max_poll_transactions {
            settings.max_poll_transactions = limit.clamp(1, MAX_POLL_LIMIT);
        }
        *state.settings.write().expect("settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    json::respond(result, debug)
}
