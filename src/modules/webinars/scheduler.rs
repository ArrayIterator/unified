// webinars -- Scheduler linkage and hook handlers.

use unified_core::hooks::HookContext;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use super::helpers::*;
use super::state::*;
use super::types::*;

// Scheduler linkage

/// Link a scheduled webinar to the scheduler module.
///
/// The scheduler listens to the `scheduler.enqueue` event with the
/// shape `{"kind", "payload"}`. A webinar start is queued as kind
/// `webinar.start` and a reminder as kind `webinar.reminder`, each
/// with the webinar id, start time, and host id in the payload. The
/// result is read back from the first hook result:
/// `{"queued": true, "job_id": ...}` means the scheduler accepted
/// the job.
///
/// CURRENT LIMIT, stated plainly: the scheduler's known-kinds list
/// may not contain a webinar kind yet, so today the scheduler can
/// answer `{"queued": false, "error": "Unknown job kind: ..."}`
/// and this function records `declined` instead of pretending the
/// job was scheduled. The webinar is still created and stays
/// usable; the follow-up is to add `webinar.start` and
/// `webinar.reminder` kinds to the scheduler (which would then emit
/// `webinar.start_requested` / `webinar.reminder_requested`, the
/// events this module already listens to below). Webinars without
/// `starts_at` are never queued and keep `not_scheduled`.
///
/// Migration: `migrations/2026-11-22-000077_webinars` (000071 through
/// 000076 were already taken by other rounds when this module was
/// written; 000074 exists twice and 000075/000076 belong to sibling
/// batch-10 builders).
pub(crate) fn schedule_webinar(state: &WebinarsState, webinar: &mut Webinar) {
    let config = state.config();
    let Some(starts_at) = webinar.starts_at else {
        webinar.scheduler_job_id = None;
        webinar.scheduler_status = "not_scheduled".to_string();
        webinar.reminder_scheduler_status = "not_scheduled".to_string();
        return;
    };
    let Some(bus) = state.bus() else {
        webinar.scheduler_job_id = None;
        webinar.scheduler_status = "declined".to_string();
        webinar.reminder_scheduler_status = "declined".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "webinar.start",
            "payload": {
                "webinar_id": webinar.id.to_string(),
                "starts_at": starts_at.to_rfc3339(),
                "host_id": webinar.host_id,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            webinar.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            webinar.scheduler_status = "queued".to_string();
        }
        _ => {
            webinar.scheduler_job_id = None;
            webinar.scheduler_status = "declined".to_string();
        }
    }
    let reminder_at = starts_at - chrono::Duration::minutes(config.reminder_lead_mins as i64);
    let reminder = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "webinar.reminder",
            "payload": {
                "webinar_id": webinar.id.to_string(),
                "starts_at": starts_at.to_rfc3339(),
                "remind_at": reminder_at.to_rfc3339(),
                "host_id": webinar.host_id,
            },
        }),
    );
    webinar.reminder_scheduler_status = match reminder.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            "queued".to_string()
        }
        _ => "declined".to_string(),
    };
}

