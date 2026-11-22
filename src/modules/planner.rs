// Resource planner — capacity planning across servers, clusters and
// providers.
//
// Routes under `/api/v1/planner`:
//
//   Pools:
//     GET    /pools                  list capacity pools (paginated)
//     POST   /pools                  create a capacity pool
//     GET    /pools/{id}             one pool, with live allocation totals
//     PUT    /pools/{id}             update a capacity pool
//     DELETE /pools/{id}             delete a pool (refused while live plans use it)
//     GET    /pools/{id}/utilisation  recorded usage samples (paginated)
//     POST   /pools/{id}/usage       record one usage sample
//     GET    /pools/{id}/forecast    capacity forecast for a horizon (30/90 days)
//     GET    /pools/{id}/overcommit  overcommit report for one pool
//   Plans:
//     GET    /plans                  list allocation plans (paginated)
//     POST   /plans                  create an allocation plan (draft)
//     GET    /plans/{id}             one plan
//     PUT    /plans/{id}             update a draft plan
//     DELETE /plans/{id}             delete a draft or expired plan
//     POST   /plans/{id}/approve     draft -> approved (overcommit checked)
//     POST   /plans/{id}/activate    approved -> active (overcommit checked)
//     POST   /plans/{id}/expire      approved/active/draft -> expired
//   Reports:
//     GET    /overcommit             overcommit report for every pool
//
// Events emitted on the global hook bus:
//
//   planner.pool.created      a pool was created
//   planner.plan.approved     a plan moved draft -> approved
//   planner.plan.activated    a plan moved approved -> active
//   planner.plan.expired      a plan was expired (explicit endpoint only)
//   planner.overcommit        planned allocations exceed pool capacity
//   planner.forecast.generated  a forecast was generated
//
// Events consumed (listeners registered in `register_hooks`):
//
//   provider.sync.completed   updates the matching pools' last-sync
//                             information (last sync time and server
//                             count). The provider sync payload carries
//                             no per-server usage numbers, so it cannot
//                             feed utilisation samples; usage is recorded
//                             through POST /pools/{id}/usage instead, or
//                             by a future inventory usage feed.
//
// Forecasts are projections, not guarantees: a least-squares linear
// trend fitted to the recorded usage samples, projected forward over
// the requested horizon. The response always carries the sample count,
// the fit quality, and a written disclaimer, so a projection is never
// mistaken for a commitment.
//
// Storage is in memory (DashMap), the same pattern as the store and
// scheduler modules. Durable persistence is a follow-up for the
// integrator: the migration sketch is in the coordinator report.

use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Event names.
// ---------------------------------------------------------------------------

pub const PLANNER_POOL_CREATED: &str = "planner.pool.created";
pub const PLANNER_PLAN_APPROVED: &str = "planner.plan.approved";
pub const PLANNER_PLAN_ACTIVATED: &str = "planner.plan.activated";
pub const PLANNER_PLAN_EXPIRED: &str = "planner.plan.expired";
pub const PLANNER_OVERCOMMIT: &str = "planner.overcommit";
pub const PLANNER_FORECAST_GENERATED: &str = "planner.forecast.generated";

/// Provider sync event consumed to refresh pool sync information.
const PROVIDER_SYNC_COMPLETED: &str = "provider.sync.completed";

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>, debug: bool) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn require_name(name: &str) -> UnifiedResult<String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(UnifiedError::bad_request("name must not be empty"));
    }
    if name.chars().count() > 200 {
        return Err(UnifiedError::bad_request("name must be at most 200 characters"));
    }
    Ok(name)
}

fn require_non_negative(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number >= 0"
        )));
    }
    Ok(value)
}

