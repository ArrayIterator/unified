// web3 -- Anchor and transparency ledger handlers.

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
use super::handlers_watched::*;
use super::helpers::*;
use super::providers::*;
use super::state::*;
use super::types::*;
use super::validation::*;

// ---------------------------------------------------------------------------
// Handlers: anchors and the transparency ledger
// ---------------------------------------------------------------------------

pub(crate) async fn create_anchor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<CreateAnchorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnchorRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let input = body.into_inner();
        let network = require_network(&state, &input.network_id)?;
        let record_type = require_slug(&input.record_type, "record_type", 64)?;
        let record_id = require_text(&input.record_id, "record_id", 128)?;
        let content_sha256 = resolve_content_hash(&input)?;
        let requested_company = match &input.company_id {
            Some(raw) if !raw.trim().is_empty() => Some(require_text(raw, "company_id", 64)?),
            _ => None,
        };
        let company_id = require_company_match(&_tenant, &req, &requested_company)?;
        let duplicate = state.anchors.iter().any(|entry| {
            let anchor = entry.value();
            anchor.record_type == record_type
                && anchor.record_id == record_id
                && anchor.content_sha256 == content_sha256
                && anchor.status != "failed"
        });
        if duplicate {
            return Err(UnifiedError::conflict(
                "This record hash is already anchored (or awaiting anchoring)",
            ));
        }
        let record = AnchorRecord {
            id: new_id(),
            record_type,
            record_id,
            content_sha256,
            network_id: network.id,
            status: "pending".to_string(),
            tx_hash: None,
            confirmations: 0,
            block_number: None,
            requested_by: caller.actor.clone(),
            company_id,
            submitted_at: None,
            confirmed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.anchors.insert(record.id.clone(), record.clone());
        state.emit(
            events::ANCHOR_REQUESTED,
            serde_json::to_value(&record).unwrap_or(Value::Null),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_anchors(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AnchorRecord>> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let params = query.into_inner();
        let mut items: Vec<AnchorRecord> = state
            .anchors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|anchor| {
                (admin || anchor.requested_by == caller.actor)
                    && (admin || in_company_scope(&anchor.company_id, &scope))
                    && params
                        .filters
                        .get("network_id")
                        .is_none_or(|network_id| &anchor.network_id == network_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| &anchor.status == status)
                    && params
                        .filters
                        .get("record_type")
                        .is_none_or(|record_type| &anchor.record_type == record_type)
            })
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|anchor| {
                anchor.record_id.to_lowercase().contains(&needle)
                    || anchor
                        .tx_hash
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

pub(crate) async fn get_anchor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnchorRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let record = state
            .anchors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Anchor not found"))?;
        if !admin && record.requested_by != caller.actor {
            return Err(UnifiedError::forbidden(
                "This anchor was requested by another user",
            ));
        }
        if !admin {
            require_record_in_company_scope(&record.company_id, &scope)?;
        }
        Ok(record)
    })();
    json::respond(result, debug)
}

