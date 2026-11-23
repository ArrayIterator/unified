//! seo - handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json as api_json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::{Arc};
use uuid::Uuid;
use super::{types::*, state::*, analysis::*, module::*};



// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Caller identity and capability checks (same trusted-parameter
// pattern as the tenants module: X-User-Id plus X-Role headers,
// `role.check` hook, fail closed).
// ---------------------------------------------------------------------------
pub(crate) const PERM_SEO_VIEW: &str = "seo:view";
pub(crate) const PERM_SEO_MANAGE: &str = "seo:manage";

pub(crate) struct SeoCaller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<SeoCaller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(SeoCaller { actor, role })
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

pub(crate) fn require_permission(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<()> {
    let caller = caller(req)?;
    if check_capability(ctx, &caller.role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{permission}\"",
            caller.role
        )))
    }
}

/// Validate a URL for storage or emission into HTML: it must parse
/// and use the http or https scheme. Anything else (including
/// `javascript:`) is refused, and the input is never echoed back.
pub(crate) fn validate_http_https_url(raw: &str) -> Result<String, UnifiedError> {
    let trimmed = raw.trim();
    let parsed = url::Url::parse(trimmed)
        .map_err(|_| UnifiedError::bad_request("URL must be a valid http(s) URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(UnifiedError::bad_request(
            "URL must use the http or https scheme",
        ));
    }
    Ok(parsed.to_string())
}


pub(crate) async fn get_settings(
    state: web::Data<SeoState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = require_permission(&ctx, &req, PERM_SEO_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let settings = state
        .settings
        .read()
        .expect("seo settings lock poisoned")
        .clone();
    api_json::success_response(settings, 200)
}


pub(crate) async fn update_settings(
    state: web::Data<SeoState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<UpdateSeoSettingsInput>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = require_permission(&ctx, &req, PERM_SEO_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let input = input.into_inner();
    let outcome = (|| -> Result<SeoSettings, UnifiedError> {
        let mut settings = state
            .settings
            .write()
            .expect("seo settings lock poisoned");
        if let Some(title) = input.site_title {
            let title = title.trim();
            if title.is_empty() {
                return Err(UnifiedError::bad_request("site_title must not be empty"));
            }
            if title.chars().count() > 200 {
                return Err(UnifiedError::bad_request(
                    "site_title must be at most 200 characters",
                ));
            }
            settings.site_title = title.to_string();
        }
        if let Some(description) = input.site_description {
            if description.chars().count() > 500 {
                return Err(UnifiedError::bad_request(
                    "site_description must be at most 500 characters",
                ));
            }
            settings.site_description = description;
        }
        if let Some(robots) = input.robots {
            validate_robots(&robots)?;
            settings.robots = robots;
        }
        if let Some(sitemap_enabled) = input.sitemap_enabled {
            settings.sitemap_enabled = sitemap_enabled;
        }
        if let Some(image) = input.default_og_image {
            // An og image is emitted into HTML, so it must be a real
            // http(s) URL; an empty value clears the stored image.
            let trimmed = image.trim();
            if trimmed.is_empty() {
                settings.default_og_image = None;
            } else {
                settings.default_og_image = Some(validate_http_https_url(trimmed)?);
            }
        }
        Ok(settings.clone())
    })();
    match outcome {
        Ok(settings) => {
            ctx.hooks.emit(
                "seo.settings.updated",
                json!({
                    "site_title": settings.site_title,
                    "robots": settings.robots,
                    "sitemap_enabled": settings.sitemap_enabled,
                }),
            );
            api_json::success_response(settings, 200)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}


pub(crate) const ROBOT_TOKENS: &[&str] = &[
    "index",
    "noindex",
    "follow",
    "nofollow",
    "noarchive",
    "nosnippet",
    "noimageindex",
    "notranslate",
    "none",
    "all",
];


pub(crate) const OG_TYPES: &[&str] = &[
    "website",
    "article",
    "book",
    "profile",
    "video.movie",
    "video.episode",
    "video.tv_show",
    "video.other",
    "music.song",
    "music.album",
    "music.playlist",
    "music.radio_station",
];


pub(crate) const TWITTER_CARDS: &[&str] =
    &["summary", "summary_large_image", "app", "player"];


pub(crate) fn validate_robots(robots: &str) -> Result<(), UnifiedError> {
    let robots = robots.trim();
    if robots.is_empty() {
        return Err(UnifiedError::bad_request("robots must not be empty"));
    }
    for token in robots.split(',') {
        let token = token.trim().to_lowercase();
        if !ROBOT_TOKENS.contains(&token.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "unknown robots token \"{token}\""
            )));
        }
    }
    Ok(())
}


pub(crate) async fn analyze(
    state: web::Data<SeoState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<AnalyzeInput>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = require_permission(&ctx, &req, PERM_SEO_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let input = input.into_inner();
    if input.title.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("title must not be empty"),
            ctx.is_debug(),
        );
    }
    if input.content.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("content must not be empty"),
            ctx.is_debug(),
        );
    }
    if let Some(keyword) = &input.keyword {
        if keyword.trim().is_empty() {
            return api_json::error_response(
                &UnifiedError::bad_request("keyword must not be empty when provided"),
                ctx.is_debug(),
            );
        }
    }
    let analysis = analyze_content(
        input.title.trim(),
        &input.content,
        input.keyword.as_deref().map(str::trim),
        input.meta_description.as_deref(),
        input.url,
        None,
    );
    state.analyses.insert(analysis.id, analysis.clone());
    ctx.hooks.emit(
        "seo.analyzed",
        json!({
            "id": analysis.id.to_string(),
            "title": analysis.title,
            "score": analysis.score,
            "grade": analysis.grade,
        }),
    );
    api_json::success_response(analysis, 200)
}


