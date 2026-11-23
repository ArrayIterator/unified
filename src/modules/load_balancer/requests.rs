// load_balancer -- Request types.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::store::*;
use super::types::*;
use super::constants::{default_true, default_weight};

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct HealthCheckInput {
    pub(crate) kind: Option<HealthCheckKind>,
    pub(crate) path: Option<String>,
    pub(crate) interval_secs: Option<u64>,
    pub(crate) timeout_secs: Option<u64>,
    pub(crate) healthy_threshold: Option<u32>,
    pub(crate) unhealthy_threshold: Option<u32>,
    pub(crate) expected_status: Option<u16>,
}

impl HealthCheckInput {
    pub(crate) fn merged(&self, base: &HealthCheckConfig) -> HealthCheckConfig {
        HealthCheckConfig {
            kind: self.kind.unwrap_or(base.kind),
            path: self.path.clone().unwrap_or_else(|| base.path.clone()),
            interval_secs: self.interval_secs.unwrap_or(base.interval_secs),
            timeout_secs: self.timeout_secs.unwrap_or(base.timeout_secs),
            healthy_threshold: self.healthy_threshold.unwrap_or(base.healthy_threshold),
            unhealthy_threshold: self.unhealthy_threshold.unwrap_or(base.unhealthy_threshold),
            expected_status: self.expected_status.or(base.expected_status),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateBalancerRequest {
    pub(crate) name: String,
    pub(crate) listen_address: String,
    pub(crate) listen_port: u16,
    #[serde(default)]
    pub(crate) tls_mode: TlsMode,
    pub(crate) domain: Option<String>,
    pub(crate) certificate_name: Option<String>,
    #[serde(default)]
    pub(crate) strategy: Strategy,
    pub(crate) health_check: Option<HealthCheckInput>,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateBalancerRequest {
    pub(crate) name: Option<String>,
    pub(crate) listen_address: Option<String>,
    pub(crate) listen_port: Option<u16>,
    pub(crate) tls_mode: Option<TlsMode>,
    pub(crate) domain: Option<String>,
    pub(crate) certificate_name: Option<String>,
    pub(crate) strategy: Option<Strategy>,
    pub(crate) health_check: Option<HealthCheckInput>,
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateBackendRequest {
    pub(crate) name: String,
    pub(crate) source_kind: BackendSourceKind,
    pub(crate) source_id: Option<String>,
    pub(crate) address: String,
    #[serde(default = "default_weight")]
    pub(crate) weight: u32,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateBackendRequest {
    pub(crate) name: Option<String>,
    pub(crate) source_kind: Option<BackendSourceKind>,
    pub(crate) source_id: Option<String>,
    pub(crate) address: Option<String>,
    pub(crate) weight: Option<u32>,
    pub(crate) enabled: Option<bool>,
    pub(crate) draining: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PreviewRequest {
    pub(crate) client_ip: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RouteQuery {
    pub(crate) client_ip: Option<String>,
}

pub(crate) fn parse_id(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    raw.parse::<Uuid>().map_err(|_| {
        UnifiedError::bad_request(format!("Invalid {what} id \"{raw}\": expected a UUID"))
    })
}

pub(crate) fn get_balancer(id: Uuid) -> UnifiedResult<LoadBalancer> {
    balancers_store()
        .get(&id)
        .map(|entry| entry.clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Load balancer \"{id}\" not found")))
}

pub(crate) fn get_backend(balancer_id: Uuid, backend_id: Uuid) -> UnifiedResult<Backend> {
    backends_store()
        .get(&backend_id)
        .filter(|entry| entry.balancer_id == balancer_id)
        .map(|entry| entry.clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Backend \"{backend_id}\" not found on load balancer \"{balancer_id}\""
            ))
        })
}

pub(crate) fn apply_list_params<T, F>(
    items: Vec<T>,
    query: &PaginationParams,
    sort_allowed: &[&str],
    default_sort: &str,
    mut sort_key: F,
) -> PageResult<T>
where
    T: Serialize,
    F: FnMut(&T, &str) -> String,
{
    let pagination = query.normalized();
    let sort = query.allowed_sort(sort_allowed, default_sort);
    let mut items = items;
    let order_desc = pagination.order == "desc";
    items.sort_by(|a, b| {
        let ordering = sort_key(a, &sort).cmp(&sort_key(b, &sort));
        if order_desc {
            ordering.reverse()
        } else {
            ordering
        }
    });
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn matches_search(haystack: &str, search: &Option<String>) -> bool {
    match search {
        None => true,
        Some(term) => haystack.to_lowercase().contains(&term.to_lowercase()),
    }
}
