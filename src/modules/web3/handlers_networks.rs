// web3 -- Network handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

use super::constants::*;
use super::events;
use super::helpers::*;
use super::providers::*;
use super::state::*;
use super::types::*;
use super::validation::*;

// ---------------------------------------------------------------------------
// Handlers: networks
// ---------------------------------------------------------------------------

pub(crate) async fn list_networks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ChainNetwork>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let params = query.into_inner();
        let mut items: Vec<ChainNetwork> = state
            .networks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|network| {
                params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| network.kind.as_str() == kind)
                    && params.filters.get("enabled").is_none_or(|enabled| {
                        network.enabled == (*enabled == "true" || *enabled == "1")
                    })
            })
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|network| {
                network.name.to_lowercase().contains(&needle)
                    || network.id.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| a.name.cmp(&b.name));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_network(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<CreateNetworkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChainNetwork> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_ADMIN)?;
        let input = body.into_inner();
        let settings = state.settings_snapshot();
        let kind = ChainKind::from_str(&input.kind).ok_or_else(|| {
            UnifiedError::bad_request("kind must be evm, stellar, cardano, or generic")
        })?;
        let name = require_text(&input.name, "name", 128)?;
        let symbol = validate_asset_symbol(&input.native_asset_symbol)?;
        if kind == ChainKind::Evm && input.chain_id.is_none() {
            return Err(UnifiedError::bad_request("EVM networks require a chain_id"));
        }
        if kind != ChainKind::Evm && input.chain_id.is_some() {
            return Err(UnifiedError::bad_request(
                "chain_id applies to EVM networks only",
            ));
        }
        let allow_private = input.allow_private_endpoint.unwrap_or(false);
        let endpoint = validate_chain_endpoint_url(
            &input.endpoint_url,
            &settings.allowed_extra_hosts,
            allow_private,
            &settings.allowed_private_hosts,
        )?
        .to_string();
        let indexer_url = match &input.indexer_url {
            Some(raw) if !raw.trim().is_empty() => Some(
                validate_chain_endpoint_url(
                    raw,
                    &settings.allowed_extra_hosts,
                    allow_private,
                    &settings.allowed_private_hosts,
                )?
                .to_string(),
            ),
            _ => None,
        };
        let verify_endpoint = match &input.verify_endpoint {
            Some(raw) if !raw.trim().is_empty() => Some(
                validate_chain_endpoint_url(
                    raw,
                    &settings.allowed_extra_hosts,
                    allow_private,
                    &settings.allowed_private_hosts,
                )?
                .to_string(),
            ),
            _ => None,
        };
        let explorer_url_template = match &input.explorer_url_template {
            Some(raw) if !raw.trim().is_empty() => Some(validate_explorer_template(raw)?),
            _ => None,
        };
        let (api_key_encrypted, api_key_configured) = match &input.api_key {
            Some(key) if !key.trim().is_empty() => {
                reject_private_key_like(key, "api_key")?;
                let secret = encryption_secret(&ctx)?;
                (
                    Some(encrypt_secret(&secret, key.trim())),
                    true,
                )
            }
            _ => (None, false),
        };
        let network = ChainNetwork {
            id: new_id(),
            name,
            kind,
            endpoint_url: endpoint,
            chain_id: input.chain_id,
            native_asset_symbol: symbol,
            explorer_url_template,
            enabled: input.enabled.unwrap_or(true),
            indexer_url,
            verify_endpoint,
            api_key_encrypted,
            api_key_configured,
            allow_private_endpoint: allow_private,
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        let endpoint_host = url::Url::parse(&network.endpoint_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_default();
        state.networks.insert(network.id.clone(), network.clone());
        if allow_private && is_private_host(&endpoint_host) {
            // Audit trail for the explicit admin decision to allow a
            // localhost or private endpoint.
            state.emit(
                events::PRIVATE_ENDPOINT_ALLOWED,
                json!({
                    "network_id": network.id,
                    "host": endpoint_host,
                    "allowed_by": caller.actor,
                }),
            );
        }
        Ok(network)
    })();
    match result {
        Ok(network) => json::success_response(network, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Distinct lowercase private hosts used by a network's stored
/// endpoints (endpoint, indexer, verification), for the
/// private-endpoint audit event.
pub(crate) fn private_endpoint_hosts(network: &ChainNetwork) -> Vec<String> {
    let mut hosts: Vec<String> = [&network.endpoint_url]
        .into_iter()
        .chain(network.indexer_url.iter())
        .chain(network.verify_endpoint.iter())
        .filter_map(|raw| url::Url::parse(raw).ok())
        .filter_map(|url| url.host_str().map(str::to_string))
        .map(|host| host.to_ascii_lowercase())
        .filter(|host| is_private_host(host))
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

pub(crate) async fn get_network(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChainNetwork> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        require_network(&state, path.as_str())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_network(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateNetworkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChainNetwork> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_ADMIN)?;
        let input = body.into_inner();
        let mut network = require_network(&state, path.as_str())?;
        let settings = state.settings_snapshot();
        let was_private_allowed = network.allow_private_endpoint;
        let allow_private = input
            .allow_private_endpoint
            .unwrap_or(network.allow_private_endpoint);
        if let Some(name) = &input.name {
            network.name = require_text(name, "name", 128)?;
        }
        if let Some(raw) = &input.endpoint_url {
            network.endpoint_url = validate_chain_endpoint_url(
                raw,
                &settings.allowed_extra_hosts,
                allow_private,
                &settings.allowed_private_hosts,
            )?
            .to_string();
        }
        if let Some(symbol) = &input.native_asset_symbol {
            network.native_asset_symbol = validate_asset_symbol(symbol)?;
        }
        if let Some(chain_id) = input.chain_id {
            if network.kind != ChainKind::Evm {
                return Err(UnifiedError::bad_request(
                    "chain_id applies to EVM networks only",
                ));
            }
            network.chain_id = Some(chain_id);
        }
        if let Some(raw) = &input.explorer_url_template {
            network.explorer_url_template = if raw.trim().is_empty() {
                None
            } else {
                Some(validate_explorer_template(raw)?)
            };
        }
        if let Some(raw) = &input.indexer_url {
            network.indexer_url = if raw.trim().is_empty() {
                None
            } else {
                Some(
                    validate_chain_endpoint_url(
                        raw,
                        &settings.allowed_extra_hosts,
                        allow_private,
                        &settings.allowed_private_hosts,
                    )?
                    .to_string(),
                )
            };
        }
        if let Some(raw) = &input.verify_endpoint {
            network.verify_endpoint = if raw.trim().is_empty() {
                None
            } else {
                Some(
                    validate_chain_endpoint_url(
                        raw,
                        &settings.allowed_extra_hosts,
                        allow_private,
                        &settings.allowed_private_hosts,
                    )?
                    .to_string(),
                )
            };
        }
        if let Some(key) = &input.api_key {
            if key.trim().is_empty() {
                network.api_key_encrypted = None;
                network.api_key_configured = false;
            } else {
                reject_private_key_like(key, "api_key")?;
                let secret = encryption_secret(&ctx)?;
                network.api_key_encrypted = Some(encrypt_secret(&secret, key.trim()));
                network.api_key_configured = true;
            }
        }
        if let Some(enabled) = input.enabled {
            network.enabled = enabled;
        }
        // Re-validate the stored endpoints against the new
        // `allow_private` value, including ones this update did not
        // touch: flipping `allow_private_endpoint` to false must not
        // leave a private endpoint usable, and flipping it to true
        // still requires the host to be on the allowlist.
        network.endpoint_url = validate_chain_endpoint_url(
            &network.endpoint_url,
            &settings.allowed_extra_hosts,
            allow_private,
            &settings.allowed_private_hosts,
        )?
        .to_string();
        if let Some(raw) = network.indexer_url.clone() {
            network.indexer_url = Some(
                validate_chain_endpoint_url(
                    &raw,
                    &settings.allowed_extra_hosts,
                    allow_private,
                    &settings.allowed_private_hosts,
                )?
                .to_string(),
            );
        }
        if let Some(raw) = network.verify_endpoint.clone() {
            network.verify_endpoint = Some(
                validate_chain_endpoint_url(
                    &raw,
                    &settings.allowed_extra_hosts,
                    allow_private,
                    &settings.allowed_private_hosts,
                )?
                .to_string(),
            );
        }
        network.allow_private_endpoint = allow_private;
        network.updated_at = now();
        state.networks.insert(network.id.clone(), network.clone());
        // Drop the cached provider so the next call rebuilds it from
        // the updated record.
        state.providers.remove(&network.id);
        if !was_private_allowed && allow_private {
            // Audit trail for a private host that becomes newly
            // allowed by this update (same event as on creation).
            for host in private_endpoint_hosts(&network) {
                state.emit(
                    events::PRIVATE_ENDPOINT_ALLOWED,
                    json!({
                        "network_id": network.id,
                        "host": host,
                        "allowed_by": caller.actor,
                    }),
                );
            }
        }
        Ok(network)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_network(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_ADMIN)?;
        let network = require_network(&state, path.as_str())?;
        let in_use = state
            .watched
            .iter()
            .any(|entry| entry.value().network_id == network.id)
            || state
                .wallets
                .iter()
                .any(|entry| entry.value().network_id == network.id)
            || state
                .anchors
                .iter()
                .any(|entry| entry.value().network_id == network.id);
        if in_use {
            return Err(UnifiedError::conflict(
                "Network is still referenced by watched addresses, wallets, or anchors; remove those first",
            ));
        }
        state.networks.remove(path.as_str());
        state.providers.remove(&network.id);
        Ok(json!({ "deleted": true, "id": network.id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn network_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<NetworkStatus> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let network = require_network(&state, path.as_str())?;
        // The secret is only needed when the network has a stored
        // API key; an empty value fails closed inside
        // `provider_for_network` if a stored key needs decrypting.
        let secret = encryption_secret(&ctx).unwrap_or_default();
        let provider = provider_for_network(&state, &network, &secret)?;
        provider.network_status().await
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn network_fee_estimate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FeeEstimate> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let network = require_network(&state, path.as_str())?;
        // The secret is only needed when the network has a stored
        // API key; an empty value fails closed inside
        // `provider_for_network` if a stored key needs decrypting.
        let secret = encryption_secret(&ctx).unwrap_or_default();
        let provider = provider_for_network(&state, &network, &secret)?;
        provider.estimate_fee().await
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Tests for the network-update private-endpoint rules (W5)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_network() -> ChainNetwork {
        ChainNetwork {
            id: "net-1".to_string(),
            name: "Local".to_string(),
            kind: ChainKind::Evm,
            endpoint_url: "http://localhost:8545".to_string(),
            chain_id: Some(1),
            native_asset_symbol: "ETH".to_string(),
            explorer_url_template: None,
            enabled: true,
            indexer_url: Some("https://api.etherscan.io".to_string()),
            verify_endpoint: Some("http://127.0.0.1:9999".to_string()),
            api_key_encrypted: None,
            api_key_configured: false,
            allow_private_endpoint: true,
            created_by: "admin".to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn private_endpoint_hosts_lists_only_private_hosts_once() {
        let hosts = private_endpoint_hosts(&test_network());
        assert_eq!(hosts, vec!["127.0.0.1".to_string(), "localhost".to_string()]);
        let mut public = test_network();
        public.endpoint_url = "https://horizon.stellar.org".to_string();
        public.indexer_url = None;
        public.verify_endpoint = None;
        assert!(private_endpoint_hosts(&public).is_empty());
    }

    #[test]
    fn stored_private_endpoint_is_rejected_when_private_is_disallowed() {
        // This is the re-validation `update_network` runs over the
        // stored endpoints: flipping allow_private to false must
        // reject the update instead of leaving the endpoint usable.
        let stored = "http://localhost:8545";
        assert!(
            validate_chain_endpoint_url(stored, &[], false, &["localhost".to_string()])
                .is_err()
        );
        assert!(
            validate_chain_endpoint_url(stored, &[], true, &["localhost".to_string()]).is_ok()
        );
    }
}
