// appointments - ics.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::scheduling::*;
use super::reminders::*;
use super::validation::*;
use super::handlers_services::*;
use super::handlers_staff::*;
use super::handlers_appointments::*;
use super::handlers_reminders::*;
use super::module::*;

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

pub(crate) fn ics_appointment_block(
    appointment: &Appointment,
    service: &AppointmentService,
) -> String {
    let mut lines: Vec<String> = vec![
        "BEGIN:VEVENT".to_string(),
        format!("UID:{}@appointments.unified", appointment.id),
        format!("DTSTAMP:{}", ics_datetime(Utc::now())),
        format!("DTSTART:{}", ics_datetime(appointment.starts_at)),
        format!("DTEND:{}", ics_datetime(appointment.ends_at)),
        format!(
            "SUMMARY:{}",
            ics_escape(&format!("{} - {}", service.name, appointment.customer_name))
        ),
    ];
    let description = format!(
        "Customer: {} ({})\nConfirmation code: {}",
        appointment.customer_name, appointment.customer_email, appointment.confirmation_code
    );
    lines.push(format!("DESCRIPTION:{}", ics_escape(&description)));
    if let Some(location) = &service.location {
        if !location.trim().is_empty() {
            lines.push(format!("LOCATION:{}", ics_escape(location)));
        }
    }
    match appointment.status {
        AppointmentStatus::Cancelled => lines.push("STATUS:CANCELLED".to_string()),
        AppointmentStatus::Completed => lines.push("STATUS:CONFIRMED".to_string()),
        _ => lines.push("STATUS:CONFIRMED".to_string()),
    }
    lines.push("END:VEVENT".to_string());
    lines
        .into_iter()
        .map(|line| ics_fold(&line))
        .collect::<Vec<_>>()
        .join("\r\n")
}

pub(crate) fn build_ics(blocks: &[String], calendar_name: &str) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCALENDAR\r\n");
    out.push_str("VERSION:2.0\r\n");
    out.push_str("PRODID:-//Unified//Appointments//EN\r\n");
    out.push_str("CALSCALE:GREGORIAN\r\n");
    out.push_str("METHOD:PUBLISH\r\n");
    out.push_str(&ics_fold(&format!("X-WR-CALNAME:{}", ics_escape(calendar_name))));
    out.push_str("\r\n");
    for block in blocks {
        out.push_str(block);
        out.push_str("\r\n");
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

pub(crate) fn ics_response(body: String) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/calendar; charset=utf-8")
        .insert_header(("Content-Disposition", "attachment; filename=\"calendar.ics\""))
        .body(body)
}

/// Public view of one appointment: the confirmation code is the
/// only credential, so the customer phone number is never included.
pub(crate) fn public_appointment_view(appointment: &Appointment, service: &AppointmentService) -> Value {
    json!({
        "id": appointment.id,
        "service_id": appointment.service_id,
        "service_name": service.name,
        "staff_user_id": appointment.staff_user_id,
        "customer_name": appointment.customer_name,
        "customer_email": appointment.customer_email,
        "starts_at": appointment.starts_at.to_rfc3339(),
        "ends_at": appointment.ends_at.to_rfc3339(),
        "status": appointment.status.as_str(),
        "price": minor_to_major(appointment.price, &appointment.currency),
        "currency": appointment.currency,
        "cancellation_fee_charged": appointment
            .cancellation_fee_charged
            .map(|fee| minor_to_major(fee, &appointment.currency)),
        "confirmation_code": appointment.confirmation_code,
        "created_at": appointment.created_at.to_rfc3339(),
        "updated_at": appointment.updated_at.to_rfc3339(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ics_escape_quotes_specials_and_drops_carriage_returns() {
        assert_eq!(ics_escape("a,b;c\\d"), "a\\,b\\;c\\\\d");
        assert_eq!(ics_escape("line1\nline2\r"), "line1\\nline2");
        assert_eq!(ics_escape("plain"), "plain");
    }

    #[test]
    fn ics_fold_keeps_short_lines_and_folds_long_ones() {
        let short = "x".repeat(75);
        assert_eq!(ics_fold(&short), short);
        let long = "y".repeat(80);
        let folded = ics_fold(&long);
        assert_eq!(folded, format!("{}\r\n {}", "y".repeat(75), "y".repeat(5)));
    }

    #[test]
    fn ics_datetime_formats_utc_compactly() {
        let at = Utc.with_ymd_and_hms(2026, 11, 22, 9, 30, 5).unwrap();
        assert_eq!(ics_datetime(at), "20261122T093005Z");
    }
}
