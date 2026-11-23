// security -- Hardening settings, brute-force, rate-limit, IP-list, headers and reputation handlers.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::hardening::{
    FailedAuthCluster, HeadersUpdate, IpListEntry, LockoutStatus, RateLimitEntry, ReputationEntry,
    SecuritySettingsUpdate, shared_state,
};
use unified_core::json::{error_response, success_response};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use dashmap::DashMap;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use super::auth::*;
use super::constants::*;
use super::helpers::*;
use super::module::clear_failed_login_entry;

/// `GET /api/v1/security/hardening/settings`: the live hardening
/// settings plus live counts (list sizes, current lockouts, flagged
/// IPs, failed-auth clusters).
pub(crate) async fn get_hardening_settings(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let state = shared_state();
    let settings = state.settings();
    let counts = state.overview_counts();
    success_response(json!({ "settings": settings, "counts": counts }), 200)
}

/// `PUT /api/v1/security/hardening/settings`: partial settings
/// update. Only the fields present are changed; the merged settings
/// are validated and nothing changes on failure. Updates the runtime
/// copy only and does not rewrite the configuration file.
pub(crate) async fn update_hardening_settings(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<SecuritySettingsUpdate>,
) -> HttpResponse {
    if let Err(response) = require_security_capability_write(&req, &ctx, CAP_SECURITY_MANAGE).await
    {
        return response;
    }
    let debug = ctx.is_debug();
    let state = shared_state();
    match state.update_settings(body.into_inner()) {
        Ok(settings) => success_response(
            json!({ "settings": settings, "counts": state.overview_counts() }),
            200,
        ),
        Err(message) => error_response(&UnifiedError::bad_request(message), debug),
    }
}

