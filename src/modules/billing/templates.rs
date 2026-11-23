//! billing - templates.

use unified_core::context::AppContext;
use unified_core::reference_id;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::HttpRequest;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use super::types::*;
use super::state::*;
use super::helpers::*;

/// Read invoice templates.
pub(crate) const PERM_BILLING_TEMPLATE_VIEW: &str = "billing.template.view";

/// Create, update, delete, restore, and set-default on invoice templates.
pub(crate) const PERM_BILLING_TEMPLATE_MANAGE: &str = "billing.template.manage";

/// Print invoices, list their print jobs, reprint, and email them.
pub(crate) const PERM_BILLING_PRINT: &str = "billing.print";

/// Event emitted when an invoice is printed. Payload:
/// `{"invoice_id", "invoice_number", "job_id", "template_id",
/// "template_version", "output_kind", "requested_by"}`.
pub const EVENT_INVOICE_PRINTED: &str = "billing.invoice.printed";

/// Event emitted when an invoice is emailed. Payload:
/// `{"invoice_id", "invoice_number", "to", "mail_job_id",
/// "requested_by"}`.
pub const EVENT_INVOICE_EMAILED: &str = "billing.invoice.emailed";

/// Event emitted on every invoice-template change (create, update,
/// delete, restore, set-default), for the audit log. Payload:
/// `{"template_id", "company_id", "version", "action",
/// "updated_by"}`.
pub const EVENT_INVOICE_TEMPLATE_CHANGED: &str = "billing.invoice_template.changed";

/// Column keys an invoice template may declare. The list is the
/// closed vocabulary the renderer understands; a new column key
/// needs a renderer case, never a migration.
pub const TEMPLATE_COLUMNS: &[&str] = &[
    "item",
    "quantity",
    "unit_price",
    "tax_amount",
    "line_total",
    "tax_rate",
];

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the assets and POS modules.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the assets module's capability check).
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

/// One line-item column of an invoice template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateColumn {
    pub key: String,
    pub label: String,
    pub visible: bool,
}

/// Input for one template column. `visible` defaults to true.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateColumnInput {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub visible: Option<bool>,
}

/// One invoice template: the full layout of a printed invoice, as
/// data. `paper_size_code` names a size from the printing module's
/// registry (for example `a4` or `thermal-80`); billing validates
/// only its shape, the printing module validates its existence at
/// print time. `logo_storage_target` + `logo_storage_key` reference
/// logo bytes in the storage module; `logo_url` is an optional
/// directly usable URL. `line_item_layout` is a free-form JSON
/// object of layout options (row density and similar) so a new
/// option never needs a migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceTemplate {
    pub id: String,
    #[serde(default)]
    pub company_id: Option<String>,
    pub name: String,
    pub is_default: bool,
    pub paper_size_code: String,
    #[serde(default)]
    pub header_text: Option<String>,
    #[serde(default)]
    pub footer_text: Option<String>,
    #[serde(default)]
    pub logo_storage_target: Option<String>,
    #[serde(default)]
    pub logo_storage_key: Option<String>,
    #[serde(default)]
    pub logo_url: Option<String>,
    pub primary_color: String,
    pub accent_color: String,
    pub font_family: String,
    pub columns: Vec<TemplateColumn>,
    pub line_item_layout: Value,
    pub show_tax_breakdown: bool,
    #[serde(default)]
    pub payment_instructions: Option<String>,
    #[serde(default)]
    pub footer_notes: Option<String>,
    pub version: u32,
    #[serde(default)]
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One archived template version: the full JSON snapshot of the
/// template as it was before an edit. Append-only; restoring a
/// version copies it forward as a new version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceTemplateVersion {
    pub id: String,
    pub template_id: String,
    pub version: u32,
    pub snapshot: Value,
    #[serde(default)]
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Input for creating an invoice template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvoiceTemplate {
    pub name: String,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    #[serde(default)]
    pub header_text: Option<String>,
    #[serde(default)]
    pub footer_text: Option<String>,
    #[serde(default)]
    pub logo_storage_target: Option<String>,
    #[serde(default)]
    pub logo_storage_key: Option<String>,
    #[serde(default)]
    pub logo_url: Option<String>,
    #[serde(default)]
    pub primary_color: Option<String>,
    #[serde(default)]
    pub accent_color: Option<String>,
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub columns: Option<Vec<TemplateColumnInput>>,
    #[serde(default)]
    pub line_item_layout: Option<Value>,
    #[serde(default)]
    pub show_tax_breakdown: Option<bool>,
    #[serde(default)]
    pub payment_instructions: Option<String>,
    #[serde(default)]
    pub footer_notes: Option<String>,
}

