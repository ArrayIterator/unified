//! SaaS Domains - hook handlers.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use unified_core::json;
use chrono::Utc;
use diesel::prelude::*;
use serde_json::Value;
use serde_json::json;
use uuid::Uuid;

use super::handlers_subdomains::count_live_allocations;
use super::product_subdomains::count_live_product_subdomains;
use super::product_subdomains::product_host_blocked_stores;
use super::product_subdomains::slugify_product_name;
use super::product_subdomains::validate_product_id;
use super::store::aliases_store;
use super::store::allocations_store;
use super::store::product_subdomains_store;
use super::store::reservations_store;
use super::store::settings;
use super::types::ProductSubdomain;
use super::types::SubdomainAllocation;
use super::validation::normalize_domain;
use super::validation::validate_label;

// Hook handlers.

/// Flip a matching alias to `verified` when the verification module
/// reports a completed verification. Only this event may flip an
/// alias to verified - there is deliberately no API endpoint that
/// does it. Payload: `{"domain": ..., "verified": true}` (or
/// `{"alias_id": ..., "verified": true}`).
///
/// NOTE: hook handlers receive no hook bus, so this handler cannot
/// re-emit a `saas_domains.alias.verified` event; the
/// `domain.verification.completed` event itself is the observable
/// signal, and the alias record carries `verified_at`.
pub(crate) fn apply_verification_completed(
    domain: Option<&str>,
    alias_id: Option<Uuid>,
    verified: bool,
) {
    if !verified {
        return;
    }
    // Domain names are case-insensitive: normalise the incoming
    // name before comparing, so a mixed-case event domain still
    // matches the lowercase domain stored on the alias.
    let normalized = domain.map(|name| name.trim().to_lowercase());
    let target = aliases_store().iter().find_map(|entry| {
        let row = entry.value();
        let matches = alias_id.map_or(false, |id| row.id == id)
            || normalized
                .as_deref()
                .map_or(false, |name| row.domain == name);
        if matches && row.status == "pending_verification" {
            Some(row.clone())
        } else {
            None
        }
    });
    if let Some(mut alias) = target {
        alias.status = "verified".to_string();
        alias.verified_at = Some(Utc::now());
        alias.updated_at = Utc::now();
        aliases_store().insert(alias.id, alias);
    }
}

// Product-subdomain hook handlers.
//
// Hook handlers receive no `AppContext` and no hook bus, so they can
// mutate the stores but CANNOT re-emit events and CANNOT run the
// cross-module checks that need a context (`domain_claimed`,
// `company_exists`, the entitlement hooks). Every handler below is
// therefore written defensively: it skips payloads it cannot scope
// safely instead of guessing, and the HTTP endpoints remain the
// paths that verify entitlements and emit the lifecycle events.
// This is the same limitation the alias-verification handler
// already documents, not a new pattern.

/// Flip a matching product subdomain to verified when the
/// verification module reports a completed verification. Accepts
/// the real payload shape (`{"domain", "status": "verified", ...}`
/// or `{"product_subdomain_id", "verified": true}`); the existing
/// alias handler only reads the `verified` boolean, which the
/// verification module does not send - product subdomains accept
/// both shapes here.
pub(crate) fn apply_product_verification_completed(
    domain: Option<&str>,
    product_subdomain_id: Option<Uuid>,
    status: Option<&str>,
    verified: bool,
) {
    if !verified && status != Some("verified") {
        return;
    }
    // Domain names are case-insensitive: normalise the incoming
    // name before comparing, so a mixed-case event domain still
    // matches the lowercase host / custom domain stored on the
    // record.
    let normalized = domain.map(|name| name.trim().to_lowercase());
    let target = product_subdomains_store().iter().find_map(|entry| {
        let row = entry.value();
        let matches = product_subdomain_id.map_or(false, |id| row.id == id)
            || normalized.as_deref().map_or(false, |name| {
                row.host == name || row.custom_domain.as_deref() == Some(name)
            });
        if matches && row.status == "pending_verification" && row.verified_at.is_none() {
            Some(row.clone())
        } else {
            None
        }
    });
    if let Some(mut row) = target {
        let now = Utc::now();
        row.verified_at = Some(now);
        row.updated_at = now;
        product_subdomains_store().insert(row.id, row);
    }
}

