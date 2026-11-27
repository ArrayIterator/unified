// web3 -- Watched address and transaction handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

use super::constants::*;
use super::helpers::*;
use super::polling::*;
use super::state::*;
use super::types::*;
use super::validation::*;
use super::providers::{require_network};

// Handlers: watched addresses and transactions

pub(crate) async fn list_watched_addresses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<WatchedAddress>> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let params = query.into_inner();
        let mut items: Vec<WatchedAddress> = state
            .watched
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|address| {
                (admin || address.user_id == caller.actor)
                    && (admin || in_company_scope(&address.company_id, &scope))
                    && params
                        .filters
                        .get("network_id")
                        .is_none_or(|network_id| &address.network_id == network_id)
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|user_id| &address.user_id == user_id)
            })
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|address| {
                address.address.to_lowercase().contains(&needle)
                    || address
                        .label
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&needle)
            });
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_watched_address(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<CreateWatchedAddressRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WatchedAddress> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let input = body.into_inner();
        let network = require_network(&state, &input.network_id)?;
        let address = validate_address(network.kind, &input.address)?;
        let label = match &input.label {
            Some(raw) if !raw.trim().is_empty() => Some(require_text(raw, "label", 128)?),
            _ => None,
        };
        let requested_company = match &input.company_id {
            Some(raw) if !raw.trim().is_empty() => Some(require_text(raw, "company_id", 64)?),
            _ => None,
        };
        let company_id = require_company_match(&_tenant, &req, &requested_company)?;
        let duplicate = state.watched.iter().any(|entry| {
            entry.value().network_id == network.id
                && entry.value().address.eq_ignore_ascii_case(&address)
                && entry.value().user_id == caller.actor
        });
        if duplicate {
            return Err(UnifiedError::conflict(
                "This address is already watched on this network",
            ));
        }
        let record = WatchedAddress {
            id: new_id(),
            network_id: network.id,
            address,
            label,
            user_id: caller.actor,
            company_id,
            last_checked_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.watched.insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn delete_watched_address(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let record = state
            .watched
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Watched address not found"))?;
        if !admin && record.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can remove this watched address",
            ));
        }
        if !admin {
            require_record_writable_in_company_scope(&record.company_id, &scope)?;
        }
        state.watched.remove(path.as_str());
        Ok(json!({ "deleted": true, "id": record.id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn poll_watched_address(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let record = state
            .watched
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Watched address not found"))?;
        if !admin && record.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can poll this watched address",
            ));
        }
        if !admin {
            require_record_writable_in_company_scope(&record.company_id, &scope)?;
        }
        poll_one_address(&state, &poll_secret(&ctx), &record).await
    })()
    .await;
    json::respond(result, debug)
}

/// Poll every watched address on every network. Admin-only: a bulk
/// poll hits every provider at once, so any write-capable caller
/// must not be able to force it and burn provider quota or rate
/// limits. (Polling a single owned address stays write-gated in
/// `poll_watched_address`.)
pub(crate) async fn poll_all_watched(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_ADMIN)?;
        poll_watched_addresses(&state, &poll_secret(&ctx)).await
    })()
    .await;
    json::respond(result, debug)
}

/// The secret handed to the poller. It is only needed when a network
/// has a stored API key; an empty value fails closed inside
/// `provider_for_network` if a stored key needs decrypting.
fn poll_secret(ctx: &AppContext) -> String {
    encryption_secret(ctx).unwrap_or_default()
}

pub(crate) async fn list_transactions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ChainTxRecord>> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let params = query.into_inner();
        let own_watched: Vec<String> = if admin {
            Vec::new()
        } else {
            state
                .watched
                .iter()
                .filter(|entry| {
                    entry.value().user_id == caller.actor
                        && in_company_scope(&entry.value().company_id, &scope)
                })
                .map(|entry| entry.value().id.clone())
                .collect()
        };
        let mut items: Vec<ChainTxRecord> = state
            .transactions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|tx| {
                (admin
                    || tx
                        .watched_address_id
                        .as_deref()
                        .is_some_and(|id| own_watched.contains(&id.to_string())))
                    && params
                        .filters
                        .get("network_id")
                        .is_none_or(|network_id| &tx.network_id == network_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| &tx.status == status)
                    && params
                        .filters
                        .get("direction")
                        .is_none_or(|direction| &tx.direction == direction)
                    && params
                        .filters
                        .get("watched_address_id")
                        .is_none_or(|watched_id| {
                            tx.watched_address_id.as_deref() == Some(watched_id.as_str())
                        })
                    && params.filters.get("address").is_none_or(|address| {
                        tx.from_address.eq_ignore_ascii_case(address)
                            || tx.to_address.eq_ignore_ascii_case(address)
                    })
            })
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|tx| {
                tx.tx_hash.to_lowercase().contains(&needle)
                    || tx.from_address.to_lowercase().contains(&needle)
                    || tx.to_address.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| b.first_seen_at.cmp(&a.first_seen_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_transaction(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChainTxRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let record = state
            .transactions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Transaction not found"))?;
        if !admin {
            let parent = record
                .watched_address_id
                .as_deref()
                .and_then(|id| state.watched.get(id))
                .map(|entry| entry.value().clone());
            let owned = parent
                .as_ref()
                .is_some_and(|address| address.user_id == caller.actor);
            if !owned {
                return Err(UnifiedError::forbidden(
                    "This transaction belongs to another user's watched address",
                ));
            }
            let parent_company = parent.and_then(|address| address.company_id);
            require_record_in_company_scope(&parent_company, &scope)?;
        }
        Ok(record)
    })();
    json::respond(result, debug)
}
