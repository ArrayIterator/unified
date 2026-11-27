// Properties - Property handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// Property handlers

pub(crate) async fn list_properties(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Property> = state
            .properties
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|property| {
                if !in_company_scope(&property.company_id, &_scope) {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if property.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if property.company_id.map(|id| id.to_string()) != Some(company_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        property.title,
                        property.description.clone().unwrap_or_default(),
                        property.slug,
                        property.address.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.cmp(&b.title),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    body: web::Json<CreatePropertyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_CREATE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let slug = match input.slug {
            Some(raw) => {
                let slug = raw.trim().to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens only",
                    ));
                }
                slug
            }
            None => slugify(&title),
        };
        if slug_in_use(&state, &slug, None) {
            return Err(UnifiedError::conflict(format!(
                "A property with slug \"{slug}\" already exists"
            )));
        }
        let latitude = input
            .latitude
            .map(valid_latitude)
            .transpose()?;
        let longitude = input
            .longitude
            .map(valid_longitude)
            .transpose()?;
        if latitude.is_some() != longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "latitude and longitude must be given together",
            ));
        }
        let address = input
            .address
            .map(|value| require_text(&value, "address", MAX_ADDRESS_LEN))
            .transpose()?;
        let total_area_sqm = match input.total_area_sqm {
            Some(area) => {
                if !area.is_finite() || area < 0.0 {
                    return Err(UnifiedError::bad_request(
                        "total_area_sqm must be a finite number >= 0",
                    ));
                }
                Some((area * 10_000.0).round() / 10_000.0)
            }
            None => None,
        };
        if input.cover_storage_target.is_some() != input.cover_storage_key.is_some() {
            return Err(UnifiedError::bad_request(
                "cover_storage_target and cover_storage_key must be given together",
            ));
        }
        if let (Some(target), Some(key)) = (&input.cover_storage_target, &input.cover_storage_key) {
            validate_storage_reference(target, key)?;
        }
        let description = input
            .description
            .map(|value| require_text(&value, "description", MAX_DESCRIPTION_LEN))
            .transpose()?;
        let now = Utc::now();
        let property = Property {
            id: Uuid::now_v7(),
            title,
            description,
            slug,
            latitude,
            longitude,
            address,
            total_area_sqm,
            company_id: require_company_match_uuid(&_tenant, &req, &input.company_id)?,
            status: PropertyStatus::Draft,
            cover_storage_target: input.cover_storage_target,
            cover_storage_key: input.cover_storage_key,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.properties.insert(property.id, property.clone());
        state.emit(
            "properties.created",
            json!({
                "property_id": property.id.to_string(),
                "slug": property.slug,
                "title": property.title,
                "actor_id": caller.actor,
            }),
        );
        Ok(property)
    })();
    match result {
        Ok(property) => created(property),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let property = load_property(&state, &path.into_inner())?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        Ok(property)
    })();
    match result {
        Ok(property) => ok(property),
        Err(error) => err(error, debug),
    }
}

/// The caller may edit a property they created; anyone else needs
/// the manage capability.
pub(crate) fn can_edit_property(ctx: &AppContext, role: &str, property: &Property, actor: &str) -> UnifiedResult<()> {
    if property.created_by == actor {
        return Ok(());
    }
    require_permission(ctx, role, PERM_PROPERTIES_MANAGE)
}

