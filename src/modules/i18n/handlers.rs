// i18n -- HTTP handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json as api_json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;

use super::catalog::*;
use super::currencies::*;
use super::languages::*;
use super::locale::*;
use super::state::*;
use super::types::*;
use super::validation::*;

// ---------------------------------------------------------------------------
// Caller identity and capabilities.
//
// Write handlers resolve the caller from the `X-User-Id` / `X-Role`
// headers and enforce a manage capability through the `role.check`
// hook, exactly like the users, field_service and admissions modules.
// The check is fail-closed: the hook payload presets `"allowed":
// false`, and only an explicit `true` answer grants access, so a
// missing roles handler (or a silent one) denies. Read endpoints
// stay public: listing, translating and negotiating a language are
// unauthenticated by design.
// ---------------------------------------------------------------------------

/// Capability required to write translations (upsert, delete, and
/// bulk .po catalogue import).
pub(crate) const PERM_I18N_MANAGE: &str = "i18n.manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks).
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
    let answer = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    answer
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

/// Resolve the caller and require `permission` in one step.
pub(crate) fn require_caller_permission(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let caller = caller(req)?;
    require_permission(ctx, &caller.role, permission)?;
    Ok(caller)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_languages() -> HttpResponse {
    api_json::success_response(json!({ "languages": builtin_languages() }), 200)
}

pub(crate) async fn list_translations(
    state: web::Data<I18nState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let Some(lang_raw) = params.filters.get("lang").cloned() else {
        return api_json::error_response(
            &UnifiedError::bad_request("query parameter \"lang\" is required"),
            false,
        );
    };
    let lang = match validate_lang(&lang_raw) {
        Ok(lang) => lang,
        Err(error) => return api_json::error_response(&error, false),
    };
    let search = params.search.clone();
    let mut items: Vec<Translation> = state
        .translations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|translation| translation.lang == lang)
        .filter(|translation| {
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return translation.key.to_lowercase().contains(&needle)
                    || translation.value.to_lowercase().contains(&needle);
            }
            true
        })
        .collect();
    let sort = params.allowed_sort(&["key", "updated_at"], "key");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "updated_at" => a.updated_at.cmp(&b.updated_at),
            _ => a.key.cmp(&b.key),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<Translation> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(
        PageResult::new(page_items, total, &pagination).to_json(),
        200,
    )
}

pub(crate) async fn upsert_translation(
    state: web::Data<I18nState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<UpsertTranslationInput>,
) -> HttpResponse {
    if let Err(error) = require_caller_permission(&ctx, &req, PERM_I18N_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let input = input.into_inner();
    let outcome = (|| -> Result<Translation, UnifiedError> {
        let lang = validate_lang(&input.lang)?;
        let key = validate_key(&input.key)?;
        let value = input.value.trim();
        if value.is_empty() {
            return Err(UnifiedError::bad_request("value must not be empty"));
        }
        if value.chars().count() > 10_000 {
            return Err(UnifiedError::bad_request(
                "value must be at most 10000 characters",
            ));
        }
        Ok(Translation {
            lang,
            key,
            value: value.to_string(),
            updated_at: Utc::now(),
        })
    })();
    let translation = match outcome {
        Ok(translation) => translation,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    // Remember the previous entry so a persist failure can roll the
    // in-memory insert back instead of leaving memory ahead of disk
    // while reporting an error.
    let map_key = (translation.lang.clone(), translation.key.clone());
    let previous = state
        .translations
        .get(&map_key)
        .map(|entry| entry.value().clone());
    state
        .translations
        .insert(map_key.clone(), translation.clone());
    if let Err(error) = state.persist_lang(&translation.lang).await {
        match previous {
            Some(previous) => {
                state.translations.insert(map_key, previous);
            }
            None => {
                state.translations.remove(&map_key);
            }
        }
        return api_json::error_response(&error, ctx.is_debug());
    }
    ctx.hooks.emit(
        "i18n.translation.updated",
        json!({
            "lang": translation.lang,
            "key": translation.key,
            "value": translation.value,
        }),
    );
    api_json::success_response(translation, 200)
}

pub(crate) async fn translate(
    state: web::Data<I18nState>,
    input: web::Json<TranslateInput>,
) -> HttpResponse {
    let input = input.into_inner();
    let lang = match validate_lang(&input.lang) {
        Ok(lang) => lang,
        Err(error) => return api_json::error_response(&error, false),
    };
    // Gettext msgids are free-form source sentences, not
    // dot-separated admin keys, so only the length is validated here.
    let key = match validate_lookup_key(&input.key) {
        Ok(key) => key,
        Err(error) => return api_json::error_response(&error, false),
    };
    if let Some(value) = state.translate_with_context(&lang, &key, input.context.as_deref()) {
        return api_json::success_response(
            json!({
                "lang": lang,
                "key": key,
                "value": value,
                "translated": true,
            }),
            200,
        );
    }
    match input.fallback {
        Some(fallback) => api_json::success_response(
            json!({
                "lang": lang,
                "key": key,
                "value": fallback,
                "translated": false,
            }),
            200,
        ),
        None => api_json::error_response(
            &UnifiedError::not_found(format!(
                "no translation for key \"{key}\" in language \"{lang}\""
            )),
            false,
        ),
    }
}

pub(crate) async fn translate_plural_handler(
    state: web::Data<I18nState>,
    input: web::Json<TranslatePluralInput>,
) -> HttpResponse {
    let input = input.into_inner();
    let lang = match validate_lang(&input.lang) {
        Ok(lang) => lang,
        Err(error) => return api_json::error_response(&error, false),
    };
    let singular = input.singular.trim();
    let plural = input.plural.trim();
    if singular.is_empty() || plural.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("singular and plural must not be empty"),
            false,
        );
    }
    if singular.chars().count() > 10_000 || plural.chars().count() > 10_000 {
        return api_json::error_response(
            &UnifiedError::bad_request("singular and plural must be at most 10000 characters"),
            false,
        );
    }
    let context = input
        .context
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let (value, index, translated) =
        state.translate_plural(&lang, singular, plural, input.count, context);
    api_json::success_response(
        json!({
            "lang": lang,
            "singular": singular,
            "plural": plural,
            "count": input.count,
            "index": index,
            "form_count": plural_form_count(&lang),
            "value": value,
            "translated": translated,
        }),
        200,
    )
}

