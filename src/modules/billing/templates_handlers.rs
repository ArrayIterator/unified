//! billing - templates handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use super::types::*;
use super::state::*;
use super::helpers::*;
use super::templates::*;

/// Thermal (receipt) lines for an invoice, as JSON values shaped
/// like the printing module's `ThermalLine` records. Used when an
/// invoice is printed to a thermal printer.
pub(crate) fn invoice_thermal_lines(invoice: &Invoice) -> Value {
    let mut lines: Vec<Value> = vec![
        json!({ "text": format!("INVOICE {}", invoice.number), "align": "center", "bold": true, "double_size": true }),
        json!({ "text": format!("Status: {}", invoice.status), "align": "left", "bold": false, "double_size": false }),
        json!({ "text": format!("Customer: {}", invoice.user_id), "align": "left", "bold": false, "double_size": false }),
        json!({ "text": "----------------------------------------", "align": "left", "bold": false, "double_size": false }),
    ];
    for item in &invoice.items {
        lines.push(json!({
            "text": format!("{} x{}  {:.2}", item.name, item.quantity, minor_to_major(item.line_total, &invoice.currency)),
            "align": "left", "bold": false, "double_size": false,
        }));
    }
    lines.push(json!({ "text": "----------------------------------------", "align": "left", "bold": false, "double_size": false }));
    lines.push(json!({ "text": format!("Subtotal: {:.2} {}", minor_to_major(invoice.subtotal, &invoice.currency), invoice.currency), "align": "right", "bold": false, "double_size": false }));
    lines.push(json!({ "text": format!("Tax: {:.2} {}", minor_to_major(invoice.tax_total, &invoice.currency), invoice.currency), "align": "right", "bold": false, "double_size": false }));
    lines.push(json!({ "text": format!("TOTAL: {:.2} {}", minor_to_major(invoice.total, &invoice.currency), invoice.currency), "align": "right", "bold": true, "double_size": false }));
    if let Some(due) = invoice.due_date {
        lines.push(json!({ "text": format!("Due: {}", due.to_rfc3339()), "align": "left", "bold": false, "double_size": false }));
    }
    Value::Array(lines)
}

/// Validate an explicit paper size in millimetres: 40mm to
/// 1200mm per side.
pub(crate) fn validate_paper_dimensions(width_mm: f64, height_mm: f64) -> UnifiedResult<()> {
    if !width_mm.is_finite() || !(40.0..=1200.0).contains(&width_mm) {
        return Err(UnifiedError::bad_request(
            "Paper width must be between 40mm and 1200mm",
        ));
    }
    if !height_mm.is_finite() || !(40.0..=1200.0).contains(&height_mm) {
        return Err(UnifiedError::bad_request(
            "Paper height must be between 40mm and 1200mm",
        ));
    }
    Ok(())
}

