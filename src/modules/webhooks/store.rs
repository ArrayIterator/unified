// Webhooks - State.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use rand::{distr::Alphanumeric, Rng};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

use super::*;

// State.

pub(crate) static HOOK_BUS: OnceLock<Arc<HookBus>> = OnceLock::new();
pub(crate) static ENDPOINTS: OnceLock<DashMap<Uuid, WebhookEndpoint>> = OnceLock::new();
pub(crate) static DELIVERIES: OnceLock<DashMap<Uuid, WebhookDelivery>> = OnceLock::new();

pub(crate) fn endpoints_store() -> &'static DashMap<Uuid, WebhookEndpoint> {
    ENDPOINTS.get_or_init(DashMap::new)
}

pub(crate) fn deliveries_store() -> &'static DashMap<Uuid, WebhookDelivery> {
    DELIVERIES.get_or_init(DashMap::new)
}

pub(crate) fn emit(event: &str, payload: Value) {
    if let Some(bus) = HOOK_BUS.get() {
        bus.emit(event, payload);
    }
}
