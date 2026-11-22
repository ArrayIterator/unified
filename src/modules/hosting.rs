// Hosting module — hosting plans and hosting accounts.
//
// A hosting plan is a sellable resource bundle (disk / CPU / RAM /
// bandwidth, price, billing cycle) optionally linked to a store product
// id, so the plan can be sold through the Store. A hosting account
// binds one plan to one domain and one company, and moves through the
// lifecycle `pending -> active -> suspended -> active` with `cancelled`
// as the terminal state.
//
// Module requirements (see `Module::requires`): the Store (plans are
// sold as store products), Domains (accounts are bound to a registered
// domain), and Network (domain syntax checks and the DNS / TLS probe
// behind domain verification). There is no separate `ssl` module in
// this codebase: TLS/SSL is a core HTTP-layer capability
// (`crate::http::tls`, ACME in `crate::http::server`), surfaced to
// modules through the Domains module — domain `ssl_status` /
// `ssl_expires_at` plus the `ssl.renew` / `ssl.swap` hook events — and
// through the Network module's TLS probe. Requiring `domains` and
// `network` is therefore the closest real declaration of the SSL
// capability, and this module never invents an `ssl` module id.
//
// Interconnection happens through the hook bus only; this module does
// not edit or call into the Store or Domains modules:
//   - It listens to `store.product.published` /
//     `store.product.unpublished` to learn which store product ids
//     exist and are published.
//   - It listens to `store.order.completed` and `order.paid` to record
//     paid orders for a hosting plan as provisioning claims, so an
//     account created with `store_order_id` can be verified against a
//     real paid order.
//   - It listens to `domain.verify` and to completed `ssl.renew` /
//     `ssl.swap` events to observe domain registration and SSL state
//     without a database.
//   - It emits `hosting.plan.created`, `hosting.plan.updated`,
//     `hosting.account.created`, `hosting.account.updated`,
//     `hosting.account.activated`, `hosting.account.suspended`,
//     `hosting.account.cancelled`, and `hosting.requirements.checked`.
//
// Persistence: in-memory stores (DashMap), exactly like the Store
// module. The Diesel tables for this module do not exist yet — see the
// migration + `schema.rs` snippet in the delivery report; once they
// are applied, persistence follows the Domains module pattern (insert
// on create, update on change, load from the pool when one is
// attached). The domain / SSL requirement checks already read the
// `domains` table from the pool when one is attached, because that
// table exists today.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::db::models::Domain;
use crate::modules::company::company_exists;
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// Billing cycle of a hosting plan.
pub const BILLING_CYCLES: [&str; 2] = ["monthly", "yearly"];

