// Store module - digital product rules and entitlement helpers.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::HttpRequest;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use super::helpers::{div_rounded, saturating_i64};
use super::state::{StoreState, new_id, now};
use super::types::{DEFAULT_DIGITAL_DOWNLOAD_LIMIT, DEFAULT_SAMPLE_RATE_LIMIT_PER_HOUR, DEFAULT_SAMPLE_RATE_LIMIT_WINDOW_MINUTES, DigitalProduct, DigitalProductFile, DigitalProductType, DigitalRoyalty, DownloadEntitlement, Order, OrderItem, PERM_DIGITAL_MANAGE, PERM_DIGITAL_READ, WatermarkFormat, default_proration_policy};

// Digital products: seeds, capability checks, validation, watermarking

/// Seed rows for the digital type registry. Configurable defaults
/// only: administrators can add, rename, disable, or remove types
/// through the type endpoints, and the migration seeds the same
/// five rows as data so the database copy matches this seed.
pub(crate) fn seed_digital_types() -> Vec<DigitalProductType> {
    let timestamp = now();
    [
        (
            "ebook",
            "Ebook",
            "A digital book delivered as a document file.",
        ),
        (
            "audiobook",
            "Audiobook",
            "A narrated audio edition of a book.",
        ),
        (
            "template",
            "Template",
            "A reusable document, design, or code template.",
        ),
        (
            "course-pack",
            "Course pack",
            "A bundled set of course materials.",
        ),
        (
            "digital-asset",
            "Digital asset",
            "A downloadable digital asset such as graphics, fonts, or data.",
        ),
    ]
    .into_iter()
    .map(|(id, label, description)| DigitalProductType {
        id: id.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        enabled: true,
        created_at: timestamp,
        updated_at: timestamp,
    })
    .collect()
}

/// Seed rows for the watermark format registry. Configurable
/// defaults only: administrators can add formats or change what
/// metadata is embedded through the watermark-format endpoints, and
/// the migration seeds the same rows as data.
pub(crate) fn seed_watermark_formats() -> Vec<WatermarkFormat> {
    let timestamp = now();
    [
        (
            "pdf",
            true,
            vec!["Author", "Creator", "Keywords"],
            "Buyer identity is written into the PDF document information dictionary.",
        ),
        (
            "epub",
            true,
            vec!["dc:identifier", "dc:creator"],
            "Buyer identity is written into the EPUB package metadata.",
        ),
        (
            "mobi",
            true,
            vec!["EXTH author", "EXTH identifier"],
            "Buyer identity is written into the MOBI EXTH records where the writer supports them.",
        ),
        (
            "mp3",
            true,
            vec!["ID3 comment", "ID3 artist"],
            "Buyer identity is written into the ID3 comment frame.",
        ),
        (
            "m4b",
            true,
            vec!["copyright", "comment"],
            "Buyer identity is written into the audiobook metadata atoms.",
        ),
        (
            "mp4",
            true,
            vec!["copyright", "comment"],
            "Buyer identity is written into the video metadata atoms.",
        ),
        (
            "zip",
            false,
            Vec::<&str>::new(),
            "Archives carry no standard document metadata; the buyer is recorded in the delivery manifest instead.",
        ),
    ]
    .into_iter()
    .map(|(format, metadata_supported, metadata_fields, notes)| WatermarkFormat {
        format: format.to_string(),
        metadata_supported,
        metadata_fields: metadata_fields
            .into_iter()
            .map(str::to_string)
            .collect(),
        notes: notes.to_string(),
        updated_at: timestamp,
    })
    .collect()
}

/// Caller identity for the capability-checked digital routes, from
/// the headers every capability-checked module in this codebase
/// uses: `X-User-Id` identifies the actor, `X-Role` carries the
/// role slug or id the capability check runs against.
pub(crate) struct DigitalCaller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn digital_caller(req: &HttpRequest) -> UnifiedResult<DigitalCaller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(DigitalCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
/// The store keeps only the hook bus (no `AppContext`), so the check
/// runs through `state.bus().emit`, the same hook other modules
/// reach through the context.
pub(crate) fn digital_has_capability(state: &StoreState, role: &str, permission: &str) -> bool {
    let context = state.bus().emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_digital_permission(
    state: &StoreState,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<DigitalCaller> {
    let caller = digital_caller(req)?;
    if digital_has_capability(state, &caller.role, permission) {
        Ok(caller)
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{}\"",
            caller.role, permission
        )))
    }
}

