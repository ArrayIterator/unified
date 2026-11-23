//! Cloudflare connector module. — Constants.
//!
//! Split from `src/modules/cloudflare.rs`; behaviour unchanged.

use super::accounts_zones::*;
use super::cache_handlers::*;
use super::client::*;
use super::dns_handlers::*;
use super::dns_validation::*;
use super::helpers::*;
use super::module::*;
use super::requests::*;
use super::state::*;
use super::status_handlers::*;

/// The only API host this module ever calls.
pub(super) const CF_API_BASE: &str = "https://api.cloudflare.com/client/v4";
pub(super) const CF_HTTP_TIMEOUT_SECS: u64 = 30;
pub(super) const CF_USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"));
/// Longest `Retry-After` delay honoured from Cloudflare.
pub(super) const CF_MAX_RETRY_AFTER_SECS: u64 = 300;
/// Default backoff when a 429 arrives without a `Retry-After` header.
pub(super) const CF_DEFAULT_RETRY_AFTER_SECS: u64 = 60;
/// Maximum purge-history entries kept in memory.
pub(super) const CF_MAX_PURGE_HISTORY: usize = 200;
/// Maximum URLs or prefixes in one purge request.
pub(super) const CF_MAX_PURGE_TARGETS: usize = 30;

/// DNS record types this module manages.
pub(super) const CF_RECORD_TYPES: &[&str] = &["A", "AAAA", "CNAME", "TXT", "MX", "SRV", "CAA"];
/// Cache levels accepted by the Cloudflare zone settings API.
pub(super) const CF_CACHE_LEVELS: &[&str] = &["aggressive", "basic", "simplified"];

/// Permissions enforced by every handler in this module through the
/// `role.check` hook (same `*.view` / `*.manage` shape as the other
/// modules). Read-only routes require `cloudflare.view`; every route
/// that changes state requires `cloudflare.manage`.
pub(super) const PERM_CLOUDFLARE_VIEW: &str = "cloudflare.view";
pub(super) const PERM_CLOUDFLARE_MANAGE: &str = "cloudflare.manage";

/// Maximum characters of a non-JSON Cloudflare error body surfaced
/// in an error message (see `parse_cf_response`).
pub(super) const CF_MAX_ERROR_BODY_CHARS: usize = 500;