#[derive(Debug, Clone, Serialize)]
pub struct HostingPlan {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    /// Disk quota in gigabytes.
    pub disk_gb: i64,
    /// CPU cores allocated to the account.
    pub cpu_cores: f64,
    /// RAM in megabytes.
    pub ram_mb: i64,
    /// Bandwidth quota in gigabytes per billing cycle.
    pub bandwidth_gb: i64,
    pub price: f64,
    pub currency: String,
    pub billing_cycle: String,
    /// Id of the store product that sells this plan, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_product_id: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostingAccount {
    pub id: Uuid,
    pub plan_id: Uuid,
    /// Domain served by this account, lower-cased.
    pub domain: String,
    /// Owning company, when the account belongs to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    /// `pending`, `active`, `suspended`, or `cancelled`.
    pub status: String,
    /// `none`, `pending`, `active`, `expired`, or `renewing`.
    pub ssl_state: String,
    /// Disk used, in megabytes.
    pub disk_used_mb: i64,
    /// Bandwidth used in the current cycle, in megabytes.
    pub bandwidth_used_mb: i64,
    /// Store order that paid for this account, when it was bought
    /// through the Store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_order_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Domain / SSL facts observed from hook events, used by the
/// requirement check when no database pool is attached. The Domains
/// module owns the authoritative registry; these observations are a
/// best-effort cache, and the check reports its source per item.
#[derive(Debug, Clone)]
struct DomainObservation {
    exists: bool,
    ssl_state: String,
    ssl_expires_at: Option<DateTime<Utc>>,
    observed_at: DateTime<Utc>,
}

/// A paid store order for a hosting plan, recorded from
/// `store.order.completed` / `order.paid` hook events.
#[derive(Debug, Clone)]
struct OrderClaim {
    order_id: String,
    plan_id: Uuid,
    user_id: String,
    company_id: Option<String>,
}

pub struct HostingState {
    plans: DashMap<Uuid, HostingPlan>,
    accounts: DashMap<Uuid, HostingAccount>,
    /// Store product id -> whether it is currently published.
    known_products: DashMap<String, bool>,
    /// Domain name -> last observed domain / SSL facts.
    domain_observations: DashMap<String, DomainObservation>,
    /// Store order id -> provisioning claim.
    order_claims: DashMap<String, OrderClaim>,
}

impl HostingState {
    fn new() -> Self {
        Self {
            plans: DashMap::new(),
            accounts: DashMap::new(),
            known_products: DashMap::new(),
            domain_observations: DashMap::new(),
            order_claims: DashMap::new(),
        }
    }
}

impl Default for HostingState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreatePlanRequest {
    name: String,
    #[serde(default)]
    description: Option<String>,
    disk_gb: i64,
    cpu_cores: f64,
    ram_mb: i64,
    bandwidth_gb: i64,
    price: f64,
    #[serde(default)]
    currency: Option<String>,
    billing_cycle: String,
    #[serde(default)]
    store_product_id: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UpdatePlanRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    disk_gb: Option<i64>,
    #[serde(default)]
    cpu_cores: Option<f64>,
    #[serde(default)]
    ram_mb: Option<i64>,
    #[serde(default)]
    bandwidth_gb: Option<i64>,
    #[serde(default)]
    price: Option<f64>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    billing_cycle: Option<String>,
    #[serde(default)]
    store_product_id: Option<Option<String>>,
    #[serde(default)]
    is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateAccountRequest {
    plan_id: Uuid,
    domain: String,
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    store_order_id: Option<String>,
    #[serde(default)]
    disk_used_mb: Option<i64>,
    #[serde(default)]
    bandwidth_used_mb: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UpdateAccountRequest {
    #[serde(default)]
    plan_id: Option<Uuid>,
    #[serde(default)]
    company_id: Option<Option<Uuid>>,
    #[serde(default)]
    disk_used_mb: Option<i64>,
    #[serde(default)]
    bandwidth_used_mb: Option<i64>,
}

// ---------------------------------------------------------------------------
// Requirement-check views.
// ---------------------------------------------------------------------------

/// One checked requirement of a hosting account.
#[derive(Debug, Clone, Serialize)]
pub struct RequirementItem {
    /// `domain_registered`, `ssl_ready`, `store_product_linked`,
    /// `store_order_paid`, or `plan_exists`.
    pub code: String,
    pub label: String,
    /// `"ok"`, `"failed"`, `"unknown"`, or `"skipped"`.
    pub status: String,
    pub detail: String,
    /// Where the answer came from: `"database"`, `"hook_cache"`,
    /// `"local_state"`, or `"unverified"`.
    pub source: String,
}

/// Full requirement report for one hosting account.
#[derive(Debug, Clone, Serialize)]
pub struct AccountRequirementReport {
    pub account_id: Uuid,
    pub domain: String,
    /// `"ok"` when every applicable check passed, `"not_ready"` when
    /// at least one check failed, `"unknown"` when nothing failed but
    /// at least one check could not be verified.
    pub status: String,
    pub can_activate: bool,
    pub checks: Vec<RequirementItem>,
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn db_pool(ctx: &AppContext) -> Option<crate::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn validate_plan_resources(
    disk_gb: i64,
    cpu_cores: f64,
    ram_mb: i64,
    bandwidth_gb: i64,
    price: f64,
) -> UnifiedResult<()> {
    if disk_gb <= 0 {
        return Err(UnifiedError::bad_request("disk_gb must be greater than 0"));
    }
    if cpu_cores <= 0.0 {
        return Err(UnifiedError::bad_request(
            "cpu_cores must be greater than 0",
        ));
    }
    if ram_mb <= 0 {
        return Err(UnifiedError::bad_request("ram_mb must be greater than 0"));
    }
    if bandwidth_gb <= 0 {
        return Err(UnifiedError::bad_request(
            "bandwidth_gb must be greater than 0",
        ));
    }
    if price < 0.0 || !price.is_finite() {
        return Err(UnifiedError::bad_request(
            "price must be a finite number >= 0",
        ));
    }
    Ok(())
}

fn validate_billing_cycle(cycle: &str) -> UnifiedResult<()> {
    if !BILLING_CYCLES.contains(&cycle) {
        return Err(UnifiedError::bad_request(format!(
            "billing_cycle must be one of: {}",
            BILLING_CYCLES.join(", ")
        )));
    }
    Ok(())
}

fn validate_currency(currency: &str) -> UnifiedResult<String> {
    let normalized = currency.trim().to_uppercase();
    if normalized.len() != 3 || !normalized.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code, for example \"USD\"",
        ));
    }
    Ok(normalized)
}

fn validate_usage_against_plan(
    plan: &HostingPlan,
    disk_used_mb: i64,
    bandwidth_used_mb: i64,
) -> UnifiedResult<()> {
    if disk_used_mb < 0 {
        return Err(UnifiedError::bad_request(
            "disk_used_mb must be >= 0",
        ));
    }
    if bandwidth_used_mb < 0 {
        return Err(UnifiedError::bad_request(
            "bandwidth_used_mb must be >= 0",
        ));
    }
    if disk_used_mb > plan.disk_gb * 1024 {
        return Err(UnifiedError::unprocessable(format!(
            "disk_used_mb ({disk_used_mb}) exceeds the plan quota of {} MB",
            plan.disk_gb * 1024
        )));
    }
    if bandwidth_used_mb > plan.bandwidth_gb * 1024 {
        return Err(UnifiedError::unprocessable(format!(
            "bandwidth_used_mb ({bandwidth_used_mb}) exceeds the plan quota of {} MB",
            plan.bandwidth_gb * 1024
        )));
    }
    Ok(())
}

/// Allowed account status transitions. `cancelled` is terminal.
fn allowed_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("pending", "active")
            | ("pending", "cancelled")
            | ("active", "suspended")
            | ("active", "cancelled")
            | ("suspended", "active")
            | ("suspended", "cancelled")
    )
}

