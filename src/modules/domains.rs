// Domains module — domain and subdomain management.
//
// Domains are verified with the Network module's DNS verifier (SPF /
// DKIM / DMARC / MX / A records) plus a live TLS probe on port 443 that
// reads the certificate expiry. SSL lifecycle interconnects with the SSL
// manager through the hook bus:
//   - `POST /{id}/ssl-renew` emits `ssl.renew` for the SSL manager.
//   - This module listens to `ssl.renew` (completed renewals carrying
//     `ssl_expires_at`) and `ssl.swap` and updates the domain's
//     `ssl_status` / `ssl_expires_at` accordingly.
//   - Every verification emits `domain.verify` with the full report, so
//     the Mail and Security modules can observe it.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::events;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::db::models::{Domain, NewDomain};
use crate::modules::company::{company_exists, site_exists};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use rustls::client::danger_client::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};
use serde::Deserialize;
use serde_json::Value;
use std::io::Write;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// In-memory store (used when no database pool is attached).
// ---------------------------------------------------------------------------

static DOMAINS: OnceLock<DashMap<Uuid, Domain>> = OnceLock::new();

fn domains_store() -> &'static DashMap<Uuid, Domain> {
    DOMAINS.get_or_init(DashMap::new)
}

fn db_pool(ctx: &AppContext) -> Option<crate::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

fn paginate<T: serde::Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

// ---------------------------------------------------------------------------
// TLS probe: connect to port 443, complete a TLS handshake, and read the
// leaf certificate's expiry date.
//
// The probe only needs to *read* the certificate, not trust it (the
// domain may be mid-renewal), so it uses a verifier that accepts any
// certificate and then parses the presented leaf certificate itself.
// The blocking socket work runs in `spawn_blocking`.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct AcceptAnyCertificate;

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger_client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger_client::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger_client::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger_client::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger_client::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger_client::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

/// Probe TLS on `domain:443`. Returns `(reachable, certificate_expiry)`.
/// The expiry is best-effort: when the certificate cannot be parsed the
/// probe still reports reachability.
async fn probe_tls(domain: &str) -> (bool, Option<DateTime<Utc>>) {
    let domain = domain.to_string();
    tokio::task::spawn_blocking(move || probe_tls_blocking(&domain))
        .await
        .unwrap_or((false, None))
}

fn probe_tls_blocking(domain: &str) -> (bool, Option<DateTime<Utc>>) {
    let address = match format!("{domain}:443").parse::<std::net::SocketAddr>() {
        Ok(addr) => addr,
        Err(_) => {
            // Hostname, not a literal address: resolve it first.
            match std::net::ToSocketAddrs::to_socket_addrs(&(domain, 443)) {
                Ok(mut addrs) => match addrs.next() {
                    Some(addr) => addr,
                    None => return (false, None),
                },
                Err(_) => return (false, None),
            }
        }
    };
    let timeout = Duration::from_secs(5);
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&address, timeout) else {
        return (false, None);
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let config = Arc::new(
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate))
            .with_no_client_auth(),
    );
    let server_name: ServerName = match domain.to_owned().try_into() {
        Ok(name) => name,
        Err(_) => return (false, None),
    };
    let Ok(mut connection) = ClientConnection::new(config, server_name) else {
        return (false, None);
    };
    // Drive the handshake to completion.
    loop {
        if connection.is_handshaking() {
            match connection.complete_io(&mut stream) {
                Ok(_) => {}
                Err(_) => return (false, None),
            }
        } else {
            break;
        }
    }
    let certificates = connection.peer_certificates().to_vec();
    let mut expiry = None;
    if let Some(leaf) = certificates.first() {
        expiry = certificate_expiry(leaf.as_ref());
    }
    (true, expiry)
}

/// Extract the `not_after` expiry from a DER certificate. Returns `None`
/// when the certificate cannot be parsed.
fn certificate_expiry(der: &[u8]) -> Option<DateTime<Utc>> {
    let certificate = x509_certificate::X509Certificate::from_der(der).ok()?;
    let not_after = certificate.tbs_certificate.validity.not_after;
    let system_time = match not_after {
        x509_certificate::Time::UtcTime(time) => time.to_system_time(),
        x509_certificate::Time::GeneralizedTime(time) => time.to_system_time(),
    };
    Some(DateTime::<Utc>::from(system_time))
}

