//! Small shared helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

pub(crate) fn valid_component_status(status: &str) -> bool {
    matches!(
        status,
        COMPONENT_OPERATIONAL
            | COMPONENT_DEGRADED
            | COMPONENT_PARTIAL_OUTAGE
            | COMPONENT_MAJOR_OUTAGE
            | COMPONENT_MAINTENANCE
    )
}

pub(crate) fn valid_incident_status(status: &str) -> bool {
    matches!(
        status,
        INCIDENT_INVESTIGATING | INCIDENT_IDENTIFIED | INCIDENT_MONITORING | INCIDENT_RESOLVED
    )
}

pub(crate) fn is_open_incident(status: &str) -> bool {
    status != INCIDENT_RESOLVED
}

pub(crate) fn valid_severity(severity: &str) -> bool {
    matches!(severity, "minor" | "major" | "critical")
}

// Caller identity and capability checks (write routes only)

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
pub(crate) struct StatusCaller {
    pub actor: String,
    pub role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<StatusCaller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(StatusCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let outcome = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    outcome
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Require the statuspage manage capability on a write route:
/// authenticated identity plus the `statuspage.manage` permission,
/// verified through the `role.check` hook. Read routes stay public.
pub(crate) fn require_statuspage_manage(ctx: &AppContext, req: &HttpRequest) -> UnifiedResult<()> {
    let found = caller(req)?;
    if check_capability(ctx, &found.role, PERM_STATUSPAGE_MANAGE) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{}\"",
            found.role, PERM_STATUSPAGE_MANAGE
        )))
    }
}

// Helpers

pub(crate) fn uptime_key(component_id: Uuid, date: &NaiveDate) -> String {
    format!("{}:{}", component_id, date.format("%Y-%m-%d"))
}

pub(crate) fn round_percent(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id \"{raw}\"")))
}

pub(crate) fn parse_date(raw: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
        UnifiedError::bad_request(format!("Invalid date \"{raw}\": expected YYYY-MM-DD"))
    })
}

pub(crate) fn validate_component_name(name: &str) -> UnifiedResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(UnifiedError::bad_request("name must not be empty"));
    }
    if name.chars().count() > 120 {
        return Err(UnifiedError::bad_request(
            "name must be at most 120 characters",
        ));
    }
    Ok(())
}

/// An incident title: non-empty and at most 200 characters, on both
/// the create and the update path.
pub(crate) fn validate_incident_title(title: &str) -> UnifiedResult<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(UnifiedError::bad_request("title must not be empty"));
    }
    if title.chars().count() > MAX_INCIDENT_TITLE_LEN {
        return Err(UnifiedError::bad_request(format!(
            "title must be at most {MAX_INCIDENT_TITLE_LEN} characters"
        )));
    }
    Ok(title.to_string())
}

