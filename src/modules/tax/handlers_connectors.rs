// Tax module - connector and audit handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// Handlers: connectors

pub(crate) async fn list_connectors(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let auth: UnifiedResult<Uuid> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        acting_company(&ctx, &_tenant, &req, &caller)
    })();
    let company_id = match auth {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let params = query.into_inner();
    let mut items: Vec<TaxConnectorView> = state
        .connectors
        .iter()
        .map(|entry| TaxConnectorView::from(entry.value()))
        .filter(|connector| {
            connector.company_id == company_id
                && matches_search(
                    &format!(
                        "{} {} {}",
                        connector.name, connector.kind, connector.endpoint_url
                    ),
                    &params,
                )
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| connector.kind == *kind)
                && params
                    .filters
                    .get("enabled")
                    .is_none_or(|flag| connector.enabled == (flag == "true"))
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_connector(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    body: web::Json<CreateTaxConnector>,
) -> HttpResponse {
    let result: UnifiedResult<TaxConnectorView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_CONNECTORS)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("name must not be empty"));
        }
        if !CONNECTOR_KINDS.contains(&input.kind.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "kind must be one of: {}",
                CONNECTOR_KINDS.join(", ")
            )));
        }
        let auth_scheme = input.auth_scheme.unwrap_or_else(|| "none".to_string());
        if !AUTH_SCHEMES.contains(&auth_scheme.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "auth_scheme must be one of: {}",
                AUTH_SCHEMES.join(", ")
            )));
        }
        let allowed_hosts: Vec<String> = input
            .allowed_hosts
            .iter()
            .map(|host| host.trim().to_lowercase())
            .filter(|host| !host.is_empty())
            .collect();
        if allowed_hosts.is_empty() {
            return Err(UnifiedError::bad_request(
                "allowed_hosts must list at least one host",
            ));
        }
        if input.kind == "example" {
            // The example provider mapping is clearly marked and uses
            // an obviously fake endpoint; it never performs a network
            // call, so its host is not DNS-checked.
            let url = Url::parse(input.endpoint_url.trim())
                .map_err(|_| UnifiedError::bad_request("endpoint_url must be a valid URL"))?;
            if url.scheme() != "https" {
                return Err(UnifiedError::bad_request("endpoint_url must use https"));
            }
        } else {
            validate_endpoint_url(&input.endpoint_url, &allowed_hosts)?;
        }
        if state.connectors.iter().any(|entry| {
            entry.value().company_id == company_id
                && entry.value().name.eq_ignore_ascii_case(input.name.trim())
        }) {
            return Err(UnifiedError::conflict(format!(
                "A tax connector named \"{}\" already exists for this company",
                input.name.trim()
            )));
        }
        let timeout_ms = input.timeout_ms.unwrap_or(10_000);
        if !(100..=120_000).contains(&timeout_ms) {
            return Err(UnifiedError::bad_request(
                "timeout_ms must be between 100 and 120000",
            ));
        }
        let max_retries = input.max_retries.unwrap_or(2);
        if max_retries > 5 {
            return Err(UnifiedError::bad_request(
                "max_retries must be between 0 and 5",
            ));
        }
        if auth_scheme != "none"
            && input
                .credential
                .as_deref()
                .is_none_or(|v| v.trim().is_empty())
        {
            return Err(UnifiedError::bad_request(
                "credential is required when auth_scheme is not none",
            ));
        }
        if auth_scheme == "hmac"
            && input
                .signing_secret
                .as_deref()
                .is_none_or(|v| v.trim().is_empty())
        {
            return Err(UnifiedError::bad_request(
                "signing_secret is required when auth_scheme is hmac",
            ));
        }
        let secret = encryption_secret(&ctx);
        let (credential_encrypted, credential_fingerprint_value) = match input.credential {
            Some(plain) if !plain.trim().is_empty() => (
                Some(encrypt_tax_credential(&secret, plain.trim())),
                Some(credential_fingerprint(plain.trim())),
            ),
            _ => (None, None),
        };
        let (signing_secret_encrypted, signing_secret_fingerprint) = match input.signing_secret {
            Some(plain) if !plain.trim().is_empty() => (
                Some(encrypt_tax_credential(&secret, plain.trim())),
                Some(credential_fingerprint(plain.trim())),
            ),
            _ => (None, None),
        };
        let connector = TaxConnector {
            id: new_id(),
            company_id,
            name: input.name.trim().to_string(),
            kind: input.kind,
            endpoint_url: input.endpoint_url.trim().to_string(),
            allowed_hosts,
            auth_scheme,
            credential_encrypted,
            credential_fingerprint: credential_fingerprint_value,
            signing_secret_encrypted,
            signing_secret_fingerprint,
            timeout_ms,
            max_retries,
            enabled: input.enabled.unwrap_or(true),
            request_mapping: input.request_mapping.unwrap_or_else(|| json!({})),
            response_mapping: input.response_mapping.unwrap_or_else(|| json!({})),
            created_at: now(),
            updated_at: now(),
        };
        state
            .connectors
            .insert(connector.id.clone(), connector.clone());
        state.record_audit(
            company_id,
            &caller.actor,
            "connector_created",
            "tax_connector",
            &connector.id,
            "tax.connector_created",
            json!({
                "name": connector.name,
                "kind": connector.kind,
                "credential_configured": connector.credential_encrypted.is_some(),
                "credential_fingerprint": connector.credential_fingerprint,
            }),
        );
        Ok(TaxConnectorView::from(&connector))
    })();
    respond_created(result)
}

