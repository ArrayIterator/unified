// App Sync module — application connection and sync for desktop apps.
//
// A desktop app registers a device and receives an HMAC-signed sync
// token. Every sync call validates the token against the server secret
// (`app.secret_key`) before touching any data. Sync state is kept in
// memory per device: each push appends a versioned record (a monotonic
// per-device version plus an `updated_at` timestamp), and pull returns
// every record newer than the client's cursor — either a version number
// or an RFC 3339 timestamp. The first successful sync emits the
// `sync.connected` hook event.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::events;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Device and sync-record state.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DeviceView {
    pub id: Uuid,
    pub device_name: String,
    pub platform: String,
    pub created_at: DateTime<Utc>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub revoked: bool,
    pub version: u64,
}

pub struct DeviceState {
    pub id: Uuid,
    pub device_name: String,
    pub platform: String,
    pub created_at: DateTime<Utc>,
    pub last_sync_at: RwLock<Option<DateTime<Utc>>>,
    pub revoked: AtomicBool,
    pub synced_once: AtomicBool,
    pub version: AtomicU64,
}

impl DeviceState {
    fn snapshot(&self) -> DeviceView {
        DeviceView {
            id: self.id,
            device_name: self.device_name.clone(),
            platform: self.platform.clone(),
            created_at: self.created_at,
            last_sync_at: *self.last_sync_at.read().expect("device lock poisoned"),
            revoked: self.revoked.load(Ordering::SeqCst),
            version: self.version.load(Ordering::SeqCst),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncRecord {
    pub id: Uuid,
    pub device_id: Uuid,
    pub version: u64,
    pub updated_at: DateTime<Utc>,
    pub payload: Value,
}

static DEVICES: OnceLock<DashMap<Uuid, Arc<DeviceState>>> = OnceLock::new();
static SYNC_RECORDS: OnceLock<DashMap<Uuid, Vec<SyncRecord>>> = OnceLock::new();

fn devices_store() -> &'static DashMap<Uuid, Arc<DeviceState>> {
    DEVICES.get_or_init(DashMap::new)
}

fn sync_store() -> &'static DashMap<Uuid, Vec<SyncRecord>> {
    SYNC_RECORDS.get_or_init(DashMap::new)
}

/// Maximum sync records retained per device (oldest are dropped).
const MAX_RECORDS_PER_DEVICE: usize = 1000;

// ---------------------------------------------------------------------------
// HMAC-signed device tokens.
//
// Token format: `<device-id>.<base64url(HMAC-SHA256(secret, device-id))>`.
// The secret is `app.secret_key`; when it is empty (development setups
// without a configured secret) a fixed development fallback is used and
// the token is still validated the same way.
// ---------------------------------------------------------------------------

fn signing_secret(ctx: &AppContext) -> String {
    let config = ctx.config.read().expect("config lock poisoned");
    if config.app.secret_key.trim().is_empty() {
        "unified-app-sync-development-secret".to_string()
    } else {
        config.app.secret_key.clone()
    }
}

fn sign_device_token(secret: &str, device_id: Uuid) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC key must accept any length");
    mac.update(device_id.as_bytes());
    let signature = mac.finalize().into_bytes();
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature);
    format!("{device_id}.{encoded}")
}

