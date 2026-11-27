//! panels module - WHMCS adapter.

use unified_core::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use serde_json::Value;
use url::Url;

use super::adapter::PanelAdapter;
use super::adapter::build_client;
use super::adapter::check_panel_host;
use super::adapter::display_name;
use super::adapter::extract_array;
use super::adapter::first_string;
use super::adapter::join_path;
use super::adapter::json_f64;
use super::adapter::json_string;
use super::adapter::json_string_owned;
use super::adapter::normalize_status;
use super::adapter::send_json;
use super::constants::MAX_LIST_LIMIT;
use super::handlers_connections::test_connection;
use super::types::PanelClient;
use super::types::PanelConnection;
use super::types::PanelDomain;
use super::types::PanelInfo;
use super::types::PanelInvoice;
use super::types::PanelKind;
use super::types::PanelProduct;
use super::types::PanelService;
use super::types::validate_panel_endpoint;

// WHMCS adapter.

/// WHMCS action API (`<endpoint>/includes/api.php`, form-encoded,
/// `identifier` + `secret` + `action` + `responsetype=json`).
///
/// Honest scope: read-only import of clients, products, services
/// (client products), domains, and invoices. Every list call walks
/// WHMCS pagination (`limitstart` / `limitnum`, 250 per page) until
/// the reported total is reached or a short page arrives, so large
/// installs import every record, not just the first page. A
/// collection too large to fetch completely (over
/// `MAX_WHMCS_RECORDS` records) fails the sync instead of returning
/// a truncated page: sync treats absence from a fetched set as a
/// remote deletion, so a partial page must never be presented as
/// the whole collection. No write operations (no client / invoice /
/// service creation or update).
/// Most WHMCS records fetched for one collection in a single sync
/// (400 pages of 250). Past this the adapter fails instead of
/// truncating: a partial page would let sync report - and
/// remote-wins delete - records the panel still has.
const MAX_WHMCS_PAGES: u32 = 400;
const MAX_WHMCS_RECORDS: u64 = MAX_WHMCS_PAGES as u64 * MAX_LIST_LIMIT as u64;

