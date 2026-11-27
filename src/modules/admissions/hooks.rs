// admissions module - Hook handlers.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{MAX_ID, MAX_SHORT, MAX_TEXT};
use super::types::Application;
use super::state::AdmissionsState;
use super::helpers::{new_id, now, slugify};

// Hook handlers (events consumed)

/// Handle `forms.mapping.requested` for target
/// `admissions_application`.
///
/// CONTRACT - the registration form's mapping must use target
/// `admissions_application` and a field map that produces these
/// fields (all values are the submitted form values, already mapped
/// by the forms module):
///   - `period_id` (required): the admission period id.
///   - `applicant_user_id` (optional): the applicant's user id; when
///     absent the submission is recorded without an owner and staff
///     assign one later.
///   - `full_name` (required), `email` (required), `phone`
///     (optional), `program_id` (required), `level` (required).
/// The handler answers with `{"created": true, "id": "<application
/// id>"}` on success or `{"created": false, "error": "..."}` on
/// failure, through the first result slot the forms module reads.
/// The created application starts in status `draft` with source
/// `form`; the applicant (or staff) completes it and submits it
/// through `POST /api/v1/admissions/applications/{id}/submit`.
/// Submissions for other targets are ignored (no result added).
pub(crate) fn on_forms_mapping_requested(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let target = hook_ctx
        .data
        .get("target")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if target != "admissions_application" {
        return;
    }
    let fields = hook_ctx
        .data
        .get("fields")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();
    let submission_id = hook_ctx
        .data
        .get("submission_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let field_str = |key: &str| -> Option<String> {
        fields
            .get(key)
            .and_then(|value| value.as_str())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let mut fail = |message: &str| {
        hook_ctx.add_result(json!({ "created": false, "error": message }));
    };
    let period_id = match field_str("period_id") {
        Some(value) if value.len() <= MAX_ID => value,
        _ => {
            fail("period_id is required and must be at most 128 characters");
            return;
        }
    };
    let Ok(period) = state.period(&period_id) else {
        fail("Admission period not found");
        return;
    };
    if !state.period_accepts_submissions(&period) {
        fail("Admission period is not open for submissions");
        return;
    }
    let full_name = match field_str("full_name") {
        Some(value) if value.len() <= MAX_TEXT => value,
        _ => {
            fail("full_name is required and must be at most 2000 characters");
            return;
        }
    };
    let email = match field_str("email") {
        Some(value) => {
            let email = value.to_lowercase();
            if email.len() > MAX_SHORT || !email.contains('@') || email.contains(' ') {
                fail("email must be a valid email address");
                return;
            }
            email
        }
        None => {
            fail("email is required");
            return;
        }
    };
    let program_id = match field_str("program_id") {
        Some(value) if value.len() <= MAX_ID => value,
        _ => {
            fail("program_id is required and must be at most 128 characters");
            return;
        }
    };
    let level = match field_str("level") {
        Some(value) if value.len() <= MAX_SHORT => value,
        _ => {
            fail("level is required and must be at most 255 characters");
            return;
        }
    };
    let applicant_user_id = field_str("applicant_user_id")
        .filter(|value| value.len() <= MAX_ID)
        .unwrap_or_default();
    if let Some(existing_submission_id) = &submission_id {
        if state.applications.iter().any(|entry| {
            entry
                .value()
                .form_submission_id
                .as_deref()
                == Some(existing_submission_id.as_str())
        }) {
            fail("An application already exists for this form submission");
            return;
        }
    }
    let application = Application {
        id: new_id(),
        period_id: period.id.clone(),
        applicant_user_id,
        full_name,
        email,
        phone: field_str("phone").filter(|value| value.len() <= MAX_SHORT),
        program_id,
        level,
        form_submission_id: submission_id.clone(),
        status: "draft".to_string(),
        total_score: None,
        fee_amount: period.registration_fee_amount,
        fee_currency: period.fee_currency.clone(),
        fee_invoice_status: "none".to_string(),
        fee_invoice_id: None,
        student_id: None,
        source: "form".to_string(),
        created_at: now(),
        updated_at: now(),
    };
    state.ensure_checklist(&application);
    state.applications.insert(application.id.clone(), application.clone());
    state.emit(
        "admissions.application_created",
        json!({
            "application_id": application.id,
            "period_id": application.period_id,
            "applicant_user_id": application.applicant_user_id,
            "source": "form",
        }),
    );
    if submission_id.is_some() {
        state.emit(
            "admissions.application_received",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
                "applicant_user_id": application.applicant_user_id,
                "form_submission_id": submission_id,
            }),
        );
    }
    hook_ctx.add_result(json!({ "created": true, "id": application.id }));
}

