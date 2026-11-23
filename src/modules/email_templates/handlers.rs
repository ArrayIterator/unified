//! email templates - handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json as api_json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;
use super::render::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Caller identity and capability checks (calculator pattern: the
// verified user id identifies the actor, the server-resolved role
// carries the role slug the capability check runs against,
// `role.check` hook, fail closed).
// Every handler below is gated: reads need
// `email_templates:read`, every mutation and test-send needs
// `email_templates:manage`. Without this gate anyone could rewrite
// or delete templates, and test-send would be an unauthenticated
// mail relay to an arbitrary address.
// ---------------------------------------------------------------------------

/// The server-verified caller (see `unified_core::caller::Caller`).
/// No verified caller means an anonymous caller, rejected before
/// any capability is consulted; only the role is needed for the
/// capability check itself.
pub(crate) struct Caller {
    role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let role = verified.role().to_string();
    Ok(Caller { role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Authorize one request: caller identity + one capability.
pub(crate) fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(ctx, &found.role, permission)?;
    Ok(found)
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, task 316).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before caller identity or any capability
/// check.
///
/// The records in this module carry no company field, so the guard
/// is a fail-closed gate only: real per-record partitioning needs a
/// `tenant_id` on the record (C11 follow-up), not a filter here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified),
    }
}

/// Fail closed (403) when no company resolves for a mutating
/// handler: the tenant context's company first, the verified
/// caller's server-resolved company otherwise. An unresolved caller
/// writes nothing (the shared `CallerScope::Unresolved` rule) — the
/// records in this module carry no company field to stamp, so there
/// is no global-row write to fall back to.
pub(crate) fn require_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?.ok_or_else(|| {
        UnifiedError::forbidden(
            "No company scope resolved for this request: writes require a resolved company",
        )
    })
}

/// Maximum template name length, in characters after trimming.
pub(crate) const MAX_TEMPLATE_NAME_LEN: usize = 200;

pub(crate) fn validate_name(name: &str) -> UnifiedResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request("Template name must not be empty"));
    }
    if trimmed.chars().count() > MAX_TEMPLATE_NAME_LEN {
        return Err(UnifiedError::bad_request(format!(
            "Template name must be at most {MAX_TEMPLATE_NAME_LEN} characters"
        )));
    }
    Ok(())
}

/// Shape check for a single recipient address (`local@domain.tld`,
/// no whitespace): the same shape the mail providers module enforces
/// in `validate_email_address`, plus an explicit whitespace rejection
/// so junk like `a b@c.com` never reaches the mail service.
pub(crate) fn valid_email_address(address: &str) -> bool {
    let address = address.trim();
    if address.is_empty() || address.chars().any(char::is_whitespace) {
        return false;
    }
    let mut parts = address.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.split('.').any(str::is_empty)
}

/// True when another template in the same category already uses
/// `name` (case-insensitive, trimmed), so confusing near-duplicate
/// templates cannot accumulate. `exclude_id` skips the template
/// being updated, so keeping its own name is not a conflict.
/// Takes the map the caller already holds, so no second lock is
/// taken while a write lock is held.
pub(crate) fn name_exists_in_category(
    templates: &HashMap<String, EmailTemplate>,
    name: &str,
    category: &str,
    exclude_id: Option<&str>,
) -> bool {
    let name = name.trim().to_lowercase();
    let category = category.trim();
    templates.values().any(|template| {
        exclude_id != Some(template.id.as_str())
            && template.category == category
            && template.name.trim().to_lowercase() == name
    })
}

pub(crate) fn template_response(template: &EmailTemplate) -> HttpResponse {
    api_json::success_response(template, 200)
}

pub(crate) async fn list_templates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EmailTemplateState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_EMAIL_TEMPLATES_READ) {
        return api_json::error_response(&error, debug);
    }
    list_filtered(&state, &query, None)
}

pub(crate) async fn list_by_category(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EmailTemplateState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_EMAIL_TEMPLATES_READ) {
        return api_json::error_response(&error, debug);
    }
    list_filtered(&state, &query, Some(path.into_inner()))
}