/// Promote waitlisted registrations into free seats, earliest
/// registration first. Called after a seat frees up (a registered
/// attendee cancels, or capacity grows). Each promotion is announced
/// with `webinar.registration.promoted` and a notification to the
/// promoted user. Returns the number of promotions made.
pub(crate) fn promote_waitlist(state: &WebinarsState, webinar: &Webinar) -> usize {
    let config = state.config();
    if !config.auto_promote_waitlist {
        return 0;
    }
    // Hold this webinar's seat lock for the whole promotion loop.
    // Without it, two concurrent runs (or a run racing a manual
    // approve/promote) could each observe a free seat and promote a
    // different waitlisted row, overfilling the webinar. The guard
    // is released when this function returns; nothing awaited runs
    // while it is held. No caller holds this lock when calling here,
    // so taking it cannot deadlock.
    let seat_lock = state.registration_lock(webinar.id);
    let _seat_guard = seat_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut promoted = 0usize;
    loop {
        if registered_count(state, webinar.id) >= webinar.capacity {
            break;
        }
        let mut waiting: Vec<WebinarRegistration> = state
            .registrations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id && row.status == "waitlisted")
            .collect();
        // A tie on registered_at must not promote arbitrarily:
        // DashMap iteration order is nondeterministic, so equal
        // timestamps fall back to created_at and then the
        // registration id for a stable, repeatable order.
        waiting.sort_by(|a, b| {
            a.registered_at
                .cmp(&b.registered_at)
                .then_with(|| a.created_at.cmp(&b.created_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        let Some(next) = waiting.into_iter().next() else {
            break;
        };
        let now = Utc::now();
        let mut row = next.clone();
        row.status = "registered".to_string();
        row.promoted_at = Some(now);
        row.updated_at = now;
        state.registrations.insert(row.id, row.clone());
        promoted += 1;
        state.emit(
            "webinar.registration.promoted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "registration_id": row.id.to_string(),
                "user_id": row.user_id,
                "promoted_at": now.to_rfc3339(),
            }),
        );
        state.notify(
            &row.user_id,
            &format!("A seat opened up: {}", webinar.title),
            "You have been promoted from the waitlist and now hold a seat.",
            Some(format!("/api/v1/webinars/{}", webinar.id)),
        );
    }
    promoted
}

/// Close every open attendance session of one webinar at `at`
/// (used when the webinar ends while people are still in it).
pub(crate) fn close_open_sessions(state: &WebinarsState, webinar_id: Uuid, at: DateTime<Utc>) {
    let open: Vec<Uuid> = state
        .attendance
        .iter()
        .filter(|entry| entry.value().webinar_id == webinar_id && entry.value().left_at.is_none())
        .map(|entry| *entry.key())
        .collect();
    for id in open {
        if let Some(mut session) = state.attendance.get_mut(&id) {
            session.left_at = Some(at);
            session.updated_at = at;
        }
    }
}

// Scheduler / order hook handlers

/// Answer `webinar.start_requested`: the event the scheduler emits
/// when a `webinar.start` job fires. A scheduled webinar whose start
/// time has arrived goes live; any other state is left alone, and an
/// unknown webinar id is ignored so a stale job can never resurrect
/// a deleted webinar.
pub(crate) fn on_webinar_start_requested(state: WebinarsState, ctx: &mut HookContext) {
    let webinar_id = ctx
        .data
        .get("webinar_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(webinar_id) = webinar_id else {
        ctx.add_result(
            json!({ "started": false, "error": "webinar.start_requested requires 'webinar_id'" }),
        );
        return;
    };
    let Some(mut entry) = state.webinars.get_mut(&webinar_id) else {
        ctx.add_result(json!({ "started": false, "webinar_id": webinar_id.to_string(), "error": "Webinar not found" }));
        return;
    };
    if entry.status != WebinarStatus::Scheduled {
        ctx.add_result(json!({
            "started": false,
            "webinar_id": webinar_id.to_string(),
            "reason": "Webinar is not scheduled",
        }));
        return;
    }
    entry.status = WebinarStatus::Live;
    entry.started_at = Some(Utc::now());
    entry.updated_at = Utc::now();
    if entry.meeting_room_id.is_some() {
        entry.meeting_link_status = "live".to_string();
    }
    let webinar = entry.clone();
    drop(entry);
    // Like the manual start path, ask the linked meetings room to
    // go live as well. Without this a scheduler-started webinar's
    // room is never asked to go live.
    if let Some(room_id) = webinar.meeting_room_id {
        state.emit(
            "meeting.start_requested",
            json!({
                "room_id": room_id.to_string(),
                "meeting_id": room_id.to_string(),
                "webinar_id": webinar.id.to_string(),
            }),
        );
    }
    state.emit(
        "webinar.started",
        json!({
            "webinar_id": webinar.id.to_string(),
            "host_id": webinar.host_id,
            "started_at": webinar.started_at,
            "meeting_room_id": webinar.meeting_room_id.map(|id| id.to_string()),
            "source": "scheduler",
        }),
    );
    ctx.add_result(json!({ "started": true, "webinar_id": webinar_id.to_string() }));
}

/// Answer `webinar.reminder_requested`: the event the scheduler
/// emits when a `webinar.reminder` job fires. Every registered
/// attendee gets a `notification.requested` reminder; the result
/// reports how many were notified.
pub(crate) fn on_webinar_reminder_requested(state: WebinarsState, ctx: &mut HookContext) {
    let webinar_id = ctx
        .data
        .get("webinar_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(webinar_id) = webinar_id else {
        ctx.add_result(
            json!({ "reminded": 0, "error": "webinar.reminder_requested requires 'webinar_id'" }),
        );
        return;
    };
    let Some(webinar) = state
        .webinars
        .get(&webinar_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "reminded": 0, "webinar_id": webinar_id.to_string(), "error": "Webinar not found" }));
        return;
    };
    if webinar.status != WebinarStatus::Scheduled && webinar.status != WebinarStatus::Live {
        ctx.add_result(json!({
            "reminded": 0,
            "webinar_id": webinar_id.to_string(),
            "reason": "Webinar is not scheduled or live",
        }));
        return;
    }
    let mut reminded = 0u64;
    for entry in state.registrations.iter() {
        let row = entry.value();
        if row.webinar_id == webinar_id && row.status == "registered" {
            state.notify(
                &row.user_id,
                &format!("Reminder: {}", webinar.title),
                "Your webinar starts soon.",
                Some(format!("/api/v1/webinars/{}", webinar.id)),
            );
            reminded += 1;
        }
    }
    ctx.add_result(json!({ "reminded": reminded, "webinar_id": webinar_id.to_string() }));
}

