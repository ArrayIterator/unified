//! Input validation for the fleet module.
//!
//! Pure functions only: no store access, no I/O. Every validator
//! returns a `UnifiedError` (400 for malformed input, 422 for input
//! that is well formed but violates a domain rule) so handlers can
//! use `?` directly.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use regex::Regex;
use std::sync::OnceLock;
use unified_core::error::{UnifiedError, UnifiedResult};

/// A plate number, normalised for comparison: upper-cased with spaces
/// and hyphens removed. Two plates that normalise to the same string
/// are the same plate.
pub fn normalise_plate(plate: &str) -> String {
    plate
        .to_uppercase()
        .chars()
        .filter(|c| *c != ' ' && *c != '-')
        .collect()
}

fn plate_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Z0-9][A-Z0-9 -]{1,11}$").expect("plate regex"))
}

/// Validate a plate number: 3-12 characters, letters, digits, spaces
/// and hyphens only, with at least one letter and one digit.
pub fn validate_plate(plate: &str) -> UnifiedResult<String> {
    let trimmed = plate.trim().to_uppercase();
    if trimmed.len() < 3 || trimmed.len() > 12 {
        return Err(UnifiedError::bad_request(
            "plate_number must be 3-12 characters",
        ));
    }
    if !plate_regex().is_match(&trimmed) {
        return Err(UnifiedError::bad_request(
            "plate_number may only contain letters, digits, spaces and hyphens",
        ));
    }
    if !trimmed.chars().any(|c| c.is_ascii_alphabetic())
        || !trimmed.chars().any(|c| c.is_ascii_digit())
    {
        return Err(UnifiedError::bad_request(
            "plate_number must contain at least one letter and one digit",
        ));
    }
    Ok(trimmed)
}

fn vin_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // 17 characters; I, O and Q are never used in a VIN.
    RE.get_or_init(|| Regex::new(r"^[A-HJ-NPR-Z0-9]{17}$").expect("vin regex"))
}

/// Validate a 17-character VIN (I, O, Q excluded).
pub fn validate_vin(vin: &str) -> UnifiedResult<String> {
    let trimmed = vin.trim().to_uppercase();
    if !vin_regex().is_match(&trimmed) {
        return Err(UnifiedError::bad_request(
            "vin must be exactly 17 characters (letters except I, O, Q, and digits)",
        ));
    }
    Ok(trimmed)
}

/// Validate a vehicle manufacture year: 1900 through next year.
pub fn validate_year(year: i32) -> UnifiedResult<i32> {
    let max = Utc::now().date_naive().year() + 1;
    if !(1900..=max).contains(&year) {
        return Err(UnifiedError::bad_request(format!(
            "year must be between 1900 and {max}"
        )));
    }
    Ok(year)
}

/// A non-negative number (odometer, cost, acquisition cost).
pub fn validate_non_negative(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number >= 0"
        )));
    }
    Ok(value)
}

/// A strictly positive number (fuel litres).
pub fn validate_positive(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value <= 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number > 0"
        )));
    }
    Ok(value)
}

/// Tenant ids are required on every request and must be a sane
/// non-empty string. This is the tenant-isolation boundary: a missing
/// tenant is a 400, never a silent global view.
pub fn validate_tenant_id(tenant_id: &str) -> UnifiedResult<String> {
    let trimmed = tenant_id.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(UnifiedError::bad_request(
            "tenant_id is required (1-128 characters)",
        ));
    }
    Ok(trimmed.to_string())
}

/// True when two half-open time windows overlap. Touching endpoints
/// (one ends exactly when the other starts) do not overlap.
pub fn windows_overlap(
    start_a: DateTime<Utc>,
    end_a: DateTime<Utc>,
    start_b: DateTime<Utc>,
    end_b: DateTime<Utc>,
) -> bool {
    start_a < end_b && start_b < end_a
}

/// Validate that a planned trip window is well ordered.
pub fn validate_time_window(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> UnifiedResult<()> {
    if end <= start {
        return Err(UnifiedError::bad_request(
            "planned_end must be after planned_start",
        ));
    }
    Ok(())
}

/// Validate a GPS fix: latitude -90..90, longitude -180..180,
/// speed >= 0, timestamp no more than 5 minutes in the future.
pub fn validate_position(
    latitude: f64,
    longitude: f64,
    speed_kmh: f64,
    recorded_at: DateTime<Utc>,
) -> UnifiedResult<()> {
    if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
        return Err(UnifiedError::bad_request(
            "latitude must be between -90 and 90",
        ));
    }
    if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
        return Err(UnifiedError::bad_request(
            "longitude must be between -180 and 180",
        ));
    }
    if !speed_kmh.is_finite() || speed_kmh < 0.0 {
        return Err(UnifiedError::bad_request("speed_kmh must be >= 0"));
    }
    let latest = Utc::now() + chrono::Duration::minutes(5);
    if recorded_at > latest {
        return Err(UnifiedError::bad_request(
            "recorded_at must not be more than 5 minutes in the future",
        ));
    }
    Ok(())
}

/// Days from today until `date`. Negative when the date is past.
pub fn days_until(date: NaiveDate) -> i64 {
    (date - Utc::now().date_naive()).num_days()
}

/// A driver licence counts as expiring soon when it expires within
/// 30 days (including today) and has not expired yet.
pub fn licence_expiring_soon(expiry: NaiveDate) -> bool {
    let days = days_until(expiry);
    (0..=30).contains(&days)
}