/// Claim a product subdomain from an event payload (purchase
/// completion / site-provision request). The claim is created ONLY
/// when the emitting flow attests that entitlements were already
/// validated: `site.provision_requested` is trusted when its
/// `source` is `saas_packages` (that module validates entitlements
/// in its provisioning step before emitting it), and
/// `saas_packages.purchase.completed` is trusted only when it
/// carries `entitlement_verified: true`. Any other payload is
/// skipped: the dashboard claim endpoint (`POST
/// /product-subdomains`) is the path that verifies entitlements.
/// Returns the new record id, or nothing when the claim was
/// skipped. Idempotent: an existing live record for the same
/// (tenant, product/package) or host is returned, never duplicated.
pub(crate) fn claim_product_subdomain_from_event(
    company_id: Uuid,
    product_id_raw: &str,
    package_id_raw: Option<&str>,
    label_raw: &str,
    claimed_by: &str,
) -> Option<Uuid> {
    let cfg = settings();
    if cfg.wildcard_base.trim().is_empty() {
        return None;
    }
    let product_id = validate_product_id(product_id_raw).ok()?;
    let package_id = match package_id_raw {
        Some(raw) if !raw.trim().is_empty() => Some(validate_product_id(raw).ok()?),
        _ => None,
    };
    let label = validate_label(label_raw, &cfg).ok()?;
    let host = format!("{label}.{}", cfg.wildcard_base.trim().to_lowercase());
    if product_host_blocked_stores(&host, None) {
        // A blocked host is only a successful claim when the caller
        // already holds a live record for THIS host: returning the
        // caller's record for the same product/package but a
        // different host would misreport a label taken by another
        // tenant as a successful claim of a different subdomain.
        return product_subdomains_store().iter().find_map(|entry| {
            let row = entry.value();
            if row.company_id == company_id
                && row.product_id == product_id
                && row.package_id == package_id
                && row.host == host
                && (row.status == "pending_verification"
                    || row.status == "active"
                    || row.status == "suspended")
            {
                Some(row.id)
            } else {
                None
            }
        });
    }
    if count_live_product_subdomains(company_id, None, None)
        >= usize::try_from(cfg.max_product_subdomains_per_tenant).unwrap_or(usize::MAX)
    {
        return None;
    }
    if count_live_product_subdomains(company_id, Some(&product_id), package_id.as_deref())
        >= usize::try_from(cfg.max_product_subdomains_per_product).unwrap_or(usize::MAX)
    {
        return None;
    }
    let now = Utc::now();
    let record = ProductSubdomain {
        id: Uuid::now_v7(),
        company_id,
        product_id,
        package_id,
        label,
        host,
        custom_domain: None,
        site_id: None,
        status: "pending_verification".to_string(),
        claimed_by: claimed_by.to_string(),
        claimed_at: now,
        verified_at: None,
        released_at: None,
        grace_until: None,
        created_at: now,
        updated_at: now,
    };
    product_subdomains_store().insert(record.id, record.clone());
    Some(record.id)
}

