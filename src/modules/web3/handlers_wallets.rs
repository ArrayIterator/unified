// web3 -- Wallet and balance handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::storage::sha256_hex;
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

// Handlers: wallets and balances

pub(crate) async fn list_wallets(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<WalletLink>> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let params = query.into_inner();
        let mut items: Vec<WalletLink> = state
            .wallets
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|wallet| {
                (admin || wallet.user_id == caller.actor)
                    && (admin || in_company_scope(&wallet.company_id, &scope))
                    && params
                        .filters
                        .get("network_id")
                        .is_none_or(|network_id| &wallet.network_id == network_id)
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|user_id| &wallet.user_id == user_id)
                    && params
                        .filters
                        .get("verification_status")
                        .is_none_or(|status| &wallet.verification_status == status)
            })
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|wallet| {
                wallet.address.to_lowercase().contains(&needle)
                    || wallet
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

pub(crate) async fn link_wallet(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<LinkWalletRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WalletLink> = (|| {
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
        let duplicate = state.wallets.iter().any(|entry| {
            let wallet = entry.value();
            wallet.user_id == caller.actor
                && wallet.network_id == network.id
                && wallet.address.eq_ignore_ascii_case(&address)
        });
        if duplicate {
            return Err(UnifiedError::conflict(
                "This address is already linked on this network",
            ));
        }
        let wallet = WalletLink {
            id: new_id(),
            user_id: caller.actor.clone(),
            company_id,
            network_id: network.id,
            address,
            label,
            verification_status: "unverified".to_string(),
            challenge_nonce_hash: None,
            challenge_expires_at: None,
            verified_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.wallets.insert(wallet.id.clone(), wallet.clone());
        state.emit(
            events::WALLET_LINKED,
            json!({
                "wallet_id": wallet.id,
                "user_id": wallet.user_id,
                "network_id": wallet.network_id,
                "address": wallet.address,
            }),
        );
        Ok(wallet)
    })();
    match result {
        Ok(wallet) => json::success_response(wallet, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_wallet(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WalletLink> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "This wallet belongs to another user",
            ));
        }
        if !admin {
            require_record_in_company_scope(&wallet.company_id, &scope)?;
        }
        Ok(wallet)
    })();
    json::respond(result, debug)
}

pub(crate) async fn unlink_wallet(
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
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can unlink this wallet",
            ));
        }
        if !admin {
            require_record_writable_in_company_scope(&wallet.company_id, &scope)?;
        }
        state.wallets.remove(path.as_str());
        state.challenges.remove(path.as_str());
        state
            .balances
            .retain(|_, balance| balance.wallet_id != wallet.id);
        state.emit(
            events::WALLET_UNLINKED,
            json!({
                "wallet_id": wallet.id,
                "user_id": wallet.user_id,
                "network_id": wallet.network_id,
                "address": wallet.address,
            }),
        );
        Ok(json!({ "deleted": true, "id": wallet.id }))
    })();
    json::respond(result, debug)
}

/// Issue a signed-message challenge for a linked wallet. Returns the
/// message to sign and the expiry; the raw nonce lives only in the
/// in-memory challenge map (`state.challenges`) and is never stored
/// in the wallet row or any persistent store -- only its SHA-256 is
/// stored on the wallet record. For Stellar wallets the response
/// also carries `stellar_memo_token`: the 28-character token the
/// owner must put in the proof transaction's text memo (the full
/// 64-character nonce cannot fit in a Stellar text memo).
pub(crate) async fn challenge_wallet(
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
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can challenge this wallet",
            ));
        }
        if !admin {
            require_record_writable_in_company_scope(&wallet.company_id, &scope)?;
        }
        let nonce_bytes: [u8; 32] = rand::random();
        let nonce: String = {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut out = String::with_capacity(nonce_bytes.len() * 2);
            for &byte in nonce_bytes.iter() {
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
            out
        };
        let network = require_network(&state, &wallet.network_id)?;
        // Stellar text memos hold at most 28 bytes, so a Stellar
        // wallet cannot memo the 64-character nonce itself. The
        // owner memos the short token derived from it instead (see
        // `stellar_memo_token`); other chains sign the full nonce.
        let memo_token = if network.kind == ChainKind::Stellar {
            Some(stellar_memo_token(&nonce))
        } else {
            None
        };
        let message = match &memo_token {
            Some(token) => format!(
                "Unified wallet verification.\n\nSign this message to prove you control this address.\n\nNonce: {nonce}\nStellar memo (text memo, max 28 characters): {token}"
            ),
            None => format!(
                "Unified wallet verification.\n\nSign this message to prove you control this address.\n\nNonce: {nonce}"
            ),
        };
        let expires_at = now() + chrono::Duration::seconds(CHALLENGE_TTL_SECS);
        state.challenges.insert(
            wallet.id.clone(),
            WalletChallenge {
                wallet_id: wallet.id.clone(),
                nonce: nonce.clone(),
                message: message.clone(),
                expires_at,
            },
        );
        if let Some(mut entry) = state.wallets.get_mut(&wallet.id) {
            entry.challenge_nonce_hash = Some(sha256_hex(nonce.as_bytes()));
            entry.challenge_expires_at = Some(expires_at);
            entry.verification_status = "pending".to_string();
            entry.updated_at = now();
        }
        Ok(json!({
            "wallet_id": wallet.id,
            "address": wallet.address,
            "network_id": wallet.network_id,
            "message": message,
            "nonce": nonce,
            "stellar_memo_token": memo_token,
            "expires_at": expires_at,
        }))
    })();
    json::respond(result, debug)
}

