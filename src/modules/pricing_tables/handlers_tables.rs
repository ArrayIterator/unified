//! pricing tables - handlers tables.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use super::types::*;
use super::state::*;
use super::validation::*;
use super::auth::*;

pub(crate) async fn list_tables(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let params = query.into_inner();
        let mut items: Vec<PricingTable> = state
            .tables
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|table| {
                matches_search(&format!("{} {} {}", table.name, table.slug, table.description), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| table.status == *status)
                    && params
                        .filters
                        .get("currency")
                        .is_none_or(|currency| table.currency == *currency)
            })
            .collect();
        let sort = params.allowed_sort(&["name", "slug", "currency", "status", "created_at"], "created_at");
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
            "currency" => items.sort_by(|a, b| a.currency.cmp(&b.currency)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    body: web::Json<CreatePricingTable>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTable> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let slug = match input.slug {
            Some(slug) if !slug.trim().is_empty() => slug.trim().to_lowercase(),
            _ => slugify(&name),
        };
        if !valid_slug(&slug) {
            return Err(UnifiedError::bad_request(
                "slug must be lowercase letters, digits, and hyphens",
            ));
        }
        if state
            .tables
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "A pricing table with slug \"{slug}\" already exists"
            )));
        }
        let currency = input.currency.unwrap_or_else(|| "USD".to_string());
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request(
                "currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        let layout = input.layout.unwrap_or_else(|| "grid".to_string());
        if !valid_layout(&layout) {
            return Err(UnifiedError::bad_request(
                "layout must be one of: grid, columns, comparison",
            ));
        }
        let theme_accent = input.theme_accent.unwrap_or_else(|| "#0B6B3A".to_string());
        if !valid_hex_color(&theme_accent) {
            return Err(UnifiedError::bad_request(
                "theme_accent must be a #rrggbb hex colour",
            ));
        }
        let yearly_discount_percent = input.yearly_discount_percent.unwrap_or(0.0);
        valid_percent(yearly_discount_percent, "yearly_discount_percent")?;
        let description = optional_text(input.description, MAX_TEXT_LEN)?;
        let table = PricingTable {
            id: new_id(),
            name,
            slug,
            description,
            currency,
            layout,
            theme_accent,
            status: "draft".to_string(),
            yearly_discount_percent,
            created_at: now(),
            updated_at: now(),
        };
        state.tables.insert(table.id.clone(), table.clone());
        let value = serde_json::to_value(&table).unwrap_or(Value::Null);
        state.emit("pricing_table.created", value);
        Ok(table)
    })();
    respond_created(result, debug)
}

