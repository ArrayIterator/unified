// Studio - List-view and form-layout handlers.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::auth::authorize;
use super::{
    CreateFormLayout, CreateListView, EVENT_LAYOUTS_CHANGED, EVENT_VIEWS_CHANGED, FormLayout,
    ListView, MAX_NAME_LEN, PERM_STUDIO_CUSTOMIZE, PERM_STUDIO_VIEW, StudioState, UpdateFormLayout,
    UpdateListView, VISIBILITY_PRIVATE, VISIBILITY_SHARED, matches_search, normalize_company_id,
    paginate, require_record_type, require_text, validate_columns, validate_filter,
    validate_layout, validate_order, validate_sort_field, validate_visibility,
};

// Handlers: list views.

pub(crate) async fn list_views(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (call, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let params = query.into_inner();
    let record_filter = params.filters.get("record_type").cloned();
    let company_filter = params.filters.get("company_id").cloned();
    let visibility_filter = params.filters.get("visibility").cloned();
    let mut items: Vec<ListView> = state
        .views
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|view| {
            matches_search(&format!("{} {}", view.name, view.record_type), &params)
                && record_filter
                    .as_ref()
                    .is_none_or(|record_type| view.record_type == *record_type)
                && company_filter
                    .as_ref()
                    .is_none_or(|company_id| view.company_id == *company_id)
                && visibility_filter
                    .as_ref()
                    .is_none_or(|visibility| view.visibility == *visibility)
                && (view.user_id == call.actor || view.visibility == VISIBILITY_SHARED)
                // Company scoping: a company caller only ever sees
                // their own company's views plus global ones.
                && caller_company.can_access_record(&view.company_id)
        })
        .collect();
    items.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then(b.updated_at.cmp(&a.updated_at))
    });
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<CreateListView>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (call, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<ListView> = (|| {
        let record_type = require_record_type(&state, &input.record_type)?;
        let company_id = normalize_company_id(&input.company_id);
        // The body company_id is client-asserted: it must match
        // the caller's server-side company scope.
        caller_company.require_company_scope(&company_id)?;
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let columns = validate_columns(&input.columns.unwrap_or_default())?;
        let filters = input.filters.unwrap_or_else(|| json!({}));
        validate_filter(&filters)?;
        let sort_field = match &input.sort_field {
            Some(field) => Some(validate_sort_field(field)?),
            None => None,
        };
        let sort_order = match &input.sort_order {
            Some(order) => Some(validate_order(order)?),
            None => None,
        };
        let group_by = match &input.group_by {
            Some(field) => Some(validate_sort_field(field)?),
            None => None,
        };
        let visibility = match &input.visibility {
            Some(visibility) => validate_visibility(visibility)?.to_string(),
            None => VISIBILITY_PRIVATE.to_string(),
        };
        // A shared view belongs to the company; a private view
        // belongs to the user. The scope drives default clearing.
        let scope = if visibility == VISIBILITY_SHARED {
            "company".to_string()
        } else {
            "user".to_string()
        };
        let now = Utc::now();
        let view = ListView {
            id: Uuid::new_v4().to_string(),
            user_id: call.actor.clone(),
            company_id: company_id.clone(),
            record_type: record_type.clone(),
            name,
            columns,
            filters,
            sort_field,
            sort_order,
            group_by,
            visibility,
            scope,
            is_default: input.is_default.unwrap_or(false),
            created_at: now,
            updated_at: now,
        };
        state.views.insert(view.id.clone(), view.clone());
        if view.is_default {
            state.clear_other_view_defaults(&view);
        }
        state.emit(
            EVENT_VIEWS_CHANGED,
            json!({
                "view_id": view.id,
                "record_type": record_type,
                "company_id": company_id,
                "action": "created",
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (call, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<ListView> = (|| {
        let view = state
            .views
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("List view not found"))?;
        // A view of another company does not exist for this
        // caller, even when it is shared.
        if !caller_company.can_access_record(&view.company_id) {
            return Err(UnifiedError::not_found("List view not found"));
        }
        if view.user_id != call.actor && view.visibility != VISIBILITY_SHARED {
            return Err(UnifiedError::forbidden(
                "This list view is private to another user",
            ));
        }
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateListView>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (call, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<ListView> = (|| {
        let mut view = state
            .views
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("List view not found"))?;
        if !caller_company.can_access_record(&view.company_id) {
            return Err(UnifiedError::not_found("List view not found"));
        }
        if view.user_id != call.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner of a list view may change it",
            ));
        }
        if let Some(name) = &input.name {
            view.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(columns) = &input.columns {
            view.columns = validate_columns(columns)?;
        }
        if let Some(filters) = &input.filters {
            validate_filter(filters)?;
            view.filters = filters.clone();
        }
        if let Some(sort_field) = &input.sort_field {
            view.sort_field = Some(validate_sort_field(sort_field)?);
        }
        if let Some(sort_order) = &input.sort_order {
            view.sort_order = Some(validate_order(sort_order)?);
        }
        if let Some(group_by) = &input.group_by {
            view.group_by = Some(validate_sort_field(group_by)?);
        }
        if let Some(visibility) = &input.visibility {
            view.visibility = validate_visibility(visibility)?.to_string();
            view.scope = if view.visibility == VISIBILITY_SHARED {
                "company".to_string()
            } else {
                "user".to_string()
            };
        }
        let make_default = input.is_default.unwrap_or(false);
        if let Some(is_default) = input.is_default {
            view.is_default = is_default;
        }
        view.updated_at = Utc::now();
        state.views.insert(view.id.clone(), view.clone());
        if make_default {
            state.clear_other_view_defaults(&view);
        }
        state.emit(
            EVENT_VIEWS_CHANGED,
            json!({
                "view_id": view.id,
                "record_type": view.record_type,
                "company_id": view.company_id,
                "action": "updated",
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (call, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let view = state
            .views
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("List view not found"))?;
        if !caller_company.can_access_record(&view.company_id) {
            return Err(UnifiedError::not_found("List view not found"));
        }
        if view.user_id != call.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner of a list view may delete it",
            ));
        }
        state.views.remove(path.as_str());
        state.emit(
            EVENT_VIEWS_CHANGED,
            json!({
                "view_id": view.id,
                "record_type": view.record_type,
                "company_id": view.company_id,
                "action": "deleted",
            }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn set_default_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (call, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<ListView> = (|| {
        let mut view = state
            .views
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("List view not found"))?;
        if !caller_company.can_access_record(&view.company_id) {
            return Err(UnifiedError::not_found("List view not found"));
        }
        if view.user_id != call.actor {
            return Err(UnifiedError::forbidden(
                "Only the owner of a list view may make it the default",
            ));
        }
        view.is_default = true;
        view.updated_at = Utc::now();
        state.views.insert(view.id.clone(), view.clone());
        state.clear_other_view_defaults(&view);
        state.emit(
            EVENT_VIEWS_CHANGED,
            json!({
                "view_id": view.id,
                "record_type": view.record_type,
                "company_id": view.company_id,
                "action": "default_changed",
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

// Handlers: form layouts.

pub(crate) async fn list_layouts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let params = query.into_inner();
    let record_filter = params.filters.get("record_type").cloned();
    let company_filter = params.filters.get("company_id").cloned();
    let mut items: Vec<FormLayout> = state
        .layouts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|layout| {
            matches_search(&format!("{} {}", layout.name, layout.record_type), &params)
                && record_filter
                    .as_ref()
                    .is_none_or(|record_type| layout.record_type == *record_type)
                && company_filter
                    .as_ref()
                    .is_none_or(|company_id| layout.company_id == *company_id)
                // Company scoping: a company caller only ever sees
                // their own company's layouts plus global ones.
                && caller_company.can_access_record(&layout.company_id)
        })
        .collect();
    items.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then(b.updated_at.cmp(&a.updated_at))
    });
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_layout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    body: web::Json<CreateFormLayout>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<FormLayout> = (|| {
        let record_type = require_record_type(&state, &input.record_type)?;
        let company_id = normalize_company_id(&input.company_id);
        // The body company_id is client-asserted: it must match
        // the caller's server-side company scope.
        caller_company.require_company_scope(&company_id)?;
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        validate_layout(&state, &record_type, &company_id, &input.sections)?;
        let now = Utc::now();
        let layout = FormLayout {
            id: Uuid::new_v4().to_string(),
            company_id: company_id.clone(),
            record_type: record_type.clone(),
            name,
            sections: input.sections,
            is_default: input.is_default.unwrap_or(false),
            created_at: now,
            updated_at: now,
        };
        state.layouts.insert(layout.id.clone(), layout.clone());
        if layout.is_default {
            state.clear_other_layout_defaults(&layout);
        }
        state.emit(
            EVENT_LAYOUTS_CHANGED,
            json!({
                "layout_id": layout.id,
                "record_type": record_type,
                "company_id": company_id,
                "action": "created",
            }),
        );
        Ok(layout)
    })();
    match result {
        Ok(layout) => json::success_response(layout, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_layout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<FormLayout> = state
        .layouts
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .filter(|layout| caller_company.can_access_record(&layout.company_id))
        .ok_or_else(|| UnifiedError::not_found("Form layout not found"));
    json::respond(result, debug)
}

pub(crate) async fn update_layout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFormLayout>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let result: UnifiedResult<FormLayout> = (|| {
        let mut layout = state
            .layouts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Form layout not found"))?;
        // A layout of another company does not exist for this
        // caller: checked before any field is changed.
        if !caller_company.can_access_record(&layout.company_id) {
            return Err(UnifiedError::not_found("Form layout not found"));
        }
        if let Some(name) = &input.name {
            layout.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(sections) = &input.sections {
            validate_layout(&state, &layout.record_type, &layout.company_id, sections)?;
            layout.sections = sections.clone();
        }
        let make_default = input.is_default.unwrap_or(false);
        if let Some(is_default) = input.is_default {
            layout.is_default = is_default;
        }
        layout.updated_at = Utc::now();
        state.layouts.insert(layout.id.clone(), layout.clone());
        if make_default {
            state.clear_other_layout_defaults(&layout);
        }
        state.emit(
            EVENT_LAYOUTS_CHANGED,
            json!({
                "layout_id": layout.id,
                "record_type": layout.record_type,
                "company_id": layout.company_id,
                "action": "updated",
            }),
        );
        Ok(layout)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_layout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_CUSTOMIZE) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        // Load first: a layout of another company does not exist
        // for this caller and is never deleted.
        let existing = state
            .layouts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Form layout not found"))?;
        if !caller_company.can_access_record(&existing.company_id) {
            return Err(UnifiedError::not_found("Form layout not found"));
        }
        let layout = state
            .layouts
            .remove(path.as_str())
            .map(|(_, layout)| layout)
            .ok_or_else(|| UnifiedError::not_found("Form layout not found"))?;
        state.emit(
            EVENT_LAYOUTS_CHANGED,
            json!({
                "layout_id": layout.id,
                "record_type": layout.record_type,
                "company_id": layout.company_id,
                "action": "deleted",
            }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_default_layout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<StudioState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (_, caller_company) = match authorize(&ctx, &req, PERM_STUDIO_VIEW) {
        Ok(scoped) => scoped,
        Err(error) => return json::error_response(&error, debug),
    };
    let record_type = path.into_inner();
    if !state.record_types.contains_key(&record_type) {
        return json::error_response(
            &UnifiedError::not_found(format!("Record type \"{record_type}\" is not registered")),
            debug,
        );
    }
    let params = query.into_inner();
    let requested_company = params
        .filters
        .get("company_id")
        .cloned()
        .unwrap_or_default();
    // An explicitly requested other company fails closed; an
    // empty request defaults to the caller's own company.
    let company_id = match caller_company.scoped_read_company(&requested_company) {
        Ok(company_id) => company_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<FormLayout> = state
        .default_layout(&record_type, &company_id)
        .ok_or_else(|| {
            UnifiedError::not_found("No default layout for this record type and company")
        });
    json::respond(result, debug)
}
