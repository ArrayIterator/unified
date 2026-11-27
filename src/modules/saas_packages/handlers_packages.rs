//! Saas Packages - HTTP handlers for packages.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::middleware::TenantContext;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::tenancy as tenancy;
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai::{register_ai_capabilities, run_ai_capability, AiCapability};
use crate::modules::licensing::check_module_entitlement;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, state::*, seed::*, auth::*, helpers::*, provisioning::*, handlers_purchases::*, handlers_ai::*, events::*, module::*};

// HTTP handlers -- packages.

pub(crate) async fn list_packages(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let vendor_filter = params.filters.get("vendor_id").cloned();
        let is_manager = check_capability(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE);
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<SaasPackage> = state
            .packages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|package| {
                // Non-managers only ever see published packages.
                if !is_manager && package.status != STATUS_PUBLISHED {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &package.status != status {
                        return false;
                    }
                }
                if let Some(vendor) = &vendor_filter {
                    if package.vendor_id.as_deref() != Some(vendor.as_str()) {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let haystack = format!("{} {} {}", package.name, package.slug, package.description).to_lowercase();
                    if !haystack.contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    body: web::Json<CreatePackageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let input = body.into_inner();
        let settings = state.settings();
        if !settings.vendor_packages_enabled && input.vendor_id.is_some() {
            return Err(UnifiedError::forbidden(
                "Vendor packages are disabled by the module settings",
            ));
        }
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?
            .unwrap_or_default();
        let slug = match &input.slug {
            Some(raw) => {
                let slug = require_text(raw, "slug", MAX_SLUG_LEN)?.to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens, without leading or trailing hyphens",
                    ));
                }
                slug
            }
            None => slugify(&name),
        };
        if slug.is_empty() {
            return Err(UnifiedError::bad_request(
                "slug must not be empty (provide a slug or a name with letters or digits)",
            ));
        }
        if state.packages.iter().any(|entry| entry.value().slug == slug) {
            return Err(UnifiedError::conflict(format!(
                "A package with slug \"{slug}\" already exists"
            )));
        }
        let product_id = optional_text(&input.product_id, "product_id", MAX_REF_LEN)?;
        let preset_settings = input.preset_settings.unwrap_or(Value::Object(Map::new()));
        if !preset_settings.is_object() {
            return Err(UnifiedError::bad_request(
                "preset_settings must be a JSON object",
            ));
        }
        let wizard_steps = match &input.wizard_steps {
            Some(steps) => validate_wizard_steps(&settings, steps)?,
            None => settings.wizard_step_names.clone(),
        };
        let modules = validate_package_modules(&ctx.modules, &settings, &input.modules)?;
        let vendor_id = optional_text(&input.vendor_id, "vendor_id", MAX_REF_LEN)?;
        let now = Utc::now();
        let package = SaasPackage {
            id: Uuid::now_v7(),
            slug,
            name,
            description,
            status: STATUS_DRAFT.to_string(),
            current_version: 1,
            product_id,
            preset_settings,
            wizard_steps,
            modules,
            created_by: caller.actor.clone(),
            vendor_id,
            created_at: now,
            updated_at: now,
        };
        state.store_version(&package);
        state.packages.insert(package.id, package.clone());
        emit_audit(
            &ctx,
            EVENT_PACKAGE_CREATED,
            &caller.actor,
            "saas_packages.package.created",
            "saas_package",
            &package.id.to_string(),
            None,
            json!({ "slug": package.slug, "status": package.status }),
        );
        Ok(package)
    })();
    match result {
        Ok(package) => created(package),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let package = load_package(&state, &path.into_inner())?;
        if package.status != STATUS_PUBLISHED
            && !check_capability(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)
        {
            return Err(UnifiedError::not_found("Package not found"));
        }
        Ok(package)
    })();
    match result {
        Ok(package) => ok(package),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePackageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let input = body.into_inner();
        let mut package = load_package(&state, &path.into_inner())?;
        if package.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict(
                "An archived package cannot be edited; clone it to create a new draft",
            ));
        }
        let settings = state.settings();
        // Editing a published package creates a new version: the old
        // snapshot stays frozen and existing purchases keep it.
        let new_version = package.status == STATUS_PUBLISHED;
        if let Some(name) = &input.name {
            package.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            package.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(product_id) = &input.product_id {
            package.product_id = Some(require_text(product_id, "product_id", MAX_REF_LEN)?);
        }
        if let Some(preset_settings) = &input.preset_settings {
            if !preset_settings.is_object() {
                return Err(UnifiedError::bad_request(
                    "preset_settings must be a JSON object",
                ));
            }
            package.preset_settings = preset_settings.clone();
        }
        if let Some(steps) = &input.wizard_steps {
            package.wizard_steps = validate_wizard_steps(&settings, steps)?;
        }
        if let Some(modules) = &input.modules {
            package.modules = validate_package_modules(&ctx.modules, &settings, modules)?;
        }
        if new_version {
            package.current_version += 1;
        }
        package.updated_at = now();
        state.store_version(&package);
        state.packages.insert(package.id, package.clone());
        emit_audit(
            &ctx,
            EVENT_PACKAGE_UPDATED,
            &caller.actor,
            "saas_packages.package.updated",
            "saas_package",
            &package.id.to_string(),
            None,
            json!({
                "slug": package.slug,
                "version": package.current_version,
                "new_version": new_version,
            }),
        );
        Ok(package)
    })();
    match result {
        Ok(package) => ok(package),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let package = load_package(&state, &path.into_inner())?;
        if package.status == STATUS_PUBLISHED {
            return Err(UnifiedError::conflict(
                "A published package cannot be deleted; archive it first",
            ));
        }
        if state
            .purchases
            .iter()
            .any(|entry| entry.value().package_id == package.id)
        {
            return Err(UnifiedError::conflict(
                "A package with purchases cannot be deleted; archive it instead",
            ));
        }
        state.packages.remove(&package.id);
        state
            .versions
            .retain(|_, version| version.package_id != package.id);
        emit_audit(
            &ctx,
            EVENT_PACKAGE_DELETED,
            &caller.actor,
            "saas_packages.package.deleted",
            "saas_package",
            &package.id.to_string(),
            None,
            json!({ "slug": package.slug }),
        );
        Ok(json!({ "deleted": true, "id": package.id, "slug": package.slug }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn publish_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let mut package = load_package(&state, &path.into_inner())?;
        if package.status == STATUS_PUBLISHED {
            return Err(UnifiedError::conflict("The package is already published"));
        }
        if package.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict(
                "An archived package cannot be published; clone it to create a new draft",
            ));
        }
        if package.modules.is_empty() {
            return Err(UnifiedError::bad_request(
                "A package with no modules cannot be published",
            ));
        }
        package.status = STATUS_PUBLISHED.to_string();
        package.updated_at = now();
        state.packages.insert(package.id, package.clone());
        emit_audit(
            &ctx,
            EVENT_PACKAGE_PUBLISHED,
            &caller.actor,
            "saas_packages.package.published",
            "saas_package",
            &package.id.to_string(),
            None,
            json!({ "slug": package.slug, "version": package.current_version }),
        );
        Ok(package)
    })();
    match result {
        Ok(package) => ok(package),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn archive_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let mut package = load_package(&state, &path.into_inner())?;
        if package.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict("The package is already archived"));
        }
        package.status = STATUS_ARCHIVED.to_string();
        package.updated_at = now();
        state.packages.insert(package.id, package.clone());
        emit_audit(
            &ctx,
            EVENT_PACKAGE_ARCHIVED,
            &caller.actor,
            "saas_packages.package.archived",
            "saas_package",
            &package.id.to_string(),
            None,
            json!({ "slug": package.slug }),
        );
        Ok(package)
    })();
    match result {
        Ok(package) => ok(package),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn clone_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackage> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let source = load_package(&state, &path.into_inner())?;
        let mut slug = format!("{}-copy", source.slug);
        let mut suffix = 2;
        while state.packages.iter().any(|entry| entry.value().slug == slug) {
            slug = format!("{}-copy-{suffix}", source.slug);
            suffix += 1;
        }
        let now = Utc::now();
        let clone = SaasPackage {
            id: Uuid::now_v7(),
            slug,
            name: format!("{} (copy)", source.name),
            description: source.description.clone(),
            status: STATUS_DRAFT.to_string(),
            current_version: 1,
            product_id: source.product_id.clone(),
            preset_settings: source.preset_settings.clone(),
            wizard_steps: source.wizard_steps.clone(),
            modules: source.modules.clone(),
            created_by: caller.actor.clone(),
            vendor_id: source.vendor_id.clone(),
            created_at: now,
            updated_at: now,
        };
        state.store_version(&clone);
        state.packages.insert(clone.id, clone.clone());
        emit_audit(
            &ctx,
            EVENT_PACKAGE_CLONED,
            &caller.actor,
            "saas_packages.package.cloned",
            "saas_package",
            &clone.id.to_string(),
            None,
            json!({ "slug": clone.slug, "source_package_id": source.id }),
        );
        Ok(clone)
    })();
    match result {
        Ok(package) => created(package),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_package_versions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let package = load_package(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<PackageVersion> = state
            .versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| version.package_id == package.id)
            .collect();
        items.sort_by(|a, b| b.version.cmp(&a.version));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_package_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackageVersion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let (raw_id, raw_version) = path.into_inner();
        let package = load_package(&state, &raw_id)?;
        let version: i32 = raw_version.trim().parse().map_err(|_| {
            UnifiedError::bad_request(format!("Invalid version \"{raw_version}\" (expected an integer)"))
        })?;
        load_version(&state, &package, version)
    })();
    match result {
        Ok(version) => ok(version),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn validate_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ValidatePackageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let package = load_package(&state, &path.into_inner())?;
        let company_id = match &body.company_id {
            Some(raw) => Some(parse_uuid(raw, "company")?),
            None => None,
        };
        if let Some(company_id) = &company_id {
            require_tenant_scope(&req, company_id)?;
        }
        let module_ids: Vec<String> = package
            .modules
            .iter()
            .map(|entry| entry.module_id.clone())
            .collect();
        let mut unknown: Vec<String> = Vec::new();
        let mut not_entitled: Vec<String> = Vec::new();
        let mut module_rows: Vec<Value> = Vec::new();
        for entry in &package.modules {
            let registered = ctx.modules.get(&entry.module_id).is_some();
            if !registered {
                unknown.push(entry.module_id.clone());
            }
            let entitled = match (&company_id, registered) {
                (Some(company_id), true) => {
                    match entitlement_available_http(&ctx, company_id, &entry.module_id) {
                        Ok(entitled) => Some(entitled),
                        Err(_) => Some(false),
                    }
                }
                _ => None,
            };
            if entitled == Some(false) {
                not_entitled.push(entry.module_id.clone());
            }
            module_rows.push(json!({
                "module_id": entry.module_id,
                "required": entry.required,
                "sort_order": entry.sort_order,
                "registered": registered,
                "entitled": entitled,
            }));
        }
        let apply_order = resolve_apply_order(&ctx.modules, &module_ids)
            .ok();
        Ok(json!({
            "package_id": package.id,
            "slug": package.slug,
            "version": package.current_version,
            "status": package.status,
            "modules": module_rows,
            "apply_order": apply_order,
            "unknown_modules": unknown,
            "not_entitled_modules": not_entitled,
            "valid": unknown.is_empty() && not_entitled.is_empty(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_package_modules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let package = load_package(&state, &path.into_inner())?;
        let params = query.into_inner();
        let items: Vec<Value> = package
            .modules
            .iter()
            .map(|entry| {
                let module = ctx.modules.get(&entry.module_id);
                json!({
                    "module_id": entry.module_id,
                    "required": entry.required,
                    "sort_order": entry.sort_order,
                    "registered": module.is_some(),
                    "name": module.as_ref().map(|module| module.name().to_string()),
                    "available_as_product": module.as_ref().map(|module| module.available_as_product()),
                })
            })
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn diff_packages(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackageDiff> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let (from_id, to_id) = path.into_inner();
        let from = load_package(&state, &from_id)?;
        let to = load_package(&state, &to_id)?;
        let mut diff = module_diff(&from.modules, &to.modules);
        diff.from_package_id = from.id;
        diff.to_package_id = to.id;
        Ok(diff)
    })();
    match result {
        Ok(diff) => ok(diff),
        Err(error) => err(error, debug),
    }
}