/// Claim a tenant-level subdomain allocation from an event
/// payload (the tenants provisioning flow). Unlike the product
/// claim above, no `product_id` is involved: the allocation is a
/// generic tenant subdomain (`<label>.<wildcard_base>`) recorded
/// in the allocations store, exactly as the HTTP
/// `POST /subdomains` endpoint creates it.
///
/// Trust basis (same as the product claim): `site.provision_requested`
/// is trusted when its `source` is `tenants` - the tenants module
/// emits it only from its provisioning pipeline, for a tenant it
/// created under a package, after the tenant record itself was
/// validated. The handler, not this function, enforces the source;
/// this function only validates the label, the store-only conflict
/// check, and the per-tenant quota.
///
/// Returns the allocation id and its status, or nothing when the
/// claim was skipped (wildcard base unconfigured, invalid or
/// reserved label, host already claimed by another tenant, or
/// quota reached). Idempotent: an existing live allocation for the
/// same (tenant, host) is returned, never duplicated. A
/// `pending_approval` allocation is returned as-is (the caller
/// decides whether that counts as claimed - the
/// `site.provision_requested` handler only reports `claimed: true`
/// for an `active` allocation, so a pending one stays
/// `subdomain_pending` on the tenant until an admin approves it).
pub(crate) fn claim_tenant_subdomain_from_event(
    company_id: Uuid,
    label_raw: &str,
) -> Option<(Uuid, String)> {
    let cfg = settings();
    if cfg.wildcard_base.trim().is_empty() {
        return None;
    }
    let label = validate_label(label_raw, &cfg).ok()?;
    let host = format!("{label}.{}", cfg.wildcard_base.trim().to_lowercase());
    if let Some(existing) = allocations_store().iter().find_map(|entry| {
        let row = entry.value();
        if row.company_id == company_id
            && row.hostname == host
            && (row.status == "active"
                || row.status == "pending_approval"
                || row.status == "suspended")
        {
            Some((row.id, row.status.clone()))
        } else {
            None
        }
    }) {
        return Some(existing);
    }
    // Store-only conflict check (same coverage as the product-claim
    // path: product subdomains, reservations, aliases, allocations).
    // The Diesel `domains`-table check in `domain_claimed` needs an
    // AppContext, which hook handlers do not have; the HTTP
    // allocation endpoint remains the full cross-module availability
    // source.
    if product_host_blocked_stores(&host, None) {
        return None;
    }
    let live_count = count_live_allocations(company_id);
    if u32::try_from(live_count).unwrap_or(u32::MAX) >= cfg.max_subdomains_per_tenant {
        return None;
    }
    let now = Utc::now();
    // System-driven provisioning claims follow the admin-assignment
    // rule from `create_subdomain`: active immediately, unless the
    // administrator configured approval for tenant assignments.
    let status = if cfg.require_subdomain_approval {
        "pending_approval".to_string()
    } else {
        "active".to_string()
    };
    let allocation = SubdomainAllocation {
        id: Uuid::now_v7(),
        label,
        hostname: host,
        company_id,
        site_id: None,
        status: status.clone(),
        approved_by: None,
        created_at: now,
        updated_at: now,
    };
    allocations_store().insert(allocation.id, allocation.clone());
    Some((allocation.id, status))
}

/// Release every subdomain of one tenant, driven by
/// `saas_domains.subdomain.release_requested` from the tenant-destroy
/// flow (schema already dropped and verified; serving already stopped
/// at suspension). Generic allocations move straight to `released`
/// (they carry no grace period - a released allocation stops
/// resolving immediately). Product subdomains move to `released`
/// with the configured grace period, exactly like a package
/// cancellation, so an in-grace host resolves as blocked with reason
/// `released` (the middleware serves the export notice) and the
/// label frees only after the grace expires. Returns the released
/// allocation ids and product-subdomain ids.
pub(crate) fn apply_tenant_destroy_release(company_id: Uuid) -> (Vec<Uuid>, Vec<Uuid>) {
    let cfg = settings();
    let now = Utc::now();
    let mut released_allocations: Vec<Uuid> = Vec::new();
    for mut entry in allocations_store().iter_mut() {
        let row = entry.value_mut();
        if row.company_id == company_id
            && (row.status == "active"
                || row.status == "suspended"
                || row.status == "pending_approval")
        {
            row.status = "released".to_string();
            row.updated_at = now;
            released_allocations.push(row.id);
        }
    }
    let grace_days = i64::from(cfg.default_product_subdomain_grace_days);
    let mut released_products: Vec<Uuid> = Vec::new();
    for mut entry in product_subdomains_store().iter_mut() {
        let row = entry.value_mut();
        if row.company_id == company_id
            && (row.status == "active"
                || row.status == "suspended"
                || row.status == "pending_verification")
        {
            row.status = "released".to_string();
            row.released_at = Some(now);
            row.grace_until = Some(now + chrono::Duration::days(grace_days));
            row.updated_at = now;
            released_products.push(row.id);
        }
    }
    (released_allocations, released_products)
}