fn persist_domain(ctx: &AppContext, domain: &Domain) {
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::domains::dsl as dom_dsl;
        #[derive(AsChangeset)]
        #[diesel(table_name = crate::db::schema::domains)]
        struct DomainChanges {
            name: Option<String>,
            is_active: Option<bool>,
            ssl_status: Option<String>,
            ssl_expires_at: Option<Option<DateTime<Utc>>>,
        }
        if let Ok(mut conn) = pool.get() {
            let _ = diesel::update(dom_dsl::domains.filter(dom_dsl::id.eq(domain.id)))
                .set(DomainChanges {
                    name: Some(domain.name.clone()),
                    is_active: Some(domain.is_active),
                    ssl_status: Some(domain.ssl_status.clone()),
                    ssl_expires_at: Some(domain.ssl_expires_at),
                })
                .execute(&mut conn);
        }
    }
    domains_store().insert(domain.id, domain.clone());
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateDomainRequest {
    name: String,
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default)]
    site_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct UpdateDomainRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
    #[serde(default)]
    ssl_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VerifyRequest {
    #[serde(default)]
    selector: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateSubdomainRequest {
    /// Subdomain label, for example `"app"` for `app.example.com`.
    name: String,
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn list_domains(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<Domain> = match db_pool(&ctx) {
            Some(pool) => {
                use crate::db::schema::domains::dsl as dom_dsl;
                let mut conn = pool.get().map_err(|error| {
                    UnifiedError::internal(format!("Cannot get database connection: {error}"))
                })?;
                dom_dsl::domains
                    .select(Domain::as_select())
                    .load(&mut conn)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot load domains: {error}"))
                    })?
            }
            None => domains_store()
                .iter()
                .map(|entry| entry.value().clone())
                .collect(),
        };
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| row.name.to_lowercase().contains(&needle));
        }
        if let Some(status) = params.filters.get("ssl_status") {
            items.retain(|row| row.ssl_status == *status);
        }
        if let Some(company_id) = params.filters.get("company_id") {
            if let Ok(wanted) = company_id.parse::<Uuid>() {
                items.retain(|row| row.company_id == Some(wanted));
            }
        }
        let sort = params.allowed_sort(&["name", "ssl_status", "created_at"], "created_at");
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "ssl_status" => items.sort_by(|a, b| a.ssl_status.cmp(&b.ssl_status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_domain(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateDomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Domain> = (|| {
        let syntax = DnsVerifier::check_domain_syntax(&body.name);
        if !syntax.valid {
            return Err(UnifiedError::bad_request(format!(
                "Invalid domain: {}",
                syntax.issues.join("; ")
            )));
        }
        let name = syntax
            .punycode
            .clone()
            .unwrap_or_else(|| body.name.trim().to_lowercase());
        if let Some(company_id) = body.company_id {
            if !company_exists(&ctx, company_id) {
                return Err(UnifiedError::not_found("Company not found"));
            }
        }
        if let Some(site_id) = body.site_id {
            if !site_exists(&ctx, site_id) {
                return Err(UnifiedError::not_found("Company site not found"));
            }
        }
        if domains_store().iter().any(|entry| entry.value().name == name) {
            return Err(UnifiedError::conflict(format!(
                "Domain \"{name}\" already exists"
            )));
        }
        let now = Utc::now();
        let domain = Domain {
            id: Uuid::now_v7(),
            name,
            company_id: body.company_id,
            site_id: body.site_id,
            is_active: true,
            ssl_status: "none".to_string(),
            ssl_expires_at: None,
            created_at: now,
            updated_at: now,
        };
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::domains::dsl as dom_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::insert_into(dom_dsl::domains)
                .values(&NewDomain {
                    id: domain.id,
                    name: domain.name.clone(),
                    company_id: domain.company_id,
                    site_id: domain.site_id,
                })
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot create domain: {error}"))
                })?;
        }
        domains_store().insert(domain.id, domain.clone());
        Ok(domain)
    })();
    match result {
        Ok(domain) => json::success_response(domain, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_domain(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Domain> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::domains::dsl as dom_dsl;
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
    })();
    json::respond(result, debug)
}

