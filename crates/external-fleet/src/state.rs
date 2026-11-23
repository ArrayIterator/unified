//! Fleet domain types and the in-memory per-tenant store.
//!
//! All records carry a `tenant_id`. Every store method takes the
//! tenant explicitly and filters by it; there is no code path that
//! reads across tenants. The store is in-memory only (DashMap +
//! RwLock); persistence is a follow-up that must keep the same
//! tenant filter on every query.

use crate::validation::{
    fuel_efficiency_km_per_litre, licence_expiring_soon, normalise_plate, validate_plate,
    validate_positive, validate_non_negative, validate_tenant_id, validate_vin,
    validate_year, vehicle_status_transition, windows_overlap,
};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use uuid::Uuid;

/// Default vehicle types. This is a configurable list, not a closed

// Split from the parent file: the implementation now lives
// in the submodules below; the public paths are unchanged
// (this file re-exports everything that used to live here).
pub mod vehicles;
pub mod drivers;
pub mod trips;
pub mod operations;
/// enum: each tenant may replace it through `PUT /vehicle-types`.
/// The defaults are only a starting point.
pub const DEFAULT_VEHICLE_TYPES: &[&str] =
    &["truck", "van", "car", "motorcycle", "other"];

/// Maximum GPS fixes kept per vehicle (bounded history).
pub const MAX_POSITIONS_PER_VEHICLE: usize = 1000;

/// Event names this module emits on the hook bus.
pub const EVENT_MAINTENANCE_DUE: &str = "fleet.maintenance.due";
pub const EVENT_TRIP_COMPLETED: &str = "fleet.trip.completed";
pub const EVENT_VEHICLE_RETIRED: &str = "fleet.vehicle.retired";

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

