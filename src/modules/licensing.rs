// Licensing module — license key management and validation.
//
// License keys use the format `XXXX-XXXX-XXXX-XXXX` over a 32-character
// alphabet (no ambiguous characters), with the final character acting as
// a checksum over the preceding characters. Validation is real: the key
// must exist, be `active`, not be expired, and have a free activation
// seat; a successful validation auto-registers the activation.
// Revocation emits the `license.revoked` hook event.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::db::models::{License, LicenseActivation, NewLicense, NewLicenseActivation};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Key generation: XXXX-XXXX-XXXX-XXXX with a checksum character.
// ---------------------------------------------------------------------------

/// Alphabet without ambiguous characters (no 0/O, 1/I/L).
const KEY_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Generate a new license key. The 16th character is a checksum: the
/// alphabet index of the sum of the indices of the first 15 characters
/// modulo the alphabet size.
pub fn generate_license_key() -> String {
    let mut rng = rand::rng();
    let mut indices: Vec<usize> = (0..15)
        .map(|_| rng.random_range(0..KEY_ALPHABET.len()))
        .collect();
    let checksum = indices.iter().sum::<usize>() % KEY_ALPHABET.len();
    indices.push(checksum);
    let chars: Vec<char> = indices
        .iter()
        .map(|index| KEY_ALPHABET[*index] as char)
        .collect();
    chars
        .chunks(4)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<String>>()
        .join("-")
}

/// True when the key has the right shape and a matching checksum.
pub fn key_checksum_valid(key: &str) -> bool {
    let compact: String = key.chars().filter(|c| *c != '-').collect();
    if compact.len() != 16 {
        return false;
    }
    let mut indices = Vec::with_capacity(16);
    for ch in compact.chars() {
        match KEY_ALPHABET.iter().position(|byte| *byte as char == ch) {
            Some(index) => indices.push(index),
            None => return false,
        }
    }
    indices[..15].iter().sum::<usize>() % KEY_ALPHABET.len() == indices[15]
}

fn normalize_key(key: &str) -> String {
    key.trim().to_uppercase()
}

// ---------------------------------------------------------------------------
// In-memory stores (used when no database pool is attached).
// ---------------------------------------------------------------------------

static LICENSES: OnceLock<DashMap<Uuid, License>> = OnceLock::new();
static ACTIVATIONS: OnceLock<DashMap<Uuid, LicenseActivation>> = OnceLock::new();

fn licenses_store() -> &'static DashMap<Uuid, License> {
    LICENSES.get_or_init(DashMap::new)
}

fn activations_store() -> &'static DashMap<Uuid, LicenseActivation> {
    ACTIVATIONS.get_or_init(DashMap::new)
}

fn db_pool(ctx: &AppContext) -> Option<crate::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

fn db_load_licenses(pool: &crate::db::DbPool) -> UnifiedResult<Vec<License>> {
    use crate::db::schema::licenses::dsl as lic_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    lic_dsl::licenses
        .select(License::as_select())
        .load(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load licenses: {error}")))
}

fn db_load_activations(
    pool: &crate::db::DbPool,
    license_id: Uuid,
) -> UnifiedResult<Vec<LicenseActivation>> {
    use crate::db::schema::license_activations::dsl as act_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    act_dsl::license_activations
        .filter(act_dsl::license_id.eq(license_id))
        .select(LicenseActivation::as_select())
        .load(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load license activations: {error}"))
        })
}

fn activations_for(license_id: Uuid) -> Vec<LicenseActivation> {
    activations_store()
        .iter()
        .filter(|entry| entry.value().license_id == license_id)
        .map(|entry| entry.value().clone())
        .collect()
}

fn find_license_by_key(key: &str) -> Option<License> {
    licenses_store()
        .iter()
        .find(|entry| entry.value().license_key == key)
        .map(|entry| entry.value().clone())
}

fn db_find_license_by_key(pool: &crate::db::DbPool, key: &str) -> UnifiedResult<Option<License>> {
    use crate::db::schema::licenses::dsl as lic_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    lic_dsl::licenses
        .filter(lic_dsl::license_key.eq(key))
        .select(License::as_select())
        .first(&mut conn)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load license: {error}")))
}

