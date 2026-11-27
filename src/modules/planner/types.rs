//! Planner - Shared types: request and response bodies, views, and state shapes.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{constants::*, helpers::*, state::*, handlers_pools::*, forecast::*, handlers_plans::*, events::*, module::*};

// Types.

/// One resource bundle: CPU cores, RAM, disk and bandwidth. Used for pool
/// capacity, plan reservations, usage samples and overcommit math, so the
/// four resources are always compared in the same units.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Resources {
    #[serde(default)]
    pub cpu_cores: f64,
    #[serde(default)]
    pub ram_gb: f64,
    #[serde(default)]
    pub disk_gb: f64,
    #[serde(default)]
    pub bandwidth_tb: f64,
}

impl Resources {
    pub(crate) fn validate(&self) -> UnifiedResult<()> {
        require_non_negative(self.cpu_cores, "cpu_cores")?;
        require_non_negative(self.ram_gb, "ram_gb")?;
        require_non_negative(self.disk_gb, "disk_gb")?;
        require_non_negative(self.bandwidth_tb, "bandwidth_tb")?;
        Ok(())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.cpu_cores <= 0.0 && self.ram_gb <= 0.0 && self.disk_gb <= 0.0 && self.bandwidth_tb <= 0.0
    }

    pub(crate) fn add(&self, other: &Resources) -> Resources {
        Resources {
            cpu_cores: self.cpu_cores + other.cpu_cores,
            ram_gb: self.ram_gb + other.ram_gb,
            disk_gb: self.disk_gb + other.disk_gb,
            bandwidth_tb: self.bandwidth_tb + other.bandwidth_tb,
        }
    }

    pub(crate) fn subtract(&self, other: &Resources) -> Resources {
        Resources {
            cpu_cores: self.cpu_cores - other.cpu_cores,
            ram_gb: self.ram_gb - other.ram_gb,
            disk_gb: self.disk_gb - other.disk_gb,
            bandwidth_tb: self.bandwidth_tb - other.bandwidth_tb,
        }
    }

    /// Per-resource excess over `capacity` (zero where within capacity).
    pub(crate) fn excess_over(&self, capacity: &Resources) -> Resources {
        Resources {
            cpu_cores: (self.cpu_cores - capacity.cpu_cores).max(0.0),
            ram_gb: (self.ram_gb - capacity.ram_gb).max(0.0),
            disk_gb: (self.disk_gb - capacity.disk_gb).max(0.0),
            bandwidth_tb: (self.bandwidth_tb - capacity.bandwidth_tb).max(0.0),
        }
    }

    pub(crate) fn overcommitted_against(&self, capacity: &Resources) -> bool {
        self.cpu_cores > capacity.cpu_cores
            || self.ram_gb > capacity.ram_gb
            || self.disk_gb > capacity.disk_gb
            || self.bandwidth_tb > capacity.bandwidth_tb
    }
}

/// A capacity pool: the total resources available from one provider
/// connection, one cluster, or a manually defined set of servers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapacityPool {
    pub id: String,
    pub name: String,
    /// Provider connection id (providers module) this pool draws from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_connection_id: Option<String>,
    /// Cluster id (clusters module) this pool draws from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_id: Option<String>,
    pub capacity: Resources,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_server_count: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded utilisation sample for a pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSample {
    pub id: String,
    pub pool_id: String,
    pub used: Resources,
    /// Where the sample came from: `api`, `provider-sync`, or
    /// `service-monitor`.
    pub source: String,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// An allocation plan: a reservation of pool capacity for a project or
/// company over a time window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationPlan {
    pub id: String,
    pub pool_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    pub reserved: Resources,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// `draft`, `approved`, `active`, `expired`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AllocationPlan {
    /// A plan past its end date no longer holds capacity, even before
    /// anyone calls the explicit expire endpoint. The end instant is
    /// exclusive: a plan whose `ends_at` is exactly `at` is already
    /// past its end, so capacity is released at `ends_at`, not after
    /// it - otherwise a plan ending exactly now would still hold
    /// capacity for that instant.
    pub(crate) fn is_past_end(&self, at: DateTime<Utc>) -> bool {
        self.ends_at <= at
    }

    /// True when the plan currently holds capacity: approved or active
    /// and inside its time window.
    pub(crate) fn holds_capacity(&self, at: DateTime<Utc>) -> bool {
        matches!(self.status.as_str(), "approved" | "active") && !self.is_past_end(at)
    }
}

// Views.

/// Pool view with live allocation and headroom numbers.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PoolView {
    #[serde(flatten)]
    pub(crate) pool: CapacityPool,
    pub(crate) allocated: Resources,
    pub(crate) headroom: Resources,
    pub(crate) overcommitted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PlanView {
    #[serde(flatten)]
    pub(crate) plan: AllocationPlan,
    pub(crate) past_end: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) overcommit_warning: Option<OvercommitDetail>,
}

/// The numbers behind an overcommit, shared by the report endpoint and
/// the warning attached to plan views.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct OvercommitDetail {
    pub(crate) pool_id: String,
    pub(crate) pool_name: String,
    pub(crate) capacity: Resources,
    pub(crate) allocated: Resources,
    pub(crate) excess: Resources,
    pub(crate) overcommitted_resources: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OvercommitReportEntry {
    pub(crate) pool_id: String,
    pub(crate) pool_name: String,
    pub(crate) capacity: Resources,
    pub(crate) allocated: Resources,
    pub(crate) headroom: Resources,
    pub(crate) excess: Resources,
    pub(crate) overcommitted: bool,
    pub(crate) overcommitted_resources: Vec<&'static str>,
    pub(crate) live_plans: u64,
}

