//! HTTP handlers for KYC case documents: linking a
//! documents-module document by reference, listing, reading,
//! and unlinking. Documents are metadata only - a document
//! reference, its SHA-256, and file metadata - never bytes.
//!
//! Changing the documents of a case that already carries an
//! AI verdict invalidates that verdict: the verdict and risk
//! are cleared and the case returns to `submitted`, so the
//! case must be re-verified before any decision.

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
    caller, can_manage, case_event, created, err, load_document, load_document_type_by_code,
    load_visible_case, ok, paginate, require_content_hash, require_text, resolve_tenant,
};
use super::state::KycState;
use super::types::{KycCaseStatus, KycDocument, LinkDocumentRequest, MAX_LABEL_LEN};

/// Path carrying a case id.
#[derive(Debug, Deserialize)]
struct CasePath {
    case_id: String,
}

/// Path carrying a case id and a document id.
#[derive(Debug, Deserialize)]
struct CaseDocumentPath {
    case_id: String,
    document_id: String,
}

/// Statuses in which a case's documents may change.
fn documents_editable(status: KycCaseStatus) -> bool {
    matches!(
        status,
        KycCaseStatus::Draft | KycCaseStatus::Submitted | KycCaseStatus::MoreInfoRequested
    )
}

/// After a document change, a case that already carried a
/// verdict must be re-verified: clear the verdict and risk,
/// return the case to `submitted`, and emit the submission
/// event so it re-enters the review queue.
fn invalidate_verdict(state: &KycState, case: &mut super::types::KycCase, actor: &str) {
    if case.ai_verdict.is_some() || case.risk_score.is_some() {
        case.ai_verdict = None;
        case.risk_score = None;
        case.risk_level_id = None;
        case.risk_level_code = None;
        if case.status != KycCaseStatus::Draft {
            case.status = KycCaseStatus::Submitted;
        }
        case.updated_at = Utc::now();
        state.cases.insert(case.id, case.clone());
        case_event(
            state,
            super::types::EVENT_CASE_SUBMITTED,
            case,
            actor,
            "documents_updated",
            Some(
                "Documents changed; previous AI verdict invalidated, re-verification required"
                    .to_string(),
            ),
        );
    }
}

pub(crate) async fn list_documents(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let case = load_visible_case(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        let params = query.into_inner();
        let type_filter = params.filters.get("document_type_code").cloned();
        let mut items: Vec<KycDocument> = state
            .case_documents(case.id)
            .into_iter()
            .filter(|document| {
                type_filter
                    .as_deref()
                    .map(|code| document.document_type_code == code)
                    .unwrap_or(true)
            })
            .collect();
        items.sort_by_key(|document| document.created_at);
        let views: Vec<Value> = items
            .iter()
            .map(|document| serde_json::to_value(document).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn link_document(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CasePath>,
    body: web::Json<LinkDocumentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut case = load_visible_case(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        if !documents_editable(case.status) {
            return Err(UnifiedError::bad_request(format!(
                "Documents cannot be changed while a case is {}",
                case.status.as_str()
            )));
        }
        let input = body.into_inner();
        let document_type =
            load_document_type_by_code(&state, &tenant_id, &input.document_type_code)?;
        if !document_type.active {
            return Err(UnifiedError::bad_request(format!(
                "Document type \"{}\" is not active",
                document_type.code
            )));
        }
        // The kind must apply to this case's subject type (an
        // empty subject-types list means it applies to all).
        if !document_type.subject_types.is_empty()
            && !document_type
                .subject_types
                .iter()
                .any(|subject_type| subject_type == &case.subject_type)
        {
            return Err(UnifiedError::bad_request(format!(
                "Document type \"{}\" does not apply to subject type \"{}\"",
                document_type.code, case.subject_type
            )));
        }
        let document_id = require_text(&input.document_id, "document_id", MAX_LABEL_LEN)?;
        let content_hash = require_content_hash(&input.content_hash)?;
        let file_name = match &input.file_name {
            Some(name) => Some(require_text(name, "file_name", 255)?),
            None => None,
        };
        let mime_type = match &input.mime_type {
            Some(mime) => Some(require_text(mime, "mime_type", 128)?),
            None => None,
        };
        let settings = state.current_settings(&tenant_id);
        let existing = state.case_documents(case.id);
        if existing.len() as i64 >= settings.max_documents_per_case {
            return Err(UnifiedError::bad_request(format!(
                "Case already has the maximum of {} documents",
                settings.max_documents_per_case
            )));
        }
        if existing
            .iter()
            .any(|document| document.document_id == document_id)
        {
            return Err(UnifiedError::conflict(format!(
                "Document {document_id} is already linked to this case"
            )));
        }
        let document = KycDocument {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            case_id: case.id,
            document_type_code: document_type.code.clone(),
            document_id,
            content_hash,
            file_name,
            mime_type,
            uploaded_by: caller.actor.clone(),
            created_at: Utc::now(),
        };
        state.documents.insert(document.id, document.clone());
        // A document change invalidates any earlier verdict.
        invalidate_verdict(&state, &mut case, &caller.actor);
        // Managers linking on behalf of a case still leave the
        // case in the queue; the event above covers the
        // re-verification case, and this audit entry covers the
        // link itself.
        super::helpers::append_audit(
            &state,
            &tenant_id,
            case.id,
            "document_linked",
            &caller.actor,
            Some(format!(
                "Linked {} document {}",
                document.document_type_code, document.document_id
            )),
        );
        Ok(serde_json::to_value(document).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_document(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CaseDocumentPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let case = load_visible_case(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        let document = load_document(&state, &tenant_id, &path.document_id)?;
        if document.case_id != case.id {
            return Err(UnifiedError::not_found(format!(
                "Document not found: {}",
                document.id
            )));
        }
        Ok(serde_json::to_value(document).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn unlink_document(
    state: web::Data<KycState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<CaseDocumentPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let mut case = load_visible_case(&state, &ctx, &tenant_id, &caller, &path.case_id)?;
        if !documents_editable(case.status) {
            return Err(UnifiedError::bad_request(format!(
                "Documents cannot be changed while a case is {}",
                case.status.as_str()
            )));
        }
        let document = load_document(&state, &tenant_id, &path.document_id)?;
        if document.case_id != case.id {
            return Err(UnifiedError::not_found(format!(
                "Document not found: {}",
                document.id
            )));
        }
        // Only the uploader or a manager may unlink.
        if document.uploaded_by != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!(
                "Document not found: {}",
                document.id
            )));
        }
        state.documents.remove(&document.id);
        invalidate_verdict(&state, &mut case, &caller.actor);
        super::helpers::append_audit(
            &state,
            &tenant_id,
            case.id,
            "document_unlinked",
            &caller.actor,
            Some(format!(
                "Unlinked {} document {}",
                document.document_type_code, document.document_id
            )),
        );
        Ok(json!({ "deleted": true, "document_id": document.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
