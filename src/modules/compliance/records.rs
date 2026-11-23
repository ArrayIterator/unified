//! Compliance Center module — GDPR / ISO support: data export — Processing-record handlers (RoPA).
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::checklists::*;
use super::consents::*;
use super::constants::*;
use super::deletion_listeners::*;
use super::deletions::*;
use super::exports::*;
use super::helpers::*;
use super::module::*;
use super::requests::*;
use super::retention::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_processing_records(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let params = query.into_inner();
        let purpose_filter = params.filters.get("purpose").cloned();
        let basis_filter = params.filters.get("lawful_basis").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ProcessingRecord> = state
            .processing_records
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                if let Some(purpose) = &purpose_filter {
                    if &record.purpose != purpose {
                        return false;
                    }
                }
                if let Some(basis) = &basis_filter {
                    if &record.lawful_basis != basis {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!(
                        "{} {} {}",
                        record.purpose,
                        record.data_categories.join(" "),
                        record.recipients.join(" ")
                    )
                    .to_lowercase()
                    .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|record| record.purpose.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_processing_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateProcessingRecordRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProcessingRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let input = body.into_inner();
        let purpose = require_text(&input.purpose, "purpose", MAX_PURPOSE_LEN)?;
        let mut data_categories: Vec<String> = Vec::new();
        for raw in &input.data_categories {
            data_categories.push(require_text(raw, "data_categories entry", MAX_CATEGORY_LEN)?);
        }
        let mut recipients: Vec<String> = Vec::new();
        for raw in &input.recipients {
            recipients.push(require_text(raw, "recipients entry", MAX_NAME_LEN)?);
        }
        let retention = require_text(&input.retention, "retention", MAX_PURPOSE_LEN)?;
        let lawful_basis = require_text(&input.lawful_basis, "lawful_basis", MAX_BASIS_LEN)?;
        let owner = match &input.owner {
            Some(raw) => Some(require_text(raw, "owner", MAX_NAME_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let record = ProcessingRecord {
            id: Uuid::now_v7(),
            purpose,
            data_categories,
            recipients,
            retention,
            lawful_basis,
            owner,
            created_at: now,
            updated_at: now,
        };
        state.processing_records.insert(record.id, record.clone());
        state.emit(
            "compliance.processing_record.created",
            json!({
                "record_id": record.id.to_string(),
                "purpose": record.purpose,
                "lawful_basis": record.lawful_basis,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => created(record),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_processing_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProcessingRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        load_processing_record(&state, &path.into_inner())
    })();
    match result {
        Ok(record) => ok(record),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn update_processing_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProcessingRecordRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProcessingRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut record = load_processing_record(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(purpose) = &input.purpose {
            record.purpose = require_text(purpose, "purpose", MAX_PURPOSE_LEN)?;
        }
        if let Some(categories) = &input.data_categories {
            let mut checked: Vec<String> = Vec::new();
            for raw in categories {
                checked.push(require_text(raw, "data_categories entry", MAX_CATEGORY_LEN)?);
            }
            record.data_categories = checked;
        }
        if let Some(recipients) = &input.recipients {
            let mut checked: Vec<String> = Vec::new();
            for raw in recipients {
                checked.push(require_text(raw, "recipients entry", MAX_NAME_LEN)?);
            }
            record.recipients = checked;
        }
        if let Some(retention) = &input.retention {
            record.retention = require_text(retention, "retention", MAX_PURPOSE_LEN)?;
        }
        if let Some(basis) = &input.lawful_basis {
            record.lawful_basis = require_text(basis, "lawful_basis", MAX_BASIS_LEN)?;
        }
        if let Some(owner) = &input.owner {
            record.owner = match owner {
                Some(raw) => Some(require_text(raw, "owner", MAX_NAME_LEN)?),
                None => None,
            };
        }
        record.updated_at = Utc::now();
        state.processing_records.insert(record.id, record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => ok(record),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn delete_processing_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let record = load_processing_record(&state, &path.into_inner())?;
        let id = record.id;
        state.processing_records.remove(&id);
        Ok(json!({ "deleted": true, "record_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