fn validate_device_token(ctx: &AppContext, token: &str) -> UnifiedResult<Arc<DeviceState>> {
    let (device_id_text, _signature) = token
        .split_once('.')
        .ok_or_else(|| UnifiedError::unauthorized("Invalid sync token format"))?;
    let device_id: Uuid = device_id_text
        .parse()
        .map_err(|_| UnifiedError::unauthorized("Invalid sync token format"))?;
    let expected = sign_device_token(&signing_secret(ctx), device_id);
    if token != expected {
        return Err(UnifiedError::unauthorized("Invalid sync token"));
    }
    let device = devices_store()
        .get(&device_id)
        .map(|entry| Arc::clone(entry.value()))
        .ok_or_else(|| UnifiedError::unauthorized("Unknown device"))?;
    if device.revoked.load(Ordering::SeqCst) {
        return Err(UnifiedError::unauthorized("Device token has been revoked"));
    }
    Ok(device)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RegisterDeviceRequest {
    device_name: String,
    #[serde(default = "default_platform")]
    platform: String,
}

fn default_platform() -> String {
    "unknown".to_string()
}

#[derive(Debug, Deserialize)]
struct SyncPushRequest {
    device_token: String,
    payload: Value,
}

#[derive(Debug, Deserialize)]
struct SyncPullRequest {
    device_token: String,
    /// Cursor: a version number, an RFC 3339 timestamp, or omitted for
    /// a full pull.
    #[serde(default)]
    since: Option<Value>,
}

/// Split a pull cursor into a version floor and a timestamp floor.
fn parse_cursor(since: &Option<Value>) -> (u64, Option<DateTime<Utc>>) {
    match since {
        Some(Value::Number(number)) => (
            number.as_u64().unwrap_or(0),
            None,
        ),
        Some(Value::String(text)) => {
            if let Ok(version) = text.parse::<u64>() {
                (version, None)
            } else if let Ok(time) = text.parse::<DateTime<Utc>>() {
                (0, Some(time))
            } else {
                (0, None)
            }
        }
        _ => (0, None),
    }
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn register_device(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<RegisterDeviceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        if body.device_name.trim().is_empty() {
            return Err(UnifiedError::bad_request("device_name is required"));
        }
        let platform = body.platform.trim();
        if platform.is_empty() {
            return Err(UnifiedError::bad_request("platform must not be empty"));
        }
        let device = Arc::new(DeviceState {
            id: Uuid::now_v7(),
            device_name: body.device_name.trim().to_string(),
            platform: platform.to_string(),
            created_at: Utc::now(),
            last_sync_at: RwLock::new(None),
            revoked: AtomicBool::new(false),
            synced_once: AtomicBool::new(false),
            version: AtomicU64::new(0),
        });
        let token = sign_device_token(&signing_secret(&ctx), device.id);
        let view = device.snapshot();
        devices_store().insert(device.id, Arc::clone(&device));
        sync_store().insert(device.id, Vec::new());
        Ok(serde_json::json!({
            "device": view,
            "device_token": token,
        }))
    })();
    match result {
        Ok(value) => json::success_response(value, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_devices(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<DeviceView> = devices_store()
            .iter()
            .map(|entry| entry.value().snapshot())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|device| device.device_name.to_lowercase().contains(&needle));
        }
        if let Some(platform) = params.filters.get("platform") {
            items.retain(|device| device.platform == *platform);
        }
        if let Some(revoked) = params.filters.get("revoked") {
            let wanted = revoked == "true" || revoked == "1";
            items.retain(|device| device.revoked == wanted);
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_device(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeviceView> = (|| {
        devices_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().snapshot())
            .ok_or_else(|| UnifiedError::not_found("Device not found"))
    })();
    json::respond(result, debug)
}

async fn revoke_device(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeviceView> = (|| {
        let device = devices_store()
            .get(&id.into_inner())
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Device not found"))?;
        device.revoked.store(true, Ordering::SeqCst);
        Ok(device.snapshot())
    })();
    json::respond(result, debug)
}

fn mark_synced(ctx: &AppContext, device: &DeviceState) {
    *device.last_sync_at.write().expect("device lock poisoned") = Some(Utc::now());
    if !device.synced_once.swap(true, Ordering::SeqCst) {
        ctx.hooks.emit(
            events::SYNC_CONNECTED,
            serde_json::json!({
                "device_id": device.id,
                "device_name": device.device_name,
                "platform": device.platform,
                "first_sync": true,
            }),
        );
    }
}

async fn sync_push(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<SyncPushRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SyncRecord> = (|| {
        if body.payload.is_null() {
            return Err(UnifiedError::bad_request("payload is required"));
        }
        let device = validate_device_token(&ctx, &body.device_token)?;
        let version = device.version.fetch_add(1, Ordering::SeqCst) + 1;
        let record = SyncRecord {
            id: Uuid::now_v7(),
            device_id: device.id,
            version,
            updated_at: Utc::now(),
            payload: body.payload.clone(),
        };
        sync_store()
            .entry(device.id)
            .or_insert_with(Vec::new)
            .push(record.clone());
        // Retain only the newest records.
        if let Some(mut records) = sync_store().get_mut(&device.id) {
            if records.len() > MAX_RECORDS_PER_DEVICE {
                let excess = records.len() - MAX_RECORDS_PER_DEVICE;
                records.drain(..excess);
            }
        }
        mark_synced(&ctx, &device);
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn sync_pull(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<SyncPullRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let device = validate_device_token(&ctx, &body.device_token)?;
        let (since_version, since_time) = parse_cursor(&body.since);
        let records: Vec<SyncRecord> = sync_store()
            .get(&device.id)
            .map(|entry| {
                entry
                    .value()
                    .iter()
                    .filter(|record| {
                        record.version > since_version
                            || since_time.map_or(false, |time| record.updated_at > time)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let current_version = device.version.load(Ordering::SeqCst);
        mark_synced(&ctx, &device);
        Ok(serde_json::json!({
            "device_id": device.id,
            "current_version": current_version,
            "records": records,
            "count": records.len(),
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct AppSyncModule;

impl Module for AppSyncModule {
    fn id(&self) -> &'static str {
        "app-sync"
    }

    fn name(&self) -> &'static str {
        "Application Connection & Sync"
    }

    fn description(&self) -> &'static str {
        "Desktop-app integration: HMAC-signed device tokens and versioned push/pull sync per device."
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/app-sync")
                .route("/devices/register", web::post().to(register_device))
                .route("/devices", web::get().to(list_devices))
                .route("/devices/{id}", web::get().to(get_device))
                .route("/devices/{id}/revoke", web::post().to(revoke_device))
                .route("/sync/push", web::post().to(sync_push))
                .route("/sync/pull", web::post().to(sync_pull)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AppSyncModule));
}
