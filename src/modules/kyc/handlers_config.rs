//! HTTP handlers for KYC configuration (manager): document
//! types, subject types, verification contexts (the default
//! context), extra instructions, risk levels, and per-tenant
//! settings. Everything here is data - the module never
//! hard-codes a document kind, a check, an instruction, or a
//! risk band.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, check_risk_range_overlap, created, err, load_context, load_document_type_by_code,
    load_instruction, load_risk_level, ok, paginate, require_code, require_description,
    require_instruction_text, require_label, require_permission, require_string_list,
    resolve_tenant, validate_score_range,
};
use super::state::KycState;
use super::types::{
    CreateContextRequest, CreateDocumentTypeRequest, CreateInstructionRequest,
    CreateRiskLevelRequest, CreateSubjectTypeRequest, KycDocumentType, KycInstruction,
    KycRiskLevel, KycSubjectType, KycVerificationContext, UpdateContextRequest,
    UpdateDocumentTypeRequest, UpdateInstructionRequest, UpdateRiskLevelRequest,
    UpdateSettingsRequest, UpdateSubjectTypeRequest, PERM_KYC_MANAGE,
};

/// Path carrying one config row id.
#[derive(Debug, Deserialize)]
struct RowPath {
    row_id: String,
}

fn require_manager(ctx: &AppContext, caller: &super::helpers::Caller) -> UnifiedResult<()> {
    require_permission(ctx, &caller.role, PERM_KYC_MANAGE)
}

fn active_filter(params: &PaginationParams) -> Option<bool> {
    params
        .filters
        .get("active")
        .map(|raw| raw == "true" || raw == "1")
}

fn search_needle(params: &PaginationParams) -> Option<String> {
    params.search.clone().map(|value| value.to_lowercase())
}

fn to_values<T: serde::Serialize>(rows: &[T]) -> Vec<Value> {
    rows.iter()
        .map(|row| serde_json::to_value(row).unwrap_or(Value::Null))
        .collect()
}

// Document types

