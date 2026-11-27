//! Fleet state.

use super::*;

impl FleetState {

    // -- fuel ----------------------------------------------------------

    pub fn add_fuel_log(&self, tenant_id: &str, input: FuelInput) -> UnifiedResult<FuelLog> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let litres = validate_positive(input.litres, "litres")?;
        let cost = validate_non_negative(input.cost, "cost")?;
        let odometer = validate_non_negative(input.odometer_km, "odometer_km")?;
        let mut vehicle = self.get_vehicle(&tenant_id, &input.vehicle_id)?;
        let last_odometer = self
            .fuel_logs
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id && entry.vehicle_id == vehicle.id)
            .map(|entry| entry.odometer_km)
            .fold(vehicle.odometer_km, f64::max);
        if odometer < last_odometer {
            return Err(UnifiedError::unprocessable(
                "odometer_km must be >= the last recorded odometer for this vehicle",
            ));
        }
        let log = FuelLog {
            id: new_id(),
            tenant_id: tenant_id.clone(),
            vehicle_id: vehicle.id.clone(),
            litres,
            cost,
            odometer_km: odometer,
            station: input.station.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            logged_at: now(),
            created_at: now(),
        };
        self.fuel_logs.insert(log.id.clone(), log.clone());
        if odometer > vehicle.odometer_km {
            vehicle.odometer_km = odometer;
            vehicle.updated_at = now();
            self.vehicles.insert(vehicle.id.clone(), vehicle.clone());
        }
        self.check_and_emit_due(&tenant_id, &vehicle.id);
        Ok(log)
    }


    /// Fuel logs for one vehicle plus the computed efficiency.
    /// `efficiency_km_per_litre` is null when fewer than two
    /// fill-ups exist (see `fuel_efficiency_km_per_litre`).
    pub fn fuel_summary(
        &self,
        tenant_id: &str,
        vehicle_id: &str,
    ) -> UnifiedResult<Value> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let vehicle = self.get_vehicle(&tenant_id, vehicle_id)?;
        let mut logs: Vec<FuelLog> = self
            .fuel_logs
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id && entry.vehicle_id == vehicle.id)
            .map(|entry| entry.clone())
            .collect();
        logs.sort_by(|a, b| {
            a.odometer_km
                .partial_cmp(&b.odometer_km)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.logged_at.cmp(&b.logged_at))
        });
        let points: Vec<(f64, f64)> = logs
            .iter()
            .map(|log| (log.odometer_km, log.litres))
            .collect();
        let efficiency = fuel_efficiency_km_per_litre(&points);
        let intervals: Vec<Value> = logs
            .windows(2)
            .map(|window| {
                let distance = window[1].odometer_km - window[0].odometer_km;
                let interval_efficiency = if distance > 0.0 && window[1].litres > 0.0 {
                    Some(distance / window[1].litres)
                } else {
                    None
                };
                json!({
                    "from_odometer_km": window[0].odometer_km,
                    "to_odometer_km": window[1].odometer_km,
                    "distance_km": distance,
                    "litres": window[1].litres,
                    "efficiency_km_per_litre": interval_efficiency,
                })
            })
            .collect();
        Ok(json!({
            "vehicle_id": vehicle.id,
            "tenant_id": tenant_id,
            "logs": logs,
            "intervals": intervals,
            "efficiency_km_per_litre": efficiency,
            "total_litres": logs.iter().map(|log| log.litres).sum::<f64>(),
            "total_cost": logs.iter().map(|log| log.cost).sum::<f64>(),
        }))
    }


    // -- maintenance ---------------------------------------------------

    pub fn create_maintenance(
        &self,
        tenant_id: &str,
        input: MaintenanceInput,
    ) -> UnifiedResult<MaintenanceItem> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let vehicle = self.get_vehicle(&tenant_id, &input.vehicle_id)?;
        if input.due_odometer_km.is_none() && input.due_date.is_none() {
            return Err(UnifiedError::bad_request(
                "at least one of due_odometer_km or due_date is required",
            ));
        }
        let due_odometer = match input.due_odometer_km {
            Some(value) => Some(validate_non_negative(value, "due_odometer_km")?),
            None => None,
        };
        let maintenance_type = input.maintenance_type.trim().to_string();
        if maintenance_type.is_empty() {
            return Err(UnifiedError::bad_request("type is required"));
        }
        let item = MaintenanceItem {
            id: new_id(),
            tenant_id,
            vehicle_id: vehicle.id,
            maintenance_type,
            description: input
                .description
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty()),
            due_odometer_km: due_odometer,
            due_date: input.due_date,
            status: "pending".to_string(),
            completed_cost: None,
            completed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        self.maintenance.insert(item.id.clone(), item.clone());
        Ok(item)
    }


    pub fn list_maintenance(&self, tenant_id: &str) -> UnifiedResult<Vec<MaintenanceItem>> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let mut out: Vec<MaintenanceItem> = self
            .maintenance
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id)
            .map(|entry| entry.clone())
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(out)
    }


    /// A pending item is due when the vehicle's live odometer has
    /// reached its due odometer OR its due date has passed.
    pub fn is_maintenance_due(&self, item: &MaintenanceItem) -> bool {
        if item.status != "pending" {
            return false;
        }
        let vehicle = match self.vehicles.get(&item.vehicle_id) {
            Some(entry) => entry.clone(),
            None => return false,
        };
        let odometer_due = item
            .due_odometer_km
            .is_some_and(|due| vehicle.odometer_km >= due);
        let date_due = item
            .due_date
            .is_some_and(|due| due <= Utc::now().date_naive());
        odometer_due || date_due
    }


    /// Pending items due right now, computed from live vehicle data.
    pub fn due_maintenance(&self, tenant_id: &str) -> UnifiedResult<Vec<MaintenanceItem>> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let due: Vec<MaintenanceItem> = self
            .maintenance
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id && self.is_maintenance_due(entry))
            .map(|entry| entry.clone())
            .collect();
        for item in &due {
            self.emit(
                EVENT_MAINTENANCE_DUE,
                json!({
                    "tenant_id": item.tenant_id,
                    "maintenance_id": item.id,
                    "vehicle_id": item.vehicle_id,
                    "type": item.maintenance_type,
                    "due_odometer_km": item.due_odometer_km,
                    "due_date": item.due_date,
                }),
            );
        }
        Ok(due)
    }


    /// Emit `fleet.maintenance.due` for any pending item of this
    /// vehicle that just became due (called after odometer changes).
    pub(crate) fn check_and_emit_due(&self, tenant_id: &str, vehicle_id: &str) {
        let due: Vec<MaintenanceItem> = self
            .maintenance
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id && entry.vehicle_id == vehicle_id)
            .map(|entry| entry.clone())
            .filter(|item| self.is_maintenance_due(item))
            .collect();
        for item in due {
            self.emit(
                EVENT_MAINTENANCE_DUE,
                json!({
                    "tenant_id": item.tenant_id,
                    "maintenance_id": item.id,
                    "vehicle_id": item.vehicle_id,
                    "type": item.maintenance_type,
                    "due_odometer_km": item.due_odometer_km,
                    "due_date": item.due_date,
                }),
            );
        }
    }


    /// Complete a maintenance item: record the cost, mark it
    /// completed, and - when next due values are given - create the
    /// follow-up pending item that resets the schedule.
    pub fn complete_maintenance(
        &self,
        tenant_id: &str,
        id: &str,
        input: MaintenanceComplete,
    ) -> UnifiedResult<Value> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let cost = validate_non_negative(input.cost, "cost")?;
        let next_odometer = match input.next_due_odometer_km {
            Some(value) => Some(validate_non_negative(value, "next_due_odometer_km")?),
            None => None,
        };
        let mut item = match self.maintenance.get(id) {
            Some(entry) if entry.tenant_id == tenant_id => entry.clone(),
            _ => return Err(UnifiedError::not_found("maintenance item not found")),
        };
        if item.status == "completed" {
            return Err(UnifiedError::conflict("maintenance item is already completed"));
        }
        item.status = "completed".to_string();
        item.completed_cost = Some(cost);
        item.completed_at = Some(now());
        item.updated_at = now();
        self.maintenance.insert(id.to_string(), item.clone());
        let mut next_item: Option<MaintenanceItem> = None;
        if next_odometer.is_some() || input.next_due_date.is_some() {
            let follow_up = MaintenanceItem {
                id: new_id(),
                tenant_id: tenant_id.clone(),
                vehicle_id: item.vehicle_id.clone(),
                maintenance_type: item.maintenance_type.clone(),
                description: item.description.clone(),
                due_odometer_km: next_odometer,
                due_date: input.next_due_date,
                status: "pending".to_string(),
                completed_cost: None,
                completed_at: None,
                created_at: now(),
                updated_at: now(),
            };
            self.maintenance
                .insert(follow_up.id.clone(), follow_up.clone());
            next_item = Some(follow_up);
        }
        Ok(json!({
            "completed": item,
            "next": next_item,
        }))
    }


    // -- costs ---------------------------------------------------------

    /// Cost rollup for one vehicle: fuel + completed maintenance +
    /// completed trips, with a per-category breakdown.
    pub fn vehicle_costs(&self, tenant_id: &str, vehicle_id: &str) -> UnifiedResult<Value> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let vehicle = self.get_vehicle(&tenant_id, vehicle_id)?;
        let fuel_cost: f64 = self
            .fuel_logs
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id && entry.vehicle_id == vehicle.id)
            .map(|entry| entry.cost)
            .sum();
        let fuel_count = self
            .fuel_logs
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id && entry.vehicle_id == vehicle.id)
            .count();
        let maintenance_cost: f64 = self
            .maintenance
            .iter()
            .filter(|entry| {
                entry.tenant_id == tenant_id
                    && entry.vehicle_id == vehicle.id
                    && entry.status == "completed"
            })
            .map(|entry| entry.completed_cost.unwrap_or(0.0))
            .sum();
        let maintenance_count = self
            .maintenance
            .iter()
            .filter(|entry| {
                entry.tenant_id == tenant_id
                    && entry.vehicle_id == vehicle.id
                    && entry.status == "completed"
            })
            .count();
        let trip_cost: f64 = self
            .trips
            .iter()
            .filter(|entry| {
                entry.tenant_id == tenant_id
                    && entry.vehicle_id == vehicle.id
                    && entry.status == "completed"
            })
            .map(|entry| entry.actual_cost.or(entry.estimated_cost).unwrap_or(0.0))
            .sum();
        let trip_count = self
            .trips
            .iter()
            .filter(|entry| {
                entry.tenant_id == tenant_id
                    && entry.vehicle_id == vehicle.id
                    && entry.status == "completed"
            })
            .count();
        Ok(json!({
            "vehicle_id": vehicle.id,
            "tenant_id": tenant_id,
            "total_cost": fuel_cost + maintenance_cost + trip_cost,
            "breakdown": {
                "fuel": { "cost": fuel_cost, "entries": fuel_count },
                "maintenance": { "cost": maintenance_cost, "entries": maintenance_count },
                "trips": { "cost": trip_cost, "entries": trip_count },
            },
        }))
    }
}
