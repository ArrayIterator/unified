//! Licensing - License validation, activation, and module entitlement checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{
    ModuleRegistry, MODULE_ENTITLEMENT_DENIED_EVENT, MODULE_ENTITLEMENT_GRANTED_EVENT,
    RouteInfo,
};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{License, LicenseActivation, NewLicense, NewLicenseActivation};
use crate::modules::apikeys::{
    authenticate_key, key_has_scope, note_used, AuthenticatedKey,
};
use crate::modules::entitlements::{entitled_via_live_sources, entitlements_state};
use actix_multipart::Multipart;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use futures_util::TryStreamExt;
use hmac::{Hmac, Mac};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;
use super::{keys::*, expiry::*, store::*, hooks::*, types::*, handlers::*, products::*, helpers::*, product_handlers::*, verify::*, offline::*, distribution::*, releases::*, module::*};

// Shared validation / activation logic.

/// Validate a license key and, when an `instance_id` is supplied and the
/// license is valid, auto-register the activation (idempotent per
/// instance). Returns the validation outcome; a missing or invalid license
/// is a data result (`valid: false`), not an HTTP error.
pub(crate) fn validate_and_maybe_activate(
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
            grace: false,
            days_in_grace: 0,
        });
    };
    // Grace state is computed once, before any status change: a
    // license inside its product grace period stays `active` below
    // instead of being marked `expired`, and validates with
    // `grace: true`.
    let (grace, days_in_grace) = grace_state(&license);
    // Keep an expired license honest even if its status was never updated.
    // A license inside its product grace period is NOT marked expired
    // here: it keeps validating (with `grace: true`) until grace ends.
    if license.status == "active" {
        if let Some(expires_at) = license.expires_at {
            if expires_at <= Utc::now() && !grace {
                license.status = "expired".to_string();
                persist_license_status(ctx, &license)?;
            }
        }
    }
    // Serialize the seat check and the insert below per license:
    // without this, two concurrent validations can both pass the
    // seat check and both insert, overshooting `max_activations`
    // (check-then-insert TOCTOU). The guard is released before the
    // history/event/hook calls at the end, so a listener can never
    // deadlock against a validation for the same license. This only
    // serialises threads inside this process; a database unique
    // constraint plus a transactional seat check is the durable
    // cross-process fix (see `activation_lock_for`).
    let seat_lock = activation_lock_for(license.id);
    let _seat_guard = seat_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        grace: false,
        days_in_grace: 0,
    };
    // A product key carries its product prefix before the checksum
    // body (for example `UNF-XXXX-XXXX-XXXX-XXXX`); strip the known
    // prefix before the checksum check so product keys validate.
    if !license_key_checksum_valid(&key, &license.product) {
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
        if expires_at <= Utc::now() && !grace {
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
                license: Some(license.clone()),
                activations_used: activations.len(),
                max_activations: license.max_activations,
                grace,
                days_in_grace,
            });
        }
    }
    if activations.len() as i32 >= license.max_activations {
        return Ok(invalid("seats_exceeded"));
    }
    // The seat is consumed under the guard above; the guard is
    // released before the history/event/hook calls, so a listener
    // can never deadlock against a validation for this license.
    let mut activated: Option<LicenseActivation> = None;
    if auto_register {
        if let Some(instance_id) = instance_id {
            let activation = LicenseActivation {
                id: Uuid::now_v7(),
                license_id: license.id,
                instance_id: instance_id.to_string(),
                hostname: hostname.map(str::to_string),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            if let Some(pool) = db_pool(ctx) {
                use unified_core::db::schema::license_activations::dsl as act_dsl;
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
            activations.push(activation.clone());
            activated = Some(activation);
        }
    }
    drop(_seat_guard);
    if let Some(activation) = &activated {
        // Activation history + hash-chained event log (SaaS): every
        // activation is recorded with actor, instance, hostname,
        // and timestamp.
        record_activation_history(
            license.id,
            "activated",
            None,
            &activation.instance_id,
            hostname,
        );
        append_license_event(
            ctx,
            license.id,
            "activated",
            None,
            serde_json::json!({
                "instance_id": activation.instance_id,
                "hostname": hostname,
            }),
        );
        ctx.hooks.emit(
            "license.activated",
            serde_json::json!({
                "license_id": license.id,
                "license_key": license.license_key,
                "instance_id": activation.instance_id,
            }),
        );
    }
    Ok(ValidationResult {
        valid: true,
        reason: None,
        license: Some(license.clone()),
        activations_used: activations.len(),
        max_activations: license.max_activations,
        grace,
        days_in_grace,
    })
}

pub(crate) fn persist_license_status(ctx: &AppContext, license: &License) -> UnifiedResult<()> {
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::licenses::dsl as lic_dsl;
        #[derive(AsChangeset)]
        #[diesel(table_name = unified_core::db::schema::licenses)]
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

// Module entitlements: a sellable module as a product.

/// True when `license` entitles `company_id` to use `module_id`.
///
/// A module entitlement is a license whose `product` names the
/// module: either the module id itself (for example
/// `product: "meetings"`) or the namespaced form `module:<id>` (for
/// example `product: "module:meetings"`). Licenses carry no separate
/// entitlements list -- the `product` field is the entitlement
/// carrier (the same field `issue_license` fills from the purchased
/// item name on `order.created`, and the store's
/// `store.entitlement.check` fallback resolves through the product
/// name). A license counts only when it belongs to the company, is
/// `active`, and has not expired. The expiry rule matches
/// validation: a license inside its product grace period still
/// validates (with `grace: true`), so its entitlement holds until
/// grace ends too. This is a read-only check: it never changes a
/// license status (unlike validation, which marks an expired
/// license `expired`).
pub(crate) fn module_license_matches(license: &License, company_id: Uuid, module_id: &str) -> bool {
    if license.company_id != Some(company_id) || license.status != "active" {
        return false;
    }
    if license
        .expires_at
        .is_some_and(|expires_at| expires_at <= Utc::now())
        && !grace_state(license).0
    {
        return false;
    }
    license.product == module_id
        || license.product.strip_prefix("module:") == Some(module_id)
}

/// True when `company_id` holds an active license entitling it to
/// use `module_id` as a product (see `module_license_matches`).
/// The database rows are authoritative when a pool is attached;
/// otherwise the in-memory store is scanned, following the same
/// pattern as `check_license_expiry`. Fails closed: an unparseable
/// company id, an empty module id, or a database error means no
/// entitlement.
pub fn has_module_entitlement(ctx: &AppContext, company_id: &str, module_id: &str) -> bool {
    if module_id.trim().is_empty() {
        return false;
    }
    let Ok(company_id) = company_id.parse::<Uuid>() else {
        return false;
    };
    match db_pool(ctx) {
        Some(pool) => match db_load_licenses(&pool) {
            Ok(licenses) => licenses
                .iter()
                .any(|license| module_license_matches(license, company_id, module_id)),
            Err(_) => false,
        },
        None => licenses_store()
            .iter()
            .any(|entry| module_license_matches(entry.value(), company_id, module_id)),
    }
}

/// Event the store module answers with the subscription
/// entitlement for one company + module (see
/// `store::on_subscription_module_entitlement_check`): payload
/// `{"company_id", "module_id"}`, one result
/// `{"entitled": bool, "source": "subscription", ...}`. The store
/// answers `entitled: false` when no granting subscription exists,
/// and the bus returns no result at all when the store module is
/// not registered; both mean "no subscription entitlement".
pub(crate) const SUBSCRIPTION_ENTITLEMENT_CHECK_EVENT: &str =
    "subscription.module_entitlement.check";

/// True when the company holds an active subscription covering the
/// module (trialing/active inside the period, past-due inside
/// grace, or cancelled at period end inside the period - the exact
/// rule lives in the store answerer). Fail closed: no answer, or
/// no entitled result, means no subscription entitlement. The
/// hook emit is synchronous, so this is safe to call from the
/// licensing gate; the answerer only reads the store's
/// subscription map and never calls back into licensing.
pub fn has_subscription_entitlement(
    ctx: &AppContext,
    company_id: &str,
    module_id: &str,
) -> bool {
    let context = ctx.hooks.emit(
        SUBSCRIPTION_ENTITLEMENT_CHECK_EVENT,
        serde_json::json!({
            "company_id": company_id,
            "module_id": module_id,
        }),
    );
    context.results.iter().any(|result| {
        result
            .get("entitled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    })
}

/// The id of the active subscription covering the module, if
/// any. Same hook as `has_subscription_entitlement`, but returns
/// the granting subscription's id so the tenants module can
/// record it on the grant (`granted_via_id` / the grant event's
/// `source_ref`): the entitlements mirror stores it as the
/// stored row's `source_ref`, which is what a later
/// subscription-cancel lapses the row through. Fail closed: no
/// answer, no entitled result, or an entitled result without an
/// id means no subscription id. (Kept separate from
/// `has_subscription_entitlement` on purpose: the bool helper
/// must stay true for entitled answers that carry no id.)
pub fn subscription_entitlement_id(
    ctx: &AppContext,
    company_id: &str,
    module_id: &str,
) -> Option<String> {
    let context = ctx.hooks.emit(
        SUBSCRIPTION_ENTITLEMENT_CHECK_EVENT,
        serde_json::json!({
            "company_id": company_id,
            "module_id": module_id,
        }),
    );
    context.results.iter().find_map(|result| {
        if result
            .get("entitled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            result
                .get("subscription_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        } else {
            None
        }
    })
}

/// Extensions-Manager-facing gate for enabling a sellable module.
///
/// Exact rule, in order:
/// 1. Unknown module id: 404, no event.
/// 2. Module is not sellable (`available_as_product() == false`):
///    no entitlement is needed; succeeds silently with no event.
/// 3. Sellable core module (`is_core() == true`): covered by an
///    implicit owner entitlement -- core modules ship with the
///    platform, so the company running this installation already
///    holds them. Emits `module.entitlement.granted` with
///    `reason: "owner_entitlement"`.
/// 4. Sellable non-core module: requires an active license for the
///    company (see `has_module_entitlement`). On success emits
///    `module.entitlement.granted` with `reason: "license"`; on
///    failure continues to step 5 instead of denying outright.
/// 5. Sellable non-core module without a license: requires an
///    active subscription covering the module (see
///    `has_subscription_entitlement`, answered by the store
///    module). On success emits `module.entitlement.granted` with
///    `reason: "subscription"`.
/// 6. Sellable non-core module without a license or a
///    subscription: requires an active stored entitlement row
///    for the company (SaaS package, manual grant, or a mirrored
///    license/subscription row). This is the leg the gate never
///    consulted before - a module covered only by a stored row
///    was entitled in `effective()` but 403 here. On success
///    emits `module.entitlement.granted` with `reason:
///    "stored_row"` and the row's `source_kind`.
/// 7. Otherwise emits `module.entitlement.denied` with
///    `reason: "no_active_entitlement"` and returns 403.
///
/// Steps 2-6 are the one shared `entitled_via_live_sources`
/// decision (same function `effective()` runs), so the gate and
/// the entitlements read can never disagree about whether a
/// module is entitled. Grace is deliberately not a leg here: a
/// grace-state module is read-only, never entitled (see
/// `EntitlementsState::grace_applies`), so it is denied at step
/// 7 exactly as `enable_module` refuses it.
///
/// The Extensions Manager must call this before enabling a module,
/// passing the company the enable is for: its enable path carries
/// no company id today, so the gate cannot be enforced inside
/// `enable_with_dependencies` until the caller resolves the
/// acting company first.
pub fn check_module_entitlement(
    ctx: &AppContext,
    company_id: &str,
    module_id: &str,
) -> UnifiedResult<()> {
    if ctx.modules.get(module_id).is_none() {
        return Err(UnifiedError::not_found(format!(
            "Module \"{module_id}\" not found"
        )));
    }
    // The shared verdict covers steps 2-6 (builtin, owner,
    // license, subscription, stored row) in gate order; the
    // stored-row leg reads the entitlements state stashed at the
    // entitlements module's `init`, and is skipped - fail closed,
    // the pre-fix behaviour - when that module has not
    // initialized yet.
    let verdict = entitled_via_live_sources(
        entitlements_state().as_deref(),
        Some(ctx),
        company_id,
        module_id,
    );
    if verdict.entitled {
        let mut payload = serde_json::json!({
            "id": module_id,
            "company_id": company_id,
            "reason": verdict.reason,
        });
        if verdict.reason == "stored_row" {
            if let Some(row) = verdict.stored.first() {
                payload
                    .as_object_mut()
                    .expect("granted payload is an object")
                    .insert(
                        "source_kind".to_string(),
                        serde_json::json!(row.source_kind),
                    );
            }
        }
        // A non-sellable module succeeds silently with no event
        // (step 2); every other grant is announced.
        if verdict.reason != "builtin" {
            ctx.hooks.emit(MODULE_ENTITLEMENT_GRANTED_EVENT, payload);
        }
        return Ok(());
    }
    ctx.hooks.emit(
        MODULE_ENTITLEMENT_DENIED_EVENT,
        serde_json::json!({
            "id": module_id,
            "company_id": company_id,
            "reason": "no_active_entitlement",
        }),
    );
    Err(UnifiedError::forbidden(format!(
        "Module \"{module_id}\" requires an active license entitlement for this company"
    ))
    .with_metadata("module_id", serde_json::json!(module_id))
    .with_metadata("company_id", serde_json::json!(company_id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::modules::Module;

    fn test_license(
        company_id: Option<uuid::Uuid>,
        status: &str,
        product: &str,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> License {
        License {
            id: uuid::Uuid::new_v4(),
            license_key: "TEST-KEY".to_string(),
            product: product.to_string(),
            user_id: None,
            company_id,
            max_activations: 1,
            status: status.to_string(),
            expires_at,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn module_license_matches_active_license() {
        let company = uuid::Uuid::new_v4();
        let future = chrono::Utc::now() + chrono::Duration::days(30);
        let license = test_license(Some(company), "active", "planner", Some(future));
        assert!(module_license_matches(&license, company, "planner"));
    }

    #[test]
    fn module_license_matches_module_prefix() {
        let company = uuid::Uuid::new_v4();
        let future = chrono::Utc::now() + chrono::Duration::days(30);
        let license = test_license(Some(company), "active", "module:planner", Some(future));
        assert!(module_license_matches(&license, company, "planner"));
        assert!(!module_license_matches(&license, company, "crm"));
    }

    #[test]
    fn module_license_matches_no_expiry_means_no_expiry_check() {
        let company = uuid::Uuid::new_v4();
        let license = test_license(Some(company), "active", "planner", None);
        assert!(module_license_matches(&license, company, "planner"));
    }

    #[test]
    fn module_license_matches_rejects_bad_licenses() {
        let company = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let future = chrono::Utc::now() + chrono::Duration::days(30);
        let past = chrono::Utc::now() - chrono::Duration::days(1);
        // Wrong company.
        let license = test_license(Some(other), "active", "planner", Some(future));
        assert!(!module_license_matches(&license, company, "planner"));
        // No company on the license.
        let license = test_license(None, "active", "planner", Some(future));
        assert!(!module_license_matches(&license, company, "planner"));
        // Not active.
        let license = test_license(Some(company), "revoked", "planner", Some(future));
        assert!(!module_license_matches(&license, company, "planner"));
        // Expired.
        let license = test_license(Some(company), "active", "planner", Some(past));
        assert!(!module_license_matches(&license, company, "planner"));
        // Different product.
        let license = test_license(Some(company), "active", "crm", Some(future));
        assert!(!module_license_matches(&license, company, "planner"));
    }

    fn insert_grace_product(slug: &str, grace_days: i64) {
        let now = chrono::Utc::now();
        license_products_store().insert(
            uuid::Uuid::new_v4(),
            LicenseProduct {
                id: uuid::Uuid::new_v4(),
                product: slug.to_string(),
                display_name: None,
                owner_id: None,
                key_prefix: None,
                segments: 4,
                key_format: KEY_FORMAT_STANDARD.to_string(),
                key_pattern: None,
                default_max_activations: 1,
                default_validity_days: None,
                grace_period_days: grace_days,
                renewal_reminder_days: Vec::new(),
                offline_max_days: 0,
                allowed_distribution_channels: Vec::new(),
                api_allowed_endpoints: Vec::new(),
                api_rate_limit_per_minute: 60,
                created_at: now,
                updated_at: now,
            },
        );
    }

    #[test]
    fn module_license_matches_holds_during_grace() {
        let company = uuid::Uuid::new_v4();
        let slug = format!("grace-entitlement-{}", uuid::Uuid::new_v4());
        insert_grace_product(&slug, 7);
        // Expired yesterday but inside the 7-day grace period: the
        // license still validates, so the entitlement holds.
        let past = chrono::Utc::now() - chrono::Duration::days(1);
        let license = test_license(Some(company), "active", &slug, Some(past));
        assert!(module_license_matches(&license, company, &slug));
        // Expired 8 days ago: grace is over, no entitlement.
        let past = chrono::Utc::now() - chrono::Duration::days(8);
        let license = test_license(Some(company), "active", &slug, Some(past));
        assert!(!module_license_matches(&license, company, &slug));
    }

    fn test_context() -> AppContext {
        AppContext::new(
            unified_core::config::AppConfig::default(),
            std::sync::Arc::new(unified_core::hooks::HookBus::new()),
            std::sync::Arc::new(unified_core::modules::ModuleRegistry::new()),
        )
    }

    #[test]
    fn concurrent_activations_never_exceed_the_seat_limit() {
        // One seat, eight concurrent first-time activations from
        // distinct instances: exactly one may succeed. Without the
        // per-license seat lock, several threads pass the seat check
        // before any of them inserts and the license overshoots.
        let ctx = std::sync::Arc::new(test_context());
        let license = issue_license(
            &ctx,
            generate_license_key(),
            format!("race-product-{}", uuid::Uuid::new_v4()),
            None,
            None,
            1,
            None,
        )
        .expect("license");
        let key = license.license_key.clone();
        let threads = 8;
        let successes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for index in 0..threads {
                let ctx = std::sync::Arc::clone(&ctx);
                let key = key.clone();
                let successes = std::sync::Arc::clone(&successes);
                scope.spawn(move || {
                    let outcome = validate_and_maybe_activate(
                        &ctx,
                        &key,
                        Some(&format!("race-instance-{index}")),
                        None,
                        true,
                    )
                    .expect("validation");
                    if outcome.valid {
                        successes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    } else {
                        assert_eq!(outcome.reason.as_deref(), Some("seats_exceeded"));
                    }
                });
            }
        });
        assert_eq!(
            successes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "exactly one concurrent activation may consume the single seat"
        );
        assert_eq!(activations_for(license.id).len(), 1);
    }

    #[test]
    fn activation_lock_is_stable_per_license() {
        let id = uuid::Uuid::new_v4();
        assert!(std::sync::Arc::ptr_eq(
            &activation_lock_for(id),
            &activation_lock_for(id)
        ));
        assert!(!std::sync::Arc::ptr_eq(
            &activation_lock_for(id),
            &activation_lock_for(uuid::Uuid::new_v4())
        ));
    }

    /// Sellable, non-core test module: an extension a company can
    /// hold by license or by subscription, never by owner
    /// entitlement.
    struct SellableAddonModule;

    impl Module for SellableAddonModule {
        fn id(&self) -> &'static str {
            "test-addon"
        }

        fn name(&self) -> &'static str {
            "Test Addon"
        }

        fn description(&self) -> &'static str {
            "Test-only sellable non-core module"
        }

        fn is_core(&self) -> bool {
            false
        }

        fn available_as_product(&self) -> bool {
            true
        }

        fn product_category(&self) -> Option<&'static str> {
            Some("commerce")
        }
    }

    fn test_context_with_subscription_answer(
        entitled: Option<bool>,
    ) -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::modules::ModuleRegistry;
        let bus = Arc::new(HookBus::new());
        if let Some(entitled) = entitled {
            bus.register(
                SUBSCRIPTION_ENTITLEMENT_CHECK_EVENT,
                "test",
                Arc::new(move |ctx: &mut HookContext| {
                    ctx.add_result(serde_json::json!({
                        "entitled": entitled,
                        "module_id": "test-addon",
                        "source": "subscription",
                    }));
                }),
            );
        }
        let modules = Arc::new(ModuleRegistry::new());
        modules.register(Arc::new(SellableAddonModule));
        AppContext::new(AppConfig::default(), bus, modules)
    }

    #[test]
    fn has_subscription_entitlement_true_when_answerer_entitles() {
        let ctx = test_context_with_subscription_answer(Some(true));
        assert!(has_subscription_entitlement(&ctx, "co-1", "test-addon"));
    }

    #[test]
    fn has_subscription_entitlement_false_when_answerer_denies() {
        let ctx = test_context_with_subscription_answer(Some(false));
        assert!(!has_subscription_entitlement(&ctx, "co-1", "test-addon"));
    }

    #[test]
    fn has_subscription_entitlement_false_when_no_answerer() {
        let ctx = test_context_with_subscription_answer(None);
        assert!(!has_subscription_entitlement(&ctx, "co-1", "test-addon"));
    }

    #[test]
    fn check_module_entitlement_grants_via_subscription() {
        let ctx = test_context_with_subscription_answer(Some(true));
        assert!(check_module_entitlement(&ctx, "co-1", "test-addon").is_ok());
    }

    #[test]
    fn check_module_entitlement_denies_without_license_or_subscription() {
        let ctx = test_context_with_subscription_answer(Some(false));
        let error = check_module_entitlement(&ctx, "co-1", "test-addon")
            .expect_err("no license and no subscription must deny");
        assert_eq!(error.code, 403u16);
    }

    fn test_context_with_subscription_id_answer() -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::modules::ModuleRegistry;
        let bus = Arc::new(HookBus::new());
        bus.register(
            SUBSCRIPTION_ENTITLEMENT_CHECK_EVENT,
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                ctx.add_result(serde_json::json!({
                    "entitled": true,
                    "module_id": "test-addon",
                    "source": "subscription",
                    "subscription_id": "sub-42",
                }));
            }),
        );
        let modules = Arc::new(ModuleRegistry::new());
        modules.register(Arc::new(SellableAddonModule));
        AppContext::new(AppConfig::default(), bus, modules)
    }

    #[test]
    fn subscription_entitlement_id_returns_granting_subscription() {
        let ctx = test_context_with_subscription_id_answer();
        assert_eq!(
            subscription_entitlement_id(&ctx, "co-1", "test-addon").as_deref(),
            Some("sub-42")
        );
    }

    #[test]
    fn subscription_entitlement_id_none_when_no_answerer() {
        let ctx = test_context_with_subscription_answer(None);
        assert_eq!(
            subscription_entitlement_id(&ctx, "co-1", "test-addon"),
            None
        );
    }

    #[test]
    fn subscription_entitlement_id_none_when_answerer_denies() {
        let ctx = test_context_with_subscription_answer(Some(false));
        assert_eq!(
            subscription_entitlement_id(&ctx, "co-1", "test-addon"),
            None
        );
    }
}
