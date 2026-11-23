// Domains module - pure resolution logic.
//
// Split from `src/modules/domains.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::db::DbPool;
use unified_core::db::models::Domain;
use crate::modules::network::DnsVerifier;
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use serde_json::Value;
use uuid::Uuid;

use super::state::{cached_db_pool, cached_hooks, db_pool, domains_store};
use super::tls::probe_tls;

// ---------------------------------------------------------------------------
// SSL status values.
// ---------------------------------------------------------------------------

/// Canonical `ssl_status` values. Anything else is rejected on
/// update; the probe only ever produces values from this set.
pub(crate) const SSL_STATUSES: &[&str] = &[
    "none", "active", "expiring", "expired", "renewing", "unreachable",
];

/// A certificate expiring within this many days is reported as
/// `expiring` (a renewal warning) rather than `active`.
pub(crate) const SSL_RENEW_WARNING_DAYS: i64 = 30;

pub(crate) fn valid_ssl_status(status: &str) -> bool {
    SSL_STATUSES.contains(&status)
}

/// Derive the SSL status from a TLS probe outcome. A certificate
/// counts as `active` only when its expiry is in the future;
/// `expiring` when it expires within the renewal-warning window;
/// `expired` when the presented certificate is already past its
/// expiry; and `unreachable` when the probe could not connect at
/// all (an unreachable host is never mislabelled `expired`). When
/// the handshake succeeds but the expiry cannot be parsed, the
/// previous status is kept: an unreadable certificate is not
/// evidence for any status change.
pub(crate) fn ssl_status_for_probe(
    ssl_ok: bool,
    expires_at: Option<DateTime<Utc>>,
    previous: &str,
    now: DateTime<Utc>,
) -> String {
    if !ssl_ok {
        return "unreachable".to_string();
    }
    match expires_at {
        Some(expires_at) if expires_at <= now => "expired".to_string(),
        Some(expires_at) if expires_at <= now + Duration::days(SSL_RENEW_WARNING_DAYS) => {
            "expiring".to_string()
        }
        Some(_) => "active".to_string(),
        None => previous.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Loading and existence checks (database first, store fallback).
// ---------------------------------------------------------------------------

/// Load one domain: the database row when a pool is attached,
/// otherwise (or when the row is absent) the in-memory store entry.
pub(crate) fn load_domain(ctx: &AppContext, id: Uuid) -> UnifiedResult<Domain> {
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::domains::dsl as dom_dsl;
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        if let Some(row) = dom_dsl::domains
            .filter(dom_dsl::id.eq(id))
            .select(Domain::as_select())
            .first(&mut conn)
            .optional()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load domain: {error}"))
            })?
        {
            return Ok(row);
        }
    }
    domains_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Domain not found"))
}

/// True when a domain with `name` already exists, in the store or
/// in the database, excluding `exclude_id` (the row being updated).
/// The database is consulted as well as the store so a duplicate
/// is still caught after a restart, when the store starts empty.
pub(crate) fn domain_exists_by_name(
    ctx: &AppContext,
    name: &str,
    exclude_id: Option<Uuid>,
) -> UnifiedResult<bool> {
    if domains_store().iter().any(|entry| {
        entry.value().name == name && Some(*entry.key()) != exclude_id
    }) {
        return Ok(true);
    }
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::domains::dsl as dom_dsl;
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        let found = dom_dsl::domains
            .filter(dom_dsl::name.eq(name))
            .select(dom_dsl::id)
            .first::<Uuid>(&mut conn)
            .optional()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot check domain name: {error}"))
            })?
            .map(|id| Some(id) != exclude_id)
            .unwrap_or(false);
        return Ok(found);
    }
    Ok(false)
}

/// True when a Diesel error is a unique-constraint violation
/// (PostgreSQL error 23505). The duplicate pre-check can race with
/// a concurrent insert, so the database constraint stays the
/// authority and callers map this case to 409 instead of 500.
pub(crate) fn is_unique_violation(error: &diesel::result::Error) -> bool {
    matches!(
        error,
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )
    )
}

// ---------------------------------------------------------------------------
// Persistence. Database errors are propagated, never swallowed: a
// failed write must surface instead of silently diverging the
// in-memory store from the database.
// ---------------------------------------------------------------------------

/// Persist `domain` to the database (when a pool is attached) and
/// then to the in-memory store. The store is only updated after a
/// successful database write, so a failed write leaves both copies
/// at the previous values.
pub(crate) fn persist_domain(ctx: &AppContext, domain: &Domain) -> UnifiedResult<()> {
    persist_domain_with_pool(db_pool(ctx), domain)
}