pub(crate) async fn list_document_types(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let params = query.into_inner();
        let active = active_filter(&params);
        let needle = search_needle(&params);
        let subject_filter = params.filters.get("subject_type").cloned();
        let mut rows: Vec<KycDocumentType> = state
            .document_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.tenant_id != tenant_id {
                    return false;
                }
                if let Some(active) = active {
                    if row.active != active {
                        return false;
                    }
                }
                if let Some(subject) = &subject_filter {
                    if !row.subject_types.is_empty()
                        && !row.subject_types.iter().any(|code| code == subject)
                    {
                        return false;
                    }
                }
                if let Some(needle) = &needle {
                    if !row.code.to_lowercase().contains(needle.as_str())
                        && !row.label.to_lowercase().contains(needle.as_str())
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        rows.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(to_values(&rows), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_document_type(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateDocumentTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let input = body.into_inner();
        let code = require_code(&input.code, "code")?;
        let label = require_label(&input.label)?;
        let description = require_description(&input.description)?;
        let subject_types = require_string_list(&input.subject_types, "subject_types")?;
        if state
            .document_types
            .iter()
            .any(|entry| entry.value().tenant_id == tenant_id && entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Document type \"{code}\" already exists for this tenant"
            )));
        }
        let now = Utc::now();
        let row = KycDocumentType {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            code,
            label,
            description,
            subject_types,
            active: input.active,
            created_at: now,
            updated_at: now,
        };
        state.document_types.insert(row.id, row.clone());
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_document_type(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let id = super::helpers::parse_uuid(&path.row_id, "document type")?;
        let row = state
            .document_types
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Document type not found: {id}")))?;
        if row.tenant_id != tenant_id {
            return Err(UnifiedError::not_found(format!(
                "Document type not found: {id}"
            )));
        }
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_document_type(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
    body: web::Json<UpdateDocumentTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let id = super::helpers::parse_uuid(&path.row_id, "document type")?;
        let mut row = state
            .document_types
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Document type not found: {id}")))?;
        if row.tenant_id != tenant_id {
            return Err(UnifiedError::not_found(format!(
                "Document type not found: {id}"
            )));
        }
        let input = body.into_inner();
        if let Some(label) = &input.label {
            row.label = require_label(label)?;
        }
        if let Some(description) = &input.description {
            row.description = match description {
                Some(text) => Some(require_description(&Some(text.clone()))?.unwrap_or_default()),
                None => None,
            };
        }
        if let Some(subject_types) = &input.subject_types {
            row.subject_types = require_string_list(subject_types, "subject_types")?;
        }
        if let Some(active) = input.active {
            row.active = active;
        }
        row.updated_at = Utc::now();
        state.document_types.insert(row.id, row.clone());
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_document_type(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let id = super::helpers::parse_uuid(&path.row_id, "document type")?;
        let row = state
            .document_types
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Document type not found: {id}")))?;
        if row.tenant_id != tenant_id {
            return Err(UnifiedError::not_found(format!(
                "Document type not found: {id}"
            )));
        }
        if state
            .documents
            .iter()
            .any(|entry| entry.value().document_type_code == row.code)
        {
            return Err(UnifiedError::bad_request(format!(
                "Document type \"{}\" is still linked to cases and cannot be deleted",
                row.code
            )));
        }
        state.document_types.remove(&id);
        Ok(json!({ "deleted": true, "document_type_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Subject types

pub(crate) async fn list_subject_types(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let params = query.into_inner();
        let active = active_filter(&params);
        let needle = search_needle(&params);
        let mut rows: Vec<KycSubjectType> = state
            .subject_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.tenant_id != tenant_id {
                    return false;
                }
                if let Some(active) = active {
                    if row.active != active {
                        return false;
                    }
                }
                if let Some(needle) = &needle {
                    if !row.code.to_lowercase().contains(needle.as_str())
                        && !row.label.to_lowercase().contains(needle.as_str())
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        rows.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(to_values(&rows), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_subject_type(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateSubjectTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let input = body.into_inner();
        let code = require_code(&input.code, "code")?;
        let label = require_label(&input.label)?;
        let description = require_description(&input.description)?;
        let required = require_string_list(
            &input.required_document_type_codes,
            "required_document_type_codes",
        )?;
        for code in &required {
            load_document_type_by_code(&state, &tenant_id, code)?;
        }
        if state
            .subject_types
            .iter()
            .any(|entry| entry.value().tenant_id == tenant_id && entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Subject type \"{code}\" already exists for this tenant"
            )));
        }
        let now = Utc::now();
        let row = KycSubjectType {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            code,
            label,
            description,
            required_document_type_codes: required,
            active: input.active,
            created_at: now,
            updated_at: now,
        };
        state.subject_types.insert(row.id, row.clone());
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_subject_type(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let id = super::helpers::parse_uuid(&path.row_id, "subject type")?;
        let row = state
            .subject_types
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Subject type not found: {id}")))?;
        if row.tenant_id != tenant_id {
            return Err(UnifiedError::not_found(format!(
                "Subject type not found: {id}"
            )));
        }
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_subject_type(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
    body: web::Json<UpdateSubjectTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let id = super::helpers::parse_uuid(&path.row_id, "subject type")?;
        let mut row = state
            .subject_types
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Subject type not found: {id}")))?;
        if row.tenant_id != tenant_id {
            return Err(UnifiedError::not_found(format!(
                "Subject type not found: {id}"
            )));
        }
        let input = body.into_inner();
        if let Some(label) = &input.label {
            row.label = require_label(label)?;
        }
        if let Some(description) = &input.description {
            row.description = match description {
                Some(text) => Some(require_description(&Some(text.clone()))?.unwrap_or_default()),
                None => None,
            };
        }
        if let Some(required) = &input.required_document_type_codes {
            let required = require_string_list(required, "required_document_type_codes")?;
            for code in &required {
                load_document_type_by_code(&state, &tenant_id, code)?;
            }
            row.required_document_type_codes = required;
        }
        if let Some(active) = input.active {
            row.active = active;
        }
        row.updated_at = Utc::now();
        state.subject_types.insert(row.id, row.clone());
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_subject_type(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let id = super::helpers::parse_uuid(&path.row_id, "subject type")?;
        let row = state
            .subject_types
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Subject type not found: {id}")))?;
        if row.tenant_id != tenant_id {
            return Err(UnifiedError::not_found(format!(
                "Subject type not found: {id}"
            )));
        }
        if state.cases.iter().any(|entry| {
            entry.value().tenant_id == tenant_id && entry.value().subject_type == row.code
        }) {
            return Err(UnifiedError::bad_request(format!(
                "Subject type \"{}\" still has cases and cannot be deleted",
                row.code
            )));
        }
        state.subject_types.remove(&id);
        Ok(json!({ "deleted": true, "subject_type_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Verification contexts (the default context)

pub(crate) async fn list_contexts(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let params = query.into_inner();
        let active = active_filter(&params);
        let needle = search_needle(&params);
        let mut rows: Vec<KycVerificationContext> = state
            .contexts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.tenant_id != tenant_id {
                    return false;
                }
                if let Some(active) = active {
                    if row.active != active {
                        return false;
                    }
                }
                if let Some(needle) = &needle {
                    if !row.name.to_lowercase().contains(needle.as_str()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        rows.sort_by(|a, b| b.version.cmp(&a.version));
        Ok(paginate(to_values(&rows), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_context(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateContextRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let input = body.into_inner();
        let name = super::helpers::require_text(&input.name, "name", 200)?;
        let description = require_description(&input.description)?;
        let visible_fields = require_string_list(&input.visible_fields, "visible_fields")?;
        let checks = require_string_list(&input.checks, "checks")?;
        if state
            .contexts
            .iter()
            .any(|entry| entry.value().tenant_id == tenant_id && entry.value().name == name)
        {
            return Err(UnifiedError::conflict(format!(
                "Verification context \"{name}\" already exists for this tenant"
            )));
        }
        if input.active {
            deactivate_other_contexts(&state, &tenant_id, None);
        }
        let now = Utc::now();
        let row = KycVerificationContext {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            name,
            description,
            version: 1,
            active: input.active,
            visible_fields,
            checks,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.contexts.insert(row.id, row.clone());
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Deactivate every other active context of the tenant, so at
/// most one default context is active at a time.
fn deactivate_other_contexts(state: &KycState, tenant_id: &str, except: Option<Uuid>) {
    for mut entry in state.contexts.iter_mut() {
        if entry.value().tenant_id == tenant_id
            && entry.value().active
            && Some(entry.value().id) != except
        {
            entry.value_mut().active = false;
            entry.value_mut().updated_at = Utc::now();
        }
    }
}

pub(crate) async fn get_context(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let row = load_context(&state, &tenant_id, &path.row_id)?;
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a context. Any content change bumps `version`, so a
/// verdict always records the version it ran under; flipping
/// `active` alone does not bump the version.
pub(crate) async fn update_context(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
    body: web::Json<UpdateContextRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let mut row = load_context(&state, &tenant_id, &path.row_id)?;
        let input = body.into_inner();
        let mut changed = false;
        if let Some(name) = &input.name {
            let name = super::helpers::require_text(name, "name", 200)?;
            if name != row.name {
                if state.contexts.iter().any(|entry| {
                    entry.value().tenant_id == tenant_id
                        && entry.value().name == name
                        && entry.value().id != row.id
                }) {
                    return Err(UnifiedError::conflict(format!(
                        "Verification context \"{name}\" already exists for this tenant"
                    )));
                }
                row.name = name;
                changed = true;
            }
        }
        if let Some(description) = &input.description {
            let description = match description {
                Some(text) => Some(require_description(&Some(text.clone()))?.unwrap_or_default()),
                None => None,
            };
            if description != row.description {
                row.description = description;
                changed = true;
            }
        }
        if let Some(visible_fields) = &input.visible_fields {
            let visible_fields = require_string_list(visible_fields, "visible_fields")?;
            if visible_fields != row.visible_fields {
                row.visible_fields = visible_fields;
                changed = true;
            }
        }
        if let Some(checks) = &input.checks {
            let checks = require_string_list(checks, "checks")?;
            if checks != row.checks {
                row.checks = checks;
                changed = true;
            }
        }
        if let Some(active) = input.active {
            if active && !row.active {
                deactivate_other_contexts(&state, &tenant_id, Some(row.id));
            }
            row.active = active;
        }
        if changed {
            row.version += 1;
        }
        row.updated_at = Utc::now();
        state.contexts.insert(row.id, row.clone());
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_context(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let row = load_context(&state, &tenant_id, &path.row_id)?;
        if state
            .instructions
            .iter()
            .any(|entry| entry.value().context_id == Some(row.id))
        {
            return Err(UnifiedError::bad_request(
                "Verification context still has instructions; delete or move them first",
            ));
        }
        state.contexts.remove(&row.id);
        Ok(json!({ "deleted": true, "context_id": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// The resolved default context for this tenant: the active
/// context merged with its active extra instructions. This is
/// the read endpoint operators use to see exactly what the AI
/// verification pipeline will follow.
pub(crate) async fn get_resolved_context(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let context = state.active_context(&tenant_id).ok_or_else(|| {
            UnifiedError::not_found("No active verification context exists for this tenant")
        })?;
        let instructions = state.applicable_instructions(&tenant_id, context.id);
        let resolved = super::types::resolve_verification_context(&context, &instructions);
        Ok(json!({
            "context_id": resolved.context_id.to_string(),
            "context_name": resolved.context_name,
            "context_version": resolved.context_version,
            "visible_fields": resolved.visible_fields,
            "checks": resolved.checks,
            "instruction_texts": resolved.instruction_texts,
            "instruction_versions": resolved.instruction_versions,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Instructions (versioned extra instructions)

pub(crate) async fn list_instructions(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let params = query.into_inner();
        let active = active_filter(&params);
        let context_filter = params.filters.get("context_id").cloned();
        let needle = search_needle(&params);
        let mut rows: Vec<KycInstruction> = state
            .instructions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.tenant_id != tenant_id {
                    return false;
                }
                if let Some(active) = active {
                    if row.active != active {
                        return false;
                    }
                }
                if let Some(context_id) = &context_filter {
                    if row.context_id.map(|id| id.to_string()) != Some(context_id.clone()) {
                        return false;
                    }
                }
                if let Some(needle) = &needle {
                    if !row.text.to_lowercase().contains(needle.as_str()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        rows.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        Ok(paginate(to_values(&rows), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_instruction(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateInstructionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let input = body.into_inner();
        let text = require_instruction_text(&input.text)?;
        if let Some(context_id) = input.context_id {
            load_context(&state, &tenant_id, &context_id.to_string())?;
        }
        let now = Utc::now();
        let row = KycInstruction {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            context_id: input.context_id,
            text,
            version: 1,
            active: input.active,
            priority: input.priority,
            supersedes_id: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.instructions.insert(row.id, row.clone());
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_instruction(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let row = load_instruction(&state, &tenant_id, &path.row_id)?;
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update an instruction. Changing the text is versioned: a
/// new row is created with `version` + 1 and the old row is
/// deactivated with `supersedes_id` pointing at the new row,
/// so the history is never rewritten. Changing only the
/// priority or the active flag updates the row in place.
pub(crate) async fn update_instruction(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
    body: web::Json<UpdateInstructionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let mut row = load_instruction(&state, &tenant_id, &path.row_id)?;
        let input = body.into_inner();
        if let Some(text) = &input.text {
            let text = require_instruction_text(text)?;
            if text != row.text {
                let now = Utc::now();
                let next = KycInstruction {
                    id: Uuid::now_v7(),
                    tenant_id: tenant_id.clone(),
                    context_id: row.context_id,
                    text,
                    version: row.version + 1,
                    active: input.active.unwrap_or(row.active),
                    priority: input.priority.unwrap_or(row.priority),
                    supersedes_id: None,
                    created_by: caller.actor.clone(),
                    created_at: now,
                    updated_at: now,
                };
                row.active = false;
                row.supersedes_id = Some(next.id);
                row.updated_at = now;
                state.instructions.insert(row.id, row.clone());
                state.instructions.insert(next.id, next.clone());
                return Ok(serde_json::to_value(next).unwrap_or(Value::Null));
            }
        }
        if let Some(priority) = input.priority {
            row.priority = priority;
        }
        if let Some(active) = input.active {
            row.active = active;
        }
        row.updated_at = Utc::now();
        state.instructions.insert(row.id, row.clone());
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_instruction(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let row = load_instruction(&state, &tenant_id, &path.row_id)?;
        state.instructions.remove(&row.id);
        Ok(json!({ "deleted": true, "instruction_id": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Risk levels

pub(crate) async fn list_risk_levels(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let params = query.into_inner();
        let active = active_filter(&params);
        let needle = search_needle(&params);
        let mut rows: Vec<KycRiskLevel> = state
            .risk_levels
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.tenant_id != tenant_id {
                    return false;
                }
                if let Some(active) = active {
                    if row.active != active {
                        return false;
                    }
                }
                if let Some(needle) = &needle {
                    if !row.code.to_lowercase().contains(needle.as_str())
                        && !row.label.to_lowercase().contains(needle.as_str())
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        rows.sort_by_key(|row| row.min_score);
        Ok(paginate(to_values(&rows), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_risk_level(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateRiskLevelRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let input = body.into_inner();
        let code = require_code(&input.code, "code")?;
        let label = require_label(&input.label)?;
        let description = require_description(&input.description)?;
        validate_score_range(input.min_score, input.max_score)?;
        if state
            .risk_levels
            .iter()
            .any(|entry| entry.value().tenant_id == tenant_id && entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Risk level \"{code}\" already exists for this tenant"
            )));
        }
        if input.active {
            check_risk_range_overlap(&state, &tenant_id, input.min_score, input.max_score, None)?;
        }
        let now = Utc::now();
        let row = KycRiskLevel {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            code,
            label,
            description,
            min_score: input.min_score,
            max_score: input.max_score,
            active: input.active,
            created_at: now,
            updated_at: now,
        };
        state.risk_levels.insert(row.id, row.clone());
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_risk_level(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let row = load_risk_level(&state, &tenant_id, &path.row_id)?;
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_risk_level(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
    body: web::Json<UpdateRiskLevelRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let mut row = load_risk_level(&state, &tenant_id, &path.row_id)?;
        let input = body.into_inner();
        if let Some(label) = &input.label {
            row.label = require_label(label)?;
        }
        if let Some(description) = &input.description {
            row.description = match description {
                Some(text) => Some(require_description(&Some(text.clone()))?.unwrap_or_default()),
                None => None,
            };
        }
        let min_score = input.min_score.unwrap_or(row.min_score);
        let max_score = input.max_score.unwrap_or(row.max_score);
        validate_score_range(min_score, max_score)?;
        let active = input.active.unwrap_or(row.active);
        if active {
            check_risk_range_overlap(&state, &tenant_id, min_score, max_score, Some(row.id))?;
        }
        row.min_score = min_score;
        row.max_score = max_score;
        row.active = active;
        row.updated_at = Utc::now();
        state.risk_levels.insert(row.id, row.clone());
        Ok(serde_json::to_value(row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_risk_level(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<RowPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let row = load_risk_level(&state, &tenant_id, &path.row_id)?;
        if state
            .cases
            .iter()
            .any(|entry| entry.value().risk_level_id == Some(row.id))
        {
            return Err(UnifiedError::bad_request(format!(
                "Risk level \"{}\" is still assigned to cases and cannot be deleted",
                row.code
            )));
        }
        state.risk_levels.remove(&row.id);
        Ok(json!({ "deleted": true, "risk_level_id": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Settings

pub(crate) async fn get_settings(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let settings = state.current_settings(&tenant_id);
        Ok(serde_json::to_value(settings).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manager(&ctx, &caller)?;
        let mut settings = state.current_settings(&tenant_id);
        let input = body.into_inner();
        if let Some(auto_approve) = input.auto_approve_low_risk {
            settings.auto_approve_low_risk = auto_approve;
        }
        if let Some(code) = &input.auto_approve_risk_level_code {
            settings.auto_approve_risk_level_code =
                require_code(code, "auto_approve_risk_level_code")?;
        }
        if let Some(require_ai) = input.require_ai_verification {
            settings.require_ai_verification = require_ai;
        }
        if let Some(expiry_days) = input.verification_expiry_days {
            if expiry_days < 1 {
                return Err(UnifiedError::bad_request(
                    "verification_expiry_days must be at least 1",
                ));
            }
            settings.verification_expiry_days = expiry_days;
        }
        if let Some(max_documents) = input.max_documents_per_case {
            if max_documents < 1 {
                return Err(UnifiedError::bad_request(
                    "max_documents_per_case must be at least 1",
                ));
            }
            settings.max_documents_per_case = max_documents;
        }
        state.save_settings(&tenant_id, settings.clone());
        Ok(serde_json::to_value(settings).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
