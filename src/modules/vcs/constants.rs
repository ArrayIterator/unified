// Third-party VCS integrations — Event names, permission strings, limits, and other constants.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::header;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::types::*;
use super::providers::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_providers::*;
use super::handlers_connections::*;
use super::handlers_drafts::*;
use super::webhooks::*;
use super::module::*;

pub(crate) type HmacSha256 = Hmac<Sha256>;

/// Capability strings checked through the `role.check` hook.
pub(crate) const PERM_VCS_VIEW: &str = "vcs.view";

pub(crate) const PERM_VCS_MANAGE: &str = "vcs.manage";

/// User-Agent sent with provider requests.
pub(crate) const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"));

/// Per-request timeout for provider API calls and asset downloads.
pub(crate) const HTTP_TIMEOUT_SECS: u64 = 30;

/// Maximum release-asset download size (512 MiB).
pub(crate) const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;

/// Maximum redirects followed when downloading a release asset.
pub(crate) const MAX_ASSET_REDIRECTS: u32 = 3;

/// Longest `Retry-After` delay honoured from a provider.
pub(crate) const MAX_RETRY_AFTER_SECS: u64 = 3600;

/// Maximum length of a stored changelog / notes body.
pub(crate) const MAX_NOTES_LEN: usize = 20_000;

/// Maximum length of a tag, owner, or repository slug.
pub(crate) const MAX_SLUG_LEN: usize = 128;

/// Built-in hosts a release asset may be downloaded from. Configured
/// self-hosted GitLab hosts are added to this list at runtime.
pub(crate) const BUILTIN_ASSET_HOSTS: [&str; 6] = [
    "github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    "gitlab.com",
    "bitbucket.org",
    "api.bitbucket.org",
];