/// Input for updating an invoice template. Every field is optional;
/// only the fields present change. Any successful update archives
/// the previous version and bumps `version` by one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInvoiceTemplate {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub company_id: Option<Option<String>>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    #[serde(default)]
    pub header_text: Option<Option<String>>,
    #[serde(default)]
    pub footer_text: Option<Option<String>>,
    #[serde(default)]
    pub logo_storage_target: Option<Option<String>>,
    #[serde(default)]
    pub logo_storage_key: Option<Option<String>>,
    #[serde(default)]
    pub logo_url: Option<Option<String>>,
    #[serde(default)]
    pub primary_color: Option<String>,
    #[serde(default)]
    pub accent_color: Option<String>,
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub columns: Option<Vec<TemplateColumnInput>>,
    #[serde(default)]
    pub line_item_layout: Option<Value>,
    #[serde(default)]
    pub show_tax_breakdown: Option<bool>,
    #[serde(default)]
    pub payment_instructions: Option<Option<String>>,
    #[serde(default)]
    pub footer_notes: Option<Option<String>>,
}

/// Input for printing one invoice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintInvoiceRequest {
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    #[serde(default)]
    pub paper_width_mm: Option<f64>,
    #[serde(default)]
    pub paper_height_mm: Option<f64>,
    #[serde(default)]
    pub output_kind: Option<String>,
    #[serde(default)]
    pub printer_profile_id: Option<String>,
}

/// Query for the print-ready document of one invoice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintReadyQuery {
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
    #[serde(default)]
    pub paper_width_mm: Option<f64>,
    #[serde(default)]
    pub paper_height_mm: Option<f64>,
    /// Preview only: render this real invoice instead of the
    /// sample invoice.
    #[serde(default)]
    pub invoice_id: Option<String>,
}

/// Input for emailing one invoice as a print-ready document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailInvoiceRequest {
    pub to: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub body_text: Option<String>,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub paper_size_code: Option<String>,
}

/// A `#RGB` or `#RRGGBB` colour.
pub(crate) fn valid_hex_color(value: &str) -> bool {
    let hex = value.strip_prefix('#').unwrap_or("");
    (hex.len() == 3 || hex.len() == 6) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// A font-family value safe to interpolate into a `<style>` block:
/// letters, digits, spaces, commas, hyphens, and quotes only -- no
/// angle brackets, braces, semicolons, slashes, or `url(`.
pub(crate) fn valid_font_family(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == ' ' || c == ',' || c == '-' || c == '\'' || c == '"'
        })
}

/// A paper-size code is a lowercase slug, matching the printing
/// module's registry codes.
pub(crate) fn valid_paper_size_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A short free-text template field (header, footer, notes,
/// instructions): at most `max` characters when present.
pub(crate) fn validate_optional_text(
    value: &Option<String>,
    max: usize,
    label: &str,
) -> UnifiedResult<()> {
    if let Some(text) = value {
        if text.len() > max {
            return Err(UnifiedError::bad_request(format!(
                "{label} must be at most {max} characters"
            )));
        }
    }
    Ok(())
}

/// Validate a logo URL: an absolute http(s) URL or a root-relative
/// path, at most 2048 characters.
pub(crate) fn validate_logo_url(value: &Option<String>) -> UnifiedResult<()> {
    if let Some(url) = value {
        if url.len() > 2048
            || !(url.starts_with("https://")
                || url.starts_with("http://")
                || url.starts_with('/'))
        {
            return Err(UnifiedError::bad_request(
                "Logo URL must be an http(s) URL or a root-relative path",
            ));
        }
    }
    Ok(())
}

/// Validate template columns: 1 to 8 columns, keys from the closed
/// vocabulary, no duplicate keys, labels 1 to 60 characters.
pub(crate) fn validate_columns(
    input: &[TemplateColumnInput],
) -> UnifiedResult<Vec<TemplateColumn>> {
    if input.is_empty() || input.len() > 8 {
        return Err(UnifiedError::bad_request(
            "An invoice template needs 1 to 8 columns",
        ));
    }
    let mut seen: Vec<String> = Vec::new();
    let mut columns = Vec::new();
    for column in input {
        if !TEMPLATE_COLUMNS.contains(&column.key.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown invoice column \"{}\"",
                column.key
            )));
        }
        if seen.contains(&column.key) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate invoice column \"{}\"",
                column.key
            )));
        }
        if column.label.trim().is_empty() || column.label.len() > 60 {
            return Err(UnifiedError::bad_request(
                "Column labels must be 1 to 60 characters",
            ));
        }
        seen.push(column.key.clone());
        columns.push(TemplateColumn {
            key: column.key.clone(),
            label: column.label.trim().to_string(),
            visible: column.visible.unwrap_or(true),
        });
    }
    Ok(columns)
}