/// Submit a pending anchor to the configured anchoring relay. The
/// relay -- never this module -- sends the on-chain transaction, using
/// the dedicated anchoring-only service key. Without a relay or a
/// key configured, submission fails with a 422 that says so.
///
/// A transient relay failure (5xx or 429, or a transport error) is
/// not terminal: the anchor stays `pending` (or `failed`, if it was
/// being resubmitted) so it can be retried. Only a terminal relay
/// rejection (other 4xx) marks the anchor `failed`. A `failed`
/// anchor may be resubmitted.
pub(crate) async fn submit_anchor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnchorRecord> = (|| async {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let anchor = state
            .anchors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Anchor not found"))?;
        if !admin && anchor.requested_by != caller.actor {
            return Err(UnifiedError::forbidden(
                "This anchor was requested by another user",
            ));
        }
        if !admin {
            require_record_writable_in_company_scope(&anchor.company_id, &scope)?;
        }
        if anchor.status != "pending" && anchor.status != "failed" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending or failed anchor can be submitted (current status: {})",
                anchor.status
            )));
        }
        let settings = state.settings_snapshot();
        let relay_url = settings.anchoring_relay_url.clone().ok_or_else(|| {
            UnifiedError::unprocessable(
                "No anchoring relay is configured: an admin must set one in web3 settings first",
            )
        })?;
        let encrypted_key = settings.anchoring_key_encrypted.clone().ok_or_else(|| {
            UnifiedError::unprocessable(
                "No anchoring service key is configured: an admin must set one in web3 settings first",
            )
        })?;
        // The anchoring key is definitely stored here, so a missing
        // app secret fails closed instead of decrypting with a fallback.
        let secret = encryption_secret(&ctx)?;
        let service_key = decrypt_secret(&secret, &encrypted_key)?;
        let response = state
            .http
            .post(relay_url.as_str())
            .bearer_auth(service_key)
            .json(&json!({
                "record_type": anchor.record_type,
                "record_id": anchor.record_id,
                "content_sha256": anchor.content_sha256,
                "network_id": anchor.network_id,
            }))
            .send()
            .await
            .map_err(|_| UnifiedError::internal("Anchoring relay request failed"))?;
        let status = response.status();
        if !status.is_success() {
            if is_retryable_relay_status(status) {
                // Transient relay failure: leave the anchor in its
                // current status (pending, or failed when this was a
                // resubmission) so it can be retried. One relay 500
                // must not brick the anchor.
                return Err(map_provider_status(status));
            }
            if let Some(mut entry) = state.anchors.get_mut(&anchor.id) {
                entry.status = "failed".to_string();
                entry.updated_at = now();
            }
            state.emit(
                events::ANCHOR_FAILED,
                json!({ "anchor_id": anchor.id, "record_type": anchor.record_type, "record_id": anchor.record_id }),
            );
            return Err(map_provider_status(status));
        }
        let body: Value = response.json().await.map_err(|_| {
            UnifiedError::internal("Anchoring relay returned an unreadable response")
        })?;
        let tx_hash = body
            .get("tx_hash")
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::internal("Anchoring relay returned no transaction hash")
            })?
            .to_string();
        let mut updated = anchor.clone();
        updated.status = "submitted".to_string();
        updated.tx_hash = Some(tx_hash);
        updated.submitted_at = Some(now());
        updated.updated_at = now();
        state
            .anchors
            .insert(updated.id.clone(), updated.clone());
        Ok(updated)
    })()
    .await;
    json::respond(result, debug)
}

/// Refresh a submitted anchor against the chain: update its
/// confirmations and move it to `confirmed` (emitting
/// `web3.anchor.confirmed`) once the threshold is reached.
pub(crate) async fn refresh_anchor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnchorRecord> = (|| async {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_WRITE)?;
        let admin = is_admin(&ctx, &caller.role);
        let anchor = state
            .anchors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Anchor not found"))?;
        if !admin && anchor.requested_by != caller.actor {
            return Err(UnifiedError::forbidden(
                "This anchor was requested by another user",
            ));
        }
        if !admin {
            require_record_writable_in_company_scope(&anchor.company_id, &scope)?;
        }
        let tx_hash = anchor.tx_hash.clone().ok_or_else(|| {
            UnifiedError::unprocessable("Anchor has no transaction hash yet; submit it first")
        })?;
        let network = require_network(&state, &anchor.network_id)?;
        // The secret is only needed when the network has a stored
        // API key; an empty value fails closed inside
        // `provider_for_network` if a stored key needs decrypting.
        let secret = encryption_secret(&ctx).unwrap_or_default();
        let provider = provider_for_network(&state, &network, &secret)?;
        let tx = provider.get_transaction(&tx_hash).await?;
        let mut updated = anchor.clone();
        updated.confirmations = tx.confirmations;
        updated.block_number = tx.block_number;
        updated.updated_at = now();
        if tx.status == "failed" {
            updated.status = "failed".to_string();
            state
                .anchors
                .insert(updated.id.clone(), updated.clone());
            state.emit(
                events::ANCHOR_FAILED,
                json!({ "anchor_id": updated.id, "record_type": updated.record_type, "record_id": updated.record_id, "tx_hash": tx_hash }),
            );
            return Ok(updated);
        }
        if tx.status == "confirmed" && tx.confirmations >= REQUIRED_CONFIRMATIONS {
            let was_confirmed = updated.status == "confirmed";
            updated.status = "confirmed".to_string();
            if updated.confirmed_at.is_none() {
                updated.confirmed_at = Some(now());
            }
            state
                .anchors
                .insert(updated.id.clone(), updated.clone());
            if !was_confirmed {
                state.emit(
                    events::ANCHOR_CONFIRMED,
                    serde_json::to_value(&updated).unwrap_or(Value::Null),
                );
            }
            return Ok(updated);
        }
        if updated.status == "pending" {
            updated.status = "submitted".to_string();
        }
        state
            .anchors
            .insert(updated.id.clone(), updated.clone());
        Ok(updated)
    })()
    .await;
    json::respond(result, debug)
}

