//! Fleet state.

use super::*;

impl FleetState {
    pub fn new() -> Self {
        Self {
            vehicles: DashMap::new(),
            drivers: DashMap::new(),
            trips: DashMap::new(),
            positions: DashMap::new(),
            fuel_logs: DashMap::new(),
            maintenance: DashMap::new(),
            vehicle_types: RwLock::new(HashMap::new()),
            hooks: RwLock::new(None),
        }
    }


    /// Attach the hook bus (called from `Module::init`).
    pub fn set_hooks(&self, bus: Arc<HookBus>) {
        if let Ok(mut guard) = self.hooks.write() {
            *guard = Some(bus);
        }
    }


    pub(crate) fn emit(&self, event: &str, data: Value) {
        let bus = self
            .hooks
            .read()
            .ok()
            .and_then(|guard| guard.clone());
        if let Some(bus) = bus {
            bus.emit(event, data);
        }
    }


    // -- vehicle types (configurable, per tenant) ---------------------

    pub fn vehicle_types_for(&self, tenant_id: &str) -> Vec<String> {
        self.vehicle_types
            .read()
            .ok()
            .and_then(|map| map.get(tenant_id).cloned())
            .unwrap_or_else(|| {
                DEFAULT_VEHICLE_TYPES
                    .iter()
                    .map(|t| t.to_string())
                    .collect()
            })
    }


    pub fn set_vehicle_types(
        &self,
        tenant_id: &str,
        types: Vec<String>,
    ) -> UnifiedResult<Vec<String>> {
        let cleaned: Vec<String> = types
            .into_iter()
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>();
        if cleaned.is_empty() {
            return Err(UnifiedError::bad_request(
                "types must contain at least one non-empty vehicle type",
            ));
        }
        let mut deduped: Vec<String> = Vec::new();
        for item in cleaned {
            if !deduped.contains(&item) {
                deduped.push(item);
            }
        }
        if let Ok(mut map) = self.vehicle_types.write() {
            map.insert(tenant_id.to_string(), deduped.clone());
        }
        Ok(deduped)
    }


    fn require_vehicle_type(&self, tenant_id: &str, vehicle_type: &str) -> UnifiedResult<String> {
        let wanted = vehicle_type.trim().to_lowercase();
        if wanted.is_empty() {
            return Err(UnifiedError::bad_request("type is required"));
        }
        let allowed = self.vehicle_types_for(tenant_id);
        if !allowed.iter().any(|t| t == &wanted) {
            return Err(UnifiedError::bad_request(format!(
                "type '{wanted}' is not in this tenant's vehicle type list: {}",
                allowed.join(", ")
            )));
        }
        Ok(wanted)
    }


    // -- vehicles ------------------------------------------------------