async fn update_domain(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateDomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Domain> = (|| {
        let id = id.into_inner();
        let mut domain = domains_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Domain not found"))?;
        if let Some(name) = &body.name {
            let syntax = DnsVerifier::check_domain_syntax(name);
            if !syntax.valid {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid domain: {}",
                    syntax.issues.join("; ")
                )));
            }
            let name = syntax
                .punycode
                .clone()
                .unwrap_or_else(|| name.trim().to_lowercase());
            if domains_store()
                .iter()
                .any(|entry| entry.value().name == name && *entry.key() != id)
            {
                return Err(UnifiedError::conflict(format!(
                    "Domain \"{name}\" already exists"
                )));
            }
            domain.name = name;
        }
        if let Some(active) = body.is_active {
            domain.is_active = active;
        }
        if let Some(status) = &body.ssl_status {
            domain.ssl_status = status.clone();
        }
        domain.updated_at = Utc::now();
        persist_domain(&ctx, &domain);
        Ok(domain)
    })();
    json::respond(result, debug)
}

async fn delete_domain(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::domains::dsl as dom_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::delete(dom_dsl::domains.filter(dom_dsl::id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete domain: {error}"))
                })?;
        }
        let removed = domains_store().remove(&id).is_some();
        if removed {
            Ok(serde_json::json!({ "deleted": true, "id": id }))
        } else {
            Err(UnifiedError::not_found("Domain not found"))
        }
    })();
    json::respond(result, debug)
}

async fn verify_domain(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: Option<web::Json<VerifyRequest>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let selector = body.as_ref().and_then(|body| body.selector.clone());
    let result: UnifiedResult<Value> = (|| async {
        let mut domain = domains_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Domain not found"))?;
        // DNS checks via the Network module's verifier (also published
        // as a `dns.check` event by the verifier endpoint; the full
        // report is published here as `domain.verify`).
        let verifier = DnsVerifier::new();
        let report = verifier
            .check_domain_full(&domain.name, selector.as_deref())
            .await?;
        let (ssl_ok, ssl_expires_at) = probe_tls(&domain.name).await;
        let dns_ok = report.syntax.valid && !report.a.is_empty();
        let mx_ok = !report.mx.is_empty();
        if ssl_ok {
            domain.ssl_status = "active".to_string();
            domain.ssl_expires_at = ssl_expires_at;
        } else if domain.ssl_status == "active" {
            domain.ssl_status = "expired".to_string();
        }
        domain.updated_at = Utc::now();
        persist_domain(&ctx, &domain);
        let outcome = serde_json::json!({
            "domain_id": domain.id,
            "domain": domain.name,
            "dns_ok": dns_ok,
            "mx_ok": mx_ok,
            "ssl_ok": ssl_ok,
            "ssl_expires_at": ssl_expires_at,
            "ssl_status": domain.ssl_status,
            "dns": report,
        });
        ctx.hooks.emit("domain.verify", outcome.clone());
        Ok(outcome)
    })()
    .await;
    json::respond(result, debug)
}

async fn ssl_renew_handler(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Domain> = (|| {
        let id = id.into_inner();
        let mut domain = domains_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Domain not found"))?;
        domain.ssl_status = "renewing".to_string();
        domain.updated_at = Utc::now();
        persist_domain(&ctx, &domain);
        // Hand the actual renewal to the SSL manager (ACME); it reports
        // back through the `ssl.renew` event with `ssl_expires_at`.
        ctx.hooks.emit(
            events::SSL_RENEW,
            serde_json::json!({
                "domain_id": domain.id,
                "domain": domain.name,
                "source": "domains",
            }),
        );
        Ok(domain)
    })();
    json::respond(result, debug)
}