// ---------------------------------------------------------------------------
// Hook context: `init` stashes the shared application context (config,
// hook bus, module registry, database pool) so the synchronous hook
// handlers below can use the database and emit events. Hook handlers
// read it lazily, so registration order (hooks before `init`) does not
// matter. The stashed context shares the live pool/config locks; only
// the uptime and pause flags are snapshots.
// ---------------------------------------------------------------------------

static LICENSING_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        cache: ctx.cache.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        start_time: std::time::Instant::now(),
        paused: std::sync::atomic::AtomicBool::new(ctx.is_paused()),
        maintenance: std::sync::atomic::AtomicBool::new(ctx.is_maintenance()),
    })
}

fn hook_ctx() -> Option<Arc<AppContext>> {
    LICENSING_CTX.get().cloned()
}

/// Create and store one license with an already-chosen key, and emit
/// `license.issued`. Shared by the HTTP create handler and the
/// `order.created` hook handler.
fn issue_license(
    ctx: &AppContext,
    key: String,
    product: String,
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
    max_activations: i32,
    expires_at: Option<DateTime<Utc>>,
) -> UnifiedResult<License> {
    let duplicate = match db_pool(ctx) {
        Some(pool) => db_find_license_by_key(&pool, &key)?.is_some(),
        None => find_license_by_key(&key).is_some(),
    };
    if duplicate {
        return Err(UnifiedError::conflict(
            "A license with this key already exists",
        ));
    }
    let now = Utc::now();
    let license = License {
        id: Uuid::now_v7(),
        license_key: key,
        product,
        user_id,
        company_id,
        max_activations,
        status: "active".to_string(),
        expires_at,
        created_at: now,
        updated_at: now,
    };
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::licenses::dsl as lic_dsl;
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        diesel::insert_into(lic_dsl::licenses)
            .values(&NewLicense {
                id: license.id,
                license_key: license.license_key.clone(),
                product: license.product.clone(),
                user_id: license.user_id,
                company_id: license.company_id,
                max_activations: license.max_activations,
                status: license.status.clone(),
                expires_at: license.expires_at,
            })
            .execute(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot create license: {error}")))?;
    }
    licenses_store().insert(license.id, license.clone());
    ctx.hooks.emit(
        events::LICENSE_ISSUED,
        serde_json::json!({
            "license_id": license.id,
            "license_key": license.license_key,
            "product": license.product,
        }),
    );
    Ok(license)
}

/// True when a license already exists for this product and owner
/// (in-memory store, plus the database when a pool is attached).
fn license_exists_for_product(
    ctx: &AppContext,
    product: &str,
    user_id: Option<Uuid>,
) -> bool {
    if licenses_store()
        .iter()
        .any(|entry| entry.value().product == product && entry.value().user_id == user_id)
    {
        return true;
    }
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::licenses::dsl as lic_dsl;
        if let Ok(mut conn) = pool.get() {
            let mut query = lic_dsl::licenses
                .filter(lic_dsl::product.eq(product))
                .into_boxed();
            query = match user_id {
                Some(id) => query.filter(lic_dsl::user_id.eq(id)),
                None => query.filter(lic_dsl::user_id.is_null()),
            };
            if let Ok(count) = query
                .count()
                .get_result::<i64>(&mut conn)
            {
                return count > 0;
            }
        }
    }
    false
}

/// Answer `license.validate_requested` (from the Software module) with
/// the same validation the HTTP `/validate` endpoint performs: the key
/// must exist, pass its checksum, be `active`, not be expired, have a
/// free activation seat, and (when the payload names a product) belong
/// to that product.
fn on_license_validate_requested(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "valid": false,
            "message": "licensing module is not initialised",
        }));
        return;
    };
    let key = ctx
        .get("license_key")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if key.is_empty() {
        ctx.add_result(serde_json::json!({
            "valid": false,
            "message": "license_key is required",
        }));
        return;
    }
    let instance_id = ctx
        .get("instance_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let hostname = ctx
        .get("hostname")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let outcome = match validate_and_maybe_activate(
        &app_ctx,
        &key,
        instance_id.as_deref(),
        hostname.as_deref(),
        true,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            ctx.add_result(serde_json::json!({
                "valid": false,
                "message": error.message,
            }));
            return;
        }
    };
    let requested_product = ctx
        .get("product")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let product_matches = match (&requested_product, &outcome.license) {
        (Some(requested), Some(license)) => license.product == *requested,
        _ => true,
    };
    let mut result = serde_json::json!({
        "valid": outcome.valid && product_matches,
    });
    if let Some(license) = &outcome.license {
        result["license_id"] = serde_json::json!(license.id);
    }
    let message = if !outcome.valid {
        outcome.reason.clone()
    } else if !product_matches {
        Some("product_mismatch".to_string())
    } else {
        None
    };
    if let Some(message) = message {
        result["message"] = serde_json::json!(message);
    }
    ctx.add_result(result);
}

