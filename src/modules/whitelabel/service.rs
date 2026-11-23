// Whitelabel module - pure resolution logic.
//
// Split from `src/modules/whitelabel.rs`; the module root re-exports the public API of this module.

use super::constants::{DOMAIN_NONE};
use super::types::{AllowanceMode, BrandingProfile, GlobalDefaults, ResolvedBranding};

// ---------------------------------------------------------------------------
// Pure resolution functions (no I/O, no locks: unit-testable)
// ---------------------------------------------------------------------------

/// Effective allowance from the global default and the per-company
/// override mode. `inherit` (or no override) follows the global
/// default; `allowed` / `denied` override it.
pub fn effective_allowance(global_allow: bool, company_mode: Option<AllowanceMode>) -> bool {
    match company_mode {
        Some(AllowanceMode::Allowed) => true,
        Some(AllowanceMode::Denied) => false,
        Some(AllowanceMode::Inherit) | None => global_allow,
    }
}

/// Resolve branding from the three levels, field by field: tenant
/// override, then company default, then the global defaults record.
/// When `allowed` is false the resolved branding is the global
/// defaults record in full — tenant and company overrides are
/// ignored entirely, so Unified branding stays everywhere.
pub fn resolve_branding(
    company_id: &str,
    tenant_id: &Option<String>,
    tenant: Option<&BrandingProfile>,
    company: Option<&BrandingProfile>,
    defaults: &GlobalDefaults,
    allowed: bool,
) -> ResolvedBranding {
    // Pick one field from the first level that sets it.
    let pick = |field: fn(&BrandingProfile) -> &Option<String>| -> Option<String> {
        tenant
            .and_then(|profile| field(profile).as_ref())
            .or_else(|| company.and_then(|profile| field(profile).as_ref()))
            .cloned()
    };
    let pick_pair = |field: fn(&BrandingProfile) -> (Option<String>, Option<String>)| -> (Option<String>, Option<String>) {
        let tenant_pair = tenant.map(field);
        let company_pair = company.map(field);
        let target = tenant_pair
            .clone()
            .and_then(|(target, _)| target)
            .or_else(|| company_pair.clone().and_then(|(target, _)| target));
        let key = tenant_pair
            .and_then(|(_, key)| key)
            .or_else(|| company_pair.and_then(|(_, key)| key));
        (target, key)
    };
    let (logo_target, logo_key) = pick_pair(|profile| {
        (
            profile.logo_storage_target.clone(),
            profile.logo_storage_key.clone(),
        )
    });
    let (favicon_target, favicon_key) = pick_pair(|profile| {
        (
            profile.favicon_storage_target.clone(),
            profile.favicon_storage_key.clone(),
        )
    });
    let (login_logo_target, login_logo_key) = pick_pair(|profile| {
        (
            profile.login_logo_storage_target.clone(),
            profile.login_logo_storage_key.clone(),
        )
    });
    let (login_bg_target, login_bg_key) = pick_pair(|profile| {
        (
            profile.login_background_storage_target.clone(),
            profile.login_background_storage_key.clone(),
        )
    });
    if !allowed {
        return ResolvedBranding {
            company_id: company_id.to_string(),
            tenant_id: tenant_id.clone(),
            allowed: false,
            enforced_platform_default: true,
            display_name: defaults.display_name.clone(),
            logo_storage_target: defaults.logo_storage_target.clone(),
            logo_storage_key: defaults.logo_storage_key.clone(),
            favicon_storage_target: defaults.favicon_storage_target.clone(),
            favicon_storage_key: defaults.favicon_storage_key.clone(),
            primary_color: defaults.primary_color.clone(),
            accent_color: defaults.accent_color.clone(),
            background_color: defaults.background_color.clone(),
            text_color: defaults.text_color.clone(),
            custom_domain: None,
            domain_status: DOMAIN_NONE.to_string(),
            hide_powered_by: false,
            sender_name: defaults.sender_name.clone(),
            sender_email: defaults.sender_email.clone(),
            login_logo_storage_target: defaults.logo_storage_target.clone(),
            login_logo_storage_key: defaults.logo_storage_key.clone(),
            login_background_storage_target: defaults.login_background_storage_target.clone(),
            login_background_storage_key: defaults.login_background_storage_key.clone(),
            login_tagline: defaults.login_tagline.clone(),
            powered_by_text: defaults.powered_by_text.clone(),
        };
    }
    let hide_powered_by = tenant
        .map(|profile| profile.hide_powered_by)
        .or_else(|| company.map(|profile| profile.hide_powered_by))
        .unwrap_or(false);
    let domain_status = tenant
        .and_then(|profile| profile.custom_domain.as_ref().map(|_| profile.domain_status.clone()))
        .or_else(|| {
            company.and_then(|profile| {
                profile
                    .custom_domain
                    .as_ref()
                    .map(|_| profile.domain_status.clone())
            })
        })
        .unwrap_or_else(|| DOMAIN_NONE.to_string());
    ResolvedBranding {
        company_id: company_id.to_string(),
        tenant_id: tenant_id.clone(),
        allowed: true,
        enforced_platform_default: false,
        display_name: pick(|profile| &profile.display_name)
            .unwrap_or_else(|| defaults.display_name.clone()),
        logo_storage_target: logo_target.or_else(|| defaults.logo_storage_target.clone()),
        logo_storage_key: logo_key.or_else(|| defaults.logo_storage_key.clone()),
        favicon_storage_target: favicon_target.or_else(|| defaults.favicon_storage_target.clone()),
        favicon_storage_key: favicon_key.or_else(|| defaults.favicon_storage_key.clone()),
        primary_color: pick(|profile| &profile.primary_color)
            .unwrap_or_else(|| defaults.primary_color.clone()),
        accent_color: pick(|profile| &profile.accent_color)
            .unwrap_or_else(|| defaults.accent_color.clone()),
        background_color: pick(|profile| &profile.background_color)
            .unwrap_or_else(|| defaults.background_color.clone()),
        text_color: pick(|profile| &profile.text_color)
            .unwrap_or_else(|| defaults.text_color.clone()),
        custom_domain: pick(|profile| &profile.custom_domain),
        domain_status,
        hide_powered_by,
        sender_name: pick(|profile| &profile.sender_name)
            .unwrap_or_else(|| defaults.sender_name.clone()),
        sender_email: pick(|profile| &profile.sender_email)
            .unwrap_or_else(|| defaults.sender_email.clone()),
        login_logo_storage_target: login_logo_target
            .or_else(|| defaults.logo_storage_target.clone()),
        login_logo_storage_key: login_logo_key.or_else(|| defaults.logo_storage_key.clone()),
        login_background_storage_target: login_bg_target
            .or_else(|| defaults.login_background_storage_target.clone()),
        login_background_storage_key: login_bg_key
            .or_else(|| defaults.login_background_storage_key.clone()),
        login_tagline: pick(|profile| &profile.login_tagline)
            .unwrap_or_else(|| defaults.login_tagline.clone()),
        powered_by_text: defaults.powered_by_text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_allowance_matrix() {
        assert!(effective_allowance(true, None));
        assert!(!effective_allowance(false, None));
        assert!(!effective_allowance(true, Some(AllowanceMode::Denied)));
        assert!(effective_allowance(false, Some(AllowanceMode::Allowed)));
        assert!(effective_allowance(true, Some(AllowanceMode::Inherit)));
        assert!(!effective_allowance(false, Some(AllowanceMode::Inherit)));
    }

    #[test]
    fn resolve_branding_denied_uses_defaults() {
        let defaults = GlobalDefaults::seed();
        let resolved = resolve_branding("c1", &None, None, None, &defaults, false);
        assert!(!resolved.allowed);
        assert!(resolved.enforced_platform_default);
        assert_eq!(resolved.display_name, "Unified");
        assert_eq!(resolved.primary_color, "#2563EB");
        assert_eq!(resolved.powered_by_text, "Powered by Unified");
    }

    #[test]
    fn resolve_branding_prefers_tenant_then_company_then_defaults() {
        let defaults = GlobalDefaults::seed();
        let mut tenant = BrandingProfile::new("c1", Some("t1".to_string()));
        tenant.display_name = Some("Tenant Brand".to_string());
        let mut company = BrandingProfile::new("c1", None);
        company.display_name = Some("Company Brand".to_string());
        let resolved = resolve_branding(
            "c1",
            &Some("t1".to_string()),
            Some(&tenant),
            Some(&company),
            &defaults,
            true,
        );
        assert!(resolved.allowed);
        assert!(!resolved.enforced_platform_default);
        assert_eq!(resolved.display_name, "Tenant Brand");
        assert_eq!(resolved.primary_color, "#2563EB");
        let resolved = resolve_branding("c1", &None, None, Some(&company), &defaults, true);
        assert_eq!(resolved.display_name, "Company Brand");
        let resolved = resolve_branding("c1", &None, None, None, &defaults, true);
        assert_eq!(resolved.display_name, "Unified");
    }
}
