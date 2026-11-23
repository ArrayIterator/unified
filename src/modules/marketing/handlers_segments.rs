// Marketing Automation — Segment handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::service_sequences::*;
use super::handlers_campaigns::*;
use super::handlers_sequences::*;
use super::handlers_scores::*;
use super::handlers_audience::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Segment handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct SegmentInput {
    name: String,
    description: Option<String>,
    rule_tree: Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SegmentUpdateInput {
    name: Option<String>,
    description: Option<String>,
    rule_tree: Option<Value>,
    status: Option<String>,
}

pub(crate) async fn list_segments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Segment>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<Segment> = state
            .segments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|segment| {
                matches_search(&format!("{} {}", segment.name, segment.description), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| segment.status == *status)
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_segment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<SegmentInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Segment> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 200)?;
        let settings = state.settings_snapshot();
        validate_rule_tree(&input.rule_tree, &settings, 0)?;
        let now = now_string();
        let segment = Segment {
            id: new_id(),
            name,
            description: input.description.unwrap_or_default(),
            rule_tree: input.rule_tree,
            status: "active".to_string(),
            last_evaluated_at: None,
            last_size: 0,
            created_at: now.clone(),
            updated_at: now,
        };
        state.segments.insert(segment.id.clone(), segment.clone());
        Ok(segment)
    })();
    match result {
        Ok(segment) => created(segment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_segment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Segment> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        state
            .segments
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Segment not found"))
    })();
    match result {
        Ok(segment) => ok(segment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_segment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SegmentUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Segment> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        let input = body.into_inner();
        if let Some(rule_tree) = &input.rule_tree {
            let settings = state.settings_snapshot();
            validate_rule_tree(rule_tree, &settings, 0)?;
        }
        if let Some(status) = &input.status {
            if status != "active" && status != "archived" {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\" or \"archived\"",
                ));
            }
        }
        let mut segment = state
            .segments
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Segment not found"))?;
        if let Some(name) = input.name {
            segment.name = require_text(&name, "name", 200)?;
        }
        if let Some(description) = input.description {
            segment.description = description;
        }
        if let Some(rule_tree) = input.rule_tree {
            segment.rule_tree = rule_tree;
        }
        if let Some(status) = input.status {
            segment.status = status;
        }
        segment.updated_at = now_string();
        Ok(segment.clone())
    })();
    match result {
        Ok(segment) => ok(segment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_segment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        if state
            .campaigns
            .iter()
            .any(|entry| entry.value().segment_id == Some(id.to_string()))
        {
            return Err(UnifiedError::conflict(
                "Segment is used by a campaign; remove it from the campaign first",
            ));
        }
        match state.segments.remove(&id.to_string()) {
            Some(_) => {
                state.segment_history.remove(&id.to_string());
                state.segment_members.remove(&id.to_string());
                Ok(json!({ "deleted": true, "id": id.to_string() }))
            }
            None => Err(UnifiedError::not_found("Segment not found")),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn evaluate_segment_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        let segment = state
            .segments
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Segment not found"))?;
        let (members, evaluated) = evaluate_segment(&state, &segment);
        record_segment_evaluation(&state, &id.to_string(), &members, evaluated);
        state.emit(
            EVENT_MARKETING_SEGMENT_EVALUATED,
            json!({
                "segment_id": id.to_string(),
                "size": members.len(),
                "evaluated_contacts": evaluated,
            }),
        );
        Ok(json!({
            "segment_id": id.to_string(),
            "size": members.len(),
            "evaluated_contacts": evaluated,
            "member_contact_ids": members,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_segment_members(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AudienceContact>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        if !state.segments.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Segment not found"));
        }
        let params = query.into_inner();
        let member_ids: Vec<String> = state
            .segment_members
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let mut items: Vec<AudienceContact> = member_ids
            .iter()
            .filter_map(|contact_id| {
                state
                    .audience
                    .get(contact_id)
                    .map(|entry| entry.value().clone())
            })
            .filter(|contact| matches_search(&format!("{} {}", contact.name, contact.email), &params))
            .collect();
        items.sort_by(|a, b| a.email.cmp(&b.email));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_segment_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<SegmentSizeSnapshot>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "segment")?;
        if !state.segments.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Segment not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<SegmentSizeSnapshot> = state
            .segment_history
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        items.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}