/// Auto-issue licenses for a new order (from the Store module). Only
/// items flagged `license_product` get a license, one per item, keyed by
/// the item name as the product; already-licensed product/owner pairs
/// are skipped so a repeated event does not issue duplicates.
fn on_order_created(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "issued": false,
            "error": "licensing module is not initialised",
        }));
        return;
    };
    let order_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<Uuid>().ok());
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<Uuid>().ok());
    let items = ctx
        .get("items")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    for item in items {
        if item
            .get("license_product")
            .and_then(|value| value.as_bool())
            != Some(true)
        {
            continue;
        }
        let product = item
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if product.is_empty() {
            continue;
        }
        if license_exists_for_product(&app_ctx, &product, user_id) {
            continue;
        }
        match issue_license(
            &app_ctx,
            generate_license_key(),
            product.clone(),
            user_id,
            company_id,
            1,
            None,
        ) {
            Ok(license) => {
                ctx.add_result(serde_json::json!({
                    "issued": true,
                    "order_id": order_id,
                    "license_id": license.id,
                    "license_key": license.license_key,
                    "product": product,
                }));
            }
            Err(error) => {
                ctx.add_result(serde_json::json!({
                    "issued": false,
                    "order_id": order_id,
                    "product": product,
                    "error": error.message,
                }));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Request / response types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateLicenseRequest {
    product: String,
    #[serde(default)]
    license_key: Option<String>,
    #[serde(default)]
    user_id: Option<Uuid>,
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default = "default_max_activations")]
    max_activations: i32,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

fn default_max_activations() -> i32 {
    1
}

#[derive(Debug, Deserialize)]
struct UpdateLicenseRequest {
    #[serde(default)]
    product: Option<String>,
    #[serde(default)]
    max_activations: Option<i32>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    expires_at: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Deserialize)]
struct ValidateRequest {
    key: String,
    #[serde(default)]
    instance_id: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ActivationRequest {
    key: String,
    instance_id: String,
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ValidationResult {
    valid: bool,
    reason: Option<String>,
    license: Option<License>,
    activations_used: usize,
    max_activations: i32,
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

fn validate_status(status: &str) -> UnifiedResult<()> {
    match status {
        "active" | "suspended" | "revoked" | "expired" => Ok(()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown license status \"{other}\" (expected active, suspended, revoked, or expired)"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Shared validation / activation logic.
// ---------------------------------------------------------------------------

/// Validate a license key and, when an `instance_id` is supplied and the
/// license is valid, auto-register the activation (idempotent per
/// instance). Returns the validation outcome; a missing or invalid license
/// is a data result (`valid: false`), not an HTTP error.
fn validate_and_maybe_activate(
    ctx: &AppContext,
    key: &str,
    instance_id: Option<&str>,
    hostname: Option<&str>,
    auto_register: bool,
) -> UnifiedResult<ValidationResult> {
    let key = normalize_key(key);
    let license = match db_pool(ctx) {
        Some(pool) => db_find_license_by_key(&pool, &key)?,
        None => find_license_by_key(&key),
    };
    let Some(mut license) = license else {
        return Ok(ValidationResult {
            valid: false,
            reason: Some("not_found".to_string()),
            license: None,
            activations_used: 0,
            max_activations: 0,
        });
    };
    // Keep an expired license honest even if its status was never updated.
    if license.status == "active" {
        if let Some(expires_at) = license.expires_at {
            if expires_at <= Utc::now() {
                license.status = "expired".to_string();
                persist_license_status(ctx, &license)?;
            }
        }
    }
    let mut activations = match db_pool(ctx) {
        Some(pool) => db_load_activations(&pool, license.id)?,
        None => activations_for(license.id),
    };
    let invalid = |reason: &str| ValidationResult {
        valid: false,
        reason: Some(reason.to_string()),
        license: Some(license.clone()),
        activations_used: activations.len(),
        max_activations: license.max_activations,
    };
    if !key_checksum_valid(&key) {
        return Ok(invalid("invalid_checksum"));
    }
    match license.status.as_str() {
        "suspended" => return Ok(invalid("suspended")),
        "revoked" => return Ok(invalid("revoked")),
        "expired" => return Ok(invalid("expired")),
        "active" => {}
        _ => return Ok(invalid("invalid_status")),
    }
    if let Some(expires_at) = license.expires_at {
        if expires_at <= Utc::now() {
            return Ok(invalid("expired"));
        }
    }
    if let Some(instance_id) = instance_id {
        if activations
            .iter()
            .any(|activation| activation.instance_id == instance_id)
        {
            return Ok(ValidationResult {
                valid: true,
                reason: None,
                license: Some(license),
                activations_used: activations.len(),
                max_activations: license.max_activations,
            });
        }
    }
    if activations.len() as i32 >= license.max_activations {
        return Ok(invalid("seats_exceeded"));
    }
    if auto_register {
        if let Some(instance_id) = instance_id {
            let activation = LicenseActivation {
                id: Uuid::now_v7(),
                license_id: license.id,
                instance_id: instance_id.to_string(),
                hostname: hostname.map(str::to_string),
                created_at: Utc::now(),
            };
            if let Some(pool) = db_pool(ctx) {
                use crate::db::schema::license_activations::dsl as act_dsl;
                let mut conn = pool.get().map_err(|error| {
                    UnifiedError::internal(format!("Cannot get database connection: {error}"))
                })?;
                diesel::insert_into(act_dsl::license_activations)
                    .values(&NewLicenseActivation {
                        id: activation.id,
                        license_id: activation.license_id,
                        instance_id: activation.instance_id.clone(),
                        hostname: activation.hostname.clone(),
                    })
                    .execute(&mut conn)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot record activation: {error}"))
                    })?;
            }
            activations_store().insert(activation.id, activation.clone());
            activations.push(activation);
            ctx.hooks.emit(
                "license.activated",
                serde_json::json!({
                    "license_id": license.id,
                    "license_key": license.license_key,
                    "instance_id": instance_id,
                }),
            );
        }
    }
    Ok(ValidationResult {
        valid: true,
        reason: None,
        license: Some(license.clone()),
        activations_used: activations.len(),
        max_activations: license.max_activations,
    })
}

fn persist_license_status(ctx: &AppContext, license: &License) -> UnifiedResult<()> {
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::licenses::dsl as lic_dsl;
        #[derive(AsChangeset)]
        #[diesel(table_name = crate::db::schema::licenses)]
        struct StatusChange {
            status: String,
        }
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        diesel::update(lic_dsl::licenses.filter(lic_dsl::id.eq(license.id)))
            .set(StatusChange {
                status: license.status.clone(),
            })
            .execute(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot update license status: {error}"))
            })?;
    }
    if let Some(mut entry) = licenses_store().get_mut(&license.id) {
        entry.status = license.status.clone();
        entry.updated_at = Utc::now();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn list_licenses(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<License> = match db_pool(&ctx) {
            Some(pool) => db_load_licenses(&pool)?,
            None => licenses_store().iter().map(|entry| entry.value().clone()).collect(),
        };
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.license_key.to_lowercase().contains(&needle)
                    || row.product.to_lowercase().contains(&needle)
            });
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(product) = params.filters.get("product") {
            items.retain(|row| row.product == *product);
        }
        let sort = params.allowed_sort(&["product", "status", "created_at"], "created_at");
        match sort.as_str() {
            "product" => items.sort_by(|a, b| a.product.cmp(&b.product)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_license(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateLicenseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        if body.product.trim().is_empty() {
            return Err(UnifiedError::bad_request("product is required"));
        }
        if body.max_activations < 1 {
            return Err(UnifiedError::bad_request(
                "max_activations must be at least 1",
            ));
        }
        let key = match &body.license_key {
            Some(custom) => {
                let key = normalize_key(custom);
                if key.len() != 19 || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    return Err(UnifiedError::bad_request(
                        "license_key must use the format XXXX-XXXX-XXXX-XXXX",
                    ));
                }
                key
            }
            None => generate_license_key(),
        };
        issue_license(
            &ctx,
            key,
            body.product.trim().to_string(),
            body.user_id,
            body.company_id,
            body.max_activations,
            body.expires_at,
        )
    })();
    match result {
        Ok(license) => json::success_response(license, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_license(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::licenses::dsl as lic_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            if let Some(row) = lic_dsl::licenses
                .filter(lic_dsl::id.eq(id))
                .select(License::as_select())
                .first(&mut conn)
                .optional()
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot load license: {error}"))
                })?
            {
                return Ok(row);
            }
        }
        licenses_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License not found"))
    })();
    json::respond(result, debug)
}

async fn update_license(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateLicenseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        let id = id.into_inner();
        let mut license = licenses_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        if let Some(status) = &body.status {
            validate_status(status)?;
            license.status = status.clone();
        }
        if let Some(product) = &body.product {
            if product.trim().is_empty() {
                return Err(UnifiedError::bad_request("product must not be empty"));
            }
            license.product = product.trim().to_string();
        }
        if let Some(max) = body.max_activations {
            if max < 1 {
                return Err(UnifiedError::bad_request(
                    "max_activations must be at least 1",
                ));
            }
            license.max_activations = max;
        }
        if let Some(expires_at) = body.expires_at {
            license.expires_at = expires_at;
        }
        license.updated_at = Utc::now();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::licenses::dsl as lic_dsl;
            #[derive(AsChangeset)]
            #[diesel(table_name = crate::db::schema::licenses)]
            struct LicenseChanges {
                product: Option<String>,
                max_activations: Option<i32>,
                status: Option<String>,
                expires_at: Option<Option<DateTime<Utc>>>,
            }
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::update(lic_dsl::licenses.filter(lic_dsl::id.eq(id)))
                .set(LicenseChanges {
                    product: body.product.clone(),
                    max_activations: body.max_activations,
                    status: body.status.clone(),
                    expires_at: body.expires_at,
                })
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot update license: {error}"))
                })?;
        }
        licenses_store().insert(id, license.clone());
        Ok(license)
    })();
    json::respond(result, debug)
}

