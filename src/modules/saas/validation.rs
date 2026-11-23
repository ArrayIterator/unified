// saas module -- Validation helpers for inputs handled by this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use regex::Regex;
use std::sync::OnceLock;

use super::SiteUpdateInput;

pub(crate) fn domain_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^(?=.{1,253}$)[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)+$")
            .expect("domain regex must compile")
    })
}

/// Validate a hostname: valid labels, at least one dot, alphabetic TLD.
pub(crate) fn validate_domain(domain: &str) -> UnifiedResult<()> {
    if !domain_regex().is_match(domain) {
        return Err(UnifiedError::bad_request(format!(
            "Domain '{domain}' is not a valid hostname"
        )));
    }
    let tld = domain.rsplit('.').next().unwrap_or("");
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(format!(
            "Domain '{domain}' must end with an alphabetic top-level domain"
        )));
    }
    Ok(())
}

/// Validate the optional fields of a site update: a provided
/// `company_id`, `name`, or `theme_id` must not be blank. This mirrors
/// the non-empty checks creation applies, so an update cannot blank a
/// field that creation requires.
pub(crate) fn validate_site_update(input: &SiteUpdateInput) -> UnifiedResult<()> {
    if let Some(company_id) = &input.company_id {
        if company_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("company_id must not be empty"));
        }
    }
    if let Some(name) = &input.name {
        if name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Site name must not be empty"));
        }
    }
    if let Some(theme_id) = &input.theme_id {
        if theme_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("theme_id must not be empty"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update_input(
        company_id: Option<&str>,
        name: Option<&str>,
        domain: Option<&str>,
        theme_id: Option<&str>,
    ) -> SiteUpdateInput {
        SiteUpdateInput {
            company_id: company_id.map(str::to_string),
            name: name.map(str::to_string),
            domain: domain.map(str::to_string),
            theme_id: theme_id.map(str::to_string),
        }
    }

    #[test]
    fn accepts_plain_domain() {
        assert!(validate_domain("example.com").is_ok());
    }

    #[test]
    fn accepts_subdomain() {
        assert!(validate_domain("sub.example.co.uk").is_ok());
    }

    #[test]
    fn rejects_bare_label_without_dot() {
        assert!(validate_domain("example").is_err());
    }

    #[test]
    fn rejects_empty_domain() {
        assert!(validate_domain("").is_err());
    }

    #[test]
    fn rejects_numeric_tld() {
        assert!(validate_domain("example.123").is_err());
    }

    #[test]
    fn rejects_spaces_and_leading_dash() {
        assert!(validate_domain("exa mple.com").is_err());
        assert!(validate_domain("-bad.com").is_err());
    }

    #[test]
    fn update_accepts_absent_optional_fields() {
        assert!(validate_site_update(&update_input(None, None, None, None)).is_ok());
    }

    #[test]
    fn update_accepts_non_blank_fields() {
        assert!(validate_site_update(&update_input(
            Some("company-1"),
            Some("New Name"),
            Some("new.example.com"),
            Some("theme-2"),
        ))
        .is_ok());
    }

    #[test]
    fn update_rejects_blank_company_id() {
        let error = validate_site_update(&update_input(Some("   "), None, None, None))
            .expect_err("blank company_id must fail");
        assert!(error.message.contains("company_id"));
    }

    #[test]
    fn update_rejects_blank_theme_id() {
        let error = validate_site_update(&update_input(None, None, None, Some("")))
            .expect_err("blank theme_id must fail");
        assert!(error.message.contains("theme_id"));
    }

    #[test]
    fn update_rejects_blank_name() {
        assert!(validate_site_update(&update_input(None, Some("  "), None, None)).is_err());
    }
}
