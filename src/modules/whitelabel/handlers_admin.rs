// Whitelabel module - admin handlers.
//
// Split from `src/modules/whitelabel.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;

use super::constants::{EVENT_ALLOWANCE_CHANGED, EVENT_DEFAULTS_CHANGED, MAX_NAME_LEN, MAX_TAGLINE_LEN, PERM_MANAGE};
use super::helpers::{caller, err, ok, page_of, parse_company_id, require_permission};
use super::state::{WhitelabelState};
use super::types::{AllowanceMode, BrandingProfile, CompanyAllowance, GlobalDefaults, HistoryEntry, PutCompanyAllowanceInput, PutDefaultsInput, PutGlobalAllowanceInput};
use super::validation::{apply_storage_pair, require_email, require_hex_color, require_storage_pair, require_text};

// ---------------------------------------------------------------------------
// Admin handlers: allowance
// ---------------------------------------------------------------------------

/// List the global allowance plus every per-company override
/// (paginated, filter by `company_id` or `mode`, search by
/// company id). Admin only.
pub(crate) async fn get_admin_allowance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let mode_filter = params.filters.get("mode").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut overrides: Vec<CompanyAllowance> = state
            .company_allowances
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                if let Some(company_id) = &company_filter {
                    if record.company_id != *company_id {
                        return false;
                    }
                }
                if let Some(mode) = &mode_filter {
                    if record.mode.as_str() != mode.as_str() {
                        return false;
                    }
                }
                if !search.is_empty() && !record.company_id.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        overrides.sort_by(|a, b| a.company_id.cmp(&b.company_id));
        let page = page_of(overrides, &params);
        let global = state.global_allowance.read().expect("allowance lock poisoned");
        Ok(json!({
            "global": {
                "allow_whitelabel": global.allow_whitelabel,
                "updated_by": global.updated_by,
                "updated_at": global.updated_at,
            },
            "overrides": page,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Set the global default allowance. Admin only. Emits
/// `whitelabel.allowance.changed` and records history.
pub(crate) async fn put_global_allowance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    body: web::Json<PutGlobalAllowanceInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let input = body.into_inner();
        let mut global = state
            .global_allowance
            .write()
            .expect("allowance lock poisoned");
        let old = global.allow_whitelabel;
        global.allow_whitelabel = input.allow_whitelabel;
        global.updated_by = Some(caller.actor.clone());
        global.updated_at = Utc::now();
        let updated = global.clone();
        drop(global);
        state.record_history(
            "global",
            &None,
            &caller.actor,
            "allowance.global_changed",
            "allow_whitelabel",
            Some(old.to_string()),
            Some(input.allow_whitelabel.to_string()),
            true,
        );
        ctx.hooks.emit(
            EVENT_ALLOWANCE_CHANGED,
            json!({
                "scope": "global",
                "allow_whitelabel": input.allow_whitelabel,
                "actor": caller.actor,
            }),
        );
        Ok(json!({
            "allow_whitelabel": updated.allow_whitelabel,
            "updated_by": updated.updated_by,
            "updated_at": updated.updated_at,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Effective allowance for one company (global default resolved
/// through the per-company override). Admin only.
pub(crate) async fn get_company_allowance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    company_id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = parse_company_id(&company_id.into_inner())?;
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

/// Set the per-company allowance override (`allowed` / `denied` /
/// `inherit`). Admin only. Emits `whitelabel.allowance.changed`
/// and records history.
pub(crate) async fn put_company_allowance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    company_id: web::Path<String>,
    body: web::Json<PutCompanyAllowanceInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = parse_company_id(&company_id.into_inner())?;
        let mode = AllowanceMode::parse(body.into_inner().mode.trim())?;
        let old = state.company_mode(&company_id).map(|mode| mode.as_str().to_string());
        state.company_allowances.insert(
            company_id.clone(),
            CompanyAllowance {
                company_id: company_id.clone(),
                mode,
                updated_by: Some(caller.actor.clone()),
                updated_at: Utc::now(),
            },
        );
        state.record_history(
            &company_id,
            &None,
            &caller.actor,
            "allowance.company_changed",
            "mode",
            old,
            Some(mode.as_str().to_string()),
            true,
        );
        ctx.hooks.emit(
            EVENT_ALLOWANCE_CHANGED,
            json!({
                "scope": "company",
                "company_id": company_id,
                "mode": mode.as_str(),
                "allowed": state.effective_allowance(&company_id),
                "actor": caller.actor,
            }),
        );
        Ok(json!({
            "company_id": company_id,
            "mode": mode.as_str(),
            "allowed": state.effective_allowance(&company_id),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Admin handlers: global defaults record
// ---------------------------------------------------------------------------

/// Read the admin-editable global defaults record. Admin only.
pub(crate) async fn get_admin_defaults(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GlobalDefaults> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        Ok(state.global_defaults())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Partial update of the admin-editable global defaults record.
/// Admin only. Every field is validated the same way tenant fields
/// are (hex colours, email address, storage pairs together). Emits
/// `whitelabel.defaults.changed` and records history.
pub(crate) async fn put_admin_defaults(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    body: web::Json<PutDefaultsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let input = body.into_inner();
        // Validate everything before touching the stored record.
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
        let sender_name = match &input.sender_name {
            Some(raw) => Some(require_text(raw, "sender_name", MAX_NAME_LEN)?),
            None => None,
        };
        let sender_email = match &input.sender_email {
            Some(raw) if raw.trim().is_empty() => Some(String::new()),
            Some(raw) => Some(require_email(raw, "sender_email")?),
            None => None,
        };
        let login_tagline = match &input.login_tagline {
            Some(raw) => Some(require_text(raw, "login_tagline", MAX_TAGLINE_LEN)?),
            None => None,
        };
        let powered_by_text = match &input.powered_by_text {
            Some(raw) => Some(require_text(raw, "powered_by_text", MAX_NAME_LEN)?),
            None => None,
        };
        let (logo_target, logo_key) = require_storage_pair(
            &input.logo_storage_target,
            &input.logo_storage_key,
            "logo",
        )?;
        let (favicon_target, favicon_key) = require_storage_pair(
            &input.favicon_storage_target,
            &input.favicon_storage_key,
            "favicon",
        )?;
        let (login_bg_target, login_bg_key) = require_storage_pair(
            &input.login_background_storage_target,
            &input.login_background_storage_key,
            "login_background",
        )?;
        let mut defaults = state.defaults.write().expect("defaults lock poisoned");
        let mut changed: Vec<String> = Vec::new();
        if let Some(value) = display_name {
            defaults.display_name = value;
            changed.push("display_name".to_string());
        }
        if let Some(value) = primary_color {
            defaults.primary_color = value;
            changed.push("primary_color".to_string());
        }
        if let Some(value) = accent_color {
            defaults.accent_color = value;
            changed.push("accent_color".to_string());
        }
        if let Some(value) = background_color {
            defaults.background_color = value;
            changed.push("background_color".to_string());
        }
        if let Some(value) = text_color {
            defaults.text_color = value;
            changed.push("text_color".to_string());
        }
        if let Some(value) = sender_name {
            defaults.sender_name = value;
            changed.push("sender_name".to_string());
        }
        if let Some(value) = sender_email {
            defaults.sender_email = value;
            changed.push("sender_email".to_string());
        }
        if let Some(value) = login_tagline {
            defaults.login_tagline = value;
            changed.push("login_tagline".to_string());
        }
        if let Some(value) = powered_by_text {
            defaults.powered_by_text = value;
            changed.push("powered_by_text".to_string());
        }
        {
            let GlobalDefaults {
                logo_storage_target,
                logo_storage_key,
                ..
            } = &mut *defaults;
            apply_storage_pair(
                logo_storage_target,
                logo_storage_key,
                logo_target,
                logo_key,
            );
        }
        if input.logo_storage_target.is_some() || input.logo_storage_key.is_some() {
            changed.push("logo".to_string());
        }
        {
            let GlobalDefaults {
                favicon_storage_target,
                favicon_storage_key,
                ..
            } = &mut *defaults;
            apply_storage_pair(
                favicon_storage_target,
                favicon_storage_key,
                favicon_target,
                favicon_key,
            );
        }
        if input.favicon_storage_target.is_some() || input.favicon_storage_key.is_some() {
            changed.push("favicon".to_string());
        }
        {
            let GlobalDefaults {
                login_background_storage_target,
                login_background_storage_key,
                ..
            } = &mut *defaults;
            apply_storage_pair(
                login_background_storage_target,
                login_background_storage_key,
                login_bg_target,
                login_bg_key,
            );
        }
        if input.login_background_storage_target.is_some()
            || input.login_background_storage_key.is_some()
        {
            changed.push("login_background".to_string());
        }
        let updated = defaults.clone();
        drop(defaults);
        state.record_history(
            "global",
            &None,
            &caller.actor,
            "defaults.changed",
            "*",
            None,
            Some(changed.join(",")),
            true,
        );
        ctx.hooks.emit(
            EVENT_DEFAULTS_CHANGED,
            json!({ "actor": caller.actor, "fields": changed }),
        );
        Ok(json!(updated))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Admin handlers: profiles + history
// ---------------------------------------------------------------------------

/// List all branding profiles (admin). Paginated, filter by
/// `company_id`, `tenant_id`, or `domain_status`, search by company
/// id, tenant id, display name, or custom domain.
pub(crate) async fn list_admin_profiles(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let tenant_filter = params.filters.get("tenant_id").cloned();
        let status_filter = params.filters.get("domain_status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut profiles: Vec<BrandingProfile> = state
            .profiles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|profile| {
                if let Some(company_id) = &company_filter {
                    if profile.company_id != *company_id {
                        return false;
                    }
                }
                if let Some(tenant_id) = &tenant_filter {
                    if profile.tenant_id.as_deref() != Some(tenant_id.as_str()) {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if profile.domain_status != *status {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        profile.company_id,
                        profile.tenant_id.clone().unwrap_or_default(),
                        profile.display_name.clone().unwrap_or_default(),
                        profile.custom_domain.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        profiles.sort_by(|a, b| {
            a.company_id
                .cmp(&b.company_id)
                .then_with(|| a.tenant_id.cmp(&b.tenant_id))
        });
        Ok(page_of(profiles, &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Get one company's branding profile (admin). `?tenant_id=` selects
/// the tenant override; without it the company default is returned.
pub(crate) async fn get_admin_profile(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    company_id: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BrandingProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let company_id = parse_company_id(&company_id.into_inner())?;
        let tenant_id = query.filters.get("tenant_id").cloned();
        state.get_profile(&company_id, &tenant_id).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "No branding profile for company \"{company_id}\""
            ))
        })
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List the full branding history (admin). Paginated, filter by
/// `company_id`, `action`, or `allowed`, search by actor or field.
pub(crate) async fn list_admin_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WhitelabelState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let params = query.into_inner();
        Ok(page_of(filter_history(&state, &params, None), &params))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Shared history filtering for the admin and tenant history lists.
/// `company_scope` restricts to one company (tenant list); `None`
/// means all companies (admin list).
pub(crate) fn filter_history(
    state: &WhitelabelState,
    params: &PaginationParams,
    company_scope: Option<&str>,
) -> Vec<HistoryEntry> {
    let company_filter = params.filters.get("company_id").cloned();
    let action_filter = params.filters.get("action").cloned();
    let allowed_filter = params.filters.get("allowed").cloned();
    let search = params.search.clone().unwrap_or_default().to_lowercase();
    let mut entries: Vec<HistoryEntry> = state
        .history
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            if let Some(scope) = company_scope {
                if entry.company_id != scope {
                    return false;
                }
            }
            if let Some(company_id) = &company_filter {
                if entry.company_id != *company_id {
                    return false;
                }
            }
            if let Some(action) = &action_filter {
                if entry.action != *action {
                    return false;
                }
            }
            if let Some(allowed) = &allowed_filter {
                let wanted = allowed == "true";
                if entry.allowed != wanted {
                    return false;
                }
            }
            if !search.is_empty() {
                let haystack = format!("{} {} {}", entry.actor, entry.field, entry.action)
                    .to_lowercase();
                if !haystack.contains(&search) {
                    return false;
                }
            }
            true
        })
        .collect();
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    entries
}
