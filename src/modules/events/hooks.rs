// Events Manager — Hook-bus listeners.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_events::*;
use super::handlers_tickets::*;
use super::handlers_registrations::*;
use super::handlers_reminders::*;
use super::handlers_public::*;
use super::module::*;

/// Answer `events.reminders.send_requested`: the event the
/// scheduler emits when an `events.reminders.send` job fires.
/// Sends the named reminder; an unknown reminder id is ignored so
/// a stale job can never send for a deleted reminder.
pub(crate) fn on_reminders_send_requested(state: EventsState, ctx: &mut HookContext) {
    let reminder_id = ctx
        .data
        .get("reminder_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(reminder_id) = reminder_id else {
        ctx.add_result(json!({ "sent": false, "error": "events.reminders.send_requested requires 'reminder_id'" }));
        return;
    };
    let Some(reminder) = state
        .reminders
        .get(&reminder_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "sent": false, "reminder_id": reminder_id.to_string(), "error": "Reminder not found" }));
        return;
    };
    if reminder.status != "pending" {
        ctx.add_result(json!({
            "sent": false,
            "reminder_id": reminder_id.to_string(),
            "reason": format!("Reminder is {}", reminder.status),
        }));
        return;
    }
    match send_reminder(&state, &reminder) {
        Ok(sent) => ctx.add_result(json!({
            "sent": true,
            "reminder_id": reminder_id.to_string(),
            "recipients": sent,
        })),
        Err(error) => ctx.add_result(json!({
            "sent": false,
            "reminder_id": reminder_id.to_string(),
            "error": error.message,
        })),
    }
}

// ---------------------------------------------------------------------------
// Payment listeners (mirror the store module)
// ---------------------------------------------------------------------------

/// Confirm a paid registration on `payment.captured`, mirroring
/// `store.rs` `on_payment_captured`:
///
/// - The registration is matched by `registration_id` first, then
///   by `order_id` (against the registration's stored `order_id`),
///   then by `order.id` (the store module's nested shape).
/// - An explicit non-`captured` status is never treated as a
///   capture; a missing status means the event name is the claim.
/// - Only `pending_payment` / `pending_verification` may move to
///   confirmed — never `cancelled`; an already-confirmed
///   registration is a safe no-op, so redelivered captures are
///   safe.
/// - Amount check: for gateway payments the captured amount
///   (`amount_minor`, in the smallest currency unit) must cover
///   the registration total, so a partial capture never confirms
///   the whole registration. Bank-transfer captures skip the
///   check: the administrator already verified the amount when
///   approving the transfer. A currency mismatch is never applied.
/// - The payment reference (provider transaction id) is recorded
///   on the registration, and `events.registration.confirmed` is
///   emitted so other modules react exactly as they do for a free
///   confirmation.
pub(crate) fn on_payment_captured(state: Arc<EventsState>, ctx: &mut HookContext) {
    let registration_id = ctx
        .get("registration_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .and_then(|order_id| {
                    state
                        .registrations
                        .iter()
                        .find(|entry| entry.value().order_id.as_deref() == Some(order_id))
                        .map(|entry| entry.value().id.to_string())
                })
        })
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .and_then(|order_id| {
                    state
                        .registrations
                        .iter()
                        .find(|entry| entry.value().order_id.as_deref() == Some(order_id))
                        .map(|entry| entry.value().id.to_string())
                })
        });
    let Some(registration_id) = registration_id else {
        ctx.add_result(json!({
            "events": "skipped",
            "reason": "payment.captured carries no registration id or matching order id",
        }));
        return;
    };
    if let Some(status) = ctx.get("status").and_then(|value| value.as_str()) {
        if status != "captured" {
            ctx.add_result(json!({
                "events": "skipped",
                "registration_id": registration_id,
                "reason": format!("payment.captured carries status \"{status}\", not \"captured\""),
            }));
            return;
        }
    }
    let provider = ctx
        .get("provider")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let reference = ctx
        .get("provider_transaction_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("transaction_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let amount_minor = ctx.get("amount_minor").and_then(|value| value.as_i64());
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Ok(id) = Uuid::parse_str(&registration_id) else {
        ctx.add_result(json!({ "events": "skipped", "reason": "Invalid registration id" }));
        return;
    };
    let Some(registration) = state
        .registrations
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "events": "skipped",
            "registration_id": registration_id,
            "reason": "registration not found",
        }));
        return;
    };
    match registration.status {
        RegistrationStatus::Confirmed => {
            ctx.add_result(json!({
                "events": "already_confirmed",
                "registration_id": registration_id,
            }));
            return;
        }
        RegistrationStatus::Cancelled | RegistrationStatus::Waitlisted => {
            ctx.add_result(json!({
                "events": "skipped",
                "registration_id": registration_id,
                "reason": format!("registration is {} and can never move to confirmed", registration.status.as_str()),
            }));
            return;
        }
        RegistrationStatus::PendingPayment | RegistrationStatus::PendingVerification => {}
    }
    if provider.as_deref() != Some("bank_transfer") {
        if let (Some(amount), Some(event_currency)) = (amount_minor, &currency) {
            if !event_currency.eq_ignore_ascii_case(&registration.currency) {
                ctx.add_result(json!({
                    "events": "skipped",
                    "registration_id": registration_id,
                    "reason": format!(
                        "captured currency {event_currency} does not match registration currency {}",
                        registration.currency
                    ),
                }));
                return;
            }
            if amount < registration.total_amount {
                ctx.add_result(json!({
                    "events": "skipped",
                    "registration_id": registration_id,
                    "reason": "captured amount does not cover the registration total (partial capture or split part)",
                }));
                return;
            }
        }
    }
    if let Some(mut stored) = state.registrations.get_mut(&id) {
        stored.status = RegistrationStatus::Confirmed;
        if let Some(reference) = &reference {
            stored.payment_reference = Some(reference.clone());
        }
        if let Some(provider) = &provider {
            stored.payment_provider = Some(provider.clone());
        }
        stored.updated_at = Utc::now();
    }
    if let Some(mut ticket) = state.ticket_types.get_mut(&registration.ticket_type_id) {
        ticket.sold += registration.quantity;
        ticket.updated_at = Utc::now();
    }
    let event_title = state
        .events
        .get(&registration.event_id)
        .map(|entry| entry.value().title.clone())
        .unwrap_or_default();
    state.emit(
        "events.registration.confirmed",
        json!({
            "event_id": registration.event_id.to_string(),
            "registration_id": registration.id.to_string(),
            "user_id": registration.user_id,
            "payment_reference": reference,
            "provider": provider,
        }),
    );
    state.notify(
        "event.registration.confirmed",
        &registration.user_id,
        &format!("Registration confirmed: {event_title}"),
        &format!(
            "Your registration for \"{event_title}\" is confirmed. Check-in code: {}.",
            registration.check_in_code
        ),
        Some(format!("/api/v1/events/{}", registration.event_id)),
    );
    ctx.add_result(json!({
        "events": "confirmed",
        "registration_id": registration_id,
        "payment_reference": reference,
    }));
}

