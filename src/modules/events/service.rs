// Events Manager - Business logic shared by handlers and hooks.

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
use super::handlers_events::*;
use super::handlers_tickets::*;
use super::handlers_registrations::*;
use super::handlers_reminders::*;
use super::handlers_public::*;
use super::hooks::*;
use super::module::*;

pub(crate) fn load_event(state: &EventsState, raw_id: &str) -> UnifiedResult<Event> {
    let id = parse_uuid(raw_id, "event")?;
    state
        .events
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Event not found: {id}")))
}

pub(crate) fn load_event_by_slug(state: &EventsState, slug: &str) -> UnifiedResult<Event> {
    state
        .events
        .iter()
        .map(|entry| entry.value().clone())
        .find(|event| event.slug == slug)
        .ok_or_else(|| UnifiedError::not_found(format!("Event not found: {slug}")))
}

pub(crate) fn load_ticket_type(state: &EventsState, event_id: Uuid, raw_id: &str) -> UnifiedResult<TicketType> {
    let id = parse_uuid(raw_id, "ticket type")?;
    state
        .ticket_types
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|ticket| ticket.event_id == event_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Ticket type not found: {id}")))
}

pub(crate) fn load_registration(state: &EventsState, raw_id: &str) -> UnifiedResult<Registration> {
    let id = parse_uuid(raw_id, "registration")?;
    state
        .registrations
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Registration not found: {id}")))
}

pub(crate) fn load_occurrence(state: &EventsState, event_id: Uuid, raw_id: &str) -> UnifiedResult<EventOccurrence> {
    let id = parse_uuid(raw_id, "occurrence")?;
    state
        .occurrences
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|occurrence| occurrence.event_id == event_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Occurrence not found: {id}")))
}

/// Effective status at read time: a published event whose window
/// contains now reads as `ongoing`; a published event whose end has
/// passed reads as `completed` (the stored status is unchanged).
pub(crate) fn effective_status(event: &Event, now: DateTime<Utc>) -> EventStatus {
    match event.status {
        EventStatus::Published if now >= event.starts_at && now < event.ends_at => {
            EventStatus::Ongoing
        }
        EventStatus::Published if now >= event.ends_at => EventStatus::Completed,
        other => other,
    }
}

pub(crate) fn public_event_view(state: &EventsState, event: &Event) -> Value {
    let now = Utc::now();
    let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "status".to_string(),
            Value::String(effective_status(event, now).as_str().to_string()),
        );
        let tickets: Vec<TicketType> = state
            .ticket_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|ticket| ticket.event_id == event.id)
            .collect();
        map.insert(
            "ticket_types".to_string(),
            serde_json::to_value(tickets).unwrap_or(Value::Null),
        );
        map.insert(
            "available_seats".to_string(),
            Value::Number(available_seats(state, event.id).into()),
        );
    }
    value
}

/// Seats still available across all ticket types of an event
/// (quantity minus confirmed / pending seats). Never negative.
pub(crate) fn available_seats(state: &EventsState, event_id: Uuid) -> i64 {
    let total: i64 = state
        .ticket_types
        .iter()
        .filter(|entry| entry.value().event_id == event_id)
        .map(|entry| entry.value().quantity)
        .sum();
    let taken: i64 = state
        .registrations
        .iter()
        .filter(|entry| {
            entry.value().event_id == event_id
                && matches!(
                    entry.value().status,
                    RegistrationStatus::Confirmed
                        | RegistrationStatus::PendingPayment
                        | RegistrationStatus::PendingVerification
                )
        })
        .map(|entry| entry.value().quantity)
        .sum();
    (total - taken).max(0)
}

pub(crate) fn ticket_available(state: &EventsState, ticket: &TicketType, quantity: i64, now: DateTime<Utc>) -> UnifiedResult<()> {
    if ticket.sold + quantity > ticket.quantity {
        return Err(UnifiedError::conflict(format!(
            "Only {} seat(s) remain for ticket type \"{}\"",
            (ticket.quantity - ticket.sold).max(0),
            ticket.name
        )));
    }
    if let Some(start) = ticket.sales_start {
        if now < start {
            return Err(UnifiedError::bad_request("Ticket sales have not started yet"));
        }
    }
    if let Some(end) = ticket.sales_end {
        if now > end {
            return Err(UnifiedError::bad_request("Ticket sales have ended"));
        }
    }
    Ok(())
}

