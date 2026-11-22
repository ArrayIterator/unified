// API Manager module.
//
// The public / client API surface, separate from the core API: a registry
// of client-facing endpoints with payload validation rules, a version
// overview, a simple per-endpoint per-minute rate limiter, and API key
// management. Keys are generated with a `unf_` prefix and only the
// HMAC-SHA256 hash of a key is stored; the full key is shown exactly once,
// at creation time.
//
// Note on execution: `POST /endpoints/{id}/execute` validates the payload
// against the endpoint rules and returns the validation result plus an
// echo. Dispatching the request into a registered core route handler is
// out of scope for this milestone.

use crate::core::context::AppContext;
use crate::core::error::UnifiedError;
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::sync::Arc;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

const KEY_PREFIX: &str = "unf_";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointRule {
    pub field: String,
    pub required: bool,
    #[serde(rename = "type")]
    pub field_type: String,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiEndpoint {
    pub id: Uuid,
    pub method: String,
    pub path: String,
    pub version: String,
    pub description: String,
    pub rate_limit: u32,
    pub enabled: bool,
    pub rules: Vec<EndpointRule>,
    pub execution_count: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateEndpointInput {
    pub method: String,
    pub path: String,
    pub version: String,
    pub description: Option<String>,
    pub rate_limit: Option<u32>,
    pub enabled: Option<bool>,
    pub rules: Option<Vec<EndpointRule>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEndpointInput {
    pub method: Option<String>,
    pub path: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub rate_limit: Option<u32>,
    pub enabled: Option<bool>,
    pub rules: Option<Vec<EndpointRule>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiKey {
    pub id: Uuid,
    pub name: String,
    pub prefix: String,
    #[serde(skip_serializing)]
    pub key_hash: String,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateKeyInput {
    pub name: String,
    pub scopes: Option<Vec<String>>,
    pub expires_in_days: Option<u32>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ApiManagerState {
    pub endpoints: Arc<DashMap<Uuid, ApiEndpoint>>,
    pub keys: Arc<DashMap<Uuid, ApiKey>>,
    /// Per-endpoint per-minute execution counters: key is
    /// `<endpoint-id>:<unix-minute>`, value is the request count.
    pub rate_counters: Arc<DashMap<String, u64>>,
    /// Secret used to HMAC-hash stored API keys. Random per process.
    pub secret: Arc<[u8; 32]>,
}

impl Default for ApiManagerState {
    fn default() -> Self {
        Self {
            endpoints: Arc::new(DashMap::new()),
            keys: Arc::new(DashMap::new()),
            rate_counters: Arc::new(DashMap::new()),
            secret: Arc::new(rand::random::<[u8; 32]>()),
        }
    }
}

impl ApiManagerState {
    fn hash_key(&self, key: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.secret[..])
            .expect("HMAC accepts any key length");
        mac.update(key.as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];
const RULE_TYPES: &[&str] = &["string", "integer", "number", "boolean", "array", "object"];

fn validate_method(method: &str) -> Result<String, UnifiedError> {
    let method = method.trim().to_uppercase();
    if !HTTP_METHODS.contains(&method.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "method must be one of {}",
            HTTP_METHODS.join(", ")
        )));
    }
    Ok(method)
}

fn validate_path(path: &str) -> Result<String, UnifiedError> {
    let path = path.trim();
    if !path.starts_with('/') {
        return Err(UnifiedError::bad_request("path must start with '/'"));
    }
    if path.contains(char::is_whitespace) {
        return Err(UnifiedError::bad_request("path must not contain whitespace"));
    }
    if path.chars().count() > 500 {
        return Err(UnifiedError::bad_request(
            "path must be at most 500 characters",
        ));
    }
    Ok(path.to_string())
}

fn validate_version(version: &str) -> Result<String, UnifiedError> {
    let version = version.trim();
    // Accept `v1`, `v1.2`, or a plain semver string such as `1.2.3`.
    let version_pattern =
        Regex::new(r"^v\d+(\.\d+)*$|^\d+\.\d+\.\d+$").expect("static regex is valid");
    if !version_pattern.is_match(version) {
        return Err(UnifiedError::bad_request(
            "version must look like \"v1\", \"v1.2\", or a semver string such as \"1.2.3\"",
        ));
    }
    Ok(version.to_string())
}

fn validate_rules(rules: &[EndpointRule]) -> Result<(), UnifiedError> {
    let field_pattern =
        Regex::new(r"^[a-zA-Z0-9_]+(\.[a-zA-Z0-9_]+)*$").expect("static regex is valid");
    for rule in rules {
        if !field_pattern.is_match(&rule.field) {
            return Err(UnifiedError::bad_request(format!(
                "rule field \"{}\" is not a valid dotted field name",
                rule.field
            )));
        }
        if !RULE_TYPES.contains(&rule.field_type.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "rule field \"{}\": type must be one of {}",
                rule.field,
                RULE_TYPES.join(", ")
            )));
        }
        if let (Some(min), Some(max)) = (rule.min_length, rule.max_length) {
            if min > max {
                return Err(UnifiedError::bad_request(format!(
                    "rule field \"{}\": min_length must not exceed max_length",
                    rule.field
                )));
            }
        }
        if let (Some(min), Some(max)) = (rule.min_value, rule.max_value) {
            if min > max {
                return Err(UnifiedError::bad_request(format!(
                    "rule field \"{}\": min_value must not exceed max_value",
                    rule.field
                )));
            }
        }
        if let Some(pattern) = &rule.pattern {
            Regex::new(pattern).map_err(|_| {
                UnifiedError::bad_request(format!(
                    "rule field \"{}\": pattern is not a valid regular expression",
                    rule.field
                ))
            })?;
        }
    }
    Ok(())
}

/// Look a dotted field up in a JSON object payload.
fn lookup_field<'a>(payload: &'a Value, field: &str) -> Option<&'a Value> {
    let mut current = payload;
    for segment in field.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn type_matches(value: &Value, field_type: &str) -> bool {
    match field_type {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

/// Validate a payload against the endpoint rules. Returns the list of
/// problems; an empty list means the payload is valid.
fn validate_payload(payload: &Value, rules: &[EndpointRule]) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    if !payload.is_object() {
        errors.push("payload must be a JSON object".to_string());
        return errors;
    }
    for rule in rules {
        let value = lookup_field(payload, &rule.field);
        let Some(value) = value else {
            if rule.required {
                errors.push(format!("field \"{}\" is required", rule.field));
            }
            continue;
        };
        if value.is_null() {
            if rule.required {
                errors.push(format!("field \"{}\" must not be null", rule.field));
            }
            continue;
        }
        if !type_matches(value, &rule.field_type) {
            errors.push(format!(
                "field \"{}\" must be of type {}",
                rule.field, rule.field_type
            ));
            continue;
        }
        if let Some(text) = value.as_str() {
            let len = text.chars().count();
            if let Some(min) = rule.min_length {
                if len < min {
                    errors.push(format!(
                        "field \"{}\" must be at least {min} characters long",
                        rule.field
                    ));
                }
            }
            if let Some(max) = rule.max_length {
                if len > max {
                    errors.push(format!(
                        "field \"{}\" must be at most {max} characters long",
                        rule.field
                    ));
                }
            }
            if let Some(pattern) = &rule.pattern {
                if let Ok(regex) = Regex::new(pattern) {
                    if !regex.is_match(text) {
                        errors.push(format!(
                            "field \"{}\" does not match the required pattern",
                            rule.field
                        ));
                    }
                }
            }
        }
        if let Some(number) = value.as_f64() {
            if let Some(min) = rule.min_value {
                if number < min {
                    errors.push(format!("field \"{}\" must be at least {min}", rule.field));
                }
            }
            if let Some(max) = rule.max_value {
                if number > max {
                    errors.push(format!("field \"{}\" must be at most {max}", rule.field));
                }
            }
        }
    }
    errors
}

// ---------------------------------------------------------------------------
// Endpoint handlers
// ---------------------------------------------------------------------------

async fn list_endpoints(
    state: web::Data<ApiManagerState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let search = params.search.clone();
    let version_filter = params.filters.get("version").cloned();
    let method_filter = params.filters.get("method").cloned();
    let enabled_filter = params.filters.get("enabled").cloned();
    let mut items: Vec<ApiEndpoint> = state
        .endpoints
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|endpoint| {
            if let Some(version) = &version_filter {
                if endpoint.version != *version {
                    return false;
                }
            }
            if let Some(method) = &method_filter {
                if endpoint.method != method.to_uppercase() {
                    return false;
                }
            }
            if let Some(enabled) = &enabled_filter {
                let wanted = enabled == "true";
                if endpoint.enabled != wanted {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return endpoint.path.to_lowercase().contains(&needle)
                    || endpoint.description.to_lowercase().contains(&needle)
                    || endpoint.version.to_lowercase().contains(&needle);
            }
            true
        })
        .collect();
    let sort = params.allowed_sort(&["created_at", "path", "version"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "path" => a.path.cmp(&b.path),
            "version" => a.version.cmp(&b.version),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<ApiEndpoint> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn create_endpoint(
    state: web::Data<ApiManagerState>,
    input: web::Json<CreateEndpointInput>,
) -> HttpResponse {
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiEndpoint, UnifiedError> {
        let method = validate_method(&input.method)?;
        let path = validate_path(&input.path)?;
        let version = validate_version(&input.version)?;
        let rules = input.rules.unwrap_or_default();
        validate_rules(&rules)?;
        let rate_limit = input.rate_limit.unwrap_or(60);
        if rate_limit == 0 || rate_limit > 100_000 {
            return Err(UnifiedError::bad_request(
                "rate_limit must be between 1 and 100000 requests per minute",
            ));
        }
        if state
            .endpoints
            .iter()
            .any(|entry| {
                entry.value().method == method
                    && entry.value().path == path
                    && entry.value().version == version
            })
        {
            return Err(UnifiedError::conflict(format!(
                "an endpoint for {method} {path} already exists in version {version}"
            )));
        }
        let now = Utc::now();
        let endpoint = ApiEndpoint {
            id: Uuid::now_v7(),
            method,
            path,
            version,
            description: input.description.unwrap_or_default(),
            rate_limit,
            enabled: input.enabled.unwrap_or(true),
            rules,
            execution_count: 0,
            created_at: now,
            updated_at: now,
        };
        state.endpoints.insert(endpoint.id, endpoint.clone());
        Ok(endpoint)
    })();
    match outcome {
        Ok(endpoint) => api_json::success_response(endpoint, 201),
        Err(error) => api_json::error_response(&error, false),
    }
}

async fn get_endpoint(state: web::Data<ApiManagerState>, id: web::Path<Uuid>) -> HttpResponse {
    match state.endpoints.get(&id.into_inner()) {
        Some(endpoint) => api_json::success_response(endpoint.value().clone(), 200),
        None => api_json::error_response(&UnifiedError::not_found("endpoint not found"), false),
    }
}

async fn update_endpoint(
    state: web::Data<ApiManagerState>,
    id: web::Path<Uuid>,
    input: web::Json<UpdateEndpointInput>,
) -> HttpResponse {
    let id = id.into_inner();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiEndpoint, UnifiedError> {
        let mut endpoint = state
            .endpoints
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("endpoint not found"))?;
        if let Some(method) = &input.method {
            endpoint.method = validate_method(method)?;
        }
        if let Some(path) = &input.path {
            endpoint.path = validate_path(path)?;
        }
        if let Some(version) = &input.version {
            endpoint.version = validate_version(version)?;
        }
        if let Some(rules) = &input.rules {
            validate_rules(rules)?;
            endpoint.rules = rules.clone();
        }
        if let Some(rate_limit) = input.rate_limit {
            if rate_limit == 0 || rate_limit > 100_000 {
                return Err(UnifiedError::bad_request(
                    "rate_limit must be between 1 and 100000 requests per minute",
                ));
            }
            endpoint.rate_limit = rate_limit;
        }
        if let Some(description) = input.description {
            endpoint.description = description;
        }
        if let Some(enabled) = input.enabled {
            endpoint.enabled = enabled;
        }
        if state.endpoints.iter().any(|entry| {
            entry.key() != &id
                && entry.value().method == endpoint.method
                && entry.value().path == endpoint.path
                && entry.value().version == endpoint.version
        }) {
            return Err(UnifiedError::conflict(format!(
                "an endpoint for {} {} already exists in version {}",
                endpoint.method, endpoint.path, endpoint.version
            )));
        }
        endpoint.updated_at = Utc::now();
        state.endpoints.insert(id, endpoint.clone());
        Ok(endpoint)
    })();
    match outcome {
        Ok(endpoint) => api_json::success_response(endpoint, 200),
        Err(error) => api_json::error_response(&error, false),
    }
}

async fn delete_endpoint(state: web::Data<ApiManagerState>, id: web::Path<Uuid>) -> HttpResponse {
    let id = id.into_inner();
    match state.endpoints.remove(&id) {
        Some((_, endpoint)) => api_json::success_response(
            json!({ "id": endpoint.id, "deleted": true }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("endpoint not found"), false),
    }
}

async fn execute_endpoint(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    payload: web::Json<Value>,
) -> HttpResponse {
    let id = id.into_inner();
    let payload = payload.into_inner();
    let Some(mut endpoint) = state.endpoints.get(&id).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("endpoint not found"), false);
    };
    if !endpoint.enabled {
        return api_json::error_response(
            &UnifiedError::forbidden("endpoint is disabled"),
            ctx.is_debug(),
        );
    }
    // Per-endpoint per-minute rate limit.
    let minute = Utc::now().timestamp() / 60;
    let counter_key = format!("{id}:{minute}");
    let count = {
        let mut counter = state.rate_counters.entry(counter_key).or_insert(0);
        *counter += 1;
        *counter
    };
    if count > u64::from(endpoint.rate_limit) {
        return api_json::error_response(
            &UnifiedError::new(
                format!(
                    "rate limit exceeded: at most {} requests per minute for this endpoint",
                    endpoint.rate_limit
                ),
                429,
            )
            .with_metadata("endpoint_id", json!(id.to_string()))
            .with_metadata("rate_limit", json!(endpoint.rate_limit)),
            ctx.is_debug(),
        );
    }

    let errors = validate_payload(&payload, &endpoint.rules);
    let valid = errors.is_empty();
    endpoint.execution_count += 1;
    endpoint.updated_at = Utc::now();
    state.endpoints.insert(id, endpoint.clone());
    ctx.hooks.emit(
        "api-manager.endpoint.executed",
        json!({
            "endpoint_id": id.to_string(),
            "method": endpoint.method,
            "path": endpoint.path,
            "version": endpoint.version,
            "valid": valid,
            "error_count": errors.len(),
        }),
    );
    api_json::success_response(
        json!({
            "endpoint": {
                "id": endpoint.id,
                "method": endpoint.method,
                "path": endpoint.path,
                "version": endpoint.version,
            },
            "valid": valid,
            "errors": errors,
            "echo": payload,
            "dispatch": "validation_only",
            "note": "Payload validation only. Dispatching the request into a registered core route handler is out of scope for this milestone.",
        }),
        200,
    )
}