/// Authoritative domain row from the database, when a pool is
/// attached. Returns `None` when there is no pool or no such domain.
fn domain_from_db(ctx: &AppContext, name: &str) -> Option<Domain> {
    let pool = db_pool(ctx)?;
    use crate::db::schema::domains::dsl as dom_dsl;
    let mut conn = pool.get().ok()?;
    dom_dsl::domains
        .filter(dom_dsl::name.eq(name))
        .select(Domain::as_select())
        .first(&mut conn)
        .optional()
        .ok()
        .flatten()
}

fn parse_expiry(value: Option<&Value>) -> Option<DateTime<Utc>> {
    value
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<DateTime<Utc>>().ok())
}

/// Requirement check for one account. Each check reports its source,
/// so a caller can tell an authoritative database answer apart from a
/// hook-observed one and from an unverifiable one.
fn check_account_requirements(
    state: &Arc<HostingState>,
    ctx: &AppContext,
    account: &HostingAccount,
) -> AccountRequirementReport {
    let mut checks: Vec<RequirementItem> = Vec::new();

    // Plan must still exist (plan deletion is refused while accounts
    // reference the plan, so this is normally always ok).
    match state.plans.get(&account.plan_id) {
        Some(plan) => {
            checks.push(RequirementItem {
                code: "plan_exists".to_string(),
                label: "Hosting plan exists".to_string(),
                status: "ok".to_string(),
                detail: format!("Plan \"{}\" is registered.", plan.name),
                source: "local_state".to_string(),
            });
            // Store product linked to the plan.
            match &plan.store_product_id {
                None => checks.push(RequirementItem {
                    code: "store_product_linked".to_string(),
                    label: "Store product linked".to_string(),
                    status: "failed".to_string(),
                    detail: "The plan has no linked store product id.".to_string(),
                    source: "local_state".to_string(),
                }),
                Some(product_id) => match state.known_products.get(product_id) {
                    Some(published) => {
                        if *published.value() {
                            checks.push(RequirementItem {
                                code: "store_product_linked".to_string(),
                                label: "Store product linked".to_string(),
                                status: "ok".to_string(),
                                detail: format!(
                                    "Store product \"{product_id}\" is published."
                                ),
                                source: "hook_cache".to_string(),
                            });
                        } else {
                            checks.push(RequirementItem {
                                code: "store_product_linked".to_string(),
                                label: "Store product linked".to_string(),
                                status: "failed".to_string(),
                                detail: format!(
                                    "Store product \"{product_id}\" is unpublished."
                                ),
                                source: "hook_cache".to_string(),
                            });
                        }
                    }
                    None => checks.push(RequirementItem {
                        code: "store_product_linked".to_string(),
                        label: "Store product linked".to_string(),
                        status: "unknown".to_string(),
                        detail: format!(
                            "Store product \"{product_id}\" has not been observed via store.product.published; the product may exist but was published before this process started or while the hook had no listener."
                        ),
                        source: "unverified".to_string(),
                    }),
                },
            }
        }
        None => {
            checks.push(RequirementItem {
                code: "plan_exists".to_string(),
                label: "Hosting plan exists".to_string(),
                status: "failed".to_string(),
                detail: "The plan this account refers to no longer exists.".to_string(),
                source: "local_state".to_string(),
            });
            checks.push(RequirementItem {
                code: "store_product_linked".to_string(),
                label: "Store product linked".to_string(),
                status: "failed".to_string(),
                detail: "Cannot check the store product link without the plan.".to_string(),
                source: "local_state".to_string(),
            });
        }
    }

    // Domain registration + SSL state: database first, hook-observed
    // cache second, unknown otherwise.
    let db_domain = domain_from_db(ctx, &account.domain);
    match &db_domain {
        Some(domain) => {
            if domain.is_active {
                checks.push(RequirementItem {
                    code: "domain_registered".to_string(),
                    label: "Domain registered".to_string(),
                    status: "ok".to_string(),
                    detail: format!("Domain \"{}\" is registered and active.", domain.name),
                    source: "database".to_string(),
                });
            } else {
                checks.push(RequirementItem {
                    code: "domain_registered".to_string(),
                    label: "Domain registered".to_string(),
                    status: "failed".to_string(),
                    detail: format!("Domain \"{}\" is registered but inactive.", domain.name),
                    source: "database".to_string(),
                });
            }
            match domain.ssl_status.as_str() {
                "active" => {
                    let expired = domain
                        .ssl_expires_at
                        .map(|expiry| expiry <= Utc::now())
                        .unwrap_or(false);
                    if expired {
                        checks.push(RequirementItem {
                            code: "ssl_ready".to_string(),
                            label: "SSL ready".to_string(),
                            status: "failed".to_string(),
                            detail: "The domain certificate is expired; renew it first."
                                .to_string(),
                            source: "database".to_string(),
                        });
                    } else {
                        checks.push(RequirementItem {
                            code: "ssl_ready".to_string(),
                            label: "SSL ready".to_string(),
                            status: "ok".to_string(),
                            detail: "The domain has an active certificate.".to_string(),
                            source: "database".to_string(),
                        });
                    }
                }
                other => checks.push(RequirementItem {
                    code: "ssl_ready".to_string(),
                    label: "SSL ready".to_string(),
                    status: "failed".to_string(),
                    detail: format!(
                        "The domain SSL status is \"{other}\"; verify the domain or renew its certificate first."
                    ),
                    source: "database".to_string(),
                }),
            }
        }
        None => match state.domain_observations.get(&account.domain) {
            Some(observation) => {
                checks.push(RequirementItem {
                    code: "domain_registered".to_string(),
                    label: "Domain registered".to_string(),
                    status: "ok".to_string(),
                    detail: format!(
                        "Domain \"{}\" was observed via a domain.verify event.",
                        account.domain
                    ),
                    source: "hook_cache".to_string(),
                });
                let expired = observation
                    .ssl_expires_at
                    .map(|expiry| expiry <= Utc::now())
                    .unwrap_or(false);
                if observation.ssl_state == "active" && !expired {
                    checks.push(RequirementItem {
                        code: "ssl_ready".to_string(),
                        label: "SSL ready".to_string(),
                        status: "ok".to_string(),
                        detail: "The domain has an active certificate (observed via hook event)."
                            .to_string(),
                        source: "hook_cache".to_string(),
                    });
                } else if expired {
                    checks.push(RequirementItem {
                        code: "ssl_ready".to_string(),
                        label: "SSL ready".to_string(),
                        status: "failed".to_string(),
                        detail: "The observed domain certificate is expired; renew it first."
                            .to_string(),
                        source: "hook_cache".to_string(),
                    });
                } else {
                    checks.push(RequirementItem {
                        code: "ssl_ready".to_string(),
                        label: "SSL ready".to_string(),
                        status: "failed".to_string(),
                        detail: format!(
                            "The observed domain SSL state is \"{}\"; verify the domain or renew its certificate first.",
                            observation.ssl_state
                        ),
                        source: "hook_cache".to_string(),
                    });
                }
            }
            None => {
                checks.push(RequirementItem {
                    code: "domain_registered".to_string(),
                    label: "Domain registered".to_string(),
                    status: "unknown".to_string(),
                    detail: "No database pool is attached and no domain.verify event has been observed for this domain, so registration cannot be confirmed. Attach a database or verify the domain first.".to_string(),
                    source: "unverified".to_string(),
                });
                checks.push(RequirementItem {
                    code: "ssl_ready".to_string(),
                    label: "SSL ready".to_string(),
                    status: "unknown".to_string(),
                    detail: "SSL state cannot be confirmed without a database pool or an observed domain.verify / ssl.renew event.".to_string(),
                    source: "unverified".to_string(),
                });
            }
        },
    }

    // Paid store order, when the account was bought through the Store.
    if let Some(order_id) = &account.store_order_id {
        match state.order_claims.get(order_id) {
            Some(claim) => {
                if claim.plan_id == account.plan_id {
                    checks.push(RequirementItem {
                        code: "store_order_paid".to_string(),
                        label: "Store order paid".to_string(),
                        status: "ok".to_string(),
                        detail: format!(
                            "Store order \"{order_id}\" was paid for this plan."
                        ),
                        source: "hook_cache".to_string(),
                    });
                } else {
                    checks.push(RequirementItem {
                        code: "store_order_paid".to_string(),
                        label: "Store order paid".to_string(),
                        status: "failed".to_string(),
                        detail: format!(
                            "Store order \"{order_id}\" was paid for a different hosting plan."
                        ),
                        source: "hook_cache".to_string(),
                    });
                }
            }
            None => checks.push(RequirementItem {
                code: "store_order_paid".to_string(),
                label: "Store order paid".to_string(),
                status: "unknown".to_string(),
                detail: format!(
                    "Store order \"{order_id}\" has not been observed via store.order.completed / order.paid; the order may predate this process or may not be paid yet."
                ),
                source: "unverified".to_string(),
            }),
        }
    }

    let has_failed = checks.iter().any(|check| check.status == "failed");
    let has_unknown = checks.iter().any(|check| check.status == "unknown");
    let status = if has_failed {
        "not_ready".to_string()
    } else if has_unknown {
        "unknown".to_string()
    } else {
        "ok".to_string()
    };
    AccountRequirementReport {
        account_id: account.id,
        domain: account.domain.clone(),
        can_activate: status == "ok",
        status,
        checks,
    }
}

