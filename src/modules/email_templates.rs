// Email Template Builder module (GrapeJS).
//
// Templates are designed in GrapeJS on the client side and stored here as
// HTML plus the GrapeJS design JSON. Every update creates a new version,
// so templates are fully restorable.
//
// Routes (all under `/api/v1/email-templates`):
//   GET    /                              - paginated list (filter by category, search)
//   POST   /                              - create a template (becomes version 1)
//   GET    /by-category/{category}        - paginated list for one category
//   GET    /{id}                          - one template, including its versions
//   PUT    /{id}                          - update (creates a new version)
//   DELETE /{id}                          - delete a template
//   GET    /{id}/versions                 - version history
//   POST   /{id}/restore/{version}        - restore a version (creates a new version)
//   POST   /{id}/variables                - placeholders found in the template
//   POST   /{id}/preview                  - render with variables substituted
//   POST   /{id}/test-send                - render + send through the Mail Manager
//
// Hook contracts:
//   Answers `template.render_requested` with `{ template_id, variables }`
//   by adding a result `{ rendered_html, subject, missing_variables }`
//   (or `{ error }` when the template does not exist). The Mail Manager
//   uses this to render templates for sending.
//   Test-send emits `mail.send_requested`; when no mail handler answers,
//   it returns 503 with a clear message.
//   Emits `template.created`, `template.updated`, `template.deleted` and
//   `template.restored` for observability.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::sync::OnceLock;
use uuid::Uuid;

pub const EVENT_TEMPLATE_RENDER_REQUESTED: &str = "template.render_requested";
pub const EVENT_MAIL_SEND_REQUESTED: &str = "mail.send_requested";

/// `{{ name }}` and `{{ user.name }}` placeholders. Dotted paths resolve
/// into nested variable objects.
fn placeholder_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"\{\{\s*([a-zA-Z0-9_.]+)\s*\}\}")
            .expect("placeholder regex must compile")
    })
}

/// Escape a substituted value so user-supplied data can never break out
/// of the template HTML.
fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Resolve a dotted variable path (`user.name`) against the variables map.
fn lookup_variable<'a>(variables: &'a HashMap<String, Value>, path: &str) -> Option<&'a Value> {
    let mut current: Option<&Value> = None;
    for (index, part) in path.split('.').enumerate() {
        current = if index == 0 {
            variables.get(part)
        } else {
            current.and_then(|value| value.get(part))
        };
        if current.is_none() {
            return None;
        }
    }
    current
}

/// Render a JSON value as plain text for substitution.
fn value_as_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Find every placeholder in a text, sorted and de-duplicated.
fn extract_variables(text: &str) -> Vec<String> {
    let mut vars: Vec<String> = placeholder_regex()
        .captures_iter(text)
        .filter_map(|caps| caps.get(1).map(|m| m.as_str().to_string()))
        .collect();
    vars.sort();
    vars.dedup();
    vars
}

/// Substitute placeholders in `text`. Unknown placeholders are left
/// untouched and recorded in `missing`.
fn substitute(
    text: &str,
    variables: &HashMap<String, Value>,
    missing: &mut Vec<String>,
) -> String {
    let regex = placeholder_regex();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in regex.captures_iter(text) {
        let whole = caps.get(0).expect("placeholder match");
        out.push_str(&text[last..whole.start()]);
        let key = caps.get(1).expect("placeholder group").as_str();
        match lookup_variable(variables, key) {
            Some(value) => out.push_str(&escape_html(&value_as_text(value))),
            None => {
                if !missing.iter().any(|item| item == key) {
                    missing.push(key.to_string());
                }
                out.push_str(whole.as_str());
            }
        }
        last = whole.end();
    }
    out.push_str(&text[last..]);
    out
}