/// A component or incident description: may be empty, but at most
/// 2000 characters, on both the create and the update path.
pub(crate) fn validate_description(description: &str) -> UnifiedResult<String> {
    let trimmed = description.trim();
    if trimmed.chars().count() > MAX_DESCRIPTION_LEN {
        return Err(UnifiedError::bad_request(format!(
            "description must be at most {MAX_DESCRIPTION_LEN} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Minimal HTML escaping for values rendered into the public page.
fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn status_label(status: &str) -> &'static str {
    match status {
        OVERALL_OPERATIONAL => "All Systems Operational",
        OVERALL_DEGRADED => "Degraded Performance",
        OVERALL_OUTAGE => "Service Outage",
        OVERALL_MAINTENANCE => "Under Maintenance",
        _ => "Unknown Status",
    }
}

fn status_color(status: &str) -> &'static str {
    match status {
        OVERALL_OPERATIONAL => "#15803d",
        OVERALL_DEGRADED => "#b45309",
        OVERALL_OUTAGE => "#b91c1c",
        OVERALL_MAINTENANCE => "#1d4ed8",
        // Component outage states share the overall palette's
        // amber/red instead of falling through to the grey default,
        // so outages read as outages on the public page.
        COMPONENT_PARTIAL_OUTAGE => "#b45309",
        COMPONENT_MAJOR_OUTAGE => "#b91c1c",
        _ => "#475569",
    }
}

pub(crate) fn render_status_html(
    overall: &str,
    components: &[ComponentStatusView],
    open_incidents: &[Incident],
    recent_incidents: &[Incident],
) -> String {
    let mut component_rows = String::new();
    for view in components {
        component_rows.push_str(&format!(
            "<li><span>{}</span><span class=\"pill\" style=\"background:{}20;color:{}\">{}</span><span class=\"uptime\">30d {}% &middot; 90d {}%</span></li>",
            escape_html(&view.component.name),
            status_color(&view.component.status),
            status_color(&view.component.status),
            escape_html(&view.component.status),
            view.uptime_30d,
            view.uptime_90d,
        ));
    }
    if component_rows.is_empty() {
        component_rows.push_str("<li>No components registered yet.</li>");
    }
    let mut incident_rows = String::new();
    for incident in open_incidents {
        incident_rows.push_str(&format!(
            "<li><strong>{}</strong> <span class=\"pill\">{}</span> <span class=\"pill\">{}</span><p>{}</p></li>",
            escape_html(&incident.title),
            escape_html(&incident.severity),
            escape_html(&incident.status),
            escape_html(&incident.description),
        ));
    }
    if incident_rows.is_empty() {
        incident_rows.push_str("<li>No open incidents.</li>");
    }
    let mut history_rows = String::new();
    for incident in recent_incidents {
        history_rows.push_str(&format!(
            "<li><strong>{}</strong> <span class=\"pill\">{}</span> <span>{}</span></li>",
            escape_html(&incident.title),
            escape_html(&incident.status),
            incident.created_at.format("%Y-%m-%d %H:%M UTC"),
        ));
    }
    if history_rows.is_empty() {
        history_rows.push_str("<li>No incidents recorded yet.</li>");
    }
    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
        <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
        <title>System Status</title><style>\
        body{{font-family:system-ui,-apple-system,sans-serif;max-width:760px;margin:2rem auto;padding:0 1rem;color:#0f172a}}\
        .banner{{background:{};color:#fff;padding:1.25rem 1.5rem;border-radius:12px}}\
        .banner h1{{margin:0;font-size:1.5rem}} .banner p{{margin:.25rem 0 0;opacity:.9}}\
        h2{{margin-top:2rem}} ul{{list-style:none;padding:0}} li{{border:1px solid #e2e8f0;border-radius:10px;padding:.75rem 1rem;margin:.5rem 0}}\
        .pill{{display:inline-block;border-radius:999px;padding:.1rem .6rem;font-size:.8rem;margin-left:.5rem}}\
        .uptime{{float:right;color:#475569;font-size:.85rem}} p{{margin:.4rem 0 0;color:#334155}}\
        footer{{margin-top:2rem;color:#64748b;font-size:.85rem}}</style></head><body>\
        <div class=\"banner\"><h1>{}</h1><p>Overall status: {}</p></div>\
        <h2>Components</h2><ul>{}</ul>\
        <h2>Open Incidents</h2><ul>{}</ul>\
        <h2>Recent Incidents</h2><ul>{}</ul>\
        <footer>Data is also available as JSON at <code>/api/v1/statuspage</code>. Times are UTC.</footer>\
        </body></html>",
        status_color(overall),
        escape_html(status_label(overall)),
        escape_html(overall),
        component_rows,
        incident_rows,
        history_rows,
    )
}

// Public handlers

pub(crate) fn sorted_components(state: &StatusPageState) -> Vec<ComponentStatusView> {
    let mut components: Vec<StatusComponent> = state
        .components
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    components.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    components
        .iter()
        .map(|component| state.component_view(component))
        .collect()
}

pub(crate) fn open_incidents(state: &StatusPageState) -> Vec<Incident> {
    let mut incidents: Vec<Incident> = state
        .incidents
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|incident| is_open_incident(&incident.status))
        .collect();
    incidents.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    incidents
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;

    #[test]
    fn valid_component_status_accepts_all_five() {
        for status in ["operational", "degraded", "partial_outage", "major_outage", "maintenance"] {
            assert!(valid_component_status(status), "{status} should be valid");
        }
        assert!(!valid_component_status("ok"));
        assert!(!valid_component_status(""));
    }

    #[test]
    fn valid_incident_status_accepts_all_four() {
        for status in ["investigating", "identified", "monitoring", "resolved"] {
            assert!(valid_incident_status(status), "{status} should be valid");
        }
        assert!(!valid_incident_status("open"));
    }

    #[test]
    fn is_open_incident_resolved_is_closed() {
        assert!(!is_open_incident("resolved"));
        assert!(is_open_incident("investigating"));
        assert!(is_open_incident("monitoring"));
    }

    #[test]
    fn valid_severity_cases() {
        assert!(valid_severity("minor"));
        assert!(valid_severity("major"));
        assert!(valid_severity("critical"));
        assert!(!valid_severity("low"));
        assert!(!valid_severity(""));
    }

    #[test]
    fn round_percent_cases() {
        assert_eq!(round_percent(100.0), 100.0);
        assert_eq!(round_percent(99.994), 99.99);
        assert_eq!(round_percent(33.333), 33.33);
        assert_eq!(round_percent(0.0), 0.0);
    }

    #[test]
    fn escape_html_escapes_all_five() {
        assert_eq!(
            escape_html("<b>\"Tom\" & 'Jerry'</b>"),
            "&lt;b&gt;&quot;Tom&quot; &amp; &#39;Jerry&#39;&lt;/b&gt;"
        );
    }

    #[test]
    fn escape_html_leaves_plain_text_unchanged() {
        assert_eq!(escape_html("All Systems Operational"), "All Systems Operational");
        assert_eq!(escape_html(""), "");
    }

    #[test]
    fn status_label_and_color_known_and_unknown() {
        assert_eq!(status_label("operational"), "All Systems Operational");
        assert_eq!(status_label("degraded"), "Degraded Performance");
        assert_eq!(status_label("outage"), "Service Outage");
        assert_eq!(status_label("maintenance"), "Under Maintenance");
        assert_eq!(status_label("bogus"), "Unknown Status");
        assert_eq!(status_color("operational"), "#15803d");
        assert_eq!(status_color("degraded"), "#b45309");
        assert_eq!(status_color("outage"), "#b91c1c");
        assert_eq!(status_color("maintenance"), "#1d4ed8");
        assert_eq!(status_color("bogus"), "#475569");
    }

    #[test]
    fn uptime_key_format() {
        let id = Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000").unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        assert_eq!(
            uptime_key(id, &date),
            "123e4567-e89b-12d3-a456-426614174000:2026-01-05"
        );
    }

    #[test]
    fn validate_component_name_cases() {
        assert!(validate_component_name("API").is_ok());
        assert!(validate_component_name("  Database  ").is_ok());
        assert!(validate_component_name("   ").is_err());
        assert!(validate_component_name("").is_err());
        assert!(validate_component_name(&"x".repeat(120)).is_ok());
        assert!(validate_component_name(&"x".repeat(121)).is_err());
    }

    #[test]
    fn status_color_outage_states_are_amber_and_red() {
        assert_eq!(status_color("partial_outage"), "#b45309");
        assert_eq!(status_color("major_outage"), "#b91c1c");
        assert_eq!(status_color("degraded"), "#b45309");
        assert_eq!(status_color("bogus"), "#475569");
    }

    #[test]
    fn validate_incident_title_cases() {
        assert_eq!(
            validate_incident_title("  Outage  ").unwrap(),
            "Outage"
        );
        assert!(validate_incident_title("").is_err());
        assert!(validate_incident_title("   ").is_err());
        assert!(validate_incident_title(&"x".repeat(200)).is_ok());
        assert!(validate_incident_title(&"x".repeat(201)).is_err());
    }

    #[test]
    fn validate_description_cases() {
        assert_eq!(validate_description("").unwrap(), "");
        assert_eq!(validate_description("  ok  ").unwrap(), "ok");
        assert!(validate_description(&"x".repeat(2000)).is_ok());
        assert!(validate_description(&"x".repeat(2001)).is_err());
    }

    #[test]
    fn require_statuspage_manage_checks_role_check_hook() {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookContext;
        use unified_core::modules::ModuleRegistry;
        use actix_web::test::TestRequest;

        fn test_ctx(allowed: &[&str]) -> Arc<AppContext> {
            let hooks = Arc::new(HookBus::new());
            let allowed: Vec<String> =
                allowed.iter().map(|permission| permission.to_string()).collect();
            hooks.register(
                "role.check",
                "test",
                Arc::new(move |ctx: &mut HookContext| {
                    let permission = ctx
                        .get("permission")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    ctx.set(
                        "allowed",
                        serde_json::json!(allowed.iter().any(|held| held == permission)),
                    );
                }),
            );
            Arc::new(AppContext::new(
                AppConfig::default(),
                hooks,
                Arc::new(ModuleRegistry::new()),
            ))
        }

        fn admin_request() -> HttpRequest {
            verified_request("admin-1", "admin")
        }

        let managing = test_ctx(&[PERM_STATUSPAGE_MANAGE]);
        assert!(require_statuspage_manage(&managing, &admin_request()).is_ok());
        let viewing = test_ctx(&["statuspage.view"]);
        assert!(require_statuspage_manage(&viewing, &admin_request()).is_err());
        let no_role = verified_request_without_roles("admin-1");
        assert!(require_statuspage_manage(&managing, &no_role).is_err());
        let anonymous = TestRequest::default().to_http_request();
        assert!(require_statuspage_manage(&managing, &anonymous).is_err());
    }

    #[test]
    fn parse_uuid_and_parse_date() {
        assert!(parse_uuid("123e4567-e89b-12d3-a456-426614174000", "component").is_ok());
        assert!(parse_uuid("nope", "component").is_err());
        assert_eq!(
            parse_date("2026-01-05").unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
        );
        assert!(parse_date("05-01-2026").is_err());
    }
}
