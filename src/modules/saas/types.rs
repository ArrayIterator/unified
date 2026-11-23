// saas module -- Shared types, records, request bodies, and constants.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub const EVENT_COMPANY_EXISTS: &str = "company.exists";

pub const EVENT_DNS_VERIFY: &str = "dns.verify";

pub const EVENT_SITE_PROVISIONED: &str = "site.provisioned";

pub const EVENT_THEME_ACTIVATE_REQUESTED: &str = "theme.activate_requested";

pub const EVENT_SITE_SUSPENDED: &str = "site.suspended";

pub const EVENT_SITE_ACTIVATED: &str = "site.activated";

pub const EVENT_SITE_DELETED: &str = "site.deleted";

/// Permissions guarding the SaaS routes. Reads (list, get, settings
/// read) require `saas.view`; every write (create, update, delete,
/// suspend, activate, settings write) requires `saas.manage`.
pub(crate) const PERM_SAAS_VIEW: &str = "saas.view";
pub(crate) const PERM_SAAS_MANAGE: &str = "saas.manage";

#[derive(Debug, Clone, Serialize)]
pub struct Site {
    pub id: String,
    pub company_id: String,
    pub name: String,
    pub domain: String,
    pub theme_id: String,
    /// `active`, `suspended`, or `provisioning`.
    pub status: String,
    /// `verified`, `unverified`, or `unchecked`.
    pub domain_status: String,
    /// `pending`, `active`, `failed`, or `none`.
    pub ssl_status: String,
    /// Per-site settings namespace.
    pub settings: HashMap<String, Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SiteInput {
    pub(crate) company_id: String,
    pub(crate) name: String,
    pub(crate) domain: String,
    pub(crate) theme_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SiteUpdateInput {
    pub(crate) company_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) domain: Option<String>,
    pub(crate) theme_id: Option<String>,
}