/// Start release (grace period) for every live product subdomain
/// of one tenant scoped to one package. Driven by
/// `saas_packages.cancelled`. The `.release_started` event cannot
/// be emitted from a hook handler (no bus); the state change is
/// the signal, and the HTTP release endpoint / expire sweep emit
/// the events for operator-driven releases.
pub(crate) fn apply_package_cancelled(company_id: Uuid, package_id: &str) {
    let cfg = settings();
    let now = Utc::now();
    let grace_days = i64::from(cfg.default_product_subdomain_grace_days);
    for mut entry in product_subdomains_store().iter_mut() {
        let row = entry.value_mut();
        if row.company_id == company_id
            && row.package_id.as_deref() == Some(package_id)
            && (row.status == "active"
                || row.status == "suspended"
                || row.status == "pending_verification")
        {
            row.status = "released".to_string();
            row.released_at = Some(now);
            row.grace_until = Some(now + chrono::Duration::days(grace_days));
            row.updated_at = now;
        }
    }
}

/// Suspend every live product subdomain affected by a revoked
/// entitlement. Payloads differ by emitter (`license.revoked`
/// carries a license `product`; `module.entitlement.denied`
/// carries `module_id` / `company_id`), so all three spellings
/// are read. A payload without a company id is skipped: suspending
/// without a tenant scope would risk touching another tenant.
pub(crate) fn apply_entitlement_revoked(
    company_id: Option<Uuid>,
    product_id: Option<&str>,
    package_id: Option<&str>,
) {
    let Some(company_id) = company_id else {
        return;
    };
    let now = Utc::now();
    for mut entry in product_subdomains_store().iter_mut() {
        let row = entry.value_mut();
        if row.company_id != company_id {
            continue;
        }
        let product_matches = product_id.map_or(false, |wanted| row.product_id == wanted);
        let package_matches =
            package_id.map_or(false, |wanted| row.package_id.as_deref() == Some(wanted));
        if !product_matches && !package_matches {
            continue;
        }
        if row.status == "active" || row.status == "pending_verification" {
            row.status = "suspended".to_string();
            row.updated_at = now;
        }
    }
}

/// Attach a verified whitelabel custom domain to the product
/// subdomain, inheriting its product scope. The whitelabel payload
/// carries no product identifier, so the attach only happens when
/// the tenant holds exactly one live product subdomain; with
/// several, the event is skipped as ambiguous (the whitelabel
/// module must carry `product_subdomain_id` for multi-product
/// tenants - see the integration notes). Unverified domains are
/// never attached: only `whitelabel.domain.verified` is listened
/// to, never `.requested`. The domain is attached only when it is
/// unclaimed in every store (product subdomains, reservations,
/// aliases, allocations) and no other product record holds it as
/// host or custom domain: product-subdomain resolution runs before
/// alias resolution, so attaching a domain another tenant holds as
/// an alias or reservation would silently steal it. The attach is
/// skipped when the check fails; the whitelabel module must
/// re-emit (or carry `product_subdomain_id`) for a retry.
pub(crate) fn apply_whitelabel_domain_verified(company_id_text: &str, domain_raw: &str) {
    let Ok(company_id) = company_id_text.parse::<Uuid>() else {
        return;
    };
    let Ok(domain) = normalize_domain(domain_raw) else {
        return;
    };
    let live: Vec<ProductSubdomain> = product_subdomains_store()
        .iter()
        .filter_map(|entry| {
            let row = entry.value();
            if row.company_id == company_id
                && (row.status == "active" || row.status == "pending_verification")
            {
                Some(row.clone())
            } else {
                None
            }
        })
        .collect();
    if live.len() != 1 {
        return;
    }
    let mut row = live.into_iter().next().expect("exactly one live record");
    if product_host_blocked_stores(&domain, Some(row.id)) {
        return;
    }
    row.custom_domain = Some(domain);
    row.updated_at = Utc::now();
    product_subdomains_store().insert(row.id, row);
}

