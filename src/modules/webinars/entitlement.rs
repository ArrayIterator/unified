// webinars -- Paid-webinar entitlement checks.

use unified_core::error::{UnifiedError, UnifiedResult};
use serde_json::{Value, json};

use super::helpers::*;
use super::state::*;
use super::types::*;

// Entitlement (paid webinars)

/// Check, at join time, whether `user_id` may enter a paid webinar.
///
/// Free webinars (`product_id` is `None`) skip the check entirely.
/// The host, co-hosts, panelists, and callers with the admin
/// capability skip it too. Otherwise the store / product module is
/// asked through the hook event `product.entitlement.check` with
/// payload `{"product_id": ..., "user_id": ...}`, and the first hook
/// result must carry `{"entitled": true}`.
///
/// Fail-closed on purpose: when no handler answers the event (no
/// product module registered yet, or the store has not wired the
/// check), joining a paid webinar answers 503 instead of letting
/// someone in for free. An explicit `{"entitled": false}` (or an
/// `error` in the result) answers 403 / 502.
pub(crate) fn check_entitlement(
    state: &WebinarsState,
    webinar: &Webinar,
    user_id: &str,
    roles: &[String],
) -> UnifiedResult<()> {
    let Some(product_id) = &webinar.product_id else {
        return Ok(());
    };
    if is_staff(webinar, user_id, roles) {
        return Ok(());
    }
    let Some(bus) = state.bus() else {
        return Err(UnifiedError::new(
            "Entitlement service is unavailable: no hook bus is attached",
            503,
        ));
    };
    let answer = bus.emit(
        "product.entitlement.check",
        json!({ "product_id": product_id, "user_id": user_id }),
    );
    match answer.results.first() {
        Some(result) if result.get("entitled").and_then(Value::as_bool) == Some(true) => Ok(()),
        Some(result) if result.get("error").is_some() => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("entitlement check failed");
            Err(UnifiedError::new(
                format!("Entitlement check failed: {reason}"),
                502,
            ))
        }
        Some(_) => Err(UnifiedError::forbidden(
            "A paid product entitlement is required to join this webinar",
        )),
        None => Err(UnifiedError::new(
            "Entitlement service did not answer product.entitlement.check: no handler is registered",
            503,
        )),
    }
}
