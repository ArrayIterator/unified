// Store module - payment reference id settings.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::reference_id::{self, ReferenceIdSettings};
use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::digital::{require_digital_permission};
use super::helpers::{caller_company, require_caller_company, require_tenant};
use super::state::{StoreState};
use super::types::{OrderItem};

// Batch 18: payment reference ids (BEGIN - append-only section)
//
// Every order issued by this module carries one public payment
// reference in the single shared format (`PREFIX-12hex-12hex`, 30
// characters; see `unified_core::reference_id`). The internal `id`
// stays a UUID; the reference is what gateways see and reconcile
// against. The default prefix and the per-product prefix map are
// admin-editable settings held on `StoreState` (durable form: the
// `reference_id_settings` table in the migration snippet in the
// batch report). A single-product order uses that product's
// configured prefix; a mixed order uses the default prefix.
// References are issued once at checkout and never changed: the
// update-order endpoint deliberately has no reference field.

/// Capability needed to read the reference settings.
pub(crate) const PERM_REFERENCE_READ: &str = "store.reference:read";

/// Capability needed to change the reference settings.
pub(crate) const PERM_REFERENCE_MANAGE: &str = "store.reference:manage";

/// Snapshot of the current reference settings.
pub(crate) fn reference_settings(state: &StoreState) -> ReferenceIdSettings {
    state
        .reference_settings
        .read()
        .expect("reference settings lock poisoned")
        .clone()
}

/// Issue the reference for a checkout: the single product's
/// configured prefix when the order holds exactly one product,
/// otherwise the default prefix.
pub(crate) fn new_order_reference(state: &StoreState, items: &[OrderItem]) -> String {
    let settings = reference_settings(state);
    let product_id = if items.len() == 1 {
        Some(items[0].product_id.as_str())
    } else {
        None
    };
    reference_id::generate_for(&settings, product_id, None)
}

/// Admin input for `PUT /api/v1/store/reference-settings`. All fields
/// are optional and applied together: `default_prefix` replaces the
/// default prefix, `product_prefixes` replaces the whole per-product
/// map, and `remove_product_prefixes` deletes individual overrides.
/// Every prefix is normalized with `format_prefix` before it is
/// stored, so an administrator can send `hstg` and the stored value
/// is `HSTG`.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateReferenceSettings {
    pub default_prefix: Option<String>,
    pub product_prefixes: Option<HashMap<String, String>>,
    pub remove_product_prefixes: Option<Vec<String>>,
}

/// Read the reference settings (capability `store.reference:read`).
pub(crate) async fn get_reference_settings(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<ReferenceIdSettings> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = caller_company(&tenant, &req)?;
        require_digital_permission(&state, &req, PERM_REFERENCE_READ)?;
        Ok(reference_settings(&state))
    })();
    json::respond(result, false)
}

/// Update the reference settings (capability
/// `store.reference:manage`). Already-issued references keep
/// validating: changing a prefix never rewrites an issued id.
pub(crate) async fn update_reference_settings(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<UpdateReferenceSettings>,
) -> HttpResponse {
    let result: UnifiedResult<ReferenceIdSettings> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        require_digital_permission(&state, &req, PERM_REFERENCE_MANAGE)?;
        let input = body.into_inner();
        let mut candidate = reference_settings(&state);
        if let Some(prefix) = input.default_prefix {
            candidate.default_prefix = reference_id::format_prefix(&prefix);
        }
        if let Some(prefixes) = input.product_prefixes {
            let mut next = ReferenceIdSettings::default();
            next.default_prefix = candidate.default_prefix.clone();
            for (key, prefix) in prefixes {
                next.set_product_prefix(&key, &prefix).map_err(|problem| {
                    UnifiedError::bad_request(problem)
                })?;
            }
            candidate = next;
        }
        if let Some(keys) = input.remove_product_prefixes {
            for key in keys {
                candidate.remove_product_prefix(&key);
            }
        }
        let problems = candidate.validate();
        if !problems.is_empty() {
            return Err(UnifiedError::unprocessable(format!(
                "Reference settings are invalid: {}",
                problems.join("; ")
            )));
        }
        {
            let mut settings = state
                .reference_settings
                .write()
                .expect("reference settings lock poisoned");
            *settings = candidate.clone();
        }
        state.emit(
            reference_id::EVENT_REFERENCE_SETTINGS_CHANGED,
            serde_json::to_value(&candidate).unwrap_or(Value::Null),
        );
        Ok(candidate)
    })();
    json::respond(result, false)
}