/// The default columns: item, quantity, unit price, tax, total.
pub(crate) fn default_template_columns() -> Vec<TemplateColumn> {
    vec![
        TemplateColumn { key: "item".to_string(), label: "Item".to_string(), visible: true },
        TemplateColumn { key: "quantity".to_string(), label: "Qty".to_string(), visible: true },
        TemplateColumn { key: "unit_price".to_string(), label: "Unit price".to_string(), visible: true },
        TemplateColumn { key: "tax_amount".to_string(), label: "Tax".to_string(), visible: true },
        TemplateColumn { key: "line_total".to_string(), label: "Total".to_string(), visible: true },
    ]
}

/// The built-in fallback template, used when no stored template
/// applies. It is never stored: it only exists so printing always
/// has a layout.
pub(crate) fn builtin_default_template(company_id: Option<String>) -> InvoiceTemplate {
    let moment = now();
    InvoiceTemplate {
        id: "builtin-default".to_string(),
        company_id,
        name: "Default".to_string(),
        is_default: false,
        paper_size_code: "a4".to_string(),
        header_text: None,
        footer_text: None,
        logo_storage_target: None,
        logo_storage_key: None,
        logo_url: None,
        primary_color: "#111827".to_string(),
        accent_color: "#2563EB".to_string(),
        font_family: "Arial, sans-serif".to_string(),
        columns: default_template_columns(),
        line_item_layout: json!({}),
        show_tax_breakdown: true,
        payment_instructions: None,
        footer_notes: None,
        version: 1,
        created_by: None,
        created_at: moment,
        updated_at: moment,
    }
}

/// Resolve the template that prints an invoice: the company's
/// configured default, then a default template of that company,
/// then the global default, then the built-in fallback.
pub(crate) fn default_template_for(state: &BillingState, company_id: Option<&str>) -> InvoiceTemplate {
    if let Some(company) = company_id {
        if let Some(entry) = state.company_default_templates.get(company) {
            if let Some(template) = state
                .invoice_templates
                .get(entry.value().as_str())
                .map(|entry| entry.value().clone())
            {
                return template;
            }
        }
        let mut company_defaults: Vec<InvoiceTemplate> = state
            .invoice_templates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|template| {
                template.is_default && template.company_id.as_deref() == Some(company)
            })
            .collect();
        company_defaults.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if let Some(template) = company_defaults.into_iter().next() {
            return template;
        }
    }
    let mut global_defaults: Vec<InvoiceTemplate> = state
        .invoice_templates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|template| template.is_default && template.company_id.is_none())
        .collect();
    global_defaults.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if let Some(template) = global_defaults.into_iter().next() {
        return template;
    }
    builtin_default_template(company_id.map(str::to_string))
}

/// Archive the current shape of a template as a version row.
pub(crate) fn archive_template_version(
    state: &BillingState,
    template: &InvoiceTemplate,
    actor: &str,
) {
    let snapshot = serde_json::to_value(template).unwrap_or(Value::Null);
    let row = InvoiceTemplateVersion {
        id: new_id(),
        template_id: template.id.clone(),
        version: template.version,
        snapshot,
        created_by: Some(actor.to_string()),
        created_at: now(),
    };
    state.template_versions.insert(row.id.clone(), row);
}

