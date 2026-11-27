// Properties - Location handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// Location handlers

pub(crate) async fn search_location(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    query: web::Query<LocationSearchQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // Tenant edge guard (C3 pilot 3): fail closed without a
    // resolved tenant. These handlers call the external geocoder
    // only and read no tenant data, so there is no scope to check.
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW) {
        return err(error, debug);
    }
    let query_text = query.q.trim().to_string();
    if query_text.is_empty() {
        return err(
            UnifiedError::bad_request("q must not be empty"),
            debug,
        );
    }
    if query_text.len() > MAX_SEARCH_QUERY_LEN {
        return err(
            UnifiedError::bad_request(format!(
                "q must be at most {MAX_SEARCH_QUERY_LEN} characters"
            )),
            debug,
        );
    }
    let provider = state.provider();
    let provider_name = provider.name().to_string();
    match provider.search(&query_text).await {
        Ok(results) => ok(json!({
            "provider": provider_name,
            "query": query_text,
            "results": results,
        })),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reverse_location(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    query: web::Query<LocationReverseQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // Tenant edge guard (C3 pilot 3): fail closed without a
    // resolved tenant. These handlers call the external geocoder
    // only and read no tenant data, so there is no scope to check.
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW) {
        return err(error, debug);
    }
    let latitude = match valid_latitude(query.lat) {
        Ok(latitude) => latitude,
        Err(error) => return err(error, debug),
    };
    let longitude = match valid_longitude(query.lng) {
        Ok(longitude) => longitude,
        Err(error) => return err(error, debug),
    };
    let provider = state.provider();
    let provider_name = provider.name().to_string();
    match provider.reverse_geocode(latitude, longitude).await {
        Ok(result) => ok(json!({
            "provider": provider_name,
            "latitude": latitude,
            "longitude": longitude,
            "result": result,
        })),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_location_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LocationSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let settings = state
            .location_settings
            .read()
            .expect("settings lock poisoned")
            .clone();
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_location_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    body: web::Json<UpdateLocationSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LocationSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state
            .location_settings
            .read()
            .expect("settings lock poisoned")
            .clone();
        if let Some(provider) = input.provider {
            let provider = provider.trim().to_lowercase();
            if provider != "none" && provider != "nominatim" {
                return Err(UnifiedError::bad_request(
                    "provider must be \"none\" or \"nominatim\"",
                ));
            }
            settings.provider = provider;
        }
        if let Some(base_url) = input.base_url {
            settings.base_url = base_url
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(allowed_host) = input.allowed_host {
            let allowed_host = allowed_host.trim().to_lowercase();
            if !valid_hostname(&allowed_host) {
                return Err(UnifiedError::bad_request(format!(
                    "allowed_host \"{allowed_host}\" is not a valid hostname"
                )));
            }
            settings.allowed_host = allowed_host;
        }
        if let Some(timeout_secs) = input.timeout_secs {
            if timeout_secs == 0 || timeout_secs > MAX_GEOCODER_TIMEOUT_SECS {
                return Err(UnifiedError::bad_request(format!(
                    "timeout_secs must be between 1 and {MAX_GEOCODER_TIMEOUT_SECS}"
                )));
            }
            settings.timeout_secs = timeout_secs;
        }
        if let Some(hosts) = input.link_allowlist_hosts {
            for host in &hosts {
                if !valid_hostname(host.trim()) {
                    return Err(UnifiedError::bad_request(format!(
                        "link_allowlist_hosts entry \"{host}\" is not a valid hostname"
                    )));
                }
            }
            settings.link_allowlist_hosts = hosts
                .into_iter()
                .map(|host| host.trim().to_lowercase())
                .collect();
        }
        // Build the provider BEFORE storing the settings, so an
        // invalid configuration is rejected instead of saved.
        let provider = build_provider(&settings)?;
        *state
            .location_settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        *state
            .location_provider
            .write()
            .expect("provider lock poisoned") = provider;
        state.emit(
            "properties.location.settings_updated",
            json!({
                "provider": settings.provider,
                "actor_id": caller.actor,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}
