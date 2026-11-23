//! HTTP handlers for social networks: admin-defined
//! networks (name, slug, kind, API base, publish path, auth
//! type, supported post types, text and media limits, rate
//! limit, enabled flag). No network is hard-coded: every
//! network is a data row, and a tenant with no network rows
//! cannot publish.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, created, err, load_network, ok, optional_text, paginate,
    parse_date_filter, record_audit, require_caller_company, require_manage, require_tenant,
    require_text, require_view, validate_api_base_url, validate_auth_type,
    validate_max_media_items, validate_max_text_length, validate_post_types,
    validate_posts_per_minute, validate_provider_kind, validate_publish_path, validate_slug,
};
use super::state::SocialMarketingState;
use super::types::{
    AuthType, CreateNetworkRequest, DEFAULT_MAX_MEDIA_ITEMS, DEFAULT_MAX_TEXT_LENGTH,
    DEFAULT_NETWORK_POSTS_PER_MINUTE, MAX_NAME_LEN, ProviderKind, SocialNetwork,
    UpdateNetworkRequest,
};

/// List networks (paginated, filter by kind, auth type,
/// enabled; search by name/slug; filter by created date
/// range with `from` / `to`).
pub(crate) async fn list_networks(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let auth_filter = params.filters.get("auth_type").cloned();
        let enabled_filter = params
            .filters
            .get("enabled")
            .and_then(|raw| match raw.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        let from_filter = match params.filters.get("from") {
            Some(raw) => Some(parse_date_filter(raw, "from")?),
            None => None,
        };
        let to_filter = match params.filters.get("to") {
            Some(raw) => Some(parse_date_filter(raw, "to")?),
            None => None,
        };
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.networks_of(&tenant_id);
        items.retain(|network| {
            if let Some(kind) = &kind_filter
                && network.kind.as_str() != kind
            {
                return false;
            }
            if let Some(auth) = &auth_filter
                && network.auth_type.as_str() != auth
            {
                return false;
            }
            if let Some(enabled) = enabled_filter
                && network.enabled != enabled
            {
                return false;
            }
            if let Some(from) = from_filter
                && network.created_at < from
            {
                return false;
            }
            if let Some(to) = to_filter
                && network.created_at > to
            {
                return false;
            }
            if let Some(needle) = &search
                && !network.name.to_lowercase().contains(needle)
                && !network.slug.to_lowercase().contains(needle)
            {
                return false;
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Create a network (manager). An HTTP network needs an
/// HTTPS API base and a publish path; a log-only network
/// takes neither. Post types, text and media limits, and
/// the rate limit are data on the row.
pub(crate) async fn create_network(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateNetworkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let slug = validate_slug(&input.slug)?;
        if state
            .networks_of(&tenant_id)
            .iter()
            .any(|network| network.slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "A network with slug \"{slug}\" already exists"
            )));
        }
        let kind = validate_provider_kind(&input.kind)?;
        let api_base_url = match kind {
            ProviderKind::Http => {
                let url = validate_api_base_url(&input.api_base_url)?;
                Some(url.ok_or_else(|| {
                    UnifiedError::bad_request(
                        "api_base_url is required for an http network".to_string(),
                    )
                })?)
            }
            ProviderKind::LogOnly => {
                if input.api_base_url.is_some() {
                    return Err(UnifiedError::bad_request(
                        "A log_only network has no api_base_url".to_string(),
                    ));
                }
                None
            }
        };
        let publish_path = match kind {
            ProviderKind::Http => match &input.publish_path {
                Some(path) => validate_publish_path(path)?,
                None => "/publish".to_string(),
            },
            ProviderKind::LogOnly => {
                if input.publish_path.is_some() {
                    return Err(UnifiedError::bad_request(
                        "A log_only network has no publish_path".to_string(),
                    ));
                }
                String::new()
            }
        };
        let auth_type = match &input.auth_type {
            Some(raw) => validate_auth_type(raw)?,
            None => AuthType::None,
        };
        if matches!(kind, ProviderKind::LogOnly) && auth_type.needs_credential() {
            return Err(UnifiedError::bad_request(
                "A log_only network takes no credential".to_string(),
            ));
        }
        let auth_header = optional_text(&input.auth_header, "auth_header", MAX_NAME_LEN)?;
        let post_types = validate_post_types(&input.post_types)?;
        let max_text_length = match input.max_text_length {
            Some(value) => validate_max_text_length(value)?,
            None => DEFAULT_MAX_TEXT_LENGTH,
        };
        let max_media_items = match input.max_media_items {
            Some(value) => validate_max_media_items(value)?,
            None => DEFAULT_MAX_MEDIA_ITEMS,
        };
        let posts_per_minute = match input.posts_per_minute {
            Some(value) => validate_posts_per_minute(value)?,
            None => DEFAULT_NETWORK_POSTS_PER_MINUTE,
        };
        let request_fields = input.request_fields.unwrap_or(Value::Null);
        if !request_fields.is_null() && !request_fields.is_object() {
            return Err(UnifiedError::bad_request(
                "request_fields must be a JSON object".to_string(),
            ));
        }
        let now = Utc::now();
        let network = SocialNetwork {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            slug,
            kind,
            api_base_url,
            publish_path,
            auth_type,
            auth_header,
            post_types,
            max_text_length,
            max_media_items,
            posts_per_minute,
            request_fields,
            enabled: input.enabled.unwrap_or(true),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        let view = serde_json::to_value(&network).unwrap_or(Value::Null);
        state.networks.insert(network.id, network.clone());
        record_audit(
            &state,
            &tenant_id,
            "network",
            &network.id.to_string(),
            "create",
            &caller.actor,
            None,
            Some(view.clone()),
        );
        Ok(view)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one network.
pub(crate) async fn get_network(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let network = load_network(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&network).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a network (manager).
pub(crate) async fn update_network(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateNetworkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut network = load_network(&state, &tenant_id, &path.into_inner())?;
        let before = serde_json::to_value(&network).unwrap_or(Value::Null);
        if let Some(name) = &input.name {
            network.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(slug) = &input.slug {
            let slug = validate_slug(slug)?;
            if slug != network.slug
                && state
                    .networks_of(&tenant_id)
                    .iter()
                    .any(|other| other.slug == slug)
            {
                return Err(UnifiedError::conflict(format!(
                    "A network with slug \"{slug}\" already exists"
                )));
            }
            network.slug = slug;
        }
        if let Some(api_base_url) = &input.api_base_url {
            match api_base_url {
                Some(url) => {
                    if network.kind != ProviderKind::Http {
                        return Err(UnifiedError::bad_request(
                            "A log_only network has no api_base_url".to_string(),
                        ));
                    }
                    network.api_base_url = validate_api_base_url(&Some(url.clone()))?;
                }
                None => {
                    if network.kind == ProviderKind::Http {
                        return Err(UnifiedError::bad_request(
                            "api_base_url is required for an http network".to_string(),
                        ));
                    }
                    network.api_base_url = None;
                }
            }
        }
        if let Some(publish_path) = &input.publish_path {
            if network.kind != ProviderKind::Http {
                return Err(UnifiedError::bad_request(
                    "A log_only network has no publish_path".to_string(),
                ));
            }
            network.publish_path = validate_publish_path(publish_path)?;
        }
        if let Some(auth_type) = &input.auth_type {
            network.auth_type = validate_auth_type(auth_type)?;
        }
        if let Some(auth_header) = &input.auth_header {
            network.auth_header = match auth_header {
                Some(text) => Some(require_text(text, "auth_header", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(post_types) = &input.post_types {
            network.post_types = validate_post_types(post_types)?;
        }
        if let Some(max_text_length) = input.max_text_length {
            network.max_text_length = validate_max_text_length(max_text_length)?;
        }
        if let Some(max_media_items) = input.max_media_items {
            network.max_media_items = validate_max_media_items(max_media_items)?;
        }
        if let Some(posts_per_minute) = input.posts_per_minute {
            network.posts_per_minute = validate_posts_per_minute(posts_per_minute)?;
        }
        if let Some(request_fields) = &input.request_fields {
            if !request_fields.is_null() && !request_fields.is_object() {
                return Err(UnifiedError::bad_request(
                    "request_fields must be a JSON object".to_string(),
                ));
            }
            network.request_fields = request_fields.clone();
        }
        if let Some(enabled) = input.enabled {
            network.enabled = enabled;
        }
        network.updated_at = Utc::now();
        let view = serde_json::to_value(&network).unwrap_or(Value::Null);
        state.networks.insert(network.id, network.clone());
        record_audit(
            &state,
            &tenant_id,
            "network",
            &network.id.to_string(),
            "update",
            &caller.actor,
            Some(before),
            Some(view.clone()),
        );
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a network (manager). Refused while any account
/// is connected to it; posts that already went through it
/// are kept.
pub(crate) async fn delete_network(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let network = load_network(&state, &tenant_id, &path.into_inner())?;
        let referenced = state.accounts.iter().any(|entry| {
            entry.value().tenant_id == tenant_id && entry.value().network_id == network.id
        });
        if referenced {
            return Err(UnifiedError::conflict(
                "Network is referenced by an account; delete or move the account first",
            ));
        }
        state.networks.remove(&network.id);
        record_audit(
            &state,
            &tenant_id,
            "network",
            &network.id.to_string(),
            "delete",
            &caller.actor,
            Some(serde_json::to_value(&network).unwrap_or(Value::Null)),
            None,
        );
        Ok(json!({ "deleted": true, "network_id": network.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