pub(crate) fn slug_in_use(state: &EventsState, slug: &str, except: Option<Uuid>) -> bool {
    state.events.iter().any(|entry| {
        entry.value().slug == slug && Some(entry.value().id) != except
    })
}

// Recurrence expansion

pub(crate) fn add_months(date: DateTime<Utc>, months: i32) -> DateTime<Utc> {
    let naive = date.naive_utc();
    let total_months = naive.year() * 12 + (naive.month() as i32 - 1) + months;
    let year = total_months.div_euclid(12);
    let month = total_months.rem_euclid(12) + 1;
    let day = naive.day().min(days_in_month(year, month as u32));
    let date_only = NaiveDate::from_ymd_opt(year, month as u32, day)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(year, month as u32, 1).expect("valid month"));
    let naive_dt = date_only.and_time(naive.time());
    Utc.from_utc_datetime(&naive_dt)
}

pub(crate) fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

pub(crate) fn step_occurrence(start: DateTime<Utc>, rule: &RecurrenceRule, steps: u32) -> DateTime<Utc> {
    let interval = rule.interval.max(1) as i64;
    let steps = steps as i64;
    match rule.frequency {
        RecurrenceFrequency::Daily => start + Duration::days(interval * steps),
        RecurrenceFrequency::Weekly => start + Duration::weeks(interval * steps),
        RecurrenceFrequency::Monthly => add_months(start, (interval * steps) as i32),
        RecurrenceFrequency::Yearly => add_months(start, (interval * steps * 12) as i32),
    }
}

/// Expand a recurrence rule into occurrence start times, honouring
/// `count` (wins over `until`), `until`, and `exceptions`. The
/// series duration is the event's own end-minus-start, applied to
/// every occurrence. Capped at `MAX_OCCURRENCES`.
pub(crate) fn expand_recurrence(event: &Event) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let duration = event.ends_at - event.starts_at;
    let Some(rule) = &event.recurrence else {
        return vec![(event.starts_at, event.ends_at)];
    };
    let mut out = Vec::new();
    let mut step: u32 = 0;
    while out.len() < MAX_OCCURRENCES {
        if let Some(count) = rule.count {
            if out.len() >= count as usize {
                break;
            }
        }
        let start = step_occurrence(event.starts_at, rule, step);
        if let Some(until) = rule.until {
            if start > until {
                break;
            }
        }
        step += 1;
        if rule.exceptions.contains(&start) {
            continue;
        }
        out.push((start, start + duration));
        // A rule with neither count nor until is open-ended: stop at
        // the cap (one year of daily occurrences is the practical
        // bound for a single expansion; materialisation uses the
        // same cap).
        if rule.count.is_none() && rule.until.is_none() && out.len() >= MAX_OCCURRENCES {
            break;
        }
    }
    out
}

/// Materialise occurrence rows for an event, replacing any existing
/// rows for it. One-off events get a single row.
pub(crate) fn materialise_occurrences(state: &EventsState, event: &Event) {
    let stale: Vec<Uuid> = state
        .occurrences
        .iter()
        .filter(|entry| entry.value().event_id == event.id)
        .map(|entry| *entry.key())
        .collect();
    for id in stale {
        state.occurrences.remove(&id);
    }
    let now = Utc::now();
    for (index, (starts_at, ends_at)) in expand_recurrence(event).into_iter().enumerate() {
        let occurrence = EventOccurrence {
            id: Uuid::now_v7(),
            event_id: event.id,
            sequence: (index + 1) as u32,
            starts_at,
            ends_at,
            status: "scheduled".to_string(),
            venue_override: None,
            overridden: false,
            created_at: now,
            updated_at: now,
        };
        state.occurrences.insert(occurrence.id, occurrence);
    }
}

pub(crate) fn occurrences_for(state: &EventsState, event_id: Uuid) -> Vec<EventOccurrence> {
    let mut items: Vec<EventOccurrence> = state
        .occurrences
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|occurrence| occurrence.event_id == event_id)
        .collect();
    items.sort_by_key(|occurrence| (occurrence.starts_at, occurrence.sequence));
    items
}

// iCalendar (RFC 5545, hand-built - no calendar crate in Cargo.toml)