/// A bank-transfer verification opened for a registration moves it
/// from `pending_payment` to `pending_verification`, mirroring the
/// store module's `on_bank_transfer_verification_created`.
pub(crate) fn on_bank_transfer_verification_created(state: Arc<EventsState>, ctx: &mut HookContext) {
    let registration_id = ctx
        .get("registration_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .and_then(|order_id| {
                    state
                        .registrations
                        .iter()
                        .find(|entry| entry.value().order_id.as_deref() == Some(order_id))
                        .map(|entry| entry.value().id.to_string())
                })
        });
    let Some(registration_id) = registration_id else {
        ctx.add_result(json!({
            "events": "skipped",
            "reason": "bank_transfer.verification.created carries no registration id or matching order id",
        }));
        return;
    };
    let Ok(id) = Uuid::parse_str(&registration_id) else {
        ctx.add_result(json!({ "events": "skipped", "reason": "Invalid registration id" }));
        return;
    };
    let Some(registration) = state
        .registrations
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "events": "skipped",
            "registration_id": registration_id,
            "reason": "registration not found",
        }));
        return;
    };
    if registration.status == RegistrationStatus::PendingVerification {
        ctx.add_result(json!({
            "events": "already_pending_verification",
            "registration_id": registration_id,
        }));
        return;
    }
    if registration.status != RegistrationStatus::PendingPayment {
        ctx.add_result(json!({
            "events": "skipped",
            "registration_id": registration_id,
            "reason": format!("registration is {} and cannot move to pending_verification", registration.status.as_str()),
        }));
        return;
    }
    if let Some(mut stored) = state.registrations.get_mut(&id) {
        stored.status = RegistrationStatus::PendingVerification;
        stored.updated_at = Utc::now();
    }
    state.emit(
        "events.registration.pending_verification",
        json!({
            "event_id": registration.event_id.to_string(),
            "registration_id": registration.id.to_string(),
            "user_id": registration.user_id,
        }),
    );
    ctx.add_result(json!({
        "events": "pending_verification",
        "registration_id": registration_id,
    }));
}

/// A rejected bank transfer cancels the registration, mirroring the
/// store module's `on_bank_transfer_rejected`. The freed seats are
/// offered to the waitlist.
pub(crate) fn on_bank_transfer_rejected(state: Arc<EventsState>, ctx: &mut HookContext) {
    let registration_id = ctx
        .get("registration_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .and_then(|order_id| {
                    state
                        .registrations
                        .iter()
                        .find(|entry| entry.value().order_id.as_deref() == Some(order_id))
                        .map(|entry| entry.value().id.to_string())
                })
        });
    let Some(registration_id) = registration_id else {
        ctx.add_result(json!({
            "events": "skipped",
            "reason": "bank_transfer.rejected carries no registration id or matching order id",
        }));
        return;
    };
    let Ok(id) = Uuid::parse_str(&registration_id) else {
        ctx.add_result(json!({ "events": "skipped", "reason": "Invalid registration id" }));
        return;
    };
    let Some(registration) = state
        .registrations
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "events": "skipped",
            "registration_id": registration_id,
            "reason": "registration not found",
        }));
        return;
    };
    if registration.status == RegistrationStatus::Cancelled {
        ctx.add_result(json!({
            "events": "already_cancelled",
            "registration_id": registration_id,
        }));
        return;
    }
    if !matches!(
        registration.status,
        RegistrationStatus::PendingPayment | RegistrationStatus::PendingVerification
    ) {
        ctx.add_result(json!({
            "events": "skipped",
            "registration_id": registration_id,
            "reason": format!("registration is {} and cannot be cancelled by a rejected transfer", registration.status.as_str()),
        }));
        return;
    }
    cancel_registration_inner(&state, &registration, Some("Bank transfer rejected"));
    ctx.add_result(json!({
        "events": "cancelled",
        "registration_id": registration_id,
    }));
}
