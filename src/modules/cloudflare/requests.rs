//! Cloudflare connector module. — Request types.
//!
//! Split from `src/modules/cloudflare.rs`; behaviour unchanged.

use super::accounts_zones::*;
use super::cache_handlers::*;
use super::client::*;
use super::constants::*;
use super::dns_handlers::*;
use super::dns_validation::*;
use super::helpers::*;
use super::module::*;
use super::state::*;
use super::status_handlers::*;

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use serde::Deserialize;
use serde_json::json;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub(super) struct UpdateSettingsInput {
    #[serde(default)]
    pub(super) api_token: Option<String>,
    #[serde(default)]
    pub(super) account_id: Option<String>,
    #[serde(default)]
    pub(super) default_zone_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct DnsRecordInput {
    #[serde(rename = "type")]
    pub(super) record_type: String,
    pub(super) name: String,
    pub(super) content: String,
    #[serde(default = "default_ttl")]
    pub(super) ttl: u32,
    #[serde(default)]
    pub(super) proxied: bool,
    #[serde(default)]
    pub(super) priority: Option<u16>,
    #[serde(default)]
    pub(super) comment: Option<String>,
}

pub(super) fn default_ttl() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateDnsRecordInput {
    #[serde(default, rename = "type")]
    pub(super) record_type: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) content: Option<String>,
    #[serde(default)]
    pub(super) ttl: Option<u32>,
    #[serde(default)]
    pub(super) proxied: Option<bool>,
    #[serde(default)]
    pub(super) priority: Option<u16>,
    #[serde(default)]
    pub(super) comment: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PurgeCacheInput {
    #[serde(default)]
    pub(super) urls: Option<Vec<String>>,
    #[serde(default)]
    pub(super) prefixes: Option<Vec<String>>,
    #[serde(default)]
    pub(super) purge_everything: bool,
    #[serde(default)]
    pub(super) confirm: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct DevelopmentModeInput {
    pub(super) enabled: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct CacheLevelInput {
    pub(super) value: String,
}

/// Build the JSON body for a DNS create/update call after validation.
/// For updates, `current` carries the record's existing values so
/// omitted fields keep their values.
pub(super) fn dns_record_body(
    input_type: &str,
    name: &str,
    content: &str,
    ttl: u32,
    proxied: bool,
    priority: Option<u16>,
    comment: &Option<String>,
) -> UnifiedResult<Value> {
    let record_type = input_type.trim().to_uppercase();
    if !CF_RECORD_TYPES.contains(&record_type.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "Unsupported record type \"{record_type}\" (expected one of {})",
            CF_RECORD_TYPES.join(", ")
        )));
    }
    let name = name.trim();
    if !valid_dns_name(name) {
        return Err(UnifiedError::bad_request(format!(
            "\"{name}\" is not a valid DNS record name"
        )));
    }
    let content = validate_record_content(&record_type, content, priority)?;
    let ttl = validate_ttl(ttl)?;
    if proxied && !matches!(record_type.as_str(), "A" | "AAAA" | "CNAME") {
        return Err(UnifiedError::bad_request(format!(
            "Only A, AAAA, and CNAME records can be proxied; {record_type} cannot"
        )));
    }
    if let Some(comment) = comment {
        if comment.chars().count() > 200 {
            return Err(UnifiedError::bad_request(
                "comment must be at most 200 characters",
            ));
        }
    }
    let mut body = json!({
        "type": record_type,
        "name": name,
        "content": content,
        "ttl": ttl,
        "proxied": proxied,
    });
    if matches!(record_type.as_str(), "MX" | "SRV") {
        if let Some(priority) = priority {
            body["priority"] = json!(priority);
        }
    }
    if let Some(comment) = comment {
        body["comment"] = json!(comment);
    }
    Ok(body)
}
