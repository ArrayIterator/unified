//! geo - types.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use serde::{Deserialize, Serialize};



#[derive(Debug, Deserialize)]
// ---------------------------------------------------------------------------
// Request / response types.
// ---------------------------------------------------------------------------
pub(crate) struct LookupQuery {
    pub(crate) ip: Option<String>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct CreateOverrideRequest {
    pub(crate) cidr: String,
    pub(crate) country_code: String,
    pub(crate) country_name: Option<String>,
    pub(crate) city: Option<String>,
    pub(crate) asn: Option<i32>,
    pub(crate) asn_organization: Option<String>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct UpdateOverrideRequest {
    pub(crate) cidr: Option<String>,
    pub(crate) country_code: Option<String>,
    pub(crate) country_name: Option<Option<String>>,
    pub(crate) city: Option<Option<String>>,
    pub(crate) asn: Option<Option<i32>>,
    pub(crate) asn_organization: Option<Option<String>>,
    pub(crate) is_active: Option<bool>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct AsnCheckRequest {
    pub(crate) ip: String,
}


#[derive(Debug, Deserialize)]
pub(crate) struct DownloadRequest {
    /// Optional custom download URL. Defaults to `geo.download_url`, then
    /// the built-in MaxMind URL (requires `licenses.maxmind`). A custom
    /// URL is only accepted when its host matches the configured
    /// download host (or the built-in MaxMind host when nothing is
    /// configured); see `resolve_download_url` in handlers.rs.
    pub(crate) url: Option<String>,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct GeoSettingsView {
    pub(crate) database_path: String,
    pub(crate) database_exists: bool,
    pub(crate) download_url: String,
    pub(crate) auto_update: bool,
    pub(crate) update_interval_hours: u64,
    pub(crate) settings_override: bool,
    pub(crate) maxmind_key_configured: bool,
    /// Shape check only (40 characters, `xxxxxx_..._mmk`); a key can be
    /// configured yet malformed, which is exactly the failure that
    /// makes a database download fail later.
    pub(crate) maxmind_key_valid: bool,
    pub(crate) overrides_count: usize,
}


pub(crate) fn validate_country_code(code: &str) -> UnifiedResult<String> {
    let code = code.trim().to_uppercase();
    if code.len() != 2 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "country_code must be a two-letter ISO country code",
        ));
    }
    Ok(code)
}


/// Validate an ASN from an override request: negative values are
/// rejected (they would wrap to a huge `u32` when `find_override`
/// converts the stored `i32` with `as u32`), and zero means "no ASN"
/// because autonomous system 0 is reserved.
pub(crate) fn validate_asn(asn: Option<i32>) -> UnifiedResult<Option<i32>> {
    match asn {
        Some(value) if value < 0 => Err(UnifiedError::bad_request(
            "asn must be a non-negative autonomous system number",
        )),
        Some(0) => Ok(None),
        other => Ok(other),
    }
}


pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_asn_rejects_negative_values() {
        assert!(validate_asn(Some(-1)).is_err());
        assert!(validate_asn(Some(i32::MIN)).is_err());
    }

    #[test]
    fn validate_asn_treats_zero_as_absent() {
        assert_eq!(validate_asn(Some(0)).unwrap(), None);
    }

    #[test]
    fn validate_asn_keeps_positive_values_and_none() {
        assert_eq!(validate_asn(Some(15169)).unwrap(), Some(15169));
        assert_eq!(validate_asn(None).unwrap(), None);
    }
}