// ---------------------------------------------------------------------------
// Plan handlers.
// ---------------------------------------------------------------------------

async fn list_plans(
    state: web::Data<Arc<HostingState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let mut items: Vec<HostingPlan> = state
        .plans
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|plan| {
            plan.name.to_lowercase().contains(&needle)
                || plan.description.to_lowercase().contains(&needle)
        });
    }
    if let Some(cycle) = params.filters.get("billing_cycle") {
        items.retain(|plan| plan.billing_cycle == *cycle);
    }
    if let Some(active) = params.filters.get("is_active") {
        if let Ok(wanted) = active.parse::<bool>() {
            items.retain(|plan| plan.is_active == wanted);
        }
    }
    if let Some(currency) = params.filters.get("currency") {
        items.retain(|plan| plan.currency.eq_ignore_ascii_case(currency));
    }
    if let Some(product_id) = params.filters.get("store_product_id") {
        items.retain(|plan| plan.store_product_id.as_deref() == Some(product_id.as_str()));
    }
    let sort = params.allowed_sort(&["name", "price", "created_at"], "created_at");
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "price" => items.sort_by(|a, b| {
            a.price
                .partial_cmp(&b.price)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params).to_json(), 200)
}

async fn create_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingPlan> = (|| {
        let name = body.name.trim().to_string();
        if name.is_empty() {
            return Err(UnifiedError::bad_request("name must not be empty"));
        }
        validate_plan_resources(
            body.disk_gb,
            body.cpu_cores,
            body.ram_mb,
            body.bandwidth_gb,
            body.price,
        )?;
        validate_billing_cycle(&body.billing_cycle)?;
        let currency = validate_currency(body.currency.as_deref().unwrap_or("USD"))?;
        if state
            .plans
            .iter()
            .any(|entry| entry.value().name.eq_ignore_ascii_case(&name))
        {
            return Err(UnifiedError::conflict(format!(
                "Hosting plan \"{name}\" already exists"
            )));
        }
        let now = Utc::now();
        let plan = HostingPlan {
            id: Uuid::now_v7(),
            name,
            description: body.description.clone().unwrap_or_default(),
            disk_gb: body.disk_gb,
            cpu_cores: body.cpu_cores,
            ram_mb: body.ram_mb,
            bandwidth_gb: body.bandwidth_gb,
            price: round_money(body.price),
            currency,
            billing_cycle: body.billing_cycle.clone(),
            store_product_id: body
                .store_product_id
                .clone()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            is_active: body.is_active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.plans.insert(plan.id, plan.clone());
        ctx.hooks.emit(
            "hosting.plan.created",
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        Ok(plan)
    })();
    match result {
        Ok(plan) => json::success_response(plan, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingPlan> = (|| {
        state
            .plans
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting plan not found"))
    })();
    json::respond(result, debug)
}

async fn update_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingPlan> = (|| {
        let id = id.into_inner();
        let mut plan = state
            .plans
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting plan not found"))?;
        if let Some(name) = &body.name {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err(UnifiedError::bad_request("name must not be empty"));
            }
            if state
                .plans
                .iter()
                .any(|entry| *entry.key() != id && entry.value().name.eq_ignore_ascii_case(&name))
            {
                return Err(UnifiedError::conflict(format!(
                    "Hosting plan \"{name}\" already exists"
                )));
            }
            plan.name = name;
        }
        if let Some(description) = &body.description {
            plan.description = description.clone();
        }
        let disk_gb = body.disk_gb.unwrap_or(plan.disk_gb);
        let cpu_cores = body.cpu_cores.unwrap_or(plan.cpu_cores);
        let ram_mb = body.ram_mb.unwrap_or(plan.ram_mb);
        let bandwidth_gb = body.bandwidth_gb.unwrap_or(plan.bandwidth_gb);
        let price = body.price.unwrap_or(plan.price);
        validate_plan_resources(disk_gb, cpu_cores, ram_mb, bandwidth_gb, price)?;
        plan.disk_gb = disk_gb;
        plan.cpu_cores = cpu_cores;
        plan.ram_mb = ram_mb;
        plan.bandwidth_gb = bandwidth_gb;
        plan.price = round_money(price);
        if let Some(cycle) = &body.billing_cycle {
            validate_billing_cycle(cycle)?;
            plan.billing_cycle = cycle.clone();
        }
        if let Some(currency) = &body.currency {
            plan.currency = validate_currency(currency)?;
        }
        if let Some(product_id) = &body.store_product_id {
            plan.store_product_id = product_id
                .clone()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(is_active) = body.is_active {
            plan.is_active = is_active;
        }
        plan.updated_at = Utc::now();
        state.plans.insert(plan.id, plan.clone());
        ctx.hooks.emit(
            "hosting.plan.updated",
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        Ok(plan)
    })();
    json::respond(result, debug)
}

async fn delete_plan(
    _ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = _ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        if !state.plans.contains_key(&id) {
            return Err(UnifiedError::not_found("Hosting plan not found"));
        }
        if state
            .accounts
            .iter()
            .any(|entry| entry.value().plan_id == id)
        {
            return Err(UnifiedError::conflict(
                "Hosting plan cannot be deleted: hosting accounts still reference it",
            ));
        }
        state.plans.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Account handlers.
// ---------------------------------------------------------------------------

async fn list_accounts(
    state: web::Data<Arc<HostingState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let mut items: Vec<HostingAccount> = state
        .accounts
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|account| account.domain.to_lowercase().contains(&needle));
    }
    if let Some(status) = params.filters.get("status") {
        items.retain(|account| account.status == *status);
    }
    if let Some(plan_id) = params.filters.get("plan_id") {
        if let Ok(wanted) = plan_id.parse::<Uuid>() {
            items.retain(|account| account.plan_id == wanted);
        }
    }
    if let Some(company_id) = params.filters.get("company_id") {
        if let Ok(wanted) = company_id.parse::<Uuid>() {
            items.retain(|account| account.company_id == Some(wanted));
        }
    }
    if let Some(ssl_state) = params.filters.get("ssl_state") {
        items.retain(|account| account.ssl_state == *ssl_state);
    }
    let sort = params.allowed_sort(&["domain", "status", "created_at"], "created_at");
    match sort.as_str() {
        "domain" => items.sort_by(|a, b| a.domain.cmp(&b.domain)),
        "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params).to_json(), 200)
}

async fn create_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    body: web::Json<CreateAccountRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingAccount> = (|| {
        let plan = state
            .plans
            .get(&body.plan_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting plan not found"))?;
        if !plan.is_active {
            return Err(UnifiedError::conflict(
                "Hosting plan is not active; activate the plan before creating accounts on it",
            ));
        }
        let syntax = DnsVerifier::check_domain_syntax(&body.domain);
        if !syntax.valid {
            return Err(UnifiedError::bad_request(format!(
                "Invalid domain: {}",
                syntax.issues.join("; ")
            )));
        }
        let domain = syntax
            .punycode
            .clone()
            .unwrap_or_else(|| body.domain.trim().to_lowercase());
        if let Some(company_id) = body.company_id {
            if !company_exists(&ctx, company_id) {
                return Err(UnifiedError::not_found("Company not found"));
            }
        }
        let store_order_id = body
            .store_order_id
            .clone()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let disk_used_mb = body.disk_used_mb.unwrap_or(0);
        let bandwidth_used_mb = body.bandwidth_used_mb.unwrap_or(0);
        validate_usage_against_plan(&plan, disk_used_mb, bandwidth_used_mb)?;
        if state.accounts.iter().any(|entry| {
            entry.value().domain == domain && entry.value().status != "cancelled"
        }) {
            return Err(UnifiedError::conflict(format!(
                "Hosting account for domain \"{domain}\" already exists"
            )));
        }
        let now = Utc::now();
        let account = HostingAccount {
            id: Uuid::now_v7(),
            plan_id: plan.id,
            domain,
            company_id: body.company_id,
            status: "pending".to_string(),
            ssl_state: "none".to_string(),
            disk_used_mb,
            bandwidth_used_mb,
            store_order_id,
            created_at: now,
            updated_at: now,
        };
        state.accounts.insert(account.id, account.clone());
        ctx.hooks.emit(
            "hosting.account.created",
            serde_json::to_value(&account).unwrap_or(Value::Null),
        );
        Ok(account)
    })();
    match result {
        Ok(account) => json::success_response(account, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingAccount> = (|| {
        state
            .accounts
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting account not found"))
    })();
    json::respond(result, debug)
}

async fn update_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateAccountRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingAccount> = (|| {
        let id = id.into_inner();
        let mut account = state
            .accounts
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting account not found"))?;
        if account.status == "cancelled" {
            return Err(UnifiedError::conflict(
                "Cancelled hosting accounts cannot be updated",
            ));
        }
        let mut plan = state
            .plans
            .get(&account.plan_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::internal("Hosting plan for this account is missing"))?;
        if let Some(plan_id) = body.plan_id {
            if plan_id != account.plan_id {
                if account.status != "pending" {
                    return Err(UnifiedError::conflict(
                        "The plan of a hosting account can only be changed while it is pending",
                    ));
                }
                plan = state
                    .plans
                    .get(&plan_id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found("Hosting plan not found"))?;
                if !plan.is_active {
                    return Err(UnifiedError::conflict(
                        "Hosting plan is not active",
                    ));
                }
                account.plan_id = plan_id;
            }
        }
        if let Some(company_id) = &body.company_id {
            if let Some(company_id) = company_id {
                if !company_exists(&ctx, *company_id) {
                    return Err(UnifiedError::not_found("Company not found"));
                }
            }
            account.company_id = *company_id;
        }
        let disk_used_mb = body.disk_used_mb.unwrap_or(account.disk_used_mb);
        let bandwidth_used_mb = body
            .bandwidth_used_mb
            .unwrap_or(account.bandwidth_used_mb);
        validate_usage_against_plan(&plan, disk_used_mb, bandwidth_used_mb)?;
        account.disk_used_mb = disk_used_mb;
        account.bandwidth_used_mb = bandwidth_used_mb;
        account.updated_at = Utc::now();
        state.accounts.insert(account.id, account.clone());
        ctx.hooks.emit(
            "hosting.account.updated",
            serde_json::to_value(&account).unwrap_or(Value::Null),
        );
        Ok(account)
    })();
    json::respond(result, debug)
}

async fn delete_account(
    _ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = _ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let account = state
            .accounts
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting account not found"))?;
        if account.status != "pending" && account.status != "cancelled" {
            return Err(UnifiedError::conflict(
                "Only pending or cancelled hosting accounts can be deleted; suspend or cancel an active account first",
            ));
        }
        state.accounts.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

/// Shared status-transition handler for activate / suspend /
/// unsuspend / cancel. Activation additionally runs the full
/// requirement check first and is refused (422) while any check
/// fails; the report is carried in the error metadata.
fn transition_account(
    ctx: &AppContext,
    state: &Arc<HostingState>,
    id: Uuid,
    target: &str,
) -> UnifiedResult<HostingAccount> {
    let mut account = state
        .accounts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Hosting account not found"))?;
    if !allowed_transition(&account.status, target) {
        return Err(UnifiedError::conflict(format!(
            "Hosting account cannot move from \"{}\" to \"{}\"",
            account.status, target
        )));
    }
    if target == "active" && account.status == "pending" {
        // Activation: requirements must all pass first.
        let report = check_account_requirements(state, ctx, &account);
        ctx.hooks.emit(
            "hosting.requirements.checked",
            serde_json::to_value(&report).unwrap_or(Value::Null),
        );
        if !report.can_activate {
            return Err(UnifiedError::unprocessable(format!(
                "Hosting account cannot be activated: requirements are {}",
                report.status
            ))
            .with_metadata("account_id", json!(account.id))
            .with_metadata(
                "requirements",
                serde_json::to_value(&report).unwrap_or(Value::Null),
            ));
        }
        if report
            .checks
            .iter()
            .any(|check| check.code == "ssl_ready" && check.status == "ok")
        {
            account.ssl_state = "active".to_string();
        }
    }
    account.status = target.to_string();
    account.updated_at = Utc::now();
    state.accounts.insert(account.id, account.clone());
    let event = match (account.status.as_str(), target) {
        ("active", _) => "hosting.account.activated",
        ("suspended", _) => "hosting.account.suspended",
        ("cancelled", _) => "hosting.account.cancelled",
        _ => "hosting.account.updated",
    };
    ctx.hooks.emit(
        event,
        serde_json::to_value(&account).unwrap_or(Value::Null),
    );
    Ok(account)
}

async fn activate_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(transition_account(&ctx, &state, id.into_inner(), "active"), debug)
}

async fn suspend_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        transition_account(&ctx, &state, id.into_inner(), "suspended"),
        debug,
    )
}

async fn unsuspend_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(transition_account(&ctx, &state, id.into_inner(), "active"), debug)
}

async fn cancel_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        transition_account(&ctx, &state, id.into_inner(), "cancelled"),
        debug,
    )
}

