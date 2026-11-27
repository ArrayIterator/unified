//! Delivery notes types: DO documents generated
//! from logistics shipments, with a data-driven number format,
//! a pure status machine, snapshotted parties and items, proof
//! of delivery linkage, and return bookkeeping.
//!
//! Nothing about numbering, statuses, or templates is
//! hard-coded: the number prefix, zero-pad width, and reset
//! rule live in per-tenant settings, and templates are rows
//! referenced by id. Money: this module carries no money
//! fields, so there is no floating point money anywhere in it.
//! Quantities are whole units (`i64`); a DO never invents a
//! quantity - shipped quantities come from the snapshot taken
//! at creation time, and returned quantities are recorded
//! against that snapshot.

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_LABEL_LEN: usize = 200;

pub(crate) const MAX_ADDRESS_LEN: usize = 500;

pub(crate) const MAX_NOTES_LEN: usize = 5000;

pub(crate) const MAX_REASON_LEN: usize = 500;

pub(crate) const MAX_UNIT_LEN: usize = 32;

pub(crate) const MAX_REFERENCE_LEN: usize = 200;

pub(crate) const MAX_PREFIX_LEN: usize = 16;

pub(crate) const MAX_TEMPLATE_ID_LEN: usize = 128;

pub(crate) const MAX_PAPER_SIZE_LEN: usize = 32;

/// Default number prefix when the tenant settings carry none.
pub(crate) const DEFAULT_NUMBER_PREFIX: &str = "SJ";

/// Default zero-pad width of the sequence part of the number.
pub(crate) const DEFAULT_NUMBER_WIDTH: i64 = 4;

/// Default reset rule for the number sequence.
pub(crate) const DEFAULT_RESET_RULE: &str = "yearly";

/// Default paper size code for the print-ready document.
pub(crate) const DEFAULT_PAPER_SIZE_CODE: &str = "A4";

/// Default print output kind (`paper`, `pdf`, `thermal`,
/// `email` - the printing module's vocabulary).
pub(crate) const DEFAULT_OUTPUT_KIND: &str = "paper";

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_DELIVERY_NOTES_VIEW: &str = "delivery_notes.view";

pub(crate) const PERM_DELIVERY_NOTES_MANAGE: &str = "delivery_notes.manage";

/// Hook event names this module emits.
pub(crate) const EVENT_CREATED: &str = "delivery_notes.created";

pub(crate) const EVENT_UPDATED: &str = "delivery_notes.updated";

pub(crate) const EVENT_DELETED: &str = "delivery_notes.deleted";

pub(crate) const EVENT_ISSUED: &str = "delivery_notes.issued";

pub(crate) const EVENT_IN_TRANSIT: &str = "delivery_notes.in_transit";

pub(crate) const EVENT_DELIVERED: &str = "delivery_notes.delivered";

pub(crate) const EVENT_RETURNED: &str = "delivery_notes.returned";

pub(crate) const EVENT_CANCELLED: &str = "delivery_notes.cancelled";

pub(crate) const EVENT_PRINTED: &str = "delivery_notes.printed";

/// Hook event names this module listens to.
pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

pub(crate) const EVENT_SHIPMENT_STATUS_CHANGED: &str = "logistics.shipment.status_changed";

pub(crate) const EVENT_SHIPMENT_DELIVERED: &str = "logistics.shipment.delivered";

pub(crate) const EVENT_SHIPMENT_RETURNED: &str = "logistics.shipment.returned";

pub(crate) const EVENT_SHIPMENT_CANCELLED: &str = "logistics.shipment.cancelled";

pub(crate) const EVENT_PROOF_CREATED: &str = "logistics.proof.created";

pub(crate) const EVENT_RENTAL_ORDER_RETURNED: &str = "rental.order.returned";

/// The AI grounding hook the AI module's runner emits for this
/// module's summary capability
/// (`<module>.ai.<capability>_requested`).
pub(crate) const EVENT_AI_SUMMARIZE_REQUESTED: &str =
    "delivery_notes.ai.summarize_delivery_note_requested";

/// The AI grounding hook for the delivery-readiness capability.
pub(crate) const EVENT_AI_READINESS_REQUESTED: &str =
    "delivery_notes.ai.check_delivery_readiness_requested";

/// Marker written into `created_by` when the owning user is
/// deleted. Rows are kept (delivery notes stay auditable);
/// only the identity is anonymized.
pub(crate) const DELETED_USER_ID: &str = "deleted-user";