pub(crate) async fn load_po_catalog_handler(
    state: web::Data<I18nState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<LoadPoCatalogInput>,
) -> HttpResponse {
    if let Err(error) = require_caller_permission(&ctx, &req, PERM_I18N_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let input = input.into_inner();
    let lang = match validate_lang(&input.lang) {
        Ok(lang) => lang,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    if input.content.is_empty() || input.content.chars().count() > 2_000_000 {
        return api_json::error_response(
            &UnifiedError::bad_request(
                "content must be a non-empty .po catalogue of at most 2000000 characters",
            ),
            ctx.is_debug(),
        );
    }
    // Snapshot the entries this import will touch (same skip and key
    // rules as `load_po_catalog`) so a persist failure can roll the
    // in-memory store back instead of leaving memory ahead of disk
    // while reporting an error.
    let touched: Vec<(String, String)> = parse_po_catalog(&input.content)
        .iter()
        .filter(|entry| !entry.msgid.is_empty() && !entry.fuzzy)
        .map(|entry| {
            let key = match &entry.msgctxt {
                Some(context) => contextual_key(&entry.msgid, context),
                None => entry.msgid.clone(),
            };
            (lang.clone(), key)
        })
        .collect();
    let previous: Vec<(
        (String, String),
        Option<Translation>,
        Option<Vec<String>>,
    )> = touched
        .iter()
        .map(|key| {
            (
                key.clone(),
                state.translations.get(key).map(|entry| entry.value().clone()),
                state.plurals.get(key).map(|entry| entry.value().clone()),
            )
        })
        .collect();
    let result = state.load_po_catalog(&lang, &input.content);
    if let Err(error) = state.persist_lang(&lang).await {
        for (key, previous_translation, previous_plurals) in previous {
            match previous_translation {
                Some(translation) => {
                    state.translations.insert(key.clone(), translation);
                }
                None => {
                    state.translations.remove(&key);
                }
            }
            match previous_plurals {
                Some(forms) => {
                    state.plurals.insert(key, forms);
                }
                None => {
                    state.plurals.remove(&key);
                }
            }
        }
        return api_json::error_response(&error, ctx.is_debug());
    }
    ctx.hooks.emit(
        "i18n.catalog.loaded",
        json!({
            "lang": lang,
            "loaded": result.loaded,
            "plural_entries": result.plural_entries,
            "skipped_fuzzy": result.skipped_fuzzy,
        }),
    );
    api_json::success_response(json!({ "lang": lang, "result": result }), 200)
}

pub(crate) async fn negotiate_language_handler(req: actix_web::HttpRequest) -> HttpResponse {
    let header = req
        .headers()
        .get("accept-language")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let matched = negotiate_language(header);
    api_json::success_response(
        json!({
            "requested": header,
            "matched": matched,
            "languages": builtin_languages(),
        }),
        200,
    )
}

pub(crate) async fn list_currencies() -> HttpResponse {
    api_json::success_response(json!({ "currencies": builtin_currencies() }), 200)
}

pub(crate) async fn get_currency(path: web::Path<String>) -> HttpResponse {
    let code = path.into_inner();
    match currency_info(&code) {
        Some(info) => api_json::success_response(json!({ "currency": info }), 200),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("unknown currency code \"{code}\"")),
            false,
        ),
    }
}