async fn list_versions(state: web::Data<ApiManagerState>) -> HttpResponse {
    let mut grouped: std::collections::BTreeMap<String, Vec<Value>> =
        std::collections::BTreeMap::new();
    for entry in state.endpoints.iter() {
        let endpoint = entry.value();
        grouped
            .entry(endpoint.version.clone())
            .or_default()
            .push(json!({
                "id": endpoint.id,
                "method": endpoint.method,
                "path": endpoint.path,
                "description": endpoint.description,
                "enabled": endpoint.enabled,
                "rate_limit": endpoint.rate_limit,
            }));
    }
    let versions: Vec<Value> = grouped
        .into_iter()
        .map(|(version, endpoints)| {
            let count = endpoints.len();
            json!({
                "version": version,
                "count": count,
                "endpoints": endpoints,
            })
        })
        .collect();
    api_json::success_response(json!({ "versions": versions }), 200)
}

// ---------------------------------------------------------------------------
// API key handlers
// ---------------------------------------------------------------------------

async fn create_key(
    state: web::Data<ApiManagerState>,
    input: web::Json<CreateKeyInput>,
) -> HttpResponse {
    let input = input.into_inner();
    let name = input.name.trim();
    if name.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("name must not be empty"),
            false,
        );
    }
    if name.chars().count() > 200 {
        return api_json::error_response(
            &UnifiedError::bad_request("name must be at most 200 characters"),
            false,
        );
    }
    let scopes = input.scopes.unwrap_or_default();
    for scope in &scopes {
        if scope.trim().is_empty() {
            return api_json::error_response(
                &UnifiedError::bad_request("scopes must not contain empty values"),
                false,
            );
        }
    }
    let expires_at = match input.expires_in_days {
        Some(days) => {
            if days == 0 || days > 3650 {
                return api_json::error_response(
                    &UnifiedError::bad_request("expires_in_days must be between 1 and 3650"),
                    false,
                );
            }
            Some(Utc::now() + Duration::days(i64::from(days)))
        }
        None => None,
    };
    let random_bytes: [u8; 32] = rand::random();
    let secret_part =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes);
    let full_key = format!("{KEY_PREFIX}{secret_part}");
    let prefix = format!("{}{}", KEY_PREFIX, &secret_part[..8.min(secret_part.len())]);
    let key = ApiKey {
        id: Uuid::now_v7(),
        name: name.to_string(),
        prefix: prefix.clone(),
        key_hash: state.hash_key(&full_key),
        scopes,
        created_at: Utc::now(),
        expires_at,
        last_used_at: None,
        revoked: false,
    };
    state.keys.insert(key.id, key.clone());
    // The full key is returned exactly once, at creation time.
    api_json::success_response(
        json!({
            "id": key.id,
            "name": key.name,
            "key": full_key,
            "prefix": prefix,
            "scopes": key.scopes,
            "created_at": key.created_at,
            "expires_at": key.expires_at,
            "revoked": false,
        }),
        201,
    )
}