/// Verify a wallet challenge. EVM wallets submit the signature of
/// the challenge message; Stellar wallets submit the hash of a
/// transaction whose text memo carries the 28-character memo token
/// derived from the challenge nonce (the full nonce cannot fit in a
/// Stellar text memo); Cardano wallets are verified by provider
/// lookup only (reported as `lookup_only`).
pub(crate) async fn verify_wallet(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<VerifyWalletRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can verify this wallet",
            ));
        }
        if !admin {
            require_record_writable_in_company_scope(&wallet.company_id, &scope)?;
        }
        let challenge = state
            .challenges
            .get(&wallet.id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found("No active challenge for this wallet; request one first")
            })?;
        if challenge.expires_at < now() {
            state.challenges.remove(&wallet.id);
            return Err(UnifiedError::bad_request(
                "Challenge expired; request a new one",
            ));
        }
        let network = require_network(&state, &wallet.network_id)?;
        // The secret is only needed when the network has a stored
        // API key; an empty value fails closed inside
        // `provider_for_network` if a stored key needs decrypting.
        let secret = encryption_secret(&ctx).unwrap_or_default();
        let provider = provider_for_network(&state, &network, &secret)?;
        let proof = match network.kind {
            ChainKind::Evm => body
                .signature
                .clone()
                .ok_or_else(|| {
                    UnifiedError::bad_request("signature is required for EVM wallets")
                })?,
            ChainKind::Stellar => body
                .proof_tx_hash
                .clone()
                .ok_or_else(|| {
                    UnifiedError::bad_request(
                        "proof_tx_hash is required for Stellar wallets: send a transaction from the address with the 28-character memo token from the challenge as its text memo",
                    )
                })?,
            _ => String::new(),
        };
        // Cardano verification ignores the nonce and reports the
        // lookup outcome; the message passed is the challenge nonce
        // so the lookup path stays explicit. Stellar verification
        // receives the short memo token, not the 64-character nonce,
        // because a Stellar text memo holds at most 28 bytes (the
        // provider also accepts the raw nonce and derives the token
        // itself, so either form verifies).
        let message = if network.kind == ChainKind::Stellar {
            stellar_memo_token(&challenge.nonce)
        } else {
            challenge.nonce.clone()
        };
        let outcome = provider
            .verify_message(&wallet.address, &message, &proof)
            .await?;
        let mut updated = wallet.clone();
        if outcome.verified {
            updated.verification_status = "verified".to_string();
            updated.verified_at = Some(now());
        } else if outcome.method == "address_lookup" {
            updated.verification_status = "lookup_only".to_string();
        } else {
            updated.verification_status = "failed".to_string();
        }
        updated.updated_at = now();
        state
            .wallets
            .insert(updated.id.clone(), updated.clone());
        state.challenges.remove(&wallet.id);
        Ok(json!({
            "wallet_id": updated.id,
            "verification_status": updated.verification_status,
            "verified": outcome.verified,
            "method": outcome.method,
            "detail": outcome.detail,
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn list_wallet_balances(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<BalanceSnapshot>> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "This wallet belongs to another user",
            ));
        }
        if !admin {
            require_record_in_company_scope(&wallet.company_id, &scope)?;
        }
        let params = query.into_inner();
        let mut items: Vec<BalanceSnapshot> = state
            .balances
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|balance| {
                balance.wallet_id == wallet.id
                    && params
                        .filters
                        .get("asset")
                        .is_none_or(|asset| &balance.asset == asset)
            })
            .collect();
        items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn refresh_wallet_balances(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BalanceSnapshot> = (|| async {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let wallet = state
            .wallets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Wallet not found"))?;
        if !admin && wallet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner or an admin can refresh this wallet",
            ));
        }
        if !admin {
            require_record_writable_in_company_scope(&wallet.company_id, &scope)?;
        }
        let network = require_network(&state, &wallet.network_id)?;
        // The secret is only needed when the network has a stored
        // API key; an empty value fails closed inside
        // `provider_for_network` if a stored key needs decrypting.
        let secret = encryption_secret(&ctx).unwrap_or_default();
        let provider = provider_for_network(&state, &network, &secret)?;
        let balance = provider.get_balance(&wallet.address, None).await?;
        let amount = if balance.amount_raw.chars().all(|c| c.is_ascii_digit()) {
            decimal_shift(&balance.amount_raw, balance.decimals)
        } else {
            None
        };
        let existing = state
            .balances
            .iter()
            .find(|entry| {
                entry.value().wallet_id == wallet.id && entry.value().asset == balance.asset
            })
            .map(|entry| entry.value().clone());
        let snapshot = BalanceSnapshot {
            id: existing
                .as_ref()
                .map(|snapshot| snapshot.id.clone())
                .unwrap_or_else(new_id),
            wallet_id: wallet.id,
            network_id: network.id,
            address: wallet.address,
            asset: balance.asset,
            amount_raw: balance.amount_raw,
            amount,
            decimals: balance.decimals,
            checked_at: balance.checked_at,
            created_at: existing
                .as_ref()
                .map(|snapshot| snapshot.created_at)
                .unwrap_or_else(now),
            updated_at: now(),
        };
        state.balances.insert(snapshot.id.clone(), snapshot.clone());
        Ok(snapshot)
    })()
    .await;
    json::respond(result, debug)
}
