// Properties - Block handlers.

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
use super::helpers::{round_measure};

// Block handlers

pub(crate) async fn list_blocks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<PropertyBlock> = state
            .blocks_of_plan(plan.id)
            .into_iter()
            .filter(|block| {
                if let Some(status) = &status_filter {
                    return block.status.as_str() == status.as_str();
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "price", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.cmp(&b.title),
                "price" => a.price.cmp(&b.price),
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

pub(crate) async fn create_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<CreateBlockRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_CREATE)?;
        let (property_id, plan_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        if matches!(property.status, PropertyStatus::Archived | PropertyStatus::SoldOut) {
            return Err(UnifiedError::conflict(format!(
                "Blocks cannot be added to a {} property",
                property.status.as_str()
            )));
        }
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let notes = input
            .notes
            .map(|value| require_text(&value, "notes", MAX_NOTES_LEN))
            .transpose()?;
        let validated = validate_geometry(&input.geometry)?;
        let price = valid_price(input.price.unwrap_or(0))?;
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        let link_allowlist = {
            let settings = state
                .location_settings
                .read()
                .expect("settings lock poisoned");
            settings.link_allowlist_hosts.clone()
        };
        let behavior = input.click_behavior.unwrap_or_default();
        let (click_link_kind, click_link_target) = validate_click_target(
            behavior,
            input.click_link_kind,
            input.click_link_target.as_deref(),
            &link_allowlist,
        )?;
        let now = Utc::now();
        let geometry_hash_value = geometry_hash(&input.geometry);
        let block = PropertyBlock {
            id: Uuid::now_v7(),
            plan_id: plan.id,
            property_id: property.id,
            title,
            notes,
            geometry: input.geometry,
            geometry_hash: geometry_hash_value,
            area_sqm: validated.area_sqm.map(round_measure),
            length_m: validated.length_m.map(round_measure),
            price,
            currency,
            status: BlockStatus::Available,
            assigned_customer_id: None,
            click_behavior: behavior,
            click_link_kind,
            click_link_target,
            order_id: None,
            created_at: now,
            updated_at: now,
        };
        state.blocks.insert(block.id, block.clone());
        state.emit(
            "properties.block.created",
            json!({
                "property_id": property.id.to_string(),
                "plan_id": plan.id.to_string(),
                "block_id": block.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(block)
    })();
    match result {
        Ok(block) => created(block),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        load_block(&state, property.id, plan.id, &block_id)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<UpdateBlockRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        let input = body.into_inner();
        // Edit rules by status: a sold / assigned block is a sale
        // record - only its notes may change. A reserved block may
        // change title, notes, and click behaviour, but not its
        // geometry or price. An available / blocked block may
        // change everything.
        match block.status {
            BlockStatus::Sold | BlockStatus::Assigned => {
                if input.geometry.is_some() || input.price.is_some() || input.title.is_some()
                    || input.click_behavior.is_some() || input.click_link_kind.is_some()
                    || input.click_link_target.is_some() || input.currency.is_some()
                {
                    return Err(UnifiedError::conflict(format!(
                        "Only the notes of a {} block can be edited",
                        block.status.as_str()
                    )));
                }
            }
            BlockStatus::Reserved => {
                if input.geometry.is_some() || input.price.is_some() || input.currency.is_some() {
                    return Err(UnifiedError::conflict(
                        "Geometry and price of a reserved block cannot be edited",
                    ));
                }
            }
            BlockStatus::Available | BlockStatus::Blocked => {}
        }
        if let Some(title) = input.title {
            block.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(notes) = input.notes {
            block.notes = notes
                .map(|value| require_text(&value, "notes", MAX_NOTES_LEN))
                .transpose()?;
        }
        if let Some(geometry) = input.geometry {
            let validated = validate_geometry(&geometry)?;
            block.geometry = geometry;
            block.geometry_hash = geometry_hash(&block.geometry);
            block.area_sqm = validated.area_sqm.map(round_measure);
            block.length_m = validated.length_m.map(round_measure);
        }
        if let Some(price) = input.price {
            block.price = valid_price(price)?;
        }
        if let Some(currency) = input.currency {
            block.currency = valid_currency(&currency)?;
        }
        if input.click_behavior.is_some() || input.click_link_kind.is_some() || input.click_link_target.is_some() {
            let behavior = input.click_behavior.unwrap_or(block.click_behavior);
            let link_allowlist = {
                let settings = state
                    .location_settings
                    .read()
                    .expect("settings lock poisoned");
                settings.link_allowlist_hosts.clone()
            };
            let (kind, target) = validate_click_target(
                behavior,
                input.click_link_kind.unwrap_or(block.click_link_kind),
                input
                    .click_link_target
                    .flatten()
                    .as_deref()
                    .or(block.click_link_target.as_deref()),
                &link_allowlist,
            )?;
            block.click_behavior = behavior;
            block.click_link_kind = kind;
            block.click_link_target = target;
        }
        block.updated_at = Utc::now();
        state.blocks.insert(block.id, block.clone());
        state.emit(
            "properties.block.updated",
            json!({
                "property_id": block.property_id.to_string(),
                "plan_id": block.plan_id.to_string(),
                "block_id": block.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let block = load_block(&state, property.id, plan.id, &block_id)?;
        if !matches!(block.status, BlockStatus::Available | BlockStatus::Blocked) {
            return Err(UnifiedError::conflict(format!(
                "Only an available or blocked block can be deleted (status is {})",
                block.status.as_str()
            )));
        }
        state.blocks.remove(&block.id);
        // History rows are the audit trail for the block: deleting
        // the block must not erase them.
        state.emit(
            "properties.block.deleted",
            json!({
                "property_id": block.property_id.to_string(),
                "plan_id": block.plan_id.to_string(),
                "block_id": block.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": block.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Resolve the buyer for a block action: the caller by default;
/// acting for another user needs the manage capability.
pub(crate) fn resolve_buyer(
    ctx: &AppContext,
    caller: &Caller,
    requested: Option<String>,
) -> UnifiedResult<String> {
    match requested {
        Some(buyer_id) => {
            let buyer_id = buyer_id.trim().to_string();
            if buyer_id.is_empty() {
                return Err(UnifiedError::bad_request("buyer_id must not be empty"));
            }
            if buyer_id != caller.actor {
                require_permission(ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
            }
            Ok(buyer_id)
        }
        None => Ok(caller.actor.clone()),
    }
}

pub(crate) async fn reserve_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        if property.status != PropertyStatus::Published {
            return Err(UnifiedError::conflict(format!(
                "Blocks of a {} property cannot be reserved",
                property.status.as_str()
            )));
        }
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status != BlockStatus::Available {
            return Err(UnifiedError::conflict(format!(
                "Only an available block can be reserved (status is {})",
                block.status.as_str()
            )));
        }
        let input = body.into_inner();
        let buyer = resolve_buyer(&ctx, &caller, input.buyer_id)?;
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        block.assigned_customer_id = Some(buyer.clone());
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Reserved,
            &caller.actor,
            reason,
            None,
            "properties.block.reserved",
        );
        state.blocks.insert(block.id, block.clone());
        state.notify(
            "properties.block.reserved",
            &buyer,
            &format!("Block \"{}\" reserved", block.title),
            &format!(
                "Block \"{}\" is reserved for you. Price: {} {}.",
                block.title,
                format_money(block.price, &block.currency),
                block.currency
            ),
            Some(format!("/api/v1/properties/{}", block.property_id)),
        );
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn release_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status != BlockStatus::Reserved {
            return Err(UnifiedError::conflict(format!(
                "Only a reserved block can be released (status is {})",
                block.status.as_str()
            )));
        }
        // Only the buyer who holds the reservation, or a manager,
        // may release it.
        let holder = block.assigned_customer_id.clone().unwrap_or_default();
        if holder != caller.actor {
            require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        }
        let input = body.into_inner();
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        // Cancel any purchase still waiting on this block.
        let pending: Vec<Uuid> = state
            .purchases
            .iter()
            .filter(|entry| {
                entry.value().block_id == block.id
                    && matches!(
                        entry.value().status.as_str(),
                        "requested" | "order_created" | "pending_verification"
                    )
            })
            .map(|entry| *entry.key())
            .collect();
        for purchase_id in pending {
            if let Some(mut purchase) = state.purchases.get_mut(&purchase_id) {
                purchase.status = "cancelled".to_string();
                purchase.updated_at = Utc::now();
            }
        }
        block.assigned_customer_id = None;
        block.order_id = None;
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Available,
            &caller.actor,
            reason,
            None,
            "properties.block.released",
        );
        state.blocks.insert(block.id, block.clone());
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn purchase_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BlockPurchase> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        if property.status != PropertyStatus::Published {
            return Err(UnifiedError::conflict(format!(
                "Blocks of a {} property cannot be purchased",
                property.status.as_str()
            )));
        }
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status != BlockStatus::Available {
            return Err(UnifiedError::conflict(format!(
                "Only an available block can be purchased (status is {})",
                block.status.as_str()
            )));
        }
        let input = body.into_inner();
        let buyer = resolve_buyer(&ctx, &caller, input.buyer_id)?;
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        let now = Utc::now();
        let purchase = BlockPurchase {
            id: Uuid::now_v7(),
            property_id: property.id,
            plan_id: plan.id,
            block_id: block.id,
            buyer_id: buyer.clone(),
            price: block.price,
            currency: block.currency.clone(),
            order_id: None,
            status: "requested".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.purchases.insert(purchase.id, purchase.clone());
        // Hold the block while the store creates the order and the
        // buyer pays: available -> reserved.
        block.assigned_customer_id = Some(buyer.clone());
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Reserved,
            &caller.actor,
            reason,
            None,
            "properties.block.reserved",
        );
        state.blocks.insert(block.id, block.clone());
        state.emit(
            "properties.block.purchase_requested",
            json!({
                "purchase_id": purchase.id.to_string(),
                "property_id": property.id.to_string(),
                "plan_id": plan.id.to_string(),
                "block_id": block.id.to_string(),
                "buyer_id": buyer,
                "price": minor_to_major(purchase.price, &purchase.currency),
                "currency": purchase.currency,
            }),
        );
        state.notify(
            "properties.block.purchase_requested",
            &purchase.buyer_id,
            &format!("Purchase started for block \"{}\"", block.title),
            &format!(
                "Your purchase of block \"{}\" has started. Price: {} {}. Complete the payment to confirm the sale.",
                block.title,
                format_money(block.price, &block.currency),
                block.currency
            ),
            Some(format!("/api/v1/properties/{}", block.property_id)),
        );
        Ok(purchase)
    })();
    match result {
        Ok(purchase) => created(purchase),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn assign_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if !matches!(block.status, BlockStatus::Available | BlockStatus::Reserved) {
            return Err(UnifiedError::conflict(format!(
                "Only an available or reserved block can be assigned (status is {})",
                block.status.as_str()
            )));
        }
        let input = body.into_inner();
        let buyer = match input.buyer_id {
            Some(buyer_id) => {
                let buyer_id = buyer_id.trim().to_string();
                if buyer_id.is_empty() {
                    return Err(UnifiedError::bad_request("buyer_id must not be empty"));
                }
                buyer_id
            }
            None => {
                return Err(UnifiedError::bad_request(
                    "buyer_id is required to assign a block",
                ));
            }
        };
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        block.assigned_customer_id = Some(buyer.clone());
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Assigned,
            &caller.actor,
            reason,
            None,
            "properties.block.assigned",
        );
        state.blocks.insert(block.id, block.clone());
        state.notify(
            "properties.block.assigned",
            &buyer,
            &format!("Block \"{}\" assigned to you", block.title),
            &format!(
                "Block \"{}\" has been assigned to you. Price: {} {}.",
                block.title,
                format_money(block.price, &block.currency),
                block.currency
            ),
            Some(format!("/api/v1/properties/{}", block.property_id)),
        );
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn block_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status == BlockStatus::Sold {
            return Err(UnifiedError::conflict("A sold block cannot be blocked"));
        }
        if block.status == BlockStatus::Blocked {
            return Err(UnifiedError::conflict("Block is already blocked"));
        }
        if block.status == BlockStatus::Reserved {
            return Err(UnifiedError::conflict(
                "A reserved block cannot be blocked: cancel the pending purchase first via release",
            ));
        }
        let input = body.into_inner();
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Blocked,
            &caller.actor,
            reason,
            None,
            "properties.block.blocked",
        );
        state.blocks.insert(block.id, block.clone());
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn unblock_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status == BlockStatus::Reserved {
            return Err(UnifiedError::conflict(
                "A reserved block cannot be unblocked: cancel the pending purchase first via release",
            ));
        }
        if block.status != BlockStatus::Blocked {
            return Err(UnifiedError::conflict(format!(
                "Only a blocked block can be unblocked (status is {})",
                block.status.as_str()
            )));
        }
        let input = body.into_inner();
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        block.assigned_customer_id = None;
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Available,
            &caller.actor,
            reason,
            None,
            "properties.block.unblocked",
        );
        state.blocks.insert(block.id, block.clone());
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn correct_block_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<CorrectStatusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status != BlockStatus::Sold {
            return Err(UnifiedError::conflict(format!(
                "Status correction applies to sold blocks only (status is {})",
                block.status.as_str()
            )));
        }
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let target = match input.target_status.as_str() {
            "available" => BlockStatus::Available,
            "reserved" => BlockStatus::Reserved,
            "assigned" => BlockStatus::Assigned,
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "target_status \"{other}\" is not a valid correction target (available, reserved, assigned)"
                )));
            }
        };
        match target {
            BlockStatus::Reserved | BlockStatus::Assigned => {
                let buyer = match input.buyer_id {
                    Some(buyer_id) => {
                        let buyer_id = buyer_id.trim().to_string();
                        if buyer_id.is_empty() {
                            return Err(UnifiedError::bad_request(
                                "buyer_id must not be empty",
                            ));
                        }
                        buyer_id
                    }
                    None => {
                        return Err(UnifiedError::bad_request(
                            "buyer_id is required when correcting to reserved or assigned",
                        ));
                    }
                };
                block.assigned_customer_id = Some(buyer);
            }
            BlockStatus::Available => {
                block.assigned_customer_id = None;
                block.order_id = None;
                // Correcting a sold block back to available voids
                // the sale, so any purchase still waiting on this
                // block is cancelled as well. BlockPurchase carries
                // no reason field; the "admin correction" reason is
                // recorded on the audit history row written below.
                let pending: Vec<Uuid> = state
                    .purchases
                    .iter()
                    .filter(|entry| {
                        entry.value().block_id == block.id
                            && matches!(
                                entry.value().status.as_str(),
                                "requested" | "order_created" | "pending_verification"
                            )
                    })
                    .map(|entry| *entry.key())
                    .collect();
                for purchase_id in pending {
                    if let Some(mut purchase) = state.purchases.get_mut(&purchase_id) {
                        purchase.status = "cancelled".to_string();
                        purchase.updated_at = Utc::now();
                    }
                }
            }
            _ => {}
        }
        move_block_status(
            &state,
            &mut block,
            target,
            &caller.actor,
            Some(format!("admin correction: {reason}")),
            None,
            "properties.block.status_corrected",
        );
        state.blocks.insert(block.id, block.clone());
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_block_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let block = load_block(&state, property.id, plan.id, &block_id)?;
        let params = query.into_inner();
        let mut items: Vec<PropertyBlockHistory> = state
            .history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.block_id == block.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn export_blocks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE) {
        return err(error, debug);
    }
    let (property_id, plan_id) = path.into_inner();
    let load = (|| {
        let property = load_property(&state, &property_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        Ok::<(Property, PropertyPlan), UnifiedError>((property, plan))
    })();
    let (property, plan) = match load {
        Ok(pair) => pair,
        Err(error) => return err(error, debug),
    };
    let blocks = state.blocks_of_plan(plan.id);
    match query.format.as_str() {
        "csv" => HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header((
                "Content-Disposition",
                // The filename uses the plan id, never the plan
                // name, so user-supplied text can never reach a
                // header value.
                format!("attachment; filename=\"plan-{}-blocks.csv\"", plan.id),
            ))
            .body(blocks_to_csv(&blocks)),
        "json" => ok(json!({
            "property_id": property.id.to_string(),
            "plan_id": plan.id.to_string(),
            "blocks": blocks,
        })),
        other => err(
            UnifiedError::bad_request(format!(
                "Unknown export format \"{other}\": expected csv or json"
            )),
            debug,
        ),
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead. The request also carries the
    /// middleware-resolved tenant context (no company), so the
    /// tenant gate passes and the caller scope is unresolved
    /// (global records only).
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
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
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    /// App context whose `role.check` hook allows exactly the
    /// given permissions (all other permissions are denied).
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed.iter().map(|permission| permission.to_string()).collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        let ctx = AppContext::new(
            unified_core::config::AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        );
        web::Data::new(Arc::new(ctx))
    }

    fn test_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    fn seed_property(state: &PropertiesState, status: PropertyStatus) -> Property {
        let now = Utc::now();
        let property = Property {
            id: Uuid::now_v7(),
            title: "Estate".to_string(),
            description: None,
            slug: format!("estate-{}", Uuid::now_v7().as_simple()),
            latitude: None,
            longitude: None,
            address: None,
            total_area_sqm: None,
            company_id: None,
            status,
            cover_storage_target: None,
            cover_storage_key: None,
            created_by: "tester".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.properties.insert(property.id, property.clone());
        property
    }

    fn seed_plan(state: &PropertiesState, property: &Property) -> PropertyPlan {
        let now = Utc::now();
        let plan = PropertyPlan {
            id: Uuid::now_v7(),
            property_id: property.id,
            name: "Plan 1".to_string(),
            description: None,
            geometry: None,
            geometry_hash: None,
            created_at: now,
            updated_at: now,
        };
        state.plans.insert(plan.id, plan.clone());
        plan
    }

    fn seed_block(
        state: &PropertiesState,
        property: &Property,
        plan: &PropertyPlan,
        status: BlockStatus,
    ) -> PropertyBlock {
        let now = Utc::now();
        let block = PropertyBlock {
            id: Uuid::now_v7(),
            plan_id: plan.id,
            property_id: property.id,
            title: "Block A-1".to_string(),
            notes: None,
            geometry: serde_json::json!({"type": "Polygon", "coordinates": []}),
            geometry_hash: String::new(),
            area_sqm: None,
            length_m: None,
            price: 10000,
            currency: "USD".to_string(),
            status,
            assigned_customer_id: Some("buyer-1".to_string()),
            click_behavior: ClickBehavior::default(),
            click_link_kind: None,
            click_link_target: None,
            order_id: None,
            created_at: now,
            updated_at: now,
        };
        state.blocks.insert(block.id, block.clone());
        block
    }

    fn seed_purchase(
        state: &PropertiesState,
        block: &PropertyBlock,
        status: &str,
        order_id: Option<&str>,
    ) -> BlockPurchase {
        let now = Utc::now();
        let purchase = BlockPurchase {
            id: Uuid::now_v7(),
            property_id: block.property_id,
            plan_id: block.plan_id,
            block_id: block.id,
            buyer_id: "buyer-1".to_string(),
            price: block.price,
            currency: block.currency.clone(),
            order_id: order_id.map(str::to_string),
            status: status.to_string(),
            created_at: now,
            updated_at: now,
        };
        state.purchases.insert(purchase.id, purchase.clone());
        purchase
    }

    fn seed_history(state: &PropertiesState, block: &PropertyBlock) {
        let entry = PropertyBlockHistory {
            id: Uuid::now_v7(),
            block_id: block.id,
            property_id: block.property_id,
            plan_id: block.plan_id,
            from_status: "available".to_string(),
            to_status: "reserved".to_string(),
            actor_id: "tester".to_string(),
            reason: None,
            order_id: None,
            created_at: Utc::now(),
        };
        state.history.insert(entry.id, entry);
    }

    fn block_path(property: &Property, plan: &PropertyPlan, block: &PropertyBlock) -> web::Path<(String, String, String)> {
        web::Path::from((
            property.id.to_string(),
            plan.id.to_string(),
            block.id.to_string(),
        ))
    }

    fn action_body() -> web::Json<BlockActionRequest> {
        web::Json(BlockActionRequest {
            buyer_id: None,
            reason: None,
        })
    }

    #[tokio::test]
    async fn reserve_block_rejects_unpublished_property() {
        let state = web::Data::new(PropertiesState::default());
        let property = seed_property(&state, PropertyStatus::Draft);
        let plan = seed_plan(&state, &property);
        let block = seed_block(&state, &property, &plan, BlockStatus::Available);
        let resp = reserve_block(
            test_ctx(&[PERM_PROPERTIES_VIEW]),
            state.clone(),
            test_request(),
            block_path(&property, &plan, &block),
            action_body(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            state.blocks.get(&block.id).unwrap().status,
            BlockStatus::Available
        );
    }

    #[tokio::test]
    async fn block_block_rejects_reserved_block() {
        let state = web::Data::new(PropertiesState::default());
        let property = seed_property(&state, PropertyStatus::Published);
        let plan = seed_plan(&state, &property);
        let block = seed_block(&state, &property, &plan, BlockStatus::Reserved);
        let resp = block_block(
            test_ctx(&[PERM_PROPERTIES_MANAGE]),
            state.clone(),
            test_request(),
            block_path(&property, &plan, &block),
            action_body(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            state.blocks.get(&block.id).unwrap().status,
            BlockStatus::Reserved
        );
    }

    #[tokio::test]
    async fn unblock_block_rejects_reserved_block() {
        let state = web::Data::new(PropertiesState::default());
        let property = seed_property(&state, PropertyStatus::Published);
        let plan = seed_plan(&state, &property);
        let block = seed_block(&state, &property, &plan, BlockStatus::Reserved);
        let resp = unblock_block(
            test_ctx(&[PERM_PROPERTIES_MANAGE]),
            state.clone(),
            test_request(),
            block_path(&property, &plan, &block),
            action_body(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            state.blocks.get(&block.id).unwrap().status,
            BlockStatus::Reserved
        );
    }

    #[tokio::test]
    async fn export_blocks_requires_manage_permission() {
        let state = web::Data::new(PropertiesState::default());
        let property = seed_property(&state, PropertyStatus::Published);
        let plan = seed_plan(&state, &property);
        let query = || web::Query(ExportQuery {
            format: "json".to_string(),
        });
        // A viewer (properties.view only) is forbidden: exports
        // expose assigned_customer_id and order_id.
        let denied = export_blocks(
            test_ctx(&[PERM_PROPERTIES_VIEW]),
            state.clone(),
            test_request(),
            web::Path::from((property.id.to_string(), plan.id.to_string())),
            query(),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        // A manager is allowed.
        let allowed = export_blocks(
            test_ctx(&[PERM_PROPERTIES_VIEW, PERM_PROPERTIES_MANAGE]),
            state.clone(),
            test_request(),
            web::Path::from((property.id.to_string(), plan.id.to_string())),
            query(),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn delete_block_keeps_history_rows() {
        let state = web::Data::new(PropertiesState::default());
        let property = seed_property(&state, PropertyStatus::Published);
        let plan = seed_plan(&state, &property);
        let block = seed_block(&state, &property, &plan, BlockStatus::Available);
        seed_history(&state, &block);
        seed_history(&state, &block);
        let resp = delete_block(
            test_ctx(&[PERM_PROPERTIES_MANAGE]),
            state.clone(),
            test_request(),
            block_path(&property, &plan, &block),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(state.blocks.get(&block.id).is_none());
        assert_eq!(
            state
                .history
                .iter()
                .filter(|entry| entry.value().block_id == block.id)
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn correct_block_status_to_available_cancels_purchase() {
        let state = web::Data::new(PropertiesState::default());
        let property = seed_property(&state, PropertyStatus::Published);
        let plan = seed_plan(&state, &property);
        let mut block = seed_block(&state, &property, &plan, BlockStatus::Sold);
        block.order_id = Some("ord-7".to_string());
        state.blocks.insert(block.id, block.clone());
        let purchase = seed_purchase(&state, &block, "order_created", Some("ord-7"));
        let resp = correct_block_status(
            test_ctx(&[PERM_PROPERTIES_MANAGE]),
            state.clone(),
            test_request(),
            block_path(&property, &plan, &block),
            web::Json(CorrectStatusRequest {
                target_status: "available".to_string(),
                reason: "sale voided".to_string(),
                buyer_id: None,
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let stored_block = state.blocks.get(&block.id).unwrap();
        assert_eq!(stored_block.status, BlockStatus::Available);
        assert!(stored_block.assigned_customer_id.is_none());
        assert!(stored_block.order_id.is_none());
        assert_eq!(
            state.purchases.get(&purchase.id).unwrap().status,
            "cancelled"
        );
        // The admin-correction reason is recorded on the audit row.
        assert!(state.history.iter().any(|entry| {
            let entry = entry.value();
            entry.block_id == block.id
                && entry
                    .reason
                    .as_deref()
                    .map(|reason| reason.contains("admin correction"))
                    .unwrap_or(false)
        }));
    }
}
