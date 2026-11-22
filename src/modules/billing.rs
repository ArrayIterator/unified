// Billing / Invoicing module.
//
// Invoices built from orders (directly or automatically from the
// `order.created` hook), payment and voiding, structured rendering, and
// overdue detection. Interconnection happens only through the hook bus.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
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
pub struct InvoiceItem {
    pub name: String,
    pub quantity: u32,
    pub unit_price: f64,
    pub line_total: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invoice {
    pub id: String,
    pub order_id: Option<String>,
    pub user_id: String,
    pub company_id: Option<String>,
    pub items: Vec<InvoiceItem>,
    pub total: f64,
    pub currency: String,
    /// `draft`, `issued`, `paid`, `void`.
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateInvoice {
    pub order_id: Option<String>,
    pub user_id: String,
    pub company_id: Option<String>,
    pub items: Vec<InvoiceItemInput>,
    pub total: Option<f64>,
    pub currency: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InvoiceItemInput {
    pub name: String,
    pub quantity: Option<u32>,
    pub unit_price: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateInvoice {
    pub status: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FromOrderRequest {
    pub order_id: String,
    pub user_id: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PayInvoiceRequest {
    pub method: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct BillingState {
    hooks: RwLock<Arc<HookBus>>,
    invoices: DashMap<String, Invoice>,
    /// Orders seen through the `order.created` hook, keyed by order id,
    /// so `from-order` can build an invoice from real order data.
    seen_orders: DashMap<String, Value>,
}

impl BillingState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            invoices: DashMap::new(),
            seen_orders: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Build a draft invoice from an order payload (as emitted by the
    /// store on `order.created`). Returns `None` when the payload lacks
    /// the fields an invoice needs.
    fn invoice_from_order_value(&self, order: &Value) -> Option<Invoice> {
        let order_id = order.get("id")?.as_str()?.to_string();
        let user_id = order.get("user_id")?.as_str()?.to_string();
        let total = order.get("total")?.as_f64()?;
        let currency = order
            .get("currency")
            .and_then(|v| v.as_str())
            .unwrap_or("USD")
            .to_string();
        let mut items: Vec<InvoiceItem> = Vec::new();
        if let Some(raw_items) = order.get("items").and_then(|v| v.as_array()) {
            for raw in raw_items {
                let name = raw
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Item")
                    .to_string();
                let quantity = raw
                    .get("quantity")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1) as u32;
                let unit_price = raw.get("unit_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let line_total = raw
                    .get("line_total")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(unit_price * quantity as f64);
                items.push(InvoiceItem {
                    name,
                    quantity,
                    unit_price,
                    line_total,
                });
            }
        }
        if items.is_empty() {
            items.push(InvoiceItem {
                name: "Order".to_string(),
                quantity: 1,
                unit_price: total,
                line_total: total,
            });
        }
        Some(Invoice {
            id: new_id(),
            order_id: Some(order_id),
            user_id,
            company_id: order
                .get("company_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            items,
            total,
            currency,
            status: "draft".to_string(),
            due_date: None,
            paid_at: None,
            created_at: now(),
            updated_at: now(),
        })
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

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn render_invoice_html(invoice: &Invoice) -> String {
    let mut rows = String::new();
    for item in &invoice.items {
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{:.2}</td><td>{:.2}</td></tr>",
            escape_html(&item.name),
            item.quantity,
            item.unit_price,
            item.line_total
        ));
    }
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Invoice {}</title></head>\
        <body><h1>Invoice {}</h1>\
        <p>Status: {}<br>User: {}<br>Currency: {}<br>Total: {:.2}</p>\
        <table><thead><tr><th>Item</th><th>Quantity</th><th>Unit price</th><th>Total</th></tr></thead>\
        <tbody>{}</tbody></table></body></html>",
        escape_html(&invoice.id),
        escape_html(&invoice.id),
        escape_html(&invoice.status),
        escape_html(&invoice.user_id),
        escape_html(&invoice.currency),
        invoice.total,
        rows
    )
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_invoices(
    state: web::Data<Arc<BillingState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "total", "due_date"], "created_at");
    let mut items: Vec<Invoice> = state
        .invoices
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|invoice| {
            matches_search(&format!("{} {}", invoice.id, invoice.user_id), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| invoice.status == *status)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| invoice.user_id == *user_id)
                && params.filters.get("order_id").is_none_or(|order_id| {
                    invoice.order_id.as_deref() == Some(order_id.as_str())
                })
                && params.filters.get("company_id").is_none_or(|company_id| {
                    invoice.company_id.as_deref() == Some(company_id.as_str())
                })
        })
        .collect();
    match sort.as_str() {
        "total" => items.sort_by(|a, b| {
            a.total
                .partial_cmp(&b.total)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_invoice(
    state: web::Data<Arc<BillingState>>,
    body: web::Json<CreateInvoice>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let input = body.into_inner();
        if input.user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        if input.items.is_empty() {
            return Err(UnifiedError::bad_request(
                "An invoice needs at least one item",
            ));
        }
        let mut items: Vec<InvoiceItem> = Vec::new();
        let mut computed_total = 0.0;
        for item in input.items {
            if item.name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Item name must not be empty"));
            }
            if !item.unit_price.is_finite() || item.unit_price < 0.0 {
                return Err(UnifiedError::bad_request(
                    "Item unit price must be a non-negative number",
                ));
            }
            let quantity = item.quantity.unwrap_or(1);
            if quantity == 0 {
                return Err(UnifiedError::bad_request("Item quantity must be at least 1"));
            }
            let line_total = round_money(item.unit_price * quantity as f64);
            computed_total += line_total;
            items.push(InvoiceItem {
                name: item.name.trim().to_string(),
                quantity,
                unit_price: round_money(item.unit_price),
                line_total,
            });
        }
        let total = round_money(input.total.unwrap_or(computed_total));
        if total < 0.0 || !total.is_finite() {
            return Err(UnifiedError::bad_request("Invoice total must be non-negative"));
        }
        let currency = input.currency.unwrap_or_else(|| "USD".to_string());
        if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(UnifiedError::bad_request(
                "Currency must be a 3-letter code such as USD",
            ));
        }
        let invoice = Invoice {
            id: new_id(),
            order_id: input.order_id,
            user_id: input.user_id.trim().to_string(),
            company_id: input.company_id,
            items,
            total,
            currency: currency.to_uppercase(),
            status: "draft".to_string(),
            due_date: input.due_date,
            paid_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.invoices.insert(invoice.id.clone(), invoice.clone());
        state.emit(
            "invoice.created",
            serde_json::to_value(&invoice).unwrap_or(Value::Null),
        );
        Ok(invoice)
    })();
    respond_created(result)
}

async fn from_order(
    state: web::Data<Arc<BillingState>>,
    body: web::Json<FromOrderRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let input = body.into_inner();
        // Reuse an existing invoice for this order when one already exists
        // (for example the draft created automatically by the hook).
        if let Some(existing) = state
            .invoices
            .iter()
            .find(|entry| entry.value().order_id.as_deref() == Some(input.order_id.as_str()))
            .map(|entry| entry.value().clone())
        {
            return Ok(existing);
        }
        let order = state
            .seen_orders
            .get(&input.order_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(
                    "Order not found: billing has not seen this order yet. \
                    Place the order through the store first, or create the \
                    invoice directly with items.",
                )
            })?;
        let mut invoice = state
            .invoice_from_order_value(&order)
            .ok_or_else(|| UnifiedError::unprocessable("Stored order data is incomplete"))?;
        if let Some(user_id) = input.user_id.filter(|id| !id.trim().is_empty()) {
            invoice.user_id = user_id;
        }
        if let Some(due_date) = input.due_date {
            invoice.due_date = Some(due_date);
        }
        state.invoices.insert(invoice.id.clone(), invoice.clone());
        state.emit(
            "invoice.created",
            serde_json::to_value(&invoice).unwrap_or(Value::Null),
        );
        Ok(invoice)
    })();
    respond_created(result)
}

async fn get_invoice(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = state
        .invoices
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Invoice not found"));
    json::respond(result, false)
}

async fn update_invoice(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
    body: web::Json<UpdateInvoice>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let input = body.into_inner();
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        if invoice.status == "paid" || invoice.status == "void" {
            return Err(UnifiedError::unprocessable(format!(
                "A {} invoice can no longer be edited",
                invoice.status
            )));
        }
        if let Some(status) = input.status {
            if !matches!(status.as_str(), "draft" | "issued") {
                return Err(UnifiedError::unprocessable(
                    "Status can only be changed to draft or issued here; use pay or void for the rest",
                ));
            }
            invoice.status = status;
        }
        if let Some(due_date) = input.due_date {
            invoice.due_date = Some(due_date);
        }
        invoice.updated_at = now();
        Ok(invoice.clone())
    })();
    json::respond(result, false)
}