/// Answer `order.paid` / `store.order.completed`: a paid order for
/// a webinar's product marks the matching registration paid. The
/// payload shape is read defensively (`product_id` plus `user_id`,
/// `buyer_id`, or `customer_id`), because the store module owns the
/// order shape; rows that do not match are ignored.
pub(crate) fn on_order_paid(state: WebinarsState, ctx: &mut HookContext) {
    let product_id = ctx
        .data
        .get("product_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let user_id = ["user_id", "buyer_id", "customer_id"]
        .iter()
        .find_map(|key| ctx.data.get(*key).and_then(|value| value.as_str()))
        .map(str::to_string);
    let (Some(product_id), Some(user_id)) = (product_id, user_id) else {
        ctx.add_result(json!({ "marked_paid": 0, "error": "order event requires 'product_id' and a buyer id" }));
        return;
    };
    let mut marked = 0u64;
    let matches: Vec<Uuid> = state
        .registrations
        .iter()
        .filter(|entry| {
            let row = entry.value();
            // Only an open registration can be marked paid. A
            // cancelled (or denied) registration must never flip
            // to paid just because its payment was still pending.
            row.user_id == user_id
                && row.payment_status == "pending"
                && matches!(
                    row.status.as_str(),
                    "registered" | "waitlisted" | "pending_approval"
                )
                && state
                    .webinars
                    .get(&row.webinar_id)
                    .is_some_and(|w| w.product_id.as_deref() == Some(product_id.as_str()))
        })
        .map(|entry| *entry.key())
        .collect();
    for id in matches {
        if let Some(mut row) = state.registrations.get_mut(&id) {
            row.payment_status = "paid".to_string();
            row.updated_at = Utc::now();
            marked += 1;
        }
    }
    ctx.add_result(json!({ "marked_paid": marked, "user_id": user_id, "product_id": product_id }));
}

// Tests for the scheduler hook handlers: the scheduler start path
// must notify the linked meetings room, paid orders must not revive
// cancelled registrations, and waitlist promotion must be stable.
#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::{HookBus, HookContext};
    use std::sync::{Arc, Mutex};

    fn test_webinar(
        status: WebinarStatus,
        meeting_room_id: Option<Uuid>,
        product_id: Option<String>,
        capacity: usize,
    ) -> Webinar {
        let now = Utc::now();
        Webinar {
            id: Uuid::new_v4(),
            title: "Test webinar".to_string(),
            description: None,
            host_id: "host-1".to_string(),
            co_host_ids: Vec::new(),
            panelist_ids: Vec::new(),
            status,
            access_model: WebinarAccessModel::Paid,
            product_id,
            capacity,
            requires_approval: false,
            allow_waitlist: true,
            chat_enabled: true,
            qa_enabled: true,
            polls_enabled: true,
            recording_enabled: true,
            meeting_room_id,
            meeting_link_status: "linked".to_string(),
            chat_channel_id: None,
            survey_id: None,
            survey_distribution_id: None,
            starts_at: Some(now + chrono::Duration::hours(1)),
            ends_at: None,
            timezone: "UTC".to_string(),
            recurrence_rule: None,
            scheduler_job_id: None,
            scheduler_status: "queued".to_string(),
            reminder_scheduler_status: "queued".to_string(),
            started_at: None,
            ended_at: None,
            follow_up_sent_at: None,
            follow_up_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_registration(
        id: Uuid,
        webinar_id: Uuid,
        user_id: &str,
        status: &str,
        at: DateTime<Utc>,
    ) -> WebinarRegistration {
        WebinarRegistration {
            id,
            webinar_id,
            user_id: user_id.to_string(),
            status: status.to_string(),
            payment_status: "pending".to_string(),
            registered_at: at,
            approved_at: None,
            cancelled_at: None,
            promoted_at: None,
            created_at: at,
            updated_at: at,
        }
    }

    /// A state with a bus that records every emitted event name.
    fn state_with_event_tap() -> (WebinarsState, Arc<Mutex<Vec<String>>>) {
        let state = WebinarsState::default();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let bus = HookBus::new();
        for event in ["meeting.start_requested", "webinar.started"] {
            let seen = Arc::clone(&seen);
            bus.register(
                event,
                "test",
                Arc::new(move |ctx: &mut HookContext| {
                    seen.lock().expect("event tap lock poisoned").push(ctx.event.clone());
                }),
            );
        }
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::new(bus));
        (state, seen)
    }

    #[test]
    fn scheduler_start_notifies_linked_meeting_room() {
        let (state, seen) = state_with_event_tap();
        let room_id = Uuid::new_v4();
        let webinar = test_webinar(WebinarStatus::Scheduled, Some(room_id), None, 10);
        let webinar_id = webinar.id;
        state.webinars.insert(webinar_id, webinar);
        let mut ctx = HookContext::new(
            "webinar.start_requested",
            json!({ "webinar_id": webinar_id.to_string() }),
        );
        on_webinar_start_requested(state.clone(), &mut ctx);
        assert_eq!(ctx.results[0].get("started").and_then(Value::as_bool), Some(true));
        let seen = seen.lock().expect("event tap lock poisoned");
        assert!(seen.contains(&"meeting.start_requested".to_string()));
        assert!(seen.contains(&"webinar.started".to_string()));
    }

    #[test]
    fn scheduler_start_without_room_emits_no_meeting_event() {
        let (state, seen) = state_with_event_tap();
        let webinar = test_webinar(WebinarStatus::Scheduled, None, None, 10);
        let webinar_id = webinar.id;
        state.webinars.insert(webinar_id, webinar);
        let mut ctx = HookContext::new(
            "webinar.start_requested",
            json!({ "webinar_id": webinar_id.to_string() }),
        );
        on_webinar_start_requested(state.clone(), &mut ctx);
        assert_eq!(ctx.results[0].get("started").and_then(Value::as_bool), Some(true));
        let seen = seen.lock().expect("event tap lock poisoned");
        assert!(!seen.contains(&"meeting.start_requested".to_string()));
        assert!(seen.contains(&"webinar.started".to_string()));
    }

    #[test]
    fn order_paid_skips_cancelled_and_denied_registrations() {
        let state = WebinarsState::default();
        let webinar = test_webinar(
            WebinarStatus::Scheduled,
            None,
            Some("prod-1".to_string()),
            10,
        );
        let webinar_id = webinar.id;
        state.webinars.insert(webinar_id, webinar);
        let at = Utc::now();
        let cancelled_id = Uuid::new_v4();
        let denied_id = Uuid::new_v4();
        let registered_id = Uuid::new_v4();
        let waitlisted_id = Uuid::new_v4();
        for (id, status) in [
            (cancelled_id, "cancelled"),
            (denied_id, "denied"),
            (registered_id, "registered"),
            (waitlisted_id, "waitlisted"),
        ] {
            state.registrations.insert(
                id,
                test_registration(id, webinar_id, "buyer-1", status, at),
            );
        }
        let mut ctx = HookContext::new(
            "order.paid",
            json!({ "product_id": "prod-1", "user_id": "buyer-1" }),
        );
        on_order_paid(state.clone(), &mut ctx);
        assert_eq!(ctx.results[0].get("marked_paid").and_then(Value::as_u64), Some(2));
        assert_eq!(state.registrations.get(&cancelled_id).unwrap().payment_status, "pending");
        assert_eq!(state.registrations.get(&denied_id).unwrap().payment_status, "pending");
        assert_eq!(state.registrations.get(&registered_id).unwrap().payment_status, "paid");
        assert_eq!(state.registrations.get(&waitlisted_id).unwrap().payment_status, "paid");
    }

    #[test]
    fn waitlist_promotion_breaks_timestamp_ties_deterministically() {
        // Two waitlisted rows with identical registered_at and
        // created_at: the lower registration id must win, every
        // time, regardless of DashMap iteration order.
        let first_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let second_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let at = Utc::now();
        for _ in 0..5 {
            let state = WebinarsState::default();
            let webinar = test_webinar(WebinarStatus::Scheduled, None, None, 1);
            let webinar_id = webinar.id;
            state.webinars.insert(webinar_id, webinar.clone());
            // Insert in reverse id order so iteration order cannot
            // be what decides the outcome.
            state.registrations.insert(
                second_id,
                test_registration(second_id, webinar_id, "user-2", "waitlisted", at),
            );
            state.registrations.insert(
                first_id,
                test_registration(first_id, webinar_id, "user-1", "waitlisted", at),
            );
            assert_eq!(promote_waitlist(&state, &webinar), 1);
            assert_eq!(state.registrations.get(&first_id).unwrap().status, "registered");
            assert_eq!(state.registrations.get(&second_id).unwrap().status, "waitlisted");
        }
    }

    #[test]
    fn waitlist_promotion_still_prefers_earliest_registration() {
        let state = WebinarsState::default();
        let webinar = test_webinar(WebinarStatus::Scheduled, None, None, 1);
        let webinar_id = webinar.id;
        state.webinars.insert(webinar_id, webinar.clone());
        let early = Utc::now() - chrono::Duration::hours(2);
        let late = Utc::now();
        let late_id = Uuid::new_v4();
        let early_id = Uuid::new_v4();
        state.registrations.insert(
            late_id,
            test_registration(late_id, webinar_id, "user-late", "waitlisted", late),
        );
        state.registrations.insert(
            early_id,
            test_registration(early_id, webinar_id, "user-early", "waitlisted", early),
        );
        assert_eq!(promote_waitlist(&state, &webinar), 1);
        assert_eq!(state.registrations.get(&early_id).unwrap().status, "registered");
        assert_eq!(state.registrations.get(&late_id).unwrap().status, "waitlisted");
    }

    #[test]
    fn concurrent_promote_waitlist_runs_cannot_overfill_capacity() {
        // Eight threads run promote_waitlist at once on a webinar
        // with one free seat and two waitlisted rows. Each run
        // holds the per-webinar seat lock for its whole loop, so
        // the runs serialize and only one promotion happens in
        // total; without the lock, two runs could each observe the
        // free seat and promote a different row.
        let state = WebinarsState::default();
        let webinar = test_webinar(WebinarStatus::Scheduled, None, None, 1);
        let webinar_id = webinar.id;
        state.webinars.insert(webinar_id, webinar.clone());
        let at = Utc::now();
        for (user, offset_secs) in [("user-1", 0), ("user-2", 1)] {
            let id = Uuid::new_v4();
            let row_at = at + chrono::Duration::seconds(offset_secs);
            state.registrations.insert(
                id,
                test_registration(id, webinar_id, user, "waitlisted", row_at),
            );
        }
        let total_promoted: usize = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                let state = &state;
                let webinar = &webinar;
                handles.push(scope.spawn(move || promote_waitlist(state, webinar)));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("promote_waitlist thread panicked"))
                .sum()
        });
        assert_eq!(total_promoted, 1);
        assert_eq!(registered_count(&state, webinar_id), 1);
    }
}