async fn delete_license(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::license_activations::dsl as act_dsl;
            use crate::db::schema::licenses::dsl as lic_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::delete(act_dsl::license_activations.filter(act_dsl::license_id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete license activations: {error}"))
                })?;
            diesel::delete(lic_dsl::licenses.filter(lic_dsl::id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete license: {error}"))
                })?;
        }
        activations_store().retain(|_, activation| activation.license_id != id);
        let removed = licenses_store().remove(&id).is_some();
        if removed {
            Ok(serde_json::json!({ "deleted": true, "id": id }))
        } else {
            Err(UnifiedError::not_found("License not found"))
        }
    })();
    json::respond(result, debug)
}

async fn validate_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ValidateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = validate_and_maybe_activate(
        &ctx,
        &body.key,
        body.instance_id.as_deref(),
        body.hostname.as_deref(),
        true,
    );
    json::respond(result, debug)
}

async fn activate_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ActivationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let outcome = validate_and_maybe_activate(
            &ctx,
            &body.key,
            Some(&body.instance_id),
            body.hostname.as_deref(),
            true,
        )?;
        if !outcome.valid {
            return Err(UnifiedError::unprocessable(format!(
                "License activation failed: {}",
                outcome.reason.unwrap_or_else(|| "invalid".to_string())
            ))
            .with_metadata("reason", Value::String(outcome.reason.unwrap_or_default())));
        }
        let license = outcome.license.clone().expect("validated license");
        let activations = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, license.id)?,
            None => activations_for(license.id),
        };
        let activation = activations
            .into_iter()
            .find(|activation| activation.instance_id == body.instance_id);
        Ok(serde_json::json!({
            "license": license,
            "activation": activation,
            "activations_used": outcome.activations_used,
            "max_activations": outcome.max_activations,
        }))
    })();
    json::respond(result, debug)
}

