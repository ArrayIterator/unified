// crm — handlers activities.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::handlers_contacts::*;
use super::handlers_leads::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Activity handlers
// ---------------------------------------------------------------------------

/// Remove every activity attached to `record_id`. Called from the
/// contact/lead delete handlers so deletes leave no orphans.
pub(crate) fn delete_activities_for(state: &CrmState, record_id: &str) {
    state.activities.remove(record_id);
}

pub(crate) async fn add_activity(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<CreateActivity>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Activity> = (|| {
        let (record_type, record_id) = path.into_inner();
        if record_type != "contacts" && record_type != "leads" {
            return Err(UnifiedError::bad_request(
                "Record type must be contacts or leads",
            ));
        }
        let exists = match record_type.as_str() {
            "contacts" => state.contacts.contains_key(&record_id),
            _ => state.leads.contains_key(&record_id),
        };
        if !exists {
            return Err(UnifiedError::not_found("Record not found"));
        }
        let input = body.into_inner();
        if input.note.trim().is_empty() {
            return Err(UnifiedError::bad_request("Activity note must not be empty"));
        }
        let activity = Activity {
            id: new_id(),
            record_type: record_type.clone(),
            record_id: record_id.clone(),
            kind: input.kind.unwrap_or_else(|| "note".to_string()),
            note: input.note.trim().to_string(),
            created_at: now(),
        };
        state
            .activities
            .entry(record_id)
            .or_default()
            .push(activity.clone());
        Ok(activity)
    })();
    respond_created(result)
}

pub(crate) async fn list_activities(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_READ) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let (record_type, record_id) = path.into_inner();
    if record_type != "contacts" && record_type != "leads" {
        return json::error_response(
            &UnifiedError::bad_request("Record type must be contacts or leads"),
            false,
        );
    }
    // Like `add_activity`: a missing record is a 404, not an empty list.
    let exists = match record_type.as_str() {
        "contacts" => state.contacts.contains_key(&record_id),
        _ => state.leads.contains_key(&record_id),
    };
    if !exists {
        return json::error_response(&UnifiedError::not_found("Record not found"), ctx.is_debug());
    }
    let items: Vec<Activity> = state
        .activities
        .get(&record_id)
        .map(|entry| {
            entry
                .value()
                .iter()
                .filter(|activity| activity.record_type == record_type)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Hook handler: `crm.activity.requested` (emitted by appointments when
// an appointment completes).
// ---------------------------------------------------------------------------

/// Record a completed visit as an activity on the matching contact
/// (preferred) or lead, matched by `customer_email`
/// (case-insensitive). Contacts and leads carry no user id, so the
/// email is the only join key the payload offers. Payloads without
/// a usable email, or with no matching record, are ignored: there
/// is nowhere to attach the activity.
pub(crate) fn on_crm_activity_requested(state: Arc<CrmState>, ctx: &mut HookContext) {
    let data = &ctx.data;
    let Some(email) = data
        .get("customer_email")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|email| !email.is_empty())
    else {
        return;
    };
    let matched: Option<(String, String)> = state
        .contacts
        .iter()
        .find(|entry| entry.value().email.eq_ignore_ascii_case(email))
        .map(|entry| ("contacts".to_string(), entry.key().clone()))
        .or_else(|| {
            state
                .leads
                .iter()
                .find(|entry| entry.value().email.eq_ignore_ascii_case(email))
                .map(|entry| ("leads".to_string(), entry.key().clone()))
        });
    let Some((record_type, record_id)) = matched else {
        return;
    };
    let kind = data
        .get("activity_type")
        .and_then(|value| value.as_str())
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or("appointment")
        .to_string();
    let note = data
        .get("subject")
        .and_then(|value| value.as_str())
        .filter(|note| !note.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "Completed appointment".to_string());
    let activity = Activity {
        id: new_id(),
        record_type,
        record_id: record_id.clone(),
        kind,
        note,
        created_at: now(),
    };
    state
        .activities
        .entry(record_id)
        .or_default()
        .push(activity);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(id: &str, email: &str) -> Contact {
        Contact {
            id: id.to_string(),
            name: "Ada Lovelace".to_string(),
            email: email.to_string(),
            company: None,
            phone: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn completed_appointment_is_recorded_as_contact_activity() {
        let state = Arc::new(CrmState::new());
        state
            .contacts
            .insert("contact-1".to_string(), contact("contact-1", "ada@example.com"));
        let mut ctx = HookContext::new(
            "crm.activity.requested",
            json!({
                "appointment_id": "appt-1",
                "customer_email": "ADA@example.com",
                "customer_name": "Ada Lovelace",
                "activity_type": "appointment_completed",
                "subject": "Completed appointment: Consultation",
            }),
        );
        on_crm_activity_requested(Arc::clone(&state), &mut ctx);
        let activities = state.activities.get("contact-1").expect("activities stored");
        assert_eq!(activities.len(), 1);
        assert_eq!(activities[0].record_type, "contacts");
        assert_eq!(activities[0].kind, "appointment_completed");
        assert_eq!(
            activities[0].note,
            "Completed appointment: Consultation"
        );
    }

    #[test]
    fn activity_request_without_a_matching_record_is_ignored() {
        let state = Arc::new(CrmState::new());
        state
            .contacts
            .insert("contact-1".to_string(), contact("contact-1", "ada@example.com"));
        let mut ctx = HookContext::new(
            "crm.activity.requested",
            json!({
                "appointment_id": "appt-2",
                "customer_email": "nobody@example.com",
                "activity_type": "appointment_completed",
                "subject": "Completed appointment: Consultation",
            }),
        );
        on_crm_activity_requested(Arc::clone(&state), &mut ctx);
        assert!(state.activities.is_empty());

        // No email at all: also ignored, never panics.
        let mut ctx = HookContext::new(
            "crm.activity.requested",
            json!({ "appointment_id": "appt-3" }),
        );
        on_crm_activity_requested(Arc::clone(&state), &mut ctx);
        assert!(state.activities.is_empty());
    }

    #[test]
    fn deleting_a_record_cascades_its_activities() {
        let state = CrmState::new();
        state
            .contacts
            .insert("contact-1".to_string(), contact("contact-1", "ada@example.com"));
        state.activities.entry("contact-1".to_string()).or_default().push(Activity {
            id: new_id(),
            record_type: "contacts".to_string(),
            record_id: "contact-1".to_string(),
            kind: "note".to_string(),
            note: "hello".to_string(),
            created_at: now(),
        });
        assert!(state.activities.contains_key("contact-1"));
        delete_activities_for(&state, "contact-1");
        assert!(!state.activities.contains_key("contact-1"));
    }
}
