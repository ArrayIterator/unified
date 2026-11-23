// Third-party VCS integrations — In-memory state and its constructors.

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

use super::constants::*;
use super::types::*;
use super::providers::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_providers::*;
use super::handlers_connections::*;
use super::handlers_drafts::*;
use super::webhooks::*;
use super::module::*;

/// Provider rate-limit state, kept per provider (and per GitLab host).
#[derive(Debug, Clone)]
pub struct RateLimitState {
    pub remaining: Option<u32>,
    pub reset_at: Option<DateTime<Utc>>,
    pub retry_after_until: Option<Instant>,
}

// ---------------------------------------------------------------------------
// Module state.
// ---------------------------------------------------------------------------

pub struct VcsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub connections: DashMap<Uuid, RepositoryConnection>,
    pub drafts: DashMap<Uuid, VersionDraft>,
    pub sync_history: DashMap<Uuid, SyncHistoryEntry>,
    pub provider_settings: DashMap<String, ProviderSettings>,
    pub rate_limits: DashMap<String, RateLimitState>,
}

impl VcsState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            connections: DashMap::new(),
            drafts: DashMap::new(),
            sync_history: DashMap::new(),
            provider_settings: DashMap::new(),
            rate_limits: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    pub(crate) fn provider_settings(&self, provider: Provider) -> ProviderSettings {
        self.provider_settings
            .get(provider.as_str())
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Extra allowed GitLab hosts for one provider (settings key
    /// `vcs.gitlab.hosts`).
    pub(crate) fn gitlab_hosts(&self, provider: Provider) -> Vec<String> {
        if provider == Provider::GitLab {
            self.provider_settings(provider).gitlab_hosts
        } else {
            Vec::new()
        }
    }
}
