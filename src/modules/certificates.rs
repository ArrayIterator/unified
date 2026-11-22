// Certificates module — advanced SSL certificate management.
//
// Config-first certificate management built on `crate::http::tls`
// (SNI storage, self-signed fallback, swap with rollback) and
// `crate::http::acme` (multi-provider ACME, EAB, HTTP-01 renewal).
// The configuration file defines everything (see the `acme` and
// `app.ssl` sections); this module is the API and dashboard surface
// over it, plus the automation around it.
//
// Routes under `/api/v1/certificates`:
//
//   GET    /                        list certificates (paginated)
//   GET    /config                  effective config + provider matrix + validation
//   GET    /validate                validate every configured certificate
//   GET    /history                 renewal history (paginated)
//   POST   /install                 install a custom certificate (validated first)
//   POST   /renew-all               renew every certificate near expiry
//   POST   /check-expiry            run the expiry check now (alerts + summary)
//   GET    /settings                runtime certificate settings (secrets redacted)
//   PUT    /settings                update runtime certificate settings
//   GET    /{domain}                one certificate: detail + recent history
//   POST   /{domain}/validate       test/validate one certificate
//   POST   /{domain}/renew          renew now (body: {"force": bool})
//   POST   /{domain}/force-renew    renew now, regardless of expiry
//   POST   /{domain}/revoke         revoke a certificate (body: {"reason": ...})
//   POST   /{domain}/swap           swap the live certificate (validated first)
//   POST   /{domain}/rollback       restore the previous live certificate
//
// Events emitted (see `cert_events` below): `ssl.renewed`,
// `ssl.renew_failed`, `ssl.revoked`, `ssl.swapped`, `ssl.expiring`,
// `ssl.installed`. The module also listens to the core `ssl.renew`
// event (dispatched by the scheduler and the Domains module) and to
// `ssl.swap` completions (emitted by the server ACME listener).
// Expiry alerts go out as `notification.requested` (user `system`)
// and as `ssl.expiring` events, so the audit log and the
// Notifications module observe them through the hook bus.
//
// Secrets (private keys, EAB credentials) live only in settings /
// config, are never logged, and are never returned by any response:
// responses carry fingerprints and metadata only.
//
// CLI commands (see `register_cli`):
//   cert-list, cert-validate, cert-renew, cert-renew-all,
//   cert-revoke, cert-check-expiry

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{CliCommandDef, CliRegistry, Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::http::acme::{
    provider_matrix, AcmeClient, AcmeConfig, ChallengeKind, KeyType,
};
use crate::http::tls::{
    status_for, validate_config_entry, validate_pem_pair, validate_ssl_config,
    CertificateInfo, CertificateValidation, SslStorage,
};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use clap::{Arg, ArgAction, ArgMatches, Command};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events.
//
// The core bus (`crate::core::hooks::events`) defines only `ssl.renew`
// and `ssl.swap`. The certificate lifecycle events below are defined
// here, in the module that owns them, so no shared file has to change
// for this module to work. For the core definitions to live in
// `crate::core::hooks::events` instead, add one constant per name
// there (for example `pub const SSL_RENEWED: &str = "ssl.renewed";`)
// and switch the emits below to those constants; the audit module
// would then also need the new names in its `AUDITED_EVENTS` list.
// ---------------------------------------------------------------------------

pub mod cert_events {
    /// Emitted after a renewal succeeds. Payload: `domain`,
    /// `provider`, `ssl_expires_at`, `completed: true`.
    pub const SSL_RENEWED: &str = "ssl.renewed";
    /// Emitted after a renewal fails. Payload: `domain`, `provider`,
    /// `error`.
    pub const SSL_RENEW_FAILED: &str = "ssl.renew_failed";
    /// Emitted after a certificate is revoked. Payload: `domain`,
    /// `reason`, `fingerprint_sha256`.
    pub const SSL_REVOKED: &str = "ssl.revoked";
    /// Emitted after a live certificate swap (or rollback). Payload:
    /// `domain`, `ssl_expires_at`, `fingerprint_sha256`,
    /// `previous_fingerprint_sha256`, `rolled_back: bool`.
    pub const SSL_SWAPPED: &str = "ssl.swapped";
    /// Emitted when a certificate approaches expiry (or already
    /// expired). Payload: `domain`, `days_remaining`, `threshold_days`,
    /// `expired: bool`.
    pub const SSL_EXPIRING: &str = "ssl.expiring";
    /// Emitted after a custom certificate is installed. Payload:
    /// `domain`, `fingerprint_sha256`, `ssl_expires_at`, `source`.
    pub const SSL_INSTALLED: &str = "ssl.installed";
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One certificate known to this module: configured, renewed,
/// installed, or observed as a fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateRecord {
    pub id: String,
    pub domain: String,
    /// ACME provider key (`letsencrypt`, `google`, `zerossl`,
    /// `buypass`, `custom`) or `self-signed` for fallbacks.
    pub provider: String,
    /// `valid`, `expiring`, `expired`, `invalid`, `self-signed`,
    /// or `revoked`.
    pub status: String,
    pub issuer: String,
    pub sans: Vec<String>,
    pub fingerprint_sha256: String,
    pub not_before: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub days_remaining: Option<i64>,
    pub self_signed: bool,
    pub wildcard: bool,
    /// `configured`, `dynamic`, `fallback`, or `custom`.
    pub source: String,
    pub revoked: bool,
    pub last_renewed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One renewal attempt, for the renewal history log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewalAttempt {
    pub id: String,
    pub domain: String,
    pub provider: String,
    /// `started`, `succeeded`, `failed`, or `skipped` (not due yet).
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub new_expires_at: Option<DateTime<Utc>>,
    pub forced: bool,
}

/// Runtime certificate settings, seeded from the config file at
/// startup and adjustable through `PUT /settings`. EAB secrets are
/// held here and never serialized: the API only ever returns
/// `AcmeConfig::redacted()`.
#[derive(Debug, Clone)]
pub struct CertModuleSettings {
    /// `acme.enabled` from the config file.
    pub enabled: bool,
    pub acme: AcmeConfig,
    /// How often the expiry check runs, in seconds (default 86400).
    pub check_interval_secs: u64,
}

impl Default for CertModuleSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            acme: AcmeConfig::default(),
            check_interval_secs: 86_400,
        }
    }
}

