//! tenants - settings.

use unified_core::error::{UnifiedError, UnifiedResult};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use super::{constants::*, vocabulary::*, events::*, records::*, state::*, auth::*, helpers::*, provisioning::*, destroy::*, handlers::*, module::*, tenancy::*};



// Settings (configurable defaults, stored as settings - nothing
// behavioural is hard-coded outside this struct's defaults).
/// Default reserved subdomain labels. These are CONFIGURABLE
/// DEFAULTS, not a fixed rule: an administrator edits them through
/// `PUT /api/v1/tenants/settings`, and provisioning reads the live
/// settings every time. The defaults cover the labels that would
/// collide with platform, mail, and infrastructure hosts.
pub(crate) fn default_reserved_slugs() -> Vec<String> {
    [
        "admin", "api", "app", "www", "mail", "support", "help", "status", "blog", "shop",
        "store", "dashboard", "login", "auth", "system", "root", "null", "undefined", "saas",
        "tenant", "tenants", "control", "cpanel", "webmail", "ftp", "smtp", "ns1", "ns2",
        "static", "cdn", "assets", "media", "docs", "dev", "staging", "test", "demo", "example",
        "localhost",
    ]
    .iter()
    .map(|value| value.to_string())
    .collect()
}


pub(crate) fn default_slug_min_length() -> u32 {
    3
}


pub(crate) fn default_slug_max_length() -> u32 {
    63
}


/// Default suspension grace in days: the window a suspended tenant
/// has before the destroy flow may consider it for removal. This
/// is a default for the destroy flow (B3) to read; nothing in this
/// module deletes a tenant when the grace ends.
pub(crate) fn default_suspension_grace_days() -> u32 {
    7
}


/// Default maximum tenants one owner may hold. Zero means
/// unlimited. This is a configurable default, not a fixed quota.
pub(crate) fn default_max_tenants_per_owner() -> u32 {
    0
}


/// Admin settings for tenant defaults. Every rule the provisioning
/// flow enforces about slugs, grace, and quotas lives here, so an
/// administrator can change behaviour without a code change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantsSettings {
    /// Subdomain labels no tenant may claim (lowercase, compared
    /// case-insensitively). Configurable default; see
    /// `default_reserved_slugs`.
    #[serde(default = "default_reserved_slugs")]
    pub reserved_slugs: Vec<String>,
    /// Minimum slug length in characters. Configurable default: 3.
    #[serde(default = "default_slug_min_length")]
    pub slug_min_length: u32,
    /// Maximum slug length in characters. Configurable default: 63
    /// (the DNS label limit).
    #[serde(default = "default_slug_max_length")]
    pub slug_max_length: u32,
    /// Suspension grace in days. Configurable default: 7.
    #[serde(default = "default_suspension_grace_days")]
    pub suspension_grace_days: u32,
    /// Maximum tenants per owner (0 = unlimited). Configurable
    /// default: 0.
    #[serde(default = "default_max_tenants_per_owner")]
    pub max_tenants_per_owner: u32,
    /// Module ids granted to a tenant when the create request names
    /// neither explicit module ids nor a package. Empty by
    /// default: a tenant with no package and no explicit list gets
    /// no modules. Each id is still entitlement-checked.
    #[serde(default)]
    pub default_module_ids: Vec<String>,
}


impl Default for TenantsSettings {
    fn default() -> Self {
        Self {
            reserved_slugs: default_reserved_slugs(),
            slug_min_length: default_slug_min_length(),
            slug_max_length: default_slug_max_length(),
            suspension_grace_days: default_suspension_grace_days(),
            max_tenants_per_owner: default_max_tenants_per_owner(),
            default_module_ids: Vec::new(),
        }
    }
}


impl TenantsSettings {
    /// Validate a settings value before it is stored. Slugs are
    /// normalized to lowercase and de-duplicated; lengths are
    /// clamped to the DNS label range.
    pub fn validated(mut self) -> UnifiedResult<Self> {
        if self.slug_min_length < 1 {
            return Err(UnifiedError::bad_request(
                "slug_min_length must be at least 1",
            ));
        }
        if self.slug_max_length > 63 {
            return Err(UnifiedError::bad_request(
                "slug_max_length must be at most 63",
            ));
        }
        if self.slug_min_length > self.slug_max_length {
            return Err(UnifiedError::bad_request(
                "slug_min_length must not exceed slug_max_length",
            ));
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut reserved: Vec<String> = Vec::new();
        for slug in self.reserved_slugs.drain(..) {
            let normalized = slug.trim().to_ascii_lowercase();
            if normalized.is_empty() || !seen.insert(normalized.clone()) {
                continue;
            }
            reserved.push(normalized);
        }
        self.reserved_slugs = reserved;
        let mut modules: Vec<String> = Vec::new();
        for module_id in self.default_module_ids.drain(..) {
            let trimmed = module_id.trim().to_string();
            if !trimmed.is_empty() && !modules.contains(&trimmed) {
                modules.push(trimmed);
            }
        }
        self.default_module_ids = modules;
        Ok(self)
    }
}