/// Verify a record against its anchor: recompute the record hash and
/// compare it with the anchored hash retrieved via the provider.
/// Result is `match`, `mismatch`, or `not-found`.
pub(crate) async fn verify_anchor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    body: web::Json<VerifyAnchorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let input = body.into_inner();
        let record_type = require_slug(&input.record_type, "record_type", 64)?;
        let record_id = require_text(&input.record_id, "record_id", 128)?;
        let claimed_hash = match (&input.content_sha256, &input.content) {
            (Some(hash), _) => validate_sha256_hex(hash, "content_sha256")?,
            (None, Some(content)) => sha256_hex(content.as_bytes()),
            (None, None) => {
                return Err(UnifiedError::bad_request(
                    "Provide content_sha256 or content to verify against the anchor",
                ));
            }
        };
        let matching: Vec<AnchorRecord> = state
            .anchors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|anchor| anchor.record_type == record_type && anchor.record_id == record_id)
            .filter(|anchor| admin || in_company_scope(&anchor.company_id, &scope))
            .collect();
        let Some(anchor) = select_anchor_for_verification(&matching).cloned() else {
            return Ok(json!({
                "result": "not-found",
                "record_type": record_type,
                "record_id": record_id,
                "claimed_hash": claimed_hash,
                "anchored_hash": Value::Null,
                "tx_found": false,
            }));
        };
        let mut tx_found = false;
        let mut confirmations = anchor.confirmations;
        if let Some(tx_hash) = &anchor.tx_hash {
            if let Ok(network) = require_network(&state, &anchor.network_id) {
                // Best-effort lookup: without a configured secret the
                // provider simply cannot be built and the lookup is
                // skipped, as before.
                if let Ok(secret) = encryption_secret(&ctx) {
                    if let Ok(provider) =
                        provider_for_network(&state, &network, &secret)
                    {
                        if let Ok(tx) = provider.get_transaction(tx_hash).await {
                            tx_found = true;
                            confirmations = tx.confirmations;
                        }
                    }
                }
            }
        }
        let result = if anchor.content_sha256 == claimed_hash {
            "match"
        } else {
            "mismatch"
        };
        Ok(json!({
            "result": result,
            "record_type": record_type,
            "record_id": record_id,
            "claimed_hash": claimed_hash,
            "anchored_hash": anchor.content_sha256,
            "anchor_id": anchor.id,
            "network_id": anchor.network_id,
            "tx_hash": anchor.tx_hash,
            "anchor_status": anchor.status,
            "confirmations": confirmations,
            "tx_found": tx_found,
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn list_ledger(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<Web3State>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LedgerEntry>> = (|| {
        let _tenant = require_tenant(&req)?;
        let scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WEB3_READ)?;
        let admin = is_admin(&ctx, &caller.role);
        let params = query.into_inner();
        let mut items: Vec<LedgerEntry> = state
            .anchors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|anchor| {
                anchor.tx_hash.is_some()
                    && (admin || anchor.requested_by == caller.actor)
                    && (admin || in_company_scope(&anchor.company_id, &scope))
                    && params
                        .filters
                        .get("network_id")
                        .is_none_or(|network_id| &anchor.network_id == network_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| &anchor.status == status)
                    && params
                        .filters
                        .get("record_type")
                        .is_none_or(|record_type| &anchor.record_type == record_type)
            })
            .map(|anchor| ledger_entry(&anchor))
            .collect();
        items.sort_by(|a, b| b.anchored_at.cmp(&a.anchored_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

/// Public transparency ledger. Default-deny: answers 403 unless an
/// admin enabled the public ledger in web3 settings. The API
/// Management module's allowed-public-access controls are the outer
/// gate for this route; this flag is the module-level gate. No caller
/// headers are required when the ledger is public.
pub(crate) async fn public_ledger(
    state: web::Data<Arc<Web3State>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let settings = state.settings_snapshot();
    if !settings.public_ledger_enabled {
        return json::error_response(
            &UnifiedError::forbidden(
                "The public transparency ledger is disabled; an admin must enable it first",
            ),
            false,
        );
    }
    let params = query.into_inner();
    let mut items: Vec<LedgerEntry> = state
        .anchors
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|anchor| {
            anchor.tx_hash.is_some()
                && anchor.status == "confirmed"
                && params
                    .filters
                    .get("network_id")
                    .is_none_or(|network_id| &anchor.network_id == network_id)
                && params
                    .filters
                    .get("record_type")
                    .is_none_or(|record_type| &anchor.record_type == record_type)
        })
        .map(|anchor| ledger_entry(&anchor))
        .collect();
    items.sort_by(|a, b| b.anchored_at.cmp(&a.anchored_at));
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Pure helpers (W6, W7) -- unit-tested below
// ---------------------------------------------------------------------------

/// True when a relay HTTP status is a transient failure worth
/// retrying: any 5xx, or 429 rate limiting. Other 4xx responses are
/// terminal rejections (bad request, unauthorized, not found) and
/// mark the anchor `failed`.
pub(crate) fn is_retryable_relay_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error() || status.as_u16() == 429
}

/// Pick the anchor a verification compares against. The newest
/// record is not always the right one: a newer `failed` submission
/// must not shadow an older `confirmed` anchor, or a valid record
/// would report `mismatch`. Preference order: newest `confirmed`,
/// then newest non-`failed`, then the newest record of any status.
pub(crate) fn select_anchor_for_verification<'a>(
    anchors: &'a [AnchorRecord],
) -> Option<&'a AnchorRecord> {
    let mut newest_first: Vec<&AnchorRecord> = anchors.iter().collect();
    newest_first.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    newest_first
        .iter()
        .copied()
        .find(|anchor| anchor.status == "confirmed")
        .or_else(|| {
            newest_first
                .iter()
                .copied()
                .find(|anchor| anchor.status != "failed")
        })
        .or_else(|| newest_first.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn test_anchor(status: &str, created_at: DateTime<Utc>) -> AnchorRecord {
        AnchorRecord {
            id: format!("anchor-{status}-{}", created_at.timestamp()),
            record_type: "invoice".to_string(),
            record_id: "INV-1".to_string(),
            content_sha256: "a".repeat(64),
            network_id: "net-1".to_string(),
            status: status.to_string(),
            tx_hash: None,
            confirmations: 0,
            block_number: None,
            requested_by: "user-1".to_string(),
            company_id: None,
            submitted_at: None,
            confirmed_at: None,
            created_at,
            updated_at: created_at,
        }
    }

    #[test]
    fn retryable_relay_status_covers_server_errors_and_rate_limits() {
        assert!(is_retryable_relay_status(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(is_retryable_relay_status(
            reqwest::StatusCode::BAD_GATEWAY
        ));
        assert!(is_retryable_relay_status(
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        ));
        assert!(is_retryable_relay_status(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(!is_retryable_relay_status(
            reqwest::StatusCode::BAD_REQUEST
        ));
        assert!(!is_retryable_relay_status(
            reqwest::StatusCode::UNAUTHORIZED
        ));
        assert!(!is_retryable_relay_status(reqwest::StatusCode::NOT_FOUND));
        assert!(!is_retryable_relay_status(reqwest::StatusCode::OK));
    }

    #[test]
    fn verification_prefers_confirmed_over_newer_failed() {
        let base = now();
        let confirmed = test_anchor("confirmed", base);
        let failed = test_anchor("failed", base + chrono::Duration::seconds(60));
        let anchors = vec![failed.clone(), confirmed.clone()];
        let picked = select_anchor_for_verification(&anchors).unwrap();
        assert_eq!(picked.status, "confirmed");
        assert_eq!(picked.id, confirmed.id);
    }

    #[test]
    fn verification_prefers_newest_non_failed_without_confirmed() {
        let base = now();
        let pending = test_anchor("pending", base);
        let submitted = test_anchor("submitted", base + chrono::Duration::seconds(60));
        let failed = test_anchor("failed", base + chrono::Duration::seconds(120));
        let anchors = vec![pending, submitted.clone(), failed];
        let picked = select_anchor_for_verification(&anchors).unwrap();
        assert_eq!(picked.status, "submitted");
        assert_eq!(picked.id, submitted.id);
    }

    #[test]
    fn verification_falls_back_to_newest_when_all_failed() {
        let base = now();
        let older = test_anchor("failed", base);
        let newer = test_anchor("failed", base + chrono::Duration::seconds(60));
        let anchors = vec![older, newer.clone()];
        let picked = select_anchor_for_verification(&anchors).unwrap();
        assert_eq!(picked.id, newer.id);
        assert!(select_anchor_for_verification(&[]).is_none());
    }
}
