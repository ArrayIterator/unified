//! Portals - caller checks, validation and loaders.
//!
//! Part of the `portals` module split; see `src/modules/portals.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PageResult;
use unified_core::pagination::PaginationParams;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use uuid::Uuid;

use super::constants::MAX_DOMAIN_LEN;
use super::constants::MAX_ROLE_LEN;
use super::constants::MAX_ROLES_PER_MENU;
use super::constants::MAX_SLUG_LEN;
use super::constants::MAX_URL_LEN;
use super::constants::MAX_VIEWS_PER_PORTAL;
use super::constants::SECTION_NAMES;
use super::state::PortalsState;
use super::types::Portal;
use super::types::PortalMenu;
use super::types::PortalView;

// Helpers

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

// Tenant edge guard (C3 pilot 3).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
/// The one exception is `get_public_portal`: the unauthenticated
/// public portal render, which stays ungated by design (see the
/// module header - the API Manager gate makes it public).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// - a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here.
pub(crate) fn caller_company_scope(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// The caller scope's company, required: client-data endpoints
/// cannot scope without a company, so an unresolved caller is
/// rejected (401) instead of reading unscoped data.
pub(crate) fn require_caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company_scope(tenant, req)?.ok_or_else(|| {
        UnifiedError::unauthorized("No company for the verified caller")
    })
}

/// Verify a request body's `company_id` (required `String`) against
/// the caller's company scope and return the company id to store.
/// Fails closed (403) when the body names a company other than the
/// caller's, and when the caller has no resolved company (an
/// unresolved caller may not claim a company - the shared
/// `CallerScope::Unresolved` rule). Portals always carry a company,
/// so there is no "omitted adopts scope" case here.
pub(crate) fn require_company_match_str(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &str,
) -> UnifiedResult<String> {
    let caller = caller_company_scope(tenant, req)?;
    match &caller {
        Some(company) if company == body_company_id => Ok(company.clone()),
        _ => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
    }
}

/// True when a stored record with a REQUIRED company id belongs to
/// the caller's company scope. Portals, menus (via their portal)
/// and records always carry a company (there are no global rows),
/// so an unresolved caller sees none of them and a scoped caller
/// sees only their own company's rows.
pub(crate) fn in_company_scope_str(
    record_company_id: &str,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => false,
        Some(company) => record_company_id == company,
    }
}

/// Fail closed (403) when a stored record with a required company
/// id is outside the caller's company scope.
pub(crate) fn require_record_in_company_scope_str(
    record_company_id: &str,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope_str(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope FOR A WRITE (C3, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records - global (unscoped) records are platform-only
/// - and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(crate) fn require_record_writable_in_company_scope_str(
    record_company_id: &str,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match caller_company {
        Some(caller)
            if !record_company_id.is_empty() && record_company_id == caller.as_str() =>
        {
            Ok(())
        }
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// A slug is lowercase letters, digits, and single hyphens only,
/// never leading or trailing with a hyphen, at most 128 chars.
pub(crate) fn valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in title.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("portal");
    }
    out.truncate(MAX_SLUG_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// A brand colour is `#RGB` or `#RRGGBB` hex, nothing else.
fn valid_hex_color(color: &str) -> bool {
    let digits = color.strip_prefix('#').unwrap_or("");
    (digits.len() == 3 || digits.len() == 6) && digits.chars().all(|c| c.is_ascii_hexdigit())
}

pub(crate) fn require_hex_color(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if !valid_hex_color(trimmed) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a hex colour such as \"#0B6B3A\" or \"#FFF\""
        )));
    }
    Ok(trimmed.to_uppercase())
}