/// Escape a TEXT value per RFC 5545 section 3.3.11: backslash,
/// semicolon, comma, and newlines. Folding is applied at 75
/// octets per line (section 3.1) with a single leading space on
/// continuation lines.
pub(crate) fn ics_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

pub(crate) fn ics_fold(line: &str) -> String {
    if line.len() <= 75 {
        return line.to_string();
    }
    let mut out = String::new();
    let mut current = String::new();
    for ch in line.chars() {
        if current.len() + ch.len_utf8() > 75 {
            out.push_str(&current);
            out.push_str("\r\n ");
            current.clear();
        }
        current.push(ch);
    }
    out.push_str(&current);
    out
}

pub(crate) fn ics_datetime(value: DateTime<Utc>) -> String {
    value.format("%Y%m%dT%H%M%SZ").to_string()
}

pub(crate) fn ics_event_block(event: &Event, occurrence: Option<&EventOccurrence>) -> String {
    let (starts_at, ends_at, location) = match occurrence {
        Some(occ) => (
            occ.starts_at,
            occ.ends_at,
            occ.venue_override
                .as_ref()
                .or(event.venue.as_ref())
                .map(|venue| venue.name.clone()),
        ),
        None => (
            event.starts_at,
            event.ends_at,
            event.venue.as_ref().map(|venue| venue.name.clone()),
        ),
    };
    let uid = match occurrence {
        Some(occ) => format!("{}-{}@events.unified", event.id, occ.id),
        None => format!("{}@events.unified", event.id),
    };
    let mut lines: Vec<String> = vec![
        "BEGIN:VEVENT".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", ics_datetime(Utc::now())),
        format!("DTSTART:{}", ics_datetime(starts_at)),
        format!("DTEND:{}", ics_datetime(ends_at)),
        format!("SUMMARY:{}", ics_escape(&event.title)),
    ];
    if let Some(description) = &event.description {
        if !description.trim().is_empty() {
            lines.push(format!("DESCRIPTION:{}", ics_escape(description)));
        }
    }
    if let Some(name) = location {
        lines.push(format!("LOCATION:{}", ics_escape(&name)));
    }
    if !event.timezone.is_empty() {
        lines.push(format!("X-TIMEZONE:{}", ics_escape(&event.timezone)));
    }
    match event.status {
        EventStatus::Cancelled => lines.push("STATUS:CANCELLED".to_string()),
        EventStatus::Completed => lines.push("STATUS:CONFIRMED".to_string()),
        _ => lines.push("STATUS:CONFIRMED".to_string()),
    }
    if let Some(rule) = &event.recurrence {
        if occurrence.is_none() {
            let mut rrule = format!("FREQ={}", rule.frequency.rfc5545());
            if rule.interval > 1 {
                rrule.push_str(&format!(";INTERVAL={}", rule.interval));
            }
            if let Some(count) = rule.count {
                rrule.push_str(&format!(";COUNT={count}"));
            } else if let Some(until) = rule.until {
                rrule.push_str(&format!(";UNTIL={}", ics_datetime(until)));
            }
            lines.push(format!("RRULE:{rrule}"));
        }
    }
    lines.push("END:VEVENT".to_string());
    lines
        .into_iter()
        .map(|line| ics_fold(&line))
        .collect::<Vec<_>>()
        .join("\r\n")
}

pub(crate) fn build_ics(events: &[(Event, Option<EventOccurrence>)], calendar_name: &str) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCALENDAR\r\n");
    out.push_str("VERSION:2.0\r\n");
    out.push_str("PRODID:-//Unified//Events Manager//EN\r\n");
    out.push_str("CALSCALE:GREGORIAN\r\n");
    out.push_str("METHOD:PUBLISH\r\n");
    out.push_str(&ics_fold(&format!("X-WR-CALNAME:{}", ics_escape(calendar_name))));
    out.push_str("\r\n");
    for (event, occurrence) in events {
        out.push_str(&ics_event_block(event, occurrence.as_ref()));
        out.push_str("\r\n");
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

// CSV export (hand-rolled: no csv crate is available, same as the
// import/export module)

pub(crate) fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

pub(crate) fn attendees_csv(attendees: &[Attendee]) -> String {
    let mut out = String::from("id,event_id,registration_id,name,email,checked_in_at,created_at\r\n");
    for attendee in attendees {
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\r\n",
            attendee.id,
            attendee.event_id,
            attendee.registration_id,
            escape_csv_field(&attendee.name),
            escape_csv_field(&attendee.email),
            attendee
                .checked_in_at
                .map(|value| value.to_rfc3339())
                .unwrap_or_default(),
            attendee.created_at.to_rfc3339(),
        ));
    }
    out
}

