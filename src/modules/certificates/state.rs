//! Certificates - in-memory state and certificate operations.
//!
//! Part of the `certificates` module split; see `src/modules/certificates.rs`.

use unified_core::config::AppConfig;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::hooks::events;
use unified_core::json;
use unified_core::pagination::PageResult;
use unified_core::pagination::PaginationParams;
use crate::http::acme::AcmeClient;
use unified_core::tls::CertificateInfo;
use unified_core::tls::CertificateValidation;
use unified_core::tls::SslStorage;
use unified_core::tls::validate_ssl_config;
use crate::modules::network::DnsVerifier;
use chrono::DateTime;
use chrono::Utc;
use dashmap::DashMap;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;
use uuid::Uuid;

use super::cert_events;
use super::modes::CertModeDefinition;
use super::modes::CfIpRanges;
use super::modes::CloudflareDetection;
use super::modes::DomainCertMode;
use super::modes::cidr_contains;
use super::modes::default_cert_mode_definitions;
use super::modes::parse_cidr;
use super::types::CertModuleSettings;
use super::types::CertificateRecord;
use super::types::RenewalAttempt;

// State

pub struct CertificatesState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) storage: RwLock<Option<Arc<SslStorage>>>,
    pub(crate) records: DashMap<String, CertificateRecord>,
    pub(crate) history: DashMap<String, RenewalAttempt>,
    pub(crate) settings: RwLock<CertModuleSettings>,
    /// Thresholds (days before expiry, plus -1 for "already
    /// expired") already alerted for, per domain, so the expiry
    /// check does not repeat the same alert on every run.
    pub(crate) alerted_thresholds: DashMap<String, Vec<i64>>,
    /// Configured cert/key file paths per domain (`default` for the
    /// default pair), so validation can re-read the files on demand.
    pub(crate) cert_paths: RwLock<HashMap<String, (String, String)>>,
    /// Per-domain certificate modes (`domain_cert_modes` table).
    pub(crate) cert_modes: DashMap<String, DomainCertMode>,
    /// Mode history per domain: earlier `DomainCertMode` rows, most
    /// recent first (capped at 20 entries per domain).
    pub(crate) mode_history: DashMap<String, Vec<DomainCertMode>>,
    /// The certificate-mode registry (admin-extensible definitions).
    pub(crate) mode_definitions: RwLock<Vec<CertModeDefinition>>,
    /// Stored Cloudflare detection results per domain.
    pub(crate) detections: DashMap<String, CloudflareDetection>,
    /// Stored Cloudflare IP-range snapshot (fetched data + date).
    pub(crate) cf_ranges: RwLock<CfIpRanges>,
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn normalize_domain(domain: &str) -> String {
    unified_core::tls::to_ascii_name(domain)
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
            cert_modes: DashMap::new(),
            mode_history: DashMap::new(),
            mode_definitions: RwLock::new(default_cert_mode_definitions()),
            detections: DashMap::new(),
            cf_ranges: RwLock::new(CfIpRanges::default()),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Ask the Notifications module (through the hook bus) to alert
    /// user `system` about a certificate event.
    pub(crate) fn notify(&self, kind: &str, title: String, body: String) {
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

    pub(crate) fn storage(&self) -> Option<Arc<SslStorage>> {
        self.storage.read().expect("storage lock poisoned").clone()
    }

    pub(crate) fn settings(&self) -> CertModuleSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
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
        *self.settings.write().expect("settings lock poisoned") =
            CertModuleSettings::from_app_config(config);
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
    pub(crate) fn seed_records_from_storage(&self, storage: &SslStorage) {
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
    pub(crate) fn report_invalid_config_entries(&self, config: &AppConfig) {
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
    pub(crate) fn report_invalid_entry(&self, report: &CertificateValidation) {
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

    pub(crate) fn upsert_record_from_validation(
        &self,
        report: &CertificateValidation,
        provider: &str,
        source: &str,
    ) {
        let domain = normalize_domain(&report.domain);
        let now = now();
        let (
            status,
            issuer,
            sans,
            fingerprint,
            not_before,
            expires_at,
            days_remaining,
            self_signed,
        ) = match &report.info {
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

    pub(crate) fn upsert_record_from_info(&self, info: &CertificateInfo, provider: &str) {
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
            let Some(record) = self.records.get(&domain).map(|entry| entry.value().clone()) else {
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
        let storage = state
            .storage()
            .ok_or_else(|| UnifiedError::internal("certificate storage is not initialized"))?;
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
                    format!(
                        "Renewal of the certificate for {domain} via {provider_name} failed: {}",
                        error.message
                    ),
                );
                Err(error)
            }
        }
    }

    /// Renew every certificate that is due (or every certificate when
    /// `force` is true). Each domain is attempted independently: one
    /// failure never stops the others.
    pub async fn renew_all(state: &Arc<Self>, force: bool) -> Vec<RenewalAttempt> {
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
                Err(error) => {
                    // `renew_domain` already recorded the failed
                    // attempt in the history before returning `Err`;
                    // reuse that record so the id in this response
                    // is the same id `GET /history` shows. Only
                    // fabricate one when no attempt was recorded
                    // (the early exits: ACME disabled, or no
                    // storage attached).
                    let attempt = state.latest_failed_attempt(&domain).unwrap_or_else(|| {
                        RenewalAttempt {
                            id: new_id(),
                            domain: domain.clone(),
                            provider: state.settings().acme.provider.clone(),
                            status: "failed".to_string(),
                            started_at: now(),
                            finished_at: Some(now()),
                            error: Some(error.message.clone()),
                            new_expires_at: None,
                            forced: force,
                        }
                    });
                    attempts.push(attempt);
                }
            }
        }
        attempts
    }

    /// The most recent failed renewal attempt for `domain` in the
    /// history, if any.
    pub(crate) fn latest_failed_attempt(&self, domain: &str) -> Option<RenewalAttempt> {
        self.history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|attempt| attempt.domain == domain && attempt.status == "failed")
            .max_by_key(|attempt| attempt.started_at)
    }
}

// Certificate modes and Cloudflare detection (state methods).

impl CertificatesState {
    pub(crate) fn mode_definitions(&self) -> Vec<CertModeDefinition> {
        self.mode_definitions
            .read()
            .expect("mode definitions lock poisoned")
            .clone()
    }

    pub(crate) fn mode_definition(&self, mode: &str) -> Option<CertModeDefinition> {
        self.mode_definitions()
            .into_iter()
            .find(|definition| definition.id == mode)
    }

    pub(crate) fn cf_ranges_snapshot(&self) -> CfIpRanges {
        self.cf_ranges
            .read()
            .expect("cloudflare ranges lock poisoned")
            .clone()
    }

    /// Record a mode change: the previous row moves into the per-domain
    /// history (capped at 20 entries) and the new row becomes current.
    pub(crate) fn record_cert_mode(&self, mode: DomainCertMode) {
        if let Some(previous) = self
            .cert_modes
            .get(&mode.domain)
            .map(|entry| entry.value().clone())
        {
            let mut history = self
                .mode_history
                .get(&mode.domain)
                .map(|entry| entry.value().clone())
                .unwrap_or_default();
            history.insert(0, previous);
            history.truncate(20);
            self.mode_history.insert(mode.domain.clone(), history);
        }
        self.cert_modes.insert(mode.domain.clone(), mode);
    }

    pub(crate) fn mode_history_for(&self, domain: &str) -> Vec<DomainCertMode> {
        self.mode_history
            .get(domain)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Fetch the published Cloudflare IP-range lists and store them as
    /// data with the fetch date. The configured source URLs are the
    /// only fixed values; the ranges themselves are never baked into
    /// the code. On failure the previously stored snapshot is kept
    /// untouched and the error is recorded on it.
    pub async fn refresh_cf_ranges(state: &Arc<Self>) -> UnifiedResult<CfIpRanges> {
        let settings = state.settings();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("Unified-CF-Ranges/1.0")
            .build()
            .map_err(|error| {
                UnifiedError::internal(format!("cannot build HTTP client: {error}"))
            })?;
        let fetch_list = async |url: &str| -> UnifiedResult<Vec<String>> {
            let text = client
                .get(url)
                .send()
                .await
                .map_err(|error| {
                    UnifiedError::new(
                        format!("cannot fetch Cloudflare IP range list from {url}: {error}"),
                        502,
                    )
                })?
                .error_for_status()
                .map_err(|error| {
                    UnifiedError::new(
                        format!("Cloudflare IP range list at {url} returned an error: {error}"),
                        502,
                    )
                })?
                .text()
                .await
                .map_err(|error| {
                    UnifiedError::new(
                        format!("cannot read Cloudflare IP range list from {url}: {error}"),
                        502,
                    )
                })?;
            let mut ranges: Vec<String> = Vec::new();
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if parse_cidr(line).is_none() {
                    return Err(UnifiedError::new(
                        format!(
                            "Cloudflare IP range list at {url} contains an invalid entry: \"{line}\""
                        ),
                        502,
                    ));
                }
                ranges.push(line.to_string());
            }
            if ranges.is_empty() {
                return Err(UnifiedError::new(
                    format!("Cloudflare IP range list at {url} is empty"),
                    502,
                ));
            }
            Ok(ranges)
        };
        let v4 = fetch_list(&settings.cf_ranges_v4_url).await;
        let v6 = fetch_list(&settings.cf_ranges_v6_url).await;
        let (v4, v6) = match (v4, v6) {
            (Ok(v4), Ok(v6)) => (v4, v6),
            (Err(error), _) | (_, Err(error)) => {
                let mut snapshot = state.cf_ranges_snapshot();
                snapshot.fetch_error = Some(error.message.clone());
                *state
                    .cf_ranges
                    .write()
                    .expect("cloudflare ranges lock poisoned") = snapshot;
                return Err(error);
            }
        };
        let snapshot = CfIpRanges {
            v4,
            v6,
            fetched_at: Some(now()),
            source_v4: settings.cf_ranges_v4_url.clone(),
            source_v6: settings.cf_ranges_v6_url.clone(),
            fetch_error: None,
        };
        *state
            .cf_ranges
            .write()
            .expect("cloudflare ranges lock poisoned") = snapshot.clone();
        Ok(snapshot)
    }

    /// Detect Cloudflare usage for `domain` and store the result.
    /// Three methods, each individually enable/disable-able through
    /// settings, each recorded honestly:
    ///
    /// (a) DNS: resolve A/AAAA records and compare them against the
    /// stored Cloudflare IP ranges (fetched live on first use; when
    /// the ranges cannot be fetched the method reports
    /// `ranges_unavailable` instead of guessing).
    /// (b) Live probe: request the domain over HTTPS and look for
    /// `CF-Ray`, `CF-Cache-Status`, and `Server: cloudflare`.
    /// (c) API check: ask the Cloudflare module through the
    /// `cloudflare.zone.lookup` hook whether the domain's zone exists
    /// there and which SSL mode it uses; no answer means "unknown".
    /// Disabled by default (`cf_detect_api` defaults to `false`)
    /// because no module answers that hook yet.
    ///
    /// When Cloudflare is detected the result records the origin
    /// expectation, and when the SSL mode is Flexible (or unconfirmed)
    /// it records an explicit warning: end-to-end encryption is NOT
    /// present between Cloudflare and the origin in Flexible mode.
    pub async fn detect_cloudflare(state: &Arc<Self>, domain: &str) -> CloudflareDetection {
        let domain = normalize_domain(domain);
        let settings = state.settings();
        let mut methods = serde_json::Map::new();

        // (a) DNS check against the stored Cloudflare IP ranges.
        if !settings.cf_detect_dns {
            methods.insert(
                "dns".to_string(),
                json!({ "enabled": false, "result": "disabled" }),
            );
        } else {
            let verifier = DnsVerifier::new();
            match verifier.lookup_ip(&domain).await {
                Ok(addresses) => {
                    let mut snapshot = state.cf_ranges_snapshot();
                    let ranges_available = !snapshot.v4.is_empty() || !snapshot.v6.is_empty();
                    if !ranges_available {
                        // First use (or an expired snapshot): fetch the
                        // published lists now and store them.
                        match Self::refresh_cf_ranges(state).await {
                            Ok(fetched) => snapshot = fetched,
                            Err(error) => {
                                methods.insert(
                                    "dns".to_string(),
                                    json!({
                                        "enabled": true,
                                        "checked": true,
                                        "addresses": addresses.iter().map(|ip| ip.to_string()).collect::<Vec<_>>(),
                                        "result": "ranges_unavailable",
                                        "error": error.message,
                                    }),
                                );
                            }
                        }
                    }
                    if !methods.contains_key("dns") {
                        let mut matched: Vec<String> = Vec::new();
                        for address in &addresses {
                            let in_ranges = snapshot
                                .v4
                                .iter()
                                .chain(snapshot.v6.iter())
                                .any(|range| cidr_contains(range, address));
                            if in_ranges {
                                matched.push(address.to_string());
                            }
                        }
                        methods.insert(
                            "dns".to_string(),
                            json!({
                                "enabled": true,
                                "checked": true,
                                "addresses": addresses.iter().map(|ip| ip.to_string()).collect::<Vec<_>>(),
                                "matched_addresses": matched,
                                "detected": !matched.is_empty(),
                                "ranges_fetched_at": snapshot.fetched_at.map(|at| at.to_rfc3339()),
                                "ranges_source_v4": snapshot.source_v4,
                                "ranges_source_v6": snapshot.source_v6,
                            }),
                        );
                    }
                }
                Err(error) => {
                    methods.insert(
                        "dns".to_string(),
                        json!({
                            "enabled": true,
                            "checked": false,
                            "result": "lookup_failed",
                            "error": error.message,
                        }),
                    );
                }
            }
        }

        // (b) Live HTTPS probe for Cloudflare response headers.
        if !settings.cf_detect_probe {
            methods.insert(
                "probe".to_string(),
                json!({ "enabled": false, "result": "disabled" }),
            );
        } else {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .user_agent("Unified-CF-Detect/1.0")
                .build();
            match client {
                Ok(client) => match client.get(format!("https://{domain}/")).send().await {
                    Ok(response) => {
                        let headers = response.headers();
                        let mut found: Vec<String> = Vec::new();
                        if headers.contains_key("cf-ray") {
                            found.push("cf-ray".to_string());
                        }
                        if headers.contains_key("cf-cache-status") {
                            found.push("cf-cache-status".to_string());
                        }
                        if headers
                            .get("server")
                            .and_then(|value| value.to_str().ok())
                            .is_some_and(|value| value.eq_ignore_ascii_case("cloudflare"))
                        {
                            found.push("server: cloudflare".to_string());
                        }
                        let status = response.status().as_u16();
                        methods.insert(
                            "probe".to_string(),
                            json!({
                                "enabled": true,
                                "checked": true,
                                "http_status": status,
                                "headers_found": found,
                                "detected": !found.is_empty(),
                            }),
                        );
                    }
                    Err(error) => {
                        methods.insert(
                            "probe".to_string(),
                            json!({
                                "enabled": true,
                                "checked": false,
                                "result": "probe_failed",
                                "error": error.to_string(),
                            }),
                        );
                    }
                },
                Err(error) => {
                    methods.insert(
                        "probe".to_string(),
                        json!({
                            "enabled": true,
                            "checked": false,
                            "result": "probe_failed",
                            "error": format!("cannot build HTTP client: {error}"),
                        }),
                    );
                }
            }
        }

        // (c) Cloudflare API check through the hook bus.
        let mut ssl_mode: Option<String> = None;
        if !settings.cf_detect_api {
            methods.insert(
                "api".to_string(),
                json!({ "enabled": false, "result": "disabled" }),
            );
        } else {
            let context = state.bus().emit(
                cert_events::CLOUDFLARE_ZONE_LOOKUP,
                json!({ "domain": &domain, "found": false, "zone_id": null, "ssl_mode": null }),
            );
            let found = context
                .get("found")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let zone_id = context
                .get("zone_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            ssl_mode = context
                .get("ssl_mode")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            let api_note: Value = if context.results.is_empty() {
                json!(
                    "no cloudflare module handler answered the zone lookup; API result is unknown"
                )
            } else {
                Value::Null
            };
            methods.insert(
                "api".to_string(),
                json!({
                    "enabled": true,
                    "checked": true,
                    "handler_answered": !context.results.is_empty(),
                    "found": found,
                    "zone_id": zone_id,
                    "ssl_mode": ssl_mode,
                    "detected": found,
                    "note": api_note,
                }),
            );
        }

        let dns_detected = methods
            .get("dns")
            .and_then(|value| value.get("detected"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let probe_detected = methods
            .get("probe")
            .and_then(|value| value.get("detected"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let api_detected = methods
            .get("api")
            .and_then(|value| value.get("detected"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let detected = dns_detected || probe_detected || api_detected;
        let end_to_end_encrypted = match ssl_mode.as_deref() {
            Some("flexible") => Some(false),
            Some("full") | Some("full_strict") => Some(true),
            _ => None,
        };
        let flexible_warning = if ssl_mode.as_deref() == Some("flexible") {
            Some(
                "Cloudflare SSL mode is Flexible: traffic between Cloudflare and the origin is NOT encrypted end to end. Switch the zone to Full (Strict) with a valid origin certificate before claiming end-to-end encryption."
                    .to_string(),
            )
        } else if detected && ssl_mode.is_none() {
            Some(
                "Cloudflare was detected but the Cloudflare SSL mode could not be confirmed: end-to-end encryption is UNCONFIRMED. Confirm Full (Strict) mode in the Cloudflare dashboard; Flexible mode does NOT encrypt traffic between Cloudflare and the origin."
                    .to_string(),
            )
        } else {
            None
        };
        let detection = CloudflareDetection {
            domain: domain.clone(),
            detected,
            methods: Value::Object(methods),
            ssl_mode,
            end_to_end_encrypted,
            flexible_warning,
            checked_at: now(),
        };
        state.detections.insert(domain.clone(), detection.clone());
        state.emit(
            cert_events::SSL_CF_DETECTED,
            json!({
                "domain": &domain,
                "detected": detected,
                "ssl_mode": &detection.ssl_mode,
                "end_to_end_encrypted": &detection.end_to_end_encrypted,
                "checked_at": detection.checked_at.to_rfc3339(),
            }),
        );
        detection
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
pub(crate) fn spawn_task<F>(future: F)
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

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
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

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_domain_lowercases_and_trims() {
        assert_eq!(normalize_domain("EXAMPLE.COM."), "example.com");
        assert_eq!(normalize_domain("  Example.com  "), "example.com");
        assert_eq!(normalize_domain("example.com"), "example.com");
    }

    #[test]
    fn cloudflare_api_detection_defaults_to_disabled() {
        // No module answers `cloudflare.zone.lookup` yet, so the API
        // method must stay off by default instead of reporting
        // "unknown" on every detection.
        assert!(!CertModuleSettings::default().cf_detect_api);
        assert!(
            !CertModuleSettings::from_app_config(&unified_core::config::AppConfig::default())
                .cf_detect_api
        );
        // The other methods keep their defaults.
        assert!(CertModuleSettings::default().cf_detect_dns);
        assert!(CertModuleSettings::default().cf_detect_probe);
    }

    fn failed_attempt(
        state: &CertificatesState,
        id: &str,
        started_at: DateTime<Utc>,
    ) -> RenewalAttempt {
        let attempt = RenewalAttempt {
            id: id.to_string(),
            domain: "example.com".to_string(),
            provider: "letsencrypt".to_string(),
            status: "failed".to_string(),
            started_at,
            finished_at: Some(started_at),
            error: Some("boom".to_string()),
            new_expires_at: None,
            forced: false,
        };
        state
            .history
            .insert(attempt.id.clone(), attempt.clone());
        attempt
    }

    #[test]
    fn latest_failed_attempt_returns_the_newest_failure() {
        let state = CertificatesState::new();
        let base = now();
        let older = failed_attempt(&state, "older", base - chrono::Duration::minutes(10));
        let newer = failed_attempt(&state, "newer", base);
        // A successful attempt for the same domain must not win.
        state.history.insert(
            "ok".to_string(),
            RenewalAttempt {
                id: "ok".to_string(),
                domain: "example.com".to_string(),
                provider: "letsencrypt".to_string(),
                status: "succeeded".to_string(),
                started_at: base + chrono::Duration::minutes(1),
                finished_at: Some(base + chrono::Duration::minutes(1)),
                error: None,
                new_expires_at: None,
                forced: false,
            },
        );
        assert_eq!(
            state.latest_failed_attempt("example.com").map(|a| a.id),
            Some(newer.id.clone())
        );
        assert_ne!(
            state.latest_failed_attempt("example.com").map(|a| a.id),
            Some(older.id.clone())
        );
        assert!(state.latest_failed_attempt("other.com").is_none());
    }
}