pub(crate) fn list_filtered(
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

pub(crate) async fn create_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EmailTemplateState>>,
    req: HttpRequest,
    body: web::Json<TemplateInput>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_EMAIL_TEMPLATES_MANAGE) {
        return api_json::error_response(&error, debug);
    }
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
    let name = input.name.trim().to_string();
    let category = input
        .category
        .unwrap_or_else(|| "general".to_string())
        .trim()
        .to_string();
    let now = Utc::now().to_rfc3339();
    let template = EmailTemplate {
        id: Uuid::now_v7().to_string(),
        name,
        subject: input.subject,
        category,
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
        if name_exists_in_category(&templates, &template.name, &template.category, None) {
            return api_json::error_response(
                &UnifiedError::conflict(format!(
                    "A template named \"{}\" already exists in category \"{}\"",
                    template.name, template.category
                )),
                debug,
            );
        }
        templates.insert(template.id.clone(), template.clone());
    }
    state.emit(
        "template.created",
        json!({ "template_id": template.id, "name": template.name }),
    );
    api_json::success_response(template, 201)
}

pub(crate) async fn get_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EmailTemplateState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_EMAIL_TEMPLATES_READ) {
        return api_json::error_response(&error, debug);
    }
    match state.get(&path.into_inner()) {
        Some(template) => template_response(&template),
        None => api_json::error_response(&UnifiedError::not_found("Email template not found"), debug),
    }
}

pub(crate) async fn update_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EmailTemplateState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TemplateUpdateInput>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_EMAIL_TEMPLATES_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = path.into_inner();
    let input = body.into_inner();
    if let Some(name) = &input.name {
        if let Err(error) = validate_name(name) {
            return api_json::error_response(&error, debug);
        }
    }
    let mut templates = state.templates.write().expect("template lock poisoned");
    let (current_name, current_category) = match templates.get(&id) {
        Some(template) => (template.name.clone(), template.category.clone()),
        None => {
            return api_json::error_response(
                &UnifiedError::not_found("Email template not found"),
                debug,
            )
        }
    };
    // The rename (or re-categorisation) must not collide with a
    // different template's (name, category) pair.
    let new_name = input.name.as_deref().map(str::trim).unwrap_or(&current_name);
    let new_category = input
        .category
        .as_deref()
        .map(str::trim)
        .unwrap_or(&current_category);
    if name_exists_in_category(&templates, new_name, new_category, Some(&id)) {
        return api_json::error_response(
            &UnifiedError::conflict(format!(
                "A template named \"{new_name}\" already exists in category \"{new_category}\""
            )),
            debug,
        );
    }
    let template = templates.get_mut(&id).expect("template checked above");
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

pub(crate) async fn delete_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EmailTemplateState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_EMAIL_TEMPLATES_MANAGE) {
        return api_json::error_response(&error, debug);
    }
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

pub(crate) async fn list_versions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EmailTemplateState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_EMAIL_TEMPLATES_READ) {
        return api_json::error_response(&error, debug);
    }
    match state.get(&path.into_inner()) {
        Some(template) => api_json::success_response(
            json!({ "template_id": template.id, "version": template.version, "versions": template.versions }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("Email template not found"), debug),
    }
}

pub(crate) async fn restore_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EmailTemplateState>>,
    req: HttpRequest,
    path: web::Path<(String, u32)>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_EMAIL_TEMPLATES_MANAGE) {
        return api_json::error_response(&error, debug);
    }
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

pub(crate) async fn list_variables(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EmailTemplateState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_EMAIL_TEMPLATES_READ) {
        return api_json::error_response(&error, debug);
    }
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

pub(crate) async fn preview_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EmailTemplateState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PreviewInput>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_EMAIL_TEMPLATES_READ) {
        return api_json::error_response(&error, debug);
    }
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

