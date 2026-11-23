//! HTTP handlers for SMS providers: admin-configured
//! sending providers (name, kind, endpoint, auth type,
//! write-only credential, sender id, per-provider rate
//! limit, enabled flag). The credential is write-only and
//! encrypted; reads show only whether a credential is
//! configured and its fingerprint. The webhook secret is
//! write-only too: the raw secret is returned exactly once
//! at creation / rotation, only its hash is stored.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::crypto::{
    credential_fingerprint, decrypt_credential, encrypt_credential, encryption_secret,
    generate_webhook_secret, hash_webhook_secret,
};
use super::helpers::{
    caller, caller_company, created, err, load_provider, ok, optional_text, paginate, record_audit,
    require_caller_company, require_manage, require_tenant, require_text, require_view,
    validate_auth_type, validate_endpoint_url, validate_provider_kind, validate_sender_id,
    validate_sends_per_minute,
};
use super::state::SmsMarketingState;
use super::types::{
    CreateProviderRequest, DEFAULT_PROVIDER_SENDS_PER_MINUTE, EVENT_PROVIDER_CREATED, MAX_NAME_LEN,
    SmsProvider, UpdateProviderRequest,
};

/// List providers (paginated, filter by kind, auth type,
/// enabled; search by name/sender id). The credential never
/// appears in the list view.
pub(crate) async fn list_providers(
    state: web::Data<SmsMarketingState>,
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
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.providers_of(&tenant_id);
        items.retain(|provider| {
            if let Some(kind) = &kind_filter
                && provider.kind.as_str() != kind
            {
                return false;
            }
            if let Some(auth) = &auth_filter
                && provider.auth_type.as_str() != auth
            {
                return false;
            }
            if let Some(enabled) = enabled_filter
                && provider.enabled != enabled
            {
                return false;
            }
            if let Some(needle) = &search
                && !provider.name.to_lowercase().contains(needle)
                && !provider.sender_id.to_lowercase().contains(needle)
            {
                return false;
            }
            true
        });
        let views: Vec<Value> = items.iter().map(|item| item.public_view()).collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Create a provider (manager). An HTTP provider needs an
/// HTTPS endpoint; a log-only provider takes none. When a
/// credential is supplied it is encrypted at once with
/// `app.secret_key` (no configured secret refuses the
/// write) and only its fingerprint is ever shown. When a
/// webhook secret is supplied its hash is stored and the raw
/// secret is returned once in the response; when none is
/// supplied a fresh secret is generated and returned once,
/// so delivery webhooks are secret-gated from the start.
pub(crate) async fn create_provider(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateProviderRequest>,
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
        let kind = validate_provider_kind(&input.kind)?;
        let endpoint_url = match kind {
            super::types::ProviderKind::Http => {
                let url = validate_endpoint_url(&input.endpoint_url)?;
                Some(url.ok_or_else(|| {
                    UnifiedError::bad_request(
                        "endpoint_url is required for an http provider".to_string(),
                    )
                })?)
            }
            super::types::ProviderKind::LogOnly => {
                if input.endpoint_url.is_some() {
                    return Err(UnifiedError::bad_request(
                        "A log_only provider has no endpoint_url".to_string(),
                    ));
                }
                None
            }
        };
        let auth_type = match &input.auth_type {
            Some(raw) => validate_auth_type(raw)?,
            None => super::types::AuthType::None,
        };
        if matches!(kind, super::types::ProviderKind::LogOnly) && auth_type.needs_credential() {
            return Err(UnifiedError::bad_request(
                "A log_only provider takes no credential".to_string(),
            ));
        }
        let auth_header = optional_text(&input.auth_header, "auth_header", MAX_NAME_LEN)?;
        let auth_username = optional_text(&input.auth_username, "auth_username", MAX_NAME_LEN)?;
        if auth_type == super::types::AuthType::Basic
            && auth_username.as_deref().unwrap_or("").is_empty()
        {
            return Err(UnifiedError::bad_request(
                "auth_username is required for basic auth".to_string(),
            ));
        }
        let sender_id = validate_sender_id(&input.sender_id)?;
        let sends_per_minute = match input.sends_per_minute {
            Some(value) => validate_sends_per_minute(value)?,
            None => DEFAULT_PROVIDER_SENDS_PER_MINUTE,
        };
        let request_fields = input.request_fields.unwrap_or(Value::Null);
        if !request_fields.is_null() && !request_fields.is_object() {
            return Err(UnifiedError::bad_request(
                "request_fields must be a JSON object".to_string(),
            ));
        }
        let now = Utc::now();
        let mut provider = SmsProvider {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            kind,
            endpoint_url,
            auth_type,
            auth_header,
            auth_username,
            credential_encrypted: None,
            credential_fingerprint: None,
            webhook_secret_hash: None,
            sender_id,
            sends_per_minute,
            request_fields,
            enabled: input.enabled.unwrap_or(true),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        if let Some(credential) = input.credential.as_deref().filter(|key| !key.is_empty()) {
            if !auth_type.needs_credential() {
                return Err(UnifiedError::bad_request(
                    "This auth type takes no credential".to_string(),
                ));
            }
            let secret = encryption_secret(&ctx)?;
            provider.credential_encrypted = Some(encrypt_credential(&secret, credential));
            provider.credential_fingerprint = Some(credential_fingerprint(credential));
        } else if auth_type.needs_credential() {
            return Err(UnifiedError::bad_request(
                "This auth type needs a credential: pass credential once (write-only, stored encrypted)".to_string(),
            ));
        }
        // The webhook secret is always set: supplied or fresh.
        // The raw value is returned exactly once, below.
        let webhook_secret = match input.webhook_secret.as_deref().filter(|s| !s.is_empty()) {
            Some(secret) => secret.to_string(),
            None => generate_webhook_secret(),
        };
        provider.webhook_secret_hash = Some(hash_webhook_secret(&webhook_secret));
        let view = provider.public_view();
        state.providers.insert(provider.id, provider.clone());
        record_audit(
            &state,
            &tenant_id,
            "provider",
            &provider.id.to_string(),
            "create",
            &caller.actor,
            None,
            Some(view.clone()),
        );
        state.emit(
            EVENT_PROVIDER_CREATED,
            json!({
                "tenant_id": tenant_id,
                "provider_id": provider.id.to_string(),
                "kind": provider.kind.as_str(),
                "created_by": caller.actor,
            }),
        );
        // The raw webhook secret is returned exactly once, in
        // this response, and never stored.
        let mut response = view;
        if let Some(object) = response.as_object_mut() {
            object.insert("webhook_secret".to_string(), Value::String(webhook_secret));
        }
        Ok(response)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one provider (masked view — the credential and the
/// webhook secret never appear).
pub(crate) async fn get_provider(
    state: web::Data<SmsMarketingState>,
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
        let provider = load_provider(&state, &tenant_id, &path.into_inner())?;
        Ok(provider.public_view())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a provider (manager). Passing `credential` replaces
/// the stored credential (an empty string clears it);
/// passing `webhook_secret` replaces the webhook secret and
/// returns the new raw secret once. Omitting either leaves
/// it untouched.
pub(crate) async fn update_provider(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateProviderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut provider = load_provider(&state, &tenant_id, &path.into_inner())?;
        let before = provider.public_view();
        let mut new_webhook_secret: Option<String> = None;
        if let Some(name) = &input.name {
            provider.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(endpoint_url) = &input.endpoint_url {
            match endpoint_url {
                Some(url) => {
                    if provider.kind != super::types::ProviderKind::Http {
                        return Err(UnifiedError::bad_request(
                            "A log_only provider has no endpoint_url".to_string(),
                        ));
                    }
                    provider.endpoint_url = validate_endpoint_url(&Some(url.clone()))?;
                }
                None => {
                    if provider.kind == super::types::ProviderKind::Http {
                        return Err(UnifiedError::bad_request(
                            "endpoint_url is required for an http provider".to_string(),
                        ));
                    }
                    provider.endpoint_url = None;
                }
            }
        }
        if let Some(auth_type) = &input.auth_type {
            provider.auth_type = validate_auth_type(auth_type)?;
        }
        if let Some(auth_header) = &input.auth_header {
            provider.auth_header = match auth_header {
                Some(text) => Some(require_text(text, "auth_header", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(auth_username) = &input.auth_username {
            provider.auth_username = match auth_username {
                Some(text) => Some(require_text(text, "auth_username", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if provider.auth_type == super::types::AuthType::Basic
            && provider.auth_username.as_deref().unwrap_or("").is_empty()
        {
            return Err(UnifiedError::bad_request(
                "auth_username is required for basic auth".to_string(),
            ));
        }
        if let Some(credential) = &input.credential {
            match credential.as_deref() {
                Some(text) if !text.is_empty() => {
                    if !provider.auth_type.needs_credential() {
                        return Err(UnifiedError::bad_request(
                            "This auth type takes no credential".to_string(),
                        ));
                    }
                    let secret = encryption_secret(&ctx)?;
                    provider.credential_encrypted = Some(encrypt_credential(&secret, text));
                    provider.credential_fingerprint = Some(credential_fingerprint(text));
                }
                _ => {
                    provider.credential_encrypted = None;
                    provider.credential_fingerprint = None;
                }
            }
        }
        if provider.auth_type.needs_credential() && provider.credential_encrypted.is_none() {
            return Err(UnifiedError::bad_request(
                "This auth type needs a credential: pass credential once (write-only, stored encrypted)".to_string(),
            ));
        }
        if let Some(webhook_secret) = &input.webhook_secret {
            match webhook_secret.as_deref() {
                Some(text) if !text.is_empty() => {
                    provider.webhook_secret_hash = Some(hash_webhook_secret(text));
                    new_webhook_secret = Some(text.to_string());
                }
                _ => {
                    provider.webhook_secret_hash = None;
                }
            }
        }
        if let Some(sender_id) = &input.sender_id {
            provider.sender_id = validate_sender_id(sender_id)?;
        }
        if let Some(sends_per_minute) = input.sends_per_minute {
            provider.sends_per_minute = validate_sends_per_minute(sends_per_minute)?;
        }
        if let Some(request_fields) = &input.request_fields {
            if !request_fields.is_null() && !request_fields.is_object() {
                return Err(UnifiedError::bad_request(
                    "request_fields must be a JSON object".to_string(),
                ));
            }
            provider.request_fields = request_fields.clone();
        }
        if let Some(enabled) = input.enabled {
            provider.enabled = enabled;
        }
        provider.updated_at = Utc::now();
        let view = provider.public_view();
        state.providers.insert(provider.id, provider.clone());
        record_audit(
            &state,
            &tenant_id,
            "provider",
            &provider.id.to_string(),
            "update",
            &caller.actor,
            Some(before),
            Some(view.clone()),
        );
        let mut response = view;
        if let Some(secret) = new_webhook_secret
            && let Some(object) = response.as_object_mut()
        {
            object.insert("webhook_secret".to_string(), Value::String(secret));
        }
        Ok(response)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a provider (manager). Refused while any campaign
/// references it; messages that already went through it are
/// kept. The stored credential and webhook secret die with
/// the row.
pub(crate) async fn delete_provider(
    state: web::Data<SmsMarketingState>,
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
        let provider = load_provider(&state, &tenant_id, &path.into_inner())?;
        let referenced = state.campaigns.iter().any(|entry| {
            entry.value().tenant_id == tenant_id && entry.value().provider_id == provider.id
        });
        if referenced {
            return Err(UnifiedError::conflict(
                "Provider is referenced by a campaign; delete or re-point the campaign first",
            ));
        }
        state.providers.remove(&provider.id);
        record_audit(
            &state,
            &tenant_id,
            "provider",
            &provider.id.to_string(),
            "delete",
            &caller.actor,
            Some(provider.public_view()),
            None,
        );
        Ok(json!({ "deleted": true, "provider_id": provider.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Rotate a provider's webhook secret (manager): generates
/// a fresh secret, stores only its hash, and returns the raw
/// secret exactly once. The old secret stops working at once.
pub(crate) async fn rotate_provider_webhook_secret(
    state: web::Data<SmsMarketingState>,
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
        let mut provider = load_provider(&state, &tenant_id, &path.into_inner())?;
        let secret = generate_webhook_secret();
        provider.webhook_secret_hash = Some(hash_webhook_secret(&secret));
        provider.updated_at = Utc::now();
        state.providers.insert(provider.id, provider.clone());
        record_audit(
            &state,
            &tenant_id,
            "provider",
            &provider.id.to_string(),
            "rotate_webhook_secret",
            &caller.actor,
            None,
            Some(provider.public_view()),
        );
        Ok(json!({
            "provider_id": provider.id.to_string(),
            "webhook_secret": secret,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Dry-run check of a provider's configuration (manager):
/// validates the endpoint, the auth type, and whether a
/// credential is stored. This makes NO network call and
/// sends NO message — a live probe would spend credit, so
/// reachability is proven by the first real campaign send,
/// not here.
pub(crate) async fn test_provider(
    state: web::Data<SmsMarketingState>,
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
        let provider = load_provider(&state, &tenant_id, &path.into_inner())?;
        if !provider.enabled {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" is disabled",
                provider.name
            )));
        }
        let mut problems: Vec<String> = Vec::new();
        if provider.kind == super::types::ProviderKind::Http {
            match &provider.endpoint_url {
                Some(url) if super::types::is_https_url(url) => {}
                _ => problems.push("endpoint_url must be an HTTPS URL".to_string()),
            }
        }
        if provider.auth_type.needs_credential() && provider.credential_encrypted.is_none() {
            problems.push("auth type needs a credential; none is stored".to_string());
        }
        if provider.auth_type == super::types::AuthType::Basic
            && provider.auth_username.as_deref().unwrap_or("").is_empty()
        {
            problems.push("basic auth needs auth_username".to_string());
        }
        // A stored credential must still decrypt with the
        // current secret; a rotated app secret breaks old
        // rows, and that must surface here, not at send time.
        let credential_readable = match &provider.credential_encrypted {
            Some(stored) => match encryption_secret(&ctx) {
                Ok(secret) => decrypt_credential(&secret, stored).is_ok(),
                Err(_) => false,
            },
            None => true,
        };
        if !credential_readable {
            problems.push(
                "stored credential cannot be decrypted with the current app secret".to_string(),
            );
        }
        Ok(json!({
            "provider_id": provider.id.to_string(),
            "kind": provider.kind.as_str(),
            "endpoint_configured": provider.endpoint_url.is_some(),
            "key_configured": provider.credential_encrypted.is_some(),
            "credential_readable": credential_readable,
            "webhook_secret_configured": provider.webhook_secret_hash.is_some(),
            "ready": problems.is_empty(),
            "problems": problems,
            "note": "Dry-run only: no network call was made and no message was sent.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