// Reminders (via the scheduler + notifications)

/// Queue one reminder through `scheduler.enqueue` with kind
/// `events.reminders.send`. Mirrors the meetings module's
/// `schedule_meeting`: the scheduler answers
/// `{"queued": true, "job_id": ...}` when it knows the kind, and
/// `{"queued": false, "error": "Unknown job kind: ..."}` today,
/// because `events.reminders.send` is not in `KNOWN_KINDS` yet.
/// The reminder is stored either way; `scheduler_status` records
/// the outcome honestly (`queued` / `declined`).
pub(crate) fn schedule_reminder(state: &EventsState, reminder: &mut EventReminder, event: &Event) {
    let Some(bus) = state.bus() else {
        reminder.scheduler_job_id = None;
        reminder.scheduler_status = "declined".to_string();
        return;
    };
    let send_at = event.starts_at - Duration::minutes(reminder.offset_minutes);
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "events.reminders.send",
            "payload": {
                "event_id": event.id.to_string(),
                "reminder_id": reminder.id.to_string(),
                "occurrence_id": reminder.occurrence_id.map(|id| id.to_string()),
                "send_at": send_at.to_rfc3339(),
                "offset_minutes": reminder.offset_minutes,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            reminder.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            reminder.scheduler_status = "queued".to_string();
        }
        _ => {
            reminder.scheduler_job_id = None;
            reminder.scheduler_status = "declined".to_string();
        }
    }
}

