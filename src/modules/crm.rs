// CRM module.
//
// Contacts and leads, a pipeline grouped by stage, lead conversion into
// a contact (emitting `company.create_requested` and
// `user.create_requested`), and activities on contacts and leads.
// Interconnection happens only through the hook bus.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
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
pub struct Contact {
    pub id: String,
    pub name: String,
    pub email: String,
    pub company: Option<String>,
    pub phone: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateContact {
    pub name: String,
    pub email: String,
    pub company: Option<String>,
    pub phone: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateContact {
    pub name: Option<String>,
    pub email: Option<String>,
    pub company: Option<String>,
    pub phone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lead {
    pub id: String,
    pub name: String,
    pub email: String,
    pub company: Option<String>,
    pub source: Option<String>,
    /// `new`, `contacted`, `qualified`, `negotiation`, `won`, `lost`, `converted`.
    pub stage: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateLead {
    pub name: String,
    pub email: String,
    pub company: Option<String>,
    pub source: Option<String>,
    pub stage: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateLead {
    pub name: Option<String>,
    pub email: Option<String>,
    pub company: Option<String>,
    pub source: Option<String>,
    pub stage: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub id: String,
    pub record_type: String,
    pub record_id: String,
    pub kind: String,
    pub note: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateActivity {
    pub note: String,
    pub kind: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct CrmState {
    hooks: RwLock<Arc<HookBus>>,
    contacts: DashMap<String, Contact>,
    leads: DashMap<String, Lead>,
    activities: DashMap<String, Vec<Activity>>,
}

impl CrmState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            contacts: DashMap::new(),
            leads: DashMap::new(),
            activities: DashMap::new(),
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

fn valid_email(email: &str) -> bool {
    let email = email.trim();
    match email.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty() && !domain.is_empty() && domain.contains('.') && !email.contains(' ')
        }
        None => false,
    }
}

fn valid_stage(stage: &str) -> bool {
    matches!(
        stage,
        "new" | "contacted" | "qualified" | "negotiation" | "won" | "lost" | "converted"
    )
}

const PIPELINE_STAGES: &[&str] = &[
    "new",
    "contacted",
    "qualified",
    "negotiation",
    "won",
    "lost",
    "converted",
];

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// Contact handlers
// ---------------------------------------------------------------------------

async fn list_contacts(
    state: web::Data<Arc<CrmState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Contact> = state
        .contacts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|contact| {
            matches_search(
                &format!(
                    "{} {} {}",
                    contact.name,
                    contact.email,
                    contact.company.clone().unwrap_or_default()
                ),
                &params,
            ) && params.filters.get("company").is_none_or(|company| {
                contact.company.as_deref() == Some(company.as_str())
            })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_contact(
    state: web::Data<Arc<CrmState>>,
    body: web::Json<CreateContact>,
) -> HttpResponse {
    let result: UnifiedResult<Contact> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Contact name must not be empty"));
        }
        if !valid_email(&input.email) {
            return Err(UnifiedError::bad_request("Contact email is not valid"));
        }
        let contact = Contact {
            id: new_id(),
            name: input.name.trim().to_string(),
            email: input.email.trim().to_string(),
            company: input.company,
            phone: input.phone,
            created_at: now(),
            updated_at: now(),
        };
        state.contacts.insert(contact.id.clone(), contact.clone());
        Ok(contact)
    })();
    respond_created(result)
}

async fn get_contact(
    state: web::Data<Arc<CrmState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Contact> = state
        .contacts
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Contact not found"));
    json::respond(result, false)
}

async fn update_contact(
    state: web::Data<Arc<CrmState>>,
    path: web::Path<String>,
    body: web::Json<UpdateContact>,
) -> HttpResponse {
    let result: UnifiedResult<Contact> = (|| {
        let input = body.into_inner();
        let mut contact = state
            .contacts
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Contact not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Contact name must not be empty"));
            }
            contact.name = name.trim().to_string();
        }
        if let Some(email) = input.email {
            if !valid_email(&email) {
                return Err(UnifiedError::bad_request("Contact email is not valid"));
            }
            contact.email = email.trim().to_string();
        }
        if let Some(company) = input.company {
            contact.company = Some(company);
        }
        if let Some(phone) = input.phone {
            contact.phone = Some(phone);
        }
        contact.updated_at = now();
        Ok(contact.clone())
    })();
    json::respond(result, false)
}

async fn delete_contact(
    state: web::Data<Arc<CrmState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .contacts
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Contact not found"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Lead handlers
// ---------------------------------------------------------------------------

async fn list_leads(
    state: web::Data<Arc<CrmState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Lead> = state
        .leads
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|lead| {
            matches_search(
                &format!(
                    "{} {} {}",
                    lead.name,
                    lead.email,
                    lead.company.clone().unwrap_or_default()
                ),
                &params,
            ) && params
                .filters
                .get("stage")
                .is_none_or(|stage| lead.stage == *stage)
                && params
                    .filters
                    .get("source")
                    .is_none_or(|source| lead.source.as_deref() == Some(source.as_str()))
                && params.filters.get("company").is_none_or(|company| {
                    lead.company.as_deref() == Some(company.as_str())
                })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_lead(
    state: web::Data<Arc<CrmState>>,
    body: web::Json<CreateLead>,
) -> HttpResponse {
    let result: UnifiedResult<Lead> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Lead name must not be empty"));
        }
        if !valid_email(&input.email) {
            return Err(UnifiedError::bad_request("Lead email is not valid"));
        }
        let stage = input.stage.unwrap_or_else(|| "new".to_string());
        if !valid_stage(&stage) {
            return Err(UnifiedError::bad_request(
                "Stage must be one of: new, contacted, qualified, negotiation, won, lost, converted",
            ));
        }
        let lead = Lead {
            id: new_id(),
            name: input.name.trim().to_string(),
            email: input.email.trim().to_string(),
            company: input.company,
            source: input.source,
            stage,
            created_at: now(),
            updated_at: now(),
        };
        state.leads.insert(lead.id.clone(), lead.clone());
        Ok(lead)
    })();
    respond_created(result)
}

async fn get_lead(
    state: web::Data<Arc<CrmState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Lead> = state
        .leads
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Lead not found"));
    json::respond(result, false)
}

async fn update_lead(
    state: web::Data<Arc<CrmState>>,
    path: web::Path<String>,
    body: web::Json<UpdateLead>,
) -> HttpResponse {
    let result: UnifiedResult<Lead> = (|| {
        let input = body.into_inner();
        let mut lead = state
            .leads
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Lead not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Lead name must not be empty"));
            }
            lead.name = name.trim().to_string();
        }
        if let Some(email) = input.email {
            if !valid_email(&email) {
                return Err(UnifiedError::bad_request("Lead email is not valid"));
            }
            lead.email = email.trim().to_string();
        }
        if let Some(company) = input.company {
            lead.company = Some(company);
        }
        if let Some(source) = input.source {
            lead.source = Some(source);
        }
        if let Some(stage) = input.stage {
            if !valid_stage(&stage) {
                return Err(UnifiedError::bad_request(
                    "Stage must be one of: new, contacted, qualified, negotiation, won, lost, converted",
                ));
            }
            if lead.stage == "converted" && stage != "converted" {
                return Err(UnifiedError::unprocessable(
                    "A converted lead cannot move back to another stage",
                ));
            }
            lead.stage = stage;
        }
        lead.updated_at = now();
        Ok(lead.clone())
    })();
    json::respond(result, false)
}

