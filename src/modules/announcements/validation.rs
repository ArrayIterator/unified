//! Audience, channel, title, version and window validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;
use super::types::{ANNOUNCEMENT_CHANNELS, AUDIENCE_ADMINS, AUDIENCE_ALL, AUDIENCE_DEPARTMENT, AUDIENCE_ROLE, AUDIENCE_SEGMENT, AUDIENCE_USER, AUDIENCE_USERS, Announcement, CATEGORY_FEATURE, CATEGORY_FIX, CATEGORY_SECURITY, STATUS_ARCHIVED, STATUS_DRAFT, STATUS_PUBLISHED};

pub(crate) fn valid_audience(audience: &str) -> bool {
    matches!(
        audience,
        AUDIENCE_ALL
            | AUDIENCE_USERS
            | AUDIENCE_ADMINS
            | AUDIENCE_ROLE
            | AUDIENCE_DEPARTMENT
            | AUDIENCE_SEGMENT
            | AUDIENCE_USER
    )
}

pub(crate) fn validate_channels(channels: &[String]) -> UnifiedResult<()> {
    if channels.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one channel must be enabled",
        ));
    }
    for channel in channels {
        if !ANNOUNCEMENT_CHANNELS.contains(&channel.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Channels must be a subset of: {}",
                ANNOUNCEMENT_CHANNELS.join(", ")
            )));
        }
    }
    Ok(())
}

/// Validate the audience trio: role / department / segment need an
/// `audience_value`; `user` needs at least one user id; email
/// addresses only make sense alongside user ids.
pub(crate) fn validate_audience(
    audience: &str,
    audience_value: Option<&str>,
    audience_user_ids: &[String],
    audience_emails: &[String],
) -> UnifiedResult<()> {
    if !valid_audience(audience) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid audience \"{audience}\": expected all, users, admins, role, department, segment, or user"
        )));
    }
    match audience {
        AUDIENCE_ROLE | AUDIENCE_DEPARTMENT | AUDIENCE_SEGMENT => {
            if audience_value.map(str::trim).unwrap_or("").is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "audience_value is required for audience \"{audience}\""
                )));
            }
        }
        AUDIENCE_USER => {
            if audience_user_ids.is_empty() {
                return Err(UnifiedError::bad_request(
                    "audience_user_ids must not be empty for audience \"user\"",
                ));
            }
        }
        _ => {}
    }
    if !audience_emails.is_empty() && audience_user_ids.is_empty() {
        return Err(UnifiedError::bad_request(
            "audience_emails requires audience_user_ids",
        ));
    }
    for email in audience_emails {
        if !email.contains('@') || !email.contains('.') {
            return Err(UnifiedError::bad_request(format!(
                "Invalid email address \"{email}\""
            )));
        }
    }
    Ok(())
}

/// True when an announcement addressed this way may be shown to a
/// caller with this identity. `roles`, `departments` and `segments`
/// are the caller's memberships resolved from authenticated identity
/// headers (see `resolve_identity`), never from query parameters:
/// a caller must not be able to claim a role, department, or segment
/// by asking for it. This is the server-side audience rule for the
/// public list, the unread list and the dashboard feed: a user only
/// ever sees announcements addressed to them.
pub(crate) fn audience_matches(
    announcement: &Announcement,
    user_id: &str,
    roles: &[String],
    departments: &[String],
    segments: &[String],
) -> bool {
    match announcement.audience.as_str() {
        AUDIENCE_ALL | AUDIENCE_USERS => true,
        AUDIENCE_ADMINS => roles.iter().any(|role| role == "admin" || role == "administrator"),
        AUDIENCE_ROLE => announcement
            .audience_value
            .as_deref()
            .is_some_and(|value| roles.iter().any(|role| role == value)),
        AUDIENCE_DEPARTMENT => announcement
            .audience_value
            .as_deref()
            .is_some_and(|value| departments.iter().any(|department| department == value)),
        AUDIENCE_SEGMENT => announcement
            .audience_value
            .as_deref()
            .is_some_and(|value| segments.iter().any(|segment| segment == value)),
        AUDIENCE_USER => announcement
            .audience_user_ids
            .iter()
            .any(|id| id == user_id),
        _ => false,
    }
}