/// Send one reminder now: one `notification.requested` per
/// confirmed attendee of the event (or of the single occurrence
/// when the reminder names one), using the notifications module's
/// payload shape. Title/body templates support `{title}`,
/// `{starts_at}`, and `{event_id}` placeholders.
pub(crate) fn send_reminder(state: &EventsState, reminder: &EventReminder) -> UnifiedResult<usize> {
    let event = state
        .events
        .get(&reminder.event_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Event not found: {}", reminder.event_id)))?;
    let title_template = reminder
        .title_template
        .clone()
        .unwrap_or_else(|| "Reminder: {title}".to_string());
    let body_template = reminder.body_template.clone().unwrap_or_else(|| {
        "Your event \"{title}\" starts at {starts_at}.".to_string()
    });
    let starts_at = reminder
        .occurrence_id
        .and_then(|id| state.occurrences.get(&id).map(|entry| entry.value().starts_at))
        .unwrap_or(event.starts_at);
    let title = title_template
        .replace("{title}", &event.title)
        .replace("{starts_at}", &starts_at.to_rfc3339())
        .replace("{event_id}", &event.id.to_string());
    let body = body_template
        .replace("{title}", &event.title)
        .replace("{starts_at}", &starts_at.to_rfc3339())
        .replace("{event_id}", &event.id.to_string());
    let link = format!("/api/v1/events/public/{}", event.slug);
    let mut sent = 0usize;
    let mut seen: Vec<String> = Vec::new();
    for entry in state.registrations.iter() {
        let registration = entry.value();
        if registration.event_id != event.id
            || registration.status != RegistrationStatus::Confirmed
        {
            continue;
        }
        if let Some(occurrence_id) = reminder.occurrence_id {
            if registration.occurrence_id != Some(occurrence_id) {
                continue;
            }
        }
        if seen.contains(&registration.user_id) {
            continue;
        }
        seen.push(registration.user_id.clone());
        state.notify("event.reminder", &registration.user_id, &title, &body, Some(link.clone()));
        sent += 1;
    }
    if let Some(mut stored) = state.reminders.get_mut(&reminder.id) {
        stored.status = "sent".to_string();
        stored.sent_at = Some(Utc::now());
        stored.updated_at = Utc::now();
    }
    state.emit(
        "events.reminder.sent",
        json!({
            "event_id": event.id.to_string(),
            "reminder_id": reminder.id.to_string(),
            "recipients": sent,
        }),
    );
    Ok(sent)
}

// Waitlist promotion + cancellation internals

/// Cancel a registration (shared by the HTTP handler and the
/// bank-transfer rejection listener): mark it cancelled, release
/// the sold seats, remove its attendee check-ins, then promote the
/// waitlist. Returns the updated registration.
pub(crate) fn cancel_registration_inner(
    state: &EventsState,
    registration: &Registration,
    reason: Option<&str>,
) -> Registration {
    let now = Utc::now();
    let mut updated = registration.clone();
    updated.status = RegistrationStatus::Cancelled;
    updated.cancelled_at = Some(now);
    updated.updated_at = now;
    state.registrations.insert(updated.id, updated.clone());
    if registration.status == RegistrationStatus::Confirmed {
        if let Some(mut ticket) = state.ticket_types.get_mut(&registration.ticket_type_id) {
            ticket.sold = (ticket.sold - registration.quantity).max(0);
            ticket.updated_at = now;
        }
    }
    state.emit(
        "events.registration.cancelled",
        json!({
            "event_id": registration.event_id.to_string(),
            "registration_id": registration.id.to_string(),
            "user_id": registration.user_id,
            "reason": reason,
        }),
    );
    promote_waitlist(state, registration.event_id, registration.ticket_type_id);
    updated
}

/// Promote waitlist entries while seats are free for the ticket
/// type. Each promoted entry becomes a registration (confirmed
/// when the ticket is free, `pending_payment` otherwise) and the
/// user is notified through `notification.requested`.
pub(crate) fn promote_waitlist(state: &EventsState, event_id: Uuid, ticket_type_id: Uuid) {
    let Some(ticket) = state
        .ticket_types
        .get(&ticket_type_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    let Some(event) = state
        .events
        .get(&event_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    let mut waiting: Vec<WaitlistEntry> = state
        .waitlist
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            entry.event_id == event_id
                && entry.ticket_type_id == ticket_type_id
                && entry.status == "waiting"
        })
        .collect();
    waiting.sort_by_key(|entry| entry.position);
    for entry in waiting {
        let mut ticket_now = match state.ticket_types.get(&ticket_type_id) {
            Some(ticket) => ticket.value().clone(),
            None => return,
        };
        if ticket_now.sold + entry.quantity > ticket_now.quantity {
            // Skip this entry and try the next one: a large
            // first-in-line entry must not block smaller entries
            // behind it from being promoted while seats are free.
            // FIFO order is kept among the entries that do fit.
            continue;
        }
        let now = Utc::now();
        let total = ticket.price.saturating_mul(entry.quantity);
        let status = if total == 0 {
            RegistrationStatus::Confirmed
        } else {
            RegistrationStatus::PendingPayment
        };
        let registration = Registration {
            id: Uuid::now_v7(),
            event_id,
            occurrence_id: None,
            ticket_type_id,
            user_id: entry.user_id.clone(),
            quantity: entry.quantity,
            total_amount: total,
            currency: ticket.currency.clone(),
            status,
            order_id: None,
            payment_reference: None,
            payment_provider: None,
            check_in_code: Uuid::new_v4().to_string(),
            checked_in_at: None,
            cancelled_at: None,
            refund_reason: None,
            created_at: now,
            updated_at: now,
        };
        if status == RegistrationStatus::Confirmed {
            ticket_now.sold += entry.quantity;
            ticket_now.updated_at = now;
            state.ticket_types.insert(ticket_type_id, ticket_now);
        }
        state.registrations.insert(registration.id, registration.clone());
        if let Some(mut waitlist_entry) = state.waitlist.get_mut(&entry.id) {
            waitlist_entry.status = "promoted".to_string();
            waitlist_entry.updated_at = now;
        }
        state.emit(
            "events.waitlist.promoted",
            json!({
                "event_id": event_id.to_string(),
                "ticket_type_id": ticket_type_id.to_string(),
                "waitlist_id": entry.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": entry.user_id,
                "status": status.as_str(),
            }),
        );
        state.notify(
            "event.waitlist.promoted",
            &entry.user_id,
            &format!("A seat opened up: {}", event.title),
            &format!(
                "A seat opened up for \"{}\". Your registration is now {}.",
                event.title,
                status.as_str()
            ),
            Some(format!("/api/v1/events/public/{}", event.slug)),
        );
        let _ = ticket;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn at(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap()
    }

    fn test_event() -> Event {
        Event {
            id: Uuid::nil(),
            title: "Test Event".to_string(),
            description: Some("A test".to_string()),
            slug: "test-event".to_string(),
            category: Vec::new(),
            tags: Vec::new(),
            venue: None,
            starts_at: at(2026, 1, 10, 9),
            ends_at: at(2026, 1, 10, 17),
            timezone: "UTC".to_string(),
            visibility: EventVisibility::Public,
            status: EventStatus::Published,
            organizer_id: "org-1".to_string(),
            company_id: None,
            cover_storage_target: None,
            cover_storage_key: None,
            recurrence: None,
            created_at: at(2026, 1, 1, 0),
            updated_at: at(2026, 1, 1, 0),
        }
    }

    #[test]
    fn days_in_month_handles_leap_years() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2025, 2), 28);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2026, 1), 31);
        assert_eq!(days_in_month(2026, 13), 30);
    }

    #[test]
    fn add_months_clamps_to_month_end_and_crosses_years() {
        assert_eq!(add_months(at(2025, 1, 31, 9), 1), at(2025, 2, 28, 9));
        assert_eq!(add_months(at(2024, 1, 31, 9), 1), at(2024, 2, 29, 9));
        assert_eq!(add_months(at(2026, 1, 15, 9), 12), at(2027, 1, 15, 9));
        assert_eq!(add_months(at(2025, 12, 15, 9), 1), at(2026, 1, 15, 9));
    }

    #[test]
    fn step_occurrence_advances_by_frequency_and_interval() {
        let daily = RecurrenceRule {
            frequency: RecurrenceFrequency::Daily,
            interval: 2,
            count: None,
            until: None,
            exceptions: Vec::new(),
        };
        assert_eq!(step_occurrence(at(2026, 1, 1, 9), &daily, 3), at(2026, 1, 7, 9));
        let weekly = RecurrenceRule { frequency: RecurrenceFrequency::Weekly, ..daily.clone() };
        assert_eq!(step_occurrence(at(2026, 1, 1, 9), &weekly, 2), at(2026, 1, 29, 9));
        let monthly = RecurrenceRule { frequency: RecurrenceFrequency::Monthly, interval: 1, ..daily.clone() };
        assert_eq!(step_occurrence(at(2026, 1, 31, 9), &monthly, 1), at(2026, 2, 28, 9));
    }

    #[test]
    fn expand_recurrence_without_a_rule_returns_the_single_event() {
        let event = test_event();
        let occurrences = expand_recurrence(&event);
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0], (at(2026, 1, 10, 9), at(2026, 1, 10, 17)));
    }

    #[test]
    fn expand_recurrence_honours_count_and_exceptions() {
        let mut event = test_event();
        event.recurrence = Some(RecurrenceRule {
            frequency: RecurrenceFrequency::Daily,
            interval: 1,
            count: Some(3),
            until: None,
            exceptions: vec![at(2026, 1, 11, 9)],
        });
        let occurrences = expand_recurrence(&event);
        assert_eq!(occurrences.len(), 3);
        let starts: Vec<DateTime<Utc>> = occurrences.iter().map(|(start, _)| *start).collect();
        assert!(!starts.contains(&at(2026, 1, 11, 9)));
        assert_eq!(starts[0], at(2026, 1, 10, 9));
        assert_eq!(starts[1], at(2026, 1, 12, 9));
    }

    #[test]
    fn effective_status_derives_ongoing_and_completed() {
        let event = test_event();
        assert_eq!(effective_status(&event, at(2026, 1, 10, 12)), EventStatus::Ongoing);
        assert_eq!(effective_status(&event, at(2026, 1, 10, 18)), EventStatus::Completed);
        assert_eq!(effective_status(&event, at(2026, 1, 9, 12)), EventStatus::Published);
        let draft = Event { status: EventStatus::Draft, ..test_event() };
        assert_eq!(effective_status(&draft, at(2026, 1, 10, 12)), EventStatus::Draft);
    }

    #[test]
    fn ics_escape_covers_specials_and_drops_carriage_returns() {
        assert_eq!(ics_escape("a;b,c\\d\ne\rf"), "a\\;b\\,c\\\\d\\nef");
        assert_eq!(ics_escape("plain"), "plain");
    }

    #[test]
    fn ics_fold_keeps_short_lines_and_folds_long_ones() {
        assert_eq!(ics_fold("short"), "short");
        let long = "x".repeat(80);
        let folded = ics_fold(&long);
        assert!(folded.contains("\r\n "));
        assert_eq!(folded.replace("\r\n ", ""), long);
        for line in folded.split("\r\n") {
            assert!(line.len() <= 76, "folded line too long: {line}");
        }
    }

    #[test]
    fn ics_datetime_uses_utc_basic_format() {
        assert_eq!(ics_datetime(at(2026, 3, 4, 5)), "20260304T050000Z");
    }

    #[test]
    fn ics_event_block_contains_the_essential_lines() {
        let block = ics_event_block(&test_event(), None);
        assert!(block.contains("BEGIN:VEVENT"));
        assert!(block.contains("SUMMARY:Test Event"));
        assert!(block.contains("DTSTART:20260110T090000Z"));
        assert!(block.contains("DTEND:20260110T170000Z"));
        assert!(block.contains("END:VEVENT"));
    }

    #[test]
    fn promote_waitlist_skips_entry_that_does_not_fit() {
        let state = EventsState::default();
        let event = test_event();
        let ticket_id = Uuid::from_u128(0x1001);
        state.events.insert(event.id, event.clone());
        state.ticket_types.insert(
            ticket_id,
            TicketType {
                id: ticket_id,
                event_id: event.id,
                name: "General".to_string(),
                description: None,
                price: 0,
                currency: "USD".to_string(),
                quantity: 10,
                sold: 8,
                sales_start: None,
                sales_end: None,
                per_order_limit: 10,
                created_at: at(2026, 1, 1, 0),
                updated_at: at(2026, 1, 1, 0),
            },
        );
        // Two free seats. The first entry wants 5 (does not fit);
        // the second wants 2 (fits).
        let first_id = Uuid::from_u128(0x2001);
        let second_id = Uuid::from_u128(0x2002);
        for (id, user_id, quantity, position) in [
            (first_id, "user-1", 5, 1),
            (second_id, "user-2", 2, 2),
        ] {
            state.waitlist.insert(
                id,
                WaitlistEntry {
                    id,
                    event_id: event.id,
                    ticket_type_id: ticket_id,
                    user_id: user_id.to_string(),
                    quantity,
                    position,
                    status: "waiting".to_string(),
                    created_at: at(2026, 1, 1, 0),
                    updated_at: at(2026, 1, 1, 0),
                },
            );
        }
        promote_waitlist(&state, event.id, ticket_id);
        // Before the fix the loop stopped at the first entry, so
        // the fitting entry behind it was never promoted.
        assert_eq!(
            state
                .waitlist
                .get(&first_id)
                .map(|entry| entry.value().status.clone()),
            Some("waiting".to_string())
        );
        assert_eq!(
            state
                .waitlist
                .get(&second_id)
                .map(|entry| entry.value().status.clone()),
            Some("promoted".to_string())
        );
        assert_eq!(
            state
                .registrations
                .iter()
                .filter(|entry| entry.value().user_id == "user-2")
                .count(),
            1
        );
        assert_eq!(
            state
                .registrations
                .iter()
                .filter(|entry| entry.value().user_id == "user-1")
                .count(),
            0
        );
        assert_eq!(
            state
                .ticket_types
                .get(&ticket_id)
                .map(|entry| entry.value().sold),
            Some(10)
        );
    }

    #[test]
    fn escape_csv_field_quotes_only_when_needed() {
        assert_eq!(escape_csv_field("plain"), "plain");
        assert_eq!(escape_csv_field("a,b"), "\"a,b\"");
        assert_eq!(escape_csv_field("a\"b"), "\"a\"\"b\"");
        assert_eq!(escape_csv_field("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn attendees_csv_quotes_fields_with_commas() {
        let attendee = Attendee {
            id: Uuid::nil(),
            event_id: Uuid::nil(),
            registration_id: Uuid::nil(),
            name: "Doe, Jane".to_string(),
            email: "jane@example.com".to_string(),
            custom_answers: json!(null),
            checked_in_at: None,
            created_at: at(2026, 1, 1, 0),
        };
        let csv = attendees_csv(std::slice::from_ref(&attendee));
        assert!(csv.starts_with("id,event_id,registration_id,name,email,checked_in_at,created_at\r\n"));
        assert!(csv.contains("\"Doe, Jane\",jane@example.com"));
    }
}