pub(crate) async fn get_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTable> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        state
            .tables
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<UpdatePricingTable>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTable> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        if let Some(slug) = &input.slug {
            if !slug.trim().is_empty() && !valid_slug(&slug.trim().to_lowercase()) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and hyphens",
                ));
            }
            if !slug.trim().is_empty()
                && state.tables.iter().any(|entry| {
                    entry.key() != path.as_str() && entry.value().slug == slug.trim().to_lowercase()
                })
            {
                return Err(UnifiedError::conflict(format!(
                    "A pricing table with slug \"{}\" already exists",
                    slug.trim().to_lowercase()
                )));
            }
        }
        if let Some(currency) = &input.currency {
            if !valid_currency(currency) {
                return Err(UnifiedError::bad_request(
                    "currency must be a 3-letter uppercase ISO 4217 code",
                ));
            }
        }
        if let Some(layout) = &input.layout {
            if !valid_layout(layout) {
                return Err(UnifiedError::bad_request(
                    "layout must be one of: grid, columns, comparison",
                ));
            }
        }
        if let Some(theme_accent) = &input.theme_accent {
            if !valid_hex_color(theme_accent) {
                return Err(UnifiedError::bad_request(
                    "theme_accent must be a #rrggbb hex colour",
                ));
            }
        }
        if let Some(discount) = input.yearly_discount_percent {
            valid_percent(discount, "yearly_discount_percent")?;
        }
        if let Some(description) = &input.description {
            if description.len() > MAX_TEXT_LEN {
                return Err(UnifiedError::bad_request(
                    "description must be at most 2000 characters",
                ));
            }
        }
        let mut table = state
            .tables
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        if let Some(name) = input.name {
            table.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(slug) = input.slug {
            if !slug.trim().is_empty() {
                table.slug = slug.trim().to_lowercase();
            }
        }
        if let Some(description) = input.description {
            table.description = description;
        }
        if let Some(currency) = input.currency {
            table.currency = currency;
        }
        if let Some(layout) = input.layout {
            table.layout = layout;
        }
        if let Some(theme_accent) = input.theme_accent {
            table.theme_accent = theme_accent;
        }
        if let Some(discount) = input.yearly_discount_percent {
            table.yearly_discount_percent = discount;
        }
        table.updated_at = now();
        Ok(table.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let table_id = path.as_str().to_string();
        let table = state
            .tables
            .remove(&table_id)
            .map(|(_, table)| table)
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        // Tiers and their features belong to the table; they go with it.
        let tier_ids: Vec<String> = state
            .tiers
            .iter()
            .filter(|entry| entry.value().table_id == table_id)
            .map(|entry| entry.key().clone())
            .collect();
        for tier_id in &tier_ids {
            state.tiers.remove(tier_id);
            state
                .features
                .retain(|_, feature| feature.tier_id != *tier_id);
        }
        state.emit(
            "pricing_table.deleted",
            json!({ "table_id": table.id, "slug": table.slug }),
        );
        Ok(json!({ "deleted": true, "id": table_id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn publish_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTable> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let mut table = state
            .tables
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        if state.tiers_of(&table.id).is_empty() {
            return Err(UnifiedError::unprocessable(
                "Cannot publish a pricing table without at least one tier",
            ));
        }
        table.status = "published".to_string();
        table.updated_at = now();
        let updated = table.clone();
        drop(table);
        let value = serde_json::to_value(&updated).unwrap_or(Value::Null);
        state.emit("pricing_table.published", value);
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn unpublish_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTable> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let mut table = state
            .tables
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        table.status = "draft".to_string();
        table.updated_at = now();
        let updated = table.clone();
        drop(table);
        let value = serde_json::to_value(&updated).unwrap_or(Value::Null);
        state.emit("pricing_table.unpublished", value);
        Ok(updated)
    })();
    json::respond(result, debug)
}

/// Structured render data for a table. Public for published tables
/// (a page renders published content without an admin session);
/// draft tables answer 404 unless the caller holds
/// `pricing_tables:read`.
pub(crate) async fn render_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTableRender> = (|| {
        let table = state
            .tables
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        if table.status != "published" {
            // Drafts are admin-only; a missing permission and a
            // missing table both answer 404 so draft ids are not
            // enumerable through this public endpoint.
            let allowed = caller(&req)
                .map(|caller| check_capability(&ctx, &caller.role, PERM_PRICING_READ))
                .unwrap_or(false);
            if !allowed {
                return Err(UnifiedError::not_found("Pricing table not found"));
            }
        }
        Ok(state.render_data(&table))
    })();
    json::respond(result, debug)
}

/// Embed snippet data: the table id and the render URL a page uses
/// to fetch the structured data, plus a placeholder snippet the
/// embedder can drop into a page.
pub(crate) async fn embed_table(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let table = state
            .tables
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing table not found"))?;
        let render_url = format!("/api/v1/pricing-tables/tables/{}/render", table.id);
        Ok(json!({
            "table_id": table.id,
            "slug": table.slug,
            "status": table.status,
            "render_url": render_url,
            "snippet": format!(
                "<div data-pricing-table=\"{}\" data-render-url=\"{}\"></div>",
                table.id, render_url
            ),
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_tiers(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let table_id = path.as_str().to_string();
        if !state.tables.contains_key(table_id.as_str()) {
            return Err(UnifiedError::not_found("Pricing table not found"));
        }
        let params = query.into_inner();
        let items: Vec<PricingTier> = state
            .tiers_of(&table_id)
            .into_iter()
            .filter(|tier| {
                matches_search(&format!("{} {}", tier.name, tier.description), &params)
                    && params
                        .filters
                        .get("highlighted")
                        .is_none_or(|flag| tier.highlighted == (flag == "true"))
            })
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_tier(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<CreatePricingTier>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTier> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let table_id = path.as_str().to_string();
        if !state.tables.contains_key(table_id.as_str()) {
            return Err(UnifiedError::not_found("Pricing table not found"));
        }
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        valid_price(input.monthly_price, "monthly_price")?;
        let yearly_price = input.yearly_price.unwrap_or(input.monthly_price.saturating_mul(12));
        valid_price(yearly_price, "yearly_price")?;
        let cta_label = input
            .cta_label
            .map(|label| require_text(&label, "cta_label", MAX_NAME_LEN))
            .transpose()?
            .unwrap_or_else(|| "Get started".to_string());
        let cta_url = input.cta_url.unwrap_or_else(|| "/pricing".to_string());
        if !valid_cta_url(&cta_url) {
            return Err(UnifiedError::bad_request(
                "cta_url must be a relative path or an https:// URL",
            ));
        }
        let description = optional_text(input.description, MAX_TEXT_LEN)?;
        let existing = state.tiers_of(&table_id);
        // The first tier of a table is always highlighted; an explicit
        // highlight clears the flag on every other tier.
        let highlighted = if existing.is_empty() {
            true
        } else {
            input.highlighted.unwrap_or(false)
        };
        let sort_order = input.sort_order.unwrap_or_else(|| {
            existing
                .iter()
                .map(|tier| tier.sort_order)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0)
        });
        let tier = PricingTier {
            id: new_id(),
            table_id: table_id.clone(),
            name,
            description,
            monthly_price: input.monthly_price,
            yearly_price,
            cta_label,
            cta_url,
            highlighted,
            sort_order,
            created_at: now(),
            updated_at: now(),
        };
        state.tiers.insert(tier.id.clone(), tier.clone());
        if highlighted {
            state.clear_other_highlights(&table_id, &tier.id);
        }
        Ok(tier)
    })();
    respond_created(result, debug)
}

pub(crate) async fn get_tier(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTier> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let (table_id, tier_id) = path.into_inner();
        state
            .tiers
            .get(tier_id.as_str())
            .filter(|entry| entry.value().table_id == table_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_tier(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String)>,
    body: web::Json<UpdatePricingTier>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTier> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let (table_id, tier_id) = path.into_inner();
        let input = body.into_inner();
        if let Some(price) = input.monthly_price {
            valid_price(price, "monthly_price")?;
        }
        if let Some(price) = input.yearly_price {
            valid_price(price, "yearly_price")?;
        }
        if let Some(cta_url) = &input.cta_url {
            if !valid_cta_url(cta_url) {
                return Err(UnifiedError::bad_request(
                    "cta_url must be a relative path or an https:// URL",
                ));
            }
        }
        // Clearing the only highlighted tier would leave the table
        // with none: reject it instead of breaking the invariant.
        if input.highlighted == Some(false) {
            let is_highlighted = state
                .tiers
                .get(tier_id.as_str())
                .map(|entry| entry.value().highlighted)
                .unwrap_or(false);
            let others_highlighted = state
                .tiers_of(&table_id)
                .iter()
                .any(|tier| tier.id != tier_id && tier.highlighted);
            if is_highlighted && !others_highlighted {
                return Err(UnifiedError::unprocessable(
                    "Cannot clear the highlighted flag on the only highlighted tier; highlight another tier first",
                ));
            }
        }
        let mut tier = state
            .tiers
            .get_mut(tier_id.as_str())
            .filter(|entry| entry.table_id == table_id)
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        if let Some(name) = input.name {
            tier.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            if description.len() > MAX_TEXT_LEN {
                return Err(UnifiedError::bad_request(
                    "description must be at most 2000 characters",
                ));
            }
            tier.description = description;
        }
        if let Some(price) = input.monthly_price {
            tier.monthly_price = price;
        }
        if let Some(price) = input.yearly_price {
            tier.yearly_price = price;
        }
        if let Some(cta_label) = input.cta_label {
            tier.cta_label = require_text(&cta_label, "cta_label", MAX_NAME_LEN)?;
        }
        if let Some(cta_url) = input.cta_url {
            tier.cta_url = cta_url;
        }
        if let Some(highlighted) = input.highlighted {
            tier.highlighted = highlighted;
        }
        if let Some(sort_order) = input.sort_order {
            tier.sort_order = sort_order;
        }
        tier.updated_at = now();
        let updated = tier.clone();
        let highlight_now = updated.highlighted;
        drop(tier);
        if highlight_now {
            state.clear_other_highlights(&table_id, &tier_id);
        }
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_tier(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let (table_id, tier_id) = path.into_inner();
        let tier = state
            .tiers
            .get(tier_id.as_str())
            .filter(|entry| entry.value().table_id == table_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
        let was_highlighted = tier.highlighted;
        state.tiers.remove(tier_id.as_str());
        state
            .features
            .retain(|_, feature| feature.tier_id != tier_id);
        if was_highlighted {
            state.promote_next_highlight(&table_id);
        }
        Ok(json!({ "deleted": true, "id": tier_id }))
    })();
    json::respond(result, debug)
}

pub(crate) fn tier_belongs(
    state: &PricingTablesState,
    table_id: &str,
    tier_id: &str,
) -> bool {
    state
        .tiers
        .get(tier_id)
        .is_some_and(|entry| entry.value().table_id == table_id)
}

pub(crate) async fn list_features(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let (table_id, tier_id) = path.into_inner();
        if !tier_belongs(&state, &table_id, &tier_id) {
            return Err(UnifiedError::not_found("Pricing tier not found"));
        }
        let params = query.into_inner();
        let items: Vec<TierFeature> = state
            .features_of(&tier_id)
            .into_iter()
            .filter(|feature| {
                matches_search(&format!("{} {}", feature.feature_text, feature.note), &params)
                    && params
                        .filters
                        .get("included")
                        .is_none_or(|flag| feature.included == (flag == "true"))
            })
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_feature(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String)>,
    body: web::Json<CreateTierFeature>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TierFeature> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let (table_id, tier_id) = path.into_inner();
        if !tier_belongs(&state, &table_id, &tier_id) {
            return Err(UnifiedError::not_found("Pricing tier not found"));
        }
        let input = body.into_inner();
        let feature_text = require_text(&input.feature_text, "feature_text", MAX_NAME_LEN)?;
        let note = optional_text(input.note, MAX_TEXT_LEN)?;
        let sort_order = input.sort_order.unwrap_or_else(|| {
            state
                .features_of(&tier_id)
                .iter()
                .map(|feature| feature.sort_order)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0)
        });
        let feature = TierFeature {
            id: new_id(),
            tier_id,
            feature_text,
            included: input.included.unwrap_or(true),
            note,
            sort_order,
            created_at: now(),
            updated_at: now(),
        };
        state.features.insert(feature.id.clone(), feature.clone());
        Ok(feature)
    })();
    respond_created(result, debug)
}

pub(crate) async fn get_feature(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TierFeature> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_READ)?;
        let (table_id, tier_id, feature_id) = path.into_inner();
        if !tier_belongs(&state, &table_id, &tier_id) {
            return Err(UnifiedError::not_found("Pricing tier not found"));
        }
        state
            .features
            .get(feature_id.as_str())
            .filter(|entry| entry.value().tier_id == tier_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tier feature not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_feature(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String, String)>,
    body: web::Json<UpdateTierFeature>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TierFeature> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let (table_id, tier_id, feature_id) = path.into_inner();
        if !tier_belongs(&state, &table_id, &tier_id) {
            return Err(UnifiedError::not_found("Pricing tier not found"));
        }
        let input = body.into_inner();
        let mut feature = state
            .features
            .get_mut(feature_id.as_str())
            .filter(|entry| entry.tier_id == tier_id)
            .ok_or_else(|| UnifiedError::not_found("Tier feature not found"))?;
        if let Some(feature_text) = input.feature_text {
            feature.feature_text = require_text(&feature_text, "feature_text", MAX_NAME_LEN)?;
        }
        if let Some(included) = input.included {
            feature.included = included;
        }
        if let Some(note) = input.note {
            if note.len() > MAX_TEXT_LEN {
                return Err(UnifiedError::bad_request(
                    "note must be at most 2000 characters",
                ));
            }
            feature.note = note;
        }
        if let Some(sort_order) = input.sort_order {
            feature.sort_order = sort_order;
        }
        feature.updated_at = now();
        Ok(feature.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_feature(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<(String, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRICING_MANAGE)?;
        let (table_id, tier_id, feature_id) = path.into_inner();
        if !tier_belongs(&state, &table_id, &tier_id) {
            return Err(UnifiedError::not_found("Pricing tier not found"));
        }
        // Check the tier match before removing, so a feature of
        // another tier is never deleted by a mismatched path.
        let belongs = state
            .features
            .get(feature_id.as_str())
            .is_some_and(|entry| entry.value().tier_id == tier_id);
        if !belongs {
            return Err(UnifiedError::not_found("Tier feature not found"));
        }
        state.features.remove(feature_id.as_str());
        Ok(json!({ "deleted": true, "id": feature_id }))
    })();
    json::respond(result, debug)
}