impl CertModuleSettings {
    fn from_app_config(config: &AppConfig) -> Self {
        Self {
            enabled: config.acme.enabled,
            acme: AcmeConfig::from_app_config(config),
            check_interval_secs: 86_400,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RenewRequest {
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
struct RevokeRequest {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SwapRequest {
    /// New certificate chain in PEM format.
    cert_pem: String,
    /// Private key in PEM format. Never logged or returned.
    key_pem: String,
}

#[derive(Debug, Deserialize)]
struct InstallRequest {
    domain: String,
    /// Certificate chain in PEM format.
    cert_pem: String,
    /// Private key in PEM format. Never logged or returned.
    key_pem: String,
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsRequest {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    directory_url: Option<String>,
    #[serde(default)]
    staging: Option<bool>,
    #[serde(default)]
    contacts: Option<Vec<String>>,
    #[serde(default)]
    eab_key_id: Option<String>,
    #[serde(default)]
    eab_hmac_key: Option<String>,
    #[serde(default)]
    renew_days_before_expiry: Option<i64>,
    #[serde(default)]
    key_type: Option<KeyType>,
    #[serde(default)]
    key_size: Option<u32>,
    #[serde(default)]
    curve: Option<String>,
    #[serde(default)]
    challenge: Option<ChallengeKind>,
    #[serde(default)]
    alert_thresholds_days: Option<Vec<i64>>,
    #[serde(default)]
    check_interval_secs: Option<u64>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct CertificatesState {
    hooks: RwLock<Arc<HookBus>>,
    storage: RwLock<Option<Arc<SslStorage>>>,
    records: DashMap<String, CertificateRecord>,
    history: DashMap<String, RenewalAttempt>,
    settings: RwLock<CertModuleSettings>,
    /// Thresholds (days before expiry, plus -1 for "already
    /// expired") already alerted for, per domain, so the expiry
    /// check does not repeat the same alert on every run.
    alerted_thresholds: DashMap<String, Vec<i64>>,
    /// Configured cert/key file paths per domain (`default` for the
    /// default pair), so validation can re-read the files on demand.
    cert_paths: RwLock<HashMap<String, (String, String)>>,
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn normalize_domain(domain: &str) -> String {
    crate::http::tls::to_ascii_name(domain)
}

impl CertificatesState {
    pub fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            storage: RwLock::new(None),
            records: DashMap::new(),
            history: DashMap::new(),
            settings: RwLock::new(CertModuleSettings::default()),
            alerted_thresholds: DashMap::new(),
            cert_paths: RwLock::new(HashMap::new()),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Ask the Notifications module (through the hook bus) to alert
    /// user `system` about a certificate event.
    fn notify(&self, kind: &str, title: String, body: String) {
        self.emit(
            "notification.requested",
            json!({
                "user_id": "system",
                "title": title,
                "body": body,
                "kind": kind,
            }),
        );
    }

    fn storage(&self) -> Option<Arc<SslStorage>> {
        self.storage.read().expect("storage lock poisoned").clone()
    }

    fn settings(&self) -> CertModuleSettings {
        self.settings.read().expect("settings lock poisoned").clone()
    }

    /// Seed the state from the config file: use the shared storage
    /// when one is attached (`shared_storage`, the handle the HTTP
    /// server built and attached to the context), otherwise build a
    /// private storage with the lenient loader. Either way the
    /// lenient behaviour holds: invalid entries are reported, never
    /// loaded, and never fail startup. One record is created per
    /// certificate, and `ssl.expiring` / `ssl.renew_failed` alerts
    /// are emitted for the entries that could not be used, so a
    /// missing or expired certificate falls back to the self-signed
    /// certificate for that domain instead of taking the server down.
    ///
    /// The shared path seeds records from the live storage
    /// (`list_certificates`, which also covers renewed and dynamic
    /// certificates) and only re-validates the configured entries to
    /// catch invalid ones the shared storage does not hold. The
    /// fallback path seeds from the lenient loader's reports instead.
    /// Callers pass `ctx.tls()` in the server path; the CLI passes
    /// `None` because it has no server context.
    pub fn seed_from_config(
        &self,
        config: &AppConfig,
        hooks: Arc<HookBus>,
        shared_storage: Option<Arc<SslStorage>>,
    ) {
        *self.hooks.write().expect("hooks lock poisoned") = hooks.clone();
        *self
            .settings
            .write()
            .expect("settings lock poisoned") = CertModuleSettings::from_app_config(config);
        {
            let mut paths = self.cert_paths.write().expect("paths lock poisoned");
            if !config.app.ssl.cert.is_empty() && !config.app.ssl.key.is_empty() {
                paths.insert(
                    "default".to_string(),
                    (config.app.ssl.cert.clone(), config.app.ssl.key.clone()),
                );
            }
            for (domain, entry) in &config.app.ssl.domains {
                if !entry.cert.is_empty() && !entry.key.is_empty() {
                    paths.insert(
                        normalize_domain(domain),
                        (entry.cert.clone(), entry.key.clone()),
                    );
                }
            }
        }
        match shared_storage {
            Some(storage) => {
                // Shared handle from the context: the same storage the
                // HTTPS listener serves, so renewals and swaps through
                // the server are visible here (and vice versa) instead
                // of this module serving a stale private copy.
                *self.storage.write().expect("storage lock poisoned") = Some(storage.clone());
                self.seed_records_from_storage(&storage);
                self.report_invalid_config_entries(config);
            }
            None => {
                // Fallback: no shared handle is attached yet. Module
                // init runs before the HTTP server builds the storage
                // (init_all precedes run_server in the start command),
                // and the CLI has no server at all, so this is the
                // normal startup path today: build a private storage
                // with the lenient loader, exactly as before.
                let (storage, reports) = SslStorage::from_config_lenient(&config.app.ssl);
                *self.storage.write().expect("storage lock poisoned") = Some(storage);
                for report in &reports {
                    self.upsert_record_from_validation(report, "custom", "configured");
                    if report.valid {
                        continue;
                    }
                    self.report_invalid_entry(report);
                }
            }
        }
    }

    /// Seed records from the live certificates in a shared storage:
    /// configured, renewed, and dynamic entries. Self-signed fallback
    /// entries are skipped, matching the refresh paths in
    /// `check_expiry` and `list_certificates`.
    fn seed_records_from_storage(&self, storage: &SslStorage) {
        for info in storage.list_certificates() {
            if info.source != "fallback" {
                self.upsert_record_from_info(&info, "acme");
            }
        }
    }

    /// Re-validate the configured entries and report the invalid ones
    /// (record plus alert). Valid entries are already seeded from the
    /// shared storage, so only invalid reports are processed here. A
    /// storage built by the strict loader holds every configured
    /// entry, so this normally finds nothing; it covers handles
    /// attached by another path that skipped an entry.
    fn report_invalid_config_entries(&self, config: &AppConfig) {
        for report in validate_ssl_config(&config.app.ssl) {
            if report.valid {
                continue;
            }
            self.upsert_record_from_validation(&report, "custom", "configured");
            self.report_invalid_entry(&report);
        }
    }

    /// Emit the alert (and notification) for one invalid configured
    /// certificate: an expiry alert when the certificate expired, a
    /// renewal-failure alert otherwise.
    fn report_invalid_entry(&self, report: &CertificateValidation) {
        let reasons = report.reasons.join("; ");
        let expired = report
            .info
            .as_ref()
            .is_some_and(|info| info.days_remaining < 0);
        if expired {
            self.emit(
                cert_events::SSL_EXPIRING,
                json!({
                    "domain": &report.domain,
                    "days_remaining": report.info.as_ref().map(|info| info.days_remaining),
                    "expired": true,
                    "reasons": &report.reasons,
                }),
            );
            self.notify(
                "ssl.expiring",
                format!("Certificate for {} has expired", report.domain),
                format!(
                    "The configured certificate for {} expired and is not being served; the domain falls back to the self-signed certificate. Reasons: {reasons}",
                    report.domain
                ),
            );
        } else {
            self.emit(
                cert_events::SSL_RENEW_FAILED,
                json!({
                    "domain": &report.domain,
                    "error": &reasons,
                    "stage": "config-validation",
                }),
            );
            self.notify(
                "ssl.renew_failed",
                format!("Certificate for {} is invalid", report.domain),
                format!(
                    "The configured certificate for {} could not be loaded and is not being served; the domain falls back to the self-signed certificate. Reasons: {reasons}",
                    report.domain
                ),
            );
        }
    }

    fn upsert_record_from_validation(
        &self,
        report: &CertificateValidation,
        provider: &str,
        source: &str,
    ) {
        let domain = normalize_domain(&report.domain);
        let now = now();
        let (status, issuer, sans, fingerprint, not_before, expires_at, days_remaining, self_signed) =
            match &report.info {
                Some(info) => {
                    let status = if report.valid {
                        info.status.clone()
                    } else {
                        "invalid".to_string()
                    };
                    (
                        status,
                        info.issuer.clone(),
                        info.sans.clone(),
                        info.fingerprint_sha256.clone(),
                        DateTime::parse_from_rfc3339(&info.not_before)
                            .ok()
                            .map(|parsed| parsed.with_timezone(&Utc)),
                        DateTime::parse_from_rfc3339(&info.not_after)
                            .ok()
                            .map(|parsed| parsed.with_timezone(&Utc)),
                        Some(info.days_remaining),
                        info.self_signed,
                    )
                }
                None => (
                    "invalid".to_string(),
                    String::new(),
                    Vec::new(),
                    String::new(),
                    None,
                    None,
                    None,
                    false,
                ),
            };
        let record = CertificateRecord {
            id: self
                .records
                .get(&domain)
                .map(|entry| entry.value().id.clone())
                .unwrap_or_else(new_id),
            domain: domain.clone(),
            provider: provider.to_string(),
            status,
            issuer,
            sans,
            fingerprint_sha256: fingerprint,
            not_before,
            expires_at,
            days_remaining,
            self_signed,
            wildcard: domain.starts_with("*."),
            source: source.to_string(),
            revoked: false,
            last_renewed_at: self
                .records
                .get(&domain)
                .and_then(|entry| entry.value().last_renewed_at),
            last_error: if report.valid {
                None
            } else {
                Some(report.reasons.join("; "))
            },
            created_at: self
                .records
                .get(&domain)
                .map(|entry| entry.value().created_at)
                .unwrap_or(now),
            updated_at: now,
        };
        self.records.insert(domain, record);
    }

    fn upsert_record_from_info(&self, info: &CertificateInfo, provider: &str) {
        let domain = normalize_domain(&info.domain);
        let now = now();
        let record = CertificateRecord {
            id: self
                .records
                .get(&domain)
                .map(|entry| entry.value().id.clone())
                .unwrap_or_else(new_id),
            domain: domain.clone(),
            provider: provider.to_string(),
            status: info.status.clone(),
            issuer: info.issuer.clone(),
            sans: info.sans.clone(),
            fingerprint_sha256: info.fingerprint_sha256.clone(),
            not_before: DateTime::parse_from_rfc3339(&info.not_before)
                .ok()
                .map(|parsed| parsed.with_timezone(&Utc)),
            expires_at: DateTime::parse_from_rfc3339(&info.not_after)
                .ok()
                .map(|parsed| parsed.with_timezone(&Utc)),
            days_remaining: Some(info.days_remaining),
            self_signed: info.self_signed,
            wildcard: domain.starts_with("*."),
            source: info.source.clone(),
            revoked: self
                .records
                .get(&domain)
                .is_some_and(|entry| entry.value().revoked),
            last_renewed_at: self
                .records
                .get(&domain)
                .and_then(|entry| entry.value().last_renewed_at),
            last_error: self
                .records
                .get(&domain)
                .and_then(|entry| entry.value().last_error.clone()),
            created_at: self
                .records
                .get(&domain)
                .map(|entry| entry.value().created_at)
                .unwrap_or(now),
            updated_at: now,
        };
        self.records.insert(domain, record);
    }

    /// Run the expiry check over every known certificate: for each
    /// configured alert threshold (30/14/7/1 days by default) that a
    /// certificate has newly crossed, emit `ssl.expiring` and a
    /// notification; expired certificates alert once as well. Returns
    /// a summary of what was checked.
    pub fn check_expiry(&self) -> Value {
        let settings = self.settings();
        let mut checked = 0u64;
        let mut expiring: Vec<Value> = Vec::new();
        let mut expired: Vec<Value> = Vec::new();
        // Refresh live data from the storage first, so the check sees
        // renewals that arrived through the server ACME listener.
        if let Some(storage) = self.storage() {
            for info in storage.list_certificates() {
                if info.source != "fallback" {
                    self.upsert_record_from_info(&info, "acme");
                }
            }
        }
        let domains: Vec<String> = self
            .records
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        for domain in domains {
            let Some(record) = self.records.get(&domain).map(|entry| entry.value().clone())
            else {
                continue;
            };
            if record.revoked || record.status == "invalid" {
                continue;
            }
            let Some(days) = record.days_remaining else {
                continue;
            };
            checked += 1;
            let mut alerted = self
                .alerted_thresholds
                .get(&domain)
                .map(|entry| entry.value().clone())
                .unwrap_or_default();
            let mut crossed: Vec<i64> = Vec::new();
            if days < 0 {
                if !alerted.contains(&-1) {
                    crossed.push(-1);
                }
                expired.push(json!({
                    "domain": &domain,
                    "days_remaining": days,
                    "expires_at": record.expires_at,
                }));
            } else {
                for threshold in &settings.acme.alert_thresholds_days {
                    if days <= *threshold && !alerted.contains(threshold) {
                        crossed.push(*threshold);
                    }
                }
                if !crossed.is_empty() {
                    expiring.push(json!({
                        "domain": &domain,
                        "days_remaining": days,
                        "expires_at": record.expires_at,
                        "threshold_days": &crossed,
                    }));
                }
            }
            for threshold in &crossed {
                alerted.push(*threshold);
                let expired_flag = *threshold == -1;
                self.emit(
                    cert_events::SSL_EXPIRING,
                    json!({
                        "domain": &domain,
                        "days_remaining": days,
                        "threshold_days": threshold,
                        "expired": expired_flag,
                    }),
                );
                if expired_flag {
                    self.notify(
                        "ssl.expiring",
                        format!("Certificate for {domain} has expired"),
                        format!(
                            "The certificate for {domain} expired {} day(s) ago; the domain falls back to the self-signed certificate until it is renewed.",
                            -days
                        ),
                    );
                } else {
                    self.notify(
                        "ssl.expiring",
                        format!("Certificate for {domain} expires in {days} day(s)"),
                        format!(
                            "The certificate for {domain} expires on {} (in {days} day(s)); renewal is due within the configured window.",
                            record
                                .expires_at
                                .map(|expiry| expiry.to_rfc3339())
                                .unwrap_or_default()
                        ),
                    );
                }
            }
            if !crossed.is_empty() {
                self.alerted_thresholds.insert(domain, alerted);
            }
        }
        json!({
            "checked": checked,
            "expiring": expiring,
            "expired": expired,
        })
    }

    /// Renew one domain through ACME, recording the attempt in the
    /// renewal history. On success the record is updated and both
    /// `ssl.renewed` and the core `ssl.swap` completion event are
    /// emitted (the Domains module listens to the latter); on failure
    /// `ssl.renew_failed` is emitted and a notification is requested.
    pub async fn renew_domain(
        state: &Arc<Self>,
        domain: &str,
        force: bool,
    ) -> UnifiedResult<RenewalAttempt> {
        let domain = normalize_domain(domain);
        let settings = state.settings();
        if !settings.enabled {
            return Err(UnifiedError::unprocessable(
                "ACME is disabled (acme.enabled is false); enable it in the configuration to renew certificates",
            ));
        }
        let storage = state.storage().ok_or_else(|| {
            UnifiedError::internal("certificate storage is not initialized")
        })?;
        let provider = settings.acme.resolve_provider()?;
        let provider_name = settings.acme.provider.clone();
        let attempt = RenewalAttempt {
            id: new_id(),
            domain: domain.clone(),
            provider: provider_name.clone(),
            status: "started".to_string(),
            started_at: now(),
            finished_at: None,
            error: None,
            new_expires_at: None,
            forced: force,
        };
        state.history.insert(attempt.id.clone(), attempt.clone());
        let outcome: UnifiedResult<bool> = async {
            let client = AcmeClient::new_with_provider(
                &provider,
                settings.acme.staging,
                &settings.acme.contacts,
                settings.acme.eab(),
            )
            .await?;
            let key_type = settings.acme.key_type;
            let curve = settings.acme.curve.clone();
            if force {
                client
                    .renew_certificate_with_key_type(&storage, &domain, key_type, &curve)
                    .await?;
                Ok(true)
            } else {
                client
                    .renew_if_expiring_with_key_type(
                        &storage,
                        &domain,
                        settings.acme.renew_days_before_expiry,
                        key_type,
                        &curve,
                    )
                    .await
            }
        }
        .await;
        let mut attempt = attempt;
        attempt.finished_at = Some(now());
        match outcome {
            Ok(true) => {
                let new_expires_at = storage.cert_expiry(&domain);
                attempt.status = "succeeded".to_string();
                attempt.new_expires_at = new_expires_at;
                state.history.insert(attempt.id.clone(), attempt.clone());
                if let Some(info) = storage.certificate_info(&domain) {
                    state.upsert_record_from_info(&info, &provider_name);
                }
                if let Some(mut record) = state.records.get_mut(&domain) {
                    record.last_renewed_at = Some(now());
                    record.last_error = None;
                    record.updated_at = now();
                }
                // Clear the alerted thresholds: the new certificate
                // starts a fresh expiry countdown.
                state.alerted_thresholds.remove(&domain);
                let expires_at = new_expires_at.map(|expiry| expiry.to_rfc3339());
                state.emit(
                    cert_events::SSL_RENEWED,
                    json!({
                        "domain": &domain,
                        "provider": &provider_name,
                        "ssl_expires_at": &expires_at,
                        "completed": true,
                    }),
                );
                // Core completion event, so the Domains module updates
                // its SSL status. The server ACME listener ignores
                // completion payloads, so this cannot re-trigger a
                // renewal.
                state.emit(
                    events::SSL_SWAP,
                    json!({
                        "domain": &domain,
                        "ssl_expires_at": &expires_at,
                        "renewed": true,
                    }),
                );
                Ok(attempt)
            }
            Ok(false) => {
                attempt.status = "skipped".to_string();
                state.history.insert(attempt.id.clone(), attempt.clone());
                Ok(attempt)
            }
            Err(error) => {
                attempt.status = "failed".to_string();
                attempt.error = Some(error.message.clone());
                state.history.insert(attempt.id.clone(), attempt.clone());
                if let Some(mut record) = state.records.get_mut(&domain) {
                    record.last_error = Some(error.message.clone());
                    record.updated_at = now();
                }
                state.emit(
                    cert_events::SSL_RENEW_FAILED,
                    json!({
                        "domain": &domain,
                        "provider": &provider_name,
                        "error": &error.message,
                    }),
                );
                state.notify(
                    "ssl.renew_failed",
                    format!("Certificate renewal failed for {domain}"),
                    format!("Renewal of the certificate for {domain} via {provider_name} failed: {}", error.message),
                );
                Err(error)
            }
        }
    }

    /// Renew every certificate that is due (or every certificate when
    /// `force` is true). Each domain is attempted independently: one
    /// failure never stops the others.
    pub async fn renew_all(
        state: &Arc<Self>,
        force: bool,
    ) -> Vec<RenewalAttempt> {
        let domains: Vec<String> = state
            .records
            .iter()
            .filter(|entry| !entry.value().revoked)
            .map(|entry| entry.key().clone())
            .collect();
        let mut attempts = Vec::new();
        for domain in domains {
            match Self::renew_domain(state, &domain, force).await {
                Ok(attempt) => attempts.push(attempt),
                Err(error) => attempts.push(RenewalAttempt {
                    id: new_id(),
                    domain,
                    provider: state.settings().acme.provider.clone(),
                    status: "failed".to_string(),
                    started_at: now(),
                    finished_at: Some(now()),
                    error: Some(error.message),
                    new_expires_at: None,
                    forced: force,
                }),
            }
        }
        attempts
    }
}

impl Default for CertificatesState {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn a future on the current runtime when there is one, otherwise
/// on a dedicated thread with its own runtime. Hook handlers are
/// synchronous, so renewal work (which needs async I/O) is always
/// spawned this way.
fn spawn_task<F>(future: F)
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

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_certificates(
    state: web::Data<Arc<CertificatesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    // Refresh live data from the storage so renewals that arrived
    // through the server ACME listener show up here.
    if let Some(storage) = state.storage() {
        for info in storage.list_certificates() {
            if info.source != "fallback" {
                state.upsert_record_from_info(&info, "acme");
            }
        }
    }
    let mut items: Vec<CertificateRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            matches_search(&format!("{} {} {}", record.domain, record.issuer, record.status), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| record.status == *status)
                && params
                    .filters
                    .get("source")
                    .is_none_or(|source| record.source == *source)
                && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| record.provider == *provider)
                && params.filters.get("revoked").is_none_or(|flag| {
                    record.revoked == (*flag == "true" || *flag == "1")
                })
        })
        .collect();
    let sort = params.allowed_sort(&["domain", "expires_at", "status"], "domain");
    match sort.as_str() {
        "expires_at" => items.sort_by(|a, b| a.expires_at.cmp(&b.expires_at)),
        "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
        _ => items.sort_by(|a, b| a.domain.cmp(&b.domain)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_config(state: web::Data<Arc<CertificatesState>>) -> HttpResponse {
    let settings = state.settings();
    let validation = match state.storage() {
        Some(_) => {
            // Re-validate the configured files live, so the dashboard
            // sees a changed file without a restart.
            let paths = state
                .cert_paths
                .read()
                .expect("paths lock poisoned")
                .clone();
            let mut reports: Vec<CertificateValidation> = Vec::new();
            let mut domains: Vec<String> = paths.keys().cloned().collect();
            domains.sort();
            for domain in domains {
                let (cert_path, key_path) = &paths[&domain];
                reports.push(validate_config_entry(&domain, cert_path, key_path));
            }
            reports
        }
        None => Vec::new(),
    };
    json::success_response(
        json!({
            "acme": settings.acme.redacted(),
            "acme_enabled": settings.enabled,
            "check_interval_secs": settings.check_interval_secs,
            "providers": provider_matrix(),
            "client_supported_challenges": ["http-01"],
            "validation": validation,
        }),
        200,
    )
}

async fn validate_all(state: web::Data<Arc<CertificatesState>>) -> HttpResponse {
    let paths = state
        .cert_paths
        .read()
        .expect("paths lock poisoned")
        .clone();
    let mut reports: Vec<CertificateValidation> = Vec::new();
    let mut domains: Vec<String> = paths.keys().cloned().collect();
    domains.sort();
    for domain in domains {
        let (cert_path, key_path) = &paths[&domain];
        reports.push(validate_config_entry(&domain, cert_path, key_path));
    }
    // Include live-only certificates (renewed or fallback) that have
    // no configured file pair, validated from their live data.
    if let Some(storage) = state.storage() {
        for info in storage.list_certificates() {
            if paths.contains_key(&info.domain) {
                continue;
            }
            let mut reasons: Vec<String> = Vec::new();
            if info.days_remaining < 0 {
                reasons.push(format!("certificate expired on {}", info.not_after));
            }
            if info.self_signed {
                reasons.push(
                    "self-signed certificate (not issued by a public authority)".to_string(),
                );
            }
            if !info.chain_complete {
                reasons.push(
                    "chain is incomplete: the leaf certificate has no intermediate certificate bundled"
                        .to_string(),
                );
            }
            reports.push(CertificateValidation {
                domain: info.domain.clone(),
                valid: reasons.is_empty(),
                reasons,
                info: Some(info),
            });
        }
    }
    reports.sort_by(|a, b| a.domain.cmp(&b.domain));
    json::success_response(json!({ "certificates": reports }), 200)
}

fn history_for(state: &CertificatesState, domain: &str, limit: usize) -> Vec<RenewalAttempt> {
    let mut items: Vec<RenewalAttempt> = state
        .history
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|attempt| attempt.domain == domain)
        .collect();
    items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    items.truncate(limit);
    items
}

async fn get_certificate(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        let live = state
            .storage()
            .and_then(|storage| storage.certificate_info(&domain));
        if let Some(info) = &live {
            state.upsert_record_from_info(info, "acme");
        }
        let record = state
            .records
            .get(&domain)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("no certificate known for {domain}"))
            })?;
        let previous = state
            .storage()
            .and_then(|storage| storage.previous_info(&domain));
        Ok(json!({
            "certificate": record,
            "live": live,
            "previous": previous,
            "history": history_for(&state, &domain, 10),
        }))
    })();
    json::respond(result, false)
}

async fn validate_certificate(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<CertificateValidation> = (|| {
        // Prefer the configured files: only they carry the private
        // key, so only they prove key-match and chain completeness.
        if let Some((cert_path, key_path)) = state
            .cert_paths
            .read()
            .expect("paths lock poisoned")
            .get(&domain)
            .cloned()
        {
            return Ok(validate_config_entry(&domain, &cert_path, &key_path));
        }
        let info = state
            .storage()
            .and_then(|storage| storage.certificate_info(&domain))
            .ok_or_else(|| {
                UnifiedError::not_found(format!("no certificate known for {domain}"))
            })?;
        let mut reasons: Vec<String> = Vec::new();
        if info.days_remaining < 0 {
            reasons.push(format!("certificate expired on {}", info.not_after));
        }
        if info.self_signed {
            reasons.push(
                "self-signed certificate (not issued by a public authority)".to_string(),
            );
        }
        if !info.chain_complete {
            reasons.push(
                "chain is incomplete: the leaf certificate has no intermediate certificate bundled"
                    .to_string(),
            );
        }
        let valid = reasons.is_empty();
        Ok(CertificateValidation {
            domain,
            valid,
            reasons,
            info: Some(info),
        })
    })();
    json::respond(result, false)
}

async fn renew_certificate_handler(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: Option<web::Json<RenewRequest>>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let force = body.map(|body| body.force).unwrap_or(false);
    let state = state.into_inner();
    let result = CertificatesState::renew_domain(&state, &domain, force).await;
    json::respond(result, false)
}

async fn force_renew_certificate(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let state = state.into_inner();
    let result = CertificatesState::renew_domain(&state, &domain, true).await;
    json::respond(result, false)
}

async fn renew_all_handler(
    state: web::Data<Arc<CertificatesState>>,
    body: Option<web::Json<RenewRequest>>,
) -> HttpResponse {
    let force = body.map(|body| body.force).unwrap_or(false);
    let state = state.into_inner();
    let attempts = CertificatesState::renew_all(&state, force).await;
    let failed = attempts
        .iter()
        .filter(|attempt| attempt.status == "failed")
        .count();
    json::success_response(
        json!({
            "attempts": attempts,
            "failed": failed,
        }),
        200,
    )
}

async fn revoke_certificate_handler(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: Option<web::Json<RevokeRequest>>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let reason = body
        .and_then(|body| body.reason.clone())
        .unwrap_or_else(|| "revoked by an administrator".to_string());
    let result: UnifiedResult<Value> = (|| {
        let storage = state.storage().ok_or_else(|| {
            UnifiedError::internal("certificate storage is not initialized")
        })?;
        let info = storage.revoke_live(&domain)?;
        if let Some(mut record) = state.records.get_mut(&domain) {
            record.revoked = true;
            record.status = "revoked".to_string();
            record.last_error = Some(reason.clone());
            record.updated_at = now();
        }
        state.emit(
            cert_events::SSL_REVOKED,
            json!({
                "domain": &domain,
                "reason": &reason,
                "fingerprint_sha256": &info.fingerprint_sha256,
            }),
        );
        state.notify(
            "ssl.revoked",
            format!("Certificate revoked for {domain}"),
            format!("The certificate for {domain} was revoked ({reason}); the domain falls back to the self-signed certificate."),
        );
        Ok(json!({
            "domain": domain,
            "revoked": true,
            "reason": reason,
            "fingerprint_sha256": info.fingerprint_sha256,
        }))
    })();
    json::respond(result, false)
}

async fn swap_certificate_handler(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: web::Json<SwapRequest>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        let storage = state.storage().ok_or_else(|| {
            UnifiedError::internal("certificate storage is not initialized")
        })?;
        // Validate before anything goes live: a bad replacement must
        // never replace a working certificate.
        let validation = validate_pem_pair(
            body.cert_pem.as_bytes(),
            body.key_pem.as_bytes(),
            Some(&domain),
        );
        if !validation.valid {
            return Err(UnifiedError::bad_request(format!(
                "new certificate failed validation: {}",
                validation.reasons.join("; ")
            ))
            .with_metadata("reasons", json!(validation.reasons)));
        }
        let new_fingerprint = validation
            .info
            .as_ref()
            .map(|info| info.fingerprint_sha256.clone())
            .unwrap_or_default();
        let previous_fingerprint = storage
            .certificate_info(&domain)
            .map(|info| info.fingerprint_sha256);
        storage.swap_certificate(&domain, body.cert_pem.as_bytes(), body.key_pem.as_bytes())?;
        // Post-swap verification: the live certificate must be the
        // one just installed. On any mismatch, roll back to the
        // previous certificate instead of serving the wrong one.
        let live = storage.certificate_info(&domain);
        let verified = live
            .as_ref()
            .is_some_and(|info| info.fingerprint_sha256 == new_fingerprint);
        if !verified {
            let _ = storage.rollback_certificate(&domain);
            return Err(UnifiedError::internal(
                "certificate swap verification failed: the live certificate is not the new one; rolled back to the previous certificate",
            ));
        }
        let info = live.expect("live certificate must exist after a verified swap");
        state.upsert_record_from_info(&info, "custom");
        state.emit(
            cert_events::SSL_SWAPPED,
            json!({
                "domain": &domain,
                "ssl_expires_at": &info.not_after,
                "fingerprint_sha256": &info.fingerprint_sha256,
                "previous_fingerprint_sha256": &previous_fingerprint,
                "rolled_back": false,
            }),
        );
        // Core completion event, so the Domains module updates its
        // SSL status.
        state.emit(
            events::SSL_SWAP,
            json!({
                "domain": &domain,
                "ssl_expires_at": &info.not_after,
            }),
        );
        Ok(json!({
            "domain": domain,
            "swapped": true,
            "certificate": info,
            "previous_fingerprint_sha256": previous_fingerprint,
        }))
    })();
    json::respond(result, false)
}

async fn rollback_certificate_handler(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        let storage = state.storage().ok_or_else(|| {
            UnifiedError::internal("certificate storage is not initialized")
        })?;
        let previous_fingerprint = storage
            .previous_info(&domain)
            .map(|info| info.fingerprint_sha256);
        storage.rollback_certificate(&domain)?;
        let info = storage.certificate_info(&domain).ok_or_else(|| {
            UnifiedError::internal("no live certificate after rollback")
        })?;
        state.upsert_record_from_info(&info, "custom");
        state.emit(
            cert_events::SSL_SWAPPED,
            json!({
                "domain": &domain,
                "ssl_expires_at": &info.not_after,
                "fingerprint_sha256": &info.fingerprint_sha256,
                "previous_fingerprint_sha256": &previous_fingerprint,
                "rolled_back": true,
            }),
        );
        Ok(json!({
            "domain": domain,
            "rolled_back": true,
            "certificate": info,
        }))
    })();
    json::respond(result, false)
}

