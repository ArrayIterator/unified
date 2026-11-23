//! SaaS Domains - settings, defaults and validation.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use std::sync::OnceLock;

use super::validation::validate_hostname;

// ---------------------------------------------------------------------------
// Settings — every tunable lives here, with a factory default that an
// administrator can override through PUT /settings. Nothing below is
// a behaviour constant: change the setting, change the behaviour.
// ---------------------------------------------------------------------------

/// Factory defaults. Each one is overridable through the settings
/// endpoints (`saas_domains.*` config keys); the defaults only apply
/// until an administrator changes them.
fn default_reserved_labels() -> Vec<String> {
    [
        "www",
        "admin",
        "api",
        "mail",
        "app",
        "dashboard",
        "static",
        "cdn",
        "support",
        "status",
    ]
    .iter()
    .map(|label| label.to_string())
    .collect()
}

fn default_label_pattern() -> String {
    // One DNS label: 1-63 chars, starts/ends alphanumeric, hyphens inside.
    r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$".to_string()
}

/// The factory-default label pattern, compiled once. Used as the
/// fallback in `SaasDomainsSettings::label_regex` when the stored
/// pattern fails to compile.
fn default_label_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(&default_label_pattern()).expect("default pattern compiles")
    })
}

fn default_max_product_subdomains_per_tenant() -> u32 {
    10
}

fn default_max_product_subdomains_per_product() -> u32 {
    1
}

fn default_product_subdomain_grace_days() -> u32 {
    30
}

fn default_product_subdomain_suggestion_suffixes() -> Vec<String> {
    ["", "-app", "-online", "-portal", "-hub", "-2", "-3"]
        .iter()
        .map(|suffix| suffix.to_string())
        .collect()
}

fn default_alias_target_types() -> Vec<String> {
    ["cname_subdomain", "a_record", "cname_server"]
        .iter()
        .map(|kind| kind.to_string())
        .collect()
}

/// Admin-editable module settings (`saas_domains.*` config keys).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaasDomainsSettings {
    /// Base domain for tenant subdomains, e.g. `tenants.example.com`.
    /// Empty means tenant-subdomain resolution is disabled.
    #[serde(default)]
    pub wildcard_base: String,
    /// Host that serves the control plane (main site / dashboard).
    /// Empty means "first listen host from the app config".
    #[serde(default)]
    pub main_host: String,
    /// Subdomain labels no tenant may claim.
    #[serde(default = "default_reserved_labels")]
    pub reserved_labels: Vec<String>,
    /// Regex an allocated subdomain label must match.
    #[serde(default = "default_label_pattern")]
    pub label_pattern: String,
    /// Maximum active/pending subdomain allocations per tenant.
    #[serde(default = "default_max_subdomains")]
    pub max_subdomains_per_tenant: u32,
    /// When true, tenant self-assignments stay `pending_approval`
    /// until an admin approves them; admin assignments are active
    /// immediately either way.
    #[serde(default)]
    pub require_subdomain_approval: bool,
    /// Default reservation lifetime in hours.
    #[serde(default = "default_reservation_ttl_hours")]
    pub default_reservation_ttl_hours: u32,
    /// Allowed alias target types (`cname_subdomain`, `a_record`,
    /// `cname_server`). A type outside this list is rejected.
    #[serde(default = "default_alias_target_types")]
    pub alias_target_types: Vec<String>,
    /// Maximum live product subdomains per tenant across all
    /// products.
    #[serde(default = "default_max_product_subdomains_per_tenant")]
    pub max_product_subdomains_per_tenant: u32,
    /// Maximum live product subdomains per tenant for one product
    /// (and one package when a package is named). The factory
    /// default of 1 enforces the "one active product subdomain per
    /// (tenant, product/package)" rule.
    #[serde(default = "default_max_product_subdomains_per_product")]
    pub max_product_subdomains_per_product: u32,
    /// Default grace period in days between a product-subdomain
    /// release and the moment its label may be claimed again.
    #[serde(default = "default_product_subdomain_grace_days")]
    pub default_product_subdomain_grace_days: u32,
    /// When true, activating a product subdomain needs an admin
    /// (`saas_domains.manage`); otherwise the owning tenant may
    /// activate its own verified product subdomain.
    #[serde(default)]
    pub require_product_subdomain_approval: bool,
    /// Suffixes the AI suggestion hook appends to a slugified
    /// product name when proposing labels. The empty string means
    /// "the bare product name itself".
    #[serde(default = "default_product_subdomain_suggestion_suffixes")]
    pub product_subdomain_suggestion_suffixes: Vec<String>,
}

fn default_max_subdomains() -> u32 {
    5
}

fn default_reservation_ttl_hours() -> u32 {
    48
}