async fn delete_lead(
    state: web::Data<Arc<CrmState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .leads
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Lead not found"))
    })();
    json::respond(result, false)
}

async fn get_pipeline(state: web::Data<Arc<CrmState>>) -> HttpResponse {
    let mut stages: Vec<Value> = Vec::new();
    let mut total_leads = 0u64;
    for stage in PIPELINE_STAGES {
        let count = state
            .leads
            .iter()
            .filter(|entry| entry.value().stage == *stage)
            .count() as u64;
        total_leads += count;
        stages.push(json!({ "stage": stage, "count": count }));
    }
    json::success_response(
        json!({ "stages": stages, "total_leads": total_leads }),
        200,
    )
}

async fn convert_lead(
    state: web::Data<Arc<CrmState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let lead = state
            .leads
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Lead not found"))?;
        if lead.stage == "converted" {
            return Err(UnifiedError::conflict("Lead is already converted"));
        }
        let contact = Contact {
            id: new_id(),
            name: lead.name.clone(),
            email: lead.email.clone(),
            company: lead.company.clone(),
            phone: None,
            created_at: now(),
            updated_at: now(),
        };
        state.contacts.insert(contact.id.clone(), contact.clone());
        if let Some(company) = &lead.company {
            state.emit(
                "company.create_requested",
                json!({ "name": company, "lead_id": lead.id, "contact_id": contact.id }),
            );
        }
        state.emit(
            "user.create_requested",
            json!({
                "name": lead.name,
                "email": lead.email,
                "company": lead.company,
                "lead_id": lead.id,
                "contact_id": contact.id,
            }),
        );
        if let Some(mut entry) = state.leads.get_mut(path.as_str()) {
            entry.stage = "converted".to_string();
            entry.updated_at = now();
        }
        let updated_lead = state
            .leads
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("lead still present");
        Ok(json!({ "lead": updated_lead, "contact": contact }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Activity handlers
// ---------------------------------------------------------------------------

async fn add_activity(
    state: web::Data<Arc<CrmState>>,
    path: web::Path<(String, String)>,
    body: web::Json<CreateActivity>,
) -> HttpResponse {
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

async fn list_activities(
    state: web::Data<Arc<CrmState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let (record_type, record_id) = path.into_inner();
    if record_type != "contacts" && record_type != "leads" {
        return json::error_response(
            &UnifiedError::bad_request("Record type must be contacts or leads"),
            false,
        );
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
// Module
// ---------------------------------------------------------------------------

pub struct CrmModule {
    state: Arc<CrmState>,
}

impl CrmModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(CrmState::new()),
        }
    }
}

impl Default for CrmModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CrmModule {
    fn id(&self) -> &'static str {
        "crm"
    }

    fn name(&self) -> &'static str {
        "CRM"
    }

    fn description(&self) -> &'static str {
        "CRM contacts and leads: pipeline, conversion and activities."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["company", "users"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/crm/contacts", owner: "crm", description: "List contacts" },
            RouteInfo { method: "POST", path: "/api/v1/crm/contacts", owner: "crm", description: "Create contact" },
            RouteInfo { method: "GET", path: "/api/v1/crm/contacts/{id}", owner: "crm", description: "Get contact" },
            RouteInfo { method: "PUT", path: "/api/v1/crm/contacts/{id}", owner: "crm", description: "Update contact" },
            RouteInfo { method: "DELETE", path: "/api/v1/crm/contacts/{id}", owner: "crm", description: "Delete contact" },
            RouteInfo { method: "GET", path: "/api/v1/crm/leads", owner: "crm", description: "List leads" },
            RouteInfo { method: "POST", path: "/api/v1/crm/leads", owner: "crm", description: "Create lead" },
            RouteInfo { method: "GET", path: "/api/v1/crm/leads/{id}", owner: "crm", description: "Get lead" },
            RouteInfo { method: "PUT", path: "/api/v1/crm/leads/{id}", owner: "crm", description: "Update lead" },
            RouteInfo { method: "DELETE", path: "/api/v1/crm/leads/{id}", owner: "crm", description: "Delete lead" },
            RouteInfo { method: "POST", path: "/api/v1/crm/leads/{id}/convert", owner: "crm", description: "Convert lead" },
            RouteInfo { method: "GET", path: "/api/v1/crm/pipeline", owner: "crm", description: "Get pipeline" },
            RouteInfo { method: "GET", path: "/api/v1/crm/{type}/{id}/activities", owner: "crm", description: "List activities" },
            RouteInfo { method: "POST", path: "/api/v1/crm/{type}/{id}/activities", owner: "crm", description: "Add activity" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/crm")
                .route("/contacts", web::get().to(list_contacts))
                .route("/contacts", web::post().to(create_contact))
                .route("/contacts/{id}", web::get().to(get_contact))
                .route("/contacts/{id}", web::put().to(update_contact))
                .route("/contacts/{id}", web::delete().to(delete_contact))
                .route("/leads", web::get().to(list_leads))
                .route("/leads", web::post().to(create_lead))
                .route("/leads/{id}", web::get().to(get_lead))
                .route("/leads/{id}", web::put().to(update_lead))
                .route("/leads/{id}", web::delete().to(delete_lead))
                .route("/leads/{id}/convert", web::post().to(convert_lead))
                .route("/pipeline", web::get().to(get_pipeline))
                .route("/{type}/{id}/activities", web::get().to(list_activities))
                .route("/{type}/{id}/activities", web::post().to(add_activity)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CrmModule::new()));
}
