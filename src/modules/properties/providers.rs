// Properties - Location providers (pluggable geocoder).

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// Location providers (pluggable geocoder)

/// Pluggable geocoder provider. Implementations must never log a
/// query or a configured URL that could contain credentials; this
/// codebase's providers need no API key at all.
#[async_trait]
pub trait LocationProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn search(&self, query: &str) -> UnifiedResult<Vec<PlaceCandidate>>;
    async fn reverse_geocode(
        &self,
        latitude: f64,
        longitude: f64,
    ) -> UnifiedResult<Option<PlaceCandidate>>;
}

/// The `none` provider: manual coordinates only. Search and
/// reverse geocoding answer with a clear "not configured" error
/// instead of a fabricated result.
pub struct NoneProvider;

#[async_trait]
impl LocationProvider for NoneProvider {
    fn name(&self) -> &'static str {
        "none"
    }

    async fn search(&self, _query: &str) -> UnifiedResult<Vec<PlaceCandidate>> {
        Err(UnifiedError::bad_request(
            "No location provider is configured: set provider to \"nominatim\" in the location settings, or enter coordinates manually",
        ))
    }

    async fn reverse_geocode(
        &self,
        _latitude: f64,
        _longitude: f64,
    ) -> UnifiedResult<Option<PlaceCandidate>> {
        Err(UnifiedError::bad_request(
            "No location provider is configured: set provider to \"nominatim\" in the location settings, or enter coordinates manually",
        ))
    }
}

/// One result row of a Nominatim-compatible `/search` response.
/// Nominatim returns latitude / longitude as strings.
#[derive(Debug, Deserialize)]
pub(crate) struct NominatimSearchItem {
    display_name: String,
    lat: String,
    lon: String,
    #[serde(default)]
    #[serde(rename = "type")]
    place_type: Option<String>,
    #[serde(default)]
    importance: Option<f64>,
}

/// One result row of a Nominatim-compatible `/reverse` response.
#[derive(Debug, Deserialize)]
pub(crate) struct NominatimReverseItem {
    display_name: String,
    lat: String,
    lon: String,
    #[serde(default)]
    #[serde(rename = "type")]
    place_type: Option<String>,
}

/// HTTP adapter for a Nominatim-compatible geocoding endpoint.
/// The base URL is validated at construction: HTTPS only, host
/// must equal the configured allowlisted host exactly, no
/// non-standard port. Requests carry a fixed User-Agent and a
/// bounded timeout. No API key is sent or logged.
pub struct NominatimProvider {
    base_url: String,
    client: reqwest::Client,
}

impl NominatimProvider {
    pub fn new(base_url: &str, timeout_secs: u64) -> UnifiedResult<Self> {
        let parsed = reqwest::Url::parse(base_url).map_err(|_| {
            UnifiedError::bad_request(format!("Invalid geocoder base URL: \"{base_url}\""))
        })?;
        if parsed.scheme() != "https" {
            return Err(UnifiedError::bad_request(
                "Geocoder base URL must use https",
            ));
        }
        if parsed.host_str().is_none() {
            return Err(UnifiedError::bad_request(
                "Geocoder base URL must have a host",
            ));
        }
        if parsed.port().is_some_and(|port| port != 443) {
            return Err(UnifiedError::bad_request(
                "Geocoder base URL must not use a non-standard port",
            ));
        }
        // The host allowlist is enforced by the settings layer
        // (`build_provider` compares the host against
        // `allowed_host` before constructing the adapter).
        let client = reqwest::Client::builder()
            .user_agent(GEOCODER_USER_AGENT)
            .timeout(Duration::from_secs(timeout_secs))
            .connect_timeout(Duration::from_secs(timeout_secs.min(5)))
            .build()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot build geocoder HTTP client: {error}"))
            })?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        })
    }

    fn parse_candidate(item: NominatimSearchItem) -> Option<PlaceCandidate> {
        let latitude: f64 = item.lat.parse().ok()?;
        let longitude: f64 = item.lon.parse().ok()?;
        if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
            return None;
        }
        Some(PlaceCandidate {
            display_name: item.display_name,
            latitude,
            longitude,
            place_type: item.place_type,
            importance: item.importance,
        })
    }
}

