// Settings module.
//
// Namespaced key-value settings under `/api/v1/settings`, stored in
// memory with typed values (`string`, `number`, `bool`, `json`). Values
// are validated through the hook bus before they are stored: the module
// emits a `config.validate` event, and any handler may mark the value
// invalid (by setting `valid: false` on the payload or contributing a
// result with `valid: false`); the write is then refused with 422.
// After a successful write or delete the module emits a
// `settings.changed` event (`{ namespace, key, type?, value?,
// deleted }`) so owning modules can keep their effective-value
// mirrors in sync.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

pub type SettingsStore = Arc<DashMap<(String, String), SettingRecord>>;

pub struct SettingsModule {
    store: SettingsStore,
}

impl SettingsModule {
    pub fn new() -> Self {
        Self {
            store: Arc::new(DashMap::new()),
        }
    }
}

impl Default for SettingsModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SettingsModule::new()));
}

/// One stored setting: its typed value plus the last update time.
#[derive(Debug, Clone, Serialize)]
pub struct SettingRecord {
    pub namespace: String,
    pub key: String,
    #[serde(rename = "type")]
    pub value_type: String,
    pub value: Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PutSettingInput {
    pub value: Value,
    /// Explicit type: `string`, `number`, `bool`, or `json`. When omitted
    /// the type is inferred from the JSON value.
    #[serde(default, rename = "type")]
    pub value_type: Option<String>,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Infer the stored type from a JSON value.
fn infer_type(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "bool",
        _ => "json",
    }
}

/// Check that a value matches its declared type.
fn type_matches(value_type: &str, value: &Value) -> bool {
    match value_type {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "bool" => value.is_boolean(),
        "json" => true,
        _ => false,
    }
}

/// Merge settings contributed by another module into the settings
/// store (`settings.merge`, payload
/// `{ "namespace": "vcs.github", "settings": { "token_configured": true, ... } }`).
/// Only `vcs.*` namespaces are accepted, and only flag values are
/// stored: the VCS module never puts a credential in the merge
/// payload, so tokens and webhook secrets stay in the VCS module's
/// own write-only store and are never written here. Merged values are
/// trusted internal flags, so they are stored without going through
/// the `config.validate` hook (that hook needs an `AppContext`, which
/// a synchronous hook handler does not have).
fn on_settings_merge(store: SettingsStore, ctx: &mut HookContext) {
    let namespace = ctx
        .get("namespace")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    if !namespace.starts_with("vcs.") {
        ctx.add_result(json!({
            "merged": 0,
            "reason": "only vcs.* namespaces can be merged",
        }));
        return;
    }
    let Some(settings) = ctx.get("settings").and_then(|value| value.as_object()) else {
        ctx.add_result(json!({
            "merged": 0,
            "reason": "settings must be an object",
        }));
        return;
    };
    let mut merged = 0usize;
    for (key, value) in settings {
        if !valid_name(key) {
            continue;
        }
        let record = SettingRecord {
            namespace: namespace.clone(),
            key: key.clone(),
            value_type: infer_type(value).to_string(),
            value: value.clone(),
            updated_at: Utc::now(),
        };
        store.insert((namespace.clone(), key.clone()), record);
        merged += 1;
    }
    ctx.add_result(json!({
        "merged": merged,
        "namespace": namespace,
    }));
}

/// Ask the hook bus whether a setting value is acceptable. Returns an
/// error describing the refusal when a handler marked it invalid.
fn validate_via_hooks(
    ctx: &AppContext,
    namespace: &str,
    key: &str,
    value_type: &str,
    value: &Value,
) -> UnifiedResult<()> {
    let hook = ctx.hooks.emit(
        events::CONFIG_VALIDATE,
        json!({
            "namespace": namespace,
            "key": key,
            "type": value_type,
            "value": value,
            "valid": true,
        }),
    );
    let mut message: Option<String> = None;
    if hook.get("valid").and_then(|value| value.as_bool()) == Some(false) {
        message = hook
            .get("message")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string());
    }
    if message.is_none() {
        for result in &hook.results {
            if result.get("valid").and_then(|value| value.as_bool()) == Some(false) {
                message = result
                    .get("message")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string())
                    .or_else(|| Some("Setting value rejected by validation".to_string()));
                break;
            }
        }
    }
    match message {
        Some(message) => Err(UnifiedError::unprocessable(message)),
        None => Ok(()),
    }
}