async fn delete_invoice(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        if invoice.status != "draft" {
            return Err(UnifiedError::unprocessable(
                "Only draft invoices can be deleted; void the invoice instead",
            ));
        }
        state.invoices.remove(path.as_str());
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

async fn pay_invoice(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
    body: web::Json<PayInvoiceRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let method = body
            .into_inner()
            .method
            .unwrap_or_else(|| "manual".to_string());
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        if invoice.status == "paid" {
            return Err(UnifiedError::conflict("Invoice is already paid"));
        }
        if invoice.status == "void" {
            return Err(UnifiedError::unprocessable("A void invoice cannot be paid"));
        }
        invoice.status = "paid".to_string();
        invoice.paid_at = Some(now());
        invoice.updated_at = now();
        let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
        drop(invoice);
        state.emit(
            "invoice.paid",
            json!({ "invoice": invoice_value, "method": method }),
        );
        Ok(state
            .invoices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("invoice still present"))
    })();
    json::respond(result, false)
}

async fn void_invoice(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        if invoice.status == "paid" {
            return Err(UnifiedError::unprocessable("A paid invoice cannot be voided"));
        }
        if invoice.status == "void" {
            return Err(UnifiedError::conflict("Invoice is already void"));
        }
        invoice.status = "void".to_string();
        invoice.updated_at = now();
        let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
        drop(invoice);
        state.emit("invoice.voided", invoice_value);
        Ok(state
            .invoices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("invoice still present"))
    })();
    json::respond(result, false)
}

