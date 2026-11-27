//! Planner - In-memory state and its constructors.

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
use super::{constants::*, helpers::*, types::*, handlers_pools::*, forecast::*, handlers_plans::*, events::*, module::*};

// State.

pub struct PlannerState {
    pub(crate) pools: DashMap<String, CapacityPool>,
    pub(crate) plans: DashMap<String, AllocationPlan>,
    pub(crate) samples: DashMap<String, UsageSample>,
}

impl PlannerState {
    pub(crate) fn new() -> Self {
        Self {
            pools: DashMap::new(),
            plans: DashMap::new(),
            samples: DashMap::new(),
        }
    }

    pub(crate) fn pool(&self, id: &str) -> UnifiedResult<CapacityPool> {
        self.pools
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Capacity pool \"{id}\" not found")))
    }

    pub(crate) fn plan(&self, id: &str) -> UnifiedResult<AllocationPlan> {
        self.plans
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Allocation plan \"{id}\" not found")))
    }

    /// Resources currently held on a pool by approved/active plans that
    /// are still inside their time window.
    pub(crate) fn allocated(&self, pool_id: &str, at: DateTime<Utc>) -> Resources {
        let mut total = Resources::default();
        for entry in self.plans.iter() {
            let plan = entry.value();
            if plan.pool_id == pool_id && plan.holds_capacity(at) {
                total = total.add(&plan.reserved);
            }
        }
        total
    }

    pub(crate) fn live_plan_count(&self, pool_id: &str, at: DateTime<Utc>) -> u64 {
        self.plans
            .iter()
            .filter(|entry| {
                let plan = entry.value();
                plan.pool_id == pool_id && plan.holds_capacity(at)
            })
            .count() as u64
    }

    pub(crate) fn pool_view(&self, pool: &CapacityPool) -> PoolView {
        let allocated = self.allocated(&pool.id, now());
        PoolView {
            headroom: pool.capacity.subtract(&allocated),
            overcommitted: allocated.overcommitted_against(&pool.capacity),
            allocated,
            pool: pool.clone(),
        }
    }

    pub(crate) fn overcommit_detail(&self, pool: &CapacityPool) -> Option<OvercommitDetail> {
        let allocated = self.allocated(&pool.id, now());
        if !allocated.overcommitted_against(&pool.capacity) {
            return None;
        }
        let mut resources: Vec<&'static str> = Vec::new();
        if allocated.cpu_cores > pool.capacity.cpu_cores {
            resources.push("cpu_cores");
        }
        if allocated.ram_gb > pool.capacity.ram_gb {
            resources.push("ram_gb");
        }
        if allocated.disk_gb > pool.capacity.disk_gb {
            resources.push("disk_gb");
        }
        if allocated.bandwidth_tb > pool.capacity.bandwidth_tb {
            resources.push("bandwidth_tb");
        }
        Some(OvercommitDetail {
            pool_id: pool.id.clone(),
            pool_name: pool.name.clone(),
            capacity: pool.capacity,
            allocated,
            excess: allocated.excess_over(&pool.capacity),
            overcommitted_resources: resources,
        })
    }

    pub(crate) fn plan_view(&self, plan: &AllocationPlan) -> UnifiedResult<PlanView> {
        let pool = self.pool(&plan.pool_id)?;
        Ok(PlanView {
            past_end: plan.is_past_end(now()),
            overcommit_warning: self.overcommit_detail(&pool),
            plan: plan.clone(),
        })
    }

    pub(crate) fn samples_for_pool(&self, pool_id: &str) -> Vec<UsageSample> {
        let mut samples: Vec<UsageSample> = self
            .samples
            .iter()
            .filter(|entry| entry.value().pool_id == pool_id)
            .map(|entry| entry.value().clone())
            .collect();
        samples.sort_by(|a, b| a.recorded_at.cmp(&b.recorded_at));
        samples
    }
}