/// Allowed vehicle status transitions. `retired` is terminal.
pub fn vehicle_status_transition(from: &str, to: &str) -> UnifiedResult<()> {
    let allowed = matches!(
        (from, to),
        ("active", "maintenance")
            | ("maintenance", "active")
            | ("active", "retired")
            | ("maintenance", "retired")
            | ("active", "active")
            | ("maintenance", "maintenance")
    );
    if !allowed {
        return Err(UnifiedError::unprocessable(format!(
            "vehicle status cannot move from {from} to {to} (retired is terminal)"
        )));
    }
    Ok(())
}

/// Allowed trip status transitions.
pub fn trip_status_transition(from: &str, to: &str) -> UnifiedResult<()> {
    let allowed = matches!(
        (from, to),
        ("planned", "in_progress")
            | ("planned", "cancelled")
            | ("in_progress", "completed")
            | ("in_progress", "cancelled")
    );
    if !allowed {
        return Err(UnifiedError::unprocessable(format!(
            "trip status cannot move from {from} to {to}"
        )));
    }
    Ok(())
}

/// Fuel efficiency (km per litre) between consecutive fill-ups.
///
/// Each fill-up after the first contributes one interval: the
/// distance driven since the previous fill-up divided by the litres
/// taken at this fill-up. The overall figure is total distance over
/// total litres across all intervals. Returns `None` when there are
/// fewer than two fill-ups or the total distance is not positive -
/// callers must surface null, never a fabricated number.
pub fn fuel_efficiency_km_per_litre(
    fill_ups: &[(f64, f64)], // (odometer_km, litres), oldest first
) -> Option<f64> {
    if fill_ups.len() < 2 {
        return None;
    }
    let mut total_distance = 0.0;
    let mut total_litres = 0.0;
    for window in fill_ups.windows(2) {
        let distance = window[1].0 - window[0].0;
        let litres = window[1].1;
        if distance > 0.0 && litres > 0.0 {
            total_distance += distance;
            total_litres += litres;
        }
    }
    if total_distance <= 0.0 || total_litres <= 0.0 {
        None
    } else {
        Some(total_distance / total_litres)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn plate_validation_accepts_indonesian_shapes() {
        assert_eq!(validate_plate("B 1234 ABC").unwrap(), "B 1234 ABC");
        assert_eq!(validate_plate("b-1234-abc").unwrap(), "B-1234-ABC");
        assert!(validate_plate("AB").is_err());
        assert!(validate_plate("123456").is_err());
        assert!(validate_plate("ABCDEF").is_err());
        assert!(validate_plate("B 1234 AB!").is_err());
    }

    #[test]
    fn plate_normalisation_collapses_separators() {
        assert_eq!(normalise_plate("B 1234 ABC"), "B1234ABC");
        assert_eq!(normalise_plate("b-1234-abc"), "B1234ABC");
    }

    #[test]
    fn vin_validation_rejects_short_and_forbidden_letters() {
        assert!(validate_vin("1HGCM82633A004352").is_ok());
        assert!(validate_vin("1HGCM82633A00435").is_err());
        assert!(validate_vin("1HGCM82633A00435I").is_err());
        assert!(validate_vin("1HGCM82633A00435O").is_err());
        assert!(validate_vin("1HGCM82633A00435Q").is_err());
    }

    #[test]
    fn windows_overlap_treats_touching_endpoints_as_free() {
        let t = |h: u32| Utc.with_ymd_and_hms(2026, 11, 22, h, 0, 0).unwrap();
        assert!(windows_overlap(t(8), t(10), t(9), t(11)));
        assert!(!windows_overlap(t(8), t(10), t(10), t(12)));
        assert!(!windows_overlap(t(8), t(10), t(5), t(8)));
        assert!(windows_overlap(t(8), t(12), t(9), t(10)));
    }

    #[test]
    fn retired_is_terminal_and_active_can_go_to_maintenance() {
        assert!(vehicle_status_transition("active", "maintenance").is_ok());
        assert!(vehicle_status_transition("maintenance", "active").is_ok());
        assert!(vehicle_status_transition("active", "retired").is_ok());
        assert!(vehicle_status_transition("retired", "active").is_err());
        assert!(vehicle_status_transition("retired", "retired").is_err());
        assert!(trip_status_transition("planned", "completed").is_err());
        assert!(trip_status_transition("in_progress", "completed").is_ok());
        assert!(trip_status_transition("completed", "cancelled").is_err());
    }

    #[test]
    fn fuel_efficiency_needs_two_fill_ups() {
        assert_eq!(fuel_efficiency_km_per_litre(&[]), None);
        assert_eq!(fuel_efficiency_km_per_litre(&[(1000.0, 50.0)]), None);
        // 500 km on 50 litres, then 400 km on 40 litres: 900 / 90 = 10.
        let eff =
            fuel_efficiency_km_per_litre(&[(1000.0, 50.0), (1500.0, 50.0), (1900.0, 40.0)]);
        assert!((eff.unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn position_rejects_out_of_range_and_far_future() {
        let now = Utc::now();
        assert!(validate_position(0.0, 0.0, 60.0, now).is_ok());
        assert!(validate_position(91.0, 0.0, 0.0, now).is_err());
        assert!(validate_position(0.0, 181.0, 0.0, now).is_err());
        assert!(validate_position(0.0, 0.0, -1.0, now).is_err());
        assert!(validate_position(0.0, 0.0, 0.0, now + chrono::Duration::minutes(6)).is_err());
        assert!(validate_position(0.0, 0.0, 0.0, now + chrono::Duration::minutes(4)).is_ok());
    }
}