/// A custom domain is a hostname: dot-separated labels of letters,
/// digits, and hyphens, never leading or trailing with a hyphen or
/// a dot, at least one dot, at most 253 chars. No scheme, port, or
/// path is allowed.
pub(crate) fn valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > MAX_DOMAIN_LEN {
        return false;
    }
    if domain.starts_with('.')
        || domain.ends_with('.')
        || domain.starts_with('-')
        || domain.ends_with('-')
        || !domain.contains('.')
    {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// A menu URL target is an `http://` or `https://` URL, at most
/// 2048 chars, with no whitespace.
fn valid_url(url: &str) -> bool {
    if url.is_empty() || url.len() > MAX_URL_LEN || url.chars().any(char::is_whitespace) {
        return false;
    }
    url.starts_with("http://") || url.starts_with("https://")
}

/// Validate a section list: every entry must be a known section,
/// no duplicates. An empty list is allowed (a portal with no
/// visible sections).
pub(crate) fn validate_sections(sections: &[String]) -> UnifiedResult<()> {
    if sections.len() > SECTION_NAMES.len() {
        return Err(UnifiedError::bad_request(format!(
            "at most {} sections are allowed",
            SECTION_NAMES.len()
        )));
    }
    let mut seen: Vec<&str> = Vec::new();
    for section in sections {
        if !SECTION_NAMES.contains(&section.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown portal section \"{section}\" (expected one of: {})",
                SECTION_NAMES.join(", ")
            )));
        }
        if seen.contains(&section.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate portal section \"{section}\""
            )));
        }
        seen.push(section.as_str());
    }
    Ok(())
}

pub(crate) fn validate_menu_target(target_kind: &str, target: &str) -> UnifiedResult<()> {
    match target_kind {
        "section" => {
            if !SECTION_NAMES.contains(&target) {
                return Err(UnifiedError::bad_request(format!(
                    "Menu target \"{target}\" is not a known portal section (expected one of: {})",
                    SECTION_NAMES.join(", ")
                )));
            }
            Ok(())
        }
        "url" => {
            if !valid_url(target) {
                return Err(UnifiedError::bad_request(
                    "Menu URL target must start with \"http://\" or \"https://\" and contain no whitespace",
                ));
            }
            Ok(())
        }
        other => Err(UnifiedError::bad_request(format!(
            "Menu target_kind must be \"section\" or \"url\", not \"{other}\""
        ))),
    }
}

pub(crate) fn validate_roles(roles: &[String]) -> UnifiedResult<()> {
    if roles.len() > MAX_ROLES_PER_MENU {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_ROLES_PER_MENU} roles are allowed per menu entry"
        )));
    }
    let mut seen: Vec<&str> = Vec::new();
    for role in roles {
        let trimmed = role.trim();
        if trimmed.is_empty() || trimmed.len() > MAX_ROLE_LEN {
            return Err(UnifiedError::bad_request(
                "menu roles must not be empty and must be at most 80 characters",
            ));
        }
        if seen.contains(&trimmed) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate menu role \"{trimmed}\""
            )));
        }
        seen.push(trimmed);
    }
    Ok(())
}

pub(crate) fn slug_in_use(state: &PortalsState, slug: &str, except: Option<Uuid>) -> bool {
    state
        .portals
        .iter()
        .any(|entry| entry.value().slug == slug && Some(entry.value().id) != except)
}

pub(crate) fn domain_in_use(state: &PortalsState, domain: &str, except: Option<Uuid>) -> bool {
    state.portals.iter().any(|entry| {
        entry.value().custom_domain.as_deref() == Some(domain) && Some(entry.value().id) != except
    })
}

/// Apply a custom-domain change to a portal and report whether the
/// domain actually changed. `domain_status` is only reset - to
/// `requested` for a new domain, `none` when the domain is removed
/// - when the value changed; re-saving the same domain leaves a
/// `verified` (or `rejected`) status untouched and must not trigger
/// a fresh verification request.
pub(crate) fn apply_custom_domain(portal: &mut Portal, new_domain: Option<String>) -> bool {
    if portal.custom_domain == new_domain {
        return false;
    }
    portal.custom_domain = new_domain;
    portal.domain_status = if portal.custom_domain.is_some() {
        "requested".to_string()
    } else {
        "none".to_string()
    };
    true
}