async fn deactivate_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ActivationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let key = normalize_key(&body.key);
        let license = match db_pool(&ctx) {
            Some(pool) => db_find_license_by_key(&pool, &key)?,
            None => find_license_by_key(&key),
        }
        .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        let mut removed = false;
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::license_activations::dsl as act_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            let count = diesel::delete(
                act_dsl::license_activations
                    .filter(act_dsl::license_id.eq(license.id))
                    .filter(act_dsl::instance_id.eq(&body.instance_id)),
            )
            .execute(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot deactivate license: {error}"))
            })?;
            removed = count > 0;
        }
        activations_store().retain(|_, activation| {
            !(activation.license_id == license.id && activation.instance_id == body.instance_id)
                || {
                    removed = true;
                    false
                }
        });
        // The retain closure above both removes and records the outcome;
        // re-check the in-memory store when no pool was present.
        if db_pool(&ctx).is_none() {
            removed = !activations_store()
                .iter()
                .any(|entry| {
                    entry.value().license_id == license.id
                        && entry.value().instance_id == body.instance_id
                })
                && removed;
        }
        if !removed {
            return Err(UnifiedError::not_found(
                "No activation exists for this instance",
            ));
        }
        ctx.hooks.emit(
            "license.deactivated",
            serde_json::json!({
                "license_id": license.id,
                "instance_id": body.instance_id,
            }),
        );
        let activations_used = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, license.id)?.len(),
            None => activations_for(license.id).len(),
        };
        Ok(serde_json::json!({
            "license": license,
            "instance_id": body.instance_id,
            "activations_used": activations_used,
            "max_activations": license.max_activations,
        }))
    })();
    json::respond(result, debug)
}