/// Handle `forms.submission.received` ({form_id, submission_id,
/// slug}). The event carries no field values, so it cannot create an
/// application on its own. When a draft application already exists
/// for the submission (created by the mapping handler) nothing more
/// is needed; when a period links this form id and no application
/// exists yet for the submission, a placeholder draft is recorded
/// with the submission id so staff can complete it. The placeholder
/// carries no applicant data and cannot be submitted until the
/// required fields are filled in.
pub(crate) fn on_forms_submission_received(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let form_id = hook_ctx
        .data
        .get("form_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let submission_id = hook_ctx
        .data
        .get("submission_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if form_id.is_empty() || submission_id.is_empty() {
        return;
    }
    if state.applications.iter().any(|entry| {
        entry
            .value()
            .form_submission_id
            .as_deref()
            == Some(submission_id)
    }) {
        return;
    }
    let Some(period) = state.periods.iter().find(|entry| {
        entry
            .value()
            .registration_form_id
            .as_deref()
            == Some(form_id)
    }) else {
        return;
    };
    let period = period.value().clone();
    if !state.period_accepts_submissions(&period) {
        return;
    }
    let application = Application {
        id: new_id(),
        period_id: period.id.clone(),
        applicant_user_id: String::new(),
        full_name: String::new(),
        email: String::new(),
        phone: None,
        program_id: String::new(),
        level: String::new(),
        form_submission_id: Some(submission_id.to_string()),
        status: "draft".to_string(),
        total_score: None,
        fee_amount: period.registration_fee_amount,
        fee_currency: period.fee_currency.clone(),
        fee_invoice_status: "none".to_string(),
        fee_invoice_id: None,
        student_id: None,
        source: "form".to_string(),
        created_at: now(),
        updated_at: now(),
    };
    state.ensure_checklist(&application);
    state.applications.insert(application.id.clone(), application.clone());
    state.emit(
        "admissions.application_received",
        json!({
            "application_id": application.id,
            "period_id": application.period_id,
            "applicant_user_id": application.applicant_user_id,
            "form_submission_id": submission_id,
        }),
    );
}

/// Handle `document.uploaded` (the full document record). When the
/// document belongs to this module (`module_id` is `admissions`) and
/// the slug of the file name's stem (without its extension) equals a
/// pending checklist item's `document_key` for an application owned
/// by `owner_id`, the item is marked `submitted` and linked to the
/// document. The event carries no application or period id, so an
/// upload that matches pending items of several applications is
/// ambiguous and auto-attaches to nothing; staff attach it manually.
/// A matching upload attaches to one item only: the earliest pending
/// item of the single matching application. Uploads that match
/// nothing are ignored.
pub(crate) fn on_document_uploaded(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let data = hook_ctx.data.clone();
    if data.get("module_id").and_then(|value| value.as_str()) != Some("admissions") {
        return;
    }
    let document_id = match data.get("id").and_then(|value| value.as_str()) {
        Some(value) => value.to_string(),
        None => return,
    };
    let document_name = match data.get("name").and_then(|value| value.as_str()) {
        Some(value) => value.to_string(),
        None => return,
    };
    let owner_id = match data.get("owner_id").and_then(|value| value.as_str()) {
        Some(value) => value.to_string(),
        None => return,
    };
    // File names carry an extension ("Birth Certificate.pdf"), so
    // slugify the stem; the key itself is already a slug.
    let stem = document_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.is_empty())
        .unwrap_or(document_name.as_str());
    let key = slugify(stem);
    if key.is_empty() {
        return;
    }
    let mut matched: Vec<(String, String, DateTime<Utc>)> = Vec::new();
    for entry in state.checklist.iter() {
        let item = entry.value();
        if item.status != "pending" || item.document_key != key {
            continue;
        }
        let Some(application) = state
            .applications
            .get(&item.application_id)
            .map(|value| value.value().clone())
        else {
            continue;
        };
        if application.applicant_user_id == owner_id {
            matched.push((item.id.clone(), application.id.clone(), item.created_at));
        }
    }
    // Matches spanning several applications are ambiguous: attach
    // nothing rather than the same document to the wrong application.
    let distinct_applications: std::collections::HashSet<&str> = matched
        .iter()
        .map(|(_, application_id, _)| application_id.as_str())
        .collect();
    if distinct_applications.len() != 1 {
        return;
    }
    matched.sort_by(|a, b| a.2.cmp(&b.2));
    let Some((item_id, application_id, _)) = matched.into_iter().next() else {
        return;
    };
    if let Some(mut item) = state.checklist.get_mut(&item_id) {
        item.status = "submitted".to_string();
        item.document_id = Some(document_id.clone());
        item.updated_at = now();
    }
    state.emit(
        "admissions.document_attached",
        json!({
            "application_id": application_id,
            "checklist_item_id": item_id,
            "document_id": document_id,
        }),
    );
}