pub(crate) async fn delete_translation(
    state: web::Data<I18nState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    if let Err(error) = require_caller_permission(&ctx, &req, PERM_I18N_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let (lang_raw, key_raw) = path.into_inner();
    let lang = match validate_lang(&lang_raw) {
        Ok(lang) => lang,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let key = match validate_key(&key_raw) {
        Ok(key) => key,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    match state.translations.remove(&(lang.clone(), key.clone())) {
        Some((_, translation)) => {
            if let Err(error) = state.persist_lang(&lang).await {
                // Roll the removal back: a persist failure must not
                // leave memory without a translation disk still has.
                state.translations.insert(
                    (translation.lang.clone(), translation.key.clone()),
                    translation,
                );
                return api_json::error_response(&error, ctx.is_debug());
            }
            ctx.hooks.emit(
                "i18n.translation.deleted",
                json!({ "lang": translation.lang, "key": translation.key }),
            );
            api_json::success_response(
                json!({ "lang": translation.lang, "key": translation.key, "deleted": true }),
                200,
            )
        }
        None => api_json::error_response(
            &UnifiedError::not_found(format!(
                "no translation for key \"{key}\" in language \"{lang}\""
            )),
            ctx.is_debug(),
        ),
    }
}

// Tests for caller identity, capability gating and the write
// handlers (including persist-failure rollback) in this file.
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

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context whose `role.check` handler answers from a fixed
    /// allow-list of permissions.
    fn allowing_context(allowed: &[&str]) -> AppContext {
        let ctx = test_context();
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(move |context: &mut unified_core::hooks::HookContext| {
                let permission = context
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                context.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        ctx
    }

    fn authed_request(role: &str) -> HttpRequest {
        verified_request("user-1", role)
    }

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("unified-i18n-handlers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
        let no_role = TestRequest::default().to_http_request();
        assert!(caller(&no_role).is_err());
        let blank = TestRequest::default().to_http_request();
        assert!(caller(&blank).is_err());
    }

    #[test]
    fn caller_reads_the_verified_caller() {
        let request = verified_request("user-1", "operator");
        let caller = caller(&request).expect("caller");
        assert_eq!(caller.actor, test_user_uuid("user-1").to_string());
        assert_eq!(caller.role, "operator");
    }

    #[test]
    fn check_capability_fails_closed_without_a_role_check_answer() {
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", PERM_I18N_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_I18N_MANAGE).is_err());
    }

    #[test]
    fn check_capability_honours_the_role_check_answer() {
        let ctx = allowing_context(&[PERM_I18N_MANAGE]);
        assert!(check_capability(&ctx, "operator", PERM_I18N_MANAGE));
        assert!(!check_capability(&ctx, "operator", "i18n.view"));
        assert!(require_permission(&ctx, "operator", PERM_I18N_MANAGE).is_ok());
        assert!(require_permission(&ctx, "operator", "i18n.view").is_err());
    }

    #[test]
    fn require_caller_permission_needs_a_verified_caller_and_permission() {
        let ctx = allowing_context(&[PERM_I18N_MANAGE]);
        assert!(require_caller_permission(
            &ctx,
            &TestRequest::default().to_http_request(),
            PERM_I18N_MANAGE
        )
        .is_err());
        let request = authed_request("operator");
        let allowed = require_caller_permission(&ctx, &request, PERM_I18N_MANAGE)
            .expect("operator holds i18n.manage in this test context");
        assert_eq!(allowed.actor, test_user_uuid("user-1").to_string());
        assert_eq!(allowed.role, "operator");
    }

    #[tokio::test]
    async fn upsert_translation_rejects_unauthenticated_and_unauthorized_callers() {
        let dir = test_dir("upsert-denied");
        let state = I18nState::new(dir);
        let ctx = Arc::new(test_context());
        let input = || {
            web::Json(UpsertTranslationInput {
                lang: "en".to_string(),
                key: "nav.test".to_string(),
                value: "Test".to_string(),
            })
        };
        // No headers at all: unauthenticated.
        let response = upsert_translation(
            web::Data::new(state.clone()),
            web::Data::new(Arc::clone(&ctx)),
            TestRequest::default().to_http_request(),
            input(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // Headers present, but the role holds no manage capability.
        let response = upsert_translation(
            web::Data::new(state.clone()),
            web::Data::new(Arc::clone(&ctx)),
            authed_request("viewer"),
            input(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(state
            .translations
            .get(&("en".to_string(), "nav.test".to_string()))
            .is_none());
    }

    #[tokio::test]
    async fn upsert_translation_allows_manager_and_persists() {
        let dir = test_dir("upsert-allowed");
        let state = I18nState::new(dir.clone());
        let ctx = Arc::new(allowing_context(&[PERM_I18N_MANAGE]));
        let response = upsert_translation(
            web::Data::new(state.clone()),
            web::Data::new(Arc::clone(&ctx)),
            authed_request("admin"),
            web::Json(UpsertTranslationInput {
                lang: "en".to_string(),
                key: "nav.test".to_string(),
                value: "Test".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            state
                .translations
                .get(&("en".to_string(), "nav.test".to_string()))
                .map(|entry| entry.value().value.clone()),
            Some("Test".to_string())
        );
        assert!(dir.join("en.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn upsert_translation_rolls_back_on_persist_failure() {
        // The state directory is a plain file, so persisting fails.
        let dir = test_dir("upsert-rollback");
        std::fs::write(&dir, "not a directory").expect("write file");
        let state = I18nState::new(dir.clone());
        let ctx = Arc::new(allowing_context(&[PERM_I18N_MANAGE]));
        let response = upsert_translation(
            web::Data::new(state.clone()),
            web::Data::new(Arc::clone(&ctx)),
            authed_request("admin"),
            web::Json(UpsertTranslationInput {
                lang: "en".to_string(),
                key: "nav.test".to_string(),
                value: "Test".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // Memory was rolled back: the failed write left no trace.
        assert!(state
            .translations
            .get(&("en".to_string(), "nav.test".to_string()))
            .is_none());
        let _ = std::fs::remove_file(&dir);
    }

    #[tokio::test]
    async fn delete_translation_rejects_unauthenticated_caller() {
        let dir = test_dir("delete-denied");
        let state = I18nState::new(dir);
        let ctx = Arc::new(test_context());
        let response = delete_translation(
            web::Data::new(state.clone()),
            web::Data::new(Arc::clone(&ctx)),
            TestRequest::default().to_http_request(),
            web::Path::from(("en".to_string(), "nav.dashboard".to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // The seeded translation is untouched.
        assert!(state
            .translations
            .get(&("en".to_string(), "nav.dashboard".to_string()))
            .is_some());
    }

    #[tokio::test]
    async fn delete_translation_rolls_back_on_persist_failure() {
        let dir = test_dir("delete-rollback");
        std::fs::write(&dir, "not a directory").expect("write file");
        let state = I18nState::new(dir.clone());
        let ctx = Arc::new(allowing_context(&[PERM_I18N_MANAGE]));
        let response = delete_translation(
            web::Data::new(state.clone()),
            web::Data::new(Arc::clone(&ctx)),
            authed_request("admin"),
            web::Path::from(("en".to_string(), "nav.dashboard".to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // The removal was rolled back: disk still has the entry, and
        // so does memory.
        assert!(state
            .translations
            .get(&("en".to_string(), "nav.dashboard".to_string()))
            .is_some());
        let _ = std::fs::remove_file(&dir);
    }

    #[tokio::test]
    async fn load_po_catalog_handler_rejects_unauthenticated_caller() {
        let dir = test_dir("po-denied");
        let state = I18nState::new(dir);
        let ctx = Arc::new(test_context());
        let response = load_po_catalog_handler(
            web::Data::new(state.clone()),
            web::Data::new(Arc::clone(&ctx)),
            TestRequest::default().to_http_request(),
            web::Json(LoadPoCatalogInput {
                lang: "en".to_string(),
                content: "msgid \"Hello\"\nmsgstr \"Halo\"\n".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(state
            .translations
            .get(&("en".to_string(), "Hello".to_string()))
            .is_none());
    }

    #[tokio::test]
    async fn translate_accepts_free_form_msgid_keys() {
        let dir = test_dir("translate-msgid");
        let state = I18nState::new(dir);
        state.load_po_catalog("en", "msgid \"Hello\"\nmsgstr \"Halo\"\n");
        let response = translate(
            web::Data::new(state),
            web::Json(TranslateInput {
                lang: "en".to_string(),
                key: "Hello".to_string(),
                fallback: None,
                context: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("response body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
        assert_eq!(value["data"]["value"], "Halo");
        assert_eq!(value["data"]["translated"], true);
    }
}