pub(crate) async fn list_analyses(
    state: web::Data<SeoState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = require_permission(&ctx, &req, PERM_SEO_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let search = params.search.clone();
    let mut items: Vec<SeoAnalysis> = state
        .analyses
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|analysis| {
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return analysis.title.to_lowercase().contains(&needle)
                    || analysis
                        .keyword
                        .as_deref()
                        .is_some_and(|keyword| keyword.to_lowercase().contains(&needle));
            }
            true
        })
        .collect();
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        if descending {
            b.created_at.cmp(&a.created_at)
        } else {
            a.created_at.cmp(&b.created_at)
        }
    });
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<SeoAnalysis> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}


pub(crate) async fn get_analysis(
    state: web::Data<SeoState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = require_permission(&ctx, &req, PERM_SEO_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    match state.analyses.get(&id.into_inner()) {
        Some(analysis) => api_json::success_response(analysis.value().clone(), 200),
        None => api_json::error_response(&UnifiedError::not_found("analysis not found"), ctx.is_debug()),
    }
}


pub(crate) fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}


pub(crate) async fn generate_meta_tags(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<MetaTagsInput>,
) -> HttpResponse {
    if let Err(error) = require_tenant(&req) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    if let Err(error) = require_permission(&ctx, &req, PERM_SEO_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let input = input.into_inner();
    let title = input.title.trim();
    if title.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("title must not be empty"),
            ctx.is_debug(),
        );
    }
    let mut warnings: Vec<String> = Vec::new();
    let title_len = title.chars().count();
    if !(30..=60).contains(&title_len) {
        warnings.push(format!(
            "Title is {title_len} characters; 30-60 is recommended for search results."
        ));
    }
    let description = input.description.as_deref().map(str::trim);
    if let Some(description) = description {
        let len = description.chars().count();
        if len > 160 {
            warnings.push(format!(
                "Description is {len} characters; search engines usually truncate past 160."
            ));
        }
        if len < 50 {
            warnings.push(format!(
                "Description is only {len} characters; 120-160 is recommended."
            ));
        }
    } else {
        warnings.push("No description provided; add one of 120-160 characters.".to_string());
    }
    let canonical = match &input.canonical {
        Some(raw) => match validate_http_https_url(raw) {
            Ok(url) => Some(url),
            Err(_) => {
                return api_json::error_response(
                    &UnifiedError::bad_request("canonical must be a valid http(s) URL"),
                    ctx.is_debug(),
                );
            }
        },
        None => None,
    };
    // An og image is emitted into HTML, so it must be a real http(s)
    // URL; anything else is omitted with a warning, as before.
    let og_image = input
        .og_image
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| validate_http_https_url(value).ok());
    if input.og_image.as_deref().is_some_and(|value| !value.trim().is_empty()) && og_image.is_none()
    {
        warnings.push("og_image is not a valid URL and was omitted.".to_string());
    }
    let robots = input.robots.as_deref().unwrap_or("index, follow");
    if let Err(error) = validate_robots(robots) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    // og_type and twitter_card accept only known values, like
    // robots does; anything else is refused instead of being
    // emitted into the page verbatim.
    let og_type = match input
        .og_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(raw) => {
            let normalized = raw.to_lowercase();
            if !OG_TYPES.contains(&normalized.as_str()) {
                return api_json::error_response(
                    &UnifiedError::bad_request(format!("unknown og_type \"{normalized}\"")),
                    ctx.is_debug(),
                );
            }
            normalized
        }
        None => "website".to_string(),
    };
    let twitter_card = match input
        .twitter_card
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(raw) => {
            let normalized = raw.to_lowercase();
            if !TWITTER_CARDS.contains(&normalized.as_str()) {
                return api_json::error_response(
                    &UnifiedError::bad_request(format!("unknown twitter_card \"{normalized}\"")),
                    ctx.is_debug(),
                );
            }
            normalized
        }
        None => "summary_large_image".to_string(),
    };

    let mut tags: Vec<MetaTag> = Vec::new();
    let mut html_lines: Vec<String> = Vec::new();
    let mut push_tag = |tag: &str, attributes: Value, html: String| {
        tags.push(MetaTag {
            tag: tag.to_string(),
            attributes,
        });
        html_lines.push(html);
    };

    push_tag(
        "title",
        json!({ "text": title }),
        format!("<title>{}</title>", escape_attr(title)),
    );
    if let Some(description) = description {
        push_tag(
            "meta",
            json!({ "name": "description", "content": description }),
            format!(
                "<meta name=\"description\" content=\"{}\">",
                escape_attr(description)
            ),
        );
    }
    push_tag(
        "meta",
        json!({ "name": "robots", "content": robots }),
        format!("<meta name=\"robots\" content=\"{}\">", escape_attr(robots)),
    );
    if let Some(keywords) = &input.keywords {
        let joined = keywords
            .iter()
            .map(|keyword| keyword.trim())
            .filter(|keyword| !keyword.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        if !joined.is_empty() {
            push_tag(
                "meta",
                json!({ "name": "keywords", "content": joined }),
                format!("<meta name=\"keywords\" content=\"{}\">", escape_attr(&joined)),
            );
        }
    }
    if let Some(canonical) = &canonical {
        push_tag(
            "link",
            json!({ "rel": "canonical", "href": canonical }),
            format!("<link rel=\"canonical\" href=\"{}\">", escape_attr(canonical)),
        );
    }
    let og_type = og_type.as_str();
    push_tag(
        "meta",
        json!({ "property": "og:title", "content": title }),
        format!(
            "<meta property=\"og:title\" content=\"{}\">",
            escape_attr(title)
        ),
    );
    push_tag(
        "meta",
        json!({ "property": "og:type", "content": og_type }),
        format!("<meta property=\"og:type\" content=\"{}\">", escape_attr(og_type)),
    );
    if let Some(description) = description {
        push_tag(
            "meta",
            json!({ "property": "og:description", "content": description }),
            format!(
                "<meta property=\"og:description\" content=\"{}\">",
                escape_attr(description)
            ),
        );
    }
    if let Some(image) = og_image {
        let image = image.as_str();
        push_tag(
            "meta",
            json!({ "property": "og:image", "content": image }),
            format!("<meta property=\"og:image\" content=\"{}\">", escape_attr(image)),
        );
    }
    let twitter_card = twitter_card.as_str();
    push_tag(
        "meta",
        json!({ "name": "twitter:card", "content": twitter_card }),
        format!(
            "<meta name=\"twitter:card\" content=\"{}\">",
            escape_attr(twitter_card)
        ),
    );
    push_tag(
        "meta",
        json!({ "name": "twitter:title", "content": title }),
        format!(
            "<meta name=\"twitter:title\" content=\"{}\">",
            escape_attr(title)
        ),
    );
    if let Some(description) = description {
        push_tag(
            "meta",
            json!({ "name": "twitter:description", "content": description }),
            format!(
                "<meta name=\"twitter:description\" content=\"{}\">",
                escape_attr(description)
            ),
        );
    }

    api_json::success_response(
        json!({
            "tags": tags,
            "html": html_lines.join("\n"),
            "warnings": warnings,
        }),
        200,
    )
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
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn test_context() -> Arc<AppContext> {
        Arc::new(AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        ))
    }

    fn allow_role_check(ctx: &AppContext) {
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
    }

    fn admin_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn settings_input() -> UpdateSeoSettingsInput {
        UpdateSeoSettingsInput {
            site_title: Some("New site title".to_string()),
            site_description: None,
            robots: None,
            sitemap_enabled: None,
            default_og_image: None,
        }
    }

    #[test]
    fn validate_http_https_url_accepts_only_http_and_https() {
        assert!(validate_http_https_url("https://example.com/og.png").is_ok());
        assert!(validate_http_https_url("http://example.com/og.png").is_ok());
        assert!(validate_http_https_url("javascript:alert(1)").is_err());
        assert!(validate_http_https_url("ftp://example.com/og.png").is_err());
        assert!(validate_http_https_url("not a url").is_err());
        assert!(validate_http_https_url("").is_err());
    }

    #[tokio::test]
    async fn update_settings_is_gated_behind_the_manage_capability() {
        let ctx = test_context();
        let ctx_data = web::Data::new(ctx.clone());
        let state = web::Data::new(SeoState::default());
        // No identity headers at all: refused with 401 before any work.
        let anonymous = TestRequest::default().to_http_request();
        let response = update_settings(
            state.clone(),
            ctx_data.clone(),
            anonymous,
            web::Json(settings_input()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // Identity headers present, but no `role.check` handler answers:
        // fail closed with 403.
        let response = update_settings(
            state.clone(),
            ctx_data.clone(),
            admin_request(),
            web::Json(settings_input()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        // A `role.check` handler that allows the capability lets the
        // request through to the normal update flow.
        allow_role_check(&ctx);
        let response = update_settings(
            state,
            ctx_data,
            admin_request(),
            web::Json(settings_input()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn update_settings_rejects_a_non_http_default_og_image() {
        let ctx = test_context();
        allow_role_check(&ctx);
        let ctx_data = web::Data::new(ctx.clone());
        let state = web::Data::new(SeoState::default());
        let mut input = settings_input();
        input.default_og_image = Some("javascript:alert(1)".to_string());
        let response = update_settings(
            state.clone(),
            ctx_data.clone(),
            admin_request(),
            web::Json(input),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let mut input = settings_input();
        input.default_og_image = Some("https://example.com/og.png".to_string());
        let response = update_settings(state, ctx_data, admin_request(), web::Json(input)).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    fn meta_tags_input() -> MetaTagsInput {
        MetaTagsInput {
            title: "A reasonably long title for testing here".to_string(),
            description: Some("A description of the page under test.".to_string()),
            keywords: None,
            canonical: None,
            og_image: None,
            og_type: None,
            robots: None,
            twitter_card: None,
        }
    }

    #[tokio::test]
    async fn generate_meta_tags_rejects_non_http_canonical_and_unknown_types() {
        let ctx = test_context();
        allow_role_check(&ctx);
        let ctx_data = web::Data::new(ctx.clone());
        let mut input = meta_tags_input();
        input.canonical = Some("javascript:alert(1)".to_string());
        let response = generate_meta_tags(ctx_data.clone(), admin_request(), web::Json(input)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let mut input = meta_tags_input();
        input.og_type = Some("bogus".to_string());
        let response = generate_meta_tags(ctx_data.clone(), admin_request(), web::Json(input)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let mut input = meta_tags_input();
        input.twitter_card = Some("bogus".to_string());
        let response = generate_meta_tags(ctx_data.clone(), admin_request(), web::Json(input)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let mut input = meta_tags_input();
        input.canonical = Some("https://example.com/page".to_string());
        input.og_type = Some("article".to_string());
        input.twitter_card = Some("summary".to_string());
        let response = generate_meta_tags(ctx_data, admin_request(), web::Json(input)).await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, group C).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// SEO (analyses, settings) records carry no company field, so the guard is a
/// fail-closed gate only: real per-company partitioning needs a
/// `tenant_id` on the records (C11 follow-up), and no per-record
/// company check is applied here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach.
    fn tenant_request() -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "acme".to_string(),
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
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
    }
}
