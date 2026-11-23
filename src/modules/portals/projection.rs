//! Portals - read-only projection from owning-module events.
//!
//! Part of the `portals` module split; see `src/modules/portals.rs`.

use unified_core::hooks::HookContext;
use unified_core::json;
use chrono::Utc;
use serde_json::json;

use super::constants::DOWNLOAD_FINISHED;
use super::constants::DOWNLOAD_STARTED;
use super::constants::INVOICE_CREATED;
use super::constants::INVOICE_PAID;
use super::constants::INVOICE_VOIDED;
use super::constants::LICENSE_ACTIVATED;
use super::constants::LICENSE_ISSUED;
use super::constants::LICENSE_REVOKED;
use super::constants::LICENSE_SUSPENDED;
use super::constants::ORDER_CANCELLED;
use super::constants::ORDER_COMPLETED;
use super::constants::ORDER_CREATED;
use super::constants::TICKET_CREATED;
use super::constants::TICKET_UPDATED;
use super::state::PortalsState;
use super::types::PortalRecord;

// ---------------------------------------------------------------------------
// Read-only projection (listeners on the owning modules' events)
// ---------------------------------------------------------------------------

/// Upsert one projection row from a hook payload. Payload shapes
/// differ per owning module, so the ids, amount, and status are
/// read defensively from the common key spellings; a payload with
/// no company id or no user id is skipped and reported, because
/// the portal can never scope such a record to a client.
fn upsert_projection(
    state: &PortalsState,
    ctx: &mut HookContext,
    kind: &str,
    source_module: &str,
    default_status: &str,
) {
    let id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("record_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("customer_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("customer_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("owner_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let (Some(id), Some(company_id), Some(user_id)) = (id, company_id, user_id) else {
        ctx.add_result(json!({
            "portals": "skipped",
            "kind": kind,
            "reason": "payload carries no id, company id, or user id, so the record cannot be scoped to a portal client",
        }));
        return;
    };
    let title = ctx
        .get("title")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("name")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("{kind} {id}"));
    let status = ctx
        .get("status")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| default_status.to_string());
    let amount = ctx
        .get("total_amount")
        .and_then(|value| value.as_f64())
        .or_else(|| ctx.get("amount").and_then(|value| value.as_f64()))
        .or_else(|| ctx.get("total").and_then(|value| value.as_f64()));
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let link = ctx
        .get("link")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let now = Utc::now();
    let key = format!("{kind}:{id}");
    let created_at = state
        .records
        .get(&key)
        .map(|entry| entry.value().created_at)
        .unwrap_or(now);
    let record = PortalRecord {
        id: id.clone(),
        kind: kind.to_string(),
        company_id,
        user_id,
        title,
        status,
        amount,
        currency,
        source_module: source_module.to_string(),
        link,
        created_at,
        updated_at: now,
    };
    state.records.insert(key, record);
    ctx.add_result(json!({
        "portals": "projected",
        "kind": kind,
        "id": id,
    }));
}

/// One listener shared by every projection event: the event name
/// decides the record kind, source module, and default status.
pub(crate) fn on_projection_event(state: PortalsState, event: &str, ctx: &mut HookContext) {
    let (kind, source_module, default_status) = match event {
        INVOICE_CREATED => ("invoice", "billing", "draft"),
        INVOICE_PAID => ("invoice", "billing", "paid"),
        INVOICE_VOIDED => ("invoice", "billing", "voided"),
        TICKET_CREATED => ("ticket", "tickets", "open"),
        TICKET_UPDATED => ("ticket", "tickets", "open"),
        ORDER_CREATED => ("subscription", "store", "created"),
        // A completed order completes the subscription row created
        // by `store.order.created` (same `subscription:<id>` key),
        // so the client's subscriptions list moves off "created".
        // A download row is only ever created from a real download
        // event (`download.started` / `download.finished` below),
        // never from an order event.
        ORDER_COMPLETED => ("subscription", "store", "completed"),
        ORDER_CANCELLED => ("subscription", "store", "cancelled"),
        LICENSE_ISSUED => ("license", "licensing", "active"),
        LICENSE_ACTIVATED => ("license", "licensing", "active"),
        LICENSE_SUSPENDED => ("license", "licensing", "suspended"),
        LICENSE_REVOKED => ("license", "licensing", "revoked"),
        DOWNLOAD_STARTED => ("download", "downloader", "downloading"),
        DOWNLOAD_FINISHED => ("download", "downloader", "finished"),
        _ => return,
    };
    upsert_projection(&state, ctx, kind, source_module, default_status);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order_payload(id: &str) -> HookContext {
        HookContext::new(
            "store.order.created",
            json!({
                "id": id,
                "company_id": "company-1",
                "user_id": "user-1",
                "title": "Order",
            }),
        )
    }

    #[test]
    fn completed_order_updates_subscription_row() {
        let state = PortalsState::default();
        let mut created = order_payload("ord-1");
        on_projection_event(state.clone(), ORDER_CREATED, &mut created);
        let mut completed = order_payload("ord-1");
        on_projection_event(state.clone(), ORDER_COMPLETED, &mut completed);

        // One subscription row, now completed; no stray download row
        // for the order id.
        let row = state
            .records
            .get("subscription:ord-1")
            .map(|entry| entry.value().clone())
            .expect("subscription row must exist");
        assert_eq!(row.kind, "subscription");
        assert_eq!(row.source_module, "store");
        assert_eq!(row.status, "completed");
        assert_eq!(state.records.len(), 1);
        assert!(state.records.get("download:ord-1").is_none());
    }

    #[test]
    fn completed_order_without_created_still_projects_subscription() {
        let state = PortalsState::default();
        let mut completed = order_payload("ord-2");
        on_projection_event(state.clone(), ORDER_COMPLETED, &mut completed);
        let row = state
            .records
            .get("subscription:ord-2")
            .map(|entry| entry.value().clone())
            .expect("subscription row must exist");
        assert_eq!(row.kind, "subscription");
        assert_eq!(row.status, "completed");
        assert!(state.records.get("download:ord-2").is_none());
    }

    #[test]
    fn cancelled_order_updates_subscription_row() {
        let state = PortalsState::default();
        let mut created = order_payload("ord-3");
        on_projection_event(state.clone(), ORDER_CREATED, &mut created);
        let mut cancelled = order_payload("ord-3");
        on_projection_event(state.clone(), ORDER_CANCELLED, &mut cancelled);
        let row = state
            .records
            .get("subscription:ord-3")
            .map(|entry| entry.value().clone())
            .expect("subscription row must exist");
        assert_eq!(row.status, "cancelled");
        assert_eq!(state.records.len(), 1);
    }

    #[test]
    fn real_download_events_still_project_download_rows() {
        let state = PortalsState::default();
        let mut started = HookContext::new(
            "download.started",
            json!({
                "id": "dl-1",
                "company_id": "company-1",
                "user_id": "user-1",
                "title": "File",
            }),
        );
        on_projection_event(state.clone(), DOWNLOAD_STARTED, &mut started);
        let mut finished = HookContext::new(
            "download.finished",
            json!({
                "id": "dl-1",
                "company_id": "company-1",
                "user_id": "user-1",
                "title": "File",
            }),
        );
        on_projection_event(state.clone(), DOWNLOAD_FINISHED, &mut finished);
        let row = state
            .records
            .get("download:dl-1")
            .map(|entry| entry.value().clone())
            .expect("download row must exist");
        assert_eq!(row.kind, "download");
        assert_eq!(row.source_module, "downloader");
        assert_eq!(row.status, "finished");
        assert_eq!(state.records.len(), 1);
    }
}
