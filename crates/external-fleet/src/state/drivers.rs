//! Fleet state.

use super::*;

impl FleetState {

    // -- drivers -------------------------------------------------------

    pub fn create_driver(&self, tenant_id: &str, input: DriverInput) -> UnifiedResult<Driver> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("name is required"));
        }
        let license = input.license_number.trim().to_string();
        if license.is_empty() {
            return Err(UnifiedError::bad_request("license_number is required"));
        }
        let status = input.status.as_deref().unwrap_or("active");
        if !matches!(status, "active" | "suspended") {
            return Err(UnifiedError::bad_request(
                "status must be active or suspended",
            ));
        }
        for existing in self.drivers.iter() {
            if existing.tenant_id == tenant_id
                && existing.license_number.eq_ignore_ascii_case(&license)
            {
                return Err(UnifiedError::conflict(
                    "license_number already exists for this tenant",
                ));
            }
        }
        let driver = Driver {
            id: new_id(),
            tenant_id,
            name,
            license_number: license,
            license_expiry: input.license_expiry,
            phone: input.phone.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
            status: status.to_string(),
            created_at: now(),
            updated_at: now(),
        };
        self.drivers.insert(driver.id.clone(), driver.clone());
        Ok(driver)
    }


    pub fn driver_view(driver: &Driver) -> DriverView {
        let days = (driver.license_expiry - Utc::now().date_naive()).num_days();
        DriverView {
            days_until_license_expiry: days,
            license_expiring_soon: licence_expiring_soon(driver.license_expiry),
            license_expired: days < 0,
            driver: driver.clone(),
        }
    }


    pub fn list_drivers(&self, tenant_id: &str) -> UnifiedResult<Vec<DriverView>> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let mut out: Vec<DriverView> = self
            .drivers
            .iter()
            .filter(|entry| entry.tenant_id == tenant_id)
            .map(|entry| Self::driver_view(&entry))
            .collect();
        out.sort_by(|a, b| a.driver.name.cmp(&b.driver.name));
        Ok(out)
    }


    pub fn get_driver(&self, tenant_id: &str, id: &str) -> UnifiedResult<DriverView> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        match self.drivers.get(id) {
            Some(entry) if entry.tenant_id == tenant_id => Ok(Self::driver_view(&entry)),
            _ => Err(UnifiedError::not_found("driver not found")),
        }
    }


    pub fn update_driver(
        &self,
        tenant_id: &str,
        id: &str,
        input: DriverUpdate,
    ) -> UnifiedResult<DriverView> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        let mut driver = match self.drivers.get(id) {
            Some(entry) if entry.tenant_id == tenant_id => entry.clone(),
            _ => return Err(UnifiedError::not_found("driver not found")),
        };
        if let Some(name) = &input.name {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err(UnifiedError::bad_request("name must not be empty"));
            }
            driver.name = name;
        }
        if let Some(license) = &input.license_number {
            let license = license.trim().to_string();
            if license.is_empty() {
                return Err(UnifiedError::bad_request("license_number must not be empty"));
            }
            for existing in self.drivers.iter() {
                if existing.tenant_id == tenant_id
                    && existing.id != id
                    && existing.license_number.eq_ignore_ascii_case(&license)
                {
                    return Err(UnifiedError::conflict(
                        "license_number already exists for this tenant",
                    ));
                }
            }
            driver.license_number = license;
        }
        if let Some(expiry) = input.license_expiry {
            driver.license_expiry = expiry;
        }
        if let Some(phone) = &input.phone {
            driver.phone = Some(phone.trim().to_string()).filter(|p| !p.is_empty());
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "suspended") {
                return Err(UnifiedError::bad_request(
                    "status must be active or suspended",
                ));
            }
            driver.status = status.to_string();
        }
        driver.updated_at = now();
        self.drivers.insert(id.to_string(), driver.clone());
        Ok(Self::driver_view(&driver))
    }


    pub fn delete_driver(&self, tenant_id: &str, id: &str) -> UnifiedResult<()> {
        let tenant_id = validate_tenant_id(tenant_id)?;
        match self.drivers.get(id) {
            Some(entry) if entry.tenant_id == tenant_id => {}
            _ => return Err(UnifiedError::not_found("driver not found")),
        }
        for trip in self.trips.iter() {
            if trip.tenant_id == tenant_id
                && trip.driver_id == id
                && matches!(trip.status.as_str(), "planned" | "in_progress")
            {
                return Err(UnifiedError::conflict(
                    "driver has an active trip and cannot be deleted",
                ));
            }
        }
        self.drivers.remove(id);
        Ok(())
    }
}