pub(crate) async fn update_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePropertyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let mut property = load_property(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&property.company_id, &_scope)?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        if matches!(property.status, PropertyStatus::Archived | PropertyStatus::SoldOut) {
            return Err(UnifiedError::conflict(format!(
                "A {} property cannot be edited",
                property.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(title) = input.title {
            property.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = input.description {
            property.description = description
                .map(|value| require_text(&value, "description", MAX_DESCRIPTION_LEN))
                .transpose()?;
        }
        if let Some(slug) = input.slug {
            let slug = slug.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only",
                ));
            }
            if slug_in_use(&state, &slug, Some(property.id)) {
                return Err(UnifiedError::conflict(format!(
                    "A property with slug \"{slug}\" already exists"
                )));
            }
            property.slug = slug;
        }
        if let Some(latitude) = input.latitude {
            property.latitude = latitude.map(valid_latitude).transpose()?;
        }
        if let Some(longitude) = input.longitude {
            property.longitude = longitude.map(valid_longitude).transpose()?;
        }
        if property.latitude.is_some() != property.longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "latitude and longitude must be given together",
            ));
        }
        if let Some(address) = input.address {
            property.address = address
                .map(|value| require_text(&value, "address", MAX_ADDRESS_LEN))
                .transpose()?;
        }
        if let Some(total_area_sqm) = input.total_area_sqm {
            property.total_area_sqm = match total_area_sqm {
                Some(area) => {
                    if !area.is_finite() || area < 0.0 {
                        return Err(UnifiedError::bad_request(
                            "total_area_sqm must be a finite number >= 0",
                        ));
                    }
                    Some((area * 10_000.0).round() / 10_000.0)
                }
                None => None,
            };
        }
        if let Some(company_id) = input.company_id {
            // A newly named company must match the caller scope;
            // an explicit clear stays a clear and is never
            // re-stamped with the caller scope.
            property.company_id = require_company_change_uuid_for(&_scope, &company_id)?;
        }
        if let Some(target) = input.cover_storage_target {
            property.cover_storage_target = target;
        }
        if let Some(key) = input.cover_storage_key {
            property.cover_storage_key = key;
        }
        if property.cover_storage_target.is_some() != property.cover_storage_key.is_some() {
            return Err(UnifiedError::bad_request(
                "cover_storage_target and cover_storage_key must be given together",
            ));
        }
        if let (Some(target), Some(key)) =
            (&property.cover_storage_target, &property.cover_storage_key)
        {
            validate_storage_reference(target, key)?;
        }
        property.updated_at = Utc::now();
        state.properties.insert(property.id, property.clone());
        state.emit(
            "properties.updated",
            json!({
                "property_id": property.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(property)
    })();
    match result {
        Ok(property) => ok(property),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let property = load_property(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&property.company_id, &_scope)?;
        if !matches!(property.status, PropertyStatus::Draft | PropertyStatus::Archived) {
            return Err(UnifiedError::conflict(format!(
                "Only a draft or archived property can be deleted (status is {})",
                property.status.as_str()
            )));
        }
        // Cascade in memory: plans, blocks, history, purchases.
        let plan_ids: Vec<Uuid> = state
            .plans_of_property(property.id)
            .into_iter()
            .map(|plan| plan.id)
            .collect();
        for plan_id in &plan_ids {
            let block_ids: Vec<Uuid> = state
                .blocks_of_plan(*plan_id)
                .into_iter()
                .map(|block| block.id)
                .collect();
            for block_id in block_ids {
                state.blocks.remove(&block_id);
                let history_ids: Vec<Uuid> = state
                    .history
                    .iter()
                    .filter(|entry| entry.value().block_id == block_id)
                    .map(|entry| *entry.key())
                    .collect();
                for history_id in history_ids {
                    state.history.remove(&history_id);
                }
            }
            state.plans.remove(plan_id);
        }
        let purchase_ids: Vec<Uuid> = state
            .purchases
            .iter()
            .filter(|entry| entry.value().property_id == property.id)
            .map(|entry| *entry.key())
            .collect();
        for purchase_id in purchase_ids {
            state.purchases.remove(&purchase_id);
        }
        state.properties.remove(&property.id);
        state.emit(
            "properties.deleted",
            json!({
                "property_id": property.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": property.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn change_property_status(
    state: &PropertiesState,
    property: &mut Property,
    to: PropertyStatus,
    actor: &str,
    event: &str,
) {
    property.status = to;
    property.updated_at = Utc::now();
    state.emit(
        event,
        json!({
            "property_id": property.id.to_string(),
            "status": to.as_str(),
            "actor_id": actor,
        }),
    );
}

pub(crate) async fn publish_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let mut property = load_property(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&property.company_id, &_scope)?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        if property.status != PropertyStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft property can be published (status is {})",
                property.status.as_str()
            )));
        }
        change_property_status(
            &state,
            &mut property,
            PropertyStatus::Published,
            &caller.actor,
            "properties.published",
        );
        state.properties.insert(property.id, property.clone());
        Ok(property)
    })();
    match result {
        Ok(property) => ok(property),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn archive_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let mut property = load_property(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&property.company_id, &_scope)?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        if property.status == PropertyStatus::Archived {
            return Err(UnifiedError::conflict("Property is already archived"));
        }
        change_property_status(
            &state,
            &mut property,
            PropertyStatus::Archived,
            &caller.actor,
            "properties.archived",
        );
        state.properties.insert(property.id, property.clone());
        Ok(property)
    })();
    match result {
        Ok(property) => ok(property),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn mark_property_sold_out(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let mut property = load_property(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&property.company_id, &_scope)?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        if property.status != PropertyStatus::Published {
            return Err(UnifiedError::conflict(format!(
                "Only a published property can be marked sold out (status is {})",
                property.status.as_str()
            )));
        }
        change_property_status(
            &state,
            &mut property,
            PropertyStatus::SoldOut,
            &caller.actor,
            "properties.sold_out",
        );
        state.properties.insert(property.id, property.clone());
        Ok(property)
    })();
    match result {
        Ok(property) => ok(property),
        Err(error) => err(error, debug),
    }
}
