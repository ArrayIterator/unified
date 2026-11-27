//! Content Management module. - Dynamic custom post types.
//!
//! Admin-definable post types: every setting lives on the
//! `PostTypeDefinition` row (labels, icon, supports, dynamic field
//! definitions, visibility, archive/menu/REST flags, enabled).
//! There is no per-type code anywhere:
//!
//! * API routes are the generic `/api/v1/content/custom/{type_slug}`
//!   set below - registering a definition makes the type usable
//!   immediately, with no deploy.
//! * The admin menu and the React list/form screens are generated
//!   from the definition (`GET /api/v1/content/post-types` +
//!   `assets/react/src/pages/CustomEntriesPage.tsx`): ONE generic
//!   component renders any type's fields.
//! * Public render is `GET .../custom/{type_slug}/slug/{slug}` for
//!   enabled public types (published entries only).
//!
//! Entries of ALL types live in ONE generic store
//! (`content_custom_entries`, JSONB `fields`); there is no table
//! per type. Field values are validated per their definition on
//! every write (type-check, required, select allowlist).
//!
//! Disabling (or deleting) a type hides its routes and UI but
//! KEEPS its entries: entries are preserved and reappear if the
//! type is re-enabled.
//!
//! C3 guards: every handler resolves the tenant first
//! (`require_tenant`), checks the caller company against it, and
//! requires a capability (`content.view` to read, `content.create`
//! to create entries, `content.manage` to manage types and entries).
//! Definitions and entries carry the tenant id and are always
//! filtered on it.

use super::ai_capabilities::*;
use super::categories::*;
use super::helpers::*;
use super::module::*;
use super::pages::*;
use super::posts::*;
use super::state::*;
use super::tags::*;
use super::types::*;

use actix_web::web;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::json;
use serde_json::{Map, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json as api_json;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

// Definition validation.

/// Validate a `supports` list: known feature names only, no
/// duplicates. An absent list means "no built-in features".
fn validate_supports(supports: &[String]) -> Result<Vec<String>, UnifiedError> {
    let mut kept: Vec<String> = Vec::new();
    for feature in supports {
        let feature = feature.trim().to_lowercase();
        if !SUPPORTED_FEATURES.contains(&feature.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "unsupported feature \"{feature}\": expected one of {}",
                SUPPORTED_FEATURES.join(", ")
            )));
        }
        if !kept.contains(&feature) {
            kept.push(feature);
        }
    }
    Ok(kept)
}

/// Validate one field definition (key shape, label length, options
/// rules) and its default value against itself.
fn validate_field_def(def: &CustomFieldDef) -> Result<CustomFieldDef, UnifiedError> {
    let key = validate_field_key(&def.key)?;
    let label = def.label.trim();
    if label.is_empty() {
        return Err(UnifiedError::bad_request("field label must not be empty"));
    }
    if label.chars().count() > 200 {
        return Err(UnifiedError::bad_request(
            "field label must be at most 200 characters",
        ));
    }
    match def.field_type {
        CustomFieldType::Select | CustomFieldType::Multiselect => {
            if def.options.is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "field \"{key}\" of type {:?} needs at least one option",
                    def.field_type
                )));
            }
            if def.options.len() > 100 {
                return Err(UnifiedError::bad_request(format!(
                    "field \"{key}\" must have at most 100 options"
                )));
            }
            let mut seen: Vec<String> = Vec::new();
            for option in &def.options {
                let option = option.trim();
                if option.is_empty() || option.chars().count() > 200 {
                    return Err(UnifiedError::bad_request(format!(
                        "field \"{key}\" options must be 1-200 characters"
                    )));
                }
                if seen.contains(&option.to_string()) {
                    return Err(UnifiedError::bad_request(format!(
                        "field \"{key}\" options must be unique"
                    )));
                }
                seen.push(option.to_string());
            }
        }
        _ => {
            if !def.options.is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "field \"{key}\" of type {:?} must not have options",
                    def.field_type
                )));
            }
        }
    }
    let mut def = def.clone();
    def.key = key;
    def.label = label.to_string();
    def.options = def
        .options
        .iter()
        .map(|option| option.trim().to_string())
        .collect();
    if let Some(default) = &def.default {
        // The default must itself be a valid value for the field.
        validate_field_value(&def, default)?;
    }
    Ok(def)
}

/// Validate a whole field-definition list: keys unique, at most 100
/// fields per type.
fn validate_field_defs(defs: &[CustomFieldDef]) -> Result<Vec<CustomFieldDef>, UnifiedError> {
    if defs.len() > 100 {
        return Err(UnifiedError::bad_request(
            "a post type must have at most 100 custom fields",
        ));
    }
    let mut kept: Vec<CustomFieldDef> = Vec::new();
    for def in defs {
        let def = validate_field_def(def)?;
        if kept.iter().any(|item| item.key == def.key) {
            return Err(UnifiedError::bad_request(format!(
                "duplicate field key \"{}\"",
                def.key
            )));
        }
        kept.push(def);
    }
    Ok(kept)
}