pub(crate) fn persist_domain_with_pool(
    pool: Option<DbPool>,
    domain: &Domain,
) -> UnifiedResult<()> {
    if let Some(pool) = pool {
        use unified_core::db::schema::domains::dsl as dom_dsl;
        #[derive(AsChangeset)]
        #[diesel(table_name = unified_core::db::schema::domains)]
        struct DomainChanges {
            name: Option<String>,
            is_active: Option<bool>,
            ssl_status: Option<String>,
            ssl_expires_at: Option<Option<DateTime<Utc>>>,
        }
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        diesel::update(dom_dsl::domains.filter(dom_dsl::id.eq(domain.id)))
            .set(DomainChanges {
                name: Some(domain.name.clone()),
                is_active: Some(domain.is_active),
                ssl_status: Some(domain.ssl_status.clone()),
                ssl_expires_at: Some(domain.ssl_expires_at),
            })
            .execute(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot persist domain: {error}"))
            })?;
    }
    domains_store().insert(domain.id, domain.clone());
    Ok(())
}

/// Apply a completed SSL renewal / swap to the matching domain
/// record. The status is expiry-aware: a renewal that somehow
/// carries an already-expired certificate is recorded as `expired`,
/// never `active`. The update is persisted to the database through
/// the pool cached at `init` time; when no pool was cached the
/// store is updated and the database failure is reported on stderr
/// (hook handlers have no error channel to return through).
pub(crate) fn apply_ssl_event(domain_id: Option<Uuid>, domain_name: Option<&str>, expires_at: Option<DateTime<Utc>>) {
    let target = domains_store().iter().find_map(|entry| {
        let row = entry.value();
        let matches = domain_id.map_or(false, |id| row.id == id)
            || domain_name.map_or(false, |name| row.name == name);
        if matches {
            Some(row.clone())
        } else {
            None
        }
    });
    if let Some(mut domain) = target {
        let now = Utc::now();
        domain.ssl_status = match expires_at {
            Some(expires_at) if expires_at <= now => "expired".to_string(),
            _ => "active".to_string(),
        };
        if expires_at.is_some() {
            domain.ssl_expires_at = expires_at;
        }
        domain.updated_at = now;
        if let Err(error) = persist_domain_with_pool(cached_db_pool(), &domain) {
            eprintln!(
                "domains: cannot persist SSL event for {}: {}",
                domain.name, error.message
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Verification (shared by the HTTP handler and the whitelabel
// request listener).
// ---------------------------------------------------------------------------

/// Run the DNS + TLS verification for one domain record and return
/// the outcome payload. When `persist` is true the updated record
/// is written back (database via `pool`, then the store); when
/// false the outcome is computed only, for domains that are not
/// managed rows of this module. The outcome is the `domain.verify`
/// payload; callers emit it on the hook bus themselves.
pub(crate) async fn verify_domain_record(
    domain: &Domain,
    selector: Option<&str>,
    pool: Option<DbPool>,
    persist: bool,
) -> UnifiedResult<Value> {
    let mut domain = domain.clone();
    let verifier = DnsVerifier::new();
    let report = verifier
        .check_domain_full(&domain.name, selector)
        .await?;
    let (ssl_ok, ssl_expires_at) = probe_tls(&domain.name).await;
    // A domain is DNS-reachable when it has an A *or* an AAAA record;
    // AAAA-only domains are valid.
    let dns_ok = report.syntax.valid && (!report.a.is_empty() || !report.aaaa.is_empty());
    let mx_ok = !report.mx.is_empty();
    domain.ssl_status = ssl_status_for_probe(ssl_ok, ssl_expires_at, &domain.ssl_status, Utc::now());
    if ssl_expires_at.is_some() {
        domain.ssl_expires_at = ssl_expires_at;
    }
    domain.updated_at = Utc::now();
    if persist {
        persist_domain_with_pool(pool, &domain)?;
    }
    Ok(serde_json::json!({
        "domain_id": domain.id,
        "domain": domain.name,
        "dns_ok": dns_ok,
        "mx_ok": mx_ok,
        "ssl_ok": ssl_ok,
        "ssl_expires_at": ssl_expires_at,
        "ssl_status": domain.ssl_status,
        "dns": report,
    }))
}

/// Find a managed domain row by name: the in-memory store first,
/// then the database through the pool cached at `init` time.
fn find_domain_by_name(name: &str) -> Option<Domain> {
    if let Some(entry) = domains_store()
        .iter()
        .find(|entry| entry.value().name == name)
    {
        return Some(entry.value().clone());
    }
    let pool = cached_db_pool()?;
    use unified_core::db::schema::domains::dsl as dom_dsl;
    let mut conn = pool.get().ok()?;
    dom_dsl::domains
        .filter(dom_dsl::name.eq(name))
        .select(Domain::as_select())
        .first(&mut conn)
        .optional()
        .ok()
        .flatten()
}

/// Run verification for a domain named in a whitelabel request and
/// emit the `domain.verify` outcome, which the whitelabel module
/// already listens to (`on_domain_verify`) to move the branding
/// profile to `verified` / `rejected`. A managed row is updated in
/// place; an unmanaged name is still verified and reported, so the
/// whitelabel profile is never left waiting.
pub(crate) async fn run_whitelabel_domain_verification(domain_name: String) {
    let pool = cached_db_pool();
    let stored = find_domain_by_name(&domain_name);
    let base = stored.clone().unwrap_or_else(|| {
        let now = Utc::now();
        Domain {
            id: Uuid::now_v7(),
            name: domain_name.clone(),
            company_id: None,
            site_id: None,
            is_active: true,
            ssl_status: "none".to_string(),
            ssl_expires_at: None,
            created_at: now,
            updated_at: now,
        }
    });
    match verify_domain_record(&base, None, pool, stored.is_some()).await {
        Ok(outcome) => {
            if let Some(hooks) = cached_hooks() {
                hooks.emit("domain.verify", outcome);
            }
        }
        Err(error) => {
            eprintln!(
                "domains: whitelabel verification for {domain_name} failed: {}",
                error.message
            );
        }
    }
}

/// Spawn an async verification worker the way the scheduler does:
/// on the current Tokio runtime when there is one, otherwise on a
/// dedicated thread with its own runtime (hook handlers are
/// synchronous).
pub(crate) fn spawn_verification_worker<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(future);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssl_status_for_probe_requires_future_expiry_for_active() {
        let now = Utc::now();
        assert_eq!(
            ssl_status_for_probe(true, Some(now + Duration::days(90)), "none", now),
            "active"
        );
        // Handshake succeeded, but the certificate is already expired.
        assert_eq!(
            ssl_status_for_probe(true, Some(now - Duration::days(1)), "active", now),
            "expired"
        );
        // Expiring within the renewal-warning window.
        assert_eq!(
            ssl_status_for_probe(true, Some(now + Duration::days(10)), "none", now),
            "expiring"
        );
        // Probe failure is unreachable, never expired.
        assert_eq!(
            ssl_status_for_probe(false, None, "active", now),
            "unreachable"
        );
        assert_eq!(
            ssl_status_for_probe(false, Some(now + Duration::days(90)), "active", now),
            "unreachable"
        );
        // Unparseable expiry keeps the previous status.
        assert_eq!(
            ssl_status_for_probe(true, None, "renewing", now),
            "renewing"
        );
    }

    #[test]
    fn valid_ssl_status_allowlists_the_canonical_set() {
        for status in ["none", "active", "expiring", "expired", "renewing", "unreachable"] {
            assert!(valid_ssl_status(status), "{status} must be accepted");
        }
        for status in ["", "pending", "ACTIVE", "ok", "ssl-active", "valid"] {
            assert!(!valid_ssl_status(status), "{status} must be rejected");
        }
    }

    #[test]
    fn apply_ssl_event_rejects_expired_renewal() {
        let now = Utc::now();
        let domain = Domain {
            id: Uuid::now_v7(),
            name: "expired.example.com".to_string(),
            company_id: None,
            site_id: None,
            is_active: true,
            ssl_status: "renewing".to_string(),
            ssl_expires_at: None,
            created_at: now,
            updated_at: now,
        };
        domains_store().insert(domain.id, domain.clone());
        apply_ssl_event(Some(domain.id), None, Some(now - Duration::days(1)));
        let stored = domains_store().get(&domain.id).expect("domain still stored");
        assert_eq!(stored.ssl_status, "expired");
        assert_eq!(stored.ssl_expires_at, Some(now - Duration::days(1)));
        domains_store().remove(&domain.id);
    }

    #[test]
    fn apply_ssl_event_marks_valid_renewal_active() {
        let now = Utc::now();
        let expires = now + Duration::days(90);
        let domain = Domain {
            id: Uuid::now_v7(),
            name: "renewed.example.com".to_string(),
            company_id: None,
            site_id: None,
            is_active: true,
            ssl_status: "renewing".to_string(),
            ssl_expires_at: None,
            created_at: now,
            updated_at: now,
        };
        domains_store().insert(domain.id, domain.clone());
        apply_ssl_event(None, Some("renewed.example.com"), Some(expires));
        let stored = domains_store().get(&domain.id).expect("domain still stored");
        assert_eq!(stored.ssl_status, "active");
        assert_eq!(stored.ssl_expires_at, Some(expires));
        domains_store().remove(&domain.id);
    }
}