/// Handle `document.deleted` ({id, ...}). Any checklist item
/// pointing at the deleted document goes back to `pending` with its
/// link cleared, so the applicant can upload a replacement.
pub(crate) fn on_document_deleted(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let document_id = hook_ctx
        .data
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if document_id.is_empty() {
        return;
    }
    for mut entry in state.checklist.iter_mut() {
        let item = entry.value_mut();
        if item.document_id.as_deref() == Some(document_id) {
            item.document_id = None;
            if item.status == "submitted" || item.status == "verified" {
                item.status = "pending".to_string();
            }
            item.verified_by = None;
            item.verified_at = None;
            item.updated_at = now();
        }
    }
}

/// Handle `invoice.paid` ({invoice, method}). When the paid invoice
/// carries an `order_id` equal to an application id, the
/// application's fee is marked paid. Billing must set `order_id` to
/// the application id when fulfilling `billing.invoice.requested`
/// for admissions (see the module header, convergence point 2).
pub(crate) fn on_invoice_paid(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let invoice = hook_ctx.data.get("invoice").cloned().unwrap_or(Value::Null);
    let order_id = invoice
        .get("order_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if order_id.is_empty() {
        return;
    }
    let invoice_id = invoice
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(mut application) = state.applications.get_mut(order_id) else {
        return;
    };
    if application.fee_invoice_status == "paid" {
        return;
    }
    application.fee_invoice_status = "paid".to_string();
    if let Some(invoice_id) = invoice_id.clone() {
        application.fee_invoice_id = Some(invoice_id.clone());
    }
    application.updated_at = now();
    let application_id = application.id.clone();
    let period_id = application.period_id.clone();
    drop(application);
    state.emit(
        "admissions.fee_paid",
        json!({
            "application_id": application_id,
            "period_id": period_id,
            "invoice_id": invoice_id,
        }),
    );
}