/// Validate one field value against its definition. Returns the
/// normalized value (media objects collapse to their document id).
/// A missing value is NOT an error here - required-ness is checked
/// by `validate_entry_fields`.
fn validate_field_value(def: &CustomFieldDef, value: &Value) -> Result<Value, UnifiedError> {
    let bad = |detail: &str| {
        UnifiedError::bad_request(format!(
            "field \"{}\" ({:?}): {detail}",
            def.key, def.field_type
        ))
    };
    match def.field_type {
        CustomFieldType::Text => {
            let text = value.as_str().ok_or_else(|| bad("must be a string"))?;
            if text.chars().count() > 500 {
                return Err(bad("must be at most 500 characters"));
            }
            Ok(Value::String(text.to_string()))
        }
        CustomFieldType::Textarea => {
            let text = value.as_str().ok_or_else(|| bad("must be a string"))?;
            if text.chars().count() > 100_000 {
                return Err(bad("must be at most 100,000 characters"));
            }
            Ok(Value::String(text.to_string()))
        }
        CustomFieldType::Richtext => {
            let text = value.as_str().ok_or_else(|| bad("must be a string"))?;
            if text.chars().count() > 1_000_000 {
                return Err(bad("must be at most 1,000,000 characters"));
            }
            Ok(Value::String(text.to_string()))
        }
        CustomFieldType::Number => {
            let number = value.as_number().ok_or_else(|| bad("must be a number"))?;
            if let Some(float) = number.as_f64() {
                if !float.is_finite() {
                    return Err(bad("must be a finite number"));
                }
            }
            Ok(Value::Number(number.clone()))
        }
        CustomFieldType::Date => {
            let text = value.as_str().ok_or_else(|| bad("must be a string"))?;
            NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .map_err(|_| bad("must be a date in YYYY-MM-DD format"))?;
            Ok(Value::String(text.to_string()))
        }
        CustomFieldType::Datetime => {
            let text = value.as_str().ok_or_else(|| bad("must be a string"))?;
            DateTime::parse_from_rfc3339(text).map_err(|_| bad("must be an RFC 3339 date-time"))?;
            Ok(Value::String(text.to_string()))
        }
        CustomFieldType::Select => {
            let text = value.as_str().ok_or_else(|| bad("must be a string"))?;
            if !def.options.iter().any(|option| option == text) {
                return Err(bad("must be one of the field's options"));
            }
            Ok(Value::String(text.to_string()))
        }
        CustomFieldType::Multiselect => {
            let items = value.as_array().ok_or_else(|| bad("must be an array"))?;
            if items.len() > 100 {
                return Err(bad("must have at most 100 selected options"));
            }
            let mut kept: Vec<Value> = Vec::new();
            for item in items {
                let text = item
                    .as_str()
                    .ok_or_else(|| bad("must be an array of strings"))?;
                if !def.options.iter().any(|option| option == text) {
                    return Err(bad("must contain only the field's options"));
                }
                if !kept.iter().any(|kept| kept.as_str() == Some(text)) {
                    kept.push(Value::String(text.to_string()));
                }
            }
            Ok(Value::Array(kept))
        }
        CustomFieldType::Checkbox => {
            let flag = value.as_bool().ok_or_else(|| bad("must be a boolean"))?;
            Ok(Value::Bool(flag))
        }
        CustomFieldType::Media => {
            // A document reference: either the document id directly
            // or an object carrying it. Normalized to the id string.
            if let Some(document_id) = value.as_str() {
                if document_id.is_empty() || document_id.chars().count() > 500 {
                    return Err(bad("must be a document id of 1-500 characters"));
                }
                return Ok(Value::String(document_id.to_string()));
            }
            if let Some(object) = value.as_object() {
                if let Some(Value::String(document_id)) = object.get("document_id") {
                    if document_id.is_empty() || document_id.chars().count() > 500 {
                        return Err(bad("must be a document id of 1-500 characters"));
                    }
                    return Ok(Value::String(document_id.clone()));
                }
            }
            Err(bad(
                "must be a document id string or {\"document_id\": \"...\"}",
            ))
        }
        CustomFieldType::Relation => {
            let text = value.as_str().ok_or_else(|| bad("must be a string"))?;
            let _id = Uuid::parse_str(text).map_err(|_| bad("must be a UUID"))?;
            Ok(Value::String(text.to_string()))
        }
    }
}

/// Validate a whole entry `fields` map against the type's
/// definitions: unknown keys are rejected, every supplied value is
/// type-checked, missing values fall back to the field default, and
/// a missing required field (with no default) is an error.
fn validate_entry_fields(
    defs: &[CustomFieldDef],
    provided: &Map<String, Value>,
) -> Result<Map<String, Value>, UnifiedError> {
    for key in provided.keys() {
        if !defs.iter().any(|def| &def.key == key) {
            return Err(UnifiedError::bad_request(format!(
                "unknown field \"{key}\" for this post type"
            )));
        }
    }
    let mut out = Map::new();
    for def in defs {
        match provided.get(&def.key) {
            Some(value) => {
                if value.is_null() {
                    if def.required {
                        return Err(UnifiedError::bad_request(format!(
                            "field \"{}\" is required",
                            def.key
                        )));
                    }
                    // Null for an optional field means "absent":
                    // fall through to the default.
                    if let Some(default) = &def.default {
                        out.insert(def.key.clone(), validate_field_value(def, default)?);
                    }
                } else {
                    out.insert(def.key.clone(), validate_field_value(def, value)?);
                }
            }
            None => {
                if let Some(default) = &def.default {
                    out.insert(def.key.clone(), validate_field_value(def, default)?);
                } else if def.required {
                    return Err(UnifiedError::bad_request(format!(
                        "field \"{}\" is required",
                        def.key
                    )));
                }
            }
        }
    }
    Ok(out)
}

/// A relation field value must reference something that exists: a
/// post, a page, or a custom entry of this tenant.
fn relation_target_exists(state: &ContentState, tenant_id: Uuid, value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    let Ok(id) = Uuid::parse_str(text) else {
        return false;
    };
    state.posts.contains_key(&id)
        || state.pages.contains_key(&id)
        || state
            .custom_entries
            .get(&id)
            .is_some_and(|entry| entry.value().tenant_id == tenant_id)
}

// Type resolution + visibility.

/// Find a type definition by slug within this tenant.
pub(super) fn find_type_by_slug(
    state: &ContentState,
    tenant_id: Uuid,
    slug: &str,
) -> Option<PostTypeDefinition> {
    state
        .post_types
        .iter()
        .map(|entry| entry.value().clone())
        .find(|definition| definition.tenant_id == tenant_id && definition.slug == slug)
}

/// Does this caller hold `content.manage`? Used for visibility
/// decisions (private/disabled types, drafts). Anonymous callers
/// never do.
fn caller_can_manage(ctx: &AppContext, req: &HttpRequest) -> bool {
    match caller(req) {
        Ok(found) => check_capability(ctx, &found.role, PERM_CONTENT_MANAGE),
        Err(_) => false,
    }
}