impl Default for SaasDomainsSettings {
    fn default() -> Self {
        Self {
            wildcard_base: String::new(),
            main_host: String::new(),
            reserved_labels: default_reserved_labels(),
            label_pattern: default_label_pattern(),
            max_subdomains_per_tenant: default_max_subdomains(),
            require_subdomain_approval: false,
            default_reservation_ttl_hours: default_reservation_ttl_hours(),
            alias_target_types: default_alias_target_types(),
            max_product_subdomains_per_tenant: default_max_product_subdomains_per_tenant(),
            max_product_subdomains_per_product: default_max_product_subdomains_per_product(),
            default_product_subdomain_grace_days: default_product_subdomain_grace_days(),
            require_product_subdomain_approval: false,
            product_subdomain_suggestion_suffixes: default_product_subdomain_suggestion_suffixes(),
        }
    }
}

impl SaasDomainsSettings {
    /// Validate a settings value before it is stored. The label
    /// pattern must compile, the wildcard base (when set) must be a
    /// valid hostname, and numeric limits must stay in sane bounds.
    pub(crate) fn validate(&self) -> UnifiedResult<()> {
        if !self.wildcard_base.is_empty() {
            validate_hostname(&self.wildcard_base)?;
        }
        if !self.main_host.is_empty() {
            validate_hostname(&self.main_host)?;
        }
        if Regex::new(&self.label_pattern).is_err() {
            return Err(UnifiedError::bad_request(
                "label_pattern is not a valid regular expression",
            ));
        }
        if self.max_subdomains_per_tenant == 0 || self.max_subdomains_per_tenant > 1000 {
            return Err(UnifiedError::bad_request(
                "max_subdomains_per_tenant must be between 1 and 1000",
            ));
        }
        if self.default_reservation_ttl_hours == 0 || self.default_reservation_ttl_hours > 24 * 365
        {
            return Err(UnifiedError::bad_request(
                "default_reservation_ttl_hours must be between 1 and 8760",
            ));
        }
        if self.alias_target_types.is_empty() {
            return Err(UnifiedError::bad_request(
                "alias_target_types must not be empty",
            ));
        }
        if self.max_product_subdomains_per_tenant == 0
            || self.max_product_subdomains_per_tenant > 1000
        {
            return Err(UnifiedError::bad_request(
                "max_product_subdomains_per_tenant must be between 1 and 1000",
            ));
        }
        if self.max_product_subdomains_per_product == 0
            || self.max_product_subdomains_per_product > 100
        {
            return Err(UnifiedError::bad_request(
                "max_product_subdomains_per_product must be between 1 and 100",
            ));
        }
        if self.default_product_subdomain_grace_days > 365 {
            return Err(UnifiedError::bad_request(
                "default_product_subdomain_grace_days must be between 0 and 365",
            ));
        }
        for suffix in &self.product_subdomain_suggestion_suffixes {
            let suffix = suffix.trim().to_lowercase();
            if suffix.len() > 63
                || (!suffix.is_empty()
                    && (!suffix
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                        || suffix.starts_with('-')
                        || suffix.ends_with('-')))
            {
                return Err(UnifiedError::bad_request(
                    "product_subdomain_suggestion_suffixes entries must be empty or valid label suffixes",
                ));
            }
        }
        for label in &self.reserved_labels {
            let label = label.trim().to_lowercase();
            if label.is_empty() || label.len() > 63 {
                return Err(UnifiedError::bad_request(
                    "reserved_labels entries must be non-empty DNS labels",
                ));
            }
        }
        Ok(())
    }

    /// Compiled label pattern, falling back to the factory default
    /// when the stored pattern ever fails to compile. The configured
    /// pattern is administrator-supplied text, so it is compiled per
    /// call; the fixed factory default is compiled once.
    pub(crate) fn label_regex(&self) -> Regex {
        Regex::new(&self.label_pattern).unwrap_or_else(|_| default_label_regex().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_validate() {
        assert!(SaasDomainsSettings::default().validate().is_ok());
    }

    #[test]
    fn settings_validate_rejects_bad_pattern() {
        let mut cfg = SaasDomainsSettings::default();
        cfg.label_pattern = "(".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn settings_validate_rejects_zero_limit() {
        let mut cfg = SaasDomainsSettings::default();
        cfg.max_subdomains_per_tenant = 0;
        assert!(cfg.validate().is_err());
        let mut cfg = SaasDomainsSettings::default();
        cfg.default_reservation_ttl_hours = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn label_regex_uses_configured_pattern() {
        let cfg = SaasDomainsSettings::default();
        let regex = cfg.label_regex();
        assert!(regex.is_match("my-label"));
        assert!(!regex.is_match("-bad"));
        assert!(!regex.is_match("has_underscore"));
    }

    #[test]
    fn label_regex_falls_back_to_default_when_configured_pattern_is_invalid() {
        // An invalid stored pattern must not panic: the fixed default
        // (compiled once) takes over, and repeated calls agree.
        let mut cfg = SaasDomainsSettings::default();
        cfg.label_pattern = "(".to_string();
        let first = cfg.label_regex();
        assert!(first.is_match("plain"));
        assert!(!first.is_match("has_underscore"));
        assert!(cfg.label_regex().is_match("plain"));
    }
}
