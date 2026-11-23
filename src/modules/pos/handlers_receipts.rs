// Pos module — receipt and product search handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;
use super::*;

pub(crate) async fn list_receipts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let params = query.into_inner();
        let register_filter = params.filters.get("register_id").cloned();
        let cashier_filter = params.filters.get("cashier_id").cloned();
        let mut items: Vec<PosReceipt> = state
            .receipts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|receipt| {
                // Receipts carry no company of their own; they are
                // scoped through the parent sale. A receipt whose
                // sale is missing is never listed (fail closed).
                let in_scope = state
                    .sales
                    .get(&receipt.sale_id)
                    .is_some_and(|sale| in_company_scope(&sale.company_id, &_scope));
                if !in_scope {
                    return false;
                }
                if let Some(register_id) = &register_filter {
                    if receipt.register_id != *register_id {
                        return false;
                    }
                }
                if let Some(cashier_id) = &cashier_filter {
                    if receipt.cashier_id != *cashier_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.issued_at.cmp(&a.issued_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_receipt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosReceipt> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let sale_id = path.into_inner();
        require_sale_in_scope(&state, &sale_id, &_scope)?;
        load_receipt_by_sale(&state, &sale_id)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_receipt_print(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let sale_id = path.into_inner();
        require_sale_in_scope(&state, &sale_id, &_scope)?;
        let receipt = load_receipt_by_sale(&state, &sale_id)?;
        Ok(json!({
            "receipt_number": receipt.number,
            "sale_number": receipt.sale_number,
            "sale_id": receipt.sale_id,
            "issued_at": receipt.issued_at,
            "currency": receipt.currency,
            "total": minor_to_major(receipt.total, &receipt.currency),
            "width_chars": state.settings().receipt_width_chars,
            "print_lines": receipt.print_lines,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Product search (hook-driven catalogue lookup)
// ---------------------------------------------------------------------------

pub(crate) async fn search_products(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<ProductSearchQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let input = query.into_inner();
        let has_query = input
            .q
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
            || input
                .barcode
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some()
            || input
                .sku
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some();
        if !has_query {
            return Err(UnifiedError::bad_request(
                "Provide q, barcode, or sku to search the catalogue",
            ));
        }
        // No catalogue handler is registered for
        // `pos.product.search_requested` anywhere in the repo, so
        // there is nothing to ask: the endpoint reports honestly
        // that product lookup is unavailable instead of emitting
        // an event nobody answers. (Follow-up: when a catalogue
        // integration exists, it should register a listener for
        // `pos.product.search_requested` and this handler should
        // emit the search and return the answered products.)
        Ok(catalogue_unavailable_response())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// The honest answer when no catalogue can be searched: no
/// products, and a note saying product lookup is pending a
/// catalogue integration — never invented catalogue rows.
pub(crate) fn catalogue_unavailable_response() -> Value {
    json!({
        "products": [],
        "catalogue_available": false,
        "note": "No catalogue handler is registered for pos.product.search_requested; \
                 product lookup is pending a catalogue integration and returns no products.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_unavailable_response_never_invents_products() {
        // Regression (O7): the search used to emit
        // `pos.product.search_requested`, an event no module in
        // the repo listens to. The endpoint now answers honestly
        // that lookup is unavailable, with no products.
        let response = catalogue_unavailable_response();
        assert_eq!(response.get("products"), Some(&Value::Array(Vec::new())));
        assert_eq!(
            response.get("catalogue_available"),
            Some(&Value::Bool(false))
        );
        let note = response
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(note.contains("pos.product.search_requested"));
        assert!(note.contains("pending"));
    }
}