/// Reset rules for the DO number sequence, as data in
/// settings. `yearly` restarts the sequence every calendar
/// year, `monthly` every calendar month, `never` never
/// restarts it.
pub(crate) const RESET_RULES: &[&str] = &["yearly", "monthly", "never"];

/// Return scopes recorded on a returned DO: every shipped unit
/// came back (`full`), only some of them did (`partial`), or
/// nothing was returned yet (`none`, the pre-return value).
pub(crate) const RETURN_SCOPES: &[&str] = &["full", "partial", "none"];

// Pure domain functions (unit-tested below)

/// Period key for the number sequence: the part of the number
/// that resets. `yearly` keys on the calendar year, `monthly`
/// on year-month, `never` on a single constant key. An unknown
/// rule falls back to the yearly key instead of inventing a
/// new reset behaviour.
pub fn number_period_key(reset_rule: &str, now: DateTime<Utc>) -> String {
    match reset_rule {
        "monthly" => format!("{:04}-{:02}", now.year(), now.month()),
        "never" => "all".to_string(),
        _ => format!("{:04}", now.year()),
    }
}

/// Format one DO number from the tenant settings, the period
/// key, and the sequence value within that period. The shape
/// is `<prefix>-<period>-<sequence>` for yearly/monthly
/// resets (`SJ-2026-0001`, `SJ-2026-11-0001`) and
/// `<prefix>-<sequence>` when the sequence never resets
/// (`SJ-0001`). The sequence is zero-padded to the configured
/// width and grows past it instead of truncating (sequence
/// 12345 at width 4 renders `12345`, never `2345`).
pub fn format_number(
    prefix: &str,
    reset_rule: &str,
    width: i64,
    period_key: &str,
    sequence: i64,
) -> String {
    let width = width.clamp(1, 8) as usize;
    let padded = format!("{sequence:0width$}", width = width);
    if reset_rule == "never" {
        format!("{prefix}-{padded}")
    } else {
        format!("{prefix}-{period_key}-{padded}")
    }
}

/// True when `prefix` is a valid number prefix: 1-16
/// characters, uppercase letters, digits, and single hyphens
/// only, never leading or trailing with a hyphen.
pub fn valid_prefix(prefix: &str) -> bool {
    if prefix.is_empty() || prefix.len() > MAX_PREFIX_LEN {
        return false;
    }
    if prefix.starts_with('-') || prefix.ends_with('-') || prefix.contains("--") {
        return false;
    }
    prefix
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
}

/// DO status machine. Allowed transitions:
/// draft -> issued, cancelled; issued -> in_transit,
/// cancelled; in_transit -> delivered, returned, cancelled;
/// delivered -> returned; returned and cancelled are
/// terminal. Anything else is rejected. Note `delivered` is
/// additionally gated in the handler: a DO may only move to
/// delivered when a proof of delivery is linked (see
/// `requires_proof_for_delivery` in settings) - the pure
/// machine allows the edge, the handler enforces the proof.
pub fn can_transition(from: DeliveryNoteStatus, to: DeliveryNoteStatus) -> bool {
    use DeliveryNoteStatus::{Cancelled, Delivered, Draft, InTransit, Issued, Returned};
    matches!(
        (from, to),
        (Draft, Issued)
            | (Draft, Cancelled)
            | (Issued, InTransit)
            | (Issued, Cancelled)
            | (InTransit, Delivered)
            | (InTransit, Returned)
            | (InTransit, Cancelled)
            | (Delivered, Returned)
    )
}

/// Total ordered quantity across items.
pub fn total_ordered(items: &[DeliveryNoteItem]) -> i64 {
    items.iter().map(|item| item.quantity_ordered).sum()
}

/// Total shipped quantity across items.
pub fn total_shipped(items: &[DeliveryNoteItem]) -> i64 {
    items.iter().map(|item| item.quantity_shipped).sum()
}

/// Total returned quantity across items.
pub fn total_returned(items: &[DeliveryNoteItem]) -> i64 {
    items.iter().map(|item| item.quantity_returned).sum()
}

/// Return scope of a DO from its items: `full` when every
/// shipped unit came back, `partial` when some (but not all)
/// did, `none` when nothing came back. A DO with no shipped
/// units at all reads as `none` - there is nothing to return.
pub fn return_scope(items: &[DeliveryNoteItem]) -> &'static str {
    let shipped = total_shipped(items);
    let returned = total_returned(items);
    if shipped <= 0 || returned <= 0 {
        "none"
    } else if returned >= shipped {
        "full"
    } else {
        "partial"
    }
}