pub(crate) async fn get_connector(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<TaxConnectorView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let connector = state
            .connectors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax connector not found"))?;
        if connector.company_id != company_id {
            return Err(UnifiedError::not_found("Tax connector not found"));
        }
        Ok(TaxConnectorView::from(&connector))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_connector(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTaxConnector>,
) -> HttpResponse {
    let result: UnifiedResult<TaxConnectorView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_CONNECTORS)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let input = body.into_inner();
        // Read the current connector first, with no write guard held,
        // so every validation - including the name-uniqueness scan
        // over the same map - runs before any mutation is applied.
        // A failed validation must leave the stored connector
        // untouched.
        let current = state
            .connectors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax connector not found"))?;
        if current.company_id != company_id {
            return Err(UnifiedError::not_found("Tax connector not found"));
        }
        if let Some(auth_scheme) = &input.auth_scheme {
            if !AUTH_SCHEMES.contains(&auth_scheme.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "auth_scheme must be one of: {}",
                    AUTH_SCHEMES.join(", ")
                )));
            }
        }
        let new_allowed_hosts = input.allowed_hosts.as_ref().map(|hosts| {
            hosts
                .iter()
                .map(|host| host.trim().to_lowercase())
                .filter(|host| !host.is_empty())
                .collect::<Vec<String>>()
        });
        if let Some(hosts) = &new_allowed_hosts {
            if hosts.is_empty() {
                return Err(UnifiedError::bad_request(
                    "allowed_hosts must list at least one host",
                ));
            }
        }
        let new_endpoint = input
            .endpoint_url
            .as_ref()
            .map(|value| value.trim().to_string());
        let effective_hosts: Vec<String> = new_allowed_hosts
            .clone()
            .unwrap_or_else(|| current.allowed_hosts.clone());
        if let Some(endpoint) = &new_endpoint {
            if current.kind == "example" {
                let url = Url::parse(endpoint)
                    .map_err(|_| UnifiedError::bad_request("endpoint_url must be a valid URL"))?;
                if url.scheme() != "https" {
                    return Err(UnifiedError::bad_request("endpoint_url must use https"));
                }
            } else {
                validate_endpoint_url(endpoint, &effective_hosts)?;
            }
        } else if let Some(hosts) = &new_allowed_hosts {
            if current.kind != "example" {
                validate_endpoint_url(&current.endpoint_url, hosts)?;
            }
        }
        if let Some(name) = &input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("name must not be empty"));
            }
            if state.connectors.iter().any(|entry| {
                entry.key() != path.as_str()
                    && entry.value().company_id == company_id
                    && entry.value().name.eq_ignore_ascii_case(name.trim())
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A tax connector named \"{}\" already exists for this company",
                    name.trim()
                )));
            }
        }
        if let Some(timeout_ms) = input.timeout_ms {
            if !(100..=120_000).contains(&timeout_ms) {
                return Err(UnifiedError::bad_request(
                    "timeout_ms must be between 100 and 120000",
                ));
            }
        }
        if let Some(max_retries) = input.max_retries {
            if max_retries > 5 {
                return Err(UnifiedError::bad_request(
                    "max_retries must be between 0 and 5",
                ));
            }
        }
        // Effective credential state after the update, checked before
        // any mutation: a scheme that needs a credential must still
        // have one, and `hmac` must still have a signing secret.
        let effective_scheme = input
            .auth_scheme
            .clone()
            .unwrap_or_else(|| current.auth_scheme.clone());
        let credential_will_exist = match &input.credential {
            Some(plain) => !plain.trim().is_empty(),
            None => current.credential_encrypted.is_some(),
        };
        let signing_will_exist = match &input.signing_secret {
            Some(plain) => !plain.trim().is_empty(),
            None => current.signing_secret_encrypted.is_some(),
        };
        if effective_scheme != "none" && !credential_will_exist {
            return Err(UnifiedError::bad_request(
                "credential is required when auth_scheme is not none",
            ));
        }
        if effective_scheme == "hmac" && !signing_will_exist {
            return Err(UnifiedError::bad_request(
                "signing_secret is required when auth_scheme is hmac",
            ));
        }
        let secret = encryption_secret(&ctx);
        let mut connector = state
            .connectors
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Tax connector not found"))?;
        if let Some(name) = input.name {
            connector.name = name.trim().to_string();
        }
        if let Some(endpoint) = new_endpoint {
            connector.endpoint_url = endpoint;
        }
        if let Some(hosts) = new_allowed_hosts {
            connector.allowed_hosts = hosts;
        }
        if let Some(auth_scheme) = input.auth_scheme {
            connector.auth_scheme = auth_scheme;
        }
        if let Some(plain) = input.credential {
            if plain.trim().is_empty() {
                connector.credential_encrypted = None;
                connector.credential_fingerprint = None;
            } else {
                connector.credential_encrypted =
                    Some(encrypt_tax_credential(&secret, plain.trim()));
                connector.credential_fingerprint = Some(credential_fingerprint(plain.trim()));
            }
        }
        if let Some(plain) = input.signing_secret {
            if plain.trim().is_empty() {
                connector.signing_secret_encrypted = None;
                connector.signing_secret_fingerprint = None;
            } else {
                connector.signing_secret_encrypted =
                    Some(encrypt_tax_credential(&secret, plain.trim()));
                connector.signing_secret_fingerprint = Some(credential_fingerprint(plain.trim()));
            }
        }
        if let Some(timeout_ms) = input.timeout_ms {
            connector.timeout_ms = timeout_ms;
        }
        if let Some(max_retries) = input.max_retries {
            connector.max_retries = max_retries;
        }
        if let Some(enabled) = input.enabled {
            connector.enabled = enabled;
        }
        if let Some(mapping) = input.request_mapping {
            connector.request_mapping = mapping;
        }
        if let Some(mapping) = input.response_mapping {
            connector.response_mapping = mapping;
        }
        connector.updated_at = now();
        let view = TaxConnectorView::from(&*connector);
        let connector_id = connector.id.clone();
        let connector_name = connector.name.clone();
        let connector_kind = connector.kind.clone();
        let credential_configured = connector.credential_encrypted.is_some();
        let fingerprint = connector.credential_fingerprint.clone();
        drop(connector);
        state.record_audit(
            company_id,
            &caller.actor,
            "connector_updated",
            "tax_connector",
            &connector_id,
            "tax.connector_updated",
            json!({
                "name": connector_name,
                "kind": connector_kind,
                "credential_configured": credential_configured,
                "credential_fingerprint": fingerprint,
            }),
        );
        Ok(view)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_connector(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_CONNECTORS)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let connector = state
            .connectors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax connector not found"))?;
        if connector.company_id != company_id {
            return Err(UnifiedError::not_found("Tax connector not found"));
        }
        state.connectors.remove(path.as_str());
        state.record_audit(
            company_id,
            &caller.actor,
            "connector_deleted",
            "tax_connector",
            &connector.id,
            "tax.connector_deleted",
            json!({ "name": connector.name, "kind": connector.kind }),
        );
        Ok(json!({ "deleted": true, "id": connector.id }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn test_connector(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let found: UnifiedResult<(Caller, Uuid, TaxConnector, Value)> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_CONNECTORS)?;
        let company_id = acting_company(&ctx, &_tenant, &req, &caller)?;
        let connector = state
            .connectors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tax connector not found"))?;
        if connector.company_id != company_id {
            return Err(UnifiedError::not_found("Tax connector not found"));
        }
        Ok((caller, company_id, connector, body.into_inner()))
    })();
    let (caller, company_id, connector, payload) = match found {
        Ok(values) => values,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if connector.kind == "example" {
        // The example provider mapping never performs a network call:
        // the test returns a simulated confirmation with obviously
        // fake field names.
        let result = json!({
            "connector_id": connector.id,
            "kind": "example",
            "simulated": true,
            "http_status": 200,
            "example_reference_number": format!("EXAMPLE-TEST-{}", connector.id),
            "example_confirmation_code": "EXAMPLE-OK",
            "received_payload": payload,
        });
        state.record_audit(
            company_id,
            &caller.actor,
            "connector_tested",
            "tax_connector",
            &connector.id,
            "tax.connector_tested",
            json!({ "name": connector.name, "kind": "example", "simulated": true }),
        );
        return json::success_response(result, 200);
    }
    let validation = validate_endpoint_url(&connector.endpoint_url, &connector.allowed_hosts);
    let url = match validation {
        Ok(url) => url,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let secret = encryption_secret(&ctx);
    let credential = match &connector.credential_encrypted {
        Some(stored) => match decrypt_tax_credential(&secret, stored) {
            Ok(value) => Some(value),
            Err(error) => return json::error_response(&error, ctx.is_debug()),
        },
        None => None,
    };
    let signing_secret = match &connector.signing_secret_encrypted {
        Some(stored) => match decrypt_tax_credential(&secret, stored) {
            Ok(value) => Some(value),
            Err(error) => return json::error_response(&error, ctx.is_debug()),
        },
        None => None,
    };
    let mut test_payload = json!({ "test": true });
    if let Value::Object(map) = &payload {
        if let Value::Object(test_map) = &mut test_payload {
            for (key, value) in map {
                test_map.insert(key.clone(), value.clone());
            }
        }
    }
    let test_payload = apply_request_mapping(&connector.request_mapping, &test_payload);
    let body_text = match serde_json::to_string(&test_payload) {
        Ok(text) => text,
        Err(error) => {
            return json::error_response(
                &UnifiedError::internal(format!("Cannot serialize test payload: {error}")),
                false,
            );
        }
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(connector.timeout_ms))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return json::error_response(
                &UnifiedError::internal(format!("Cannot build HTTP client: {error}")),
                false,
            );
        }
    };
    let mut last_error = String::new();
    let attempts = connector.max_retries as usize + 1;
    let mut outcome: Option<Value> = None;
    for attempt in 0..attempts {
        let timestamp = Utc::now().timestamp();
        let mut request = client
            .post(url.clone())
            .header("Content-Type", "application/json")
            .header("X-Tax-Test", "true");
        match connector.auth_scheme.as_str() {
            "bearer" => {
                if let Some(key) = &credential {
                    request = request.bearer_auth(key);
                }
            }
            "api_key" => {
                if let Some(key) = &credential {
                    request = request.header("X-Api-Key", key);
                }
            }
            "hmac" => {
                if let Some(signing) = &signing_secret {
                    let signature = sign_body(signing, timestamp, &body_text);
                    request = request
                        .header("X-Tax-Signature", format!("sha256={signature}"))
                        .header("X-Tax-Timestamp", timestamp.to_string());
                }
            }
            _ => {}
        }
        match request.body(body_text.clone()).send().await {
            Ok(response) => {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                let preview: String = text.chars().take(RESPONSE_PREVIEW_LIMIT).collect();
                let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                let mapped = apply_response_mapping(&connector.response_mapping, &parsed);
                outcome = Some(json!({
                    "connector_id": connector.id,
                    "kind": connector.kind,
                    "simulated": false,
                    "http_status": status.as_u16(),
                    "success": status.is_success(),
                    "response_preview": preview,
                    "mapped": mapped,
                }));
                break;
            }
            Err(error) => {
                last_error = format!("Provider request failed: {error}");
                if attempt + 1 < attempts {
                    tokio::time::sleep(Duration::from_millis(200 * (1 << attempt))).await;
                }
            }
        }
    }
    let result = match outcome {
        Some(value) => value,
        None => {
            return json::error_response(
                &UnifiedError::internal(format!(
                    "Connector test failed after {attempts} attempt(s): {last_error}"
                )),
                false,
            );
        }
    };
    state.record_audit(
        company_id,
        &caller.actor,
        "connector_tested",
        "tax_connector",
        &connector.id,
        "tax.connector_tested",
        json!({
            "name": connector.name,
            "kind": connector.kind,
            "simulated": false,
            "http_status": result.get("http_status"),
        }),
    );
    json::success_response(result, 200)
}

// Handlers: audit trail

pub(crate) async fn list_audit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TaxState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let auth: UnifiedResult<Uuid> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        acting_company(&ctx, &_tenant, &req, &caller)
    })();
    let company_id = match auth {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let params = query.into_inner();
    let mut items: Vec<TaxAuditEntry> = state
        .audit
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            entry.company_id == company_id
                && params
                    .filters
                    .get("action")
                    .is_none_or(|action| entry.action == *action)
                && params
                    .filters
                    .get("resource_type")
                    .is_none_or(|kind| entry.resource_type == *kind)
                && params
                    .filters
                    .get("resource_id")
                    .is_none_or(|id| entry.resource_id == *id)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}
