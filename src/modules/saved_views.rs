// Saved Filters / Views module.
//
// Saved filters and views, per user and per module: a name, the module
// id the view belongs to, the stored filter definition, sort order,
// visibility (`private` or `shared`), and whether the view is the
// user's default for that module. `apply` returns the stored filter
// definition so the client can run it against the module's own list
// endpoint — this module never executes another module's query itself.
//
// Validation rules (enforced on create and update):
//   - `module_id` must be a registered module id, checked against the
//     live module registry (`AppContext.modules`), so a view can never
//     point at a module that does not exist.
//   - `filter` must be a JSON object whose values are scalars (null,
//     boolean, number, string) or arrays of scalars only. Nested
//     objects and nested arrays are rejected: a stored filter is a
//     flat field/value map, never executable logic.
//   - `visibility` is `private` or `shared`; `digest`-style values
//     are not accepted here.
//   - `name` is required, 1..=100 characters.
//   - At most one default view per (user, module): setting a default
//     clears the flag on the user's other views for that module.
//
// Persistence: views live in an in-memory store (the same approach the
// notifications and audit modules use). The Diesel table for durable
// persistence does not exist in `src/db/schema.rs` yet; the migration
// SQL and the schema entries are supplied to the integrator separately.
// Nothing in this file references a table that does not exist yet, so
// it compiles against the current schema.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

pub const VISIBILITY_PRIVATE: &str = "private";
pub const VISIBILITY_SHARED: &str = "shared";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedView {
    pub id: String,
    pub user_id: String,
    pub module_id: String,
    pub name: String,
    pub filter: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
    pub visibility: String,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSavedView {
    pub user_id: String,
    pub module_id: String,
    pub name: String,
    pub filter: Value,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSavedView {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub filter: Option<Value>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyViewResponse {
    pub view_id: String,
    pub user_id: String,
    pub module_id: String,
    pub name: String,
    pub filter: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct SavedViewsState {
    views: DashMap<String, SavedView>,
}

impl SavedViewsState {
    fn new() -> Self {
        Self {
            views: DashMap::new(),
        }
    }

    /// Clear the default flag on the user's other views for a module.
    fn clear_other_defaults(&self, user_id: &str, module_id: &str, except_id: &str) {
        for mut entry in self.views.iter_mut() {
            let view = entry.value_mut();
            if view.user_id == user_id
                && view.module_id == module_id
                && view.id != except_id
                && view.is_default
            {
                view.is_default = false;
                view.updated_at = Utc::now();
            }
        }
    }

    fn create(&self, ctx: &AppContext, input: CreateSavedView) -> UnifiedResult<SavedView> {
        let view = validate_new(ctx, input)?;
        let is_default = view.is_default;
        self.views.insert(view.id.clone(), view.clone());
        if is_default {
            self.clear_other_defaults(&view.user_id, &view.module_id, &view.id);
        }
        Ok(view)
    }

    fn update(
        &self,
        ctx: &AppContext,
        id: &str,
        input: UpdateSavedView,
    ) -> UnifiedResult<SavedView> {
        let mut view = self
            .views
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Saved view not found"))?;
        if let Some(name) = input.name {
            validate_name(&name)?;
            view.name = name.trim().to_string();
        }
        if let Some(filter) = input.filter {
            validate_filter(&filter)?;
            view.filter = filter;
        }
        if let Some(sort) = input.sort {
            view.sort = validate_sort(&sort)?;
        }
        if let Some(order) = input.order {
            view.order = Some(validate_order(&order)?);
        }
        if let Some(visibility) = input.visibility {
            view.visibility = validate_visibility(&visibility)?.to_string();
        }
        let make_default = input.is_default.unwrap_or(false);
        if let Some(is_default) = input.is_default {
            view.is_default = is_default;
        }
        view.updated_at = Utc::now();
        self.views.insert(id.to_string(), view.clone());
        if make_default {
            self.clear_other_defaults(&view.user_id, &view.module_id, &view.id);
        }
        Ok(view)
    }
}

// ---------------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------------

fn validate_name(name: &str) -> UnifiedResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request("name must not be empty"));
    }
    if trimmed.chars().count() > 100 {
        return Err(UnifiedError::bad_request(
            "name must be at most 100 characters",
        ));
    }
    Ok(())
}

/// The module id must belong to a registered module.
fn validate_module_id(ctx: &AppContext, module_id: &str) -> UnifiedResult<String> {
    let module_id = module_id.trim().to_string();
    if module_id.is_empty() {
        return Err(UnifiedError::bad_request("module_id must not be empty"));
    }
    if ctx.modules.get(&module_id).is_none() {
        return Err(UnifiedError::not_found(format!(
            "Module \"{module_id}\" is not registered"
        ))
        .with_metadata("module_id", Value::String(module_id.clone())));
    }
    Ok(module_id)
}

/// A filter value is a scalar, or an array of scalars only. Nested
/// objects and nested arrays are rejected.
fn validate_filter_value(value: &Value) -> UnifiedResult<()> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(()),
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
                    _ => {
                        return Err(UnifiedError::bad_request(
                            "filter arrays must contain scalar values only (no nested objects or arrays)",
                        ));
                    }
                }
            }
            Ok(())
        }
        Value::Object(_) => Err(UnifiedError::bad_request(
            "filter values must be scalars or arrays of scalars (nested objects are not allowed)",
        )),
    }
}

