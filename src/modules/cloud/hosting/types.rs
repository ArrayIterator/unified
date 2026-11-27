// hosting module - Domain types.
//
// Split from the former single-file module `src/modules/hosting.rs`.
// The module root (`hosting.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::Domain;
use crate::modules::company::company_exists;
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

// Types.

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
    /// Price per billing cycle in minor units of `currency`. JSON
    /// carries major units (see `serde_major`).
    #[serde(with = "unified_core::money::serde_major")]
    pub price: i64,
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
pub(crate) struct DomainObservation {
    pub(crate) exists: bool,
    pub(crate) ssl_state: String,
    pub(crate) ssl_expires_at: Option<DateTime<Utc>>,
    pub(crate) observed_at: DateTime<Utc>,
}

/// A paid store order for a hosting plan, recorded from
/// `store.order.completed` / `order.paid` hook events.
#[derive(Debug, Clone)]
pub(crate) struct OrderClaim {
    pub(crate) order_id: String,
    pub(crate) plan_id: Uuid,
    pub(crate) user_id: String,
    pub(crate) company_id: Option<String>,
}

pub struct HostingState {
    pub(crate) plans: DashMap<Uuid, HostingPlan>,
    pub(crate) accounts: DashMap<Uuid, HostingAccount>,
    /// Store product id -> whether it is currently published.
    pub(crate) known_products: DashMap<String, bool>,
    /// Domain name -> last observed domain / SSL facts.
    pub(crate) domain_observations: DashMap<String, DomainObservation>,
    /// (Store order id, plan id) -> provisioning claim. One order
    /// can pay for several plans, so claims are keyed by both fields
    /// and no plan's claim overwrites another's.
    pub(crate) order_claims: DashMap<(String, Uuid), OrderClaim>,
}

impl HostingState {
    pub(crate) fn new() -> Self {
        Self {
            plans: DashMap::new(),
            accounts: DashMap::new(),
            known_products: DashMap::new(),
            domain_observations: DashMap::new(),
            order_claims: DashMap::new(),
        }
    }
}

/// Process-wide shared hosting state.
///
/// The hosting state is shared between the compat `hosting` module
/// (legacy `/api/v1/hosting/*` routes) and the `cloud` module
/// (`/api/v1/cloud/hosting/*`), so a plan or account created under
/// either scope is visible under the other. Both modules register the
/// same `Arc` as actix app data in their own scopes, and the hook
/// subscriptions capture it too.
pub(crate) fn shared_state() -> Arc<HostingState> {
    static STATE: OnceLock<Arc<HostingState>> = OnceLock::new();
    STATE.get_or_init(|| Arc::new(HostingState::new())).clone()
}

impl Default for HostingState {
    fn default() -> Self {
        Self::new()
    }
}