/// Split one comma-separated header value into trimmed, non-empty parts.
pub(crate) fn split_list(raw: Option<&String>) -> Vec<String> {
    raw.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Caller identity and capability checks
// ---------------------------------------------------------------------------

/// Capabilities checked through the `role.check` hook; fails closed.
/// The admin role holds every capability through the roles module
/// wildcard; other roles need an explicit grant.
pub(crate) const PERM_ANNOUNCEMENTS_VIEW: &str = "announcements.view";

pub(crate) const PERM_ANNOUNCEMENTS_MANAGE: &str = "announcements.manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the events and community
/// modules.
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

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, task 316).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before caller identity or any capability
/// check.
///
/// The records in this module carry no company field, so the guard
/// is a fail-closed gate only: real per-record partitioning needs a
/// `tenant_id` on the record (C11 follow-up), not a filter here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Fail closed (403) when no company resolves for a mutating
/// handler: the tenant context's company first, the verified
/// caller's server-resolved company otherwise. An unresolved caller
/// writes nothing (the shared `CallerScope::Unresolved` rule) — the
/// records in this module carry no company field to stamp, so there
/// is no global-row write to fall back to.
pub(crate) fn require_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?.ok_or_else(|| {
        UnifiedError::forbidden(
            "No company scope resolved for this request: writes require a resolved company",
        )
    })
}

/// Resolve the caller and require `permission` in one step, exactly
/// like the cache module's `authorize`.
pub(crate) fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

/// Identity resolved from the server-verified caller, never from query
/// parameters or headers: the verified user id and roles come from
/// `unified_core::caller::Caller` (auth middleware). Departments and
/// segments have no verified source yet, so they are always empty —
/// an anonymous caller has no user id, no roles, no departments, no
/// segments, so only broadcast announcements match.
pub(crate) struct CallerIdentity {
    pub(crate) user_id: String,
    pub(crate) roles: Vec<String>,
    pub(crate) departments: Vec<String>,
    pub(crate) segments: Vec<String>,
}

pub(crate) fn resolve_identity(req: &HttpRequest) -> CallerIdentity {
    let verified = unified_core::caller::caller_from(req);
    CallerIdentity {
        user_id: verified.user_id.clone(),
        roles: verified.roles.clone(),
        departments: Vec::new(),
        segments: Vec::new(),
    }
}

pub(crate) fn valid_announcement_status(status: &str) -> bool {
    matches!(status, STATUS_DRAFT | STATUS_PUBLISHED | STATUS_ARCHIVED)
}

pub(crate) fn valid_category(category: &str) -> bool {
    matches!(category, CATEGORY_FEATURE | CATEGORY_FIX | CATEGORY_SECURITY)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id \"{raw}\"")))
}

pub(crate) fn validate_title_body(title: &str, body: &str) -> UnifiedResult<()> {
    if title.trim().is_empty() {
        return Err(UnifiedError::bad_request("title must not be empty"));
    }
    if title.chars().count() > 200 {
        return Err(UnifiedError::bad_request(
            "title must be at most 200 characters",
        ));
    }
    if body.trim().is_empty() {
        return Err(UnifiedError::bad_request("body must not be empty"));
    }
    if body.chars().count() > 20000 {
        return Err(UnifiedError::bad_request(
            "body must be at most 20000 characters",
        ));
    }
    Ok(())
}

pub(crate) fn validate_window(
    publish_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
) -> UnifiedResult<()> {
    if let (Some(publish_at), Some(expires_at)) = (publish_at, expires_at) {
        if expires_at <= publish_at {
            return Err(UnifiedError::bad_request(
                "expires_at must be after publish_at",
            ));
        }
    }
    Ok(())
}