async fn get_account_requirements(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AccountRequirementReport> = (|| {
        let account = state
            .accounts
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting account not found"))?;
        let report = check_account_requirements(&state, &ctx, &account);
        ctx.hooks.emit(
            "hosting.requirements.checked",
            serde_json::to_value(&report).unwrap_or(Value::Null),
        );
        Ok(report)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook listeners (store / domains / SSL observations).
// ---------------------------------------------------------------------------

/// Record a store order as a provisioning claim when any of its items
/// buys a hosting plan (the item's store product id matches a plan's
/// linked store product id).
fn record_order_claim(state: &Arc<HostingState>, data: &Value) {
    let Some(order_id) = data.get("id").and_then(Value::as_str) else {
        return;
    };
    let user_id = data
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let company_id = data
        .get("company_id")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<Uuid>().ok());
    let Some(items) = data.get("items").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let Some(product_id) = item.get("product_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(plan) = state
            .plans
            .iter()
            .find(|entry| entry.value().store_product_id.as_deref() == Some(product_id))
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        state.order_claims.insert(
            order_id.to_string(),
            OrderClaim {
                order_id: order_id.to_string(),
                plan_id: plan.id,
                user_id: user_id.clone(),
                company_id,
            },
        );
    }
}

/// Record domain / SSL facts from a `domain.verify` report.
fn observe_domain_verify(state: &Arc<HostingState>, data: &Value) {
    let Some(domain) = data.get("domain").and_then(Value::as_str) else {
        return;
    };
    let ssl_status = data
        .get("ssl_status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let ssl_ok = data.get("ssl_ok").and_then(Value::as_bool).unwrap_or(false);
    let ssl_state = if !ssl_status.is_empty() {
        ssl_status
    } else if ssl_ok {
        "active".to_string()
    } else {
        "none".to_string()
    };
    state.domain_observations.insert(
        domain.to_string(),
        DomainObservation {
            exists: true,
            ssl_state,
            ssl_expires_at: parse_expiry(data.get("ssl_expires_at")),
            observed_at: Utc::now(),
        },
    );
}

/// Record a completed SSL renewal / swap: only payloads carrying a
/// new expiry (or an explicit `completed` flag) count, so a renewal
/// request is never mistaken for a completed one.
fn observe_ssl_completion(state: &Arc<HostingState>, data: &Value) {
    let completed = data.get("completed").and_then(Value::as_bool).unwrap_or(false);
    let expires_at = parse_expiry(data.get("ssl_expires_at"));
    if expires_at.is_none() && !completed {
        return;
    }
    let Some(domain) = data.get("domain").and_then(Value::as_str) else {
        return;
    };
    state.domain_observations.insert(
        domain.to_string(),
        DomainObservation {
            exists: true,
            ssl_state: "active".to_string(),
            ssl_expires_at: expires_at,
            observed_at: Utc::now(),
        },
    );
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct HostingModule {
    state: Arc<HostingState>,
}

impl HostingModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(HostingState::new()),
        }
    }
}

impl Default for HostingModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for HostingModule {
    fn id(&self) -> &'static str {
        "hosting"
    }

    fn name(&self) -> &'static str {
        "Hosting Manager"
    }

    fn description(&self) -> &'static str {
        "Hosting plans and accounts: sellable resource bundles linked to store products, accounts bound to a registered domain with SSL state, and a requirements check before activation."
    }

    fn requires(&self) -> &[&'static str] {
        // Store (plans are sold as store products), Domains (accounts
        // are bound to a registered domain; SSL state is surfaced
        // through the domains module and its ssl.renew / ssl.swap
        // events — there is no separate ssl module id), Network
        // (domain syntax checks and the DNS / TLS probe behind domain
        // verification).
        &["store", "domains", "network"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["serde", "uuid", "chrono", "actix-web", "dashmap", "diesel"]
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "store.product.published",
            "hosting",
            Arc::new(move |context: &mut HookContext| {
                if let Some(product_id) = context.get("id").and_then(Value::as_str) {
                    state
                        .known_products
                        .insert(product_id.to_string(), true);
                }
            }),
        );
        let state = self.state.clone();
        bus.register(
            "store.product.unpublished",
            "hosting",
            Arc::new(move |context: &mut HookContext| {
                if let Some(product_id) = context.get("id").and_then(Value::as_str) {
                    state
                        .known_products
                        .insert(product_id.to_string(), false);
                }
            }),
        );
        // A paid order for a hosting plan becomes a provisioning
        // claim; both the completion event and the payment event are
        // observed so the claim exists as early as possible.
        for event in ["store.order.completed", "order.paid"] {
            let state = self.state.clone();
            bus.register(
                event,
                "hosting",
                Arc::new(move |context: &mut HookContext| {
                    record_order_claim(&state, &context.data);
                }),
            );
        }
        let state = self.state.clone();
        bus.register(
            "domain.verify",
            "hosting",
            Arc::new(move |context: &mut HookContext| {
                observe_domain_verify(&state, &context.data);
            }),
        );
        for event in [events::SSL_RENEW, events::SSL_SWAP] {
            let state = self.state.clone();
            bus.register(
                event,
                "hosting",
                Arc::new(move |context: &mut HookContext| {
                    observe_ssl_completion(&state, &context.data);
                }),
            );
        }
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/hosting")
                .route("/plans", web::get().to(list_plans))
                .route("/plans", web::post().to(create_plan))
                .route("/plans/{id}", web::get().to(get_plan))
                .route("/plans/{id}", web::put().to(update_plan))
                .route("/plans/{id}", web::delete().to(delete_plan))
                .route("/accounts", web::get().to(list_accounts))
                .route("/accounts", web::post().to(create_account))
                .route("/accounts/{id}", web::get().to(get_account))
                .route("/accounts/{id}", web::put().to(update_account))
                .route("/accounts/{id}", web::delete().to(delete_account))
                .route(
                    "/accounts/{id}/requirements",
                    web::get().to(get_account_requirements),
                )
                .route(
                    "/accounts/{id}/activate",
                    web::post().to(activate_account),
                )
                .route(
                    "/accounts/{id}/suspend",
                    web::post().to(suspend_account),
                )
                .route(
                    "/accounts/{id}/unsuspend",
                    web::post().to(unsuspend_account),
                )
                .route(
                    "/accounts/{id}/cancel",
                    web::post().to(cancel_account),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/hosting/plans",
                owner: "hosting",
                description: "List hosting plans",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/plans",
                owner: "hosting",
                description: "Create a hosting plan",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/hosting/plans/{id}",
                owner: "hosting",
                description: "Get one hosting plan",
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/hosting/plans/{id}",
                owner: "hosting",
                description: "Update a hosting plan",
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/hosting/plans/{id}",
                owner: "hosting",
                description: "Delete a hosting plan",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/hosting/accounts",
                owner: "hosting",
                description: "List hosting accounts",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/accounts",
                owner: "hosting",
                description: "Create a hosting account",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/hosting/accounts/{id}",
                owner: "hosting",
                description: "Get one hosting account",
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/hosting/accounts/{id}",
                owner: "hosting",
                description: "Update a hosting account",
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/hosting/accounts/{id}",
                owner: "hosting",
                description: "Delete a pending or cancelled hosting account",
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/hosting/accounts/{id}/requirements",
                owner: "hosting",
                description: "Check a hosting account's activation requirements",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/accounts/{id}/activate",
                owner: "hosting",
                description: "Activate a hosting account after its requirements pass",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/accounts/{id}/suspend",
                owner: "hosting",
                description: "Suspend a hosting account",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/accounts/{id}/unsuspend",
                owner: "hosting",
                description: "Unsuspend a hosting account",
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/hosting/accounts/{id}/cancel",
                owner: "hosting",
                description: "Cancel a hosting account",
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(HostingModule::new()));
}