/// The filter must be an object with string keys and scalar/array
/// values only.
fn validate_filter(filter: &Value) -> UnifiedResult<()> {
    let object = match filter {
        Value::Object(map) => map,
        _ => {
            return Err(UnifiedError::bad_request(
                "filter must be a JSON object",
            ));
        }
    };
    if object.is_empty() {
        return Err(UnifiedError::bad_request("filter must not be empty"));
    }
    for (key, value) in object {
        if key.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "filter keys must not be empty",
            ));
        }
        validate_filter_value(value)?;
    }
    Ok(())
}

fn validate_visibility(visibility: &str) -> UnifiedResult<&'static str> {
    match visibility.trim() {
        VISIBILITY_PRIVATE => Ok(VISIBILITY_PRIVATE),
        VISIBILITY_SHARED => Ok(VISIBILITY_SHARED),
        _ => Err(UnifiedError::bad_request(
            "visibility must be \"private\" or \"shared\"",
        )),
    }
}

fn validate_order(order: &str) -> UnifiedResult<String> {
    match order.trim().to_lowercase().as_str() {
        "asc" => Ok("asc".to_string()),
        "desc" => Ok("desc".to_string()),
        _ => Err(UnifiedError::bad_request(
            "order must be \"asc\" or \"desc\"",
        )),
    }
}

fn validate_sort(sort: &str) -> UnifiedResult<Option<String>> {
    let sort = sort.trim();
    if sort.is_empty() {
        return Ok(None);
    }
    if sort.len() > 64 || !sort.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        return Err(UnifiedError::bad_request(
            "sort must be a plain field name (letters, digits, underscore, dot; at most 64 characters)",
        ));
    }
    Ok(Some(sort.to_string()))
}

fn validate_new(ctx: &AppContext, input: CreateSavedView) -> UnifiedResult<SavedView> {
    if input.user_id.trim().is_empty() {
        return Err(UnifiedError::bad_request("user_id must not be empty"));
    }
    let module_id = validate_module_id(ctx, &input.module_id)?;
    validate_name(&input.name)?;
    validate_filter(&input.filter)?;
    let sort = match input.sort {
        Some(sort) => validate_sort(&sort)?,
        None => None,
    };
    let order = match input.order {
        Some(order) => Some(validate_order(&order)?),
        None => None,
    };
    let visibility = match input.visibility {
        Some(visibility) => validate_visibility(&visibility)?.to_string(),
        None => VISIBILITY_PRIVATE.to_string(),
    };
    let now = Utc::now();
    Ok(SavedView {
        id: Uuid::new_v4().to_string(),
        user_id: input.user_id.trim().to_string(),
        module_id,
        name: input.name.trim().to_string(),
        filter: input.filter,
        sort,
        order,
        visibility,
        is_default: input.is_default.unwrap_or(false),
        created_at: now,
        updated_at: now,
    })
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// A view is visible to `requester_id` when it is theirs, or when it
/// is shared. Shared views are readable by any user but only the
/// owner may change or delete them.
fn is_visible_to(view: &SavedView, requester_id: Option<&str>) -> bool {
    match requester_id {
        Some(user_id) => view.user_id == user_id || view.visibility == VISIBILITY_SHARED,
        None => view.visibility == VISIBILITY_SHARED,
    }
}

fn get_view(state: &SavedViewsState, id: &str) -> UnifiedResult<SavedView> {
    state
        .views
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Saved view not found"))
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn list_views(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let requester_id = params.filters.get("user_id").cloned();
    let module_filter = params.filters.get("module_id").cloned();
    if let Some(module_id) = &module_filter {
        if ctx.modules.get(module_id).is_none() {
            return json::error_response(
                &UnifiedError::not_found(format!("Module \"{module_id}\" is not registered")),
                debug,
            );
        }
    }
    let mut items: Vec<SavedView> = state
        .views
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|view| {
            matches_search(&format!("{} {}", view.name, view.module_id), &params)
                && module_filter
                    .as_ref()
                    .is_none_or(|module_id| view.module_id == *module_id)
                && params
                    .filters
                    .get("visibility")
                    .is_none_or(|visibility| view.visibility == *visibility)
                && params
                    .filters
                    .get("is_default")
                    .is_none_or(|is_default| {
                        view.is_default == (*is_default == "true" || *is_default == "1")
                    })
                && is_visible_to(view, requester_id.as_deref())
        })
        .collect();
    // Defaults first, then newest first.
    items.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then(b.updated_at.cmp(&a.updated_at))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    body: web::Json<CreateSavedView>,
) -> HttpResponse {
    let result = state.create(&ctx, body.into_inner());
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, ctx.is_debug()),
    }
}