/// A deterministic sample invoice, used by the template preview
/// when no real invoice id is given.
pub(crate) fn sample_invoice() -> Invoice {
    let moment = now();
    Invoice {
        id: "sample".to_string(),
        number: "INV-SAMPLE".to_string(),
        reference: reference_id::EXAMPLE_REFERENCE_ID.to_string(),
        order_id: None,
        subscription_id: None,
        user_id: "sample-customer".to_string(),
        company_id: None,
        items: vec![
            InvoiceItem {
                name: "Consulting services".to_string(),
                quantity: 2,
                unit_price: 15000,
                line_total: 30000,
                tax_rate: 0.10,
                tax_amount: 3000,
            },
            InvoiceItem {
                name: "Annual hosting plan".to_string(),
                quantity: 1,
                unit_price: 24000,
                line_total: 24000,
                tax_rate: 0.10,
                tax_amount: 2400,
            },
        ],
        subtotal: 54000,
        tax_total: 5400,
        total: 59400,
        currency: "USD".to_string(),
        status: "issued".to_string(),
        overdue: false,
        tax_inclusive: false,
        amount_paid: 0,
        credit_total: 0,
        balance: 59400,
        issue_date: Some(moment),
        due_date: Some(moment + Duration::days(30)),
        payment_terms_days: Some(30),
        paid_at: None,
        notes: Some("Thank you for your business.".to_string()),
        reminders_sent: vec![],
        created_at: moment,
        updated_at: moment,
    }
}

/// One cell of the line-item table, per the template column key.
/// Money cells render in major units for the invoice currency.
pub(crate) fn template_cell(item: &InvoiceItem, key: &str, currency: &str) -> String {
    match key {
        "item" => escape_html(&item.name),
        "quantity" => item.quantity.to_string(),
        "unit_price" => format!("{:.2}", minor_to_major(item.unit_price, currency)),
        "tax_amount" => format!("{:.2}", minor_to_major(item.tax_amount, currency)),
        "line_total" => format!("{:.2}", minor_to_major(item.line_total, currency)),
        "tax_rate" => format!("{:.1}%", item.tax_rate * 100.0),
        _ => String::new(),
    }
}

