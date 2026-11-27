//! Fleet state.

use super::*;

impl FleetState {

    // -- trips ---------------------------------------------------------

    /// True when the vehicle or driver already has a live trip whose
    /// planned window overlaps `[start, end)`.
    fn has_overlapping_trip(
        &self,
        tenant_id: &str,
        vehicle_id: &str,
        driver_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        exclude_id: Option<&str>,
    ) -> bool {
        self.trips.iter().any(|entry| {
            let trip = entry.value();
            trip.tenant_id == tenant_id
                && Some(trip.id.as_str()) != exclude_id
                && matches!(trip.status.as_str(), "planned" | "in_progress")
                && (trip.vehicle_id == vehicle_id || trip.driver_id == driver_id)
                && windows_overlap(trip.planned_start, trip.planned_end, start, end)
        })
    }


    pub fn create_trip(&self, tenant_id: &str, input: TripInput) -> UnifiedResult<Trip> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        crate::validation::validate_time_window(input.planned_start, input.planned_end)?;
        let start_odometer =
            validate_non_negative(input.start_odometer_km, "start_odometer_km")?;
        let estimated_cost = match input.estimated_cost {
            Some(cost) => Some(validate_non_negative(cost, "estimated_cost")?),
            None => None,
        };
        let vehicle = self.get_vehicle(&tenant_id, &input.vehicle_id)?;
        if vehicle.status != "active" {
            return Err(UnifiedError::unprocessable(
                "vehicle must be active to take a trip",
            ));
        }
        if start_odometer < vehicle.odometer_km {
            return Err(UnifiedError::unprocessable(
                "start_odometer_km must be >= the vehicle's current odometer",
            ));
        }
        let driver = match self.drivers.get(&input.driver_id) {
            Some(entry) if entry.tenant_id == tenant_id => entry.clone(),
            _ => return Err(UnifiedError::not_found("driver not found")),
        };
        if driver.status != "active" {
            return Err(UnifiedError::unprocessable(
                "driver must be active to take a trip",
            ));
        }
        if self.has_overlapping_trip(
            &tenant_id,
            &vehicle.id,
            &driver.id,
            input.planned_start,
            input.planned_end,
            None,
        ) {
            return Err(UnifiedError::conflict(
                "vehicle or driver already has a trip overlapping this time window",
            ));
        }
        let origin = input.origin.trim().to_string();
        let destination = input.destination.trim().to_string();
        if origin.is_empty() || destination.is_empty() {
            return Err(UnifiedError::bad_request(
                "origin and destination are required",
            ));
        }
        let trip = Trip {
            id: new_id(),
            tenant_id,
            vehicle_id: vehicle.id,
            driver_id: driver.id,
            origin,
            destination,
            planned_start: input.planned_start,
            planned_end: input.planned_end,
            cargo_notes: input
                .cargo_notes
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty()),
            status: "planned".to_string(),
            start_odometer_km: start_odometer,
            end_odometer_km: None,
            distance_km: None,
            duration_minutes: None,
            estimated_cost,
            actual_cost: None,
            started_at: None,
            completed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        self.trips.insert(trip.id.clone(), trip.clone());
        Ok(trip)
    }


    pub fn list_trips(&self, tenant_id: &str) -> UnifiedResult<Vec<Trip>> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let mut out: Vec<Trip> = self
            .trips
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id)
            .map(|entry| entry.clone())
            .collect();
        out.sort_by(|a, b| b.planned_start.cmp(&a.planned_start));
        Ok(out)
    }


    pub fn get_trip(&self, tenant_id: &str, id: &str) -> UnifiedResult<Trip> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        match self.trips.get(id) {
            Some(entry) if entry.tenant_id == tenant_id => Ok(entry.clone()),
            _ => Err(UnifiedError::not_found("trip not found")),
        }
    }


    pub fn patch_trip_status(
        &self,
        tenant_id: &str,
        id: &str,
        patch: TripStatusPatch,
    ) -> UnifiedResult<Trip> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let mut trip = self.get_trip(&tenant_id, id)?;
        crate::validation::trip_status_transition(&trip.status, &patch.status)?;
        let now = now();
        match patch.status.as_str() {
            "in_progress" => {
                trip.started_at = Some(now);
            }
            "completed" => {
                let end_odometer = match patch.end_odometer_km {
                    Some(value) => validate_non_negative(value, "end_odometer_km")?,
                    None => {
                        return Err(UnifiedError::bad_request(
                            "end_odometer_km is required to complete a trip",
                        ));
                    }
                };
                if end_odometer < trip.start_odometer_km {
                    return Err(UnifiedError::unprocessable(
                        "end_odometer_km must be >= start_odometer_km",
                    ));
                }
                let actual_cost = match patch.actual_cost {
                    Some(cost) => Some(validate_non_negative(cost, "actual_cost")?),
                    None => None,
                };
                trip.end_odometer_km = Some(end_odometer);
                trip.distance_km = Some(end_odometer - trip.start_odometer_km);
                trip.completed_at = Some(now);
                trip.actual_cost = actual_cost;
                if let Some(started_at) = trip.started_at {
                    trip.duration_minutes =
                        Some((now - started_at).num_seconds() as f64 / 60.0);
                }
                // The vehicle's odometer advances to the trip end.
                if let Some(mut vehicle) = self.vehicles.get_mut(&trip.vehicle_id) {
                    if vehicle.tenant_id == tenant_id && end_odometer > vehicle.odometer_km {
                        vehicle.odometer_km = end_odometer;
                        vehicle.updated_at = now;
                    }
                }
            }
            _ => {}
        }
        trip.status = patch.status.clone();
        trip.updated_at = now;
        self.trips.insert(id.to_string(), trip.clone());
        if trip.status == "completed" {
            self.emit(
                EVENT_TRIP_COMPLETED,
                json!({
                    "tenant_id": trip.tenant_id,
                    "trip_id": trip.id,
                    "vehicle_id": trip.vehicle_id,
                    "driver_id": trip.driver_id,
                    "distance_km": trip.distance_km,
                    "duration_minutes": trip.duration_minutes,
                }),
            );
            self.check_and_emit_due(&trip.tenant_id, &trip.vehicle_id);
        }
        Ok(trip)
    }


    // -- GPS positions -------------------------------------------------

    pub fn add_position(
        &self,
        tenant_id: &str,
        vehicle_id: &str,
        input: PositionInput,
    ) -> UnifiedResult<GpsPosition> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        crate::validation::validate_position(
            input.latitude,
            input.longitude,
            input.speed_kmh,
            input.recorded_at,
        )?;
        let mut vehicle = self.get_vehicle(&tenant_id, vehicle_id)?;
        let position = GpsPosition {
            id: new_id(),
            tenant_id: tenant_id.clone(),
            vehicle_id: vehicle.id.clone(),
            latitude: input.latitude,
            longitude: input.longitude,
            speed_kmh: input.speed_kmh,
            recorded_at: input.recorded_at,
            received_at: now(),
        };
        let mut track = self
            .positions
            .entry(vehicle.id.clone())
            .or_insert_with(VecDeque::new);
        track.push_back(position.clone());
        while track.len() > MAX_POSITIONS_PER_VEHICLE {
            track.pop_front();
        }
        drop(track);
        vehicle.last_latitude = Some(input.latitude);
        vehicle.last_longitude = Some(input.longitude);
        vehicle.last_position_at = Some(input.recorded_at);
        vehicle.updated_at = now();
        self.vehicles
            .insert(vehicle.id.clone(), vehicle.clone());
        Ok(position)
    }


    pub fn list_positions(
        &self,
        tenant_id: &str,
        vehicle_id: &str,
    ) -> UnifiedResult<Vec<GpsPosition>> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let _vehicle = self.get_vehicle(&tenant_id, vehicle_id)?;
        Ok(self
            .positions
            .get(vehicle_id)
            .map(|track| track.iter().cloned().collect())
            .unwrap_or_default())
    }
}
