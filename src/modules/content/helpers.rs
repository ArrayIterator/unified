//! Content Management module. — Helpers.
//!
//! Split from `src/modules/content.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::categories::*;
use super::module::*;
use super::pages::*;
use super::posts::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

/// Capabilities checked through the `role.check` hook (fail closed),
/// following the logistics module: reads need `content.view`,
/// creates need `content.create`, and updates, deletes, and
/// publish/unpublish need `content.manage`.
pub(super) const PERM_CONTENT_VIEW: &str = "content.view";
pub(super) const PERM_CONTENT_CREATE: &str = "content.create";
pub(super) const PERM_CONTENT_MANAGE: &str = "content.manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai module.
pub(super) struct Caller {
    /// Authenticated actor id. Retained for identity parity with the
    /// ai module's caller; this module's checks only need the role,
    /// so the field is otherwise unused.
    #[allow(dead_code)]
    pub(super) actor: String,
    pub(super) role: String,
}

pub(super) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(super) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(super) fn require_permission(
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

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, group E).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only — there is no per-record company check to
/// apply. Real partitioning needs a `tenant_id` on the record, a
/// C11 follow-up, not a C3 gate.
pub(super) fn require_tenant(
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
pub(super) fn caller_company(
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

/// Writes require a resolved company: a mutating handler with no
/// resolvable company scope is refused (403). Reads proceed on the
/// tenant gate plus the mismatch check above — records here carry
/// no company field, so there is nothing to partition on read.
pub(super) fn require_caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?
        .ok_or_else(|| UnifiedError::forbidden("A write requires a resolved company scope"))
}

/// Resolve the author a create body may record (`author_id` on a
/// post or page). The recorded author is the header caller: a body
/// value naming someone else is forgery and is rejected with 403,
/// EXCEPT when the caller holds `content.manage` — the
/// admin/on-behalf flow (an editor publishing under another
/// author's byline), verified through the same `role.check`
/// pattern as every other check. An omitted author stays omitted:
/// no false attribution is recorded, and no author is invented —
/// only a supplied value naming someone else is refused.
pub(super) fn resolve_body_author(
    requested: Option<Uuid>,
    actor: &str,
    ctx: &AppContext,
    role: &str,
) -> UnifiedResult<Option<Uuid>> {
    match requested {
        None => Ok(None),
        Some(id) if id.to_string() == actor => Ok(Some(id)),
        Some(id) if check_capability(ctx, role, PERM_CONTENT_MANAGE) => Ok(Some(id)),
        Some(_) => Err(UnifiedError::forbidden(
            "author_id must match the authenticated caller unless the caller holds content.manage",
        )),
    }
}

/// Resolve the caller from the request headers and require
/// `permission` for them, in one step. Every content route calls
/// this first, before touching any state, so an unauthenticated or
/// unauthorized request can neither read nor mutate content.
pub(super) fn authorize(req: &HttpRequest, ctx: &AppContext, permission: &str) -> UnifiedResult<()> {
    let caller = caller(req)?;
    require_permission(ctx, &caller.role, permission)
}

/// Turn a title into a URL slug: lowercase, transliterate common
/// Latin diacritics to plain ASCII (`é` -> `e`, `ß` -> `ss`,
/// `æ` -> `ae`), keep ASCII alphanumerics, and collapse every other
/// run of characters to a single `-`. Letters outside the Latin
/// scripts covered by the transliteration table (for example Cyrillic
/// or CJK) have no ASCII equivalent and are dropped, so a title made
/// only of such letters yields an empty slug.
pub fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = true; // suppresses a leading dash
    for ch in input.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if let Some(replacement) = transliterate(ch) {
            slug.push_str(replacement);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// Map one lowercased non-ASCII letter to its plain ASCII
/// equivalent. Returns `None` for characters with no Latin
/// transliteration; `slugify` drops those like any other
/// non-alphanumeric character.
fn transliterate(ch: char) -> Option<&'static str> {
    Some(match ch {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'č' => "c",
        'ð' | 'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĳ' => "ij",
        'ĵ' => "j",
        'ķ' => "k",
        'ł' => "l",
        'ñ' | 'ń' | 'ň' | 'ņ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ř' | 'ŗ' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// Parse a `filters[status]` value: only `"published"` and `"draft"`
/// are accepted. Anything else is a 400 instead of silently
/// matching drafts.
pub(super) fn parse_status_filter(raw: &str) -> Result<ContentStatus, UnifiedError> {
    match raw {
        "published" => Ok(ContentStatus::Published),
        "draft" => Ok(ContentStatus::Draft),
        other => Err(UnifiedError::bad_request(format!(
            "invalid status filter \"{other}\": expected \"published\" or \"draft\""
        ))),
    }
}

/// Append `-2`, `-3`, ... until the slug is free.
pub(super) fn unique_slug(base: &str, taken: &dyn Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Remove HTML tags from a string, keeping the text between them.
pub(super) fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Build an excerpt: an explicit excerpt wins (trimmed, capped at 300
/// characters); otherwise the plain text of the body is truncated to 160
/// characters at the last word boundary and suffixed with an ellipsis.
pub fn make_excerpt(content: &str, provided: Option<&str>) -> String {
    if let Some(text) = provided {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return trimmed.chars().take(300).collect();
        }
    }
    let plain: String = strip_html(content)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if plain.chars().count() <= 160 {
        return plain;
    }
    let cut: String = plain.chars().take(160).collect();
    match cut.rfind(' ') {
        Some(index) => format!("{}...", &cut[..index]),
        None => format!("{cut}..."),
    }
}

pub(super) fn validate_title(title: &str) -> Result<String, UnifiedError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(UnifiedError::bad_request("title must not be empty"));
    }
    if title.chars().count() > 500 {
        return Err(UnifiedError::bad_request(
            "title must be at most 500 characters",
        ));
    }
    Ok(title.to_string())
}

pub(super) fn validate_content(content: Option<String>) -> Result<String, UnifiedError> {
    let content = content.unwrap_or_default();
    if content.chars().count() > 1_000_000 {
        return Err(UnifiedError::bad_request(
            "content must be at most 1,000,000 characters",
        ));
    }
    Ok(content)
}

/// Resolve a caller-supplied slug (or derive one from the title) and make
/// it unique inside the collection described by `taken`.
pub(super) fn resolve_slug(
    requested: Option<String>,
    title: &str,
    taken: &dyn Fn(&str) -> bool,
) -> Result<String, UnifiedError> {
    let base = match requested {
        Some(raw) => slugify(&raw),
        None => slugify(title),
    };
    if base.is_empty() {
        return Err(UnifiedError::bad_request(
            "slug must contain at least one letter or digit",
        ));
    }
    Ok(unique_slug(&base, taken))
}

/// Slice a sorted, filtered list into one page.
pub(super) fn paginate<T: Serialize + Clone>(
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

pub(super) fn matches_search(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

pub(super) fn sort_posts(mut items: Vec<Post>, params: &PaginationParams) -> Vec<Post> {
    let sort = params.allowed_sort(&["created_at", "updated_at", "title"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "title" => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            "updated_at" => a.updated_at.cmp(&b.updated_at),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    items
}

pub(super) fn sort_pages(mut items: Vec<Page>, params: &PaginationParams) -> Vec<Page> {
    let sort = params.allowed_sort(&["created_at", "updated_at", "title"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "title" => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            "updated_at" => a.updated_at.cmp(&b.updated_at),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    items
}

pub(super) fn sort_categories(
    mut items: Vec<Category>,
    params: &PaginationParams,
) -> Vec<Category> {
    let sort = params.allowed_sort(&["created_at", "name"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    items
}

pub(super) fn sort_tags(mut items: Vec<Tag>, params: &PaginationParams) -> Vec<Tag> {
    let sort = params.allowed_sort(&["created_at", "name"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    items
}

pub(super) fn sort_post_types(
    mut items: Vec<PostTypeDefinition>,
    params: &PaginationParams,
) -> Vec<PostTypeDefinition> {
    let sort = params.allowed_sort(&["created_at", "plural_label", "menu_position"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "plural_label" => a.plural_label.to_lowercase().cmp(&b.plural_label.to_lowercase()),
            "menu_position" => a.menu_position.cmp(&b.menu_position),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    items
}

pub(super) fn sort_custom_entries(
    mut items: Vec<CustomEntry>,
    params: &PaginationParams,
) -> Vec<CustomEntry> {
    let sort = params.allowed_sort(&["created_at", "updated_at", "title"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "title" => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            "updated_at" => a.updated_at.cmp(&b.updated_at),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    items
}

/// Validate a caller-supplied kebab-case slug (post type slugs,
/// tag slugs supplied explicitly): lowercase ASCII letters,
/// digits, and single hyphens only; no leading/trailing hyphen,
/// no `--`, 1..=64 characters. Returns the slug unchanged on
/// success. Derived slugs from `slugify` already satisfy this.
pub(super) fn validate_kebab_slug(raw: &str, what: &str) -> Result<String, UnifiedError> {
    let slug = raw.trim().to_lowercase();
    if slug.is_empty() || slug.len() > 64 {
        return Err(UnifiedError::bad_request(format!(
            "{what} slug must be 1-64 characters"
        )));
    }
    let mut previous_dash = true; // rejects a leading dash
    for ch in slug.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            previous_dash = false;
        } else if ch == '-' && !previous_dash {
            previous_dash = true;
        } else {
            return Err(UnifiedError::bad_request(format!(
                "{what} slug must be kebab-case: lowercase letters, digits, and single hyphens only"
            )));
        }
    }
    if previous_dash {
        return Err(UnifiedError::bad_request(format!(
            "{what} slug must not end with a hyphen"
        )));
    }
    Ok(slug)
}

/// Validate a dynamic field key: snake_case, starts with a
/// lowercase letter, 1..=64 characters.
pub(super) fn validate_field_key(raw: &str) -> Result<String, UnifiedError> {
    let key = raw.trim().to_lowercase();
    if key.is_empty() || key.len() > 64 {
        return Err(UnifiedError::bad_request(
            "field key must be 1-64 characters",
        ));
    }
    let mut chars = key.chars();
    if !chars.next().is_some_and(|ch| ch.is_ascii_lowercase()) {
        return Err(UnifiedError::bad_request(
            "field key must start with a lowercase letter",
        ));
    }
    if !key
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Err(UnifiedError::bad_request(
            "field key must be snake_case: lowercase letters, digits, and underscores only",
        ));
    }
    Ok(key)
}

/// Validate a module sprite symbol name (post type icon):
/// lowercase letters, digits, hyphens, and underscores, 1..=64
/// characters.
pub(super) fn validate_icon_name(raw: &str) -> Result<String, UnifiedError> {
    let icon = raw.trim().to_lowercase();
    if icon.is_empty() || icon.len() > 64 {
        return Err(UnifiedError::bad_request(
            "icon must be 1-64 characters",
        ));
    }
    if !icon
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        return Err(UnifiedError::bad_request(
            "icon must be a module sprite symbol name: lowercase letters, digits, hyphens, and underscores only",
        ));
    }
    Ok(icon)
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
    use std::sync::Arc;

    fn post_with(title: &str, created_at: DateTime<Utc>) -> Post {
        Post {
            id: Uuid::new_v4(),
            title: title.to_string(),
            slug: slugify(title),
            content: String::new(),
            excerpt: String::new(),
            status: ContentStatus::Draft,
            category_id: None,
            author_id: None,
            attachments: Vec::new(),
            created_at,
            updated_at: created_at,
            published_at: None,
        }
    }

    #[test]
    fn slugify_cases() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  spaced   out  "), "spaced-out");
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify("Café"), "cafe");
        assert_eq!(slugify("a--b"), "a-b");
    }

    #[test]
    fn slugify_transliterates_latin_diacritics() {
        assert_eq!(slugify("naïve façade"), "naive-facade");
        assert_eq!(slugify("Straße"), "strasse");
        assert_eq!(slugify("Æsop's Fables"), "aesops-fables");
        assert_eq!(slugify("Łódź"), "lodz");
        assert_eq!(slugify("Crème brûlée"), "creme-brulee");
        assert_eq!(slugify("Ñoño"), "nono");
        assert_eq!(slugify("Ångström"), "angstrom");
        assert_eq!(slugify("Œuvre Þór"), "oeuvre-thor");
        // Letters with no Latin transliteration are still dropped.
        assert_eq!(slugify("Привет"), "");
        assert_eq!(slugify("Hello Привет"), "hello");
    }

    #[test]
    fn parse_status_filter_accepts_only_published_or_draft() {
        assert_eq!(
            parse_status_filter("published").unwrap(),
            ContentStatus::Published
        );
        assert_eq!(
            parse_status_filter("draft").unwrap(),
            ContentStatus::Draft
        );
        for invalid in ["garbage", "publish", "Published", "DRAFT", ""] {
            let error = parse_status_filter(invalid).unwrap_err();
            assert_eq!(error.code, 400, "filter {invalid:?} must be rejected");
        }
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        use actix_web::test::TestRequest;

        let missing_both = caller(&TestRequest::default().to_http_request());
        assert_eq!(missing_both.unwrap_err().code, 401);
        let missing_role = caller(
            &TestRequest::default().to_http_request(),
        );
        assert_eq!(missing_role.unwrap_err().code, 401);
        let missing_user = caller(&TestRequest::default().to_http_request());
        assert_eq!(missing_user.unwrap_err().code, 401);
        let present = caller(&verified_request("user-1", "admin")).unwrap();
        assert_eq!(present.actor, test_user_uuid("user-1").to_string());
        assert_eq!(present.role, "admin");
    }

    fn test_context() -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;

        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        ctx
    }

    fn authed_request() -> HttpRequest {
        use actix_web::test::TestRequest;

        verified_request("user-1", "admin")
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_handler() {
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_CONTENT_VIEW));
        let error = require_permission(&ctx, "admin", PERM_CONTENT_VIEW).unwrap_err();
        assert_eq!(error.code, 403);
        assert!(require_permission(&allowing_context(), "admin", PERM_CONTENT_VIEW).is_ok());
        assert!(require_permission(&allowing_context(), "admin", PERM_CONTENT_MANAGE).is_ok());
    }

    #[test]
    fn authorize_rejects_anonymous_and_unprivileged_callers() {
        use actix_web::test::TestRequest;

        let ctx = test_context();
        // No identity headers at all: 401.
        let anonymous = authorize(
            &TestRequest::default().to_http_request(),
            &ctx,
            PERM_CONTENT_VIEW,
        );
        assert_eq!(anonymous.unwrap_err().code, 401);
        // Headers present, but no `role.check` handler answers: the
        // guard fails closed with 403.
        let unprivileged = authorize(&authed_request(), &ctx, PERM_CONTENT_VIEW);
        assert_eq!(unprivileged.unwrap_err().code, 403);
        // A handler that grants the permission: allowed.
        assert!(authorize(&authed_request(), &allowing_context(), PERM_CONTENT_VIEW).is_ok());
        assert!(authorize(&authed_request(), &allowing_context(), PERM_CONTENT_CREATE).is_ok());
        assert!(authorize(&authed_request(), &allowing_context(), PERM_CONTENT_MANAGE).is_ok());
    }

    #[test]
    fn unique_slug_appends_counter() {
        assert_eq!(unique_slug("hello", &|_| false), "hello");
        assert_eq!(unique_slug("hello", &|s| s == "hello"), "hello-2");
        assert_eq!(
            unique_slug("hello", &|s| s == "hello" || s == "hello-2"),
            "hello-3"
        );
    }

    #[test]
    fn strip_html_keeps_text_only() {
        assert_eq!(strip_html("<p>Hello <b>World</b></p>"), "Hello World");
        assert_eq!(strip_html("plain text"), "plain text");
        assert_eq!(strip_html("<p>Hello"), "Hello");
        assert_eq!(strip_html(""), "");
    }

    #[test]
    fn make_excerpt_prefers_provided_text() {
        assert_eq!(
            make_excerpt("body text", Some("  Custom excerpt  ")),
            "Custom excerpt"
        );
        assert_eq!(make_excerpt("body text", Some("   ")), "body text");
        let long = "a".repeat(400);
        assert_eq!(make_excerpt("body", Some(&long)).chars().count(), 300);
    }

    #[test]
    fn make_excerpt_truncates_long_bodies() {
        let short = make_excerpt("Hello <b>brave</b> world", None);
        assert_eq!(short, "Hello brave world");
        let long_body = "word ".repeat(60);
        let excerpt = make_excerpt(&long_body, None);
        assert!(excerpt.ends_with("..."));
        assert!(!excerpt.contains('<'));
        assert!(excerpt.chars().count() <= 163);
    }

    #[test]
    fn validate_title_and_content() {
        assert_eq!(validate_title("  News  ").unwrap(), "News");
        assert!(validate_title("").is_err());
        assert!(validate_title("   ").is_err());
        assert!(validate_title(&"a".repeat(501)).is_err());
        assert_eq!(validate_content(None).unwrap(), "");
        assert_eq!(validate_content(Some("x".to_string())).unwrap(), "x");
        assert!(validate_content(Some("a".repeat(1_000_001))).is_err());
    }

    #[test]
    fn resolve_slug_uses_requested_or_derived_slug() {
        assert_eq!(
            resolve_slug(Some("  My Post!! ".to_string()), "Title", &|_| false).unwrap(),
            "my-post"
        );
        assert_eq!(
            resolve_slug(None, "Hello World", &|s| s == "hello-world").unwrap(),
            "hello-world-2"
        );
        assert!(resolve_slug(None, "!!!", &|_| false).is_err());
    }

    #[test]
    fn matches_search_is_case_insensitive() {
        assert!(matches_search("Hello World", "hello"));
        assert!(matches_search("Hello World", "WORLD"));
        assert!(!matches_search("Hello World", "bye"));
    }

    #[test]
    fn sort_posts_by_title_and_date() {
        let early = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let late = DateTime::from_timestamp(1_700_000_600, 0).unwrap();
        let items = vec![post_with("Banana", late), post_with("Apple", early)];
        let by_date = sort_posts(items.clone(), &PaginationParams::default());
        assert_eq!(by_date[0].title, "Apple");
        let by_title = sort_posts(
            items,
            &PaginationParams {
                sort: Some("title".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(by_title[0].title, "Apple");
        let desc = sort_posts(
            vec![post_with("Banana", late), post_with("Apple", early)],
            &PaginationParams {
                order: Some("desc".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(desc[0].title, "Banana");
    }

    #[test]
    fn paginate_slices_and_reports_totals() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
    }

    /// Build a request carrying a resolved tenant context plus a
    /// verified caller, for the tenant-gate tests below.
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        let verified = match caller_company {
            Some(company) => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
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
    fn caller_company_prefers_tenant_company() {
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company.clone())
        );
        // No tenant company: the verified caller's company is the scope.
        let req = tenant_request("", Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request(
            &uuid::Uuid::now_v7().to_string(),
            Some(&uuid::Uuid::now_v7().to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn require_caller_company_refuses_writes_without_a_resolved_company() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_caller_company(&tenant, &req).is_err());
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(require_caller_company(&tenant, &req).unwrap(), company);
    }

    #[test]
    fn validate_kebab_slug_cases() {
        assert_eq!(validate_kebab_slug("events", "post type").unwrap(), "events");
        assert_eq!(
            validate_kebab_slug("  My-Type  ", "post type").unwrap(),
            "my-type"
        );
        assert_eq!(
            validate_kebab_slug("a-b-2", "post type").unwrap(),
            "a-b-2"
        );
        for invalid in ["", "-lead", "trail-", "double--dash", "has space", "UPPER_OK", "under_score", "café"] {
            assert!(
                validate_kebab_slug(invalid, "post type").is_err(),
                "slug {invalid:?} must be rejected"
            );
        }
        assert!(validate_kebab_slug(&"a".repeat(65), "post type").is_err());
    }

    #[test]
    fn validate_field_key_cases() {
        assert_eq!(validate_field_key("event_date").unwrap(), "event_date");
        assert_eq!(validate_field_key("field2").unwrap(), "field2");
        for invalid in ["", "2fast", "_lead", "has-dash", "has space", "Upper", "a".repeat(65).as_str()] {
            assert!(
                validate_field_key(invalid).is_err(),
                "key {invalid:?} must be rejected"
            );
        }
    }

    #[test]
    fn validate_icon_name_cases() {
        assert_eq!(validate_icon_name("calendar").unwrap(), "calendar");
        assert_eq!(validate_icon_name("file-text").unwrap(), "file-text");
        assert!(validate_icon_name("has space").is_err());
        assert!(validate_icon_name("").is_err());
    }
}