async fn create_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<CreateSubdomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Domain> = (|| {
        let id = id.into_inner();
        let parent = domains_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Domain not found"))?;
        let label = body.name.trim().to_lowercase();
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(UnifiedError::bad_request(
                "Subdomain name must be a valid DNS label (1-63 characters, letters, digits, hyphens)",
            ));
        }
        let name = format!("{label}.{}", parent.name);
        if domains_store().iter().any(|entry| entry.value().name == name) {
            return Err(UnifiedError::conflict(format!(
                "Domain \"{name}\" already exists"
            )));
        }
        let now = Utc::now();
        let subdomain = Domain {
            id: Uuid::now_v7(),
            name,
            company_id: parent.company_id,
            site_id: parent.site_id,
            is_active: true,
            ssl_status: "none".to_string(),
            ssl_expires_at: None,
            created_at: now,
            updated_at: now,
        };
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::domains::dsl as dom_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::insert_into(dom_dsl::domains)
                .values(&NewDomain {
                    id: subdomain.id,
                    name: subdomain.name.clone(),
                    company_id: subdomain.company_id,
                    site_id: subdomain.site_id,
                })
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot create subdomain: {error}"))
                })?;
        }
        domains_store().insert(subdomain.id, subdomain.clone());
        Ok(subdomain)
    })();
    match result {
        Ok(domain) => json::success_response(domain, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Apply a completed SSL renewal / swap to the matching domain record.
fn apply_ssl_event(domain_id: Option<Uuid>, domain_name: Option<&str>, expires_at: Option<DateTime<Utc>>) {
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
        domain.ssl_status = "active".to_string();
        if expires_at.is_some() {
            domain.ssl_expires_at = expires_at;
        }
        domain.updated_at = Utc::now();
        domains_store().insert(domain.id, domain);
    }
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct DomainsModule;

impl Module for DomainsModule {
    fn id(&self) -> &'static str {
        "domains"
    }

    fn name(&self) -> &'static str {
        "Domain Manager"
    }

    fn description(&self) -> &'static str {
        "Domain and subdomain management with DNS verification, TLS certificate probing, and SSL renewal interconnection."
    }

    fn register_hooks(&self, bus: &crate::core::hooks::HookBus) {
        // A completed renewal (from the SSL manager) carries the domain
        // id/name and the new expiry; our own `ssl-renew` request does
        // not, so it cannot flip the status back prematurely.
        for event in [events::SSL_RENEW, events::SSL_SWAP] {
            bus.register(
                event,
                "domains",
                Arc::new(|context: &mut crate::core::hooks::HookContext| {
                    let domain_id = context
                        .get("domain_id")
                        .and_then(Value::as_str)
                        .and_then(|value| value.parse::<Uuid>().ok());
                    let domain_name = context
                        .get("domain")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let expires_at = context
                        .get("ssl_expires_at")
                        .and_then(Value::as_str)
                        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
                    // Only act on completion payloads, not on requests.
                    if expires_at.is_some() || context.get("completed").is_some() {
                        apply_ssl_event(domain_id, domain_name.as_deref(), expires_at);
                    }
                }),
            );
        }
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/domains", owner: "domains", description: "List domains" },
            RouteInfo { method: "POST", path: "/api/v1/domains", owner: "domains", description: "Create domain" },
            RouteInfo { method: "GET", path: "/api/v1/domains/{id}", owner: "domains", description: "Get domain" },
            RouteInfo { method: "PUT", path: "/api/v1/domains/{id}", owner: "domains", description: "Update domain" },
            RouteInfo { method: "DELETE", path: "/api/v1/domains/{id}", owner: "domains", description: "Delete domain" },
            RouteInfo { method: "POST", path: "/api/v1/domains/{id}/verify", owner: "domains", description: "Verify domain" },
            RouteInfo { method: "POST", path: "/api/v1/domains/{id}/ssl-renew", owner: "domains", description: "Ssl renew handler" },
            RouteInfo { method: "POST", path: "/api/v1/domains/{id}/subdomains", owner: "domains", description: "Create subdomain" },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/domains")
                .route("", web::get().to(list_domains))
                .route("", web::post().to(create_domain))
                .route("/{id}", web::get().to(get_domain))
                .route("/{id}", web::put().to(update_domain))
                .route("/{id}", web::delete().to(delete_domain))
                .route("/{id}/verify", web::post().to(verify_domain))
                .route("/{id}/ssl-renew", web::post().to(ssl_renew_handler))
                .route("/{id}/subdomains", web::post().to(create_subdomain)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DomainsModule));
}