/// Validate a set of per-item returned quantities against the
/// shipped snapshot: every returned quantity must be between 0
/// and the item's shipped quantity, and at least one item must
/// return something. Returns the scope the return would land
/// in, or an error message describing the first violation.
pub fn validate_return_quantities(
    items: &[DeliveryNoteItem],
    returned: &[(Uuid, i64)],
) -> Result<&'static str, String> {
    if returned.is_empty() {
        return Err("A return needs at least one returned item".to_string());
    }
    let mut any_returned = false;
    let mut projected: Vec<DeliveryNoteItem> = items.to_vec();
    for (item_id, quantity) in returned {
        let Some(item) = projected.iter_mut().find(|row| &row.id == item_id) else {
            return Err(format!("Unknown delivery note item: {item_id}"));
        };
        if *quantity < 0 {
            return Err(format!(
                "Returned quantity for item {item_id} must not be negative"
            ));
        }
        if *quantity > item.quantity_shipped {
            return Err(format!(
                "Returned quantity for item {item_id} ({quantity}) exceeds the shipped quantity ({})",
                item.quantity_shipped
            ));
        }
        if *quantity > 0 {
            any_returned = true;
        }
        item.quantity_returned = item
            .quantity_returned
            .saturating_add(*quantity)
            .min(item.quantity_shipped);
    }
    if !any_returned {
        return Err("A return needs at least one item with a quantity above zero".to_string());
    }
    Ok(return_scope(&projected))
}