/// One stored version of a template.
#[derive(Debug, Clone, Serialize)]
pub struct TemplateVersion {
    pub version: u32,
    pub subject: String,
    pub html: String,
    pub json: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailTemplate {
    pub id: String,
    pub name: String,
    pub subject: String,
    pub category: String,
    pub html: String,
    /// GrapeJS design JSON (blocks, styles, component tree).
    pub json: Value,
    pub version: u32,
    pub versions: Vec<TemplateVersion>,
    pub created_at: String,
    pub updated_at: String,
}

impl EmailTemplate {
    fn snapshot(&self) -> TemplateVersion {
        TemplateVersion {
            version: self.version,
            subject: self.subject.clone(),
            html: self.html.clone(),
            json: self.json.clone(),
            created_at: self.updated_at.clone(),
        }
    }
}

pub struct EmailTemplateState {
    templates: RwLock<HashMap<String, EmailTemplate>>,
    hooks: RwLock<Option<Arc<HookBus>>>,
    debug: AtomicBool,
}

impl EmailTemplateState {
    fn new() -> Self {
        Self {
            templates: RwLock::new(HashMap::new()),
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
        }
    }

    fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("template hooks lock poisoned") = Some(hooks);
    }

    fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("template hooks lock poisoned").clone()
    }

    fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    fn get(&self, id: &str) -> Option<EmailTemplate> {
        self.templates
            .read()
            .expect("template lock poisoned")
            .get(id)
            .cloned()
    }

    /// Render a template with variables. Returns rendered HTML, rendered
    /// subject, and the placeholders that had no value.
    fn render(
        template: &EmailTemplate,
        variables: &HashMap<String, Value>,
    ) -> (String, String, Vec<String>) {
        let mut missing = Vec::new();
        let html = substitute(&template.html, variables, &mut missing);
        let subject = substitute(&template.subject, variables, &mut missing);
        (html, subject, missing)
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(hooks) = self.hooks() {
            hooks.emit(event, data);
        }
    }
}

