// Whitelabel module - tenant branding handlers.
//
// Split from `src/modules/whitelabel.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json::error_response;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;

use super::constants::{DOMAIN_NONE, DOMAIN_REQUESTED, EVENT_BRANDING_CHANGED, EVENT_BRANDING_DENIED, EVENT_BRANDING_RESET, EVENT_DOMAIN_REQUESTED, EVENT_SENDER_REQUESTED, MAX_NAME_LEN, MAX_TAGLINE_LEN, PERM_BRANDING_MANAGE, PERM_VIEW};
use super::handlers_admin::{filter_history};
use super::helpers::{caller, caller_company, caller_tenant, err, ok, page_of, parse_company_id, require_permission};
use super::state::{WhitelabelState};
use super::types::{BrandingProfile, PutBrandingInput, ResolvedBranding};
use super::validation::{apply_storage_pair, require_domain, require_email, require_hex_color, require_storage_pair, require_text};

// ---------------------------------------------------------------------------
// Tenant handlers
// ---------------------------------------------------------------------------

/// The caller's own effective allowance. Tenant read capability.
pub(crate) async fn get_my_allowance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = caller_company(&req)?;
        let mode = state.company_mode(&company_id);
        Ok(json!({
            "company_id": company_id,
            "mode": mode.map(|mode| mode.as_str()),
            "allowed": state.effective_allowance(&company_id),
            "global_allow_whitelabel": state.global_allow(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// The caller's own server-resolved branding. Enforcement happens
/// here, on the server: when the tenant is not allowed, the resolved
/// branding is the global defaults record in full. Tenant read
/// capability.
pub(crate) async fn get_my_branding(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ResolvedBranding> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = caller_company(&req)?;
        let tenant_id = caller_tenant(&req)?;
        Ok(state.resolve_for(&company_id, &tenant_id))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Save the caller's own branding profile. When white labelling is
/// NOT allowed for the tenant, the write is rejected with 403 and a
/// `whitelabel.branding.denied` audit event is emitted plus a history
/// entry — the attempt itself is part of the audit trail. A new
/// custom domain is stored as `requested` and a
/// `whitelabel.domain.requested` event is emitted for the domains
/// module; only a VERIFIED domain is ever attached as the resolved
/// custom domain (the resolver reports the stored status, and
/// renderers must only use a domain whose status is `verified`).
pub(crate) async fn put_my_branding(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    body: web::Json<PutBrandingInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BrandingProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BRANDING_MANAGE)?;
        let company_id = caller_company(&req)?;
        let tenant_id = caller_tenant(&req)?;
        let input = body.into_inner();
        // Enforcement first: no custom branding without permission.
        if !state.effective_allowance(&company_id) {
            let fields = branding_input_fields(&input);
            state.record_history(
                &company_id,
                &tenant_id,
                &caller.actor,
                "branding.denied",
                "*",
                None,
                Some(fields.join(",")),
                false,
            );
            ctx.hooks.emit(
                EVENT_BRANDING_DENIED,
                json!({
                    "company_id": company_id,
                    "tenant_id": tenant_id,
                    "actor": caller.actor,
                    "fields": fields,
                }),
            );
            return Err(UnifiedError::forbidden(
                "White labelling is not allowed for this company: Unified branding applies",
            ));
        }
        // Validate everything before writing anything.
        let display_name = match &input.display_name {
            Some(raw) => Some(require_text(raw, "display_name", MAX_NAME_LEN)?),
            None => None,
        };
        let primary_color = match &input.primary_color {
            Some(raw) => Some(require_hex_color(raw, "primary_color")?),
            None => None,
        };
        let accent_color = match &input.accent_color {
            Some(raw) => Some(require_hex_color(raw, "accent_color")?),
            None => None,
        };
        let background_color = match &input.background_color {
            Some(raw) => Some(require_hex_color(raw, "background_color")?),
            None => None,
        };
        let text_color = match &input.text_color {
            Some(raw) => Some(require_hex_color(raw, "text_color")?),
            None => None,
        };
        let custom_domain = match &input.custom_domain {
            Some(Some(raw)) => Some(Some(require_domain(raw)?)),
            Some(None) => Some(None),
            None => None,
        };
        let sender_name = match &input.sender_name {
            Some(raw) => Some(require_text(raw, "sender_name", MAX_NAME_LEN)?),
            None => None,
        };
        let sender_email = match &input.sender_email {
            Some(raw) => Some(require_email(raw, "sender_email")?),
            None => None,
        };
        let login_tagline = match &input.login_tagline {
            Some(raw) => Some(require_text(raw, "login_tagline", MAX_TAGLINE_LEN)?),
            None => None,
        };
        let (logo_target, logo_key) =
            require_storage_pair(&input.logo_storage_target, &input.logo_storage_key, "logo")?;
        let (favicon_target, favicon_key) = require_storage_pair(
            &input.favicon_storage_target,
            &input.favicon_storage_key,
            "favicon",
        )?;
        let (login_logo_target, login_logo_key) = require_storage_pair(
            &input.login_logo_storage_target,
            &input.login_logo_storage_key,
            "login_logo",
        )?;
        let (login_bg_target, login_bg_key) = require_storage_pair(
            &input.login_background_storage_target,
            &input.login_background_storage_key,
            "login_background",
        )?;
        let key = WhitelabelState::profile_key(&company_id, &tenant_id);
        let mut profile = state
            .profiles
            .get(&key)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| BrandingProfile::new(&company_id, tenant_id.clone()));
        let mut changed: Vec<String> = Vec::new();
        if let Some(value) = display_name {
            profile.display_name = Some(value);
            changed.push("display_name".to_string());
        }
        if let Some(value) = primary_color {
            profile.primary_color = Some(value);
            changed.push("primary_color".to_string());
        }
        if let Some(value) = accent_color {
            profile.accent_color = Some(value);
            changed.push("accent_color".to_string());
        }
        if let Some(value) = background_color {
            profile.background_color = Some(value);
            changed.push("background_color".to_string());
        }
        if let Some(value) = text_color {
            profile.text_color = Some(value);
            changed.push("text_color".to_string());
        }
        if let Some(value) = sender_name {
            profile.sender_name = Some(value);
            changed.push("sender_name".to_string());
        }
        if let Some(value) = sender_email {
            profile.sender_email = Some(value);
            changed.push("sender_email".to_string());
        }
        if let Some(value) = login_tagline {
            profile.login_tagline = Some(value);
            changed.push("login_tagline".to_string());
        }
        if let Some(value) = input.hide_powered_by {
            profile.hide_powered_by = value;
            changed.push("hide_powered_by".to_string());
        }
        apply_storage_pair(
            &mut profile.logo_storage_target,
            &mut profile.logo_storage_key,
            logo_target,
            logo_key,
        );
        if input.logo_storage_target.is_some() || input.logo_storage_key.is_some() {
            changed.push("logo".to_string());
        }
        apply_storage_pair(
            &mut profile.favicon_storage_target,
            &mut profile.favicon_storage_key,
            favicon_target,
            favicon_key,
        );
        if input.favicon_storage_target.is_some() || input.favicon_storage_key.is_some() {
            changed.push("favicon".to_string());
        }
        apply_storage_pair(
            &mut profile.login_logo_storage_target,
            &mut profile.login_logo_storage_key,
            login_logo_target,
            login_logo_key,
        );
        if input.login_logo_storage_target.is_some() || input.login_logo_storage_key.is_some() {
            changed.push("login_logo".to_string());
        }
        apply_storage_pair(
            &mut profile.login_background_storage_target,
            &mut profile.login_background_storage_key,
            login_bg_target,
            login_bg_key,
        );
        if input.login_background_storage_target.is_some()
            || input.login_background_storage_key.is_some()
        {
            changed.push("login_background".to_string());
        }
        // Custom domain: a changed domain restarts verification.
        // Only a VERIFIED domain may be attached; until the domains
        // module verifies it, the status stays `requested`.
        let mut domain_changed = false;
        if let Some(value) = custom_domain {
            if profile.custom_domain != value {
                domain_changed = value.is_some();
                profile.custom_domain = value;
                profile.domain_status = if domain_changed {
                    DOMAIN_REQUESTED.to_string()
                } else {
                    DOMAIN_NONE.to_string()
                };
                changed.push("custom_domain".to_string());
            }
        }
        profile.updated_at = Utc::now();
        state.profiles.insert(key, profile.clone());
        state.record_history(
            &company_id,
            &tenant_id,
            &caller.actor,
            "branding.updated",
            "*",
            None,
            Some(changed.join(",")),
            true,
        );
        ctx.hooks.emit(
            EVENT_BRANDING_CHANGED,
            json!({
                "company_id": company_id,
                "tenant_id": tenant_id,
                "actor": caller.actor,
                "fields": changed,
            }),
        );
        if domain_changed {
            let domain = profile.custom_domain.clone().unwrap_or_default();
            ctx.hooks.emit(
                EVENT_DOMAIN_REQUESTED,
                json!({
                    "company_id": company_id,
                    "tenant_id": tenant_id,
                    "domain": domain,
                }),
            );
            state.record_history(
                &company_id,
                &tenant_id,
                &caller.actor,
                "domain.requested",
                "custom_domain",
                None,
                Some(domain),
                true,
            );
        }
        if profile.sender_email.is_some() || profile.sender_name.is_some() {
            ctx.hooks.emit(
                EVENT_SENDER_REQUESTED,
                json!({
                    "company_id": company_id,
                    "tenant_id": tenant_id,
                    "sender_name": profile.sender_name,
                    "sender_email": profile.sender_email,
                }),
            );
        }
        Ok(profile)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Field names present in a branding write, for the denied-attempt
/// audit record (no values: the attempt is logged, not the content).
pub(crate) fn branding_input_fields(input: &PutBrandingInput) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    if input.display_name.is_some() {
        fields.push("display_name".to_string());
    }
    if input.logo_storage_target.is_some() || input.logo_storage_key.is_some() {
        fields.push("logo".to_string());
    }
    if input.favicon_storage_target.is_some() || input.favicon_storage_key.is_some() {
        fields.push("favicon".to_string());
    }
    if input.primary_color.is_some() {
        fields.push("primary_color".to_string());
    }
    if input.accent_color.is_some() {
        fields.push("accent_color".to_string());
    }
    if input.background_color.is_some() {
        fields.push("background_color".to_string());
    }
    if input.text_color.is_some() {
        fields.push("text_color".to_string());
    }
    if input.custom_domain.is_some() {
        fields.push("custom_domain".to_string());
    }
    if input.hide_powered_by.is_some() {
        fields.push("hide_powered_by".to_string());
    }
    if input.sender_name.is_some() {
        fields.push("sender_name".to_string());
    }
    if input.sender_email.is_some() {
        fields.push("sender_email".to_string());
    }
    if input.login_logo_storage_target.is_some() || input.login_logo_storage_key.is_some() {
        fields.push("login_logo".to_string());
    }
    if input.login_background_storage_target.is_some()
        || input.login_background_storage_key.is_some()
    {
        fields.push("login_background".to_string());
    }
    if input.login_tagline.is_some() {
        fields.push("login_tagline".to_string());
    }
    fields
}

/// Reset the caller's own branding profile to the platform defaults
/// (deletes the profile). Removing custom branding is always safe,
/// so this is allowed even when white labelling is not permitted:
/// it can only move the tenant towards the default branding.
/// Audit-logged and emits `whitelabel.branding.reset`.
pub(crate) async fn delete_my_branding(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BRANDING_MANAGE)?;
        let company_id = caller_company(&req)?;
        let tenant_id = caller_tenant(&req)?;
        let key = WhitelabelState::profile_key(&company_id, &tenant_id);
        match state.profiles.remove(&key) {
            Some(_) => {
                state.record_history(
                    &company_id,
                    &tenant_id,
                    &caller.actor,
                    "branding.reset",
                    "*",
                    None,
                    Some("reset to platform defaults".to_string()),
                    true,
                );
                ctx.hooks.emit(
                    EVENT_BRANDING_RESET,
                    json!({
                        "company_id": company_id,
                        "tenant_id": tenant_id,
                        "actor": caller.actor,
                    }),
                );
                Ok(json!({
                    "deleted": true,
                    "company_id": company_id,
                    "tenant_id": tenant_id,
                }))
            }
            None => Err(UnifiedError::not_found(format!(
                "No branding profile for company \"{company_id}\""
            ))),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// The caller's own branding history (paginated, filter by `action`
/// or `allowed`, search by actor or field). Scoped to the caller's
/// company: a tenant never sees another company's history.
pub(crate) async fn list_my_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW)?;
        let company_id = caller_company(&req)?;
        let params = query.into_inner();
        Ok(page_of(
            filter_history(&state, &params, Some(&company_id)),
            &params,
        ))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public handler (API Manager ApiAccessRule gated, like portals)
// ---------------------------------------------------------------------------

/// Server-resolved branding for one company's login / portal page
/// render. No capability check on purpose: the API Manager denies
/// the call unless an administrator created the matching
/// `ApiAccessRule` for `GET /api/v1/whitelabel/public/*` (default
/// deny). Only resolved branding is returned — never allowance
/// settings, history, or another tenant's data — and a tenant that
/// is not allowed resolves to the platform defaults.
pub(crate) async fn get_public_branding(
    state: web::Data<WhitelabelState>,
    company_id: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(company_id) => company_id,
        Err(error) => return error_response(&error, false),
    };
    let tenant_id = query.filters.get("tenant_id").cloned();
    ok(state.resolve_for(&company_id, &tenant_id))
}