/// Answer the AI suggestion hook
/// (`saas_domains.ai.suggest_subdomain_requested`): payload
/// `{"product_name" | "product_id", "company_id"?, "count"?}`.
/// Suggestions only, availability-checked against the live store;
/// nothing is reserved. The AI module gates the call through its
/// own contracts before emitting this event; this handler never
/// calls the AI module back.
pub(crate) fn answer_ai_suggest_subdomain(ctx: &mut unified_core::hooks::HookContext) {
    let product_name = ctx
        .get("product_name")
        .and_then(Value::as_str)
        .or_else(|| ctx.get("product_id").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    if product_name.trim().is_empty() {
        return;
    }
    let count = ctx
        .get("count")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, 10) as usize)
        .unwrap_or(5);
    // Hook handlers have no AppContext, so the availability check
    // runs against the stores only (the same store-only check the
    // hook claim path uses). The HTTP check endpoint remains the
    // full cross-module availability source.
    let cfg = settings();
    let base = slugify_product_name(&product_name);
    if base.is_empty() || cfg.wildcard_base.trim().is_empty() {
        ctx.add_result(json!({ "suggestions": [] }));
        return;
    }
    let mut suggestions: Vec<Value> = Vec::new();
    for suffix in &cfg.product_subdomain_suggestion_suffixes {
        if suggestions.len() >= count {
            break;
        }
        let label = format!("{base}{suffix}");
        if validate_label(&label, &cfg).is_err() {
            continue;
        }
        let host = format!("{label}.{}", cfg.wildcard_base.trim().to_lowercase());
        if product_host_blocked_stores(&host, None) {
            continue;
        }
        suggestions.push(json!({
            "label": label,
            "host": host,
            "available": true,
        }));
    }
    ctx.add_result(json!({
        "product_name": product_name,
        "suggestions": suggestions,
    }));
}