/// Build the snapshotted items of a new DO from the caller's
/// item inputs: ordered and shipped quantities both start at
/// the supplied quantity (the snapshot freezes what leaves
/// the warehouse), returned starts at zero. A non-positive
/// quantity is rejected - a DO never carries a zero or
/// negative line.
pub fn snapshot_items(
    inputs: &[DeliveryNoteItemInput],
    tenant_id: &str,
    delivery_note_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<DeliveryNoteItem>, String> {
    if inputs.is_empty() {
        return Err("A delivery note needs at least one item".to_string());
    }
    let mut items = Vec::with_capacity(inputs.len());
    for input in inputs {
        if input.quantity <= 0 {
            return Err(format!(
                "Item quantity must be above zero (got {} for \"{}\")",
                input.quantity, input.description
            ));
        }
        items.push(DeliveryNoteItem {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.to_string(),
            delivery_note_id,
            product_id: input.product_id.clone(),
            description: input.description.clone(),
            quantity_ordered: input.quantity,
            quantity_shipped: input.quantity,
            quantity_returned: 0,
            unit: input.unit.clone(),
            notes: input.notes.clone(),
            created_at: now,
            updated_at: now,
        });
    }
    Ok(items)
}

/// Escape text for the print-ready HTML document: `&`, `<`,
/// `>`, `"`, and `'` are escaped so snapshotted names and
/// addresses can never break out of the document markup.
pub fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render the print-ready HTML document for one DO: company /
/// sender header, receiver block, driver and vehicle, item
/// table with ordered / shipped / returned quantities, and the
/// signature block. This is the structured printable
/// representation the printing module queues as a paper / pdf
/// / email job; the same HTML is returned inline by the print
/// endpoint so a client can print even when the printing
/// module is not registered.
pub fn render_print_html(
    note: &DeliveryNote,
    items: &[DeliveryNoteItem],
    settings: &DeliveryNoteSettings,
) -> String {
    let mut rows = String::new();
    for (index, item) in items.iter().enumerate() {
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            index + 1,
            escape_html(&item.description),
            escape_html(&item.unit),
            item.quantity_ordered,
            item.quantity_shipped,
            item.quantity_returned,
            escape_html(item.notes.as_deref().unwrap_or("")),
        ));
    }
    let paper = settings
        .default_paper_size_code
        .as_deref()
        .unwrap_or(DEFAULT_PAPER_SIZE_CODE);
    let signature_block = match (note.recipient_name.as_deref(), note.recipient_signed_at) {
        (Some(name), Some(signed_at)) => format!(
            "<p>Received by: {} (signed {})</p>",
            escape_html(name),
            signed_at.to_rfc3339()
        ),
        (Some(name), None) => format!("<p>Received by: {}</p>", escape_html(name)),
        _ => "<p>Received by: ______________________ (name, signature, date)</p>".to_string(),
    };
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Delivery Note {number}</title>\
        <style>@page {{ size: {paper}; margin: 16mm; }} body {{ font-family: sans-serif; color: #111; }} \
        table {{ border-collapse: collapse; width: 100%; }} th, td {{ border: 1px solid #999; padding: 6px 8px; text-align: left; }} \
        th {{ background: #eee; }}</style></head><body>\
        <h1>Delivery Note {number}</h1>\
        <p>Status: {status}</p>\
        <h2>Sender</h2><p>{sender_name}<br>{sender_address}</p>\
        <h2>Receiver</h2><p>{receiver_name}<br>{receiver_address}</p>\
        <p>Warehouse: {warehouse}<br>Driver: {driver}<br>Vehicle: {vehicle}<br>Shipment: {shipment}</p>\
        <table><thead><tr><th>#</th><th>Description</th><th>Unit</th><th>Ordered</th><th>Shipped</th><th>Returned</th><th>Notes</th></tr></thead><tbody>{rows}</tbody></table>\
        <p>Total ordered: {ordered} &middot; Total shipped: {shipped} &middot; Total returned: {returned}</p>\
        {signature_block}\
        </body></html>",
        number = escape_html(&note.number),
        paper = escape_html(paper),
        status = note.status.as_str(),
        sender_name = escape_html(&note.sender.name),
        sender_address = escape_html(&note.sender.address),
        receiver_name = escape_html(&note.receiver.name),
        receiver_address = escape_html(&note.receiver.address),
        warehouse = escape_html(note.warehouse_name.as_deref().unwrap_or("-")),
        driver = escape_html(note.driver_name.as_deref().unwrap_or("-")),
        vehicle = escape_html(
            note.vehicle_plate
                .as_deref()
                .unwrap_or("-")
        ),
        shipment = escape_html(note.tracking_number.as_deref().unwrap_or("-")),
        rows = rows,
        ordered = total_ordered(items),
        shipped = total_shipped(items),
        returned = total_returned(items),
        signature_block = signature_block,
    )
}

/// Render the thermal lines for one DO as JSON values shaped
/// like the printing module's `ThermalLine` objects (`text`,
/// `align`, `bold`, `double_size`), so the print endpoint can
/// queue a thermal job through `printing.job.requested` without
/// depending on the printing module's Rust types.
pub fn render_thermal_lines(
    note: &DeliveryNote,
    items: &[DeliveryNoteItem],
) -> Vec<serde_json::Value> {
    let mut lines: Vec<serde_json::Value> = Vec::new();
    let line = |text: String, align: &str, bold: bool, double_size: bool| {
        serde_json::json!({
            "text": text,
            "align": align,
            "bold": bold,
            "double_size": double_size,
        })
    };
    lines.push(line("DELIVERY NOTE".to_string(), "center", true, true));
    lines.push(line(note.number.clone(), "center", true, false));
    lines.push(line(
        format!("Status: {}", note.status.as_str()),
        "left",
        false,
        false,
    ));
    lines.push(line(
        format!("From: {}", note.sender.name),
        "left",
        false,
        false,
    ));
    lines.push(line(
        format!("To: {}", note.receiver.name),
        "left",
        false,
        false,
    ));
    if let Some(driver) = &note.driver_name {
        lines.push(line(format!("Driver: {driver}"), "left", false, false));
    }
    if let Some(plate) = &note.vehicle_plate {
        lines.push(line(format!("Vehicle: {plate}"), "left", false, false));
    }
    for item in items {
        lines.push(line(
            format!(
                "{} x {} {} (shipped {})",
                item.description, item.quantity_ordered, item.unit, item.quantity_shipped
            ),
            "left",
            false,
            false,
        ));
    }
    lines.push(line(
        format!("Total shipped: {}", total_shipped(items)),
        "left",
        true,
        false,
    ));
    lines
}

// Types

/// DO status. Serialized snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryNoteStatus {
    Draft,
    Issued,
    InTransit,
    Delivered,
    Returned,
    Cancelled,
}

impl Default for DeliveryNoteStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl DeliveryNoteStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Issued => "issued",
            Self::InTransit => "in_transit",
            Self::Delivered => "delivered",
            Self::Returned => "returned",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "issued" => Some(Self::Issued),
            "in_transit" => Some(Self::InTransit),
            "delivered" => Some(Self::Delivered),
            "returned" => Some(Self::Returned),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Terminal statuses: no transition leaves them.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Returned | Self::Cancelled)
    }
}

/// One party of a DO (sender or receiver), snapshotted at
/// creation time so later edits to a shipment, order, or
/// company record never rewrite the document's history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryParty {
    pub name: String,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// One line item of a DO. `product_id` references a product