/// Validate a version string: `1.2.3`, `v1.2.3`, or with a pre-release
/// suffix such as `1.2.3-beta.1`.
pub(crate) fn validate_version(version: &str) -> UnifiedResult<()> {
    let version = version.trim();
    if version.is_empty() {
        return Err(UnifiedError::bad_request("version must not be empty"));
    }
    if version.chars().count() > 50 {
        return Err(UnifiedError::bad_request(
            "version must be at most 50 characters",
        ));
    }
    let core = version.strip_prefix('v').unwrap_or(version);
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(UnifiedError::bad_request(format!(
            "Invalid version \"{version}\": expected semantic versioning such as 1.2.3"
        )));
    }
    Ok(())
}

pub(crate) fn validate_date(raw: &str) -> UnifiedResult<()> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
        UnifiedError::bad_request(format!("Invalid date \"{raw}\": expected YYYY-MM-DD"))
    })?;
    Ok(())
}

pub(crate) fn validate_user_id(user_id: &str) -> UnifiedResult<()> {
    if user_id.trim().is_empty() {
        return Err(UnifiedError::bad_request("user_id must not be empty"));
    }
    if user_id.chars().count() > 256 {
        return Err(UnifiedError::bad_request(
            "user_id must be at most 256 characters",
        ));
    }
    Ok(())
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

pub(crate) fn sort_announcements(
    mut items: Vec<Announcement>,
    params: &PaginationParams,
) -> Vec<Announcement> {
    let ascending = params.normalized().order == "asc";
    let field = params.allowed_sort(&["created_at", "publish_at", "title"], "created_at");
    // Pinned first on the public ordering, then the chosen sort. The
    // pinned key never flips with the sort direction: a pinned
    // announcement stays first whether the list runs newest- or
    // oldest-first.
    items.sort_by(|a, b| {
        b.pinned.cmp(&a.pinned).then_with(|| {
            let ord = match field.as_str() {
                "publish_at" => b
                    .publish_at
                    .cmp(&a.publish_at)
                    .then_with(|| b.created_at.cmp(&a.created_at)),
                "title" => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
                _ => b.created_at.cmp(&a.created_at),
            };
            if ascending {
                ord.reverse()
            } else {
                ord
            }
        })
    });
    items
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use crate::modules::announcements::types::Announcement;
    use actix_web::test::TestRequest;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn sort_params(order: Option<&str>) -> PaginationParams {
        PaginationParams {
            page: Some(1),
            per_page: Some(10),
            sort: None,
            order: order.map(str::to_string),
            search: None,
            filters: HashMap::new(),
        }
    }

    fn make_announcement(
        audience: &str,
        value: Option<&str>,
        user_ids: &[&str],
    ) -> Announcement {
        Announcement {
            id: Uuid::new_v4(),
            title: "Title".to_string(),
            body: "Body".to_string(),
            audience: audience.to_string(),
            audience_value: value.map(str::to_string),
            audience_user_ids: user_ids.iter().map(|id| id.to_string()).collect(),
            audience_emails: Vec::new(),
            channels: vec!["in_app".to_string()],
            email_template_id: None,
            status: "published".to_string(),
            publish_at: None,
            expires_at: None,
            pinned: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            published_at: None,
        }
    }

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn valid_audience_accepts_known_audiences() {
        for audience in ["all", "users", "admins", "role", "department", "segment", "user"] {
            assert!(valid_audience(audience), "audience {audience}");
        }
        assert!(!valid_audience("everyone"));
        assert!(!valid_audience(""));
    }

    #[test]
    fn validate_channels_requires_known_channels() {
        assert!(validate_channels(&["in_app".to_string()]).is_ok());
        assert!(validate_channels(&[
            "in_app".to_string(),
            "email".to_string(),
            "webhook".to_string(),
        ])
        .is_ok());
        assert!(validate_channels(&[]).is_err());
        assert!(validate_channels(&["sms".to_string()]).is_err());
    }

    #[test]
    fn validate_audience_enforces_value_and_user_rules() {
        assert!(validate_audience("role", None, &[], &[]).is_err());
        assert!(validate_audience("role", Some("editor"), &[], &[]).is_ok());
        assert!(validate_audience("department", Some("  "), &[], &[]).is_err());
        assert!(validate_audience("user", None, &[], &[]).is_err());
        assert!(validate_audience("user", None, &["u1".to_string()], &[]).is_ok());
        assert!(validate_audience("all", None, &[], &["a@b.com".to_string()]).is_err());
        assert!(validate_audience(
            "user",
            None,
            &["u1".to_string()],
            &["not-an-email".to_string()],
        )
        .is_err());
        assert!(validate_audience(
            "user",
            None,
            &["u1".to_string()],
            &["a@b.com".to_string()],
        )
        .is_ok());
        assert!(validate_audience("everyone", None, &[], &[]).is_err());
    }

    #[test]
    fn audience_matches_routes_by_identity() {
        let all = make_announcement("all", None, &[]);
        assert!(audience_matches(&all, "anyone", &[], &[], &[]));
        let admins = make_announcement("admins", None, &[]);
        assert!(audience_matches(&admins, "u1", &["admin".to_string()], &[], &[]));
        assert!(!audience_matches(&admins, "u1", &["member".to_string()], &[], &[]));
        let role = make_announcement("role", Some("editor"), &[]);
        assert!(audience_matches(&role, "u1", &["editor".to_string()], &[], &[]));
        assert!(!audience_matches(&role, "u1", &["viewer".to_string()], &[], &[]));
        let dept = make_announcement("department", Some("support"), &[]);
        assert!(audience_matches(
            &dept,
            "u1",
            &[],
            &["support".to_string()],
            &[]
        ));
        let user = make_announcement("user", None, &["u1"]);
        assert!(audience_matches(&user, "u1", &[], &[], &[]));
        assert!(!audience_matches(&user, "u2", &[], &[], &[]));
        let unknown = make_announcement("everyone", None, &[]);
        assert!(!audience_matches(&unknown, "u1", &[], &[], &[]));
    }

    #[test]
    fn split_list_splits_and_trims() {
        let raw = "a, b,, c ".to_string();
        assert_eq!(split_list(Some(&raw)), vec!["a", "b", "c"]);
        assert!(split_list(None).is_empty());
        let empty = String::new();
        assert!(split_list(Some(&empty)).is_empty());
    }

    #[test]
    fn valid_announcement_status_and_category() {
        assert!(valid_announcement_status("draft"));
        assert!(valid_announcement_status("published"));
        assert!(valid_announcement_status("archived"));
        assert!(!valid_announcement_status("live"));
        assert!(valid_category("feature"));
        assert!(valid_category("fix"));
        assert!(valid_category("security"));
        assert!(!valid_category("news"));
    }

    #[test]
    fn validate_title_body_enforces_limits() {
        assert!(validate_title_body("Title", "Body").is_ok());
        assert!(validate_title_body("", "Body").is_err());
        assert!(validate_title_body("Title", "").is_err());
        assert!(validate_title_body(&"x".repeat(201), "Body").is_err());
        assert!(validate_title_body("Title", &"x".repeat(20001)).is_err());
    }

    #[test]
    fn validate_window_requires_expiry_after_publish() {
        assert!(validate_window(None, None).is_ok());
        assert!(validate_window(
            Some(at("2026-11-22T10:00:00Z")),
            Some(at("2026-11-23T10:00:00Z")),
        )
        .is_ok());
        assert!(validate_window(
            Some(at("2026-11-23T10:00:00Z")),
            Some(at("2026-11-22T10:00:00Z")),
        )
        .is_err());
        assert!(validate_window(
            Some(at("2026-11-22T10:00:00Z")),
            Some(at("2026-11-22T10:00:00Z")),
        )
        .is_err());
    }

    #[test]
    fn validate_version_accepts_semver_forms() {
        assert!(validate_version("1.2.3").is_ok());
        assert!(validate_version("v1.2.3").is_ok());
        assert!(validate_version("1.2.3-beta.1").is_ok());
        assert!(validate_version("1.2").is_err());
        assert!(validate_version("").is_err());
        assert!(validate_version(&"1".repeat(51)).is_err());
    }

    #[test]
    fn validate_date_and_user_id() {
        assert!(validate_date("2026-11-22").is_ok());
        assert!(validate_date("22/11/2026").is_err());
        assert!(validate_date("2026-13-01").is_err());
        assert!(validate_user_id("u1").is_ok());
        assert!(validate_user_id("").is_err());
        assert!(validate_user_id(&"x".repeat(257)).is_err());
    }

    #[test]
    fn sort_announcements_puts_pinned_first_in_either_direction() {
        let mut old = make_announcement("all", None, &[]);
        old.title = "Old".to_string();
        old.created_at = at("2026-11-20T10:00:00Z");
        let mut pinned = make_announcement("all", None, &[]);
        pinned.title = "New pinned".to_string();
        pinned.pinned = true;
        pinned.created_at = at("2026-11-21T10:00:00Z");
        let mut new = make_announcement("all", None, &[]);
        new.title = "New".to_string();
        new.created_at = at("2026-11-22T10:00:00Z");
        // Newest-first: pinned first, then newest unpinned.
        let sorted = sort_announcements(
            vec![old.clone(), new.clone(), pinned.clone()],
            &sort_params(None),
        );
        assert_eq!(sorted[0].title, "New pinned");
        assert_eq!(sorted[1].title, "New");
        assert_eq!(sorted[2].title, "Old");
        // Oldest-first: pinned still first, then oldest unpinned.
        let sorted = sort_announcements(
            vec![old.clone(), new.clone(), pinned.clone()],
            &sort_params(Some("asc")),
        );
        assert_eq!(sorted[0].title, "New pinned");
        assert_eq!(sorted[1].title, "Old");
        assert_eq!(sorted[2].title, "New");
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        let req = verified_request("user-1", "admin");
        let found = caller(&req).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
    }

    #[test]
    fn capability_check_fails_closed_without_a_role_check_listener() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_ANNOUNCEMENTS_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_ANNOUNCEMENTS_MANAGE).is_err());
        let req = verified_request("user-1", "admin");
        assert!(authorize(&ctx, &req, PERM_ANNOUNCEMENTS_MANAGE).is_err());
    }

    #[test]
    fn resolve_identity_reads_the_verified_caller_not_query_parameters() {
        let req = verified_request("u1", "editor");
        let identity = resolve_identity(&req);
        assert_eq!(identity.user_id, test_user_uuid("u1").to_string());
        assert_eq!(identity.roles, vec!["editor"]);
        assert_eq!(identity.departments, vec!["support", "sales"]);
        assert_eq!(identity.segments, vec!["beta"]);
        let anonymous = resolve_identity(&TestRequest::default().to_http_request());
        assert!(anonymous.user_id.is_empty());
        assert!(anonymous.roles.is_empty());
        assert!(anonymous.departments.is_empty());
        assert!(anonymous.segments.is_empty());
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(tenant.company_id.as_deref(), Some("company-1"));
        let company = caller_company(&tenant, &req).expect("company must resolve");
        assert_eq!(company.as_deref(), Some("company-1"));
        assert_eq!(
            require_company(&tenant, &req).expect("company must resolve"),
            "company-1"
        );
    }

    #[test]
    fn require_company_fails_closed_without_a_resolved_company() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(tenant.company_id.is_none());
        assert!(
            caller_company(&tenant, &req)
                .expect("mismatch check must pass")
                .is_none()
        );
        assert!(require_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_refuses_a_verified_company_mismatch() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                .with_company("company-2".to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }
}
