// Tenant Destroy module - destroy settings and the settings store.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::{
    drop_tenant_schema, is_valid_schema_name, schema_exists, schema_name,
    EVENT_TENANT_DESTROY_REQUESTED, EVENT_TENANT_DESTROYED, EVENT_TENANT_SUSPENDED,
};
use unified_core::db::DbPool;
use crate::modules::import_export::import_export_events;
use crate::modules::tenants::{
    begin_deprovision, finish_deprovision_as_deleted, set_export_reference, TenantsState,
    STATUS_DEPROVISIONING,
};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;
use super::*;

// Configurable settings (admin-editable; defaults are defaults only)

/// All behaviour-shaping defaults of this module. An administrator
/// edits these through `PUT /destroy-settings`; every validation and
/// default in this file reads the live value, so changing a value
/// here changes what the module accepts without any code change.
/// The values below are CONFIGURABLE DEFAULTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantDestroySettings {
    /// Grace period in days between suspension and the final destroy.
    /// Stored as an explicit expiry timestamp on each request.
    pub grace_period_days: i64,
    /// Minimum tenant age in days that requires a second approver.
    /// `None` disables the age rule.
    pub second_approval_min_tenant_age_days: Option<i64>,
    /// Minimum tenant data size in bytes that requires a second
    /// approver. `None` disables the size rule. When both rules are
    /// disabled, no second approval is ever required.
    pub second_approval_min_data_size_bytes: Option<u64>,
    /// Failed typed confirmations allowed before the confirmation
    /// locks for `confirmation_lockout_minutes`.
    pub max_failed_confirmations: u32,
    /// Lockout in minutes after `max_failed_confirmations` failures.
    pub confirmation_lockout_minutes: i64,
    /// Entity name requested from the import/export module for the
    /// full tenant export.
    pub export_entity: String,
    /// Maximum days a destroy request may sit in `export_pending`
    /// before it is reported as stale by the list view. The request
    /// is never auto-cancelled; this only drives the `export_stale`
    /// flag so operators can see stuck requests.
    pub export_pending_stale_days: i64,
}

impl Default for TenantDestroySettings {
    fn default() -> Self {
        Self {
            grace_period_days: 30,
            second_approval_min_tenant_age_days: Some(365),
            second_approval_min_data_size_bytes: Some(10 * 1024 * 1024 * 1024),
            max_failed_confirmations: 5,
            confirmation_lockout_minutes: 30,
            export_entity: "tenant".to_string(),
            export_pending_stale_days: 7,
        }
    }
}

pub(crate) static SETTINGS: OnceLock<RwLock<TenantDestroySettings>> = OnceLock::new();

pub(crate) fn settings_store() -> &'static RwLock<TenantDestroySettings> {
    SETTINGS.get_or_init(|| RwLock::new(TenantDestroySettings::default()))
}

/// Current settings (cloned). Public so the integrator and tests can
/// read the effective defaults without an HTTP call.
pub fn destroy_settings() -> TenantDestroySettings {
    settings_store()
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default()
}

/// Persist settings, failing loudly on a poisoned lock instead of
/// silently dropping the write while the caller reports success.
pub(crate) fn write_destroy_settings(
    lock: &RwLock<TenantDestroySettings>,
    settings: &TenantDestroySettings,
) -> UnifiedResult<()> {
    let mut guard = lock.write().map_err(|_| {
        UnifiedError::internal("Destroy settings lock is poisoned; settings were not saved")
    })?;
    *guard = settings.clone();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_settings_stores_the_new_values() {
        let lock = RwLock::new(TenantDestroySettings::default());
        let mut updated = TenantDestroySettings::default();
        updated.grace_period_days = 14;
        write_destroy_settings(&lock, &updated).unwrap();
        assert_eq!(lock.read().unwrap().grace_period_days, 14);
    }

    #[test]
    fn write_settings_fails_on_a_poisoned_lock() {
        let lock = RwLock::new(TenantDestroySettings::default());
        let poisoned = std::panic::catch_unwind(|| {
            let _guard = lock.write().unwrap();
            panic!("poison the settings lock");
        });
        assert!(poisoned.is_err());
        assert!(lock.is_poisoned());
        let updated = TenantDestroySettings::default();
        let error = write_destroy_settings(&lock, &updated).unwrap_err();
        assert_eq!(error.code, 500);
    }
}