async fn suspend_handler(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        let id = id.into_inner();
        let mut license = licenses_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        license.status = "suspended".to_string();
        license.updated_at = Utc::now();
        persist_license_status(&ctx, &license)?;
        ctx.hooks.emit(
            "license.suspended",
            serde_json::json!({ "license_id": license.id, "license_key": license.license_key }),
        );
        Ok(license)
    })();
    json::respond(result, debug)
}

async fn revoke_handler(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        let id = id.into_inner();
        let mut license = licenses_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        license.status = "revoked".to_string();
        license.updated_at = Utc::now();
        persist_license_status(&ctx, &license)?;
        ctx.hooks.emit(
            events::LICENSE_REVOKED,
            serde_json::json!({ "license_id": license.id, "license_key": license.license_key }),
        );
        Ok(license)
    })();
    json::respond(result, debug)
}

async fn list_activations(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let exists = match db_pool(&ctx) {
            Some(pool) => {
                use crate::db::schema::licenses::dsl as lic_dsl;
                let mut conn = pool.get().map_err(|error| {
                    UnifiedError::internal(format!("Cannot get database connection: {error}"))
                })?;
                lic_dsl::licenses
                    .filter(lic_dsl::id.eq(id))
                    .select(License::as_select())
                    .first(&mut conn)
                    .optional()
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot load license: {error}"))
                    })?
                    .is_some()
            }
            None => licenses_store().contains_key(&id),
        };
        if !exists {
            return Err(UnifiedError::not_found("License not found"));
        }
        let mut items = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, id)?,
            None => activations_for(id),
        };
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.instance_id.to_lowercase().contains(&needle)
                    || row
                        .hostname
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&needle)
            });
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct LicensingModule;

impl Module for LicensingModule {
    fn id(&self) -> &'static str {
        "licensing"
    }

    fn name(&self) -> &'static str {
        "Licensing"
    }

    fn description(&self) -> &'static str {
        "License key management: generation, validation, activation seats, suspension, and revocation."
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = LICENSING_CTX.set(stash_ctx(ctx));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register(
            "license.validate_requested",
            "licensing",
            Arc::new(on_license_validate_requested),
        );
        bus.register("order.created", "licensing", Arc::new(on_order_created));
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/licensing")
                .route("/licenses", web::get().to(list_licenses))
                .route("/licenses", web::post().to(create_license))
                .route("/validate", web::post().to(validate_handler))
                .route("/activate", web::post().to(activate_handler))
                .route("/deactivate", web::post().to(deactivate_handler))
                .route("/licenses/{id}", web::get().to(get_license))
                .route("/licenses/{id}", web::put().to(update_license))
                .route("/licenses/{id}", web::delete().to(delete_license))
                .route("/licenses/{id}/suspend", web::post().to(suspend_handler))
                .route("/licenses/{id}/revoke", web::post().to(revoke_handler))
                .route(
                    "/licenses/{id}/activations",
                    web::get().to(list_activations),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LicensingModule));
}