fn parse_datetime(value: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| {
            UnifiedError::bad_request(format!("{field} must be an RFC 3339 date-time"))
        })
}

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

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
    fn validate(&self) -> UnifiedResult<()> {
        require_non_negative(self.cpu_cores, "cpu_cores")?;
        require_non_negative(self.ram_gb, "ram_gb")?;
        require_non_negative(self.disk_gb, "disk_gb")?;
        require_non_negative(self.bandwidth_tb, "bandwidth_tb")?;
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.cpu_cores <= 0.0 && self.ram_gb <= 0.0 && self.disk_gb <= 0.0 && self.bandwidth_tb <= 0.0
    }

    fn add(&self, other: &Resources) -> Resources {
        Resources {
            cpu_cores: self.cpu_cores + other.cpu_cores,
            ram_gb: self.ram_gb + other.ram_gb,
            disk_gb: self.disk_gb + other.disk_gb,
            bandwidth_tb: self.bandwidth_tb + other.bandwidth_tb,
        }
    }

    fn subtract(&self, other: &Resources) -> Resources {
        Resources {
            cpu_cores: self.cpu_cores - other.cpu_cores,
            ram_gb: self.ram_gb - other.ram_gb,
            disk_gb: self.disk_gb - other.disk_gb,
            bandwidth_tb: self.bandwidth_tb - other.bandwidth_tb,
        }
    }

    /// Per-resource excess over `capacity` (zero where within capacity).
    fn excess_over(&self, capacity: &Resources) -> Resources {
        Resources {
            cpu_cores: (self.cpu_cores - capacity.cpu_cores).max(0.0),
            ram_gb: (self.ram_gb - capacity.ram_gb).max(0.0),
            disk_gb: (self.disk_gb - capacity.disk_gb).max(0.0),
            bandwidth_tb: (self.bandwidth_tb - capacity.bandwidth_tb).max(0.0),
        }
    }

    fn overcommitted_against(&self, capacity: &Resources) -> bool {
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
    /// anyone calls the explicit expire endpoint.
    fn is_past_end(&self, at: DateTime<Utc>) -> bool {
        self.ends_at < at
    }

    /// True when the plan currently holds capacity: approved or active
    /// and inside its time window.
    fn holds_capacity(&self, at: DateTime<Utc>) -> bool {
        matches!(self.status.as_str(), "approved" | "active") && !self.is_past_end(at)
    }
}

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

/// Pool view with live allocation and headroom numbers.
#[derive(Debug, Clone, Serialize)]
struct PoolView {
    #[serde(flatten)]
    pool: CapacityPool,
    allocated: Resources,
    headroom: Resources,
    overcommitted: bool,
}

#[derive(Debug, Clone, Serialize)]
struct PlanView {
    #[serde(flatten)]
    plan: AllocationPlan,
    past_end: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    overcommit_warning: Option<OvercommitDetail>,
}

/// The numbers behind an overcommit, shared by the report endpoint and
/// the warning attached to plan views.
#[derive(Debug, Clone, Serialize)]
struct OvercommitDetail {
    pool_id: String,
    pool_name: String,
    capacity: Resources,
    allocated: Resources,
    excess: Resources,
    overcommitted_resources: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
struct OvercommitReportEntry {
    pool_id: String,
    pool_name: String,
    capacity: Resources,
    allocated: Resources,
    headroom: Resources,
    excess: Resources,
    overcommitted: bool,
    overcommitted_resources: Vec<&'static str>,
    live_plans: u64,
}

/// One resource inside a forecast.
#[derive(Debug, Clone, Serialize)]
struct ResourceProjection {
    resource: &'static str,
    current: f64,
    projected: f64,
    capacity: f64,
    projected_utilisation_pct: Option<f64>,
    exhaustion_date: Option<String>,
    confidence: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct ForecastView {
    pool_id: String,
    pool_name: String,
    horizon_days: u32,
    generated_at: DateTime<Utc>,
    samples_used: usize,
    first_sample_at: Option<DateTime<Utc>>,
    last_sample_at: Option<DateTime<Utc>>,
    projections: Vec<ResourceProjection>,
    confidence: &'static str,
    disclaimer: String,
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreatePoolRequest {
    name: String,
    #[serde(default)]
    provider_connection_id: Option<String>,
    #[serde(default)]
    cluster_id: Option<String>,
    capacity: Resources,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdatePoolRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    provider_connection_id: Option<Option<String>>,
    #[serde(default)]
    cluster_id: Option<Option<String>>,
    #[serde(default)]
    capacity: Option<Resources>,
    #[serde(default)]
    notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
struct RecordUsageRequest {
    used: Resources,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    recorded_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreatePlanRequest {
    pool_id: String,
    name: String,
    #[serde(default)]
    company_id: Option<String>,
    #[serde(default)]
    project_name: Option<String>,
    reserved: Resources,
    starts_at: String,
    ends_at: String,
}

#[derive(Debug, Deserialize)]
struct UpdatePlanRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    company_id: Option<Option<String>>,
    #[serde(default)]
    project_name: Option<Option<String>>,
    #[serde(default)]
    reserved: Option<Resources>,
    #[serde(default)]
    starts_at: Option<String>,
    #[serde(default)]
    ends_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ForecastQuery {
    #[serde(default)]
    horizon_days: Option<u32>,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct PlannerState {
    pools: DashMap<String, CapacityPool>,
    plans: DashMap<String, AllocationPlan>,
    samples: DashMap<String, UsageSample>,
}

impl PlannerState {
    fn new() -> Self {
        Self {
            pools: DashMap::new(),
            plans: DashMap::new(),
            samples: DashMap::new(),
        }
    }

    fn pool(&self, id: &str) -> UnifiedResult<CapacityPool> {
        self.pools
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Capacity pool \"{id}\" not found")))
    }

    fn plan(&self, id: &str) -> UnifiedResult<AllocationPlan> {
        self.plans
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Allocation plan \"{id}\" not found")))
    }