async fn render_invoice(
    state: web::Data<Arc<BillingState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        let html = render_invoice_html(&invoice);
        Ok(json!({ "invoice": invoice, "html": html }))
    })();
    json::respond(result, false)
}

async fn list_overdue(
    state: web::Data<Arc<BillingState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let now_value = now();
    let mut items: Vec<Invoice> = state
        .invoices
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|invoice| {
            (invoice.status == "draft" || invoice.status == "issued")
                && invoice
                    .due_date
                    .is_some_and(|due| due < now_value)
                && matches_search(&format!("{} {}", invoice.id, invoice.user_id), &params)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| invoice.user_id == *user_id)
        })
        .collect();
    items.sort_by(|a, b| a.due_date.cmp(&b.due_date));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

fn on_order_created(state: Arc<BillingState>, ctx: &mut HookContext) {
    let order = ctx.data.clone();
    // Remember the order so `from-order` can build from it later.
    if let Some(order_id) = order.get("id").and_then(|v| v.as_str()) {
        state
            .seen_orders
            .insert(order_id.to_string(), order.clone());
    }
    // Auto-create a draft invoice, unless one already exists for the order.
    let Some(invoice) = state.invoice_from_order_value(&order) else {
        return;
    };
    let order_id = invoice.order_id.clone();
    let already_exists = state.invoices.iter().any(|entry| {
        entry.value().order_id == order_id && entry.value().status != "void"
    });
    if already_exists {
        return;
    }
    state.invoices.insert(invoice.id.clone(), invoice.clone());
    state.emit(
        "invoice.created",
        serde_json::to_value(&invoice).unwrap_or(Value::Null),
    );
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct BillingModule {
    state: Arc<BillingState>,
}

impl BillingModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(BillingState::new()),
        }
    }
}

impl Default for BillingModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for BillingModule {
    fn id(&self) -> &'static str {
        "billing"
    }

    fn name(&self) -> &'static str {
        "Billing"
    }

    fn description(&self) -> &'static str {
        "Billing and invoicing: invoices from orders, payment, voiding and overdue detection."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["store"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: crate::core::hooks::HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_order_created(state.clone(), ctx));
        bus.register("order.created", "billing", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/billing")
                .route("/invoices", web::get().to(list_invoices))
                .route("/invoices", web::post().to(create_invoice))
                .route("/invoices/from-order", web::post().to(from_order))
                .route("/invoices/{id}", web::get().to(get_invoice))
                .route("/invoices/{id}", web::put().to(update_invoice))
                .route("/invoices/{id}", web::delete().to(delete_invoice))
                .route("/invoices/{id}/pay", web::post().to(pay_invoice))
                .route("/invoices/{id}/void", web::post().to(void_invoice))
                .route("/invoices/{id}/render", web::get().to(render_invoice))
                .route("/overdue", web::get().to(list_overdue)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(BillingModule::new()));
}