/// A plain email address check: one `@`, non-empty local and
/// domain parts, a dot in the domain, no whitespace.
pub(crate) fn valid_email(value: &str) -> bool {
    if value.len() < 3 || value.len() > 254 || value.chars().any(char::is_whitespace) {
        return false;
    }
    let mut parts = value.split('@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    parts.next().is_none() && !local.is_empty() && domain.contains('.') && !domain.is_empty()
}

pub(crate) async fn list_invoice_templates(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<InvoiceTemplate>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<InvoiceTemplate> = state
            .invoice_templates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|template| {
                in_company_scope(&template.company_id, &_scope)
                    && matches_search(&format!("{} {}", template.name, template.id), &params)
                    && params.filters.get("company_id").is_none_or(|company_id| {
                        template.company_id.as_deref() == Some(company_id.as_str())
                            || template.company_id.is_none()
                    })
                    && params
                        .filters
                        .get("is_default")
                        .is_none_or(|flag| template.is_default.to_string() == *flag)
                    && params
                        .filters
                        .get("paper_size_code")
                        .is_none_or(|code| template.paper_size_code == *code)
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn create_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateInvoiceTemplate>,
) -> HttpResponse {
    let result: UnifiedResult<InvoiceTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_MANAGE)?;
        let input = body.into_inner();
        if input.name.trim().is_empty() || input.name.len() > 120 {
            return Err(UnifiedError::bad_request(
                "Template name must be 1 to 120 characters",
            ));
        }
        let company_id = require_company_match(&_tenant, &req, &input.company_id)?;
        let paper_size_code = input
            .paper_size_code
            .clone()
            .unwrap_or_else(|| "a4".to_string());
        if !valid_paper_size_code(&paper_size_code) {
            return Err(UnifiedError::bad_request(
                "Paper size code must be a lowercase slug",
            ));
        }
        if let Some(color) = &input.primary_color {
            if !valid_hex_color(color) {
                return Err(UnifiedError::bad_request(
                    "Primary colour must be a #RGB or #RRGGBB hex colour",
                ));
            }
        }
        if let Some(color) = &input.accent_color {
            if !valid_hex_color(color) {
                return Err(UnifiedError::bad_request(
                    "Accent colour must be a #RGB or #RRGGBB hex colour",
                ));
            }
        }
        if let Some(font) = &input.font_family {
            if !valid_font_family(font) {
                return Err(UnifiedError::bad_request(
                    "Font family contains characters that are not allowed in a style block",
                ));
            }
        }
        validate_optional_text(&input.header_text, 5000, "Header text")?;
        validate_optional_text(&input.footer_text, 5000, "Footer text")?;
        validate_optional_text(
            &input.payment_instructions,
            5000,
            "Payment instructions",
        )?;
        validate_optional_text(&input.footer_notes, 5000, "Footer notes")?;
        validate_logo_url(&input.logo_url)?;
        for (label, value) in [
            ("Logo storage target", &input.logo_storage_target),
            ("Logo storage key", &input.logo_storage_key),
        ] {
            if let Some(text) = value {
                if text.trim().is_empty() || text.len() > 255 {
                    return Err(UnifiedError::bad_request(format!(
                        "{label} must be 1 to 255 characters"
                    )));
                }
            }
        }
        let columns = match input.columns {
            Some(columns) => validate_columns(&columns)?,
            None => default_template_columns(),
        };
        if let Some(layout) = &input.line_item_layout {
            if !layout.is_object() || layout.to_string().len() > 8000 {
                return Err(UnifiedError::bad_request(
                    "Line-item layout must be a JSON object of at most 8000 characters",
                ));
            }
        }
        let moment = now();
        let template = InvoiceTemplate {
            id: new_id(),
            company_id,
            name: input.name.trim().to_string(),
            is_default: false,
            paper_size_code,
            header_text: input.header_text,
            footer_text: input.footer_text,
            logo_storage_target: input.logo_storage_target,
            logo_storage_key: input.logo_storage_key,
            logo_url: input.logo_url,
            primary_color: input
                .primary_color
                .unwrap_or_else(|| "#111827".to_string()),
            accent_color: input
                .accent_color
                .unwrap_or_else(|| "#2563EB".to_string()),
            font_family: input
                .font_family
                .unwrap_or_else(|| "Arial, sans-serif".to_string()),
            columns,
            line_item_layout: input.line_item_layout.unwrap_or_else(|| json!({})),
            show_tax_breakdown: input.show_tax_breakdown.unwrap_or(true),
            payment_instructions: input.payment_instructions,
            footer_notes: input.footer_notes,
            version: 1,
            created_by: Some(caller.actor.clone()),
            created_at: moment,
            updated_at: moment,
        };
        state
            .invoice_templates
            .insert(template.id.clone(), template.clone());
        archive_template_version(&state, &template, &caller.actor);
        state.emit(
            EVENT_INVOICE_TEMPLATE_CHANGED,
            json!({
                "template_id": template.id.clone(),
                "company_id": template.company_id.clone(),
                "version": template.version,
                "action": "created",
                "updated_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    match result {
        Ok(template) => json::success_response(template, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<InvoiceTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_VIEW)?;
        let template = state
            .invoice_templates
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        require_record_in_company_scope(&template.company_id, &_scope)?;
        Ok(template)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateInvoiceTemplate>,
) -> HttpResponse {
    let result: UnifiedResult<InvoiceTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_MANAGE)?;
        let id = path.into_inner();
        let mut template = state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        require_record_writable_in_company_scope(&template.company_id, &_scope)?;
        // Archive the pre-edit shape first: the version row must
        // hold the template as it was before this update.
        archive_template_version(&state, &template, &caller.actor);
        let input = body.into_inner();
        if let Some(name) = input.name {
            if name.trim().is_empty() || name.len() > 120 {
                return Err(UnifiedError::bad_request(
                    "Template name must be 1 to 120 characters",
                ));
            }
            template.name = name.trim().to_string();
        }
        if let Some(company_id) = input.company_id {
            template.company_id = require_company_change(&_scope, &company_id)?;
        }
        if let Some(code) = input.paper_size_code {
            if !valid_paper_size_code(&code) {
                return Err(UnifiedError::bad_request(
                    "Paper size code must be a lowercase slug",
                ));
            }
            template.paper_size_code = code;
        }
        if let Some(header_text) = input.header_text {
            validate_optional_text(&header_text, 5000, "Header text")?;
            template.header_text = header_text;
        }
        if let Some(footer_text) = input.footer_text {
            validate_optional_text(&footer_text, 5000, "Footer text")?;
            template.footer_text = footer_text;
        }
        if let Some(target) = input.logo_storage_target {
            if let Some(text) = &target {
                if text.trim().is_empty() || text.len() > 255 {
                    return Err(UnifiedError::bad_request(
                        "Logo storage target must be 1 to 255 characters",
                    ));
                }
            }
            template.logo_storage_target = target;
        }
        if let Some(key) = input.logo_storage_key {
            if let Some(text) = &key {
                if text.trim().is_empty() || text.len() > 255 {
                    return Err(UnifiedError::bad_request(
                        "Logo storage key must be 1 to 255 characters",
                    ));
                }
            }
            template.logo_storage_key = key;
        }
        if let Some(url) = input.logo_url {
            validate_logo_url(&url)?;
            template.logo_url = url;
        }
        if let Some(color) = input.primary_color {
            if !valid_hex_color(&color) {
                return Err(UnifiedError::bad_request(
                    "Primary colour must be a #RGB or #RRGGBB hex colour",
                ));
            }
            template.primary_color = color;
        }
        if let Some(color) = input.accent_color {
            if !valid_hex_color(&color) {
                return Err(UnifiedError::bad_request(
                    "Accent colour must be a #RGB or #RRGGBB hex colour",
                ));
            }
            template.accent_color = color;
        }
        if let Some(font) = input.font_family {
            if !valid_font_family(&font) {
                return Err(UnifiedError::bad_request(
                    "Font family contains characters that are not allowed in a style block",
                ));
            }
            template.font_family = font;
        }
        if let Some(columns) = input.columns {
            template.columns = validate_columns(&columns)?;
        }
        if let Some(layout) = input.line_item_layout {
            if !layout.is_object() || layout.to_string().len() > 8000 {
                return Err(UnifiedError::bad_request(
                    "Line-item layout must be a JSON object of at most 8000 characters",
                ));
            }
            template.line_item_layout = layout;
        }
        if let Some(show) = input.show_tax_breakdown {
            template.show_tax_breakdown = show;
        }
        if let Some(instructions) = input.payment_instructions {
            validate_optional_text(&instructions, 5000, "Payment instructions")?;
            template.payment_instructions = instructions;
        }
        if let Some(notes) = input.footer_notes {
            validate_optional_text(&notes, 5000, "Footer notes")?;
            template.footer_notes = notes;
        }
        // The pre-edit shape was archived above; bump the version.
        template.version += 1;
        template.updated_at = now();
        state
            .invoice_templates
            .insert(id.clone(), template.clone());
        state.emit(
            EVENT_INVOICE_TEMPLATE_CHANGED,
            json!({
                "template_id": template.id.clone(),
                "company_id": template.company_id.clone(),
                "version": template.version,
                "action": "updated",
                "updated_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_MANAGE)?;
        let id = path.into_inner();
        let template = state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        require_record_writable_in_company_scope(&template.company_id, &_scope)?;
        if let Some(company) = &template.company_id {
            if let Some(default_id) = state.company_default_templates.get(company) {
                if default_id.value().as_str() == id {
                    state.company_default_templates.remove(company);
                }
            }
        }
        state.invoice_templates.remove(&id);
        state
            .template_versions
            .retain(|_, row| row.template_id != id);
        state.emit(
            EVENT_INVOICE_TEMPLATE_CHANGED,
            json!({
                "template_id": id.clone(),
                "company_id": template.company_id,
                "version": template.version,
                "action": "deleted",
                "updated_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, false)
}

pub(crate) async fn list_invoice_template_versions(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<InvoiceTemplateVersion>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_VIEW)?;
        let id = path.into_inner();
        let template = state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        require_record_in_company_scope(&template.company_id, &_scope)?;
        let params = query.into_inner();
        let mut items: Vec<InvoiceTemplateVersion> = state
            .template_versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.template_id == id)
            .collect();
        items.sort_by(|a, b| a.version.cmp(&b.version));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn restore_invoice_template_version(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, u32)>,
) -> HttpResponse {
    let result: UnifiedResult<InvoiceTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_MANAGE)?;
        let (id, version) = path.into_inner();
        let mut current = state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        require_record_writable_in_company_scope(&current.company_id, &_scope)?;
        let row = state
            .template_versions
            .iter()
            .map(|entry| entry.value().clone())
            .find(|row| row.template_id == id && row.version == version)
            .ok_or_else(|| {
                UnifiedError::not_found("Template version not found")
            })?;
        let mut restored: InvoiceTemplate =
            serde_json::from_value(row.snapshot.clone()).map_err(|_| {
                UnifiedError::internal("Stored template version cannot be read")
            })?;
        // Restoring copies the old shape forward as a new version:
        // history is never rewritten, and the default flag of the
        // live template is preserved.
        archive_template_version(&state, &current, &caller.actor);
        restored.id = current.id.clone();
        restored.is_default = current.is_default;
        restored.version = current.version + 1;
        restored.updated_at = now();
        current = restored;
        state
            .invoice_templates
            .insert(id.clone(), current.clone());
        state.emit(
            EVENT_INVOICE_TEMPLATE_CHANGED,
            json!({
                "template_id": current.id.clone(),
                "company_id": current.company_id.clone(),
                "version": current.version,
                "action": "restored",
                "restored_from_version": version,
                "updated_by": caller.actor,
            }),
        );
        Ok(current)
    })();
    json::respond(result, false)
}

pub(crate) async fn set_default_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let result: UnifiedResult<InvoiceTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_MANAGE)?;
        let id = path.into_inner();
        let mut template = state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        require_record_writable_in_company_scope(&template.company_id, &_scope)?;
        let company_id = body
            .get("company_id")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if company_id.is_empty() {
            return Err(UnifiedError::bad_request("company_id must not be empty"));
        }
        // The body names the company this default applies to;
        // it must match the caller scope. The match always
        // resolves a named company, so the fallback is unreachable.
        let company_id =
            require_company_match(&_tenant, &req, &Some(company_id))?.unwrap_or_default();
        // Clear the default flag on the company's other templates.
        // A global template keeps its own flag untouched: the
        // per-company mapping below is the source of truth for it.
        for mut entry in state.invoice_templates.iter_mut() {
            if entry.company_id.as_deref() == Some(company_id.as_str()) && entry.id != id {
                entry.is_default = false;
                entry.updated_at = now();
            }
        }
        if template.company_id.as_deref() == Some(company_id.as_str()) {
            template.is_default = true;
        }
        template.updated_at = now();
        state
            .company_default_templates
            .insert(company_id.clone(), id.clone());
        state
            .invoice_templates
            .insert(id.clone(), template.clone());
        state.emit(
            EVENT_INVOICE_TEMPLATE_CHANGED,
            json!({
                "template_id": template.id.clone(),
                "company_id": company_id.clone(),
                "version": template.version,
                "action": "set_default",
                "updated_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    json::respond(result, false)
}

pub(crate) async fn preview_invoice_template(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PrintReadyQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_TEMPLATE_VIEW)?;
        let id = path.into_inner();
        let template = state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found"))?;
        require_record_in_company_scope(&template.company_id, &_scope)?;
        let query = query.into_inner();
        let width_mm = query.paper_width_mm.unwrap_or(210.0);
        let height_mm = query.paper_height_mm.unwrap_or(297.0);
        validate_paper_dimensions(width_mm, height_mm)?;
        // The preview renders a real invoice when `invoice_id` is
        // given, otherwise sample data.
        let (invoice, sample) = match query.invoice_id {
            Some(invoice_id) => {
                let found = state
                    .invoices
                    .get(&invoice_id)
                    .map(|entry| view_invoice(entry.value()))
                    .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
                require_record_in_company_scope(&found.company_id, &_scope)?;
                (found, false)
            }
            None => (sample_invoice(), true),
        };
        let html = render_print_ready_html(&invoice, &template, width_mm, height_mm);
        Ok(json!({
            "template_id": template.id,
            "template_version": template.version,
            "invoice_id": invoice.id,
            "sample": sample,
            "paper_size_code": template.paper_size_code,
            "paper_width_mm": width_mm,
            "paper_height_mm": height_mm,
            "html": html,
        }))
    })();
    json::respond(result, false)
}

/// Resolve the template for a print: an explicit template id, or
/// the resolved default for the invoice's company.
pub(crate) fn resolve_print_template(
    state: &BillingState,
    template_id: Option<String>,
    company_id: Option<&str>,
) -> UnifiedResult<InvoiceTemplate> {
    match template_id {
        Some(id) => state
            .invoice_templates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice template not found")),
        None => Ok(default_template_for(state, company_id)),
    }
}

/// Resolve the paper size and dimensions for a print: an explicit
/// size code (or the template's), and explicit millimetre
/// dimensions (or A4).
pub(crate) fn resolve_print_paper(
    request_code: Option<String>,
    template: &InvoiceTemplate,
    width_mm: Option<f64>,
    height_mm: Option<f64>,
) -> UnifiedResult<(String, f64, f64)> {
    let code = request_code.unwrap_or_else(|| template.paper_size_code.clone());
    if !valid_paper_size_code(&code) {
        return Err(UnifiedError::bad_request(
            "Paper size code must be a lowercase slug",
        ));
    }
    let width = width_mm.unwrap_or(210.0);
    let height = height_mm.unwrap_or(297.0);
    validate_paper_dimensions(width, height)?;
    Ok((code, width, height))
}

/// Queue a print job through the printing module and return the
/// `(job_id, status)` it answers with. Fails when the printing
/// module has no `printing.job.requested` handler (it is not
/// registered) or when it reports an error.
pub(crate) fn request_print_job(
    state: &BillingState,
    payload: Value,
) -> UnifiedResult<(String, String)> {
    if !state.bus().has("printing.job.requested") {
        return Err(UnifiedError::internal(
            "Printing module is not registered: printing.job.requested has no handler",
        ));
    }
    let context = state.bus().emit("printing.job.requested", payload);
    match context.get("job_id").and_then(|value| value.as_str()) {
        Some(job_id) => {
            let status = context
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("queued")
                .to_string();
            Ok((job_id.to_string(), status))
        }
        None => {
            let detail = context
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown error")
                .to_string();
            Err(UnifiedError::internal(format!(
                "Printing module did not accept the print job: {detail}"
            )))
        }
    }
}

pub(crate) async fn print_invoice(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PrintInvoiceRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_PRINT)?;
        let id = path.into_inner();
        let invoice = state
            .invoices
            .get(&id)
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_in_company_scope(&invoice.company_id, &_scope)?;
        let input = body.into_inner();
        let template = resolve_print_template(
            &state,
            input.template_id.clone(),
            invoice.company_id.as_deref(),
        )?;
        require_record_in_company_scope(&template.company_id, &_scope)?;
        let (paper_size_code, width_mm, height_mm) = resolve_print_paper(
            input.paper_size_code.clone(),
            &template,
            input.paper_width_mm,
            input.paper_height_mm,
        )?;
        let output_kind = input
            .output_kind
            .clone()
            .unwrap_or_else(|| "paper".to_string());
        if !["paper", "thermal", "pdf", "email"].contains(&output_kind.as_str()) {
            return Err(UnifiedError::bad_request(
                "Print output kind must be paper, thermal, pdf, or email",
            ));
        }
        let mut payload = json!({
            "invoice_id": invoice.id.clone(),
            "company_id": invoice.company_id.clone(),
            "template_id": template.id.clone(),
            "template_version": template.version,
            "paper_size_code": paper_size_code.clone(),
            "output_kind": output_kind.clone(),
            "printer_profile_id": input.printer_profile_id.clone(),
            "requested_by": caller.actor.clone(),
        });
        if output_kind == "thermal" {
            payload["thermal_lines"] = invoice_thermal_lines(&invoice);
        } else {
            payload["document_html"] =
                json!(render_print_ready_html(&invoice, &template, width_mm, height_mm));
        }
        let (job_id, status) = request_print_job(&state, payload)?;
        state.emit(
            EVENT_INVOICE_PRINTED,
            json!({
                "invoice_id": invoice.id.clone(),
                "invoice_number": invoice.number.clone(),
                "job_id": job_id,
                "template_id": template.id.clone(),
                "template_version": template.version,
                "output_kind": output_kind.clone(),
                "requested_by": caller.actor,
            }),
        );
        Ok(json!({
            "invoice_id": invoice.id.clone(),
            "invoice_number": invoice.number,
            "job_id": job_id,
            "status": status,
            "output_kind": output_kind,
            "template_id": template.id,
            "template_version": template.version,
            "paper_size_code": paper_size_code,
            "print_ready_url": format!("/api/v1/billing/invoices/{}/print-ready", invoice.id),
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn print_ready_invoice(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PrintReadyQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_PRINT)?;
        let id = path.into_inner();
        let invoice = state
            .invoices
            .get(&id)
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_in_company_scope(&invoice.company_id, &_scope)?;
        let query = query.into_inner();
        let template = resolve_print_template(
            &state,
            query.template_id.clone(),
            invoice.company_id.as_deref(),
        )?;
        require_record_in_company_scope(&template.company_id, &_scope)?;
        let (paper_size_code, width_mm, height_mm) = resolve_print_paper(
            query.paper_size_code.clone(),
            &template,
            query.paper_width_mm,
            query.paper_height_mm,
        )?;
        let html = render_print_ready_html(&invoice, &template, width_mm, height_mm);
        Ok(json!({
            "invoice_id": invoice.id,
            "invoice_number": invoice.number,
            "template_id": template.id,
            "template_version": template.version,
            "paper_size_code": paper_size_code,
            "paper_width_mm": width_mm,
            "paper_height_mm": height_mm,
            "html": html,
            "note": "Print-ready HTML document with @page rules for the chosen paper size. Binary PDF conversion is a deployment step (headless browser or a configured converter hook), not a PDF binary.",
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn list_invoice_print_jobs(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_PRINT)?;
        let id = path.into_inner();
        let invoice = state
            .invoices
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_in_company_scope(&invoice.company_id, &_scope)?;
        let params = query.into_inner();
        if !state.bus().has("printing.jobs.list_requested") {
            let page = paginate(Vec::<Value>::new(), &params);
            return Ok(json!({
                "invoice_id": id,
                "jobs": page.items,
                "pagination": page.pagination,
                "total": page.pagination.total,
                "printing_available": false,
            }));
        }
        let status_filter = params.filters.get("status").cloned();
        let mut payload = json!({ "invoice_id": id.clone() });
        if let Some(status) = status_filter {
            payload["status"] = json!(status);
        }
        let context = state.bus().emit("printing.jobs.list_requested", payload);
        let all_jobs: Vec<Value> = context
            .get("jobs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // The hook answers with the FULL jobs list, so the page is
        // sliced here and the total is the list length — a hook
        // `total` would describe jobs this handler does not hold, so
        // it is not trusted for pagination.
        let page = paginate(all_jobs, &params);
        Ok(json!({
            "invoice_id": id,
            "jobs": page.items,
            "pagination": page.pagination,
            "total": page.pagination.total,
            "printing_available": true,
        }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn reprint_invoice_print_job(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_PRINT)?;
        let job_id = path.into_inner();
        if !state.bus().has("printing.job.reprint_requested") {
            return Err(UnifiedError::internal(
                "Printing module is not registered: printing.job.reprint_requested has no handler",
            ));
        }
        let context = state.bus().emit(
            "printing.job.reprint_requested",
            json!({ "job_id": job_id.clone(), "requested_by": caller.actor }),
        );
        match context.get("job_id").and_then(|value| value.as_str()) {
            Some(new_job_id) => {
                let status = context
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("queued")
                    .to_string();
                Ok(json!({
                    "job_id": new_job_id,
                    "previous_job_id": job_id,
                    "status": status,
                }))
            }
            None => {
                let detail = context
                    .get("error")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                Err(UnifiedError::internal(format!(
                    "Printing module did not accept the reprint: {detail}"
                )))
            }
        }
    })();
    json::respond(result, false)
}

pub(crate) async fn email_invoice_pdf(
    state: web::Data<Arc<BillingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EmailInvoiceRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BILLING_PRINT)?;
        let id = path.into_inner();
        let invoice = state
            .invoices
            .get(&id)
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_in_company_scope(&invoice.company_id, &_scope)?;
        let input = body.into_inner();
        if !valid_email(&input.to) {
            return Err(UnifiedError::bad_request(
                "Recipient email address is not valid",
            ));
        }
        let template = resolve_print_template(
            &state,
            input.template_id.clone(),
            invoice.company_id.as_deref(),
        )?;
        require_record_in_company_scope(&template.company_id, &_scope)?;
        let (paper_size_code, width_mm, height_mm) = resolve_print_paper(
            input.paper_size_code.clone(),
            &template,
            None,
            None,
        )?;
        let html = render_print_ready_html(&invoice, &template, width_mm, height_mm);
        let subject = input
            .subject
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| format!("Invoice {}", invoice.number));
        if subject.len() > 200 {
            return Err(UnifiedError::bad_request(
                "Email subject must be at most 200 characters",
            ));
        }
        let body_text = input.body_text.filter(|text| !text.trim().is_empty()).unwrap_or_else(|| {
            format!(
                "Invoice {} for {:.2} {} is attached as a print-ready document. Balance due: {:.2} {}.",
                invoice.number, minor_to_major(invoice.total, &invoice.currency), invoice.currency, minor_to_major(invoice.balance, &invoice.currency), invoice.currency
            )
        });
        // The mail module has no attachment field: the print-ready
        // document travels as the HTML body of the message.
        let mail_context = state.bus().emit(
            "mail.send_requested",
            json!({
                "to": input.to.clone(),
                "subject": subject,
                "body_text": body_text,
                "body_html": html,
                "variables": {
                    "invoice_id": invoice.id.clone(),
                    "invoice_number": invoice.number.clone(),
                },
            }),
        );
        let mail_job_id = mail_context
            .results
            .iter()
            .find_map(|result| {
                result
                    .get("job_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            });
        state.emit(
            EVENT_INVOICE_EMAILED,
            json!({
                "invoice_id": invoice.id.clone(),
                "invoice_number": invoice.number.clone(),
                "to": input.to.clone(),
                "mail_job_id": mail_job_id,
                "paper_size_code": paper_size_code,
                "requested_by": caller.actor,
            }),
        );
        Ok(json!({
            "invoice_id": invoice.id,
            "invoice_number": invoice.number,
            "to": input.to,
            "mailed": true,
            "mail_job_id": mail_job_id,
            "template_id": template.id,
            "template_version": template.version,
            "note": "The mail module has no attachment field, so the print-ready document was sent as the HTML body of the message.",
        }))
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_paper_dimensions_accepts_a4() {
        assert!(validate_paper_dimensions(210.0, 297.0).is_ok());
    }

    #[test]
    fn validate_paper_dimensions_rejects_zero_width() {
        assert!(validate_paper_dimensions(0.0, 297.0).is_err());
    }

    #[test]
    fn validate_paper_dimensions_rejects_oversized_height() {
        assert!(validate_paper_dimensions(210.0, 5000.0).is_err());
    }

    #[test]
    fn validate_paper_dimensions_rejects_nan() {
        assert!(validate_paper_dimensions(f64::NAN, 297.0).is_err());
    }

    #[test]
    fn valid_email_accepts_plain_addresses_and_rejects_junk() {
        assert!(valid_email("a@b.co"));
        assert!(!valid_email("no-at"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a @b.co"));
        assert!(!valid_email("a@b@c.co"));
    }
}