    pub fn create_vehicle(&self, tenant_id: &str, input: VehicleInput) -> UnifiedResult<Vehicle> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let plate = validate_plate(&input.plate_number)?;
        let vin = validate_vin(&input.vin)?;
        let year = validate_year(input.year)?;
        let vehicle_type = self.require_vehicle_type(&tenant_id, &input.vehicle_type)?;
        let odometer = validate_non_negative(input.odometer_km.unwrap_or(0.0), "odometer_km")?;
        let status = input.status.as_deref().unwrap_or("active");
        if !matches!(status, "active" | "maintenance" | "retired") {
            return Err(UnifiedError::bad_request(
                "status must be active, maintenance or retired",
            ));
        }
        let make = input.make.trim().to_string();
        let model = input.model.trim().to_string();
        if make.is_empty() || model.is_empty() {
            return Err(UnifiedError::bad_request("make and model are required"));
        }
        let fuel_type = input
            .fuel_type
            .unwrap_or_else(|| "diesel".to_string())
            .trim()
            .to_string();
        if fuel_type.is_empty() {
            return Err(UnifiedError::bad_request("fuel_type must not be empty"));
        }
        let acquisition_cost = match input.acquisition_cost {
            Some(cost) => Some(validate_non_negative(cost, "acquisition_cost")?),
            None => None,
        };
        let plate_key = normalise_plate(&plate);
        for existing in self.vehicles.iter() {
            if existing.tenant_id == tenant_id {
                if normalise_plate(&existing.plate_number) == plate_key {
                    return Err(UnifiedError::conflict(format!(
                        "plate_number {plate} already exists for this tenant"
                    )));
                }
                if existing.vin.eq_ignore_ascii_case(&vin) {
                    return Err(UnifiedError::conflict(
                        "vin already exists for this tenant",
                    ));
                }
            }
        }
        let vehicle = Vehicle {
            id: new_id(),
            tenant_id,
            plate_number: plate,
            make,
            model,
            year,
            vin,
            vehicle_type,
            status: status.to_string(),
            odometer_km: odometer,
            fuel_type,
            acquisition_date: input.acquisition_date,
            acquisition_cost,
            last_latitude: None,
            last_longitude: None,
            last_position_at: None,
            created_at: now(),
            updated_at: now(),
        };
        self.vehicles.insert(vehicle.id.clone(), vehicle.clone());
        Ok(vehicle)
    }


    pub fn list_vehicles(&self, tenant_id: &str) -> UnifiedResult<Vec<Vehicle>> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let mut out: Vec<Vehicle> = self
            .vehicles
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id)
            .map(|entry| entry.clone())
            .collect();
        out.sort_by(|a, b| a.plate_number.cmp(&b.plate_number));
        Ok(out)
    }


    /// Fetch one vehicle, scoped to the tenant. A record owned by
    /// another tenant is reported as not found (never leaked).
    pub fn get_vehicle(&self, tenant_id: &str, id: &str) -> UnifiedResult<Vehicle> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        match self.vehicles.get(id) {
            Some(entry) if entry.tenant_id == tenant_id => Ok(entry.clone()),
            _ => Err(UnifiedError::not_found("vehicle not found")),
        }
    }


    pub fn update_vehicle(
        &self,
        tenant_id: &str,
        id: &str,
        input: VehicleUpdate,
    ) -> UnifiedResult<Vehicle> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let mut vehicle = self.get_vehicle(&tenant_id, id)?;
        if let Some(plate) = &input.plate_number {
            let plate = validate_plate(plate)?;
            let key = normalise_plate(&plate);
            for existing in self.vehicles.iter() {
                if existing.tenant_id == tenant_id
                    && existing.id != id
                    && normalise_plate(&existing.plate_number) == key
                {
                    return Err(UnifiedError::conflict(format!(
                        "plate_number {plate} already exists for this tenant"
                    )));
                }
            }
            vehicle.plate_number = plate;
        }
        if let Some(vin) = &input.vin {
            let vin = validate_vin(vin)?;
            for existing in self.vehicles.iter() {
                if existing.tenant_id == tenant_id
                    && existing.id != id
                    && existing.vin.eq_ignore_ascii_case(&vin)
                {
                    return Err(UnifiedError::conflict(
                        "vin already exists for this tenant",
                    ));
                }
            }
            vehicle.vin = vin;
        }
        if let Some(year) = input.year {
            vehicle.year = validate_year(year)?;
        }
        if let Some(vehicle_type) = &input.vehicle_type {
            vehicle.vehicle_type = self.require_vehicle_type(&tenant_id, vehicle_type)?;
        }
        if let Some(status) = &input.status {
            vehicle_status_transition(&vehicle.status, status)?;
            vehicle.status = status.to_string();
        }
        if let Some(odometer) = input.odometer_km {
            let odometer = validate_non_negative(odometer, "odometer_km")?;
            if odometer < vehicle.odometer_km {
                return Err(UnifiedError::unprocessable(
                    "odometer_km cannot move backwards",
                ));
            }
            vehicle.odometer_km = odometer;
        }
        if let Some(make) = &input.make {
            let make = make.trim().to_string();
            if make.is_empty() {
                return Err(UnifiedError::bad_request("make must not be empty"));
            }
            vehicle.make = make;
        }
        if let Some(model) = &input.model {
            let model = model.trim().to_string();
            if model.is_empty() {
                return Err(UnifiedError::bad_request("model must not be empty"));
            }
            vehicle.model = model;
        }
        if let Some(fuel_type) = &input.fuel_type {
            let fuel_type = fuel_type.trim().to_string();
            if fuel_type.is_empty() {
                return Err(UnifiedError::bad_request("fuel_type must not be empty"));
            }
            vehicle.fuel_type = fuel_type;
        }
        if input.acquisition_date.is_some() {
            vehicle.acquisition_date = input.acquisition_date;
        }
        if let Some(cost) = input.acquisition_cost {
            vehicle.acquisition_cost = Some(validate_non_negative(cost, "acquisition_cost")?);
        }
        let retired = vehicle.status == "retired";
        vehicle.updated_at = now();
        self.vehicles.insert(id.to_string(), vehicle.clone());
        if retired {
            self.emit(
                EVENT_VEHICLE_RETIRED,
                json!({"tenant_id": vehicle.tenant_id, "vehicle_id": vehicle.id, "plate_number": vehicle.plate_number}),
            );
        }
        Ok(vehicle)
    }


    pub fn delete_vehicle(&self, tenant_id: &str, id: &str) -> UnifiedResult<()> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let _vehicle = self.get_vehicle(&tenant_id, id)?;
        for trip in self.trips.iter() {
            if trip.tenant_id == tenant_id
                && trip.vehicle_id == id
                && matches!(trip.status.as_str(), "planned" | "in_progress")
            {
                return Err(UnifiedError::conflict(
                    "vehicle has an active trip and cannot be deleted",
                ));
            }
        }
        self.vehicles.remove(id);
        self.positions.remove(id);
        Ok(())
    }
}