/// Total record count WHMCS reports for an action (`totalresults`,
/// a number or a numeric string). `None` when the panel does not
/// report one, in which case a short page is the only end signal.
pub(crate) fn whmcs_total_results(body: &Value) -> Option<u64> {
    match body.get("totalresults") {
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(text)) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// Whether another page must be fetched after a page of `page_len`
/// records, given `fetched` records so far and the reported total.
/// A short page always ends the walk, even when the total is
/// unknown or disagrees with the page size.
pub(crate) fn whmcs_has_more_pages(page_len: usize, fetched: u64, total: Option<u64>) -> bool {
    if page_len < MAX_LIST_LIMIT as usize {
        return false;
    }
    match total {
        Some(total) => fetched < total,
        None => true,
    }
}
pub struct WhmcsAdapter {
    pub(crate) connection: PanelConnection,
    pub(crate) client: reqwest::Client,
    pub(crate) base: Url,
}

impl WhmcsAdapter {
    pub fn new(connection: &PanelConnection) -> UnifiedResult<Self> {
        let endpoint = connection.endpoint.clone().unwrap_or_default();
        let base = validate_panel_endpoint(Some(&endpoint), "whmcs", connection.allow_private_endpoint)?;
        Ok(Self {
            connection: connection.clone(),
            client: build_client(&base, connection.allow_private_endpoint)?,
            base,
        })
    }

    pub(crate) fn api_url(&self) -> UnifiedResult<Url> {
        let url = join_path(&self.base, "/includes/api.php")?;
        check_panel_host(&url, &self.base, "whmcs", self.connection.allow_private_endpoint)?;
        Ok(url)
    }

    pub(crate) async fn call(&self, action: &str) -> UnifiedResult<Value> {
        self.call_page(action, 0).await
    }

    pub(crate) async fn call_page(&self, action: &str, limitstart: u32) -> UnifiedResult<Value> {
        let identifier = self
            .connection
            .credentials
            .api_identifier
            .clone()
            .unwrap_or_default();
        let secret = self
            .connection
            .credentials
            .api_secret
            .clone()
            .unwrap_or_default();
        let params: Vec<(String, String)> = vec![
            ("identifier".to_string(), identifier),
            ("secret".to_string(), secret),
            ("action".to_string(), action.to_string()),
            ("responsetype".to_string(), "json".to_string()),
            ("limitnum".to_string(), MAX_LIST_LIMIT.to_string()),
            ("limitstart".to_string(), limitstart.to_string()),
        ];
        let started = std::time::Instant::now();
        let body = send_json(
            &self.client,
            self.client.post(self.api_url()?).form(&params),
            "whmcs",
        )
        .await?;
        let _ = started;
        // WHMCS reports API-level failures inside a 200 body.
        if json_string(&body, "result").is_some_and(|result| result == "error") {
            let message = json_string(&body, "message").unwrap_or("WHMCS API error");
            return Err(UnifiedError::bad_request(format!(
                "WHMCS API action \"{action}\" failed: {message}"
            )));
        }
        Ok(body)
    }

    /// Fetch every page of an action and return all records as owned
    /// values. Fails (instead of truncating) when the collection is
    /// larger than `MAX_WHMCS_RECORDS`, so callers never mistake a
    /// partial page for the whole collection.
    pub(crate) async fn call_all(
        &self,
        action: &str,
        wrappers: &[&str],
    ) -> UnifiedResult<Vec<Value>> {
        let mut items: Vec<Value> = Vec::new();
        let mut limitstart: u32 = 0;
        loop {
            let body = self.call_page(action, limitstart).await?;
            let total = whmcs_total_results(&body);
            if let Some(total) = total {
                if total > MAX_WHMCS_RECORDS {
                    return Err(UnifiedError::bad_request(format!(
                        "WHMCS action \"{action}\" reports {total} records, over the {MAX_WHMCS_RECORDS} import limit; refusing a partial import"
                    )));
                }
            }
            let page: Vec<Value> = extract_array(&body, None, wrappers)
                .into_iter()
                .cloned()
                .collect();
            let page_len = page.len();
            items.extend(page);
            if !whmcs_has_more_pages(page_len, items.len() as u64, total) {
                break;
            }
            limitstart += MAX_LIST_LIMIT;
            if limitstart / MAX_LIST_LIMIT >= MAX_WHMCS_PAGES {
                return Err(UnifiedError::bad_request(format!(
                    "WHMCS action \"{action}\" exceeded {MAX_WHMCS_PAGES} pages; refusing a partial import"
                )));
            }
        }
        Ok(items)
    }

    pub(crate) async fn test_with(&self, action: &str) -> UnifiedResult<PanelInfo> {
        let started = std::time::Instant::now();
        self.call(action).await?;
        Ok(PanelInfo {
            kind: "whmcs".to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            detail: format!("WHMCS API action \"{action}\" succeeded"),
        })
    }
}

#[async_trait]
impl PanelAdapter for WhmcsAdapter {
    fn kind(&self) -> PanelKind {
        PanelKind::Whmcs
    }

    async fn test_connection(&self) -> UnifiedResult<PanelInfo> {
        self.test_with("GetClients").await
    }

    async fn list_clients(&self) -> UnifiedResult<Vec<PanelClient>> {
        let items = self.call_all("GetClients", &["clients.client", "clients"]).await?;
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "userid"])?;
                Some(PanelClient {
                    id,
                    name: display_name(item),
                    email: json_string_owned(item, "email"),
                    company: json_string_owned(item, "companyname"),
                    status: normalize_status(json_string(item, "status").unwrap_or("active")),
                    created_at: json_string_owned(item, "datecreated"),
                })
            })
            .collect())
    }

    async fn list_products(&self) -> UnifiedResult<Vec<PanelProduct>> {
        let items = self.call_all("GetProducts", &["products.product", "products"]).await?;
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["pid", "id"])?;
                let pricing = item.get("pricing");
                let price = pricing.and_then(|pricing| {
                    pricing
                        .get("USD")
                        .and_then(|usd| json_f64(usd, "monthly"))
                        .or_else(|| json_f64(pricing, "monthly"))
                });
                Some(PanelProduct {
                    id,
                    name: first_string(item, &["name"]).unwrap_or_else(|| "Unknown".to_string()),
                    description: json_string_owned(item, "description"),
                    price,
                    currency: Some("USD".to_string()),
                    billing_cycle: None,
                    status: "active".to_string(),
                })
            })
            .collect())
    }

    async fn list_services(&self) -> UnifiedResult<Vec<PanelService>> {
        let items = self.call_all("GetClientsProducts", &["products.product", "products"]).await?;
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "serviceid"])?;
                Some(PanelService {
                    id,
                    name: first_string(item, &["name", "productname"])
                        .unwrap_or_else(|| "Unknown".to_string()),
                    client_id: first_string(item, &["clientid", "userid"]),
                    product_id: first_string(item, &["pid", "productid"]),
                    domain: json_string_owned(item, "domain"),
                    status: normalize_status(json_string(item, "status").unwrap_or("active")),
                })
            })
            .collect())
    }

    async fn list_domains(&self) -> UnifiedResult<Vec<PanelDomain>> {
        let items = self.call_all("GetDomains", &["domains.domain", "domains"]).await?;
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "domainid"])?;
                Some(PanelDomain {
                    id,
                    name: first_string(item, &["domainname", "domain"])
                        .unwrap_or_else(|| "Unknown".to_string()),
                    client_id: first_string(item, &["userid", "clientid"]),
                    status: normalize_status(json_string(item, "status").unwrap_or("active")),
                    expires_at: json_string_owned(item, "expirydate"),
                })
            })
            .collect())
    }

    async fn list_invoices(&self) -> UnifiedResult<Vec<PanelInvoice>> {
        let items = self.call_all("GetInvoices", &["invoices.invoice", "invoices"]).await?;
        Ok(items
            .iter()
            .filter_map(|item| {
                let id = first_string(item, &["id", "invoiceid"])?;
                Some(PanelInvoice {
                    id,
                    client_id: first_string(item, &["userid", "clientid"]),
                    total: json_f64(item, "total"),
                    currency: json_string_owned(item, "currencycode"),
                    status: normalize_status(json_string(item, "status").unwrap_or("unpaid")),
                    due_date: json_string_owned(item, "duedate"),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::whmcs_has_more_pages;
    use super::whmcs_total_results;
    use serde_json::json;

    #[test]
    fn total_results_reads_number_and_numeric_string() {
        assert_eq!(whmcs_total_results(&json!({"totalresults": 500})), Some(500));
        assert_eq!(
            whmcs_total_results(&json!({"totalresults": "500"})),
            Some(500)
        );
        assert_eq!(whmcs_total_results(&json!({})), None);
        assert_eq!(whmcs_total_results(&json!({"totalresults": "n/a"})), None);
    }

    #[test]
    fn short_page_always_ends_the_walk() {
        assert!(!whmcs_has_more_pages(0, 0, Some(1000)));
        assert!(!whmcs_has_more_pages(249, 249, Some(1000)));
        assert!(!whmcs_has_more_pages(249, 249, None));
    }

    #[test]
    fn full_page_continues_until_total_reached() {
        assert!(whmcs_has_more_pages(250, 250, Some(500)));
        assert!(whmcs_has_more_pages(250, 250, None));
        assert!(!whmcs_has_more_pages(250, 500, Some(500)));
        assert!(!whmcs_has_more_pages(250, 501, Some(500)));
    }
}