/// Resolve a type slug for a read: unknown slugs are 404, and a
/// disabled type, a private type, or a non-REST type is 404 for
/// callers without `content.manage` (no existence leak).
fn resolve_type_for_read(
    state: &ContentState,
    ctx: &AppContext,
    req: &HttpRequest,
    tenant_id: Uuid,
    slug: &str,
) -> Result<PostTypeDefinition, UnifiedError> {
    let definition = find_type_by_slug(state, tenant_id, slug)
        .ok_or_else(|| UnifiedError::not_found("post type not found"))?;
    if caller_can_manage(ctx, req) {
        return Ok(definition);
    }
    if !definition.enabled
        || definition.visibility != PostTypeVisibility::Public
        || !definition.rest_exposed
    {
        return Err(UnifiedError::not_found("post type not found"));
    }
    Ok(definition)
}

/// Resolve a type slug for a write: the type must exist and be
/// enabled. Visibility does not block writes (the capability
/// check already ran); a disabled type refuses new entries.
fn resolve_type_for_write(
    state: &ContentState,
    tenant_id: Uuid,
    slug: &str,
) -> Result<PostTypeDefinition, UnifiedError> {
    let definition = find_type_by_slug(state, tenant_id, slug)
        .ok_or_else(|| UnifiedError::not_found("post type not found"))?;
    if !definition.enabled {
        return Err(UnifiedError::conflict(
            "post type is disabled: entries are preserved but no new entries can be created",
        ));
    }
    Ok(definition)
}

/// May this caller read this entry? Disabled or private types and
/// drafts are visible only to `content.manage` holders; published
/// entries of enabled public types are visible to any viewer.
fn entry_readable(definition: &PostTypeDefinition, entry: &CustomEntry, can_manage: bool) -> bool {
    if can_manage {
        return true;
    }
    definition.enabled
        && definition.visibility == PostTypeVisibility::Public
        && definition.rest_exposed
        && entry.status == ContentStatus::Published
}

/// Emit the shared `content.published` / `content.unpublished`
/// hooks for a custom entry (kind `custom:{type_slug}`), so the
/// documents module flips attached media the same way as for
/// posts and pages. No attachments are carried: custom entries
/// reference media through `thumbnail` / media fields, not the
/// post attachment list.
fn emit_custom_published(ctx: &AppContext, entry: &CustomEntry, tenant_id: Uuid, published: bool) {
    let kind = format!("custom:{}", entry.type_slug);
    if published {
        emit_published(
            ctx,
            &kind,
            &entry.id,
            &entry.title,
            &entry.slug,
            &entry.content,
            tenant_id,
            &[],
        );
    } else {
        emit_unpublished(
            ctx,
            &kind,
            &entry.id,
            &entry.title,
            &entry.slug,
            tenant_id,
            &[],
        );
    }
}

fn emit_type_event(ctx: &AppContext, event: &str, definition: &PostTypeDefinition) {
    ctx.hooks.emit(
        event,
        json!({
            "id": definition.id.to_string(),
            "slug": definition.slug,
            "tenant_id": definition.tenant_id.to_string(),
            "enabled": definition.enabled,
        }),
    );
}

// Post type definition CRUD.

pub(super) async fn list_post_types(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let search = params.search.clone();
    let enabled_filter = params.filters.get("enabled").map(|raw| raw == "true");
    let visibility_filter = params.filters.get("visibility").cloned();
    let mut items: Vec<PostTypeDefinition> = state
        .post_types
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|definition| definition.tenant_id == tenant.tenant_id)
        .filter(|definition| {
            if let Some(wanted) = enabled_filter {
                if definition.enabled != wanted {
                    return false;
                }
            }
            if let Some(visibility) = &visibility_filter {
                let rendered = match definition.visibility {
                    PostTypeVisibility::Public => "public",
                    PostTypeVisibility::Private => "private",
                };
                if rendered != visibility {
                    return false;
                }
            }
            if let Some(needle) = &search {
                if !matches_search(&definition.plural_label, needle)
                    && !matches_search(&definition.singular_label, needle)
                    && !matches_search(&definition.slug, needle)
                {
                    return false;
                }
            }
            true
        })
        .collect();
    items = sort_post_types(items, &params);
    api_json::success_response(paginate(items, &params).to_json(), 200)
}