fn list_settings(
    store: web::Data<SettingsStore>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let sort = query.allowed_sort(&["namespace", "key", "updated_at"], "namespace");
    let descending = pagination.order == "desc";
    let namespace_filter = pagination.filters.get("namespace").cloned();
    let search = pagination.search.clone().map(|s| s.to_lowercase());

    let mut records: Vec<SettingRecord> = store
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            if let Some(namespace) = &namespace_filter {
                if record.namespace != *namespace {
                    return false;
                }
            }
            if let Some(search) = &search {
                let haystack = format!("{} {} {}", record.namespace, record.key, record.value)
                    .to_lowercase();
                if !haystack.contains(search.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();
    records.sort_by(|a, b| match sort.as_str() {
        "key" => a.key.cmp(&b.key),
        "updated_at" => a.updated_at.cmp(&b.updated_at),
        _ => a
            .namespace
            .cmp(&b.namespace)
            .then_with(|| a.key.cmp(&b.key)),
    });
    if descending {
        records.reverse();
    }
    let total = records.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(records.len());
    let items = if start < records.len() {
        records[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
}

fn get_setting(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<SettingsStore>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (namespace, key) = path.into_inner();
    match store.get(&(namespace.clone(), key.clone())) {
        Some(record) => success_response(record.value().clone(), 200),
        None => error_response(
            &UnifiedError::not_found(format!("Setting \"{namespace}/{key}\" not found")),
            debug,
        ),
    }
}

fn put_setting(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<SettingsStore>,
    path: web::Path<(String, String)>,
    body: web::Json<PutSettingInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (namespace, key) = path.into_inner();
    let input = body.into_inner();
    let result: UnifiedResult<SettingRecord> = (|| {
        if !valid_name(&namespace) || !valid_name(&key) {
            return Err(UnifiedError::bad_request(
                "Namespace and key must contain only letters, digits, '-', '_' or '.'",
            ));
        }
        let value_type = match &input.value_type {
            Some(value_type) => {
                if !matches!(value_type.as_str(), "string" | "number" | "bool" | "json") {
                    return Err(UnifiedError::bad_request(
                        "Setting type must be \"string\", \"number\", \"bool\" or \"json\"",
                    ));
                }
                if !type_matches(value_type, &input.value) {
                    return Err(UnifiedError::bad_request(format!(
                        "Value does not match the declared type \"{value_type}\""
                    )));
                }
                value_type.clone()
            }
            None => infer_type(&input.value).to_string(),
        };
        validate_via_hooks(&ctx, &namespace, &key, &value_type, &input.value)?;
        let record = SettingRecord {
            namespace: namespace.clone(),
            key: key.clone(),
            value_type,
            value: input.value,
            updated_at: Utc::now(),
        };
        store.insert((namespace, key), record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => {
            // Batch 18: tell owning modules the value changed so
            // their effective-value mirrors stay in sync (the
            // payments module listens for `cards.*` /
            // `reference_id.*` here). Emitted only after the write
            // lands; a refused write emits nothing.
            ctx.hooks.emit(
                "settings.changed",
                json!({
                    "namespace": record.namespace.clone(),
                    "key": record.key.clone(),
                    "type": record.value_type.clone(),
                    "value": record.value.clone(),
                    "deleted": false,
                }),
            );
            success_response(record, 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

fn delete_setting(
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<SettingsStore>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (namespace, key) = path.into_inner();
    match store.remove(&(namespace.clone(), key.clone())) {
        Some(_) => {
            // Batch 18: see `put_setting` — a delete reverts the
            // key to its compiled-in default in listening modules.
            ctx.hooks.emit(
                "settings.changed",
                json!({ "namespace": namespace.clone(), "key": key.clone(), "deleted": true }),
            );
            success_response(
                json!({ "deleted": true, "namespace": namespace, "key": key }),
                200,
            )
        }
        None => error_response(
            &UnifiedError::not_found(format!("Setting \"{namespace}/{key}\" not found")),
            debug,
        ),
    }
}

impl Module for SettingsModule {
    fn id(&self) -> &'static str {
        "settings"
    }

    fn name(&self) -> &'static str {
        "Settings"
    }

    fn description(&self) -> &'static str {
        "Namespaced key-value settings with typed values, validated through the config.validate hook."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let store = self.store.clone();
        bus.register(
            "settings.merge",
            "settings",
            Arc::new(move |ctx: &mut HookContext| {
                on_settings_merge(store.clone(), ctx)
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/settings", owner: "settings", description: "List settings" },
            RouteInfo { method: "GET", path: "/api/v1/settings/{namespace}/{key}", owner: "settings", description: "Get setting" },
            RouteInfo { method: "PUT", path: "/api/v1/settings/{namespace}/{key}", owner: "settings", description: "Put setting" },
            RouteInfo { method: "DELETE", path: "/api/v1/settings/{namespace}/{key}", owner: "settings", description: "Delete setting" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/settings")
                .app_data(web::Data::new(self.store.clone()))
                .route("", web::get().to(list_settings))
                .route("/{namespace}/{key}", web::get().to(get_setting))
                .route("/{namespace}/{key}", web::put().to(put_setting))
                .route("/{namespace}/{key}", web::delete().to(delete_setting)),
        );
    }
}