// ---------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vehicle {
    pub id: String,
    pub tenant_id: String,
    pub plate_number: String,
    pub make: String,
    pub model: String,
    pub year: i32,
    pub vin: String,
    /// Vehicle type: a string validated against the tenant's
    /// configurable type list (see `DEFAULT_VEHICLE_TYPES`).
    #[serde(rename = "type")]
    pub vehicle_type: String,
    /// `active` | `maintenance` | `retired` (terminal).
    pub status: String,
    pub odometer_km: f64,
    pub fuel_type: String,
    pub acquisition_date: Option<NaiveDate>,
    pub acquisition_cost: Option<f64>,
    pub last_latitude: Option<f64>,
    pub last_longitude: Option<f64>,
    pub last_position_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Driver {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub license_number: String,
    pub license_expiry: NaiveDate,
    pub phone: Option<String>,
    /// `active` | `suspended`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Driver as returned by reads, with the computed licence fields.
#[derive(Debug, Clone, Serialize)]
pub struct DriverView {
    #[serde(flatten)]
    pub driver: Driver,
    pub days_until_license_expiry: i64,
    pub license_expiring_soon: bool,
    pub license_expired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trip {
    pub id: String,
    pub tenant_id: String,
    pub vehicle_id: String,
    pub driver_id: String,
    pub origin: String,
    pub destination: String,
    pub planned_start: DateTime<Utc>,
    pub planned_end: DateTime<Utc>,
    pub cargo_notes: Option<String>,
    /// `planned` | `in_progress` | `completed` | `cancelled`.
    pub status: String,
    pub start_odometer_km: f64,
    pub end_odometer_km: Option<f64>,
    /// Distance in km, set on completion (end - start odometer).
    pub distance_km: Option<f64>,
    /// Duration in minutes, set on completion (completed_at - started_at).
    pub duration_minutes: Option<f64>,
    pub estimated_cost: Option<f64>,
    pub actual_cost: Option<f64>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpsPosition {
    pub id: String,
    pub tenant_id: String,
    pub vehicle_id: String,
    pub latitude: f64,
    pub longitude: f64,
    pub speed_kmh: f64,
    pub recorded_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuelLog {
    pub id: String,
    pub tenant_id: String,
    pub vehicle_id: String,
    pub litres: f64,
    pub cost: f64,
    pub odometer_km: f64,
    pub station: Option<String>,
    pub logged_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceItem {
    pub id: String,
    pub tenant_id: String,
    pub vehicle_id: String,
    #[serde(rename = "type")]
    pub maintenance_type: String,
    pub description: Option<String>,
    pub due_odometer_km: Option<f64>,
    pub due_date: Option<NaiveDate>,
    /// `pending` | `completed`.
    pub status: String,
    pub completed_cost: Option<f64>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct VehicleInput {
    pub tenant_id: Option<String>,
    pub plate_number: String,
    pub make: String,
    pub model: String,
    pub year: i32,
    pub vin: String,
    #[serde(rename = "type")]
    pub vehicle_type: String,
    pub status: Option<String>,
    pub odometer_km: Option<f64>,
    pub fuel_type: Option<String>,
    pub acquisition_date: Option<NaiveDate>,
    pub acquisition_cost: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct VehicleUpdate {
    pub tenant_id: Option<String>,
    pub plate_number: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub year: Option<i32>,
    pub vin: Option<String>,
    #[serde(rename = "type")]
    pub vehicle_type: Option<String>,
    pub status: Option<String>,
    pub odometer_km: Option<f64>,
    pub fuel_type: Option<String>,
    pub acquisition_date: Option<NaiveDate>,
    pub acquisition_cost: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct DriverInput {
    pub tenant_id: Option<String>,
    pub name: String,
    pub license_number: String,
    pub license_expiry: NaiveDate,
    pub phone: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DriverUpdate {
    pub tenant_id: Option<String>,
    pub name: Option<String>,
    pub license_number: Option<String>,
    pub license_expiry: Option<NaiveDate>,
    pub phone: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TripInput {
    pub tenant_id: Option<String>,
    pub vehicle_id: String,
    pub driver_id: String,
    pub origin: String,
    pub destination: String,
    pub planned_start: DateTime<Utc>,
    pub planned_end: DateTime<Utc>,
    pub cargo_notes: Option<String>,
    pub start_odometer_km: f64,
    pub estimated_cost: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct TripStatusPatch {
    pub tenant_id: Option<String>,
    pub status: String,
    /// Required when completing a trip; must be >= start odometer.
    pub end_odometer_km: Option<f64>,
    pub actual_cost: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct PositionInput {
    pub tenant_id: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
    pub speed_kmh: f64,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct FuelInput {
    pub tenant_id: Option<String>,
    pub vehicle_id: String,
    pub litres: f64,
    pub cost: f64,
    pub odometer_km: f64,
    pub station: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MaintenanceInput {
    pub tenant_id: Option<String>,
    pub vehicle_id: String,
    #[serde(rename = "type")]
    pub maintenance_type: String,
    pub description: Option<String>,
    pub due_odometer_km: Option<f64>,
    pub due_date: Option<NaiveDate>,
}

#[derive(Debug, Deserialize)]
pub struct MaintenanceComplete {
    pub tenant_id: Option<String>,
    pub cost: f64,
    /// When given, a follow-up pending item is created with these dues.
    pub next_due_odometer_km: Option<f64>,
    pub next_due_date: Option<NaiveDate>,
}

#[derive(Debug, Deserialize)]
pub struct VehicleTypesInput {
    pub tenant_id: Option<String>,
    pub types: Vec<String>,
}

// ---------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------

/// In-memory fleet store. All maps are keyed by record id; the
/// tenant filter is applied by every method, never by callers.
pub struct FleetState {
    vehicles: DashMap<String, Vehicle>,
    drivers: DashMap<String, Driver>,
    trips: DashMap<String, Trip>,
    positions: DashMap<String, VecDeque<GpsPosition>>,
    fuel_logs: DashMap<String, FuelLog>,
    maintenance: DashMap<String, MaintenanceItem>,
    /// Per-tenant configurable vehicle type lists.
    vehicle_types: RwLock<HashMap<String, Vec<String>>>,
    /// Hook bus attached in `init()`; `None` until then (tests, or a
    /// server that never calls `init`). Emitting is best-effort.
    hooks: RwLock<Option<Arc<HookBus>>>,
}

impl Default for FleetState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn vehicle_input() -> VehicleInput {
        VehicleInput {
            tenant_id: Some("tenant-a".to_string()),
            plate_number: "B 1234 ABC".to_string(),
            make: "Toyota".to_string(),
            model: "Hiace".to_string(),
            year: 2022,
            vin: "1HGCM82633A004352".to_string(),
            vehicle_type: "van".to_string(),
            status: None,
            odometer_km: Some(10_000.0),
            fuel_type: Some("diesel".to_string()),
            acquisition_date: None,
            acquisition_cost: Some(350_000_000.0),
        }
    }

    fn driver_input() -> DriverInput {
        DriverInput {
            tenant_id: Some("tenant-a".to_string()),
            name: "Budi Santoso".to_string(),
            license_number: "SIM-001".to_string(),
            license_expiry: Utc::now().date_naive() + Duration::days(10),
            phone: Some("08123456789".to_string()),
            status: None,
        }
    }

    #[test]
    fn plate_and_vin_are_unique_per_tenant_only() {
        let state = FleetState::new();
        state.create_vehicle("tenant-a", vehicle_input()).unwrap();
        // Same plate in another tenant is fine.
        let mut other = vehicle_input();
        other.vin = "2HGCM82633A004353".to_string();
        assert!(state.create_vehicle("tenant-b", other).is_ok());
        // Same plate in the same tenant conflicts.
        let mut dup = vehicle_input();
        dup.vin = "3HGCM82633A004354".to_string();
        assert!(state.create_vehicle("tenant-a", dup).is_err());
        // Reads never cross tenants.
        assert_eq!(state.list_vehicles("tenant-a").unwrap().len(), 1);
        assert_eq!(state.list_vehicles("tenant-b").unwrap().len(), 1);
    }

    #[test]
    fn unknown_vehicle_type_is_rejected_until_configured() {
        let state = FleetState::new();
        let mut input = vehicle_input();
        input.vehicle_type = "pickup".to_string();
        assert!(state.create_vehicle("tenant-a", input).is_err());
        state
            .set_vehicle_types("tenant-a", vec!["pickup".to_string(), "van".to_string()])
            .unwrap();
        let mut input = vehicle_input();
        input.vehicle_type = "pickup".to_string();
        input.plate_number = "B 9999 ZZ".to_string();
        input.vin = "4HGCM82633A004355".to_string();
        assert!(state.create_vehicle("tenant-a", input).is_ok());
    }

    #[test]
    fn overlapping_trips_conflict_and_touching_windows_do_not() {
        let state = FleetState::new();
        let vehicle = state.create_vehicle("tenant-a", vehicle_input()).unwrap();
        let driver = state.create_driver("tenant-a", driver_input()).unwrap();
        let start = Utc.with_ymd_and_hms(2026, 11, 23, 8, 0, 0).unwrap();
        let trip = TripInput {
            tenant_id: Some("tenant-a".to_string()),
            vehicle_id: vehicle.id.clone(),
            driver_id: driver.id.clone(),
            origin: "Padang".to_string(),
            destination: "Bukittinggi".to_string(),
            planned_start: start,
            planned_end: start + Duration::hours(2),
            cargo_notes: None,
            start_odometer_km: 10_000.0,
            estimated_cost: None,
        };
        state.create_trip("tenant-a", trip).unwrap();
        // Overlapping window for the same vehicle conflicts.
        let clash = TripInput {
            tenant_id: Some("tenant-a".to_string()),
            vehicle_id: vehicle.id.clone(),
            driver_id: driver.id.clone(),
            origin: "Padang".to_string(),
            destination: "Solok".to_string(),
            planned_start: start + Duration::hours(1),
            planned_end: start + Duration::hours(3),
            cargo_notes: None,
            start_odometer_km: 10_000.0,
            estimated_cost: None,
        };
        assert!(state.create_trip("tenant-a", clash).is_err());
        // Touching the end of the first trip is free.
        let next = TripInput {
            tenant_id: Some("tenant-a".to_string()),
            vehicle_id: vehicle.id.clone(),
            driver_id: driver.id.clone(),
            origin: "Padang".to_string(),
            destination: "Solok".to_string(),
            planned_start: start + Duration::hours(2),
            planned_end: start + Duration::hours(4),
            cargo_notes: None,
            start_odometer_km: 10_000.0,
            estimated_cost: None,
        };
        assert!(state.create_trip("tenant-a", next).is_ok());
    }

    #[test]
    fn completing_a_trip_requires_monotonic_odometer_and_records_distance() {
        let state = FleetState::new();
        let vehicle = state.create_vehicle("tenant-a", vehicle_input()).unwrap();
        let driver = state.create_driver("tenant-a", driver_input()).unwrap();
        let start = Utc.with_ymd_and_hms(2026, 11, 23, 8, 0, 0).unwrap();
        let trip = state
            .create_trip(
                "tenant-a",
                TripInput {
                    tenant_id: Some("tenant-a".to_string()),
                    vehicle_id: vehicle.id.clone(),
                    driver_id: driver.id.clone(),
                    origin: "Padang".to_string(),
                    destination: "Bukittinggi".to_string(),
                    planned_start: start,
                    planned_end: start + Duration::hours(2),
                    cargo_notes: None,
                    start_odometer_km: 10_000.0,
                    estimated_cost: Some(150_000.0),
                },
            )
            .unwrap();
        // Cannot complete straight from planned.
        let patch = TripStatusPatch {
            tenant_id: Some("tenant-a".to_string()),
            status: "completed".to_string(),
            end_odometer_km: Some(10_120.0),
            actual_cost: None,
        };
        assert!(state.patch_trip_status("tenant-a", &trip.id, patch).is_err());
        state
            .patch_trip_status(
                "tenant-a",
                &trip.id,
                TripStatusPatch {
                    tenant_id: Some("tenant-a".to_string()),
                    status: "in_progress".to_string(),
                    end_odometer_km: None,
                    actual_cost: None,
                },
            )
            .unwrap();
        // End odometer below start is rejected.
        let bad = TripStatusPatch {
            tenant_id: Some("tenant-a".to_string()),
            status: "completed".to_string(),
            end_odometer_km: Some(9_999.0),
            actual_cost: None,
        };
        assert!(state.patch_trip_status("tenant-a", &trip.id, bad).is_err());
        let done = state
            .patch_trip_status(
                "tenant-a",
                &trip.id,
                TripStatusPatch {
                    tenant_id: Some("tenant-a".to_string()),
                    status: "completed".to_string(),
                    end_odometer_km: Some(10_120.0),
                    actual_cost: Some(180_000.0),
                },
            )
            .unwrap();
        assert_eq!(done.distance_km, Some(120.0));
        assert!(done.duration_minutes.is_some());
        // Vehicle odometer advanced to the trip end.
        assert_eq!(
            state.get_vehicle("tenant-a", &vehicle.id).unwrap().odometer_km,
            10_120.0
        );
    }

    #[test]
    fn fuel_efficiency_is_null_with_one_fill_up_and_correct_with_two() {
        let state = FleetState::new();
        let vehicle = state.create_vehicle("tenant-a", vehicle_input()).unwrap();
        state
            .add_fuel_log(
                "tenant-a",
                FuelInput {
                    tenant_id: Some("tenant-a".to_string()),
                    vehicle_id: vehicle.id.clone(),
                    litres: 50.0,
                    cost: 500_000.0,
                    odometer_km: 10_000.0,
                    station: Some("SPBU Padang".to_string()),
                },
            )
            .unwrap();
        let summary = state.fuel_summary("tenant-a", &vehicle.id).unwrap();
        assert!(summary["efficiency_km_per_litre"].is_null());
        // Odometer may not move backwards.
        let backwards = FuelInput {
            tenant_id: Some("tenant-a".to_string()),
            vehicle_id: vehicle.id.clone(),
            litres: 10.0,
            cost: 100_000.0,
            odometer_km: 9_999.0,
            station: None,
        };
        assert!(state.add_fuel_log("tenant-a", backwards).is_err());
        state
            .add_fuel_log(
                "tenant-a",
                FuelInput {
                    tenant_id: Some("tenant-a".to_string()),
                    vehicle_id: vehicle.id.clone(),
                    litres: 50.0,
                    cost: 500_000.0,
                    odometer_km: 10_500.0,
                    station: None,
                },
            )
            .unwrap();
        let summary = state.fuel_summary("tenant-a", &vehicle.id).unwrap();
        assert!((summary["efficiency_km_per_litre"].as_f64().unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn maintenance_due_uses_live_odometer_and_date() {
        let state = FleetState::new();
        let vehicle = state.create_vehicle("tenant-a", vehicle_input()).unwrap();
        let today = Utc::now().date_naive();
        // Due by odometer only once the vehicle reaches it.
        state
            .create_maintenance(
                "tenant-a",
                MaintenanceInput {
                    tenant_id: Some("tenant-a".to_string()),
                    vehicle_id: vehicle.id.clone(),
                    maintenance_type: "oil_change".to_string(),
                    description: None,
                    due_odometer_km: Some(10_500.0),
                    due_date: None,
                },
            )
            .unwrap();
        assert!(state.due_maintenance("tenant-a").unwrap().is_empty());
        // Past due date is due immediately.
        state
            .create_maintenance(
                "tenant-a",
                MaintenanceInput {
                    tenant_id: Some("tenant-a".to_string()),
                    vehicle_id: vehicle.id.clone(),
                    maintenance_type: "inspection".to_string(),
                    description: None,
                    due_odometer_km: None,
                    due_date: Some(today - Duration::days(1)),
                },
            )
            .unwrap();
        let due = state.due_maintenance("tenant-a").unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].maintenance_type, "inspection");
        // Driving the odometer past the threshold makes the first due.
        state
            .update_vehicle(
                "tenant-a",
                &vehicle.id,
                VehicleUpdate {
                    tenant_id: Some("tenant-a".to_string()),
                    plate_number: None,
                    make: None,
                    model: None,
                    year: None,
                    vin: None,
                    vehicle_type: None,
                    status: None,
                    odometer_km: Some(10_600.0),
                    fuel_type: None,
                    acquisition_date: None,
                    acquisition_cost: None,
                },
            )
            .unwrap();
        assert_eq!(state.due_maintenance("tenant-a").unwrap().len(), 2);
    }

    #[test]
    fn licence_expiring_soon_is_computed_on_read() {
        let state = FleetState::new();
        let driver = state.create_driver("tenant-a", driver_input()).unwrap();
        let view = state.get_driver("tenant-a", &driver.id).unwrap();
        assert!(view.license_expiring_soon);
        assert_eq!(view.days_until_license_expiry, 10);
        assert!(!view.license_expired);
    }

    #[test]
    fn costs_roll_up_fuel_maintenance_and_trips() {
        let state = FleetState::new();
        let vehicle = state.create_vehicle("tenant-a", vehicle_input()).unwrap();
        state
            .add_fuel_log(
                "tenant-a",
                FuelInput {
                    tenant_id: Some("tenant-a".to_string()),
                    vehicle_id: vehicle.id.clone(),
                    litres: 50.0,
                    cost: 500_000.0,
                    odometer_km: 10_000.0,
                    station: None,
                },
            )
            .unwrap();
        let item = state
            .create_maintenance(
                "tenant-a",
                MaintenanceInput {
                    tenant_id: Some("tenant-a".to_string()),
                    vehicle_id: vehicle.id.clone(),
                    maintenance_type: "oil_change".to_string(),
                    description: None,
                    due_odometer_km: Some(10_000.0),
                    due_date: None,
                },
            )
            .unwrap();
        let completed = state
            .complete_maintenance(
                "tenant-a",
                &item.id,
                MaintenanceComplete {
                    tenant_id: Some("tenant-a".to_string()),
                    cost: 250_000.0,
                    next_due_odometer_km: Some(15_000.0),
                    next_due_date: None,
                },
            )
            .unwrap();
        assert!(completed["next"].is_object());
        let costs = state.vehicle_costs("tenant-a", &vehicle.id).unwrap();
        assert_eq!(costs["total_cost"].as_f64().unwrap(), 750_000.0);
        assert_eq!(costs["breakdown"]["fuel"]["cost"].as_f64().unwrap(), 500_000.0);
        assert_eq!(
            costs["breakdown"]["maintenance"]["cost"].as_f64().unwrap(),
            250_000.0
        );
        // The follow-up item is pending, not counted as a cost.
        assert_eq!(state.list_maintenance("tenant-a").unwrap().len(), 2);
    }
}