pub(crate) fn require_digital_manage(
    state: &StoreState,
    req: &HttpRequest,
) -> UnifiedResult<DigitalCaller> {
    require_digital_permission(state, req, PERM_DIGITAL_MANAGE)
}

pub(crate) fn require_digital_read(
    state: &StoreState,
    req: &HttpRequest,
) -> UnifiedResult<DigitalCaller> {
    require_digital_permission(state, req, PERM_DIGITAL_READ)
}

/// True when the caller holds the manage permission (used to widen
/// a self-service read to another user's records). Missing identity
/// headers mean "no", never an error: this is a widening check, not
/// the gate itself.
pub(crate) fn digital_can_manage(state: &StoreState, req: &HttpRequest) -> bool {
    let Ok(caller) = digital_caller(req) else {
        return false;
    };
    digital_has_capability(state, &caller.role, PERM_DIGITAL_MANAGE)
}

/// Gate for the intentionally public storefront reads (public
/// digital catalog, previews, sample download). The API layer still
/// denies these endpoints by default until an administrator creates
/// an allow rule (see `api_manager`); inside the handler, a caller
/// that presents a role must hold `store.digital:read`, while an
/// anonymous caller only ever sees published products and
/// seller-allowed samples. The check runs on every call so the
/// public surface is never an unguarded code path.
pub(crate) fn require_digital_public(state: &StoreState, req: &HttpRequest) -> UnifiedResult<()> {
    let Some(role) = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if digital_has_capability(state, role, PERM_DIGITAL_READ) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_DIGITAL_READ}\""
        )))
    }
}