/// One resource inside a forecast.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResourceProjection {
    pub(crate) resource: &'static str,
    pub(crate) current: f64,
    pub(crate) projected: f64,
    pub(crate) capacity: f64,
    pub(crate) projected_utilisation_pct: Option<f64>,
    pub(crate) exhaustion_date: Option<String>,
    pub(crate) confidence: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ForecastView {
    pub(crate) pool_id: String,
    pub(crate) pool_name: String,
    pub(crate) horizon_days: u32,
    pub(crate) generated_at: DateTime<Utc>,
    pub(crate) samples_used: usize,
    pub(crate) first_sample_at: Option<DateTime<Utc>>,
    pub(crate) last_sample_at: Option<DateTime<Utc>>,
    pub(crate) projections: Vec<ResourceProjection>,
    pub(crate) confidence: &'static str,
    pub(crate) disclaimer: String,
}

// Request types.

#[derive(Debug, Deserialize)]
pub(crate) struct CreatePoolRequest {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) provider_connection_id: Option<String>,
    #[serde(default)]
    pub(crate) cluster_id: Option<String>,
    pub(crate) capacity: Resources,
    #[serde(default)]
    pub(crate) notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdatePoolRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) provider_connection_id: Option<Option<String>>,
    #[serde(default)]
    pub(crate) cluster_id: Option<Option<String>>,
    #[serde(default)]
    pub(crate) capacity: Option<Resources>,
    #[serde(default)]
    pub(crate) notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecordUsageRequest {
    pub(crate) used: Resources,
    #[serde(default)]
    pub(crate) source: Option<String>,
    #[serde(default)]
    pub(crate) recorded_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreatePlanRequest {
    pub(crate) pool_id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) company_id: Option<String>,
    #[serde(default)]
    pub(crate) project_name: Option<String>,
    pub(crate) reserved: Resources,
    pub(crate) starts_at: String,
    pub(crate) ends_at: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdatePlanRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) company_id: Option<Option<String>>,
    #[serde(default)]
    pub(crate) project_name: Option<Option<String>>,
    #[serde(default)]
    pub(crate) reserved: Option<Resources>,
    #[serde(default)]
    pub(crate) starts_at: Option<String>,
    #[serde(default)]
    pub(crate) ends_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ForecastQuery {
    /// Forecast horizon in days. Accepted values: 7, 30, 60, 90,
    /// 180; defaults to 30 when omitted.
    #[serde(default)]
    pub(crate) horizon_days: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resources(cpu: f64, ram: f64, disk: f64, bw: f64) -> Resources {
        Resources {
            cpu_cores: cpu,
            ram_gb: ram,
            disk_gb: disk,
            bandwidth_tb: bw,
        }
    }

    #[test]
    fn resources_math() {
        let a = resources(4.0, 8.0, 100.0, 1.0);
        let b = resources(1.0, 2.0, 25.0, 0.5);
        assert_eq!(a.add(&b), resources(5.0, 10.0, 125.0, 1.5));
        assert_eq!(a.subtract(&b), resources(3.0, 6.0, 75.0, 0.5));
        assert_eq!(
            a.excess_over(&resources(3.0, 10.0, 50.0, 2.0)),
            resources(1.0, 0.0, 50.0, 0.0)
        );
        assert!(a.overcommitted_against(&resources(3.0, 8.0, 100.0, 1.0)));
        assert!(!a.overcommitted_against(&a));
        assert!(!a.overcommitted_against(&resources(5.0, 9.0, 101.0, 2.0)));
        assert!(Resources::default().is_empty());
        assert!(!a.is_empty());
        assert!(a.validate().is_ok());
        assert!(resources(-1.0, 0.0, 0.0, 0.0).validate().is_err());
        assert!(resources(f64::NAN, 0.0, 0.0, 0.0).validate().is_err());
    }

    fn plan(status: &str, ends_at: chrono::DateTime<chrono::Utc>) -> AllocationPlan {
        AllocationPlan {
            id: "plan-1".to_string(),
            pool_id: "pool-1".to_string(),
            name: "Plan".to_string(),
            company_id: None,
            project_name: None,
            reserved: Resources::default(),
            starts_at: chrono::Utc::now() - chrono::Duration::days(1),
            ends_at,
            status: status.to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn allocation_plan_holds_capacity() {
        let at = chrono::Utc::now();
        let future = at + chrono::Duration::days(1);
        let past = at - chrono::Duration::days(1);
        assert!(plan("approved", future).holds_capacity(at));
        assert!(plan("active", future).holds_capacity(at));
        assert!(!plan("draft", future).holds_capacity(at));
        assert!(!plan("expired", future).holds_capacity(at));
        assert!(!plan("approved", past).holds_capacity(at));
        assert!(!plan("active", past).holds_capacity(at));
        assert!(plan("approved", past).is_past_end(at));
        assert!(!plan("approved", future).is_past_end(at));
    }

    /// A plan ending exactly now is past its end (exclusive end
    /// instant) and no longer holds capacity.
    #[test]
    fn allocation_plan_ending_exactly_now_is_past_end() {
        let at = chrono::Utc::now();
        assert!(plan("approved", at).is_past_end(at));
        assert!(!plan("approved", at).holds_capacity(at));
        assert!(!plan("active", at).holds_capacity(at));
        // One instant before the end it still holds capacity.
        let just_before = at - chrono::Duration::seconds(1);
        assert!(!plan("approved", at).is_past_end(just_before));
        assert!(plan("approved", at).holds_capacity(just_before));
    }
}
