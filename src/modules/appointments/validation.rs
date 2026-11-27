// appointments - validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::scheduling::*;
use super::reminders::*;
use super::ics::*;
use super::handlers_services::*;
use super::handlers_staff::*;
use super::handlers_appointments::*;
use super::handlers_reminders::*;
use super::module::*;

// Handlers: services

pub(crate) fn validate_service_fields(
    duration_minutes: i64,
    price: i64,
    capacity: i64,
    buffer_before: Option<i64>,
    buffer_after: Option<i64>,
    notice_hours: Option<i64>,
    fee_percent: Option<f64>,
) -> UnifiedResult<i64> {
    if !(MIN_DURATION_MINUTES..=MAX_DURATION_MINUTES).contains(&duration_minutes) {
        return Err(UnifiedError::bad_request(format!(
            "duration_minutes must be between {MIN_DURATION_MINUTES} and {MAX_DURATION_MINUTES}"
        )));
    }
    let price = valid_price(price)?;
    if !(1..=MAX_CAPACITY).contains(&capacity) {
        return Err(UnifiedError::bad_request(format!(
            "capacity must be between 1 and {MAX_CAPACITY}"
        )));
    }
    for (value, field) in [
        (buffer_before, "buffer_before_minutes"),
        (buffer_after, "buffer_after_minutes"),
    ] {
        if let Some(minutes) = value {
            if !(0..=MAX_BUFFER_MINUTES).contains(&minutes) {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be between 0 and {MAX_BUFFER_MINUTES}"
                )));
            }
        }
    }
    if let Some(hours) = notice_hours {
        if !(0..=MAX_NOTICE_HOURS).contains(&hours) {
            return Err(UnifiedError::bad_request(format!(
                "cancellation_notice_hours must be between 0 and {MAX_NOTICE_HOURS}"
            )));
        }
    }
    if let Some(percent) = fee_percent {
        if !percent.is_finite() || !(0.0..=MAX_FEE_PERCENT).contains(&percent) {
            return Err(UnifiedError::bad_request(
                "cancellation_fee_percent must be between 0 and 100",
            ));
        }
    }
    Ok(price)
}