/// Expire stale reservations without an AppContext (hook handlers
/// have none): mutate the store and return the expired ids. The
/// HTTP expire endpoint uses `expire_stale_reservations`, which
/// additionally emits one `saas_domains.reservation.expired` event
/// per reservation.
pub(crate) fn expire_stale_reservations_hook() -> Vec<Uuid> {
    let now = Utc::now();
    let mut expired: Vec<Uuid> = Vec::new();
    for mut entry in reservations_store().iter_mut() {
        let row = entry.value_mut();
        if row.status == "reserved" && row.reserved_until <= now {
            row.status = "expired".to_string();
            row.updated_at = now;
            expired.push(row.id);
        }
    }
    expired
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::store::settings_store;
    use super::super::types::DomainAlias;
    use super::super::types::DomainReservation;
    use chrono::DateTime;
    use chrono::Duration;

    fn unique_suffix() -> String {
        Uuid::now_v7().as_simple().to_string()[..12].to_string()
    }

    fn unique_label(prefix: &str) -> String {
        format!("{prefix}-{}", unique_suffix())
    }

    fn unique_domain(prefix: &str) -> String {
        format!("{prefix}-{}.example.com", unique_suffix())
    }

    /// Point the global settings at a wildcard base. Deliberately
    /// not restored: every test here sets the same value, so the
    /// store stays deterministic across parallel tests.
    fn use_test_settings() {
        let mut cfg = settings();
        cfg.wildcard_base = "tenants.example.com".to_string();
        *settings_store()
            .write()
            .expect("saas_domains settings lock poisoned") = cfg;
    }

    fn seed_product_subdomain(
        company_id: Uuid,
        label: &str,
        status: &str,
        verified_at: Option<DateTime<Utc>>,
    ) -> ProductSubdomain {
        let now = Utc::now();
        let row = ProductSubdomain {
            id: Uuid::now_v7(),
            company_id,
            product_id: "product-a".to_string(),
            package_id: None,
            label: label.to_string(),
            host: format!("{label}.tenants.example.com"),
            custom_domain: None,
            site_id: None,
            status: status.to_string(),
            claimed_by: "test".to_string(),
            claimed_at: now,
            verified_at,
            released_at: None,
            grace_until: None,
            created_at: now,
            updated_at: now,
        };
        product_subdomains_store().insert(row.id, row.clone());
        row
    }

    fn seed_alias(domain: &str, company_id: Uuid, status: &str) -> DomainAlias {
        let now = Utc::now();
        let row = DomainAlias {
            id: Uuid::now_v7(),
            domain: domain.to_string(),
            company_id,
            target_type: "cname_subdomain".to_string(),
            target_value: "app.example.com".to_string(),
            status: status.to_string(),
            site_id: None,
            verified_at: None,
            created_at: now,
            updated_at: now,
        };
        aliases_store().insert(row.id, row.clone());
        row
    }

    fn seed_reservation(domain: &str) -> DomainReservation {
        let now = Utc::now();
        let row = DomainReservation {
            id: Uuid::now_v7(),
            domain: domain.to_string(),
            company_id: Uuid::now_v7(),
            reserved_by: "test".to_string(),
            reserved_until: now + Duration::hours(1),
            status: "reserved".to_string(),
            site_id: None,
            created_at: now,
            updated_at: now,
        };
        reservations_store().insert(row.id, row.clone());
        row
    }

    fn seed_allocation(company_id: Uuid, label: &str, status: &str) -> SubdomainAllocation {
        let now = Utc::now();
        let row = SubdomainAllocation {
            id: Uuid::now_v7(),
            label: label.to_string(),
            hostname: format!("{label}.tenants.example.com"),
            company_id,
            site_id: None,
            status: status.to_string(),
            approved_by: None,
            created_at: now,
            updated_at: now,
        };
        allocations_store().insert(row.id, row.clone());
        row
    }

    #[test]
    fn whitelabel_attach_sets_custom_domain_when_unclaimed() {
        let company = Uuid::now_v7();
        let row = seed_product_subdomain(
            company,
            &unique_label("shop"),
            "active",
            Some(Utc::now()),
        );
        let domain = unique_domain("brand");
        apply_whitelabel_domain_verified(&company.to_string(), &domain);
        let updated = product_subdomains_store().get(&row.id).unwrap();
        assert_eq!(
            updated.value().custom_domain.as_deref(),
            Some(domain.as_str())
        );
        product_subdomains_store().remove(&row.id);
    }

    #[test]
    fn whitelabel_attach_refused_when_domain_held_as_alias() {
        let company = Uuid::now_v7();
        let row = seed_product_subdomain(
            company,
            &unique_label("shop"),
            "active",
            Some(Utc::now()),
        );
        let domain = unique_domain("brand");
        let alias = seed_alias(&domain, Uuid::now_v7(), "verified");
        apply_whitelabel_domain_verified(&company.to_string(), &domain);
        let updated = product_subdomains_store().get(&row.id).unwrap();
        assert!(updated.value().custom_domain.is_none());
        product_subdomains_store().remove(&row.id);
        aliases_store().remove(&alias.id);
    }

    #[test]
    fn whitelabel_attach_refused_when_domain_is_other_product_host() {
        let company = Uuid::now_v7();
        let row = seed_product_subdomain(
            company,
            &unique_label("shop"),
            "active",
            Some(Utc::now()),
        );
        let domain = unique_domain("brand");
        let mut other =
            seed_product_subdomain(Uuid::now_v7(), &unique_label("other"), "active", Some(Utc::now()));
        other.host = domain.clone();
        product_subdomains_store().insert(other.id, other.clone());
        apply_whitelabel_domain_verified(&company.to_string(), &domain);
        let updated = product_subdomains_store().get(&row.id).unwrap();
        assert!(updated.value().custom_domain.is_none());
        product_subdomains_store().remove(&row.id);
        product_subdomains_store().remove(&other.id);
    }

    #[test]
    fn whitelabel_attach_refused_when_domain_is_other_product_custom_domain() {
        let company = Uuid::now_v7();
        let row = seed_product_subdomain(
            company,
            &unique_label("shop"),
            "active",
            Some(Utc::now()),
        );
        let domain = unique_domain("brand");
        let mut other =
            seed_product_subdomain(Uuid::now_v7(), &unique_label("other"), "active", Some(Utc::now()));
        other.custom_domain = Some(domain.clone());
        product_subdomains_store().insert(other.id, other.clone());
        apply_whitelabel_domain_verified(&company.to_string(), &domain);
        let updated = product_subdomains_store().get(&row.id).unwrap();
        assert!(updated.value().custom_domain.is_none());
        product_subdomains_store().remove(&row.id);
        product_subdomains_store().remove(&other.id);
    }

    #[test]
    fn whitelabel_attach_refused_when_domain_reserved() {
        let company = Uuid::now_v7();
        let row = seed_product_subdomain(
            company,
            &unique_label("shop"),
            "active",
            Some(Utc::now()),
        );
        let domain = unique_domain("brand");
        let reservation = seed_reservation(&domain);
        apply_whitelabel_domain_verified(&company.to_string(), &domain);
        let updated = product_subdomains_store().get(&row.id).unwrap();
        assert!(updated.value().custom_domain.is_none());
        product_subdomains_store().remove(&row.id);
        reservations_store().remove(&reservation.id);
    }

    #[test]
    fn blocked_claim_does_not_return_record_for_different_host() {
        use_test_settings();
        let company = Uuid::now_v7();
        let existing = seed_product_subdomain(
            company,
            &unique_label("other"),
            "active",
            Some(Utc::now()),
        );
        let taken_label = unique_label("taken");
        let taken_host = format!("{taken_label}.tenants.example.com");
        let reservation = seed_reservation(&taken_host);
        let claimed =
            claim_product_subdomain_from_event(company, "product-a", None, &taken_label, "test");
        assert_eq!(claimed, None);
        product_subdomains_store().remove(&existing.id);
        reservations_store().remove(&reservation.id);
    }

    #[test]
    fn blocked_claim_returns_existing_record_for_same_host() {
        use_test_settings();
        let company = Uuid::now_v7();
        let label = unique_label("same");
        let existing =
            seed_product_subdomain(company, &label, "pending_verification", None);
        let claimed =
            claim_product_subdomain_from_event(company, "product-a", None, &label, "test");
        assert_eq!(claimed, Some(existing.id));
        product_subdomains_store().remove(&existing.id);
    }

    #[test]
    fn suspended_allocation_counts_toward_quota() {
        use_test_settings();
        let company = Uuid::now_v7();
        let mut ids = Vec::new();
        for index in 0..4 {
            ids.push(seed_allocation(company, &unique_label(&format!("a{index}")), "active").id);
        }
        ids.push(seed_allocation(company, &unique_label("s"), "suspended").id);
        // 4 active + 1 suspended = 5, the default per-tenant quota.
        let claimed = claim_tenant_subdomain_from_event(company, &unique_label("new"));
        assert_eq!(claimed, None);
        for id in ids {
            allocations_store().remove(&id);
        }
    }

    #[test]
    fn released_allocation_does_not_count_toward_quota() {
        use_test_settings();
        let company = Uuid::now_v7();
        let mut ids = Vec::new();
        for index in 0..4 {
            ids.push(seed_allocation(company, &unique_label(&format!("a{index}")), "active").id);
        }
        ids.push(seed_allocation(company, &unique_label("r"), "released").id);
        let claimed = claim_tenant_subdomain_from_event(company, &unique_label("new"));
        let (id, status) = claimed.expect("released allocation must not block the quota");
        assert_eq!(status, "active");
        ids.push(id);
        for id in ids {
            allocations_store().remove(&id);
        }
    }

    #[test]
    fn verification_completed_matches_mixed_case_domain() {
        let alias = seed_alias("mixed.example.com", Uuid::now_v7(), "pending_verification");
        apply_verification_completed(Some("MiXeD.Example.COM"), None, true);
        let updated = aliases_store().get(&alias.id).unwrap();
        assert_eq!(updated.value().status, "verified");
        assert!(updated.value().verified_at.is_some());
        aliases_store().remove(&alias.id);
    }

    #[test]
    fn product_verification_completed_matches_mixed_case_domain() {
        let label = unique_label("app");
        let row = seed_product_subdomain(Uuid::now_v7(), &label, "pending_verification", None);
        let mixed_case_host = row.host.to_uppercase();
        apply_product_verification_completed(Some(&mixed_case_host), None, Some("verified"), false);
        let updated = product_subdomains_store().get(&row.id).unwrap();
        assert!(updated.value().verified_at.is_some());
        product_subdomains_store().remove(&row.id);
    }
}