async fn install_certificate_handler(
    state: web::Data<Arc<CertificatesState>>,
    body: web::Json<InstallRequest>,
) -> HttpResponse {
    let domain = normalize_domain(&body.domain);
    let result: UnifiedResult<Value> = (|| {
        let storage = state.storage().ok_or_else(|| {
            UnifiedError::internal("certificate storage is not initialized")
        })?;
        // Validate before it goes live: key match, expiry, SAN
        // coverage, and chain completeness.
        let validation = validate_pem_pair(
            body.cert_pem.as_bytes(),
            body.key_pem.as_bytes(),
            Some(&domain),
        );
        if !validation.valid {
            return Err(UnifiedError::bad_request(format!(
                "certificate failed validation: {}",
                validation.reasons.join("; ")
            ))
            .with_metadata("reasons", json!(validation.reasons)));
        }
        storage.swap_certificate(&domain, body.cert_pem.as_bytes(), body.key_pem.as_bytes())?;
        let info = storage.certificate_info(&domain).ok_or_else(|| {
            UnifiedError::internal("certificate vanished after install")
        })?;
        state.upsert_record_from_info(&info, "custom");
        if let Some(mut record) = state.records.get_mut(&domain) {
            record.source = "custom".to_string();
            record.provider = "custom".to_string();
            record.revoked = false;
            record.updated_at = now();
        }
        state.emit(
            cert_events::SSL_INSTALLED,
            json!({
                "domain": &domain,
                "fingerprint_sha256": &info.fingerprint_sha256,
                "ssl_expires_at": &info.not_after,
                "source": "custom",
            }),
        );
        Ok(json!({
            "domain": domain,
            "installed": true,
            "certificate": info,
        }))
    })();
    json::respond(result, false)
}

