// Pos module — request bodies.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;
use super::*;

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateRegisterRequest {
    pub name: String,
    pub code: Option<String>,
    pub location: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRegisterRequest {
    pub name: Option<String>,
    pub location: Option<Option<String>>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssignCashierRequest {
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub struct OpenShiftRequest {
    pub register_id: String,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub opening_float: Option<i64>,
    pub currency: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CloseShiftRequest {
    #[serde(with = "unified_core::money::serde_major")]
    pub counted_cash: i64,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewSaleItemRequest {
    pub product_id: String,
    pub name: String,
    pub barcode: Option<String>,
    pub sku: Option<String>,
    #[serde(with = "unified_core::money::serde_major")]
    pub unit_price: i64,
    pub quantity: Option<u32>,
    pub discount_type: Option<String>,
    pub discount_value: Option<f64>,
    pub tax_rate: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSaleRequest {
    pub register_id: String,
    pub shift_id: Option<String>,
    pub company_id: Option<String>,
    pub currency: Option<String>,
    pub customer_name: Option<String>,
    pub notes: Option<String>,
    pub discount_type: Option<String>,
    pub discount_value: Option<f64>,
    pub items: Option<Vec<NewSaleItemRequest>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSaleRequest {
    pub discount_type: Option<Option<String>>,
    pub discount_value: Option<f64>,
    pub customer_name: Option<Option<String>>,
    pub notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSaleItemRequest {
    pub quantity: Option<u32>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub unit_price: Option<i64>,
    pub discount_type: Option<Option<String>>,
    pub discount_value: Option<f64>,
    pub tax_rate: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct CompletePaymentRequest {
    pub method_id: String,
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub tendered: Option<i64>,
    pub reference: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CompleteSaleRequest {
    pub payments: Vec<CompletePaymentRequest>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RefundItemRequest {
    pub item_id: String,
    pub quantity: u32,
}

#[derive(Debug, Deserialize)]
pub struct RefundSaleRequest {
    pub items: Option<Vec<RefundItemRequest>>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub amount: Option<i64>,
    pub method_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePaymentMethodRequest {
    pub code: String,
    pub label: String,
    pub kind: Option<String>,
    pub is_cash: Option<bool>,
    pub requires_reference: Option<bool>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePaymentMethodRequest {
    pub label: Option<String>,
    pub kind: Option<String>,
    pub is_cash: Option<bool>,
    pub requires_reference: Option<bool>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub store_name: Option<String>,
    pub store_address: Option<Option<String>>,
    pub receipt_footer: Option<Option<String>>,
    pub receipt_width_chars: Option<u32>,
    pub default_currency: Option<String>,
    pub country_code: Option<Option<String>>,
    pub default_tax_rate: Option<f64>,
    pub tax_inclusive: Option<bool>,
    pub max_discount_percent: Option<f64>,
    pub require_cashier_assignment: Option<bool>,
    pub allow_split_payment: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ProductSearchQuery {
    pub q: Option<String>,
    pub barcode: Option<String>,
    pub sku: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ReportQuery {
    pub date: Option<String>,
    pub register_id: Option<String>,
}