/// by id only (no foreign key - products live in the store /
/// assets modules). `quantity_ordered` and
/// `quantity_shipped` are frozen at creation; only
/// `quantity_returned` grows, through the return endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryNoteItem {
    pub id: Uuid,
    pub tenant_id: String,
    pub delivery_note_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub description: String,
    pub quantity_ordered: i64,
    pub quantity_shipped: i64,
    pub quantity_returned: i64,
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One delivery note. `shipment_id` links the
/// logistics shipment by id only (no foreign key - shipments
/// live in the logistics module); `rental_order_id` links a
/// rental order the same way when the shipment came from a
/// rental; `company_id` links the issuing company by id only.
/// The sender, receiver, addresses, driver, vehicle, and
/// warehouse are snapshots taken at creation time. Delivery
/// proof is a logistics `ProofOfDelivery` id plus the
/// recipient signature metadata stored here (the logistics
/// proof itself may carry no signature - see the module
/// header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryNote {
    pub id: Uuid,
    pub tenant_id: String,
    pub number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_id: Option<Uuid>,
    /// `linked` (the shipment id was confirmed through the
    /// `logistics.shipment.lookup` hook query), `unverified`
    /// (no handler answered the query - see the module
    /// header), or `none` (no shipment linked).
    pub shipment_link_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rental_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    pub status: DeliveryNoteStatus,
    pub sender: DeliveryParty,
    pub receiver: DeliveryParty,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warehouse_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warehouse_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_plate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof_of_delivery_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_name: Option<String>,
    /// Storage reference (`target:key`) of the captured
    /// recipient signature bytes, when one was captured. The
    /// bytes live in the storage layer; only the reference is
    /// stored here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_signature_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_signed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_reason: Option<String>,
    /// `full`, `partial`, or `none` (see `return_scope`).
    pub return_scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returned_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-tenant delivery notes settings. The number format is
/// data: `number_prefix` + sequence, zero-padded to
/// `number_width`, restarting per `number_reset_rule`
/// (`yearly`, `monthly`, or `never`). `require_proof_for_delivery`
/// gates the delivered transition on a linked proof of
/// delivery; `auto_advance_from_shipment` lets logistics
/// shipment events move DOs forward (see `hooks.rs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryNoteSettings {
    pub number_prefix: String,
    pub number_width: i64,
    pub number_reset_rule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_template_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_paper_size_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_output_kind: Option<String>,
    pub require_proof_for_delivery: bool,
    pub auto_advance_from_shipment: bool,
}

impl Default for DeliveryNoteSettings {
    fn default() -> Self {
        Self {
            number_prefix: DEFAULT_NUMBER_PREFIX.to_string(),
            number_width: DEFAULT_NUMBER_WIDTH,
            number_reset_rule: DEFAULT_RESET_RULE.to_string(),
            default_template_id: None,
            default_paper_size_code: Some(DEFAULT_PAPER_SIZE_CODE.to_string()),
            default_output_kind: Some(DEFAULT_OUTPUT_KIND.to_string()),
            require_proof_for_delivery: true,
            auto_advance_from_shipment: true,
        }
    }
}

// Request bodies

/// One item input for DO creation: the product reference, a
/// description, the quantity leaving the warehouse, its unit,
/// and optional notes. The snapshot freezes ordered and
/// shipped at this quantity.
#[derive(Debug, Clone, Deserialize)]
pub struct DeliveryNoteItemInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub description: String,
    pub quantity: i64,
    #[serde(default = "default_unit")]
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

fn default_unit() -> String {
    "pcs".to_string()
}

/// Party input for DO creation (sender or receiver).
#[derive(Debug, Clone, Deserialize)]
pub struct DeliveryPartyInput {
    pub name: String,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDeliveryNoteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rental_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    pub sender: DeliveryPartyInput,
    pub receiver: DeliveryPartyInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warehouse_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warehouse_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_plate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub items: Vec<DeliveryNoteItemInput>,
}

#[derive(Debug, Deserialize)]
pub struct CreateFromShipmentRequest {
    pub sender: DeliveryPartyInput,
    pub receiver: DeliveryPartyInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warehouse_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warehouse_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_plate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rental_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub items: Vec<DeliveryNoteItemInput>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDeliveryNoteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<DeliveryPartyInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<DeliveryPartyInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warehouse_id: Option<Option<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warehouse_name: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_id: Option<Option<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_name: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_id: Option<Option<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_plate: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_type: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rental_order_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Option<Uuid>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateItemRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub description: String,
    pub quantity: i64,
    #[serde(default = "default_unit")]
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateItemRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_ordered: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_shipped: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<Option<String>>,
}