#[derive(Debug, Deserialize)]
struct TemplateInput {
    name: String,
    subject: String,
    category: Option<String>,
    html: String,
    json: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct TemplateUpdateInput {
    name: Option<String>,
    subject: Option<String>,
    category: Option<String>,
    html: Option<String>,
    json: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct PreviewInput {
    variables: Option<HashMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
struct TestSendInput {
    to: String,
    variables: Option<HashMap<String, Value>>,
}

fn validate_name(name: &str) -> UnifiedResult<()> {
    if name.trim().is_empty() {
        return Err(UnifiedError::bad_request("Template name must not be empty"));
    }
    Ok(())
}

fn template_response(template: &EmailTemplate) -> HttpResponse {
    api_json::success_response(template, 200)
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

async fn list_templates(
    state: web::Data<Arc<EmailTemplateState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    list_filtered(&state, &query, None)
}

async fn list_by_category(
    state: web::Data<Arc<EmailTemplateState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    list_filtered(&state, &query, Some(path.into_inner()))
}

fn list_filtered(
    state: &EmailTemplateState,
    query: &PaginationParams,
    category: Option<String>,
) -> HttpResponse {
    let pagination = query.normalized();
    let mut templates: Vec<EmailTemplate> = state
        .templates
        .read()
        .expect("template lock poisoned")
        .values()
        .cloned()
        .collect();
    if let Some(category) = &category {
        templates.retain(|template| template.category == *category);
    }
    if let Some(filter_category) = pagination.filters.get("category") {
        templates.retain(|template| template.category == *filter_category);
    }
    if let Some(search) = &pagination.search {
        let needle = search.to_lowercase();
        templates.retain(|template| {
            template.name.to_lowercase().contains(&needle)
                || template.subject.to_lowercase().contains(&needle)
        });
    }
    let sort = query.allowed_sort(&["name", "category", "created_at", "updated_at"], "created_at");
    match sort.as_str() {
        "name" => templates.sort_by(|a, b| a.name.cmp(&b.name)),
        "category" => templates.sort_by(|a, b| a.category.cmp(&b.category)),
        "updated_at" => templates.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
        _ => templates.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if pagination.order == "desc" {
        templates.reverse();
    }
    let total = templates.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<EmailTemplate> = templates
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn create_template(
    state: web::Data<Arc<EmailTemplateState>>,
    body: web::Json<TemplateInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    if let Err(error) = validate_name(&input.name) {
        return api_json::error_response(&error, debug);
    }
    if input.subject.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("Template subject must not be empty"),
            debug,
        );
    }
    let now = Utc::now().to_rfc3339();
    let template = EmailTemplate {
        id: Uuid::now_v7().to_string(),
        name: input.name.trim().to_string(),
        subject: input.subject,
        category: input
            .category
            .unwrap_or_else(|| "general".to_string())
            .trim()
            .to_string(),
        html: input.html,
        json: input.json.unwrap_or(Value::Null),
        version: 1,
        versions: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    let mut template = template;
    template.versions.push(template.snapshot());
    {
        let mut templates = state.templates.write().expect("template lock poisoned");
        templates.insert(template.id.clone(), template.clone());
    }
    state.emit(
        "template.created",
        json!({ "template_id": template.id, "name": template.name }),
    );
    api_json::success_response(template, 201)
}

async fn get_template(
    state: web::Data<Arc<EmailTemplateState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match state.get(&path.into_inner()) {
        Some(template) => template_response(&template),
        None => api_json::error_response(&UnifiedError::not_found("Email template not found"), debug),
    }
}

async fn update_template(
    state: web::Data<Arc<EmailTemplateState>>,
    path: web::Path<String>,
    body: web::Json<TemplateUpdateInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = path.into_inner();
    let input = body.into_inner();
    if let Some(name) = &input.name {
        if let Err(error) = validate_name(name) {
            return api_json::error_response(&error, debug);
        }
    }
    let mut templates = state.templates.write().expect("template lock poisoned");
    let template = match templates.get_mut(&id) {
        Some(template) => template,
        None => {
            return api_json::error_response(
                &UnifiedError::not_found("Email template not found"),
                debug,
            )
        }
    };
    if let Some(name) = input.name {
        template.name = name.trim().to_string();
    }
    if let Some(subject) = input.subject {
        template.subject = subject;
    }
    if let Some(category) = input.category {
        template.category = category.trim().to_string();
    }
    if let Some(html) = input.html {
        template.html = html;
    }
    if let Some(json) = input.json {
        template.json = json;
    }
    // Every update creates a new version.
    template.version += 1;
    template.updated_at = Utc::now().to_rfc3339();
    template.versions.push(template.snapshot());
    let template = template.clone();
    drop(templates);
    state.emit(
        "template.updated",
        json!({ "template_id": template.id, "version": template.version }),
    );
    template_response(&template)
}

async fn delete_template(
    state: web::Data<Arc<EmailTemplateState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = path.into_inner();
    let removed = state
        .templates
        .write()
        .expect("template lock poisoned")
        .remove(&id);
    match removed {
        Some(template) => {
            state.emit("template.deleted", json!({ "template_id": id }));
            api_json::success_response(json!({ "deleted": true, "id": template.id }), 200)
        }
        None => api_json::error_response(&UnifiedError::not_found("Email template not found"), debug),
    }
}

async fn list_versions(
    state: web::Data<Arc<EmailTemplateState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match state.get(&path.into_inner()) {
        Some(template) => api_json::success_response(
            json!({ "template_id": template.id, "version": template.version, "versions": template.versions }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("Email template not found"), debug),
    }
}

async fn restore_version(
    state: web::Data<Arc<EmailTemplateState>>,
    path: web::Path<(String, u32)>,
) -> HttpResponse {
    let debug = state.is_debug();
    let (id, version) = path.into_inner();
    let mut templates = state.templates.write().expect("template lock poisoned");
    let template = match templates.get_mut(&id) {
        Some(template) => template,
        None => {
            return api_json::error_response(
                &UnifiedError::not_found("Email template not found"),
                debug,
            )
        }
    };
    let snapshot = match template.versions.iter().find(|v| v.version == version) {
        Some(snapshot) => snapshot.clone(),
        None => {
            return api_json::error_response(
                &UnifiedError::not_found(format!("Template version {version} not found")),
                debug,
            )
        }
    };
    template.subject = snapshot.subject;
    template.html = snapshot.html;
    template.json = snapshot.json;
    // Restoring creates a new version, so the restore itself is versioned.
    template.version += 1;
    template.updated_at = Utc::now().to_rfc3339();
    template.versions.push(template.snapshot());
    let template = template.clone();
    drop(templates);
    state.emit(
        "template.restored",
        json!({ "template_id": template.id, "restored_version": version, "version": template.version }),
    );
    template_response(&template)
}

async fn list_variables(
    state: web::Data<Arc<EmailTemplateState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    match state.get(&path.into_inner()) {
        Some(template) => {
            let mut vars = extract_variables(&template.html);
            vars.extend(extract_variables(&template.subject));
            if !template.json.is_null() {
                vars.extend(extract_variables(&serde_json::to_string(&template.json).unwrap_or_default()));
            }
            vars.sort();
            vars.dedup();
            api_json::success_response(
                json!({ "template_id": template.id, "variables": vars }),
                200,
            )
        }
        None => api_json::error_response(&UnifiedError::not_found("Email template not found"), debug),
    }
}

async fn preview_template(
    state: web::Data<Arc<EmailTemplateState>>,
    path: web::Path<String>,
    body: web::Json<PreviewInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let variables = body.into_inner().variables.unwrap_or_default();
    match state.get(&path.into_inner()) {
        Some(template) => {
            let (rendered_html, subject, missing) =
                EmailTemplateState::render(&template, &variables);
            api_json::success_response(
                json!({
                    "template_id": template.id,
                    "rendered_html": rendered_html,
                    "subject": subject,
                    "missing_variables": missing
                }),
                200,
            )
        }
        None => api_json::error_response(&UnifiedError::not_found("Email template not found"), debug),
    }
}

async fn test_send_template(
    state: web::Data<Arc<EmailTemplateState>>,
    path: web::Path<String>,
    body: web::Json<TestSendInput>,
) -> HttpResponse {
    let state: Arc<EmailTemplateState> = state.into_inner();
    let debug = state.is_debug();
    let input = body.into_inner();
    let template = match state.get(&path.into_inner()) {
        Some(template) => template,
        None => {
            return api_json::error_response(
                &UnifiedError::not_found("Email template not found"),
                debug,
            )
        }
    };
    if input.to.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("Recipient 'to' must not be empty"),
            debug,
        );
    }
    let variables = input.variables.clone().unwrap_or_default();
    let (rendered_html, subject, missing) = EmailTemplateState::render(&template, &variables);
    let hooks = match state.hooks() {
        Some(hooks) => hooks,
        None => {
            return api_json::error_response(
                &UnifiedError::new("Mail service is unavailable (no hook bus)", 503),
                debug,
            )
        }
    };
    let answer = hooks.emit(
        EVENT_MAIL_SEND_REQUESTED,
        json!({
            "to": input.to,
            "subject": subject,
            "body_html": rendered_html,
            "template_id": template.id,
            "variables": variables,
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("error").is_none() => api_json::success_response(
            json!({
                "queued": result.get("queued").cloned().unwrap_or(Value::Bool(true)),
                "job_id": result.get("job_id").cloned().unwrap_or(Value::Null),
                "via": result.get("via").cloned().unwrap_or(Value::Null),
                "missing_variables": missing
            }),
            200,
        ),
        Some(result) => {
            let reason = result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("mail service rejected the request");
            api_json::error_response(
                &UnifiedError::new(format!("Mail service rejected the request: {reason}"), 502),
                debug,
            )
        }
        None => api_json::error_response(
            &UnifiedError::new(
                "Mail service did not answer the mail.send_requested event: no mail handler is registered",
                503,
            ),
            debug,
        ),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct EmailTemplatesModule {
    state: Arc<EmailTemplateState>,
}

impl EmailTemplatesModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(EmailTemplateState::new()),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(EmailTemplatesModule::new()));
}

impl Module for EmailTemplatesModule {
    fn id(&self) -> &'static str {
        "email-templates"
    }

    fn name(&self) -> &'static str {
        "Email Template Builder"
    }

    fn description(&self) -> &'static str {
        "GrapeJS email templates with versioning, placeholder variables, preview rendering, and test sending through the Mail Manager."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["mail"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Rendering service used by the Mail Manager (and any other module)
        // when a template_id is supplied for sending.
        let state = self.state.clone();
        bus.register(
            EVENT_TEMPLATE_RENDER_REQUESTED,
            "email-templates",
            Arc::new(move |ctx| {
                let template_id = ctx
                    .get("template_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let variables: HashMap<String, Value> = ctx
                    .get("variables")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                match state.get(&template_id) {
                    Some(template) => {
                        let (rendered_html, subject, missing) =
                            EmailTemplateState::render(&template, &variables);
                        ctx.add_result(json!({
                            "template_id": template_id,
                            "rendered_html": rendered_html,
                            "subject": subject,
                            "missing_variables": missing
                        }));
                    }
                    None => {
                        ctx.add_result(json!({
                            "error": format!("Email template not found: {template_id}")
                        }));
                    }
                }
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/email-templates/by-category/{category}", owner: "email-templates", description: "List by category" },
            RouteInfo { method: "GET", path: "/api/v1/email-templates", owner: "email-templates", description: "List templates" },
            RouteInfo { method: "POST", path: "/api/v1/email-templates", owner: "email-templates", description: "Create template" },
            RouteInfo { method: "GET", path: "/api/v1/email-templates/{id}", owner: "email-templates", description: "Get template" },
            RouteInfo { method: "PUT", path: "/api/v1/email-templates/{id}", owner: "email-templates", description: "Update template" },
            RouteInfo { method: "DELETE", path: "/api/v1/email-templates/{id}", owner: "email-templates", description: "Delete template" },
            RouteInfo { method: "GET", path: "/api/v1/email-templates/{id}/versions", owner: "email-templates", description: "List versions" },
            RouteInfo { method: "POST", path: "/api/v1/email-templates/{id}/restore/{version}", owner: "email-templates", description: "Restore version" },
            RouteInfo { method: "POST", path: "/api/v1/email-templates/{id}/variables", owner: "email-templates", description: "List variables" },
            RouteInfo { method: "POST", path: "/api/v1/email-templates/{id}/preview", owner: "email-templates", description: "Preview template" },
            RouteInfo { method: "POST", path: "/api/v1/email-templates/{id}/test-send", owner: "email-templates", description: "Test send template" },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/email-templates")
                .app_data(web::Data::new(state))
                // Static segment first so `/by-category/{category}` is
                // never captured by the `/{id}` routes below.
                .route("/by-category/{category}", web::get().to(list_by_category))
                .route("", web::get().to(list_templates))
                .route("", web::post().to(create_template))
                .route("/{id}", web::get().to(get_template))
                .route("/{id}", web::put().to(update_template))
                .route("/{id}", web::delete().to(delete_template))
                .route("/{id}/versions", web::get().to(list_versions))
                .route("/{id}/restore/{version}", web::post().to(restore_version))
                .route("/{id}/variables", web::post().to(list_variables))
                .route("/{id}/preview", web::post().to(preview_template))
                .route("/{id}/test-send", web::post().to(test_send_template)),
        );
    }
}