async fn list_history(
    state: web::Data<Arc<CertificatesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<RenewalAttempt> = state
        .history
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|attempt| {
            matches_search(&format!("{} {}", attempt.domain, attempt.status), &params)
                && params
                    .filters
                    .get("domain")
                    .is_none_or(|domain| attempt.domain == *domain)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| attempt.status == *status)
                && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| attempt.provider == *provider)
        })
        .collect();
    items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn check_expiry_handler(state: web::Data<Arc<CertificatesState>>) -> HttpResponse {
    let summary = state.check_expiry();
    json::success_response(summary, 200)
}

async fn get_settings(state: web::Data<Arc<CertificatesState>>) -> HttpResponse {
    let settings = state.settings();
    json::success_response(
        json!({
            "enabled": settings.enabled,
            "acme": settings.acme.redacted(),
            "check_interval_secs": settings.check_interval_secs,
            "providers": provider_matrix(),
        }),
        200,
    )
}

async fn update_settings(
    state: web::Data<Arc<CertificatesState>>,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(enabled) = input.enabled {
            settings.enabled = enabled;
        }
        if let Some(provider) = input.provider {
            settings.acme.provider = provider;
        }
        if let Some(directory_url) = input.directory_url {
            settings.acme.directory_url = directory_url;
        }
        if let Some(staging) = input.staging {
            settings.acme.staging = staging;
        }
        if let Some(contacts) = input.contacts {
            settings.acme.contacts = contacts;
        }
        if let Some(eab_key_id) = input.eab_key_id {
            settings.acme.eab_key_id = eab_key_id;
        }
        if let Some(eab_hmac_key) = input.eab_hmac_key {
            settings.acme.eab_hmac_key = eab_hmac_key;
        }
        if let Some(days) = input.renew_days_before_expiry {
            settings.acme.renew_days_before_expiry = days;
        }
        if let Some(key_type) = input.key_type {
            settings.acme.key_type = key_type;
        }
        if let Some(key_size) = input.key_size {
            settings.acme.key_size = key_size;
        }
        if let Some(curve) = input.curve {
            settings.acme.curve = curve;
        }
        if let Some(challenge) = input.challenge {
            settings.acme.challenge = challenge;
        }
        if let Some(thresholds) = input.alert_thresholds_days {
            settings.acme.alert_thresholds_days = thresholds;
        }
        if let Some(interval) = input.check_interval_secs {
            settings.check_interval_secs = interval;
        }
        let problems = settings.acme.validate();
        if !problems.is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "invalid certificate settings: {}",
                problems.join("; ")
            ))
            .with_metadata("problems", json!(problems)));
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        Ok(json!({
            "enabled": settings.enabled,
            "acme": settings.acme.redacted(),
            "check_interval_secs": settings.check_interval_secs,
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Handle the core `ssl.renew` event: a scheduler job (or the Domains
/// module) asks for a renewal. A payload with a `domain` renews that
/// domain (forced when `force` is true); a payload with `check: true`
/// or no domain runs the expiry check and renews every certificate
/// that is due. Completion payloads (carrying `ssl_expires_at` or
/// `completed`) are ignored here so a renewal is never re-run.
fn on_ssl_renew(state: Arc<CertificatesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    if data.get("ssl_expires_at").is_some() || data.get("completed").is_some() {
        return;
    }
    let force = data
        .get("force")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    match data.get("domain").and_then(|value| value.as_str()) {
        Some(domain) => {
            let domain = domain.to_string();
            let state = state.clone();
            let renewal_domain = domain.clone();
            spawn_task(async move {
                match CertificatesState::renew_domain(&state, &renewal_domain, force).await {
                    Ok(_) => {}
                    Err(error) => {
                        log::error!("certificate renewal failed for {renewal_domain}: {error}");
                    }
                }
            });
            ctx.add_result(json!({ "queued": true, "domain": domain }));
        }
        None => {
            let state = state.clone();
            spawn_task(async move {
                let _summary = state.check_expiry();
                let _attempts = CertificatesState::renew_all(&state, force).await;
            });
            ctx.add_result(json!({ "queued": true, "check": true }));
        }
    }
}

/// Apply a completed renewal / swap (from the server ACME listener or
/// from this module itself) to the matching record.
fn on_ssl_completion(state: Arc<CertificatesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(domain) = data
        .get("domain")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };
    let expires_at = data
        .get("ssl_expires_at")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    if expires_at.is_none() && data.get("completed").is_none() {
        return;
    }
    let domain = normalize_domain(&domain);
    if let Some(storage) = state.storage() {
        if let Some(info) = storage.certificate_info(&domain) {
            state.upsert_record_from_info(&info, "acme");
        }
    }
    if let Some(mut record) = state.records.get_mut(&domain) {
        if let Some(expires_at) = expires_at {
            record.expires_at = Some(expires_at);
            record.days_remaining =
                Some(expires_at.signed_duration_since(now()).num_days());
            record.status =
                status_for(record.days_remaining.unwrap_or(0), record.self_signed).to_string();
        }
        record.last_renewed_at = Some(now());
        record.last_error = None;
        record.updated_at = now();
    }
    state.alerted_thresholds.remove(&domain);
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

pub struct CertificatesModule {
    state: Arc<CertificatesState>,
}

impl CertificatesModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(CertificatesState::new()),
        }
    }
}

impl Default for CertificatesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CertificatesModule {
    fn id(&self) -> &'static str {
        "certificates"
    }

    fn name(&self) -> &'static str {
        "Certificates"
    }

    fn description(&self) -> &'static str {
        "Advanced SSL certificate management: config-first ACME certificates (multi-provider, EAB, HTTP-01), custom certificate install, swap with rollback, revocation, renewal history, and expiry alerts."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["scheduler", "notifications", "domains"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let config = ctx.config.read().expect("config lock poisoned").clone();
        // Prefer the shared storage the HTTP server attached to the
        // context; `ctx.tls()` is `None` at init time in the normal
        // startup order (module init runs before `run_server` builds
        // the storage), so seeding falls back to its own lenient
        // storage there — see `seed_from_config`.
        self.state.seed_from_config(&config, ctx.hooks.clone(), ctx.tls());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            events::SSL_RENEW,
            "certificates",
            Arc::new(move |ctx: &mut HookContext| on_ssl_renew(state.clone(), ctx)),
        );
        for event in [
            events::SSL_SWAP,
            cert_events::SSL_RENEWED,
            cert_events::SSL_SWAPPED,
        ] {
            let state = self.state.clone();
            bus.register(
                event,
                "certificates",
                Arc::new(move |ctx: &mut HookContext| on_ssl_completion(state.clone(), ctx)),
            );
        }
    }

    fn register_cli(&self, registry: &mut CliRegistry) {
        registry.register(CliCommandDef {
            name: "cert-list",
            about: "List certificates with expiry, issuer, SANs, and status",
            command: Command::new("cert-list")
                .about("List certificates with expiry, issuer, SANs, and status"),
            handler: Arc::new(|matches| cli_list(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-validate",
            about: "Validate one certificate (or every configured certificate)",
            command: Command::new("cert-validate")
                .about("Validate one certificate (or every configured certificate)")
                .arg(
                    Arg::new("domain")
                        .value_name("DOMAIN")
                        .required(false)
                        .help("Domain to validate (validates every configured certificate when omitted)"),
                ),
            handler: Arc::new(|matches| cli_validate(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-renew",
            about: "Renew one domain certificate now (ACME)",
            command: Command::new("cert-renew")
                .about("Renew one domain certificate now (ACME)")
                .arg(
                    Arg::new("domain")
                        .value_name("DOMAIN")
                        .required(true)
                        .help("Domain whose certificate to renew"),
                )
                .arg(
                    Arg::new("force")
                        .long("force")
                        .action(ArgAction::SetTrue)
                        .help("Renew regardless of expiry"),
                ),
            handler: Arc::new(|matches| cli_renew(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-renew-all",
            about: "Renew every certificate that is due (ACME)",
            command: Command::new("cert-renew-all")
                .about("Renew every certificate that is due (ACME)")
                .arg(
                    Arg::new("force")
                        .long("force")
                        .action(ArgAction::SetTrue)
                        .help("Renew every certificate regardless of expiry"),
                ),
            handler: Arc::new(|matches| cli_renew_all(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-revoke",
            about: "Revoke a certificate (stops serving it; falls back to self-signed)",
            command: Command::new("cert-revoke")
                .about("Revoke a certificate (stops serving it; falls back to self-signed)")
                .arg(
                    Arg::new("domain")
                        .value_name("DOMAIN")
                        .required(true)
                        .help("Domain whose certificate to revoke"),
                )
                .arg(
                    Arg::new("reason")
                        .long("reason")
                        .value_name("REASON")
                        .required(false)
                        .help("Reason for the revocation"),
                ),
            handler: Arc::new(|matches| cli_revoke(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-check-expiry",
            about: "Run the expiry check now (emits expiry alerts)",
            command: Command::new("cert-check-expiry")
                .about("Run the expiry check now (emits expiry alerts)"),
            handler: Arc::new(|matches| cli_check_expiry(matches)),
        });
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/certificates")
                .route("", web::get().to(list_certificates))
                .route("/config", web::get().to(get_config))
                .route("/validate", web::get().to(validate_all))
                .route("/history", web::get().to(list_history))
                .route("/install", web::post().to(install_certificate_handler))
                .route("/renew-all", web::post().to(renew_all_handler))
                .route("/check-expiry", web::post().to(check_expiry_handler))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/{domain}", web::get().to(get_certificate))
                .route("/{domain}/validate", web::post().to(validate_certificate))
                .route("/{domain}/renew", web::post().to(renew_certificate_handler))
                .route("/{domain}/force-renew", web::post().to(force_renew_certificate))
                .route("/{domain}/revoke", web::post().to(revoke_certificate_handler))
                .route("/{domain}/swap", web::post().to(swap_certificate_handler))
                .route("/{domain}/rollback", web::post().to(rollback_certificate_handler)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/certificates", owner: "certificates", description: "List certificates with expiry, issuer, SANs, and status" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/config", owner: "certificates", description: "Effective certificate config, provider matrix, and validation" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/validate", owner: "certificates", description: "Validate every configured certificate" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/history", owner: "certificates", description: "Renewal history" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/install", owner: "certificates", description: "Install a custom certificate (validated first)" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/renew-all", owner: "certificates", description: "Renew every certificate that is due" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/check-expiry", owner: "certificates", description: "Run the expiry check now" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/settings", owner: "certificates", description: "Runtime certificate settings (secrets redacted)" },
            RouteInfo { method: "PUT", path: "/api/v1/certificates/settings", owner: "certificates", description: "Update runtime certificate settings" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/{domain}", owner: "certificates", description: "One certificate: detail plus recent history" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/validate", owner: "certificates", description: "Test and validate one certificate" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/renew", owner: "certificates", description: "Renew one certificate now" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/force-renew", owner: "certificates", description: "Force-renew one certificate" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/revoke", owner: "certificates", description: "Revoke one certificate" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/swap", owner: "certificates", description: "Swap the live certificate (validated first, rollback on failure)" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/rollback", owner: "certificates", description: "Restore the previous live certificate" },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CertificatesModule::new()));
}

// ---------------------------------------------------------------------------
// CLI handlers.
//
// The CLI builds its own state from the config file (the module state
// is not initialized in the CLI path: `build_cli_registry` never runs
// `init_all`), so every command loads the configuration fresh, builds
// the lenient storage, and does the work directly.
// ---------------------------------------------------------------------------

fn cli_state() -> UnifiedResult<(Arc<CertificatesState>, Arc<HookBus>)> {
    let config = AppConfig::load_default().unwrap_or_default();
    let hooks = Arc::new(HookBus::new());
    let state = Arc::new(CertificatesState::new());
    // The CLI has no server context and no shared storage handle, so
    // seeding builds its own storage with the lenient loader.
    state.seed_from_config(&config, hooks.clone(), None);
    Ok((state, hooks))
}

fn cli_config() -> AppConfig {
    AppConfig::load_default().unwrap_or_default()
}

fn cli_list(_matches: &ArgMatches) -> UnifiedResult<()> {
    let config = cli_config();
    let (storage, _reports) = SslStorage::from_config_lenient(&config.app.ssl);
    let infos = storage.list_certificates();
    if infos.is_empty() {
        println!("No certificates configured or stored.");
        return Ok(());
    }
    println!(
        "{:<32} {:<12} {:<12} {:<40} {}",
        "DOMAIN", "STATUS", "EXPIRES IN", "ISSUER", "SANS"
    );
    for info in infos {
        println!(
            "{:<32} {:<12} {:<12} {:<40} {}",
            info.domain,
            info.status,
            format!("{} day(s)", info.days_remaining),
            info.issuer,
            info.sans.join(", ")
        );
    }
    Ok(())
}

fn cli_validate(matches: &ArgMatches) -> UnifiedResult<()> {
    let config = cli_config();
    match matches.get_one::<String>("domain") {
        Some(domain) => {
            let domain = normalize_domain(domain);
            let paths = config.app.ssl.domains.get(&domain);
            let report = match paths {
                Some(entry) if !entry.cert.is_empty() && !entry.key.is_empty() => {
                    validate_config_entry(&domain, &entry.cert, &entry.key)
                }
                _ => {
                    let (storage, _) = SslStorage::from_config_lenient(&config.app.ssl);
                    match storage.certificate_info(&domain) {
                        Some(info) => CertificateValidation {
                            domain: domain.clone(),
                            valid: info.status == "valid" || info.status == "expiring",
                            reasons: if info.days_remaining < 0 {
                                vec![format!("certificate expired on {}", info.not_after)]
                            } else if info.self_signed {
                                vec!["self-signed certificate (not issued by a public authority)"
                                    .to_string()]
                            } else {
                                Vec::new()
                            },
                            info: Some(info),
                        },
                        None => {
                            return Err(UnifiedError::not_found(format!(
                                "no certificate known for {domain}"
                            )))
                        }
                    }
                }
            };
            print_validation(&report);
            if report.valid {
                Ok(())
            } else {
                Err(UnifiedError::bad_request(format!(
                    "certificate for {domain} failed validation"
                )))
            }
        }
        None => {
            let reports = validate_ssl_config(&config.app.ssl);
            if reports.is_empty() {
                println!("No certificates configured.");
                return Ok(());
            }
            let mut failed = 0;
            for report in &reports {
                print_validation(report);
                if !report.valid {
                    failed += 1;
                }
            }
            if failed == 0 {
                Ok(())
            } else {
                Err(UnifiedError::bad_request(format!(
                    "{failed} certificate(s) failed validation"
                )))
            }
        }
    }
}

fn print_validation(report: &CertificateValidation) {
    println!(
        "{}: {}",
        report.domain,
        if report.valid { "valid" } else { "INVALID" }
    );
    for reason in &report.reasons {
        println!("  - {reason}");
    }
    if let Some(info) = &report.info {
        println!("  issuer: {}", info.issuer);
        println!("  expires: {} (in {} day(s))", info.not_after, info.days_remaining);
        println!("  SANs: {}", info.sans.join(", "));
        println!("  fingerprint (SHA-256): {}", info.fingerprint_sha256);
        println!("  chain: {} certificate(s), complete: {}", info.chain_length, info.chain_complete);
        println!(
            "  key matches: {}",
            info.key_matches
                .map(|matches| if matches { "yes".to_string() } else { "no".to_string() })
                .unwrap_or_else(|| "unknown".to_string())
        );
    }
}

fn run_async<F>(future: F) -> UnifiedResult<()>
where
    F: std::future::Future<Output = UnifiedResult<()>> + Send + 'static,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| UnifiedError::internal(format!("cannot start async runtime: {error}")))?;
    runtime.block_on(future)
}

fn cli_renew(matches: &ArgMatches) -> UnifiedResult<()> {
    let domain = matches
        .get_one::<String>("domain")
        .map(|value| value.to_string())
        .unwrap_or_default();
    let force = matches.get_flag("force");
    let (state, _hooks) = cli_state()?;
    run_async(async move {
        match CertificatesState::renew_domain(&state, &domain, force).await {
            Ok(attempt) => {
                println!(
                    "Renewal for {domain}: {} (new expiry: {})",
                    attempt.status,
                    attempt
                        .new_expires_at
                        .map(|expiry| expiry.to_rfc3339())
                        .unwrap_or_else(|| "-".to_string())
                );
                if attempt.status == "failed" {
                    return Err(UnifiedError::internal(
                        attempt.error.unwrap_or_else(|| "renewal failed".to_string()),
                    ));
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    })
}

fn cli_renew_all(matches: &ArgMatches) -> UnifiedResult<()> {
    let force = matches.get_flag("force");
    let (state, _hooks) = cli_state()?;
    run_async(async move {
        let attempts = CertificatesState::renew_all(&state, force).await;
        let failed = attempts
            .iter()
            .filter(|attempt| attempt.status == "failed")
            .count();
        for attempt in &attempts {
            println!(
                "{}: {} {}",
                attempt.domain,
                attempt.status,
                attempt.error.clone().unwrap_or_default()
            );
        }
        if failed == 0 {
            Ok(())
        } else {
            Err(UnifiedError::internal(format!(
                "{failed} renewal(s) failed"
            )))
        }
    })
}

fn cli_revoke(matches: &ArgMatches) -> UnifiedResult<()> {
    let domain = matches
        .get_one::<String>("domain")
        .map(|value| value.to_string())
        .unwrap_or_default();
    let reason = matches
        .get_one::<String>("reason")
        .cloned()
        .unwrap_or_else(|| "revoked by an administrator".to_string());
    let (state, _hooks) = cli_state()?;
    let storage = state.storage().ok_or_else(|| {
        UnifiedError::internal("certificate storage is not initialized")
    })?;
    let domain = normalize_domain(&domain);
    let info = storage.revoke_live(&domain)?;
    if let Some(mut record) = state.records.get_mut(&domain) {
        record.revoked = true;
        record.status = "revoked".to_string();
        record.last_error = Some(reason.clone());
        record.updated_at = now();
    }
    state.emit(
        cert_events::SSL_REVOKED,
        json!({
            "domain": &domain,
            "reason": &reason,
            "fingerprint_sha256": &info.fingerprint_sha256,
        }),
    );
    println!("Certificate for {domain} revoked ({reason}).");
    println!("The domain now falls back to the self-signed certificate.");
    Ok(())
}

fn cli_check_expiry(_matches: &ArgMatches) -> UnifiedResult<()> {
    let (state, _hooks) = cli_state()?;
    let summary = state.check_expiry();
    let checked = summary.get("checked").and_then(|value| value.as_u64()).unwrap_or(0);
    let expiring = summary
        .get("expiring")
        .and_then(|value| value.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    let expired = summary
        .get("expired")
        .and_then(|value| value.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    println!("Checked {checked} certificate(s): {expiring} expiring, {expired} expired.");
    for entry in summary
        .get("expiring")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
    {
        println!(
            "  expiring: {} ({} day(s) left)",
            entry.get("domain").and_then(|value| value.as_str()).unwrap_or("?"),
            entry.get("days_remaining").and_then(|value| value.as_i64()).unwrap_or(0)
        );
    }
    for entry in summary
        .get("expired")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
    {
        println!(
            "  expired: {}",
            entry.get("domain").and_then(|value| value.as_str()).unwrap_or("?")
        );
    }
    Ok(())
}