    /// Resources currently held on a pool by approved/active plans that
    /// are still inside their time window.
    fn allocated(&self, pool_id: &str, at: DateTime<Utc>) -> Resources {
        let mut total = Resources::default();
        for entry in self.plans.iter() {
            let plan = entry.value();
            if plan.pool_id == pool_id && plan.holds_capacity(at) {
                total = total.add(&plan.reserved);
            }
        }
        total
    }

    fn live_plan_count(&self, pool_id: &str, at: DateTime<Utc>) -> u64 {
        self.plans
            .iter()
            .filter(|entry| {
                let plan = entry.value();
                plan.pool_id == pool_id && plan.holds_capacity(at)
            })
            .count() as u64
    }

    fn pool_view(&self, pool: &CapacityPool) -> PoolView {
        let allocated = self.allocated(&pool.id, now());
        PoolView {
            headroom: pool.capacity.subtract(&allocated),
            overcommitted: allocated.overcommitted_against(&pool.capacity),
            allocated,
            pool: pool.clone(),
        }
    }

    fn overcommit_detail(&self, pool: &CapacityPool) -> Option<OvercommitDetail> {
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

    fn plan_view(&self, plan: &AllocationPlan) -> UnifiedResult<PlanView> {
        let pool = self.pool(&plan.pool_id)?;
        Ok(PlanView {
            past_end: plan.is_past_end(now()),
            overcommit_warning: self.overcommit_detail(&pool),
            plan: plan.clone(),
        })
    }

    fn samples_for_pool(&self, pool_id: &str) -> Vec<UsageSample> {
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

// ---------------------------------------------------------------------------
// Pool handlers.
// ---------------------------------------------------------------------------

async fn list_pools(
    state: web::Data<Arc<PlannerState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<PoolView> = state
        .pools
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|pool| {
            matches_search(&format!("{} {}", pool.name, pool.notes.clone().unwrap_or_default()), &params)
                && params
                    .filters
                    .get("provider_connection_id")
                    .is_none_or(|value| pool.provider_connection_id.as_deref() == Some(value.as_str()))
                && params
                    .filters
                    .get("cluster_id")
                    .is_none_or(|value| pool.cluster_id.as_deref() == Some(value.as_str()))
                && params.filters.get("overcommitted").is_none_or(|flag| {
                    let want = flag == "true" || flag == "1";
                    let allocated = state.allocated(&pool.id, now());
                    allocated.overcommitted_against(&pool.capacity) == want
                })
        })
        .map(|pool| state.pool_view(&pool))
        .collect();
    items.sort_by(|a, b| a.pool.created_at.cmp(&b.pool.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_pool(
    ctx: web::Data<Arc<crate::core::context::AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    body: web::Json<CreatePoolRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PoolView> = (|| {
        let input = body.into_inner();
        let name = require_name(&input.name)?;
        input.capacity.validate()?;
        if input.capacity.is_empty() {
            return Err(UnifiedError::bad_request(
                "capacity must reserve at least one resource above zero",
            ));
        }
        let timestamp = now();
        let pool = CapacityPool {
            id: new_id(),
            name,
            provider_connection_id: input
                .provider_connection_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            cluster_id: input
                .cluster_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            capacity: input.capacity,
            notes: input.notes.map(|value| value.trim().to_string()).filter(|value| !value.is_empty()),
            last_sync_at: None,
            last_server_count: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.pools.insert(pool.id.clone(), pool.clone());
        ctx.hooks.emit(
            PLANNER_POOL_CREATED,
            serde_json::to_value(&pool).unwrap_or(Value::Null),
        );
        Ok(state.pool_view(&pool))
    })();
    respond_created(result, debug)
}

async fn get_pool(
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PoolView> = (|| {
        let pool = state.pool(path.as_str())?;
        Ok(state.pool_view(&pool))
    })();
    json::respond(result, false)
}

async fn update_pool(
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
    body: web::Json<UpdatePoolRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PoolView> = (|| {
        let input = body.into_inner();
        let mut pool = state.pool(path.as_str())?;
        if let Some(name) = input.name {
            pool.name = require_name(&name)?;
        }
        if let Some(provider_connection_id) = input.provider_connection_id {
            pool.provider_connection_id = provider_connection_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(cluster_id) = input.cluster_id {
            pool.cluster_id = cluster_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(capacity) = input.capacity {
            capacity.validate()?;
            if capacity.is_empty() {
                return Err(UnifiedError::bad_request(
                    "capacity must reserve at least one resource above zero",
                ));
            }
            pool.capacity = capacity;
        }
        if let Some(notes) = input.notes {
            pool.notes = notes.map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
        }
        pool.updated_at = now();
        state.pools.insert(pool.id.clone(), pool.clone());
        Ok(state.pool_view(&pool))
    })();
    json::respond(result, false)
}

async fn delete_pool(
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let pool = state.pool(path.as_str())?;
        let live = state.live_plan_count(&pool.id, now());
        if live > 0 {
            return Err(UnifiedError::conflict(format!(
                "Capacity pool \"{}\" still has {live} live allocation plan(s); expire or delete them first",
                pool.id
            ))
            .with_metadata("live_plans", Value::from(live)));
        }
        state.pools.remove(&pool.id);
        state
            .samples
            .retain(|_, sample| sample.pool_id != pool.id);
        Ok(json!({ "deleted": true, "id": pool.id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Utilisation handlers.
// ---------------------------------------------------------------------------

async fn list_utilisation(
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<UsageSample>> = (|| {
        let pool = state.pool(path.as_str())?;
        let mut items = state.samples_for_pool(&pool.id);
        items.retain(|sample| {
            params
                .filters
                .get("source")
                .is_none_or(|source| sample.source == *source)
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn record_usage(
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
    body: web::Json<RecordUsageRequest>,
) -> HttpResponse {
    let result: UnifiedResult<UsageSample> = (|| {
        let pool = state.pool(path.as_str())?;
        let input = body.into_inner();
        input.used.validate()?;
        let source = input
            .source
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "api".to_string());
        if !matches!(source.as_str(), "api" | "provider-sync" | "service-monitor") {
            return Err(UnifiedError::bad_request(
                "source must be one of: api, provider-sync, service-monitor",
            ));
        }
        let recorded_at = match input.recorded_at {
            Some(value) => parse_datetime(&value, "recorded_at")?,
            None => now(),
        };
        if recorded_at > now() {
            return Err(UnifiedError::bad_request("recorded_at must not be in the future"));
        }
        let sample = UsageSample {
            id: new_id(),
            pool_id: pool.id,
            used: input.used,
            source,
            recorded_at,
            created_at: now(),
        };
        state.samples.insert(sample.id.clone(), sample.clone());
        Ok(sample)
    })();
    respond_created(result, false)
}

// ---------------------------------------------------------------------------
// Forecast.
// ---------------------------------------------------------------------------

/// Least-squares linear trend over (days, value) points. Returns
/// (slope per day, intercept, R-squared), or `None` when fewer than two
/// points were given or all x values are identical.
fn linear_trend(points: &[(f64, f64)]) -> Option<(f64, f64, f64)> {
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f64;
    let mean_x = points.iter().map(|point| point.0).sum::<f64>() / n;
    let mean_y = points.iter().map(|point| point.1).sum::<f64>() / n;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    let mut ss_tot = 0.0;
    for (x, y) in points {
        sxx += (x - mean_x) * (x - mean_x);
        sxy += (x - mean_x) * (y - mean_y);
        ss_tot += (y - mean_y) * (y - mean_y);
    }
    if sxx <= f64::EPSILON {
        return None;
    }
    let slope = sxy / sxx;
    let intercept = mean_y - slope * mean_x;
    let mut ss_res = 0.0;
    for (x, y) in points {
        let predicted = intercept + slope * x;
        ss_res += (y - predicted) * (y - predicted);
    }
    let r_squared = if ss_tot <= f64::EPSILON {
        1.0
    } else {
        (1.0 - ss_res / ss_tot).clamp(0.0, 1.0)
    };
    Some((slope, intercept, r_squared))
}

fn confidence_for(samples: usize, r_squared: f64) -> &'static str {
    if samples >= 14 && r_squared >= 0.7 {
        "high"
    } else if samples >= 7 && r_squared >= 0.4 {
        "medium"
    } else {
        "low"
    }
}

fn project_resource(
    resource: &'static str,
    samples: &[UsageSample],
    extract: fn(&Resources) -> f64,
    capacity: f64,
    horizon_days: f64,
    last_at: DateTime<Utc>,
) -> ResourceProjection {
    let first_at = samples.first().map(|sample| sample.recorded_at);
    let points: Vec<(f64, f64)> = samples
        .iter()
        .map(|sample| {
            let days = sample
                .recorded_at
                .signed_duration_since(first_at.unwrap_or(sample.recorded_at))
                .num_seconds() as f64
                / 86_400.0;
            (days, extract(&sample.used))
        })
        .collect();
    let current = samples.last().map(|sample| extract(&sample.used)).unwrap_or(0.0);
    let last_days = points.last().map(|point| point.0).unwrap_or(0.0);
    let (projected, confidence, exhaustion_date) = match linear_trend(&points) {
        Some((slope, intercept, r_squared)) => {
            let projected = (intercept + slope * (last_days + horizon_days)).max(0.0);
            let exhaustion = if slope > 1e-9 && current < capacity {
                let days_to_exhaust = (capacity - current) / slope;
                if days_to_exhaust >= 0.0 && days_to_exhaust <= 3650.0 {
                    let date = last_at + chrono::Duration::seconds((days_to_exhaust * 86_400.0) as i64);
                    Some(date.to_rfc3339())
                } else {
                    None
                }
            } else {
                None
            };
            (
                projected,
                confidence_for(samples.len(), r_squared),
                exhaustion,
            )
        }
        None => (current, "low", None),
    };
    let utilisation = if capacity > 0.0 {
        Some((projected / capacity * 100.0 * 100.0).round() / 100.0)
    } else {
        None
    };
    ResourceProjection {
        resource,
        current: (current * 100.0).round() / 100.0,
        projected: (projected * 100.0).round() / 100.0,
        capacity,
        projected_utilisation_pct: utilisation,
        exhaustion_date,
        confidence,
    }
}

async fn get_forecast(
    ctx: web::Data<Arc<crate::core::context::AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
    query: web::Query<ForecastQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ForecastView> = (|| {
        let pool = state.pool(path.as_str())?;
        let horizon_days = query.into_inner().horizon_days.unwrap_or(30);
        if !matches!(horizon_days, 7 | 30 | 60 | 90 | 180) {
            return Err(UnifiedError::bad_request(
                "horizon_days must be one of: 7, 30, 60, 90, 180",
            ));
        }
        let samples = state.samples_for_pool(&pool.id);
        if samples.len() < 2 {
            return Err(UnifiedError::unprocessable(format!(
                "Forecast needs at least 2 usage samples for pool \"{}\"; {} recorded so far",
                pool.id,
                samples.len()
            ))
            .with_metadata("samples_recorded", Value::from(samples.len())));
        }
        let last_at = samples.last().map(|sample| sample.recorded_at).unwrap_or_else(now);
        let horizon = horizon_days as f64;
        let projections = vec![
            project_resource("cpu_cores", &samples, |used| used.cpu_cores, pool.capacity.cpu_cores, horizon, last_at),
            project_resource("ram_gb", &samples, |used| used.ram_gb, pool.capacity.ram_gb, horizon, last_at),
            project_resource("disk_gb", &samples, |used| used.disk_gb, pool.capacity.disk_gb, horizon, last_at),
            project_resource("bandwidth_tb", &samples, |used| used.bandwidth_tb, pool.capacity.bandwidth_tb, horizon, last_at),
        ];
        let confidence = projections
            .iter()
            .map(|projection| projection.confidence)
            .min_by_key(|confidence| match *confidence {
                "low" => 0,
                "medium" => 1,
                _ => 2,
            })
            .unwrap_or("low");
        let forecast = ForecastView {
            pool_id: pool.id.clone(),
            pool_name: pool.name.clone(),
            horizon_days,
            generated_at: now(),
            samples_used: samples.len(),
            first_sample_at: samples.first().map(|sample| sample.recorded_at),
            last_sample_at: Some(last_at),
            projections,
            confidence,
            disclaimer: format!(
                "Projection only, not a guarantee: a linear trend fitted to {} recorded usage sample(s) and projected {} day(s) forward. Real usage depends on future demand, scaling events and provider changes; re-check after new samples arrive.",
                samples.len(),
                horizon_days
            ),
        };
        ctx.hooks.emit(
            PLANNER_FORECAST_GENERATED,
            serde_json::to_value(&forecast).unwrap_or(Value::Null),
        );
        Ok(forecast)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Overcommit report.
// ---------------------------------------------------------------------------

fn overcommit_entry(state: &PlannerState, pool: &CapacityPool) -> OvercommitReportEntry {
    let allocated = state.allocated(&pool.id, now());
    let excess = allocated.excess_over(&pool.capacity);
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
    OvercommitReportEntry {
        headroom: pool.capacity.subtract(&allocated),
        overcommitted: allocated.overcommitted_against(&pool.capacity),
        live_plans: state.live_plan_count(&pool.id, now()),
        pool_id: pool.id.clone(),
        pool_name: pool.name.clone(),
        capacity: pool.capacity,
        allocated,
        excess,
        overcommitted_resources: resources,
    }
}

async fn get_pool_overcommit(
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<OvercommitReportEntry> = (|| {
        let pool = state.pool(path.as_str())?;
        Ok(overcommit_entry(&state, &pool))
    })();
    json::respond(result, false)
}

async fn list_overcommit(
    state: web::Data<Arc<PlannerState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<OvercommitReportEntry> = state
        .pools
        .iter()
        .map(|entry| overcommit_entry(&state, entry.value()))
        .filter(|entry| {
            matches_search(&entry.pool_name, &params)
                && params.filters.get("overcommitted").is_none_or(|flag| {
                    entry.overcommitted == (flag == "true" || flag == "1")
                })
        })
        .collect();
    items.sort_by(|a, b| a.pool_name.cmp(&b.pool_name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Plan handlers.
// ---------------------------------------------------------------------------

/// After a plan changes state, check the pool for overcommit and emit
/// `planner.overcommit` with the numbers when allocations exceed
/// capacity. Returns the detail when overcommitted, so the caller can
/// attach it to the response as a warning.
fn check_overcommit(
    ctx: &crate::core::context::AppContext,
    state: &PlannerState,
    pool: &CapacityPool,
) -> Option<OvercommitDetail> {
    let detail = state.overcommit_detail(pool)?;
    ctx.hooks.emit(
        PLANNER_OVERCOMMIT,
        serde_json::to_value(&detail).unwrap_or(Value::Null),
    );
    Some(detail)
}

async fn list_plans(
    state: web::Data<Arc<PlannerState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<PlanView>> = (|| {
        let mut items: Vec<PlanView> = Vec::new();
        for entry in state.plans.iter() {
            let plan = entry.value().clone();
            if !matches_search(
                &format!(
                    "{} {} {}",
                    plan.name,
                    plan.project_name.clone().unwrap_or_default(),
                    plan.company_id.clone().unwrap_or_default()
                ),
                &params,
            ) {
                continue;
            }
            if let Some(status) = params.filters.get("status") {
                if plan.status != *status {
                    continue;
                }
            }
            if let Some(pool_id) = params.filters.get("pool_id") {
                if plan.pool_id != *pool_id {
                    continue;
                }
            }
            if let Some(company_id) = params.filters.get("company_id") {
                if plan.company_id.as_deref() != Some(company_id.as_str()) {
                    continue;
                }
            }
            items.push(state.plan_view(&plan)?);
        }
        items.sort_by(|a, b| a.plan.created_at.cmp(&b.plan.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn create_plan(
    state: web::Data<Arc<PlannerState>>,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let input = body.into_inner();
        let pool = state.pool(&input.pool_id)?;
        let name = require_name(&input.name)?;
        input.reserved.validate()?;
        if input.reserved.is_empty() {
            return Err(UnifiedError::bad_request(
                "reserved must request at least one resource above zero",
            ));
        }
        let starts_at = parse_datetime(&input.starts_at, "starts_at")?;
        let ends_at = parse_datetime(&input.ends_at, "ends_at")?;
        if ends_at <= starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        let timestamp = now();
        let plan = AllocationPlan {
            id: new_id(),
            pool_id: pool.id,
            name,
            company_id: input
                .company_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            project_name: input
                .project_name
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            reserved: input.reserved,
            starts_at,
            ends_at,
            status: "draft".to_string(),
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.plans.insert(plan.id.clone(), plan.clone());
        state.plan_view(&plan)
    })();
    respond_created(result, false)
}

async fn get_plan(
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let plan = state.plan(path.as_str())?;
        state.plan_view(&plan)
    })();
    json::respond(result, false)
}

async fn update_plan(
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let input = body.into_inner();
        let mut plan = state.plan(path.as_str())?;
        if plan.status != "draft" {
            return Err(UnifiedError::conflict(format!(
                "Only a draft plan can be updated; plan \"{}\" is {}",
                plan.id, plan.status
            )));
        }
        if let Some(name) = input.name {
            plan.name = require_name(&name)?;
        }
        if let Some(company_id) = input.company_id {
            plan.company_id = company_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(project_name) = input.project_name {
            plan.project_name = project_name
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(reserved) = input.reserved {
            reserved.validate()?;
            if reserved.is_empty() {
                return Err(UnifiedError::bad_request(
                    "reserved must request at least one resource above zero",
                ));
            }
            plan.reserved = reserved;
        }
        if let Some(starts_at) = input.starts_at {
            plan.starts_at = parse_datetime(&starts_at, "starts_at")?;
        }
        if let Some(ends_at) = input.ends_at {
            plan.ends_at = parse_datetime(&ends_at, "ends_at")?;
        }
        if plan.ends_at <= plan.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        plan.updated_at = now();
        state.plans.insert(plan.id.clone(), plan.clone());
        state.plan_view(&plan)
    })();
    json::respond(result, false)
}

async fn delete_plan(
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let plan = state.plan(path.as_str())?;
        if !matches!(plan.status.as_str(), "draft" | "expired") {
            return Err(UnifiedError::conflict(format!(
                "Only a draft or expired plan can be deleted; plan \"{}\" is {}",
                plan.id, plan.status
            )));
        }
        state.plans.remove(&plan.id);
        Ok(json!({ "deleted": true, "id": plan.id }))
    })();
    json::respond(result, false)
}

async fn approve_plan(
    ctx: web::Data<Arc<crate::core::context::AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let mut plan = state.plan(path.as_str())?;
        if plan.status != "draft" {
            return Err(UnifiedError::conflict(format!(
                "Only a draft plan can be approved; plan \"{}\" is {}",
                plan.id, plan.status
            )));
        }
        if plan.is_past_end(now()) {
            return Err(UnifiedError::conflict(format!(
                "Plan \"{}\" already ended on {}; it cannot be approved",
                plan.id,
                plan.ends_at.to_rfc3339()
            )));
        }
        plan.status = "approved".to_string();
        plan.updated_at = now();
        state.plans.insert(plan.id.clone(), plan.clone());
        let pool = state.pool(&plan.pool_id)?;
        let warning = check_overcommit(&ctx, &state, &pool);
        ctx.hooks.emit(
            PLANNER_PLAN_APPROVED,
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        let mut view = state.plan_view(&plan)?;
        view.overcommit_warning = warning;
        Ok(view)
    })();
    json::respond(result, false)
}

async fn activate_plan(
    ctx: web::Data<Arc<crate::core::context::AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let mut plan = state.plan(path.as_str())?;
        if plan.status != "approved" {
            return Err(UnifiedError::conflict(format!(
                "Only an approved plan can be activated; plan \"{}\" is {}",
                plan.id, plan.status
            )));
        }
        if plan.is_past_end(now()) {
            return Err(UnifiedError::conflict(format!(
                "Plan \"{}\" already ended on {}; it cannot be activated",
                plan.id,
                plan.ends_at.to_rfc3339()
            )));
        }
        plan.status = "active".to_string();
        plan.updated_at = now();
        state.plans.insert(plan.id.clone(), plan.clone());
        let pool = state.pool(&plan.pool_id)?;
        let warning = check_overcommit(&ctx, &state, &pool);
        ctx.hooks.emit(
            PLANNER_PLAN_ACTIVATED,
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        let mut view = state.plan_view(&plan)?;
        view.overcommit_warning = warning;
        Ok(view)
    })();
    json::respond(result, false)
}

async fn expire_plan(
    ctx: web::Data<Arc<crate::core::context::AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let mut plan = state.plan(path.as_str())?;
        if plan.status == "expired" {
            return Err(UnifiedError::conflict(format!(
                "Plan \"{}\" is already expired",
                plan.id
            )));
        }
        plan.status = "expired".to_string();
        plan.updated_at = now();
        state.plans.insert(plan.id.clone(), plan.clone());
        ctx.hooks.emit(
            PLANNER_PLAN_EXPIRED,
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        state.plan_view(&plan)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Hook listeners.
// ---------------------------------------------------------------------------

/// Provider sync completed: refresh the last-sync information of every
/// pool that draws from this connection. The sync payload carries no
/// per-server usage numbers, so it cannot create utilisation samples;
/// those come from POST /pools/{id}/usage.
fn on_provider_sync_completed(state: Arc<PlannerState>, data: &Value) {
    let Some(connection_id) = data.get("connection_id").and_then(|value| value.as_str()) else {
        return;
    };
    let server_count = data
        .get("server_count")
        .and_then(|value| value.as_u64());
    for mut entry in state.pools.iter_mut() {
        let pool = entry.value_mut();
        if pool.provider_connection_id.as_deref() != Some(connection_id) {
            continue;
        }
        pool.last_sync_at = Some(now());
        pool.last_server_count = server_count;
        pool.updated_at = now();
    }
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct PlannerModule {
    state: Arc<PlannerState>,
}

impl PlannerModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(PlannerState::new()),
        }
    }
}

impl Default for PlannerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PlannerModule {
    fn id(&self) -> &'static str {
        "planner"
    }

    fn name(&self) -> &'static str {
        "Resource Planner"
    }

    fn description(&self) -> &'static str {
        "Capacity planning across servers, clusters and providers: capacity pools, utilisation samples, linear-trend forecasts, allocation plans and overcommit warnings."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            PROVIDER_SYNC_COMPLETED,
            "planner",
            Arc::new(move |ctx: &mut crate::core::hooks::HookContext| {
                on_provider_sync_completed(state.clone(), &ctx.data.clone());
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/planner")
                .route("/overcommit", web::get().to(list_overcommit))
                .route("/pools", web::get().to(list_pools))
                .route("/pools", web::post().to(create_pool))
                .route("/pools/{id}", web::get().to(get_pool))
                .route("/pools/{id}", web::put().to(update_pool))
                .route("/pools/{id}", web::delete().to(delete_pool))
                .route("/pools/{id}/utilisation", web::get().to(list_utilisation))
                .route("/pools/{id}/usage", web::post().to(record_usage))
                .route("/pools/{id}/forecast", web::get().to(get_forecast))
                .route("/pools/{id}/overcommit", web::get().to(get_pool_overcommit))
                .route("/plans", web::get().to(list_plans))
                .route("/plans", web::post().to(create_plan))
                .route("/plans/{id}", web::get().to(get_plan))
                .route("/plans/{id}", web::put().to(update_plan))
                .route("/plans/{id}", web::delete().to(delete_plan))
                .route("/plans/{id}/approve", web::post().to(approve_plan))
                .route("/plans/{id}/activate", web::post().to(activate_plan))
                .route("/plans/{id}/expire", web::post().to(expire_plan)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/planner/pools", owner: "planner", description: "List capacity pools with live allocation totals." },
            RouteInfo { method: "POST", path: "/api/v1/planner/pools", owner: "planner", description: "Create a capacity pool." },
            RouteInfo { method: "GET", path: "/api/v1/planner/pools/{id}", owner: "planner", description: "Get one capacity pool with allocation and headroom." },
            RouteInfo { method: "PUT", path: "/api/v1/planner/pools/{id}", owner: "planner", description: "Update a capacity pool." },
            RouteInfo { method: "DELETE", path: "/api/v1/planner/pools/{id}", owner: "planner", description: "Delete a capacity pool (refused while live plans use it)." },
            RouteInfo { method: "GET", path: "/api/v1/planner/pools/{id}/utilisation", owner: "planner", description: "List recorded utilisation samples for a pool." },
            RouteInfo { method: "POST", path: "/api/v1/planner/pools/{id}/usage", owner: "planner", description: "Record one utilisation sample for a pool." },
            RouteInfo { method: "GET", path: "/api/v1/planner/pools/{id}/forecast", owner: "planner", description: "Linear-trend capacity forecast for a pool (horizon_days 7/30/60/90/180)." },
            RouteInfo { method: "GET", path: "/api/v1/planner/pools/{id}/overcommit", owner: "planner", description: "Overcommit report for one pool." },
            RouteInfo { method: "GET", path: "/api/v1/planner/overcommit", owner: "planner", description: "Overcommit report for every pool." },
            RouteInfo { method: "GET", path: "/api/v1/planner/plans", owner: "planner", description: "List allocation plans." },
            RouteInfo { method: "POST", path: "/api/v1/planner/plans", owner: "planner", description: "Create an allocation plan (draft)." },
            RouteInfo { method: "GET", path: "/api/v1/planner/plans/{id}", owner: "planner", description: "Get one allocation plan." },
            RouteInfo { method: "PUT", path: "/api/v1/planner/plans/{id}", owner: "planner", description: "Update a draft allocation plan." },
            RouteInfo { method: "DELETE", path: "/api/v1/planner/plans/{id}", owner: "planner", description: "Delete a draft or expired allocation plan." },
            RouteInfo { method: "POST", path: "/api/v1/planner/plans/{id}/approve", owner: "planner", description: "Approve a draft plan (overcommit checked)." },
            RouteInfo { method: "POST", path: "/api/v1/planner/plans/{id}/activate", owner: "planner", description: "Activate an approved plan (overcommit checked)." },
            RouteInfo { method: "POST", path: "/api/v1/planner/plans/{id}/expire", owner: "planner", description: "Expire a plan and release its capacity." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PlannerModule::new()));
}