pub(crate) fn valid_type_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn valid_format(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 12
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

pub(crate) fn valid_isbn(value: &str) -> bool {
    let compact: String = value
        .chars()
        .filter(|c| *c != '-' && *c != ' ')
        .collect();
    if compact.len() != 10 && compact.len() != 13 {
        return false;
    }
    let (head, tail) = compact.split_at(compact.len() - 1);
    if !head.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let tail = tail.chars().next().expect("split leaves one char");
    if compact.len() == 10 {
        tail.is_ascii_digit() || tail == 'X' || tail == 'x'
    } else {
        tail.is_ascii_digit()
    }
}

pub(crate) fn valid_language(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() > 2 {
        return false;
    }
    let language = parts[0];
    if language.len() < 2
        || language.len() > 3
        || !language.chars().all(|c| c.is_ascii_lowercase())
    {
        return false;
    }
    match parts.get(1) {
        None => true,
        Some(region) => {
            region.len() == 2 && region.chars().all(|c| c.is_ascii_uppercase())
        }
    }
}

pub(crate) fn valid_royalty_kind(kind: &str) -> bool {
    matches!(kind, "percent" | "fixed")
}

/// Validate a royalty rule: the kind must be known and the value
/// for that kind must be in range. Percent is basis points
/// (1..=10000); fixed is minor units (> 0). The other field must be
/// zero, so a rule never carries two meanings at once.
pub(crate) fn validate_royalty(kind: &str, percent_bp: i64, fixed_minor: i64) -> UnifiedResult<()> {
    if !valid_royalty_kind(kind) {
        return Err(UnifiedError::bad_request(
            "Royalty kind must be one of: percent, fixed",
        ));
    }
    if kind == "percent" {
        if percent_bp < 0 || percent_bp > 10_000 {
            return Err(UnifiedError::bad_request(
                "Percentage royalty must be between 0 and 10000 basis points (0% to 100%)",
            ));
        }
        if fixed_minor != 0 {
            return Err(UnifiedError::bad_request(
                "A percent royalty must not carry a fixed amount",
            ));
        }
    } else if fixed_minor < 0 {
        return Err(UnifiedError::bad_request(
            "Fixed royalty must be a non-negative amount in minor units",
        ));
    } else if percent_bp != 0 {
        return Err(UnifiedError::bad_request(
            "A fixed royalty must not carry a percent amount",
        ));
    }
    Ok(())
}

pub(crate) fn validate_digital_metadata(
    isbn: Option<&str>,
    language: Option<&str>,
    pages: Option<u32>,
    formats: Option<&[String]>,
) -> UnifiedResult<()> {
    if let Some(isbn) = isbn {
        if !isbn.trim().is_empty() && !valid_isbn(isbn.trim()) {
            return Err(UnifiedError::bad_request(
                "ISBN must be a valid ISBN-10 or ISBN-13 (digits, with an optional trailing X for ISBN-10)",
            ));
        }
    }
    if let Some(language) = language {
        if !language.trim().is_empty() && !valid_language(language.trim()) {
            return Err(UnifiedError::bad_request(
                "Language must be a tag such as en or id-ID",
            ));
        }
    }
    if let Some(pages) = pages {
        if pages == 0 {
            return Err(UnifiedError::bad_request(
                "Page count must be at least 1 when set",
            ));
        }
    }
    if let Some(formats) = formats {
        if formats.len() > 16 {
            return Err(UnifiedError::bad_request(
                "At most 16 formats can be listed for a digital product",
            ));
        }
        for format in formats {
            if !valid_format(&format.to_lowercase()) {
                return Err(UnifiedError::bad_request(
                    "Formats must be lowercase alphanumeric tokens such as pdf or epub",
                ));
            }
        }
    }
    Ok(())
}

/// Searchable text for a digital product: its metadata plus the
/// linked store product's name and description.
pub(crate) fn digital_search_text(state: &StoreState, digital: &DigitalProduct) -> String {
    let product_text = state
        .products
        .get(&digital.product_id)
        .map(|entry| format!("{} {}", entry.value().name, entry.value().description))
        .unwrap_or_default();
    format!(
        "{} {} {} {} {} {}",
        product_text,
        digital.type_id,
        digital.author.clone().unwrap_or_default(),
        digital.publisher.clone().unwrap_or_default(),
        digital.edition.clone().unwrap_or_default(),
        digital.isbn.clone().unwrap_or_default()
    )
}

/// Purchase grant id carried as the `license_id` of a digital
/// download entitlement. Digital products have no license; the
/// grant id (one per order line) plays the `license_id` role in the
/// signed URL so the existing HMAC-signed download flow serves
/// digital files unchanged, with per-file download limits.
pub(crate) fn digital_grant_id(order_id: &str, product_id: &str) -> String {
    format!("digital:{order_id}:{product_id}")
}

/// Storage reference of a digital file by its file id, for the
/// signed-download response (the catalog lookup only knows software
/// releases, so digital files resolve here).
pub(crate) fn digital_file_storage_id(state: &StoreState, file_id: &str) -> Option<String> {
    state
        .digital_files
        .get(file_id)
        .map(|entry| entry.value().storage_file_id.clone())
}

/// Watermark descriptor for one delivered digital file: the metadata
/// fields the delivery layer must embed, carrying the buyer identity.
///
/// HONESTY NOTE (read before changing this): watermarking is
/// metadata-level only. It embeds the buyer identity into file
/// metadata where the format supports it (see the watermark format
/// registry, seeded with PDF, EPUB, MOBI, MP3, M4B and MP4, and
/// editable by administrators). It does NOT prevent copying or
/// redistribution, it is NOT unbreakable DRM, and it must never be
/// described as DRM anywhere. Formats without standard document
/// metadata (archives, installers) record the buyer in the delivery
/// manifest instead and say so in the descriptor.
pub(crate) fn watermark_descriptor(
    state: &StoreState,
    format: &str,
    buyer_id: &str,
    order_id: &str,
) -> Value {
    const LIMITATION: &str = "Watermarking embeds the buyer identity into file metadata only. It does not prevent copying or redistribution and is not unbreakable DRM.";
    let format = format.to_lowercase();
    let enabled = state
        .digital_defaults
        .read()
        .map(|defaults| defaults.watermark_enabled)
        .unwrap_or(true);
    let Some(entry) = state.watermark_formats.get(format.as_str()) else {
        return json!({
            "format": format,
            "metadata_supported": false,
            "buyer": buyer_id,
            "order_id": order_id,
            "fields": {},
            "limitation": "Unknown file format: no metadata watermark is defined for this format. The buyer is recorded in the download history only. Watermarking is metadata-level only and is not unbreakable DRM.",
        });
    };
    let format_entry = entry.value().clone();
    drop(entry);
    if !enabled || !format_entry.metadata_supported {
        return json!({
            "format": format,
            "metadata_supported": false,
            "buyer": buyer_id,
            "order_id": order_id,
            "fields": {},
            "limitation": if enabled { format_entry.notes } else { "Watermarking is disabled in the digital settings.".to_string() },
        });
    }
    let mut fields = serde_json::Map::new();
    for field in &format_entry.metadata_fields {
        fields.insert(
            field.clone(),
            json!(format!("Licensed to {buyer_id} - order {order_id}")),
        );
    }
    json!({
        "format": format,
        "metadata_supported": true,
        "buyer": buyer_id,
        "order_id": order_id,
        "fields": fields,
        "limitation": LIMITATION,
    })
}

/// Mint (or return the existing) download entitlement for one
/// digital file of a paid order. One entitlement per file, so each
/// file has its own download limit; the purchase grant id is shared
/// across the files of the same order line. Returns the entitlement
/// and whether it was newly created.
pub(crate) fn ensure_digital_entitlement(
    state: &StoreState,
    order: &Order,
    digital: &DigitalProduct,
    file: &DigitalProductFile,
) -> (DownloadEntitlement, bool) {
    if let Some(existing) = state
        .entitlements
        .iter()
        .find(|entry| {
            let entitlement = entry.value();
            entitlement.order_id == order.id
                && entitlement.product_id == digital.product_id
                && entitlement.release_id.as_deref() == Some(file.id.as_str())
        })
        .map(|entry| entry.value().clone())
    {
        return (existing, false);
    }
    (mint_digital_entitlement(state, order, digital, file), true)
}

/// Build and store the entitlement for one digital file of a paid
/// order. Shared by `ensure_digital_entitlement` (single file) and
/// the batched grant below, so the minting rule lives in one place:
/// callers that already know the entitlement does not exist skip
/// the scan and mint directly.
fn mint_digital_entitlement(
    state: &StoreState,
    order: &Order,
    digital: &DigitalProduct,
    file: &DigitalProductFile,
) -> DownloadEntitlement {
    let grant_id = digital_grant_id(&order.id, &digital.product_id);
    let limit = if digital.download_limit > 0 {
        digital.download_limit
    } else {
        state
            .digital_defaults
            .read()
            .map(|defaults| defaults.default_download_limit)
            .unwrap_or(DEFAULT_DIGITAL_DOWNLOAD_LIMIT)
    };
    let entitlement = DownloadEntitlement {
        id: new_id(),
        license_id: Some(grant_id),
        license_key: None,
        order_id: order.id.clone(),
        user_id: order.user_id.clone(),
        product_id: digital.product_id.clone(),
        tier_id: None,
        software_id: None,
        software_slug: None,
        release_id: Some(file.id.clone()),
        download_limit: limit,
        downloads_used: 0,
        status: "active".to_string(),
        expires_at: None,
        created_at: now(),
        updated_at: now(),
    };
    state
        .entitlements
        .insert(entitlement.id.clone(), entitlement.clone());
    entitlement
}

/// Grant downloads and accrue royalties for the digital products in
/// a paid order. Called from every place an order becomes paid (the
/// `payment.captured` listener, the manual pay path, and the manual
/// status update), so a digital purchase always unlocks its files
/// and always accrues its royalty exactly once: minting is
/// idempotent (an existing entitlement for the same order line and
/// file is returned, never duplicated) and royalty accrual skips
/// lines that already accrued for the order (one royalty row per
/// digital product per order). Royalties accrue on the net line
/// total: the order discount is shared across lines in proportion
/// to their line totals, and a fixed royalty is charged per unit
/// sold. A product with a zero royalty value accrues nothing.
pub(crate) fn grant_digital_purchases_for_paid_order(state: &StoreState, order: &Order) {
    let mut files_granted = 0usize;
    let mut royalties_accrued = 0usize;
    // One pass over each table up front. Before this change every
    // order line rescanned the products, files and royalties tables
    // in full, and every file rescanned entitlements, so granting an
    // order cost items x (products + files + royalties) full-map
    // scans. The lookups below are O(1) instead. Group order is the
    // same encounter order the per-line filters saw (first match
    // wins for a duplicated product id, as before).
    let products_by_product_id: HashMap<&str, &DigitalProduct> = {
        let mut index = HashMap::new();
        for entry in state.digital_products.iter() {
            let product = entry.value();
            index
                .entry(product.product_id.as_str())
                .or_insert(product);
        }
        index
    };
    let mut files_by_product: HashMap<&str, Vec<&DigitalProductFile>> = HashMap::new();
    for entry in state.digital_files.iter() {
        let file = entry.value();
        files_by_product
            .entry(file.digital_product_id.as_str())
            .or_default()
            .push(file);
    }
    // Digital product ids that already accrued a royalty for this
    // order. The order id is the same for every lookup in this call,
    // so it is not part of the key. Owned ids: the set outlives the
    // scan that built it while the map gains rows during granting,
    // so borrowed keys would be unsound here.
    let mut accrued: HashSet<String> = state
        .digital_royalties
        .iter()
        .filter(|entry| entry.value().order_id == order.id)
        .map(|entry| entry.value().digital_product_id.clone())
        .collect();
    // (order id, product id, file id) triples that already hold an
    // entitlement. Owned keys: the set outlives the scan that built
    // it while the map gains rows during granting, so borrowed keys
    // would be unsound here.
    let mut entitled: HashSet<(String, String, Option<String>)> = state
        .entitlements
        .iter()
        .filter(|entry| entry.value().order_id == order.id)
        .map(|entry| {
            let entitlement = entry.value();
            (
                entitlement.order_id.clone(),
                entitlement.product_id.clone(),
                entitlement.release_id.clone(),
            )
        })
        .collect();
    for item in &order.items {
        let Some(digital) = products_by_product_id.get(item.product_id.as_str()) else {
            continue;
        };
        let digital = *digital;
        if let Some(files) = files_by_product.get(digital.id.as_str()) {
            for file in files {
                let key = (
                    order.id.clone(),
                    digital.product_id.clone(),
                    Some(file.id.clone()),
                );
                if entitled.contains(&key) {
                    continue;
                }
                // The set already proves no entitlement exists for
                // this file, so mint directly instead of rescanning
                // entitlements through `ensure_digital_entitlement`.
                mint_digital_entitlement(state, order, digital, file);
                entitled.insert(key);
                files_granted += 1;
            }
        }
        if digital.royalty_percent_bp <= 0 && digital.royalty_fixed_minor <= 0 {
            continue;
        }
        if accrued.contains(&digital.id) {
            continue;
        }
        // The order discount is shared across lines in proportion
        // to their line totals, rounded once to minor units (the
        // integer equivalent of the old `round_money` share math).
        let net = if order.subtotal > 0 {
            item.line_total
                - div_rounded(
                    i128::from(order.discount_total) * i128::from(item.line_total),
                    i128::from(order.subtotal),
                ) as i64
        } else {
            item.line_total
        };
        // Integer math only: the fixed branch is already minor
        // units; the percent branch computes `net * bp / 10_000`
        // with `i128` intermediates, so no float ever touches the
        // royalty amount.
        let amount = match digital.royalty_kind.as_str() {
            "fixed" => digital
                .royalty_fixed_minor
                .saturating_mul(i64::from(item.quantity)),
            _ => saturating_i64(div_rounded(
                i128::from(net) * i128::from(digital.royalty_percent_bp),
                10_000,
            )),
        };
        if amount <= 0 {
            continue;
        }
        let royalty = DigitalRoyalty {
            id: new_id(),
            digital_product_id: digital.id.clone(),
            product_id: item.product_id.clone(),
            order_id: order.id.clone(),
            buyer_user_id: order.user_id.clone(),
            payee: digital.royalty_payee.clone(),
            payee_company_id: digital
                .royalty_payee_company_id
                .clone()
                .or_else(|| item.company_id.clone()),
            amount,
            currency: order.currency.clone(),
            royalty_kind: digital.royalty_kind.clone(),
            royalty_percent_bp: digital.royalty_percent_bp,
            royalty_fixed_minor: digital.royalty_fixed_minor,
            status: "accrued".to_string(),
            payout_id: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .digital_royalties
            .insert(royalty.id.clone(), royalty.clone());
        accrued.insert(digital.id.clone());
        royalties_accrued += 1;
        state.emit(
            "store.digital.royalty.accrued",
            serde_json::to_value(&royalty).unwrap_or(Value::Null),
        );
    }
    if files_granted > 0 || royalties_accrued > 0 {
        state.emit(
            "store.digital.downloads.granted",
            json!({
                "order_id": order.id,
                "user_id": order.user_id,
                "files_granted": files_granted,
                "royalties_accrued": royalties_accrued,
            }),
        );
    }
}

/// Reverse still-unpaid royalties of a refunded order. Only
/// `accrued` royalties reverse automatically: royalties already
/// attached to a payout request (`payout_requested` or `approved`)
/// or already paid keep their status and need a manual adjustment,
/// because money may already have been approved or moved. Called
/// from `transition_order` next to the license suspension, so every
/// refund path reverses the same way. Returns the reversed count.
pub(crate) fn reverse_digital_royalties_for_order(state: &StoreState, order_id: &str) -> usize {
    let mut reversed = 0usize;
    for mut entry in state.digital_royalties.iter_mut() {
        if entry.value().order_id == order_id && entry.value().status == "accrued" {
            entry.value().status = "reversed".to_string();
            entry.value().updated_at = now();
            reversed += 1;
        }
    }
    reversed
}

/// Sample-download rate limit: at most `limit` downloads per window
/// for one identity and one preview. Returns an error when the
/// window is exhausted. Old entries are pruned on each check so the
/// map does not grow without bound.
pub(crate) fn check_sample_rate_limit(
    state: &StoreState,
    identity: &str,
    preview_id: &str,
) -> UnifiedResult<()> {
    let (limit, window_minutes) = state
        .digital_defaults
        .read()
        .map(|defaults| {
            (
                defaults.sample_rate_limit_per_hour,
                defaults.sample_rate_limit_window_minutes,
            )
        })
        .unwrap_or((
            DEFAULT_SAMPLE_RATE_LIMIT_PER_HOUR,
            DEFAULT_SAMPLE_RATE_LIMIT_WINDOW_MINUTES,
        ));
    let key = format!("{identity}:{preview_id}");
    let cutoff = now() - chrono::Duration::minutes(i64::from(window_minutes));
    let mut timestamps = state.sample_downloads.entry(key).or_default();
    timestamps.retain(|timestamp| *timestamp > cutoff);
    if timestamps.len() as u32 >= limit {
        return Err(UnifiedError::new(
            format!(
                "Sample download rate limit reached: at most {limit} sample downloads per {window_minutes} minutes for this preview"
            ),
            429,
        ));
    }
    timestamps.push(now());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_type_slug_rules() {
        assert!(valid_type_slug("ebook"));
        assert!(valid_type_slug("my-type-1"));
        assert!(!valid_type_slug("Ebook"));
        assert!(!valid_type_slug(""));
        assert!(!valid_type_slug(&"a".repeat(33)));
        assert!(!valid_type_slug("has space"));
        assert!(!valid_type_slug("under_score"));
    }

    #[test]
    fn valid_format_rules() {
        assert!(valid_format("pdf"));
        assert!(valid_format("mp4"));
        assert!(!valid_format("PDF"));
        assert!(!valid_format(""));
        assert!(!valid_format("toolongformat1"));
    }

    #[test]
    fn valid_isbn_accepts_isbn10_and_isbn13() {
        assert!(valid_isbn("978-3-16-148410-0"));
        assert!(valid_isbn("0-306-40615-2"));
        assert!(valid_isbn("0-306-40615-X"));
        assert!(!valid_isbn("123"));
        assert!(!valid_isbn("978-3-16-148410-X"));
        assert!(!valid_isbn("ABCDEFGHIJ"));
    }

    #[test]
    fn valid_language_accepts_bcp47_short_forms() {
        assert!(valid_language("en"));
        assert!(valid_language("en-US"));
        assert!(valid_language("eng"));
        assert!(!valid_language("e"));
        assert!(!valid_language("EN"));
        assert!(!valid_language("en-us"));
        assert!(!valid_language("en-US-x"));
    }

    #[test]
    fn validate_royalty_bounds() {
        assert!(validate_royalty("percent", 5000, 0).is_ok());
        assert!(validate_royalty("percent", 10_001, 0).is_err());
        assert!(validate_royalty("percent", 5000, 100).is_err());
        assert!(validate_royalty("fixed", 0, 1000).is_ok());
        assert!(validate_royalty("fixed", 100, 1000).is_err());
        assert!(validate_royalty("fixed", 0, -1).is_err());
        assert!(validate_royalty("bogus", 1000, 0).is_err());
        assert!(validate_royalty("percent", -1, 0).is_err());
    }

    fn digital_product(product_id: &str) -> DigitalProduct {
        let timestamp = now();
        DigitalProduct {
            id: "dig-1".to_string(),
            product_id: product_id.to_string(),
            type_id: "ebook".to_string(),
            author: None,
            publisher: None,
            edition: None,
            isbn: None,
            language: None,
            pages: None,
            formats: vec!["pdf".to_string()],
            cover_storage_id: None,
            sample_allowed: false,
            download_limit: 3,
            royalty_kind: "percent".to_string(),
            royalty_percent_bp: 1000,
            royalty_fixed_minor: 0,
            royalty_payee: Some("author-1".to_string()),
            royalty_payee_company_id: None,
            status: "published".to_string(),
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    fn digital_file(id: &str, digital_product_id: &str) -> DigitalProductFile {
        let timestamp = now();
        DigitalProductFile {
            id: id.to_string(),
            digital_product_id: digital_product_id.to_string(),
            storage_file_id: format!("storage-{id}"),
            format: "pdf".to_string(),
            label: None,
            file_size_bytes: None,
            checksum: None,
            is_primary: false,
            sort_order: 0,
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    fn paid_order() -> Order {
        let timestamp = now();
        Order {
            id: "ord-1".to_string(),
            reference: "ORD-1".to_string(),
            user_id: "user-1".to_string(),
            items: vec![OrderItem {
                product_id: "prod-1".to_string(),
                name: "Ebook".to_string(),
                unit_price: 5000,
                quantity: 2,
                line_total: 10000,
                license_product: false,
                file_id: None,
                tier_id: None,
                tier_name: None,
                software_id: None,
                software_slug: None,
                company_id: None,
                pricing_model: "one_time".to_string(),
                billing_period: None,
                seats: 1,
                expires_at: None,
                download_limit: 5,
                course_id: None,
                exam_id: None,
                trial_days: 0,
                proration_policy: default_proration_policy(),
                grace_period_days: 0,
                dunning_max_retries: 0,
                dunning_retry_interval_days: 0,
                module_id: None,
            }],
            subtotal: 10000,
            discount_total: 0,
            total: 10000,
            currency: "USD".to_string(),
            coupon_code: None,
            discount_evaluation: None,
            company_id: None,
            trial_until: None,
            status: "paid".to_string(),
            provider: None,
            payment_reference: None,
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    #[test]
    fn grant_digital_purchases_grants_each_file_once_and_accrues_one_royalty() {
        let state = StoreState::new();
        state
            .digital_products
            .insert("dig-1".to_string(), digital_product("prod-1"));
        state
            .digital_files
            .insert("file-1".to_string(), digital_file("file-1", "dig-1"));
        state
            .digital_files
            .insert("file-2".to_string(), digital_file("file-2", "dig-1"));
        let order = paid_order();
        grant_digital_purchases_for_paid_order(&state, &order);
        assert_eq!(state.entitlements.len(), 2);
        assert!(state.entitlements.iter().all(|entry| {
            let entitlement = entry.value();
            entitlement.order_id == "ord-1"
                && entitlement.product_id == "prod-1"
                && entitlement.status == "active"
        }));
        assert_eq!(state.digital_royalties.len(), 1);
        let royalty = state
            .digital_royalties
            .iter()
            .next()
            .expect("one royalty")
            .value()
            .clone();
        assert_eq!(royalty.order_id, "ord-1");
        assert_eq!(royalty.digital_product_id, "dig-1");
        // 10% of the 10000 net line total, in minor units.
        assert_eq!(royalty.amount, 1000);
        // Granting the same paid order again is idempotent: no
        // duplicate entitlements and no second royalty.
        grant_digital_purchases_for_paid_order(&state, &order);
        assert_eq!(state.entitlements.len(), 2);
        assert_eq!(state.digital_royalties.len(), 1);
    }

    #[test]
    fn grant_digital_purchases_ignores_lines_without_a_digital_product() {
        let state = StoreState::new();
        let mut order = paid_order();
        order.items[0].product_id = "prod-unknown".to_string();
        grant_digital_purchases_for_paid_order(&state, &order);
        assert_eq!(state.entitlements.len(), 0);
        assert_eq!(state.digital_royalties.len(), 0);
    }
}