/// `GET /api/v1/security/hardening/brute-force`: paginated lockout /
/// attempt status per IP and per account. Filters: `kind` (`ip` or
/// `account`), `locked` (`true` / `false`); `search` matches the key.
pub(crate) async fn list_brute_force_status(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let pagination = query.normalized();
    let mut statuses: Vec<LockoutStatus> = shared_state().lockout_statuses();
    if let Some(kind) = pagination.filters.get("kind") {
        statuses.retain(|entry| &entry.kind == kind);
    }
    if let Some(locked) = pagination.filters.get("locked") {
        let want = locked == "true";
        statuses.retain(|entry| entry.locked == want);
    }
    statuses.retain(|entry| matches_search(&entry.key, &pagination.search));
    success_response(paginate_items(statuses, &query), 200)
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnlockRequest {
    pub target: String,
    pub kind: String,
}

/// `POST /api/v1/security/hardening/brute-force/unlock`: manually
/// clear a lockout / attempt record for one IP (`kind: "ip"`) or one
/// account (`kind: "account"`). An IP unlock also clears the
/// module's per-IP failed-login counter, so the IP stops being
/// flagged in `GET /failed-logins`. Emits
/// `security.bruteforce.unlocked`.
pub(crate) async fn unlock_brute_force(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    store: web::Data<Arc<DashMap<String, u64>>>,
    body: web::Json<UnlockRequest>,
) -> HttpResponse {
    if let Err(response) = require_security_capability_write(&req, &ctx, CAP_SECURITY_MANAGE).await
    {
        return response;
    }
    let debug = ctx.is_debug();
    let kind = body.kind.trim();
    if kind != "ip" && kind != "account" {
        return error_response(
            &UnifiedError::bad_request("kind must be \"ip\" or \"account\""),
            debug,
        );
    }
    let target = body.target.trim();
    if target.is_empty() {
        return error_response(
            &UnifiedError::bad_request("target must not be empty"),
            debug,
        );
    }
    let unlocked = shared_state().unlock(kind, target);
    // The per-IP failed-login counter is a separate map from the
    // hardening lockout records: an IP unlock must clear it too, or
    // the IP stays flagged in GET /failed-logins and in the
    // auth_failures check forever. The unlock succeeds when either
    // store had a record for the target.
    let cleared_failed_logins = kind == "ip" && clear_failed_login_entry(&store, target);
    if unlocked || cleared_failed_logins {
        success_response(
            json!({ "unlocked": true, "kind": kind, "key": target }),
            200,
        )
    } else {
        error_response(
            &UnifiedError::not_found(format!(
                "No brute-force record found for {kind} \"{target}\""
            )),
            debug,
        )
    }
}

/// `GET /api/v1/security/hardening/rate-limit`: paginated tracked
/// rate-limit windows. Filters: `scope` (`ip`, `user`, or
/// `endpoint`); `search` matches the window key.
pub(crate) async fn list_rate_limit_status(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let pagination = query.normalized();
    let mut entries: Vec<RateLimitEntry> = shared_state().rate_limit_entries();
    if let Some(scope) = pagination.filters.get("scope") {
        entries.retain(|entry| &entry.scope == scope);
    }
    entries.retain(|entry| matches_search(&entry.key, &pagination.search));
    success_response(paginate_items(entries, &query), 200)
}

/// `GET /api/v1/security/hardening/ip-lists`: paginated allow/block
/// list entries. Filter: `list` (`allow` or `block`); `search`
/// matches the CIDR text.
pub(crate) async fn list_ip_entries(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let pagination = query.normalized();
    let state = shared_state();
    let mut entries: Vec<IpListEntry> = Vec::new();
    match pagination.filters.get("list").map(String::as_str) {
        Some("allow") => entries.extend(state.ip_list_entries("allow")),
        Some("block") => entries.extend(state.ip_list_entries("block")),
        _ => {
            entries.extend(state.ip_list_entries("allow"));
            entries.extend(state.ip_list_entries("block"));
            entries.sort_by(|a, b| a.cidr.cmp(&b.cidr));
        }
    }
    entries.retain(|entry| matches_search(&entry.cidr, &pagination.search));
    success_response(paginate_items(entries, &query), 200)
}

#[derive(Debug, Clone, Deserialize)]
pub struct IpListAddRequest {
    pub list: String,
    pub cidr: String,
}

/// `POST /api/v1/security/hardening/ip-lists`: add one CIDR range or
/// bare IP to the allowlist or the blocklist. CIDR notation is
/// validated for IPv4 and IPv6; a bare IP becomes a host route.
pub(crate) async fn add_ip_entry(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<IpListAddRequest>,
) -> HttpResponse {
    if let Err(response) = require_security_capability_write(&req, &ctx, CAP_SECURITY_MANAGE).await
    {
        return response;
    }
    let debug = ctx.is_debug();
    match shared_state().add_ip_entry(body.list.trim(), body.cidr.trim()) {
        Ok(entry) => success_response(entry, 201),
        Err(message) => error_response(&UnifiedError::bad_request(message), debug),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct IpListRemoveQuery {
    pub list: Option<String>,
    pub cidr: Option<String>,
}

/// `DELETE /api/v1/security/hardening/ip-lists?list=allow&cidr=...`:
/// remove one entry from the allowlist or the blocklist. Removing a
/// missing entry is a 404.
pub(crate) async fn remove_ip_entry(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<IpListRemoveQuery>,
) -> HttpResponse {
    if let Err(response) = require_security_capability_write(&req, &ctx, CAP_SECURITY_MANAGE).await
    {
        return response;
    }
    let debug = ctx.is_debug();
    let (Some(list), Some(cidr)) = (query.list.clone(), query.cidr.clone()) else {
        return error_response(
            &UnifiedError::bad_request("query parameters \"list\" and \"cidr\" are required"),
            debug,
        );
    };
    match shared_state().remove_ip_entry(list.trim(), cidr.trim()) {
        Ok(true) => success_response(json!({ "removed": true, "list": list, "cidr": cidr }), 200),
        Ok(false) => error_response(
            &UnifiedError::not_found(format!("No \"{list}\" list entry found for \"{cidr}\"")),
            debug,
        ),
        Err(message) => error_response(&UnifiedError::bad_request(message), debug),
    }
}

/// `GET /api/v1/security/hardening/headers`: the configured security
/// header values (empty means that header is disabled).
pub(crate) async fn get_headers_config(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let settings = shared_state().settings();
    success_response(
        json!({
            "strict_transport_security": settings.headers_hsts,
            "content_type_options": settings.headers_content_type_options,
            "frame_options": settings.headers_frame_options,
            "referrer_policy": settings.headers_referrer_policy,
            "content_security_policy": settings.headers_content_security_policy,
            "permissions_policy": settings.headers_permissions_policy,
        }),
        200,
    )
}

/// `PUT /api/v1/security/hardening/headers`: partial header update.
/// Only the fields present are changed; an empty string disables
/// that header.
pub(crate) async fn update_headers_config(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<HeadersUpdate>,
) -> HttpResponse {
    if let Err(response) = require_security_capability_write(&req, &ctx, CAP_SECURITY_MANAGE).await
    {
        return response;
    }
    let settings = shared_state().update_headers(body.into_inner());
    success_response(
        json!({
            "strict_transport_security": settings.headers_hsts,
            "content_type_options": settings.headers_content_type_options,
            "frame_options": settings.headers_frame_options,
            "referrer_policy": settings.headers_referrer_policy,
            "content_security_policy": settings.headers_content_security_policy,
            "permissions_policy": settings.headers_permissions_policy,
        }),
        200,
    )
}

/// `GET /api/v1/security/hardening/failed-auth-alerts`: paginated
/// failed-auth clusters (repeated failures grouped by IP + account),
/// most recently active first. `search` matches the IP or account.
pub(crate) async fn list_failed_auth_alerts(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let pagination = query.normalized();
    let mut clusters: Vec<FailedAuthCluster> = shared_state().failed_auth_clusters();
    clusters.retain(|cluster| {
        let haystack = format!(
            "{} {} {}",
            cluster.key,
            cluster.ip.clone().unwrap_or_default(),
            cluster.account.clone().unwrap_or_default()
        );
        matches_search(&haystack, &pagination.search)
    });
    success_response(paginate_items(clusters, &query), 200)
}

/// `GET /api/v1/security/hardening/reputation`: paginated IP
/// reputation records, highest score first. Filter: `flagged`
/// (`true` / `false`); `search` matches the IP.
pub(crate) async fn list_reputation(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(response) = require_security_capability(&req, &ctx, CAP_SECURITY_VIEW).await {
        return response;
    }
    let pagination = query.normalized();
    let mut entries: Vec<ReputationEntry> = shared_state().reputation_entries();
    if let Some(flagged) = pagination.filters.get("flagged") {
        let want = flagged == "true";
        entries.retain(|entry| entry.flagged == want);
    }
    entries.retain(|entry| matches_search(&entry.ip, &pagination.search));
    success_response(paginate_items(entries, &query), 200)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReputationFlagRequest {
    pub ip: String,
    pub reason: Option<String>,
}

/// `POST /api/v1/security/hardening/reputation/flag`: manually flag
/// an IP with a reason. Emits `security.ip.flagged`.
pub(crate) async fn flag_reputation(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ReputationFlagRequest>,
) -> HttpResponse {
    if let Err(response) = require_security_capability_write(&req, &ctx, CAP_SECURITY_MANAGE).await
    {
        return response;
    }
    let debug = ctx.is_debug();
    match shared_state().flag_ip(body.ip.trim(), body.reason.clone()) {
        Ok(entry) => success_response(entry, 200),
        Err(message) => error_response(&UnifiedError::bad_request(message), debug),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReputationUnflagRequest {
    pub ip: String,
}

/// `POST /api/v1/security/hardening/reputation/unflag`: manually
/// unflag an IP and reset its score. Emits `security.ip.unflagged`.
pub(crate) async fn unflag_reputation(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ReputationUnflagRequest>,
) -> HttpResponse {
    if let Err(response) = require_security_capability_write(&req, &ctx, CAP_SECURITY_MANAGE).await
    {
        return response;
    }
    let debug = ctx.is_debug();
    match shared_state().unflag_ip(body.ip.trim()) {
        Ok(entry) => success_response(entry, 200),
        Err(message) => error_response(&UnifiedError::bad_request(message), debug),
    }
}