/// Apply a domain verification outcome to a portal. Only a portal
/// whose `domain_status` is `requested` may move, and only to
/// `verified` or `rejected`; anything else is a conflict or a bad
/// request. When `domain` is given it must match the portal's
/// requested domain, so a result for one domain cannot flip the
/// status of a portal that asked for another. This is the write-back
/// path for the domains module's verification result until it grows
/// a listener on `portals.domain.requested`.
pub(crate) fn apply_domain_verification(
    portal: &mut Portal,
    status: &str,
    domain: Option<&str>,
) -> UnifiedResult<()> {
    let status = status.trim().to_lowercase();
    if status != "verified" && status != "rejected" {
        return Err(UnifiedError::bad_request(
            "domain status must be \"verified\" or \"rejected\"",
        ));
    }
    let Some(current_domain) = portal.custom_domain.clone() else {
        return Err(UnifiedError::bad_request(
            "portal has no custom domain to verify",
        ));
    };
    if let Some(domain) = domain {
        let domain = domain.trim().to_lowercase();
        if domain != current_domain {
            return Err(UnifiedError::bad_request(format!(
                "verification result is for \"{domain}\", but this portal requested \"{current_domain}\""
            )));
        }
    }
    if portal.domain_status != "requested" {
        return Err(UnifiedError::conflict(format!(
            "domain_status is already \"{}\"; only a \"requested\" domain can be verified or rejected",
            portal.domain_status
        )));
    }
    portal.domain_status = status;
    portal.updated_at = Utc::now();
    Ok(())
}

pub(crate) fn load_portal(state: &PortalsState, raw_id: &str) -> UnifiedResult<Portal> {
    let id = parse_uuid(raw_id, "portal")?;
    state
        .portals
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Portal not found: {id}")))
}

pub(crate) fn load_portal_by_slug(state: &PortalsState, slug: &str) -> UnifiedResult<Portal> {
    state
        .portals
        .iter()
        .map(|entry| entry.value().clone())
        .find(|portal| portal.slug == slug)
        .ok_or_else(|| UnifiedError::not_found(format!("Portal not found: {slug}")))
}

pub(crate) fn load_menu(
    state: &PortalsState,
    portal_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<PortalMenu> {
    let id = parse_uuid(raw_id, "menu")?;
    state
        .menus
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|menu| menu.portal_id == portal_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Menu entry not found: {id}")))
}

pub(crate) fn menus_for(state: &PortalsState, portal_id: Uuid) -> Vec<PortalMenu> {
    let mut items: Vec<PortalMenu> = state
        .menus
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|menu| menu.portal_id == portal_id)
        .collect();
    items.sort_by_key(|menu| (menu.position, menu.created_at));
    items
}

/// A menu entry is visible to a client role when it is enabled and
/// its role list is empty (every role) or contains the role.
fn menu_visible_to(menu: &PortalMenu, role: &str) -> bool {
    menu.visible && (menu.roles.is_empty() || menu.roles.iter().any(|entry| entry == role))
}

/// Map a portal section name (plural, as stored on the portal)
/// to the projection record kind (singular, as stored on the
/// record).
pub(crate) fn section_kind(section: &str) -> &'static str {
    match section {
        "invoices" => "invoice",
        "tickets" => "ticket",
        "downloads" => "download",
        "subscriptions" => "subscription",
        "licenses" => "license",
        _ => "",
    }
}

/// Record a page view, dropping the oldest view of the portal once
/// the per-portal cap is reached.
pub(crate) fn record_view(state: &PortalsState, portal: &Portal, req: &HttpRequest) {
    let verified = unified_core::caller::caller_from(req);
    let viewer_user_id = if verified.is_authenticated() {
        Some(verified.user_id.clone())
    } else {
        None
    };
    let ip = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            req.connection_info()
                .realip_remote_addr()
                .map(str::to_string)
        });
    let user_agent = req
        .headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let view = PortalView {
        id: Uuid::now_v7(),
        portal_id: portal.id,
        slug: portal.slug.clone(),
        viewer_user_id: viewer_user_id.clone(),
        ip,
        user_agent,
        viewed_at: Utc::now(),
    };
    let portal_views: Vec<PortalView> = state
        .views
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| entry.portal_id == portal.id)
        .collect();
    if portal_views.len() >= MAX_VIEWS_PER_PORTAL {
        if let Some(oldest) = portal_views.iter().min_by_key(|entry| entry.viewed_at) {
            state.views.remove(&oldest.id);
        }
    }
    state.views.insert(view.id, view);
    state.emit(
        "portals.page.viewed",
        json!({
            "portal_id": portal.id.to_string(),
            "slug": portal.slug,
            "viewer_user_id": viewer_user_id,
        }),
    );
}