/// Handle `academic.student.created` ({application_id, student_id}).
/// Links the student record created by the academic system to the
/// enrolled application. Applications are never turned into student
/// records here.
pub(crate) fn on_academic_student_created(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let application_id = hook_ctx
        .data
        .get("application_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let student_id = hook_ctx
        .data
        .get("student_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if application_id.is_empty() || student_id.is_empty() {
        return;
    }
    if let Some(mut application) = state.applications.get_mut(application_id) {
        application.student_id = Some(student_id.to_string());
        // The status is left untouched: enrolment already sets it to
        // `enrolled` before requesting the student record, so this
        // handler only links the student id back.
        application.updated_at = now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::ChecklistItem;

    fn application(id: &str, owner: &str, status: &str) -> Application {
        Application {
            id: id.to_string(),
            period_id: "p1".to_string(),
            applicant_user_id: owner.to_string(),
            full_name: "Test Applicant".to_string(),
            email: "test@example.com".to_string(),
            phone: None,
            program_id: "prog-1".to_string(),
            level: "undergraduate".to_string(),
            form_submission_id: None,
            status: status.to_string(),
            total_score: None,
            fee_amount: None,
            fee_currency: "USD".to_string(),
            fee_invoice_status: "none".to_string(),
            fee_invoice_id: None,
            student_id: None,
            source: "manual".to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn checklist_item(id: &str, application_id: &str, key: &str) -> ChecklistItem {
        ChecklistItem {
            id: id.to_string(),
            application_id: application_id.to_string(),
            period_id: "p1".to_string(),
            required_document_id: "doc-1".to_string(),
            document_key: key.to_string(),
            title: "Birth Certificate".to_string(),
            required: true,
            status: "pending".to_string(),
            document_id: None,
            verified_by: None,
            verified_at: None,
            rejection_reason: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn upload_event(document_id: &str, name: &str, owner: &str) -> HookContext {
        HookContext::new(
            "document.uploaded",
            json!({
                "id": document_id,
                "name": name,
                "module_id": "admissions",
                "owner_id": owner,
            }),
        )
    }

    #[test]
    fn on_document_uploaded_matches_the_slugified_file_stem() {
        let state = Arc::new(AdmissionsState::new());
        state
            .applications
            .insert("app-1".to_string(), application("app-1", "user-1", "submitted"));
        state.checklist.insert(
            "item-1".to_string(),
            checklist_item("item-1", "app-1", "birth-certificate"),
        );
        let mut ctx = upload_event("doc-1", "Birth Certificate.pdf", "user-1");
        on_document_uploaded(state.clone(), &mut ctx);
        let item = state.checklist.get("item-1").unwrap();
        assert_eq!(item.status, "submitted");
        assert_eq!(item.document_id.as_deref(), Some("doc-1"));
    }

    #[test]
    fn on_document_uploaded_ignores_ambiguous_matches_across_applications() {
        let state = Arc::new(AdmissionsState::new());
        state
            .applications
            .insert("app-1".to_string(), application("app-1", "user-1", "submitted"));
        state
            .applications
            .insert("app-2".to_string(), application("app-2", "user-1", "submitted"));
        state.checklist.insert(
            "item-1".to_string(),
            checklist_item("item-1", "app-1", "birth-certificate"),
        );
        state.checklist.insert(
            "item-2".to_string(),
            checklist_item("item-2", "app-2", "birth-certificate"),
        );
        let mut ctx = upload_event("doc-1", "Birth Certificate.pdf", "user-1");
        on_document_uploaded(state.clone(), &mut ctx);
        assert_eq!(
            state.checklist.get("item-1").unwrap().status,
            "pending"
        );
        assert_eq!(
            state.checklist.get("item-2").unwrap().status,
            "pending"
        );
    }

    #[test]
    fn on_document_uploaded_ignores_other_owners_and_other_modules() {
        let state = Arc::new(AdmissionsState::new());
        state
            .applications
            .insert("app-1".to_string(), application("app-1", "user-1", "submitted"));
        state.checklist.insert(
            "item-1".to_string(),
            checklist_item("item-1", "app-1", "birth-certificate"),
        );
        let mut ctx = upload_event("doc-1", "Birth Certificate.pdf", "user-2");
        on_document_uploaded(state.clone(), &mut ctx);
        assert_eq!(
            state.checklist.get("item-1").unwrap().status,
            "pending"
        );
        let mut ctx = HookContext::new(
            "document.uploaded",
            json!({
                "id": "doc-1",
                "name": "Birth Certificate.pdf",
                "module_id": "other-module",
                "owner_id": "user-1",
            }),
        );
        on_document_uploaded(state.clone(), &mut ctx);
        assert_eq!(
            state.checklist.get("item-1").unwrap().status,
            "pending"
        );
    }

    #[test]
    fn on_academic_student_created_links_student_without_changing_status() {
        let state = Arc::new(AdmissionsState::new());
        state
            .applications
            .insert("app-1".to_string(), application("app-1", "user-1", "accepted"));
        let mut ctx = HookContext::new(
            "academic.student.created",
            json!({ "application_id": "app-1", "student_id": "stu-1" }),
        );
        on_academic_student_created(state.clone(), &mut ctx);
        let stored = state.applications.get("app-1").unwrap();
        assert_eq!(stored.student_id.as_deref(), Some("stu-1"));
        // The dead accepted-to-enrolled branch is gone: the status
        // is left exactly as it was.
        assert_eq!(stored.status, "accepted");
    }
}