pub(super) async fn create_post_type(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    input: web::Json<CreatePostTypeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let input = input.into_inner();
    let outcome = (|| -> Result<PostTypeDefinition, UnifiedError> {
        let slug = validate_kebab_slug(&input.slug, "post type")?;
        if find_type_by_slug(&state, tenant.tenant_id, &slug).is_some() {
            return Err(UnifiedError::conflict(format!(
                "post type slug \"{slug}\" already exists"
            )));
        }
        let singular_label = input.singular_label.trim();
        let plural_label = input.plural_label.trim();
        for (label, what) in [
            (singular_label, "singular label"),
            (plural_label, "plural label"),
        ] {
            if label.is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "{what} must not be empty"
                )));
            }
            if label.chars().count() > 200 {
                return Err(UnifiedError::bad_request(format!(
                    "{what} must be at most 200 characters"
                )));
            }
        }
        if let Some(description) = &input.description {
            if description.chars().count() > 1000 {
                return Err(UnifiedError::bad_request(
                    "description must be at most 1000 characters",
                ));
            }
        }
        let icon = match &input.icon {
            Some(icon) => validate_icon_name(icon)?,
            None => "document".to_string(),
        };
        let supports = validate_supports(&input.supports)?;
        let fields = validate_field_defs(&input.fields)?;
        let now = Utc::now();
        let definition = PostTypeDefinition {
            id: Uuid::now_v7(),
            tenant_id: tenant.tenant_id,
            slug,
            singular_label: singular_label.to_string(),
            plural_label: plural_label.to_string(),
            icon,
            description: input.description,
            supports,
            fields,
            visibility: input.visibility.unwrap_or_default(),
            has_archive: input.has_archive.unwrap_or(false),
            show_in_menu: input.show_in_menu.unwrap_or(false),
            menu_position: input.menu_position.unwrap_or(0),
            rest_exposed: input.rest_exposed.unwrap_or(true),
            enabled: input.enabled.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.post_types.insert(definition.id, definition.clone());
        Ok(definition)
    })();
    match outcome {
        Ok(definition) => {
            emit_type_event(&ctx, "content.post_type_registered", &definition);
            api_json::success_response(definition, 201)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn get_post_type(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    match state.post_types.get(&id.into_inner()) {
        Some(definition) if definition.value().tenant_id == tenant.tenant_id => {
            api_json::success_response(definition.value().clone(), 200)
        }
        _ => api_json::error_response(
            &UnifiedError::not_found("post type not found"),
            ctx.is_debug(),
        ),
    }
}

pub(super) async fn update_post_type(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    input: web::Json<UpdatePostTypeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let id = id.into_inner();
    let input = input.into_inner();
    let outcome = (|| -> Result<PostTypeDefinition, UnifiedError> {
        let mut definition = state
            .post_types
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|definition| definition.tenant_id == tenant.tenant_id)
            .ok_or_else(|| UnifiedError::not_found("post type not found"))?;
        let old_slug = definition.slug.clone();
        if let Some(slug) = &input.slug {
            let slug = validate_kebab_slug(slug, "post type")?;
            if slug != definition.slug
                && find_type_by_slug(&state, tenant.tenant_id, &slug).is_some()
            {
                return Err(UnifiedError::conflict(format!(
                    "post type slug \"{slug}\" already exists"
                )));
            }
            definition.slug = slug;
        }
        if let Some(label) = &input.singular_label {
            let label = label.trim();
            if label.is_empty() || label.chars().count() > 200 {
                return Err(UnifiedError::bad_request(
                    "singular label must be 1-200 characters",
                ));
            }
            definition.singular_label = label.to_string();
        }
        if let Some(label) = &input.plural_label {
            let label = label.trim();
            if label.is_empty() || label.chars().count() > 200 {
                return Err(UnifiedError::bad_request(
                    "plural label must be 1-200 characters",
                ));
            }
            definition.plural_label = label.to_string();
        }
        if let Some(icon) = &input.icon {
            definition.icon = validate_icon_name(icon)?;
        }
        if let Some(description) = input.description {
            if description.chars().count() > 1000 {
                return Err(UnifiedError::bad_request(
                    "description must be at most 1000 characters",
                ));
            }
            definition.description = Some(description);
        }
        if let Some(supports) = &input.supports {
            definition.supports = validate_supports(supports)?;
        }
        if let Some(fields) = &input.fields {
            definition.fields = validate_field_defs(fields)?;
        }
        if let Some(visibility) = input.visibility {
            definition.visibility = visibility;
        }
        if let Some(has_archive) = input.has_archive {
            definition.has_archive = has_archive;
        }
        if let Some(show_in_menu) = input.show_in_menu {
            definition.show_in_menu = show_in_menu;
        }
        if let Some(menu_position) = input.menu_position {
            definition.menu_position = menu_position;
        }
        if let Some(rest_exposed) = input.rest_exposed {
            definition.rest_exposed = rest_exposed;
        }
        if let Some(enabled) = input.enabled {
            definition.enabled = enabled;
        }
        definition.updated_at = Utc::now();
        // A slug rename re-keys every entry of the type so the
        // generic routes keep finding them under the new slug.
        if definition.slug != old_slug {
            for mut entry in state.custom_entries.iter_mut() {
                if entry.value().tenant_id == tenant.tenant_id
                    && entry.value().type_slug == old_slug
                {
                    entry.value_mut().type_slug = definition.slug.clone();
                }
            }
        }
        state.post_types.insert(id, definition.clone());
        Ok(definition)
    })();
    match outcome {
        Ok(definition) => api_json::success_response(definition, 200),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

/// Delete a post type: a SOFT disable. The definition stays (with
/// `enabled: false`) and every entry is preserved; the type's
/// routes and menu entry disappear until it is re-enabled. There
/// is no hard delete for a type - deleting a type must never
/// destroy its entries.
pub(super) async fn delete_post_type(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    set_post_type_enabled(state, ctx, req, id, false, "content.post_type_disabled").await
}

pub(super) async fn enable_post_type(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    set_post_type_enabled(state, ctx, req, id, true, "content.post_type_enabled").await
}

pub(super) async fn disable_post_type(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    set_post_type_enabled(state, ctx, req, id, false, "content.post_type_disabled").await
}

async fn set_post_type_enabled(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    enabled: bool,
    event: &'static str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let id = id.into_inner();
    match state.post_types.get(&id).map(|entry| entry.value().clone()) {
        Some(mut definition) if definition.tenant_id == tenant.tenant_id => {
            definition.enabled = enabled;
            definition.updated_at = Utc::now();
            state.post_types.insert(id, definition.clone());
            emit_type_event(&ctx, event, &definition);
            api_json::success_response(definition, 200)
        }
        _ => api_json::error_response(
            &UnifiedError::not_found("post type not found"),
            ctx.is_debug(),
        ),
    }
}

// Custom entries.

/// Check the entry body against the type's `supports`: values for
/// unsupported built-in features are rejected instead of silently
/// dropped.
fn check_supports(
    definition: &PostTypeDefinition,
    content: &str,
    excerpt: Option<&str>,
    thumbnail: Option<&str>,
    category_id: Option<Uuid>,
    tag_ids: &[Uuid],
    fields: &Map<String, Value>,
) -> Result<(), UnifiedError> {
    let supports = |feature: &str| definition.supports.iter().any(|item| item == feature);
    if !content.is_empty() && !supports("editor") {
        return Err(UnifiedError::bad_request(
            "this post type does not support the editor",
        ));
    }
    if let Some(excerpt) = excerpt {
        if !excerpt.trim().is_empty() && !supports("excerpt") {
            return Err(UnifiedError::bad_request(
                "this post type does not support excerpts",
            ));
        }
    }
    if let Some(thumbnail) = thumbnail {
        if !thumbnail.trim().is_empty() && !supports("thumbnail") {
            return Err(UnifiedError::bad_request(
                "this post type does not support thumbnails",
            ));
        }
    }
    if category_id.is_some() && !supports("categories") {
        return Err(UnifiedError::bad_request(
            "this post type does not support categories",
        ));
    }
    if !tag_ids.is_empty() && !supports("tags") {
        return Err(UnifiedError::bad_request(
            "this post type does not support tags",
        ));
    }
    if !fields.is_empty() && !supports("custom-fields") {
        return Err(UnifiedError::bad_request(
            "this post type does not support custom fields",
        ));
    }
    Ok(())
}

pub(super) async fn list_custom_entries(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    type_slug: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let definition = match resolve_type_for_read(
        &state,
        &ctx,
        &req,
        tenant.tenant_id,
        &type_slug.into_inner(),
    ) {
        Ok(definition) => definition,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let can_manage = caller_can_manage(&ctx, &req);
    let params = query.into_inner();
    let search = params.search.clone();
    let status_filter = match params
        .filters
        .get("status")
        .map(|raw| parse_status_filter(raw))
    {
        Some(Ok(status)) => Some(status),
        Some(Err(error)) => return api_json::error_response(&error, debug),
        None => None,
    };
    let category_filter = params.filters.get("category").cloned();
    let tag_filter = params.filters.get("tag").cloned();
    // A tag filter names a tag id or slug; an unknown tag matches
    // nothing (same as the category filter on posts).
    let tag_ids: Vec<Uuid> = match &tag_filter {
        Some(raw) => state
            .tags
            .iter()
            .filter(|entry| {
                entry.value().tenant_id == tenant.tenant_id
                    && (entry.value().id.to_string() == *raw || entry.value().slug == *raw)
            })
            .map(|entry| *entry.key())
            .collect(),
        None => Vec::new(),
    };
    let mut items: Vec<CustomEntry> = state
        .custom_entries
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| entry.tenant_id == tenant.tenant_id && entry.type_slug == definition.slug)
        .filter(|entry| entry_readable(&definition, entry, can_manage))
        .filter(|entry| {
            if let Some(wanted) = &status_filter {
                if entry.status != *wanted {
                    return false;
                }
            }
            if let Some(category_id) = &category_filter {
                if entry.category_id.map(|id| id.to_string()) != Some(category_id.clone()) {
                    return false;
                }
            }
            if tag_filter.is_some() {
                let attached =
                    tag_ids_for_target(&state, tenant.tenant_id, TAG_TARGET_CUSTOM, entry.id);
                if !tag_ids.iter().any(|tag_id| attached.contains(tag_id)) {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let field_hit = entry.fields.values().any(|value| {
                    value
                        .as_str()
                        .is_some_and(|text| matches_search(text, needle))
                });
                if !matches_search(&entry.title, needle)
                    && !matches_search(&entry.content, needle)
                    && !matches_search(&entry.slug, needle)
                    && !field_hit
                {
                    return false;
                }
            }
            true
        })
        .collect();
    items = sort_custom_entries(items, &params);
    api_json::success_response(paginate(items, &params).to_json(), 200)
}

pub(super) async fn create_custom_entry(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    type_slug: web::Path<String>,
    input: web::Json<CreateCustomEntryInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let actor = match caller(&req) {
        Ok(actor) => actor,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = require_permission(&ctx, &actor.role, PERM_CONTENT_CREATE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let type_slug = type_slug.into_inner();
    let input = input.into_inner();
    let definition = match resolve_type_for_write(&state, tenant.tenant_id, &type_slug) {
        Ok(definition) => definition,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    if !definition.rest_exposed && !check_capability(&ctx, &actor.role, PERM_CONTENT_MANAGE) {
        return api_json::error_response(
            &UnifiedError::not_found("post type not found"),
            ctx.is_debug(),
        );
    }
    let author_id = match resolve_body_author(input.author_id, &actor.actor, &ctx, &actor.role) {
        Ok(author_id) => author_id,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let outcome = (|| -> Result<CustomEntry, UnifiedError> {
        let title = match &input.title {
            Some(title) => validate_title(title)?,
            None => String::new(),
        };
        let content = validate_content(input.content)?;
        let excerpt = input.excerpt.as_deref().unwrap_or("");
        if excerpt.chars().count() > 300 {
            return Err(UnifiedError::bad_request(
                "excerpt must be at most 300 characters",
            ));
        }
        let thumbnail = input.thumbnail.as_deref().unwrap_or("");
        if thumbnail.chars().count() > 500 {
            return Err(UnifiedError::bad_request(
                "thumbnail must be at most 500 characters",
            ));
        }
        if let Some(category_id) = input.category_id {
            if !state.categories.contains_key(&category_id) {
                return Err(UnifiedError::bad_request(format!(
                    "category {category_id} does not exist"
                )));
            }
        }
        let fields = validate_entry_fields(&definition.fields, &input.fields)?;
        for (key, value) in &fields {
            if let Some(def) = definition.fields.iter().find(|def| &def.key == key) {
                if def.field_type == CustomFieldType::Relation
                    && !relation_target_exists(&state, tenant.tenant_id, value)
                {
                    return Err(UnifiedError::bad_request(format!(
                        "field \"{key}\" references a post, page, or entry that does not exist"
                    )));
                }
            }
        }
        check_supports(
            &definition,
            &content,
            Some(excerpt),
            Some(thumbnail),
            input.category_id,
            &input.tag_ids,
            &fields,
        )?;
        // Tag ids must name tags of this tenant.
        for tag_id in &input.tag_ids {
            match state.tags.get(tag_id).map(|entry| entry.value().clone()) {
                Some(tag) if tag.tenant_id == tenant.tenant_id => {}
                _ => {
                    return Err(UnifiedError::bad_request(format!(
                        "tag {tag_id} does not exist"
                    )));
                }
            }
        }
        let slug = resolve_slug(
            input.slug,
            if title.is_empty() { "entry" } else { &title },
            &|candidate| {
                state.custom_entries.iter().any(|entry| {
                    entry.value().tenant_id == tenant.tenant_id
                        && entry.value().type_slug == definition.slug
                        && entry.value().slug == candidate
                })
            },
        )?;
        let now = Utc::now();
        let status = input.status.unwrap_or_default();
        let published_at = if status == ContentStatus::Published {
            Some(now)
        } else {
            None
        };
        let entry = CustomEntry {
            id: Uuid::now_v7(),
            tenant_id: tenant.tenant_id,
            type_slug: definition.slug.clone(),
            title,
            slug,
            content,
            excerpt: make_excerpt(
                &content,
                if excerpt.is_empty() {
                    None
                } else {
                    Some(excerpt)
                },
            ),
            status,
            category_id: input.category_id,
            author_id,
            thumbnail: if thumbnail.is_empty() {
                None
            } else {
                Some(thumbnail.to_string())
            },
            fields,
            tag_ids: Vec::new(),
            created_at: now,
            updated_at: now,
            published_at,
        };
        state.custom_entries.insert(entry.id, entry.clone());
        // The join is the tag authority; the entry snapshot is
        // updated together with it.
        if !input.tag_ids.is_empty() {
            let attached = assign_tags_to_target(
                &state,
                tenant.tenant_id,
                TAG_TARGET_CUSTOM,
                entry.id,
                &input.tag_ids,
            )?;
            let mut stored = entry.clone();
            stored.tag_ids = attached.into_iter().map(|tag| tag.id).collect();
            state.custom_entries.insert(entry.id, stored.clone());
            return Ok(stored);
        }
        Ok(entry)
    })();
    match outcome {
        Ok(entry) => {
            if entry.status == ContentStatus::Published {
                emit_custom_published(&ctx, &entry, tenant.tenant_id, true);
            }
            api_json::success_response(entry, 201)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn get_custom_entry(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let (type_slug, id) = path.into_inner();
    let definition = match resolve_type_for_read(&state, &ctx, &req, tenant.tenant_id, &type_slug) {
        Ok(definition) => definition,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let can_manage = caller_can_manage(&ctx, &req);
    match state
        .custom_entries
        .get(&id)
        .map(|entry| entry.value().clone())
    {
        Some(entry)
            if entry.tenant_id == tenant.tenant_id
                && entry.type_slug == definition.slug
                && entry_readable(&definition, &entry, can_manage) =>
        {
            api_json::success_response(entry, 200)
        }
        _ => api_json::error_response(&UnifiedError::not_found("entry not found"), ctx.is_debug()),
    }
}

/// Public render by slug: published entries of enabled public
/// types only. Drafts, private types, and disabled types are 404
/// (no existence leak).
pub(super) async fn get_custom_entry_by_slug(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let (type_slug, slug) = path.into_inner();
    let definition = match resolve_type_for_read(&state, &ctx, &req, tenant.tenant_id, &type_slug) {
        Ok(definition) => definition,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let can_manage = caller_can_manage(&ctx, &req);
    let found = state
        .custom_entries
        .iter()
        .map(|entry| entry.value().clone())
        .find(|entry| {
            entry.tenant_id == tenant.tenant_id
                && entry.type_slug == definition.slug
                && entry.slug == slug
                && entry_readable(&definition, entry, can_manage)
        });
    match found {
        Some(entry) => api_json::success_response(entry, 200),
        None => {
            api_json::error_response(&UnifiedError::not_found("entry not found"), ctx.is_debug())
        }
    }
}

pub(super) async fn update_custom_entry(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, Uuid)>,
    input: web::Json<UpdateCustomEntryInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let (type_slug, id) = path.into_inner();
    let input = input.into_inner();
    let definition = match resolve_type_for_write(&state, tenant.tenant_id, &type_slug) {
        Ok(definition) => definition,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let outcome = (|| -> Result<(CustomEntry, Option<bool>), UnifiedError> {
        let mut entry = state
            .custom_entries
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                entry.tenant_id == tenant.tenant_id && entry.type_slug == definition.slug
            })
            .ok_or_else(|| UnifiedError::not_found("entry not found"))?;
        let mut transition: Option<bool> = None;
        if let Some(title) = &input.title {
            entry.title = validate_title(title)?;
        }
        if let Some(content) = &input.content {
            entry.content = validate_content(Some(content.clone()))?;
        }
        if let Some(category_id) = input.category_id {
            if !state.categories.contains_key(&category_id) {
                return Err(UnifiedError::bad_request(format!(
                    "category {category_id} does not exist"
                )));
            }
            entry.category_id = Some(category_id);
        }
        if input.author_id.is_some() {
            entry.author_id = input.author_id;
        }
        if let Some(slug) = input.slug {
            let base = slugify(&slug);
            if base.is_empty() {
                return Err(UnifiedError::bad_request(
                    "slug must contain at least one letter or digit",
                ));
            }
            entry.slug = unique_slug(&base, &|candidate| {
                state.custom_entries.iter().any(|item| {
                    item.value().tenant_id == tenant.tenant_id
                        && item.value().type_slug == definition.slug
                        && item.value().slug == candidate
                        && item.key() != &id
                })
            });
        }
        if let Some(excerpt) = input.excerpt.as_deref() {
            if excerpt.chars().count() > 300 {
                return Err(UnifiedError::bad_request(
                    "excerpt must be at most 300 characters",
                ));
            }
            entry.excerpt = make_excerpt(&entry.content, Some(excerpt));
        } else if input.content.is_some() {
            entry.excerpt = make_excerpt(&entry.content, None);
        }
        if let Some(thumbnail) = input.thumbnail {
            if thumbnail.chars().count() > 500 {
                return Err(UnifiedError::bad_request(
                    "thumbnail must be at most 500 characters",
                ));
            }
            entry.thumbnail = if thumbnail.is_empty() {
                None
            } else {
                Some(thumbnail)
            };
        }
        if let Some(fields) = &input.fields {
            let fields = validate_entry_fields(&definition.fields, fields)?;
            for (key, value) in &fields {
                if let Some(def) = definition.fields.iter().find(|def| &def.key == key) {
                    if def.field_type == CustomFieldType::Relation
                        && !relation_target_exists(&state, tenant.tenant_id, value)
                    {
                        return Err(UnifiedError::bad_request(format!(
                            "field \"{key}\" references a post, page, or entry that does not exist"
                        )));
                    }
                }
            }
            entry.fields = fields;
        }
        if let Some(tag_ids) = &input.tag_ids {
            let attached =
                replace_tags_for_target(&state, tenant.tenant_id, TAG_TARGET_CUSTOM, id, tag_ids)?;
            entry.tag_ids = attached.into_iter().map(|tag| tag.id).collect();
        }
        check_supports(
            &definition,
            &entry.content,
            Some(&entry.excerpt),
            entry.thumbnail.as_deref(),
            entry.category_id,
            &entry.tag_ids,
            &entry.fields,
        )?;
        if let Some(status) = input.status {
            if status != entry.status {
                if status == ContentStatus::Published {
                    entry.published_at = Some(Utc::now());
                    transition = Some(true);
                } else {
                    entry.published_at = None;
                    transition = Some(false);
                }
                entry.status = status;
            }
        }
        entry.updated_at = Utc::now();
        state.custom_entries.insert(id, entry.clone());
        Ok((entry, transition))
    })();
    match outcome {
        Ok((entry, transition)) => {
            match transition {
                Some(published) => emit_custom_published(&ctx, &entry, tenant.tenant_id, published),
                None => {}
            }
            api_json::success_response(entry, 200)
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn delete_custom_entry(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let (type_slug, id) = path.into_inner();
    let definition = match resolve_type_for_write(&state, tenant.tenant_id, &type_slug) {
        Ok(definition) => definition,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    match state
        .custom_entries
        .get(&id)
        .map(|entry| entry.value().clone())
    {
        Some(entry)
            if entry.tenant_id == tenant.tenant_id && entry.type_slug == definition.slug =>
        {
            remove_all_tags_for_target(&state, tenant.tenant_id, TAG_TARGET_CUSTOM, id);
            state.custom_entries.remove(&id);
            api_json::success_response(json!({ "id": id, "deleted": true }), 200)
        }
        _ => api_json::error_response(&UnifiedError::not_found("entry not found"), ctx.is_debug()),
    }
}

// Tags on a custom entry.

pub(super) async fn list_custom_entry_tags(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_VIEW) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let (type_slug, id) = path.into_inner();
    let definition = match resolve_type_for_read(&state, &ctx, &req, tenant.tenant_id, &type_slug) {
        Ok(definition) => definition,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let can_manage = caller_can_manage(&ctx, &req);
    match state
        .custom_entries
        .get(&id)
        .map(|entry| entry.value().clone())
    {
        Some(entry)
            if entry.tenant_id == tenant.tenant_id
                && entry.type_slug == definition.slug
                && entry_readable(&definition, &entry, can_manage) =>
        {
            api_json::success_response(
                tags_for_target(&state, tenant.tenant_id, TAG_TARGET_CUSTOM, id),
                200,
            )
        }
        _ => api_json::error_response(&UnifiedError::not_found("entry not found"), ctx.is_debug()),
    }
}

pub(super) async fn assign_custom_entry_tags(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, Uuid)>,
    input: web::Json<AssignTagsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let (type_slug, id) = path.into_inner();
    let definition = match resolve_type_for_write(&state, tenant.tenant_id, &type_slug) {
        Ok(definition) => definition,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let entry = match state
        .custom_entries
        .get(&id)
        .map(|entry| entry.value().clone())
    {
        Some(entry)
            if entry.tenant_id == tenant.tenant_id && entry.type_slug == definition.slug =>
        {
            entry
        }
        _ => {
            return api_json::error_response(
                &UnifiedError::not_found("entry not found"),
                ctx.is_debug(),
            )
        }
    };
    if !definition.supports.iter().any(|item| item == "tags") {
        return api_json::error_response(
            &UnifiedError::bad_request("this post type does not support tags"),
            ctx.is_debug(),
        );
    }
    let tag_ids = assigned_tag_ids(&input.into_inner());
    if tag_ids.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("provide tag_id or tag_ids"),
            ctx.is_debug(),
        );
    }
    match assign_tags_to_target(&state, tenant.tenant_id, TAG_TARGET_CUSTOM, id, &tag_ids) {
        Ok(_) => {
            let mut entry = entry;
            entry.tag_ids = tag_ids_for_target(&state, tenant.tenant_id, TAG_TARGET_CUSTOM, id);
            state.custom_entries.insert(id, entry);
            api_json::success_response(
                tags_for_target(&state, tenant.tenant_id, TAG_TARGET_CUSTOM, id),
                200,
            )
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

pub(super) async fn unassign_custom_entry_tag(
    state: web::Data<ContentState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&req, &ctx, PERM_CONTENT_MANAGE) {
        return api_json::error_response(&error, ctx.is_debug());
    }
    let (type_slug, id, tag_id) = path.into_inner();
    let definition = match resolve_type_for_write(&state, tenant.tenant_id, &type_slug) {
        Ok(definition) => definition,
        Err(error) => return api_json::error_response(&error, ctx.is_debug()),
    };
    let entry = match state
        .custom_entries
        .get(&id)
        .map(|entry| entry.value().clone())
    {
        Some(entry)
            if entry.tenant_id == tenant.tenant_id && entry.type_slug == definition.slug =>
        {
            entry
        }
        _ => {
            return api_json::error_response(
                &UnifiedError::not_found("entry not found"),
                ctx.is_debug(),
            )
        }
    };
    if remove_tag_from_target(&state, tenant.tenant_id, TAG_TARGET_CUSTOM, id, tag_id) {
        let mut entry = entry;
        entry.tag_ids = tag_ids_for_target(&state, tenant.tenant_id, TAG_TARGET_CUSTOM, id);
        state.custom_entries.insert(id, entry);
        api_json::success_response(
            tags_for_target(&state, tenant.tenant_id, TAG_TARGET_CUSTOM, id),
            200,
        )
    } else {
        api_json::error_response(
            &UnifiedError::not_found("tag is not assigned to this entry"),
            ctx.is_debug(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field_def(
        key: &str,
        field_type: CustomFieldType,
        required: bool,
        options: &[&str],
    ) -> CustomFieldDef {
        CustomFieldDef {
            key: key.to_string(),
            label: key.to_string(),
            field_type,
            required,
            default: None,
            options: options.iter().map(|option| option.to_string()).collect(),
        }
    }

    fn field_def_with_default(
        key: &str,
        field_type: CustomFieldType,
        default: Value,
    ) -> CustomFieldDef {
        CustomFieldDef {
            key: key.to_string(),
            label: key.to_string(),
            field_type,
            required: false,
            default: Some(default),
            options: Vec::new(),
        }
    }

    #[test]
    fn field_value_text_and_number() {
        let text = field_def("name", CustomFieldType::Text, false, &[]);
        assert!(validate_field_value(&text, &json!("hello")).is_ok());
        assert!(validate_field_value(&text, &json!(42)).is_err());
        assert!(validate_field_value(&text, &json!("a".repeat(501))).is_err());
        let number = field_def("price", CustomFieldType::Number, false, &[]);
        assert!(validate_field_value(&number, &json!(15000)).is_ok());
        assert!(validate_field_value(&number, &json!(1.5)).is_ok());
        assert!(validate_field_value(&number, &json!("15000")).is_err());
    }

    #[test]
    fn field_value_date_and_datetime() {
        let date = field_def("day", CustomFieldType::Date, false, &[]);
        assert!(validate_field_value(&date, &json!("2026-12-05")).is_ok());
        assert!(validate_field_value(&date, &json!("2026-02-30")).is_err());
        assert!(validate_field_value(&date, &json!("05/12/2026")).is_err());
        let datetime = field_def("at", CustomFieldType::Datetime, false, &[]);
        assert!(validate_field_value(&datetime, &json!("2026-12-05T10:00:00Z")).is_ok());
        assert!(validate_field_value(&datetime, &json!("2026-12-05 10:00")).is_err());
    }

    #[test]
    fn field_value_select_allowlist() {
        let select = field_def("venue", CustomFieldType::Select, false, &["hall", "online"]);
        assert!(validate_field_value(&select, &json!("hall")).is_ok());
        assert!(validate_field_value(&select, &json!("roof")).is_err());
        let multi = field_def("tags", CustomFieldType::Multiselect, false, &["a", "b"]);
        assert_eq!(
            validate_field_value(&multi, &json!(["a", "a", "b"])).unwrap(),
            json!(["a", "b"])
        );
        assert!(validate_field_value(&multi, &json!(["a", "c"])).is_err());
        assert!(validate_field_value(&multi, &json!("a")).is_err());
    }

    #[test]
    fn field_value_checkbox_media_relation() {
        let flag = field_def("featured", CustomFieldType::Checkbox, false, &[]);
        assert!(validate_field_value(&flag, &json!(true)).is_ok());
        assert!(validate_field_value(&flag, &json!("yes")).is_err());
        let media = field_def("photo", CustomFieldType::Media, false, &[]);
        assert_eq!(
            validate_field_value(&media, &json!({"document_id": "doc-1"})).unwrap(),
            json!("doc-1")
        );
        assert!(validate_field_value(&media, &json!({})).is_err());
        let relation = field_def("event", CustomFieldType::Relation, false, &[]);
        assert!(validate_field_value(&relation, &json!(Uuid::now_v7().to_string())).is_ok());
        assert!(validate_field_value(&relation, &json!("not-a-uuid")).is_err());
    }

    #[test]
    fn field_defs_reject_bad_shapes() {
        // Select without options.
        let defs = vec![field_def("venue", CustomFieldType::Select, false, &[])];
        assert!(validate_field_defs(&defs).is_err());
        // Options on a text field.
        let defs = vec![field_def("name", CustomFieldType::Text, false, &["a"])];
        assert!(validate_field_defs(&defs).is_err());
        // Duplicate keys.
        let defs = vec![
            field_def("name", CustomFieldType::Text, false, &[]),
            field_def("name", CustomFieldType::Text, false, &[]),
        ];
        assert!(validate_field_defs(&defs).is_err());
        // Bad key shape.
        let defs = vec![field_def("Bad Key", CustomFieldType::Text, false, &[])];
        assert!(validate_field_defs(&defs).is_err());
        // A default that fails its own validation.
        let defs = vec![CustomFieldDef {
            key: "venue".to_string(),
            label: "Venue".to_string(),
            field_type: CustomFieldType::Select,
            required: false,
            default: Some(json!("roof")),
            options: vec!["hall".to_string()],
        }];
        assert!(validate_field_defs(&defs).is_err());
    }

    #[test]
    fn entry_fields_apply_defaults_and_reject_unknown_and_missing_required() {
        let defs = vec![
            field_def("event_date", CustomFieldType::Date, true, &[]),
            field_def_with_default("ticket_price", CustomFieldType::Number, json!(0)),
        ];
        // Unknown key rejected.
        let mut provided = Map::new();
        provided.insert("event_date".to_string(), json!("2026-12-05"));
        provided.insert("nope".to_string(), json!("x"));
        assert!(validate_entry_fields(&defs, &provided).is_err());
        // Missing required rejected.
        let provided = Map::new();
        assert!(validate_entry_fields(&defs, &provided).is_err());
        // Default applied for the missing optional field.
        let mut provided = Map::new();
        provided.insert("event_date".to_string(), json!("2026-12-05"));
        let out = validate_entry_fields(&defs, &provided).unwrap();
        assert_eq!(out.get("ticket_price"), Some(&json!(0)));
        // Null for an optional field falls back to its default.
        let mut provided = Map::new();
        provided.insert("event_date".to_string(), json!("2026-12-05"));
        provided.insert("ticket_price".to_string(), Value::Null);
        let out = validate_entry_fields(&defs, &provided).unwrap();
        assert_eq!(out.get("ticket_price"), Some(&json!(0)));
    }

    #[test]
    fn supports_allowlist() {
        assert!(validate_supports(&["title".to_string(), "tags".to_string()]).is_ok());
        assert_eq!(
            validate_supports(&["Title".to_string(), "title".to_string()]).unwrap(),
            vec!["title".to_string()]
        );
        assert!(validate_supports(&["widget".to_string()]).is_err());
    }
}