pub(crate) async fn test_send_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<EmailTemplateState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TestSendInput>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let _company = match require_company(&_tenant, &req) {
        Ok(company) => company,
        Err(error) => return api_json::error_response(&error, state.is_debug()),
    };
    let debug = state.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_EMAIL_TEMPLATES_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let state: Arc<EmailTemplateState> = state.get_ref().clone();
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
    let to = input.to.trim().to_string();
    if !valid_email_address(&to) {
        return api_json::error_response(
            &UnifiedError::bad_request("Recipient 'to' must be a valid email address"),
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
            "to": to,
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

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn test_state() -> Arc<EmailTemplateState> {
        Arc::new(EmailTemplateState::new())
    }

    fn state_data(state: Arc<EmailTemplateState>) -> web::Data<Arc<EmailTemplateState>> {
        web::Data::new(state)
    }

    /// Context with no `role.check` handler: every capability check
    /// fails closed.
    fn ungated_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// Context whose `role.check` hook always answers allowed, so the
    /// authorized path of each handler can be exercised.
    fn allowing_context() -> AppContext {
        let bus = Arc::new(HookBus::new());
        bus.register(
            "role.check",
            "test",
            Arc::new(|ctx| {
                ctx.set("allowed", Value::Bool(true));
            }),
        );
        AppContext::new(AppConfig::default(), bus, Arc::new(ModuleRegistry::new()))
    }

    fn ctx_data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn anonymous_request() -> HttpRequest {
        TestRequest::default().to_http_request()
    }

    fn template_input(name: &str, category: &str) -> web::Json<TemplateInput> {
        web::Json(
            serde_json::from_value(json!({
                "name": name,
                "subject": "Hello",
                "category": category,
                "html": "<p>Hi {{ name }}</p>",
            }))
            .expect("test template input must deserialize"),
        )
    }

    fn update_input(value: Value) -> web::Json<TemplateUpdateInput> {
        web::Json(serde_json::from_value(value).expect("test update input must deserialize"))
    }

    fn test_send_input(to: &str) -> web::Json<TestSendInput> {
        web::Json(
            serde_json::from_value(json!({ "to": to }))
                .expect("test send input must deserialize"),
        )
    }

    fn seed_template(state: &EmailTemplateState, name: &str, category: &str) -> String {
        let now = Utc::now().to_rfc3339();
        let template = EmailTemplate {
            id: Uuid::now_v7().to_string(),
            name: name.to_string(),
            subject: "Hello".to_string(),
            category: category.to_string(),
            html: "<p>Hi</p>".to_string(),
            json: Value::Null,
            version: 1,
            versions: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        };
        let id = template.id.clone();
        state
            .templates
            .write()
            .expect("template lock poisoned")
            .insert(id.clone(), template);
        id
    }

    #[test]
    fn validate_name_rejects_empty_and_blank_names() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
    }

    #[test]
    fn validate_name_accepts_a_plain_name() {
        assert!(validate_name("Welcome").is_ok());
    }

    #[test]
    fn validate_name_accepts_names_with_spaces() {
        assert!(validate_name("Welcome email").is_ok());
    }

    #[test]
    fn validate_name_rejects_names_over_200_characters() {
        assert!(validate_name(&"a".repeat(201)).is_err());
        assert!(validate_name(&"a".repeat(200)).is_ok());
    }

    #[test]
    fn valid_email_address_accepts_well_formed_addresses() {
        assert!(valid_email_address("user@example.com"));
        assert!(valid_email_address("first.last+tag@mail.example.co.id"));
        assert!(valid_email_address("  user@example.com  "));
    }

    #[test]
    fn valid_email_address_rejects_malformed_addresses() {
        for bad in [
            "",
            "plain",
            "a@@b.com",
            "a@b",
            "a b@c.com",
            "@example.com",
            "a@.b.co",
            "a@b..com",
            "a@b.com.",
            "a b@c.com",
        ] {
            assert!(!valid_email_address(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn caller_rejects_requests_without_a_verified_caller() {
        let no_user = TestRequest::default().to_http_request();
        assert!(caller(&no_user).is_err());
        let no_role = TestRequest::default().to_http_request();
        assert!(caller(&no_role).is_err());
        let blank_user = TestRequest::default().to_http_request();
        assert!(caller(&blank_user).is_err());
        let found = caller(&authed_request()).unwrap();
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn check_capability_fails_closed_without_a_role_check_handler() {
        let ctx = ungated_context();
        assert!(!check_capability(&ctx, "admin", PERM_EMAIL_TEMPLATES_MANAGE));
        assert!(!check_capability(&ctx, "admin", PERM_EMAIL_TEMPLATES_READ));
        assert!(require_permission(&ctx, "admin", PERM_EMAIL_TEMPLATES_MANAGE).is_err());
    }

    #[test]
    fn require_permission_allows_when_role_check_answers_allowed() {
        let ctx = allowing_context();
        assert!(require_permission(&ctx, "admin", PERM_EMAIL_TEMPLATES_MANAGE).is_ok());
        assert!(require_permission(&ctx, "admin", PERM_EMAIL_TEMPLATES_READ).is_ok());
    }

    #[tokio::test]
    async fn list_templates_rejects_anonymous_callers() {
        let response = list_templates(
            ctx_data(ungated_context()),
            state_data(test_state()),
            anonymous_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_template_rejects_anonymous_callers() {
        let response = get_template(
            ctx_data(ungated_context()),
            state_data(test_state()),
            anonymous_request(),
            web::Path::from("missing".to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_template_rejects_anonymous_callers() {
        let response = create_template(
            ctx_data(ungated_context()),
            state_data(test_state()),
            anonymous_request(),
            template_input("Welcome", "general"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn update_template_rejects_anonymous_callers() {
        let state = test_state();
        let id = seed_template(&state, "Welcome", "general");
        let response = update_template(
            ctx_data(ungated_context()),
            state_data(state),
            anonymous_request(),
            web::Path::from(id),
            update_input(json!({ "subject": "Changed" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn delete_template_rejects_anonymous_callers() {
        let state = test_state();
        let id = seed_template(&state, "Welcome", "general");
        let response = delete_template(
            ctx_data(ungated_context()),
            state_data(state),
            anonymous_request(),
            web::Path::from(id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn restore_version_rejects_anonymous_callers() {
        let state = test_state();
        let id = seed_template(&state, "Welcome", "general");
        let response = restore_version(
            ctx_data(ungated_context()),
            state_data(state),
            anonymous_request(),
            web::Path::from((id, 1u32)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_send_template_rejects_anonymous_callers() {
        let state = test_state();
        let id = seed_template(&state, "Welcome", "general");
        let response = test_send_template(
            ctx_data(ungated_context()),
            state_data(state),
            anonymous_request(),
            web::Path::from(id),
            test_send_input("user@example.com"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_templates_allows_authorized_readers() {
        let response = list_templates(
            ctx_data(allowing_context()),
            state_data(test_state()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn create_template_rejects_a_duplicate_name_in_the_same_category() {
        let state = test_state();
        let first = create_template(
            ctx_data(allowing_context()),
            state_data(state.clone()),
            authed_request(),
            template_input("Welcome", "general"),
        )
        .await;
        assert_eq!(first.status(), StatusCode::CREATED);
        let duplicate = create_template(
            ctx_data(allowing_context()),
            state_data(state.clone()),
            authed_request(),
            template_input("welcome", "general"),
        )
        .await;
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn create_template_allows_the_same_name_in_another_category() {
        let state = test_state();
        let first = create_template(
            ctx_data(allowing_context()),
            state_data(state.clone()),
            authed_request(),
            template_input("Welcome", "general"),
        )
        .await;
        assert_eq!(first.status(), StatusCode::CREATED);
        let other_category = create_template(
            ctx_data(allowing_context()),
            state_data(state.clone()),
            authed_request(),
            template_input("Welcome", "billing"),
        )
        .await;
        assert_eq!(other_category.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_template_rejects_an_overlong_name() {
        let response = create_template(
            ctx_data(allowing_context()),
            state_data(test_state()),
            authed_request(),
            template_input(&"a".repeat(201), "general"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_template_rejects_a_rename_into_a_duplicate() {
        let state = test_state();
        seed_template(&state, "Welcome", "general");
        let other = seed_template(&state, "Goodbye", "general");
        let response = update_template(
            ctx_data(allowing_context()),
            state_data(state),
            authed_request(),
            web::Path::from(other),
            update_input(json!({ "name": "welcome" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn update_template_allows_keeping_its_own_name() {
        let state = test_state();
        let id = seed_template(&state, "Welcome", "general");
        let response = update_template(
            ctx_data(allowing_context()),
            state_data(state),
            authed_request(),
            web::Path::from(id),
            update_input(json!({ "name": "Welcome", "subject": "Changed" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_send_template_rejects_malformed_recipients() {
        let state = test_state();
        let id = seed_template(&state, "Welcome", "general");
        for bad in [
            "",
            "plain",
            "a@@b.com",
            "a@b",
            "a b@c.com",
            "@example.com",
            "a@.b.co",
            "a@b..com",
        ] {
            let response = test_send_template(
                ctx_data(allowing_context()),
                state_data(state.clone()),
                authed_request(),
                web::Path::from(id.clone()),
                test_send_input(bad),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad:?} must be rejected");
        }
    }

    #[tokio::test]
    async fn test_send_template_passes_a_valid_recipient_to_the_mail_service_check() {
        let state = test_state();
        let id = seed_template(&state, "Welcome", "general");
        // No hook bus is attached to the test state, so a well-formed
        // recipient gets past validation and fails downstream with 503
        // (mail service unavailable) instead of 400.
        let response = test_send_template(
            ctx_data(allowing_context()),
            state_data(state),
            authed_request(),
            web::Path::from(id),
            test_send_input("user@example.com"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(tenant.company_id.as_deref(), Some("company-1"));
        let company = caller_company(&tenant, &req).expect("company must resolve");
        assert_eq!(company.as_deref(), Some("company-1"));
        assert_eq!(
            require_company(&tenant, &req).expect("company must resolve"),
            "company-1"
        );
    }

    #[test]
    fn require_company_fails_closed_without_a_resolved_company() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(tenant.company_id.is_none());
        assert!(
            caller_company(&tenant, &req)
                .expect("mismatch check must pass")
                .is_none()
        );
        assert!(require_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_refuses_a_verified_company_mismatch() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                .with_company("company-2".to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }
}