/// Public view of a portal: branding, visible sections, and the
/// menus the caller may see. Client records are never part of this
/// view.
///
/// `role` is `Some` only for an authenticated caller whose role the
/// server already knows (the client home route); `None` is the
/// unauthenticated public render, which shows only menus with an
/// empty role list. A client-supplied role header must never be
/// passed here: anyone can claim any role, so trusting it would
/// leak role-gated menu labels and targets to anonymous visitors.
pub(crate) fn public_portal_view(
    state: &PortalsState,
    portal: &Portal,
    role: Option<&str>,
) -> Value {
    let menus: Vec<Value> = menus_for(state, portal.id)
        .into_iter()
        .filter(|menu| match role {
            Some(role) => menu_visible_to(menu, role),
            None => menu.visible && menu.roles.is_empty(),
        })
        .map(|menu| {
            json!({
                "id": menu.id.to_string(),
                "label": menu.label,
                "target_kind": menu.target_kind,
                "target": menu.target,
                "position": menu.position,
            })
        })
        .collect();
    json!({
        "id": portal.id.to_string(),
        "company_id": portal.company_id,
        "name": portal.name,
        "slug": portal.slug,
        "custom_domain": portal.custom_domain,
        "domain_status": portal.domain_status,
        "logo_storage_target": portal.logo_storage_target,
        "logo_storage_key": portal.logo_storage_key,
        "primary_color": portal.primary_color,
        "secondary_color": portal.secondary_color,
        "accent_color": portal.accent_color,
        "background_color": portal.background_color,
        "text_color": portal.text_color,
        "sections": portal.sections,
        "status": portal.status.as_str(),
        "menus": menus,
    })
}

pub(crate) fn default_sections() -> Vec<String> {
    SECTION_NAMES.iter().map(|name| name.to_string()).collect()
}

