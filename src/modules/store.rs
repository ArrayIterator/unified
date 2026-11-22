// Cart / Store / Marketplace module.
//
// Products, a per-session cart with server-side totals, checkout that
// creates an order and emits `order.created` (licensing and billing listen
// to it), payment initiation, and digital delivery references.
//
// Interconnection happens only through the hook bus: this module never
// calls another module directly.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    pub id: String,
    pub name: String,
    pub description: String,
    pub price: f64,
    pub currency: String,
    /// Product kind: `theme`, `plugin`, or `software`.
    pub kind: String,
    pub license_product: bool,
    pub file_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProduct {
    pub name: String,
    pub description: Option<String>,
    pub price: f64,
    pub currency: Option<String>,
    pub kind: String,
    pub license_product: Option<bool>,
    pub file_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProduct {
    pub name: Option<String>,
    pub description: Option<String>,
    pub price: Option<f64>,
    pub currency: Option<String>,
    pub kind: Option<String>,
    pub license_product: Option<bool>,
    pub file_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartItem {
    pub product_id: String,
    pub name: String,
    pub unit_price: f64,
    pub quantity: u32,
    pub line_total: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cart {
    pub owner: String,
    pub items: Vec<CartItem>,
    pub currency: String,
    pub total: f64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddCartItem {
    pub product_id: String,
    pub quantity: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoveCartItem {
    pub product_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderItem {
    pub product_id: String,
    pub name: String,
    pub unit_price: f64,
    pub quantity: u32,
    pub line_total: f64,
    pub license_product: bool,
    pub file_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub user_id: String,
    pub items: Vec<OrderItem>,
    pub total: f64,
    pub currency: String,
    /// `pending`, `payment_pending`, `paid`, `cancelled`.
    pub status: String,
    pub provider: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckoutItem {
    pub product_id: String,
    pub quantity: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckoutRequest {
    pub user_id: Option<String>,
    pub items: Option<Vec<CheckoutItem>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PayRequest {
    pub provider: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateOrder {
    pub status: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct StoreState {
    hooks: RwLock<Arc<HookBus>>,
    products: DashMap<String, Product>,
    carts: DashMap<String, Cart>,
    orders: DashMap<String, Order>,
}

impl StoreState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            products: DashMap::new(),
            carts: DashMap::new(),
            orders: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

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

fn valid_kind(kind: &str) -> bool {
    matches!(kind, "theme" | "plugin" | "software")
}

fn valid_currency(currency: &str) -> bool {
    currency.len() == 3 && currency.chars().all(|c| c.is_ascii_alphabetic())
}

/// Cart owner: `x-session-id` header, then `x-user-id`, then `guest`.
fn owner_of(req: &HttpRequest) -> String {
    for header in ["x-session-id", "x-user-id"] {
        if let Some(value) = req
            .headers()
            .get(header)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.trim().is_empty())
        {
            return value.trim().to_string();
        }
    }
    "guest".to_string()
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// Product handlers
// ---------------------------------------------------------------------------

async fn list_products(
    state: web::Data<Arc<StoreState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["name", "price", "created_at"], "created_at");
    let mut items: Vec<Product> = state
        .products
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|product| {
            matches_search(
                &format!("{} {}", product.name, product.description),
                &params,
            ) && params
                .filters
                .get("kind")
                .is_none_or(|kind| product.kind == *kind)
                && params
                    .filters
                    .get("currency")
                    .is_none_or(|currency| product.currency.eq_ignore_ascii_case(currency))
                && params.filters.get("license_product").is_none_or(|flag| {
                    product.license_product == (*flag == "true" || *flag == "1")
                })
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "price" => items.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_product(
    state: web::Data<Arc<StoreState>>,
    body: web::Json<CreateProduct>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Product name must not be empty"));
        }
        if !input.price.is_finite() || input.price < 0.0 {
            return Err(UnifiedError::bad_request(
                "Product price must be a non-negative number",
            ));
        }
        if !valid_kind(&input.kind) {
            return Err(UnifiedError::bad_request(
                "Product kind must be one of: theme, plugin, software",
            ));
        }
        let currency = input.currency.unwrap_or_else(|| "USD".to_string());
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request(
                "Currency must be a 3-letter code such as USD",
            ));
        }
        let product = Product {
            id: new_id(),
            name: input.name.trim().to_string(),
            description: input.description.unwrap_or_default(),
            price: round_money(input.price),
            currency: currency.to_uppercase(),
            kind: input.kind,
            license_product: input.license_product.unwrap_or(false),
            file_id: input.file_id,
            created_at: now(),
            updated_at: now(),
        };
        state.products.insert(product.id.clone(), product.clone());
        Ok(product)
    })();
    respond_created(result)
}

async fn get_product(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = state
        .products
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Product not found"));
    json::respond(result, false)
}

async fn update_product(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    body: web::Json<UpdateProduct>,
) -> HttpResponse {
    let result: UnifiedResult<Product> = (|| {
        let input = body.into_inner();
        let mut product = state
            .products
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Product name must not be empty"));
            }
            product.name = name.trim().to_string();
        }
        if let Some(description) = input.description {
            product.description = description;
        }
        if let Some(price) = input.price {
            if !price.is_finite() || price < 0.0 {
                return Err(UnifiedError::bad_request(
                    "Product price must be a non-negative number",
                ));
            }
            product.price = round_money(price);
        }
        if let Some(currency) = input.currency {
            if !valid_currency(&currency) {
                return Err(UnifiedError::bad_request(
                    "Currency must be a 3-letter code such as USD",
                ));
            }
            product.currency = currency.to_uppercase();
        }
        if let Some(kind) = input.kind {
            if !valid_kind(&kind) {
                return Err(UnifiedError::bad_request(
                    "Product kind must be one of: theme, plugin, software",
                ));
            }
            product.kind = kind;
        }
        if let Some(license_product) = input.license_product {
            product.license_product = license_product;
        }
        if let Some(file_id) = input.file_id {
            product.file_id = Some(file_id);
        }
        product.updated_at = now();
        Ok(product.clone())
    })();
    json::respond(result, false)
}

async fn delete_product(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .products
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Product not found"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Cart handlers
// ---------------------------------------------------------------------------

fn empty_cart(owner: &str) -> Cart {
    Cart {
        owner: owner.to_string(),
        items: Vec::new(),
        currency: "USD".to_string(),
        total: 0.0,
        updated_at: now(),
    }
}

fn recalculate_cart(cart: &mut Cart) {
    cart.total = round_money(cart.items.iter().map(|item| item.line_total).sum());
    cart.updated_at = now();
}

async fn get_cart(state: web::Data<Arc<StoreState>>, req: HttpRequest) -> HttpResponse {
    let owner = owner_of(&req);
    let cart = state
        .carts
        .get(&owner)
        .map(|entry| entry.value().clone())
        .unwrap_or_else(|| empty_cart(&owner));
    json::success_response(cart, 200)
}

async fn add_to_cart(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<AddCartItem>,
) -> HttpResponse {
    let result: UnifiedResult<Cart> = (|| {
        let input = body.into_inner();
        let quantity = input.quantity.unwrap_or(1);
        if quantity == 0 {
            return Err(UnifiedError::bad_request("Quantity must be at least 1"));
        }
        let product = state
            .products
            .get(&input.product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        let owner = owner_of(&req);
        let mut cart = state
            .carts
            .get(&owner)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| empty_cart(&owner));
        if !cart.items.is_empty() && cart.currency != product.currency {
            return Err(UnifiedError::unprocessable(
                "All cart items must use the same currency",
            ));
        }
        cart.currency = product.currency.clone();
        match cart
            .items
            .iter_mut()
            .find(|item| item.product_id == product.id)
        {
            Some(item) => {
                item.quantity += quantity;
                item.unit_price = product.price;
                item.line_total = round_money(product.price * item.quantity as f64);
            }
            None => cart.items.push(CartItem {
                product_id: product.id.clone(),
                name: product.name.clone(),
                unit_price: product.price,
                quantity,
                line_total: round_money(product.price * quantity as f64),
            }),
        }
        recalculate_cart(&mut cart);
        state.carts.insert(owner, cart.clone());
        Ok(cart)
    })();
    json::respond(result, false)
}

async fn remove_from_cart(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: Option<web::Json<RemoveCartItem>>,
) -> HttpResponse {
    let result: UnifiedResult<Cart> = (|| {
        let owner = owner_of(&req);
        let mut cart = state
            .carts
            .get(&owner)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| empty_cart(&owner));
        match body.and_then(|b| b.into_inner().product_id) {
            Some(product_id) => {
                let before = cart.items.len();
                cart.items.retain(|item| item.product_id != product_id);
                if cart.items.len() == before {
                    return Err(UnifiedError::not_found("Product is not in the cart"));
                }
            }
            None => cart.items.clear(),
        }
        if cart.items.is_empty() {
            cart.currency = "USD".to_string();
        }
        recalculate_cart(&mut cart);
        state.carts.insert(owner, cart.clone());
        Ok(cart)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Order handlers
// ---------------------------------------------------------------------------

async fn checkout(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CheckoutRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = (|| {
        let input = body.into_inner();
        let owner = input
            .user_id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| owner_of(&req));
        // Resolve the items: an explicit list wins, otherwise the stored cart.
        let requested: Vec<CheckoutItem> = match input.items {
            Some(items) => items,
            None => state
                .carts
                .get(&owner)
                .map(|entry| {
                    entry
                        .value()
                        .items
                        .iter()
                        .map(|item| CheckoutItem {
                            product_id: item.product_id.clone(),
                            quantity: Some(item.quantity),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        if requested.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Cannot check out an empty cart",
            ));
        }
        let mut items: Vec<OrderItem> = Vec::new();
        let mut currency: Option<String> = None;
        let mut total = 0.0;
        for requested_item in requested {
            let quantity = requested_item.quantity.unwrap_or(1);
            if quantity == 0 {
                return Err(UnifiedError::bad_request("Quantity must be at least 1"));
            }
            let product = state
                .products
                .get(&requested_item.product_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "Product {} not found",
                        requested_item.product_id
                    ))
                })?;
            match &currency {
                Some(existing) if *existing != product.currency => {
                    return Err(UnifiedError::unprocessable(
                        "All order items must use the same currency",
                    ));
                }
                None => currency = Some(product.currency.clone()),
                _ => {}
            }
            let line_total = round_money(product.price * quantity as f64);
            total += line_total;
            items.push(OrderItem {
                product_id: product.id.clone(),
                name: product.name.clone(),
                unit_price: product.price,
                quantity,
                line_total,
                license_product: product.license_product,
                file_id: product.file_id.clone(),
            });
        }
        let order = Order {
            id: new_id(),
            user_id: owner.clone(),
            items,
            total: round_money(total),
            currency: currency.unwrap_or_else(|| "USD".to_string()),
            status: "pending".to_string(),
            provider: None,
            created_at: now(),
            updated_at: now(),
        };
        state.orders.insert(order.id.clone(), order.clone());
        // The cart is consumed by the checkout.
        state.carts.remove(&owner);
        state.emit(
            "order.created",
            serde_json::to_value(&order).unwrap_or(Value::Null),
        );
        Ok(order)
    })();
    respond_created(result)
}

async fn list_orders(
    state: web::Data<Arc<StoreState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "total"], "created_at");
    let mut items: Vec<Order> = state
        .orders
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|order| {
            matches_search(&format!("{} {}", order.id, order.user_id), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| order.status == *status)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| order.user_id == *user_id)
        })
        .collect();
    match sort.as_str() {
        "total" => items.sort_by(|a, b| a.total.partial_cmp(&b.total).unwrap_or(std::cmp::Ordering::Equal)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_order(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = state
        .orders
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Order not found"));
    json::respond(result, false)
}

async fn update_order(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    body: web::Json<UpdateOrder>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = (|| {
        let input = body.into_inner();
        let mut order = state
            .orders
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        if let Some(status) = input.status {
            let allowed: &[&str] = match order.status.as_str() {
                "pending" | "payment_pending" => &["cancelled"],
                _ => &[],
            };
            if !allowed.contains(&status.as_str()) {
                return Err(UnifiedError::unprocessable(format!(
                    "Cannot move order from {} to {}",
                    order.status, status
                )));
            }
            order.status = status;
        }
        order.updated_at = now();
        Ok(order.clone())
    })();
    json::respond(result, false)
}

async fn delete_order(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let order = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        if order.status != "pending" && order.status != "cancelled" {
            return Err(UnifiedError::unprocessable(
                "Only pending or cancelled orders can be deleted",
            ));
        }
        state.orders.remove(path.as_str());
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

async fn pay_order(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
    body: web::Json<PayRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let provider = body.into_inner().provider.trim().to_string();
        if provider.is_empty() {
            return Err(UnifiedError::bad_request("Payment provider must not be empty"));
        }
        let mut order = state
            .orders
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        if order.status == "paid" {
            return Err(UnifiedError::conflict("Order is already paid"));
        }
        if order.status == "cancelled" {
            return Err(UnifiedError::unprocessable("Cancelled orders cannot be paid"));
        }
        order.provider = Some(provider.clone());
        if provider == "manual" {
            // The manual provider completes immediately; no gateway call.
            order.status = "paid".to_string();
            order.updated_at = now();
            let order_value = serde_json::to_value(&*order).unwrap_or(Value::Null);
            drop(order);
            state.emit("payment.completed", order_value.clone());
            state.emit("order.paid", order_value.clone());
            let paid = state
                .orders
                .get(path.as_str())
                .map(|entry| entry.value().clone())
                .expect("order still present");
            return Ok(json!({ "order": paid, "payment_status": "completed", "provider": provider }));
        }
        // Any other provider is handed off through hooks: a real gateway
        // module listens to `payment.initiated` and completes the payment
        // asynchronously.
        // TODO: integrate real payment gateways (provider registry with
        // signed callbacks) as a next milestone; only "manual" completes
        // synchronously today.
        order.status = "payment_pending".to_string();
        order.updated_at = now();
        let order_value = serde_json::to_value(&*order).unwrap_or(Value::Null);
        drop(order);
        state.emit(
            "payment.initiated",
            json!({
                "order": order_value,
                "provider": provider,
                "amount": state.orders.get(path.as_str()).map(|o| o.total).unwrap_or(0.0),
            }),
        );
        let pending = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("order still present");
        Ok(json!({ "order": pending, "payment_status": "pending", "provider": provider }))
    })();
    json::respond(result, false)
}

async fn download_order(
    state: web::Data<Arc<StoreState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let order = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        if order.status != "paid" {
            return Err(UnifiedError::forbidden(
                "Digital delivery is only available for paid orders",
            ));
        }
        let file_id = order
            .items
            .iter()
            .find_map(|item| item.file_id.clone())
            .ok_or_else(|| {
                UnifiedError::unprocessable("This order has no downloadable file attached")
            })?;
        let job_id = new_id();
        state.emit(
            events::DOWNLOAD_STARTED,
            json!({
                "order_id": order.id,
                "user_id": order.user_id,
                "file_id": file_id,
                "job_id": job_id,
            }),
        );
        Ok(json!({
            "download": {
                "job_id": job_id,
                "order_id": order.id,
                "file_id": file_id,
                "status": "ready",
                "expires_at": (now() + chrono::Duration::hours(24)).to_rfc3339(),
            }
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct StoreModule {
    state: Arc<StoreState>,
}

impl StoreModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(StoreState::new()),
        }
    }
}

impl Default for StoreModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for StoreModule {
    fn id(&self) -> &'static str {
        "store"
    }

    fn name(&self) -> &'static str {
        "Store"
    }

    fn description(&self) -> &'static str {
        "Cart, store and marketplace: products, checkout, payments and digital delivery."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["licensing", "billing"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/store")
                .route("/products", web::get().to(list_products))
                .route("/products", web::post().to(create_product))
                .route("/products/{id}", web::get().to(get_product))
                .route("/products/{id}", web::put().to(update_product))
                .route("/products/{id}", web::delete().to(delete_product))
                .route("/cart", web::get().to(get_cart))
                .route("/cart", web::post().to(add_to_cart))
                .route("/cart", web::delete().to(remove_from_cart))
                .route("/checkout", web::post().to(checkout))
                .route("/orders", web::get().to(list_orders))
                .route("/orders/{id}", web::get().to(get_order))
                .route("/orders/{id}", web::put().to(update_order))
                .route("/orders/{id}", web::delete().to(delete_order))
                .route("/orders/{id}/pay", web::post().to(pay_order))
                .route("/orders/{id}/download", web::get().to(download_order)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(StoreModule::new()));
}
