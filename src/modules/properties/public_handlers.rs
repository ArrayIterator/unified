// Properties — Public handlers (no auth; gated by the API Manager ApiAccessRule).

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

// ---------------------------------------------------------------------------
// Public handlers (no auth; gated by the API Manager ApiAccessRule)
// ---------------------------------------------------------------------------

/// Public list view: the property plus a plan count. Never
/// exposes buyer or order data (blocks are not listed here).
pub(crate) fn public_list_view(state: &PropertiesState, property: &Property) -> Value {
    let mut value = serde_json::to_value(property).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "plan_count".to_string(),
            Value::Number(state.plans_of_property(property.id).len().into()),
        );
    }
    value
}

pub(crate) async fn list_public_properties(
    state: web::Data<PropertiesState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let search = params.search.clone().unwrap_or_default().to_lowercase();
    let mut items: Vec<Value> = state
        .properties
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|property| property.status == PropertyStatus::Published)
        .filter(|property| {
            if search.is_empty() {
                return true;
            }
            let haystack = format!(
                "{} {} {} {}",
                property.title,
                property.description.clone().unwrap_or_default(),
                property.slug,
                property.address.clone().unwrap_or_default()
            )
            .to_lowercase();
            haystack.contains(&search)
        })
        .map(|property| public_list_view(&state, &property))
        .collect();
    let sort = params.allowed_sort(&["title", "created_at"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "title" => a
                .get("title")
                .and_then(Value::as_str)
                .cmp(&b.get("title").and_then(Value::as_str)),
            _ => a
                .get("created_at")
                .and_then(Value::as_str)
                .cmp(&b.get("created_at").and_then(Value::as_str)),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    ok(paginate(items, &params).to_json())
}

pub(crate) async fn get_public_property(
    state: web::Data<PropertiesState>,
    path: web::Path<String>,
) -> HttpResponse {
    let slug = path.into_inner();
    match load_property_by_slug(&state, &slug) {
        Ok(property) => {
            if property.status != PropertyStatus::Published {
                return err(
                    UnifiedError::not_found(format!("Property not found: {slug}")),
                    false,
                );
            }
            ok(public_property_view(&state, &property))
        }
        Err(error) => err(error, false),
    }
}

pub(crate) async fn get_public_plan_svg(
    state: web::Data<PropertiesState>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (slug, plan_id) = path.into_inner();
    let loaded = (|| {
        let property = load_property_by_slug(&state, &slug)?;
        if property.status != PropertyStatus::Published {
            return Err(UnifiedError::not_found(format!(
                "Property not found: {slug}"
            )));
        }
        let plan = load_plan(&state, property.id, &plan_id)?;
        Ok::<(Property, PropertyPlan), UnifiedError>((property, plan))
    })();
    let (property, plan) = match loaded {
        Ok(pair) => pair,
        Err(error) => return err(error, false),
    };
    let blocks = state.blocks_of_plan(plan.id);
    match render_plan_svg(&plan, &blocks, &format!("{} — {}", property.title, plan.name)) {
        Ok(svg) => HttpResponse::Ok()
            .content_type("image/svg+xml; charset=utf-8")
            .body(svg),
        Err(error) => err(error, false),
    }
}

pub(crate) async fn get_public_plan_print(
    state: web::Data<PropertiesState>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (slug, plan_id) = path.into_inner();
    let loaded = (|| {
        let property = load_property_by_slug(&state, &slug)?;
        if property.status != PropertyStatus::Published {
            return Err(UnifiedError::not_found(format!(
                "Property not found: {slug}"
            )));
        }
        let plan = load_plan(&state, property.id, &plan_id)?;
        Ok::<(Property, PropertyPlan), UnifiedError>((property, plan))
    })();
    let (property, plan) = match loaded {
        Ok(pair) => pair,
        Err(error) => return err(error, false),
    };
    let blocks = state.blocks_of_plan(plan.id);
    match render_plan_svg(&plan, &blocks, &format!("{} — {}", property.title, plan.name)) {
        Ok(svg) => {
            let html = render_print_html(&property, &plan, &blocks, &svg, false);
            HttpResponse::Ok()
                .content_type("text/html; charset=utf-8")
                .body(html)
        }
        Err(error) => err(error, false),
    }
}