pub(crate) fn default_color(primary: bool) -> String {
    if primary {
        "#0B6B3A".to_string()
    } else {
        "#FFFFFF".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        let verified = match caller_company {
            Some(company) => unified_core::auth::Caller::verified(
                uuid::Uuid::now_v7(),
                Vec::new(),
            )
            .with_company(company.to_string()),
            None => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new()),
        };
        req.extensions_mut().insert(verified);
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_scope_prefers_tenant_then_verified_caller() {
        let req = tenant_request("acme", Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company_scope(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
        let req = tenant_request("", Some("acme"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company_scope(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
        let req = tenant_request("acme", Some("other-company"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company_scope(&tenant, &req).is_err());
    }

    #[test]
    fn require_caller_company_refuses_unresolved_callers() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_caller_company(&tenant, &req).is_err());
        let req = tenant_request("acme", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(require_caller_company(&tenant, &req).unwrap(), "acme");
    }

    #[test]
    fn in_company_scope_str_is_own_company_only() {
        assert!(in_company_scope_str("acme", &Some("acme".to_string())));
        assert!(!in_company_scope_str("acme", &Some("other".to_string())));
        // Portals always carry a company: no global rows for an
        // unresolved caller to see.
        assert!(!in_company_scope_str("acme", &None));
    }

    #[test]
    fn require_company_match_str_refuses_foreign_and_unresolved_callers() {
        let req = tenant_request("acme", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            require_company_match_str(&tenant, &req, "acme").unwrap(),
            "acme"
        );
        assert!(require_company_match_str(&tenant, &req, "other").is_err());
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_company_match_str(&tenant, &req, "acme").is_err());
    }

    #[test]
    fn slug_validation() {
        assert!(valid_slug("my-portal"));
        assert!(valid_slug("a"));
        assert!(!valid_slug("My-Portal"));
        assert!(!valid_slug("-bad"));
        assert!(!valid_slug("bad-"));
        assert!(!valid_slug("a--b"));
        assert!(!valid_slug(""));
    }

    #[test]
    fn slugify_normalizes_titles() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("  Spaces  "), "spaces");
        assert_eq!(slugify("a--b"), "a-b");
        assert_eq!(slugify(""), "portal");
        assert_eq!(slugify("!!!"), "portal");
    }

    #[test]
    fn hex_colour_validation() {
        assert!(valid_hex_color("#0B6B3A"));
        assert!(valid_hex_color("#FFF"));
        assert!(valid_hex_color("#fff"));
        assert!(!valid_hex_color("0B6B3A"));
        assert!(!valid_hex_color("#GGGGGG"));
        assert!(!valid_hex_color("#1234"));
        assert_eq!(require_hex_color("#0b6b3a", "color").unwrap(), "#0B6B3A");
        assert!(require_hex_color("red", "color").is_err());
    }

    #[test]
    fn domain_and_url_validation() {
        assert!(valid_domain("portal.example.com"));
        assert!(valid_domain("a.b"));
        assert!(!valid_domain("example"));
        assert!(!valid_domain(".example.com"));
        assert!(!valid_domain("example.com."));
        assert!(!valid_domain("bad_host.com"));
        assert!(valid_url("https://example.com/x"));
        assert!(valid_url("http://x"));
        assert!(!valid_url("ftp://x"));
        assert!(!valid_url("https://exa mple.com"));
    }

    #[test]
    fn section_and_menu_target_validation() {
        assert!(validate_sections(&[]).is_ok());
        assert!(validate_sections(&["invoices".to_string()]).is_ok());
        assert!(validate_sections(&["bogus".to_string()]).is_err());
        assert!(validate_sections(&["invoices".to_string(), "invoices".to_string()]).is_err());
        assert!(validate_menu_target("section", "invoices").is_ok());
        assert!(validate_menu_target("section", "bogus").is_err());
        assert!(validate_menu_target("url", "https://x.com").is_ok());
        assert!(validate_menu_target("url", "ftp://x").is_err());
        assert!(validate_menu_target("other", "x").is_err());
    }

    #[test]
    fn role_list_validation() {
        assert!(validate_roles(&[]).is_ok());
        assert!(validate_roles(&["admin".to_string()]).is_ok());
        assert!(validate_roles(&["admin".to_string(), "admin".to_string()]).is_err());
        assert!(validate_roles(&["".to_string()]).is_err());
        let many: Vec<String> = (0..21).map(|i| format!("role{i}")).collect();
        assert!(validate_roles(&many).is_err());
    }

    fn test_portal() -> Portal {
        Portal {
            id: Uuid::now_v7(),
            company_id: "company-1".to_string(),
            name: "Test Portal".to_string(),
            slug: "test-portal".to_string(),
            custom_domain: Some("portal.example.com".to_string()),
            domain_status: "requested".to_string(),
            logo_storage_target: None,
            logo_storage_key: None,
            primary_color: "#0B6B3A".to_string(),
            secondary_color: "#FFFFFF".to_string(),
            accent_color: "#C9A227".to_string(),
            background_color: "#FFFFFF".to_string(),
            text_color: "#1A1A1A".to_string(),
            sections: vec!["invoices".to_string()],
            status: crate::modules::portals::types::PortalStatus::Active,
            created_by: "user-1".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn test_menu(portal_id: Uuid, label: &str, visible: bool, roles: Vec<String>) -> PortalMenu {
        PortalMenu {
            id: Uuid::now_v7(),
            portal_id,
            label: label.to_string(),
            target_kind: "section".to_string(),
            target: "invoices".to_string(),
            position: 0,
            visible,
            roles,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn unchanged_domain_keeps_verified_status() {
        let mut portal = test_portal();
        portal.domain_status = "verified".to_string();
        assert!(!apply_custom_domain(
            &mut portal,
            Some("portal.example.com".to_string())
        ));
        assert_eq!(portal.domain_status, "verified");
        assert_eq!(
            portal.custom_domain.as_deref(),
            Some("portal.example.com")
        );
    }

    #[test]
    fn changed_domain_resets_status_to_requested() {
        let mut portal = test_portal();
        portal.domain_status = "verified".to_string();
        assert!(apply_custom_domain(
            &mut portal,
            Some("new.example.com".to_string())
        ));
        assert_eq!(portal.domain_status, "requested");
        assert_eq!(portal.custom_domain.as_deref(), Some("new.example.com"));
    }

    #[test]
    fn removing_domain_sets_status_none_and_repeated_removal_is_noop() {
        let mut portal = test_portal();
        assert!(apply_custom_domain(&mut portal, None));
        assert_eq!(portal.domain_status, "none");
        assert!(portal.custom_domain.is_none());
        assert!(!apply_custom_domain(&mut portal, None));
        assert_eq!(portal.domain_status, "none");
        assert!(apply_custom_domain(
            &mut portal,
            Some("portal.example.com".to_string())
        ));
        assert_eq!(portal.domain_status, "requested");
    }

    #[test]
    fn domain_verification_accepts_verified_and_rejected() {
        let mut portal = test_portal();
        assert!(apply_domain_verification(&mut portal, "verified", Some("portal.example.com"))
            .is_ok());
        assert_eq!(portal.domain_status, "verified");

        let mut portal = test_portal();
        assert!(apply_domain_verification(&mut portal, "rejected", None).is_ok());
        assert_eq!(portal.domain_status, "rejected");
    }

    #[test]
    fn domain_verification_rejects_bad_input_and_repeat() {
        let mut portal = test_portal();
        assert!(apply_domain_verification(&mut portal, "pending", None).is_err());
        assert!(apply_domain_verification(
            &mut portal,
            "verified",
            Some("other.example.com")
        )
        .is_err());
        assert_eq!(portal.domain_status, "requested");

        let mut no_domain = test_portal();
        no_domain.custom_domain = None;
        no_domain.domain_status = "none".to_string();
        assert!(apply_domain_verification(&mut no_domain, "verified", None).is_err());

        let mut done = test_portal();
        apply_domain_verification(&mut done, "verified", None).unwrap();
        assert!(apply_domain_verification(&mut done, "rejected", None).is_err());
        assert_eq!(done.domain_status, "verified");
    }

    #[test]
    fn public_view_hides_role_gated_menus_from_anonymous_callers() {
        let state = PortalsState::default();
        let portal = test_portal();
        let public = test_menu(portal.id, "Public", true, vec![]);
        let admin = test_menu(portal.id, "Admin", true, vec!["admin".to_string()]);
        let hidden = test_menu(portal.id, "Hidden", false, vec![]);
        state.portals.insert(portal.id, portal.clone());
        state.menus.insert(public.id, public);
        state.menus.insert(admin.id, admin);
        state.menus.insert(hidden.id, hidden);

        let labels = |view: &Value| {
            view["menus"]
                .as_array()
                .expect("menus must be an array")
                .iter()
                .filter_map(|menu| menu["label"].as_str())
                .map(str::to_string)
                .collect::<Vec<String>>()
        };

        // Anonymous public render: only the role-less visible menu.
        assert_eq!(labels(&public_portal_view(&state, &portal, None)), vec!["Public"]);
        // Authenticated admin: role-gated menu appears.
        let admin_labels = labels(&public_portal_view(&state, &portal, Some("admin")));
        assert_eq!(admin_labels.len(), 2);
        assert!(admin_labels.contains(&"Public".to_string()));
        assert!(admin_labels.contains(&"Admin".to_string()));
        // Authenticated non-admin: same as anonymous.
        assert_eq!(
            labels(&public_portal_view(&state, &portal, Some("client"))),
            vec!["Public"]
        );
    }
}
