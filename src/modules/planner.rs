// Resource planner -- capacity planning across servers, clusters and
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
//     GET    /pools/{id}/forecast    capacity forecast for a horizon
//                                  (7/30/60/90/180 days; default 30)
//     GET    /pools/{id}/overcommit  overcommit report for one pool
//   Plans:
//     GET    /plans                  list allocation plans (paginated)
//     POST   /plans                  create an allocation plan (draft)
//     GET    /plans/{id}             one plan
//     PUT    /plans/{id}             update a draft plan
//     DELETE /plans/{id}             delete a draft or expired plan
//     POST   /plans/{id}/approve     draft -> approved (overcommit warned, not blocked)
//     POST   /plans/{id}/activate    approved -> active (overcommit warned, not blocked)
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
// Authorisation: every route requires the `X-User-Id` and `X-Role`
// headers and a capability checked through the `role.check` hook
// (same pattern as the recruitment module; fails closed when no
// handler answers). Reads require `planner.view`; all mutations —
// creating, updating or deleting pools and plans, recording usage,
// and the approve / activate / expire transitions — require
// `planner.manage`.
//
// Overcommit policy: approve and activate only warn on overcommit.
// The warning is attached to the plan view and the
// `planner.overcommit` event is emitted, but the transition is never
// refused for overcommit alone — a pool may legitimately run
// overcommitted while scaling out, and the numbers are reported
// instead of blocking the workflow.
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

pub mod ai_capabilities;
pub mod constants;
mod helpers;
pub mod types;
pub mod state;
mod handlers_pools;
mod forecast;
mod handlers_plans;
mod events;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::{PLANNER_POOL_CREATED, PLANNER_PLAN_APPROVED, PLANNER_PLAN_ACTIVATED, PLANNER_PLAN_EXPIRED, PLANNER_OVERCOMMIT, PLANNER_FORECAST_GENERATED};
pub use self::types::{Resources, CapacityPool, UsageSample, AllocationPlan};
pub use self::state::{PlannerState};
pub use self::module::{PlannerModule, register};