async fn list_keys(
    state: web::Data<ApiManagerState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let search = params.search.clone();
    let revoked_filter = params.filters.get("revoked").cloned();
    let mut items: Vec<ApiKey> = state
        .keys
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|key| {
            if let Some(revoked) = &revoked_filter {
                let wanted = revoked == "true";
                if key.revoked != wanted {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return key.name.to_lowercase().contains(&needle)
                    || key.prefix.to_lowercase().contains(&needle);
            }
            true
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<ApiKey> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn revoke_key(state: web::Data<ApiManagerState>, id: web::Path<Uuid>) -> HttpResponse {
    let id = id.into_inner();
    match state.keys.get(&id).map(|entry| entry.value().clone()) {
        Some(mut key) => {
            if key.revoked {
                return api_json::error_response(
                    &UnifiedError::conflict("key is already revoked"),
                    false,
                );
            }
            key.revoked = true;
            state.keys.insert(id, key.clone());
            api_json::success_response(
                json!({ "id": key.id, "revoked": true }),
                200,
            )
        }
        None => api_json::error_response(&UnifiedError::not_found("key not found"), false),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ApiManagerModule {
    state: ApiManagerState,
}

impl ApiManagerModule {
    pub fn new() -> Self {
        Self {
            state: ApiManagerState::default(),
        }
    }
}

impl Default for ApiManagerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ApiManagerModule {
    fn id(&self) -> &'static str {
        "api-manager"
    }

    fn name(&self) -> &'static str {
        "API Manager"
    }

    fn description(&self) -> &'static str {
        "Public client API registry with payload validation, rate limits, and API keys."
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/api-manager")
                .app_data(state)
                .route("/endpoints", web::get().to(list_endpoints))
                .route("/endpoints", web::post().to(create_endpoint))
                .route("/endpoints/{id}", web::get().to(get_endpoint))
                .route("/endpoints/{id}", web::put().to(update_endpoint))
                .route("/endpoints/{id}", web::delete().to(delete_endpoint))
                .route("/endpoints/{id}/execute", web::post().to(execute_endpoint))
                .route("/versions", web::get().to(list_versions))
                .route("/keys", web::post().to(create_key))
                .route("/keys", web::get().to(list_keys))
                .route("/keys/{id}", web::delete().to(revoke_key)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ApiManagerModule::new()));
}