/// Advance a DO through the status machine. `target` names
/// the status to move to; when omitted the DO moves to its
/// next forward status (draft -> issued, issued ->
/// in_transit, in_transit -> delivered). `delivered` through
/// this endpoint still needs a linked proof of delivery -
/// use the deliver endpoint to attach one.
#[derive(Debug, Deserialize)]
pub struct AdvanceRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Deliver a DO: link the logistics proof of delivery and
/// record the recipient signature metadata. The logistics
/// `ProofOfDelivery` carries a signature storage reference
/// but no signer name or signed-at time, so the DO stores
/// the recipient name, the signature reference, and the
/// signed-at time itself.
#[derive(Debug, Deserialize)]
pub struct DeliverRequest {
    pub proof_of_delivery_id: Uuid,
    pub recipient_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_signature_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_signed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Return a DO (partial or full): per-item returned
/// quantities plus the reason. The return scope is computed
/// from the quantities, never taken from the caller.
#[derive(Debug, Deserialize)]
pub struct ReturnRequest {
    pub reason: String,
    #[serde(default)]
    pub items: Vec<ReturnedItemInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rental_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One returned line: the item id and how many units came back.
#[derive(Debug, Deserialize)]
pub struct ReturnedItemInput {
    pub item_id: Uuid,
    pub quantity: i64,
}

#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Print a DO: queue a print job through the printing module
/// and return the print-ready document inline. All fields are
/// optional - settings supply the defaults.
#[derive(Debug, Deserialize)]
pub struct PrintRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper_size_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printer_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_width: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_reset_rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_template_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_paper_size_code: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_output_kind: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_proof_for_delivery: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_advance_from_shipment: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: Uuid, ordered: i64, shipped: i64, returned: i64) -> DeliveryNoteItem {
        DeliveryNoteItem {
            id,
            tenant_id: "tenant-1".to_string(),
            delivery_note_id: Uuid::new_v4(),
            product_id: None,
            description: "Widget".to_string(),
            quantity_ordered: ordered,
            quantity_shipped: shipped,
            quantity_returned: returned,
            unit: "pcs".to_string(),
            notes: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn note(status: DeliveryNoteStatus) -> DeliveryNote {
        DeliveryNote {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            number: "SJ-2026-0001".to_string(),
            shipment_id: None,
            shipment_link_status: "none".to_string(),
            order_id: None,
            rental_order_id: None,
            company_id: None,
            status,
            sender: DeliveryParty {
                name: "Warehouse A".to_string(),
                address: "1 Depot Road".to_string(),
                phone: None,
                email: None,
            },
            receiver: DeliveryParty {
                name: "Customer".to_string(),
                address: "2 Market Street".to_string(),
                phone: None,
                email: None,
            },
            warehouse_id: None,
            warehouse_name: None,
            driver_id: None,
            driver_name: None,
            vehicle_id: None,
            vehicle_plate: None,
            vehicle_type: None,
            tracking_number: None,
            proof_of_delivery_id: None,
            recipient_name: None,
            recipient_signature_ref: None,
            recipient_signed_at: None,
            notes: None,
            return_reason: None,
            return_scope: "none".to_string(),
            returned_at: None,
            issued_at: None,
            delivered_at: None,
            cancelled_at: None,
            created_by: "user-1".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn status_machine_transitions() {
        use DeliveryNoteStatus::{Cancelled, Delivered, Draft, InTransit, Issued, Returned};
        assert!(can_transition(Draft, Issued));
        assert!(can_transition(Draft, Cancelled));
        assert!(!can_transition(Draft, InTransit));
        assert!(!can_transition(Draft, Delivered));
        assert!(!can_transition(Draft, Returned));
        assert!(can_transition(Issued, InTransit));
        assert!(can_transition(Issued, Cancelled));
        assert!(!can_transition(Issued, Delivered));
        assert!(!can_transition(Issued, Returned));
        assert!(can_transition(InTransit, Delivered));
        assert!(can_transition(InTransit, Returned));
        assert!(can_transition(InTransit, Cancelled));
        assert!(!can_transition(InTransit, Issued));
        assert!(can_transition(Delivered, Returned));
        assert!(!can_transition(Delivered, Cancelled));
        assert!(!can_transition(Delivered, InTransit));
        assert!(!can_transition(Returned, Delivered));
        assert!(!can_transition(Cancelled, Issued));
        assert!(!can_transition(Draft, Draft));
        assert!(Returned.is_terminal());
        assert!(Cancelled.is_terminal());
        assert!(!Draft.is_terminal());
        assert!(!Delivered.is_terminal());
    }

    #[test]
    fn status_round_trips() {
        for (status, text) in [
            (DeliveryNoteStatus::Draft, "draft"),
            (DeliveryNoteStatus::Issued, "issued"),
            (DeliveryNoteStatus::InTransit, "in_transit"),
            (DeliveryNoteStatus::Delivered, "delivered"),
            (DeliveryNoteStatus::Returned, "returned"),
            (DeliveryNoteStatus::Cancelled, "cancelled"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(DeliveryNoteStatus::from_str(text), Some(status));
        }
        assert_eq!(DeliveryNoteStatus::from_str("shipped"), None);
    }

    #[test]
    fn number_period_keys() {
        use chrono::TimeZone;
        let jan = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let nov = Utc.with_ymd_and_hms(2026, 11, 22, 0, 0, 0).unwrap();
        assert_eq!(number_period_key("yearly", jan), "2026");
        assert_eq!(number_period_key("monthly", jan), "2026-01");
        assert_eq!(number_period_key("monthly", nov), "2026-11");
        assert_eq!(number_period_key("never", nov), "all");
        // Unknown rule falls back to yearly.
        assert_eq!(number_period_key("weekly", nov), "2026");
    }

    #[test]
    fn number_formatting() {
        assert_eq!(format_number("SJ", "yearly", 4, "2026", 1), "SJ-2026-0001");
        assert_eq!(
            format_number("SJ", "monthly", 4, "2026-11", 42),
            "SJ-2026-11-0042"
        );
        assert_eq!(format_number("SJ", "never", 4, "all", 7), "SJ-0007");
        assert_eq!(
            format_number("DO", "yearly", 6, "2026", 3),
            "DO-2026-000003"
        );
        // A sequence past the width grows instead of truncating.
        assert_eq!(
            format_number("SJ", "yearly", 4, "2026", 12345),
            "SJ-2026-12345"
        );
        // Width clamps to 1..=8.
        assert_eq!(format_number("SJ", "never", 0, "all", 5), "SJ-5");
        assert_eq!(format_number("SJ", "never", 99, "all", 5), "SJ-00000005");
    }

    #[test]
    fn prefix_validation() {
        assert!(valid_prefix("SJ"));
        assert!(valid_prefix("DO-1"));
        assert!(valid_prefix("SJ2026"));
        assert!(!valid_prefix("sj"));
        assert!(!valid_prefix(""));
        assert!(!valid_prefix("-SJ"));
        assert!(!valid_prefix("SJ-"));
        assert!(!valid_prefix("SJ--1"));
        assert!(!valid_prefix("S J"));
        assert!(!valid_prefix(&"S".repeat(17)));
    }

    #[test]
    fn quantity_totals_and_return_scope() {
        let items = vec![
            item(Uuid::new_v4(), 10, 8, 0),
            item(Uuid::new_v4(), 5, 5, 2),
        ];
        assert_eq!(total_ordered(&items), 15);
        assert_eq!(total_shipped(&items), 13);
        assert_eq!(total_returned(&items), 2);
        assert_eq!(return_scope(&items), "partial");
        let full = vec![item(Uuid::new_v4(), 4, 4, 4)];
        assert_eq!(return_scope(&full), "full");
        assert!(RETURN_SCOPES.contains(&return_scope(&full)));
        let none = vec![item(Uuid::new_v4(), 4, 4, 0)];
        assert_eq!(return_scope(&none), "none");
        assert!(RETURN_SCOPES.contains(&return_scope(&none)));
        let empty: Vec<DeliveryNoteItem> = vec![];
        assert_eq!(return_scope(&empty), "none");
        assert_eq!(total_shipped(&empty), 0);
    }

    #[test]
    fn return_quantity_validation() {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let items = vec![item(first, 10, 10, 0), item(second, 5, 3, 0)];
        // Partial return.
        assert_eq!(
            validate_return_quantities(&items, &[(first, 4)]).unwrap(),
            "partial"
        );
        // Full return across both lines.
        assert_eq!(
            validate_return_quantities(&items, &[(first, 10), (second, 3)]).unwrap(),
            "full"
        );
        // Over-return is rejected.
        assert!(validate_return_quantities(&items, &[(second, 4)]).is_err());
        // Negative is rejected.
        assert!(validate_return_quantities(&items, &[(first, -1)]).is_err());
        // Unknown item is rejected.
        assert!(validate_return_quantities(&items, &[(Uuid::new_v4(), 1)]).is_err());
        // Empty return and all-zero return are rejected.
        assert!(validate_return_quantities(&items, &[]).is_err());
        assert!(validate_return_quantities(&items, &[(first, 0)]).is_err());
    }

    #[test]
    fn snapshot_freezes_ordered_and_shipped() {
        let now = Utc::now();
        let note_id = Uuid::new_v4();
        let inputs = vec![
            DeliveryNoteItemInput {
                product_id: Some("prod-1".to_string()),
                description: "Widget".to_string(),
                quantity: 3,
                unit: "box".to_string(),
                notes: Some("Fragile".to_string()),
            },
            DeliveryNoteItemInput {
                product_id: None,
                description: "Cable".to_string(),
                quantity: 12,
                unit: "pcs".to_string(),
                notes: None,
            },
        ];
        let items = snapshot_items(&inputs, "tenant-1", note_id, now).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].quantity_ordered, 3);
        assert_eq!(items[0].quantity_shipped, 3);
        assert_eq!(items[0].quantity_returned, 0);
        assert_eq!(items[0].delivery_note_id, note_id);
        assert_eq!(items[1].quantity_ordered, 12);
        assert_eq!(items[1].unit, "pcs");
        // Empty input and non-positive quantities are rejected.
        assert!(snapshot_items(&[], "tenant-1", note_id, now).is_err());
        let bad = vec![DeliveryNoteItemInput {
            product_id: None,
            description: "Bad".to_string(),
            quantity: 0,
            unit: "pcs".to_string(),
            notes: None,
        }];
        assert!(snapshot_items(&bad, "tenant-1", note_id, now).is_err());
    }

    #[test]
    fn html_escaping() {
        assert_eq!(
            escape_html("<b>Tom & Jerry</b> \"quoted\""),
            "&lt;b&gt;Tom &amp; Jerry&lt;/b&gt; &quot;quoted&quot;"
        );
        assert_eq!(escape_html("plain"), "plain");
        assert_eq!(escape_html("it's"), "it&#39;s");
    }

    #[test]
    fn print_html_contains_snapshot_and_totals() {
        let mut note_data = note(DeliveryNoteStatus::Issued);
        note_data.warehouse_name = Some("Central Warehouse".to_string());
        note_data.driver_name = Some("Dedi".to_string());
        note_data.vehicle_plate = Some("B 1234 CD".to_string());
        let items = vec![
            item(Uuid::new_v4(), 10, 8, 0),
            item(Uuid::new_v4(), 5, 5, 1),
        ];
        let html = render_print_html(&note_data, &items, &DeliveryNoteSettings::default());
        assert!(html.contains("SJ-2026-0001"));
        assert!(html.contains("Central Warehouse"));
        assert!(html.contains("Dedi"));
        assert!(html.contains("B 1234 CD"));
        assert!(html.contains("Total ordered: 15"));
        assert!(html.contains("Total shipped: 13"));
        assert!(html.contains("Total returned: 1"));
        // No signature yet: the blank signature block is shown.
        assert!(html.contains("Received by: ______________________"));
    }

    #[test]
    fn thermal_lines_carry_note_and_items() {
        let note_data = note(DeliveryNoteStatus::Issued);
        let items = vec![item(Uuid::new_v4(), 2, 2, 0)];
        let lines = render_thermal_lines(&note_data, &items);
        let joined: Vec<String> = lines
            .iter()
            .filter_map(|line| {
                line.get("text")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();
        assert!(joined.iter().any(|text| text.contains("SJ-2026-0001")));
        assert!(joined.iter().any(|text| text.contains("Total shipped: 2")));
    }

    #[test]
    fn settings_defaults_are_safe() {
        let settings = DeliveryNoteSettings::default();
        assert_eq!(settings.number_prefix, "SJ");
        assert_eq!(settings.number_width, 4);
        assert_eq!(settings.number_reset_rule, "yearly");
        assert!(settings.require_proof_for_delivery);
        assert!(settings.auto_advance_from_shipment);
        assert_eq!(settings.default_paper_size_code.as_deref(), Some("A4"));
        assert_eq!(settings.default_output_kind.as_deref(), Some("paper"));
    }
}