#[async_trait]
impl LocationProvider for NominatimProvider {
    fn name(&self) -> &'static str {
        "nominatim"
    }

    async fn search(&self, query: &str) -> UnifiedResult<Vec<PlaceCandidate>> {
        let url = format!("{}/search", self.base_url);
        let response = self
            .client
            .get(url)
            .query(&[
                ("format", "jsonv2"),
                ("q", query),
                ("limit", "5"),
                ("addressdetails", "1"),
            ])
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Geocoder request failed: {error}"))
            })?;
        if !response.status().is_success() {
            return Err(UnifiedError::internal(format!(
                "Geocoder answered with HTTP {}",
                response.status()
            )));
        }
        let items: Vec<NominatimSearchItem> = response.json().await.map_err(|error| {
            UnifiedError::internal(format!("Geocoder answered with an unreadable body: {error}"))
        })?;
        let mut out: Vec<PlaceCandidate> = items
            .into_iter()
            .filter_map(Self::parse_candidate)
            .take(MAX_SEARCH_RESULTS)
            .collect();
        out.truncate(MAX_SEARCH_RESULTS);
        Ok(out)
    }

    async fn reverse_geocode(
        &self,
        latitude: f64,
        longitude: f64,
    ) -> UnifiedResult<Option<PlaceCandidate>> {
        let url = format!("{}/reverse", self.base_url);
        let latitude = latitude.to_string();
        let longitude = longitude.to_string();
        let response = self
            .client
            .get(url)
            .query(&[
                ("format", "jsonv2"),
                ("lat", latitude.as_str()),
                ("lon", longitude.as_str()),
                ("zoom", "18"),
                ("addressdetails", "1"),
            ])
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Geocoder request failed: {error}"))
            })?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(UnifiedError::internal(format!(
                "Geocoder answered with HTTP {}",
                response.status()
            )));
        }
        let item: NominatimReverseItem = response.json().await.map_err(|error| {
            UnifiedError::internal(format!("Geocoder answered with an unreadable body: {error}"))
        })?;
        let latitude: f64 = item.lat.parse().map_err(|_| {
            UnifiedError::internal("Geocoder answered with an invalid latitude")
        })?;
        let longitude: f64 = item.lon.parse().map_err(|_| {
            UnifiedError::internal("Geocoder answered with an invalid longitude")
        })?;
        Ok(Some(PlaceCandidate {
            display_name: item.display_name,
            latitude,
            longitude,
            place_type: item.place_type,
            importance: None,
        }))
    }
}

/// Build the provider for a settings snapshot. Returns an error
/// when the settings name a provider that cannot be constructed
/// (for example a Nominatim base URL whose host does not match
/// the allowlisted host).
pub(crate) fn build_provider(settings: &LocationSettings) -> UnifiedResult<Arc<dyn LocationProvider>> {
    match settings.provider.as_str() {
        "none" => Ok(Arc::new(NoneProvider)),
        "nominatim" => {
            let Some(base_url) = &settings.base_url else {
                return Err(UnifiedError::bad_request(
                    "Nominatim provider needs a base_url in the location settings",
                ));
            };
            let parsed = reqwest::Url::parse(base_url).map_err(|_| {
                UnifiedError::bad_request(format!("Invalid geocoder base URL: \"{base_url}\""))
            })?;
            let host = parsed.host_str().unwrap_or("");
            if host != settings.allowed_host {
                return Err(UnifiedError::bad_request(format!(
                    "Geocoder host \"{host}\" is not the allowlisted host \"{}\"",
                    settings.allowed_host
                )));
            }
            Ok(Arc::new(NominatimProvider::new(
                base_url,
                settings.timeout_secs,
            )?))
        }
        other => Err(UnifiedError::bad_request(format!(
            "Unknown location provider \"{other}\": expected \"none\" or \"nominatim\""
        ))),
    }
}

/// A hostname is labels of letters, digits, and hyphens separated
/// by dots, never empty, never starting or ending with a hyphen
/// or a dot, at most 253 characters.
pub(crate) fn valid_hostname(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    if host.starts_with('-') || host.ends_with('-') || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}