/// Render an invoice with a template as a print-ready HTML
/// document. `@page` carries the chosen paper size so a print or
/// PDF step honours it; every user-controlled string is escaped.
/// The document is HTML, not PDF bytes: binary PDF conversion is
/// a deployment step (headless browser or a configured converter
/// hook).
pub(crate) fn render_print_ready_html(
    invoice: &Invoice,
    template: &InvoiceTemplate,
    width_mm: f64,
    height_mm: f64,
) -> String {
    let mut header_cells = String::new();
    let mut rows = String::new();
    let visible: Vec<&TemplateColumn> = template
        .columns
        .iter()
        .filter(|column| column.visible)
        .collect();
    for column in &visible {
        header_cells.push_str(&format!("<th>{}</th>", escape_html(&column.label)));
    }
    for item in &invoice.items {
        let mut cells = String::new();
        for column in &visible {
            cells.push_str(&format!(
                "<td>{}</td>",
                template_cell(item, &column.key, &invoice.currency)
            ));
        }
        rows.push_str(&format!("<tr>{cells}</tr>"));
    }
    let mut tax_rows = String::new();
    if template.show_tax_breakdown {
        let mut by_rate: Vec<(f64, i64)> = Vec::new();
        for item in &invoice.items {
            let rate = item.tax_rate;
            match by_rate.iter_mut().find(|(r, _)| (*r - rate).abs() < f64::EPSILON) {
                Some((_, amount)) => *amount += item.tax_amount,
                None => by_rate.push((rate, item.tax_amount)),
            }
        }
        by_rate.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (rate, amount) in by_rate {
            tax_rows.push_str(&format!(
                "<tr><td>Tax {:.1}%</td><td>{:.2}</td></tr>",
                rate * 100.0,
                minor_to_major(amount, &invoice.currency)
            ));
        }
    }
    let due = invoice
        .due_date
        .map(|due| due.to_rfc3339())
        .unwrap_or_else(|| "-".to_string());
    let issued = invoice
        .issue_date
        .map(|issued| issued.to_rfc3339())
        .unwrap_or_else(|| "-".to_string());
    let logo = match (&template.logo_url, &template.logo_storage_target, &template.logo_storage_key) {
        (Some(url), _, _) => format!(
            "<img src=\"{}\" alt=\"Logo\" style=\"max-height:64px;max-width:220px;\">",
            escape_html(url)
        ),
        (None, Some(target), Some(key)) => format!(
            "<div class=\"logo-ref\">Logo: {}/{}</div>",
            escape_html(target),
            escape_html(key)
        ),
        _ => String::new(),
    };
    let header = template
        .header_text
        .as_deref()
        .map(|text| format!("<div class=\"header-text\">{}</div>", escape_html(text)))
        .unwrap_or_default();
    let footer = template
        .footer_text
        .as_deref()
        .map(|text| format!("<div class=\"footer-text\">{}</div>", escape_html(text)))
        .unwrap_or_default();
    let payment = template
        .payment_instructions
        .as_deref()
        .map(|text| {
            format!(
                "<section><h2>Payment instructions</h2><p>{}</p></section>",
                escape_html(text)
            )
        })
        .unwrap_or_default();
    let notes = template
        .footer_notes
        .as_deref()
        .map(|text| format!("<p class=\"notes\">{}</p>", escape_html(text)))
        .unwrap_or_default();
    let invoice_notes = invoice
        .notes
        .as_deref()
        .map(|text| format!("<p class=\"notes\">{}</p>", escape_html(text)))
        .unwrap_or_default();
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Invoice {number}</title><style>\
        @page {{ size: {width}mm {height}mm; margin: 10mm; }}\
        body {{ font-family: {font}; color: #111827; }}\
        h1 {{ color: {primary}; }}\
        h2 {{ color: {accent}; }}\
        table {{ width: 100%; border-collapse: collapse; }}\
        th, td {{ border: 1px solid #d1d5db; padding: 6px 8px; text-align: left; }}\
        th {{ background: {primary}; color: #ffffff; }}\
        .totals td {{ border: none; }}\
        .logo-ref {{ color: #6b7280; }}\
        .notes {{ color: #4b5563; }}\
        </style></head><body>{logo}{header}\
        <h1>Invoice {number}</h1>\
        <p>Status: {status}<br>Customer: {user}<br>Currency: {currency}<br>Issued: {issued}<br>Due date: {due}</p>\
        <table><thead><tr>{header_cells}</tr></thead><tbody>{rows}</tbody></table>\
        <table class=\"totals\"><tbody>\
        <tr><td>Subtotal</td><td>{subtotal:.2}</td></tr>{tax_rows}\
        <tr><td>Tax total</td><td>{tax_total:.2}</td></tr>\
        <tr><td><strong>Total</strong></td><td><strong>{total:.2}</strong></td></tr>\
        <tr><td>Paid</td><td>{paid:.2}</td></tr>\
        <tr><td>Balance</td><td>{balance:.2}</td></tr>\
        </tbody></table>{payment}{invoice_notes}{notes}{footer}</body></html>",
        number = escape_html(&invoice.number),
        width = width_mm,
        height = height_mm,
        font = template.font_family,
        primary = template.primary_color,
        accent = template.accent_color,
        logo = logo,
        header = header,
        status = escape_html(&invoice.status),
        user = escape_html(&invoice.user_id),
        currency = escape_html(&invoice.currency),
        issued = issued,
        due = due,
        header_cells = header_cells,
        rows = rows,
        subtotal = minor_to_major(invoice.subtotal, &invoice.currency),
        tax_rows = tax_rows,
        tax_total = minor_to_major(invoice.tax_total, &invoice.currency),
        total = minor_to_major(invoice.total, &invoice.currency),
        paid = minor_to_major(invoice.amount_paid, &invoice.currency),
        balance = minor_to_major(invoice.balance, &invoice.currency),
        payment = payment,
        invoice_notes = invoice_notes,
        notes = notes,
        footer = footer,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_hex_color_accepts_three_and_six_digit_codes() {
        assert!(valid_hex_color("#0B6B3A"));
        assert!(valid_hex_color("#FFF"));
        assert!(valid_hex_color("#fff"));
    }

    #[test]
    fn valid_hex_color_rejects_bad_codes() {
        assert!(!valid_hex_color("#GGG"));
        assert!(!valid_hex_color("0B6B3A"));
        assert!(!valid_hex_color("#12345"));
        assert!(!valid_hex_color("#1234567"));
    }

    #[test]
    fn valid_font_family_accepts_common_stacks() {
        assert!(valid_font_family("Inter, sans-serif"));
        assert!(valid_font_family("Georgia, serif"));
    }

    #[test]
    fn valid_font_family_rejects_unsafe_characters() {
        assert!(!valid_font_family("Arial <script>"));
        assert!(!valid_font_family(""));
        assert!(!valid_font_family("Inter; color: red"));
    }

    #[test]
    fn valid_paper_size_code_accepts_lowercase_codes() {
        assert!(valid_paper_size_code("a4"));
        assert!(valid_paper_size_code("a-4_1"));
        assert!(!valid_paper_size_code("A4"));
        assert!(!valid_paper_size_code("a4 "));
        assert!(!valid_paper_size_code(""));
    }
}
