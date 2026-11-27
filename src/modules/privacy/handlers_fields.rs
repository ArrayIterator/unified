// Handlers: redaction, field declarations, payload validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use super::*;

// Handlers: redaction, field declarations, payload validation

pub(crate) async fn redact(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<RedactInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let input = body.into_inner();
        let redacted = match (&input.text, &input.value) {
            (Some(text), _) => json!({ "text": redact_text(text) }),
            (None, Some(value)) => {
                json!({ "value": redact_value(value, input.redact_names) })
            }
            (None, None) => {
                return Err(UnifiedError::bad_request(
                    "Provide \"text\" or \"value\" to redact",
                ));
            }
        };
        state.audit(
            "privacy.redaction.used",
            &caller.actor,
            "privacy.redact",
            "privacy:redact",
            json!({ "redact_names": input.redact_names }),
        );
        Ok(redacted)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_field_declarations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<FieldDeclaration>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<FieldDeclaration> = state
            .declarations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(
                    &format!("{} {} {}", record.module_id, record.field_name, record.purpose),
                    &params,
                ) && params
                    .filters
                    .get("module_id")
                    .is_none_or(|module_id| record.module_id == *module_id)
                    && params
                        .filters
                        .get("field_name")
                        .is_none_or(|field| record.field_name == *field)
            })
            .collect();
        sort_desc_by_created(&mut items, |record| record.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_field_declaration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<CreateFieldDeclarationInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FieldDeclaration> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        let input = body.into_inner();
        if !valid_slug(&input.module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        if !valid_field_name(&input.field_name) {
            return Err(UnifiedError::bad_request(
                "field_name must be lowercase letters, digits, or '_'",
            ));
        }
        let purpose = require_text(&input.purpose, "purpose", 500)?;
        if state.declarations.iter().any(|entry| {
            entry.value().module_id == input.module_id
                && entry.value().field_name == input.field_name
        }) {
            return Err(UnifiedError::conflict(format!(
                "Field \"{}\" is already declared for module \"{}\"",
                input.field_name, input.module_id
            )));
        }
        let record = FieldDeclaration {
            id: new_id(),
            module_id: input.module_id,
            field_name: input.field_name,
            purpose,
            required: input.required,
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .declarations
            .insert(record.id.clone(), record.clone());
        state.audit(
            EVENT_PRIVACY_FIELDS_CHANGED,
            &caller.actor,
            "privacy.field_declaration.created",
            &format!("module:{}", record.module_id),
            json!({
                "module_id": record.module_id,
                "field_name": record.field_name,
                "declaration_id": record.id,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_field_declaration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FieldDeclaration> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        state
            .declarations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Field declaration not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_field_declaration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFieldDeclarationInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FieldDeclaration> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        let input = body.into_inner();
        let mut record = state
            .declarations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Field declaration not found"))?;
        if let Some(purpose) = &input.purpose {
            record.purpose = require_text(purpose, "purpose", 500)?;
        }
        if let Some(required) = input.required {
            record.required = required;
        }
        record.updated_at = now();
        state
            .declarations
            .insert(record.id.clone(), record.clone());
        state.audit(
            EVENT_PRIVACY_FIELDS_CHANGED,
            &caller.actor,
            "privacy.field_declaration.updated",
            &format!("module:{}", record.module_id),
            json!({
                "module_id": record.module_id,
                "field_name": record.field_name,
                "declaration_id": record.id,
            }),
        );
        Ok(record)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_field_declaration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        let record = state
            .declarations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Field declaration not found"))?;
        state.declarations.remove(path.as_str());
        state.audit(
            EVENT_PRIVACY_FIELDS_CHANGED,
            &caller.actor,
            "privacy.field_declaration.deleted",
            &format!("module:{}", record.module_id),
            json!({
                "module_id": record.module_id,
                "field_name": record.field_name,
                "declaration_id": record.id,
            }),
        );
        Ok(json!({ "deleted": true, "id": record.id }))
    })();
    json::respond(result, debug)
}

/// Validate an incoming payload against the declared personal-data
/// fields of a module. Undeclared personal-data keys are rejected
/// with a 422 listing them; a module with no declaration allows no
/// personal-data fields at all. Non-personal keys always pass.
pub(crate) async fn validate_payload(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<ValidatePayloadInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        let input = body.into_inner();
        if !valid_slug(&input.module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        let object = match input.payload.as_object() {
            Some(object) => object,
            None => {
                return Err(UnifiedError::bad_request(
                    "payload must be a JSON object",
                ));
            }
        };
        let declared = state.declared_fields(&input.module_id);
        let mut rejected: Vec<String> = Vec::new();
        for key in object.keys() {
            if is_pii_key(key) && !declared.iter().any(|field| field == key) {
                rejected.push(key.clone());
            }
        }
        rejected.sort();
        if !rejected.is_empty() {
            return Err(UnifiedError::unprocessable(format!(
                "Payload for module \"{}\" contains undeclared personal-data fields: {}",
                input.module_id,
                rejected.join(", ")
            ))
            .with_metadata("rejected_fields", json!(rejected))
            .with_metadata("module_id", json!(input.module_id))
            .with_metadata("declared_fields", json!(declared)));
        }
        Ok(json!({
            "module_id": input.module_id,
            "allowed": true,
            "checked_fields": object.keys().collect::<Vec<_>>(),
            "declared_fields": declared,
        }))
    })();
    json::respond(result, debug)
}