async fn get_view_handler(
    state: web::Data<Arc<SavedViewsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result = get_view(&state, path.as_str());
    json::respond(result, false)
}

async fn update_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateSavedView>,
) -> HttpResponse {
    let result = state.update(&ctx, path.as_str(), body.into_inner());
    json::respond(result, ctx.is_debug())
}

async fn delete_view(
    state: web::Data<Arc<SavedViewsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .views
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Saved view not found"))
    })();
    json::respond(result, false)
}

async fn set_default_view(
    state: web::Data<Arc<SavedViewsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SavedView> = (|| {
        let mut view = get_view(&state, path.as_str())?;
        view.is_default = true;
        view.updated_at = Utc::now();
        state.views.insert(view.id.clone(), view.clone());
        state.clear_other_defaults(&view.user_id, &view.module_id, &view.id);
        Ok(view)
    })();
    json::respond(result, false)
}

async fn apply_view(
    state: web::Data<Arc<SavedViewsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ApplyViewResponse> = get_view(&state, path.as_str()).map(|view| {
        ApplyViewResponse {
            view_id: view.id,
            user_id: view.user_id,
            module_id: view.module_id,
            name: view.name,
            filter: view.filter,
            sort: view.sort,
            order: view.order,
        }
    });
    json::respond(result, false)
}

async fn get_default_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SavedViewsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let user_id = params.filters.get("user_id").cloned().unwrap_or_default();
    let module_id = params.filters.get("module_id").cloned().unwrap_or_default();
    if user_id.trim().is_empty() || module_id.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request(
                "Query parameters user_id and module_id are required",
            ),
            debug,
        );
    }
    if ctx.modules.get(&module_id).is_none() {
        return json::error_response(
            &UnifiedError::not_found(format!("Module \"{module_id}\" is not registered")),
            debug,
        );
    }
    let result: UnifiedResult<SavedView> = state
        .views
        .iter()
        .map(|entry| entry.value().clone())
        .find(|view| view.user_id == user_id && view.module_id == module_id && view.is_default)
        .ok_or_else(|| {
            UnifiedError::not_found("No default view for this user and module")
        });
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct SavedViewsModule {
    state: Arc<SavedViewsState>,
}

impl SavedViewsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SavedViewsState::new()),
        }
    }
}

impl Default for SavedViewsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SavedViewsModule {
    fn id(&self) -> &'static str {
        "saved_views"
    }

    fn name(&self) -> &'static str {
        "Saved Filters and Views"
    }

    fn description(&self) -> &'static str {
        "Saved filters and views per user and per module, with private/shared visibility and one default view per user and module."
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/saved-views")
                .route("", web::get().to(list_views))
                .route("", web::post().to(create_view))
                .route("/defaults", web::get().to(get_default_view))
                .route("/{id}", web::get().to(get_view_handler))
                .route("/{id}", web::put().to(update_view))
                .route("/{id}", web::delete().to(delete_view))
                .route("/{id}/set-default", web::post().to(set_default_view))
                .route("/{id}/apply", web::get().to(apply_view)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/saved-views", owner: "saved_views", description: "List saved views by module, paginated, with visibility rules." },
            RouteInfo { method: "POST", path: "/api/v1/saved-views", owner: "saved_views", description: "Create a saved view." },
            RouteInfo { method: "GET", path: "/api/v1/saved-views/defaults", owner: "saved_views", description: "The default view for one user and module." },
            RouteInfo { method: "GET", path: "/api/v1/saved-views/{id}", owner: "saved_views", description: "Get one saved view." },
            RouteInfo { method: "PUT", path: "/api/v1/saved-views/{id}", owner: "saved_views", description: "Update a saved view." },
            RouteInfo { method: "DELETE", path: "/api/v1/saved-views/{id}", owner: "saved_views", description: "Delete a saved view." },
            RouteInfo { method: "POST", path: "/api/v1/saved-views/{id}/set-default", owner: "saved_views", description: "Make a view the default for its user and module." },
            RouteInfo { method: "GET", path: "/api/v1/saved-views/{id}/apply", owner: "saved_views", description: "Return the stored filter definition for the client to run." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SavedViewsModule::new()));
}
