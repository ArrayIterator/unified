// ACME client (HTTP-01) built on instant-acme.
//
// The account key is a P-256 ECDSA key persisted at `data/acme/account.key`
// (instant-acme only supports P-256 account keys). Challenge responses are
// published through a shared token map; the HTTP server must serve
// `/.well-known/acme-challenge/<token>` from it on port 80.
//
// Several authorities are supported through `AcmeProvider` (Let's Encrypt
// by default, plus Google Trust Services, ZeroSSL, BuyPass, and custom
// directory URLs); providers that require External Account Binding pass
// their EAB credentials at account creation.

use unified_core::config::AppConfig;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::tls::{to_ascii_name, SslStorage};
use chrono::{Duration, Utc};
use futures_util::StreamExt;
use instant_acme::{
    Account, AuthorizationHandle, AuthorizationStatus, ChallengeStatus, ChallengeType,
    ExternalAccountKey, Identifier, Key, NewAccount, NewOrder, Order, OrderStatus,
    RetryPolicy,
};
use rcgen::{
    CertificateParams, DistinguishedName, KeyPair, PKCS_ECDSA_P256_SHA256,
    PKCS_ECDSA_P384_SHA384, PKCS_RSA_SHA256,
};
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

/// Let's Encrypt production directory.
pub const LETS_ENCRYPT_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";
/// Let's Encrypt staging directory (use for testing).
pub const LETS_ENCRYPT_STAGING_URL: &str =
    "https://acme-staging-v02.api.letsencrypt.org/directory";
/// Google Trust Services production directory.
pub const GOOGLE_URL: &str = "https://dv.acme-v02.api.pki.goog/directory";
/// Google Trust Services staging directory.
pub const GOOGLE_STAGING_URL: &str = "https://dv.acme-v02.test-api.pki.goog/directory";
/// ZeroSSL production directory (requires External Account Binding).
pub const ZEROSSL_URL: &str = "https://acme.zerossl.com/v2/DV90";
/// BuyPass production directory.
pub const BUYPASS_URL: &str = "https://api.buypass.com/acme/directory";
/// BuyPass staging directory.
pub const BUYPASS_STAGING_URL: &str = "https://api.test.buypass.com/acme/directory";

/// Default renewal window: renew when the certificate expires within
/// this many days.
pub const DEFAULT_RENEW_DAYS: i64 = 30;
/// Renewal window lower bound: renewing earlier than this wastes the
/// authority's rate-limit budget for no benefit.
pub const MIN_RENEW_DAYS: i64 = 7;
/// Renewal window upper bound: a certificate is never valid longer
/// than 90 days, so renewing earlier than that is meaningless.
pub const MAX_RENEW_DAYS: i64 = 90;

/// Clamp a caller-supplied renewal window to the supported range.
fn clamp_renew_days(days: i64) -> i64 {
    days.clamp(MIN_RENEW_DAYS, MAX_RENEW_DAYS)
}

/// An ACME certificate authority. Only the directory URL (and, for
/// ZeroSSL, External Account Binding) differs between providers for
/// account creation and ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcmeProvider {
    LetsEncrypt,
    Google,
    ZeroSsl,
    BuyPass,
    /// A custom authority, holding its production directory URL.
    Custom(String),
}

impl AcmeProvider {
    /// Display name of the provider, for error messages.
    pub fn name(&self) -> &str {
        match self {
            Self::LetsEncrypt => "Let's Encrypt",
            Self::Google => "Google Trust Services",
            Self::ZeroSsl => "ZeroSSL",
            Self::BuyPass => "BuyPass",
            Self::Custom(_) => "custom ACME provider",
        }
    }

    /// Directory URL for this provider. Returns an error when the
    /// provider has no staging directory and staging was requested.
    pub fn directory_url(&self, staging: bool) -> UnifiedResult<String> {
        match self {
            Self::LetsEncrypt => Ok(if staging {
                LETS_ENCRYPT_STAGING_URL.to_string()
            } else {
                LETS_ENCRYPT_URL.to_string()
            }),
            Self::Google => Ok(if staging {
                GOOGLE_STAGING_URL.to_string()
            } else {
                GOOGLE_URL.to_string()
            }),
            Self::ZeroSsl => {
                if staging {
                    Err(UnifiedError::bad_request(
                        "ZeroSSL does not offer a staging directory",
                    ))
                } else {
                    Ok(ZEROSSL_URL.to_string())
                }
            }
            Self::BuyPass => Ok(if staging {
                BUYPASS_STAGING_URL.to_string()
            } else {
                BUYPASS_URL.to_string()
            }),
            Self::Custom(url) => {
                if staging {
                    Err(UnifiedError::bad_request(
                        "a custom ACME provider has no staging directory; use its production URL",
                    ))
                } else {
                    Ok(url.clone())
                }
            }
        }
    }

    /// Whether this client can obtain a wildcard certificate from
    /// the provider. Always false: the providers can issue
    /// wildcards, but only over DNS-01, and this client is HTTP-01
    /// only — `order_certificate` rejects `*.` domains. Reporting
    /// the provider's capability here would over-claim what the
    /// client can actually do.
    pub fn supports_wildcard(&self) -> bool {
        false
    }

    /// True when the provider can issue certificates for IP addresses.
    /// Only ZeroSSL does among the built-in providers.
    pub fn supports_ip(&self) -> bool {
        matches!(self, Self::ZeroSsl)
    }

    /// True when account creation requires External Account Binding.
    pub fn requires_eab(&self) -> bool {
        matches!(self, Self::ZeroSsl)
    }
}

/// Where the ACME account key is stored, relative to the working directory.
pub const ACCOUNT_KEY_PATH: &str = "data/acme/account.key";

/// Process-wide shared HTTP-01 challenge store: token -> key
/// authorization. `order_certificate` publishes its tokens here (in
/// addition to the per-client instance map) so the HTTP server can
/// answer `/.well-known/acme-challenge/<token>` without holding a
/// reference to any one `AcmeClient`.
static CHALLENGES: OnceLock<Arc<RwLock<HashMap<String, String>>>> = OnceLock::new();

pub fn challenge_store() -> &'static Arc<RwLock<HashMap<String, String>>> {
    CHALLENGES.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

fn acme_err(error: instant_acme::Error) -> UnifiedError {
    UnifiedError::internal(format!("ACME error: {error}"))
}

/// Deactivate a pending or valid authorization, so a failed order does
/// not leave a dangling authorization consuming the authority's rate
/// limit. Failures are ignored: deactivation is best-effort cleanup.
async fn deactivate(authorization: &mut AuthorizationHandle<'_>) {
    if matches!(
        authorization.status,
        AuthorizationStatus::Pending | AuthorizationStatus::Valid
    ) {
        authorization.deactivate().await.ok();
    }
}

/// Deactivate one authorization of an order by its position in the
/// order's authorization list, using a fresh handle.
///
/// `AuthorizationHandle::challenge` takes `&'a mut self` where `'a`
/// is the handle's own lifetime, so once a challenge handle has been
/// created the original handle stays mutably borrowed for the rest
/// of the iteration and cannot be passed to `deactivate` again. A
/// fresh handle from a new `authorizations()` iterator has never been
/// challenged, so it can be deactivated. The order's authorization
/// states are cached after the first pass, so this re-iterates
/// without extra fetches.
async fn deactivate_authorization(order: &mut Order, index: usize) {
    let mut authorizations = order.authorizations();
    let mut target = None;
    for _ in 0..=index {
        target = authorizations.next().await;
    }
    if let Some(Ok(mut handle)) = target {
        deactivate(&mut handle).await;
    }
}

/// Removes published HTTP-01 tokens from both challenge stores when it
/// drops, so a failed or cancelled order never leaves stale tokens
/// behind for the challenge route to answer.
struct ChallengeTokens {
    instance: Arc<RwLock<HashMap<String, String>>>,
    published: Vec<String>,
}

impl ChallengeTokens {
    fn new(instance: Arc<RwLock<HashMap<String, String>>>) -> Self {
        Self {
            instance,
            published: Vec::new(),
        }
    }
}

impl Drop for ChallengeTokens {
    fn drop(&mut self) {
        if self.published.is_empty() {
            return;
        }
        if let Ok(mut map) = self.instance.write() {
            for token in &self.published {
                map.remove(token);
            }
        }
        if let Ok(mut shared) = challenge_store().write() {
            for token in &self.published {
                shared.remove(token);
            }
        }
    }
}

fn rcgen_err(error: rcgen::Error) -> UnifiedError {
    UnifiedError::internal(format!("certificate error: {error}"))
}

fn load_account_key(path: &Path) -> UnifiedResult<Option<(Key, PrivatePkcs8KeyDer<'static>)>> {
    if !path.exists() {
        return Ok(None);
    }
    let pem = std::fs::read_to_string(path).map_err(|e| {
        UnifiedError::internal(format!("cannot read ACME account key {}: {e}", path.display()))
    })?;
    let key_pair = KeyPair::from_pem(&pem).map_err(|e| {
        UnifiedError::internal(format!("invalid ACME account key {}: {e}", path.display()))
    })?;
    let pkcs8: PrivatePkcs8KeyDer<'static> = key_pair.serialize_der().into();
    let key = Key::from_pkcs8_der(pkcs8.clone_key()).map_err(acme_err)?;
    Ok(Some((key, pkcs8)))
}

fn save_account_key(path: &Path, key_pair: &KeyPair) -> UnifiedResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Create the key file with mode 0600 atomically: the old
    // `fs::write` + `set_permissions(0o600)` left the key briefly
    // world-readable (default umask) between the two calls.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        use std::io::Write;
        file.write_all(key_pair.serialize_pem().as_bytes())?;
        file.sync_all()?;
        // `mode(0o600)` only applies when the file is created; an
        // already-existing key keeps its old permissions, so tighten
        // unconditionally. The file was just created 0600 (or was
        // already there), so no world-readable window opens here.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, key_pair.serialize_pem())?;
    }
    Ok(())
}

/// ACME client bound to one directory (for example Let's Encrypt).
pub struct AcmeClient {
    account: Account,
    directory_url: String,
    /// token -> key authorization, served at /.well-known/acme-challenge/.
    challenges: Arc<RwLock<HashMap<String, String>>>,
}

impl AcmeClient {
    /// Create an account at the Let's Encrypt production directory, or
    /// load the existing one. See `new_with_provider` for other
    /// authorities and `new_with_directory` for the shared logic.
    pub async fn new(directory_url: &str, contacts: &[String]) -> UnifiedResult<Self> {
        Self::new_with_directory(directory_url, contacts, None).await
    }

    /// Create an account with the given provider (staging when
    /// requested), or load the existing one. `eab` is the External
    /// Account Binding `(key id, HMAC key)` required by providers such
    /// as ZeroSSL; it is only used when a new account is created.
    pub async fn new_with_provider(
        provider: &AcmeProvider,
        staging: bool,
        contacts: &[String],
        eab: Option<(&str, &str)>,
    ) -> UnifiedResult<Self> {
        if provider.requires_eab() && eab.is_none() {
            return Err(UnifiedError::bad_request(format!(
                "{} requires External Account Binding (EAB) credentials",
                provider.name()
            )));
        }
        let directory_url = provider.directory_url(staging)?;
        Self::new_with_directory(&directory_url, contacts, eab).await
    }

    /// Create an account at `directory_url`, or load the existing one.
    /// The account key is loaded from `data/acme/account.key` when present,
    /// otherwise a new P-256 key is generated, persisted there, and used to
    /// register the account with the given contacts (`mailto:` is added
    /// when missing).
    pub async fn new_with_directory(
        directory_url: &str,
        contacts: &[String],
        eab: Option<(&str, &str)>,
    ) -> UnifiedResult<Self> {
        let key_path = PathBuf::from(ACCOUNT_KEY_PATH);
        let (key, der, existed) = match load_account_key(&key_path)? {
            Some((key, der)) => (key, der, true),
            None => {
                let key_pair =
                    KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(rcgen_err)?;
                save_account_key(&key_path, &key_pair)?;
                let pkcs8: PrivatePkcs8KeyDer<'static> = key_pair.serialize_der().into();
                let key = Key::from_pkcs8_der(pkcs8.clone_key()).map_err(acme_err)?;
                (key, pkcs8, false)
            }
        };
        let builder = Account::builder().map_err(acme_err)?;
        let (account, _credentials) = if existed {
            builder
                .from_key((key, PrivateKeyDer::Pkcs8(der)), directory_url.to_string())
                .await
                .map_err(acme_err)?
        } else {
            let normalized: Vec<String> = contacts
                .iter()
                .map(|contact| {
                    if contact.starts_with("mailto:") {
                        contact.clone()
                    } else {
                        format!("mailto:{contact}")
                    }
                })
                .collect();
            let contact_refs: Vec<&str> =
                normalized.iter().map(String::as_str).collect();
            let eab_key = eab.map(|(id, hmac)| {
                ExternalAccountKey::new(id.to_string(), hmac.as_bytes())
            });
            builder
                .create(
                    &NewAccount {
                        contact: &contact_refs,
                        terms_of_service_agreed: true,
                        only_return_existing: false,
                    },
                    directory_url.to_string(),
                    eab_key.as_ref(),
                )
                .await
                .map_err(acme_err)?
        };
        Ok(Self {
            account,
            directory_url: directory_url.to_string(),
            challenges: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    pub fn directory_url(&self) -> &str {
        &self.directory_url
    }

    /// Order a certificate for `domains` via HTTP-01. Returns
    /// `(certificate chain PEM, private key PEM)`. Wildcard domains are
    /// rejected: HTTP-01 cannot validate them (DNS-01 is required).
    /// The caller must serve the challenge map on port 80 while this runs.
    pub async fn order_certificate(
        &self,
        domains: &[String],
    ) -> UnifiedResult<(String, String)> {
        if domains.is_empty() {
            return Err(UnifiedError::bad_request(
                "at least one domain is required for a certificate order",
            ));
        }
        let mut normalized = Vec::with_capacity(domains.len());
        for domain in domains {
            let ascii = to_ascii_name(domain);
            if ascii.starts_with("*.") {
                return Err(UnifiedError::bad_request(format!(
                    "wildcard domain {domain} needs DNS-01 validation, HTTP-01 cannot issue it"
                )));
            }
            if rustls_pki_types::DnsName::try_from(ascii.as_str()).is_err() {
                return Err(UnifiedError::bad_request(format!(
                    "invalid domain name: {domain}"
                )));
            }
            normalized.push(ascii);
        }
        // Duplicate identifiers in one order are rejected by the
        // authority, so drop repeats (keeping first-seen order).
        let mut seen = HashSet::new();
        normalized.retain(|domain| seen.insert(domain.clone()));
        let identifiers: Vec<Identifier> = normalized
            .iter()
            .map(|domain| Identifier::Dns(domain.clone()))
            .collect();
        let mut order = self
            .account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(acme_err)?;
        let cert_key = KeyPair::generate().map_err(rcgen_err)?;
        let mut params = CertificateParams::new(normalized.clone()).map_err(rcgen_err)?;
        params.distinguished_name = DistinguishedName::new();
        let csr = params.serialize_request(&cert_key).map_err(rcgen_err)?;

        let mut tokens = ChallengeTokens::new(self.challenges.clone());
        let mut authorization_index = 0usize;
        let mut authorizations = order.authorizations();
        while let Some(authorization) = authorizations.next().await {
            // `AuthorizationHandle::challenge` borrows the handle for
            // the handle's whole lifetime, so the per-authorization
            // work runs in its own scope: the handle is dropped at
            // the end of the scope, and only then can the iterator be
            // dropped and a fresh handle taken from the order to
            // deactivate a failed authorization.
            let failure: Option<UnifiedError> = {
                let mut authorization = authorization.map_err(acme_err)?;
                let mut challenge = authorization
                    .challenge(ChallengeType::Http01)
                    .ok_or_else(|| {
                        UnifiedError::internal(
                            "ACME server offers no HTTP-01 challenge for this domain",
                        )
                    })?;
                let token = challenge.token.clone();
                let key_authorization = challenge.key_authorization();
                self.challenges
                    .write()
                    .expect("challenge map lock poisoned")
                    .insert(token.clone(), key_authorization.as_str().to_string());
                challenge_store()
                    .write()
                    .expect("challenge store lock poisoned")
                    .insert(token.clone(), key_authorization.as_str().to_string());
                tokens.published.push(token);
                // A challenge that is already valid needs no ready signal.
                let mut set_ready_error = None;
                if challenge.status == ChallengeStatus::Pending {
                    if let Err(error) = challenge.set_ready().await {
                        set_ready_error = Some(error);
                    }
                }
                let problem = challenge.error.clone();
                drop(challenge);
                if let Some(error) = set_ready_error {
                    Some(acme_err(error))
                } else if let Some(problem) = problem {
                    Some(UnifiedError::internal(format!(
                        "ACME challenge failed: {problem:?}"
                    )))
                } else {
                    None
                }
            };
            if let Some(error) = failure {
                drop(authorizations);
                deactivate_authorization(&mut order, authorization_index).await;
                return Err(error);
            }
            authorization_index += 1;
        }
        let status = order
            .poll_ready(&RetryPolicy::new())
            .await
            .map_err(acme_err)?;
        // Validation is over: the challenge tokens are removed by the
        // `ChallengeTokens` guard when this function returns, on every
        // path including the errors above.
        if status != OrderStatus::Ready {
            return Err(UnifiedError::internal(format!(
                "ACME order did not become ready (status: {status:?})"
            )));
        }
        order.finalize_csr(csr.der()).await.map_err(acme_err)?;
        let chain = order
            .certificate()
            .await
            .map_err(acme_err)?
            .ok_or_else(|| {
                UnifiedError::internal("ACME server returned no certificate chain")
            })?;
        Ok((chain, cert_key.serialize_pem()))
    }

    /// Renew the stored certificate for `domain` when it expires within
    /// `days`. The window is clamped to `MIN_RENEW_DAYS..=MAX_RENEW_DAYS`.
    /// Returns true when a renewal happened. The `ssl.renew` hook is not
    /// emitted here: this client holds no hook bus, so the caller should
    /// emit it when this returns true.
    pub async fn renew_if_expiring(
        &self,
        storage: &SslStorage,
        domain: &str,
        days: i64,
    ) -> UnifiedResult<bool> {
        let expiry = storage.cert_expiry(domain).ok_or_else(|| {
            UnifiedError::not_found(format!("no certificate stored for {domain}"))
        })?;
        if expiry > Utc::now() + Duration::days(clamp_renew_days(days)) {
            return Ok(false);
        }
        self.renew_stored(storage, domain).await?;
        Ok(true)
    }

    /// Order a replacement for the stored certificate and swap it into
    /// the live storage. The replacement is requested for every
    /// renewable name on the old certificate (see `renewal_domains`),
    /// so a renewal never silently drops a subject alternative name.
    async fn renew_stored(&self, storage: &SslStorage, domain: &str) -> UnifiedResult<()> {
        let domains = renewal_domains(storage, domain);
        if domains.is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "stored certificate for {domain} has no DNS names renewable over HTTP-01 (wildcard names need DNS-01)"
            )));
        }
        let (cert_pem, key_pem) = self.order_certificate(&domains).await?;
        storage.renew(domain, cert_pem.as_bytes(), key_pem.as_bytes())
    }
}

/// Domains to re-request when renewing the stored certificate for
/// `domain`: every DNS name on the stored leaf certificate, so a
/// renewal never silently drops a SAN. Wildcard and IP names are
/// excluded because HTTP-01 cannot validate them; when nothing
/// renewable remains, the requested domain itself is used when it is
/// a plain DNS name.
fn renewal_domains(storage: &SslStorage, domain: &str) -> Vec<String> {
    let mut domains: Vec<String> = storage
        .certificate_names(domain)
        .unwrap_or_default()
        .into_iter()
        .filter(|name| {
            !name.starts_with("*.") && name.parse::<std::net::IpAddr>().is_err()
        })
        .collect();
    if domains.is_empty() {
        let ascii = to_ascii_name(domain);
        if !ascii.starts_with("*.") && ascii.parse::<std::net::IpAddr>().is_err() {
            domains.push(ascii);
        }
    }
    domains
}

// ---------------------------------------------------------------------------
// Advanced certificate management: challenge support matrix, key types,
// and the config-first ACME configuration.
//
// Challenge support, per provider, at the certificate-authority level:
// every built-in provider offers both HTTP-01 and DNS-01, and all of
// them issue wildcard certificates, but only over DNS-01. This client
// performs HTTP-01 only: there is no DNS provider integration in this
// codebase, so a DNS-01 order is rejected with a clear error instead of
// failing halfway through, and wildcard orders are rejected for the
// same reason (see `order_certificate` above). A certificate issued
// with DNS-01 elsewhere can still be installed through the
// certificates module, which validates it before it goes live.
// ---------------------------------------------------------------------------

/// Default expiry-alert thresholds, in days before expiry.
pub const DEFAULT_ALERT_THRESHOLDS: &[i64] = &[30, 14, 7, 1];

/// An ACME challenge type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChallengeKind {
    #[serde(rename = "http-01")]
    Http01,
    #[serde(rename = "dns-01")]
    Dns01,
}

impl ChallengeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http01 => "http-01",
            Self::Dns01 => "dns-01",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "http-01" | "http01" => Some(Self::Http01),
            "dns-01" | "dns01" => Some(Self::Dns01),
            _ => None,
        }
    }
}

/// Challenge types this client actually performs. HTTP-01 only: DNS-01
/// needs a DNS provider integration, which this codebase does not have.
pub const CLIENT_SUPPORTED_CHALLENGES: &[ChallengeKind] = &[ChallengeKind::Http01];

/// Private key algorithm for newly issued certificates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyType {
    Rsa,
    Ecdsa,
}

/// Generate a private key for the requested algorithm. RSA keys are
/// generated at 2048 bits: that is what the bundled generator
/// produces, so `AcmeConfig::validate` rejects any other RSA size
/// instead of silently issuing a different size than configured.
/// ECDSA uses the requested curve (`P-256` or `P-384`).
pub fn generate_key_pair(key_type: KeyType, curve: &str) -> UnifiedResult<KeyPair> {
    match key_type {
        KeyType::Rsa => KeyPair::generate_for(&PKCS_RSA_SHA256).map_err(rcgen_err),
        KeyType::Ecdsa => match curve {
            "P-384" => KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).map_err(rcgen_err),
            "P-256" => KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(rcgen_err),
            other => Err(UnifiedError::bad_request(format!(
                "unsupported ECDSA curve: {other} (expected P-256 or P-384)"
            ))),
        },
    }
}

/// Public description of one ACME provider for the API / dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub key: &'static str,
    pub name: String,
    pub directory_url: String,
    pub staging_url: Option<String>,
    pub staging_available: bool,
    pub requires_eab: bool,
    pub supports_wildcard: bool,
    pub supports_ip: bool,
    pub challenges: Vec<&'static str>,
}

/// Support matrix for every built-in provider, plus the custom slot.
pub fn provider_matrix() -> Vec<ProviderInfo> {
    let mut out = Vec::new();
    for provider in [
        AcmeProvider::LetsEncrypt,
        AcmeProvider::Google,
        AcmeProvider::ZeroSsl,
        AcmeProvider::BuyPass,
    ] {
        let staging_url = provider.directory_url(true).ok();
        out.push(ProviderInfo {
            key: match &provider {
                AcmeProvider::LetsEncrypt => "letsencrypt",
                AcmeProvider::Google => "google",
                AcmeProvider::ZeroSsl => "zerossl",
                AcmeProvider::BuyPass => "buypass",
                AcmeProvider::Custom(_) => "custom",
            },
            name: provider.name().to_string(),
            directory_url: provider
                .directory_url(false)
                .unwrap_or_default(),
            staging_available: staging_url.is_some(),
            staging_url,
            requires_eab: provider.requires_eab(),
            supports_wildcard: provider.supports_wildcard(),
            supports_ip: provider.supports_ip(),
            // What this client can actually perform (HTTP-01 only),
            // not what the CA offers: listing DNS-01 here would
            // over-claim like the old wildcard flag did.
            challenges: CLIENT_SUPPORTED_CHALLENGES
                .iter()
                .map(ChallengeKind::as_str)
                .collect(),
        });
    }
    out.push(ProviderInfo {
        key: "custom",
        name: "custom ACME provider".to_string(),
        directory_url: String::new(),
        staging_url: None,
        staging_available: false,
        requires_eab: false,
        supports_wildcard: false,
        supports_ip: false,
        challenges: vec!["http-01"],
    });
    out
}

/// Config-first ACME configuration.
///
/// This is the configuration the certificates module reads and the
/// shape the `acme` section of the config file should take (see the
/// coordinator report for the exact `AppConfig` snippet: the section
/// lives in `AcmeSection` there, and this struct is the in-memory view
/// built from it). Secrets (`eab_key_id`, `eab_hmac_key`) are only
/// ever held here and passed to account creation; `redacted()` is the
/// only JSON view of this struct, and it never contains them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AcmeConfig {
    /// Provider key: `letsencrypt`, `google`, `zerossl`, `buypass`,
    /// or `custom`.
    pub provider: String,
    /// Explicit directory URL. Empty means the provider default
    /// (or the staging directory when `staging` is true).
    pub directory_url: String,
    /// Use the provider staging directory. Providers without a
    /// staging directory (ZeroSSL, custom) reject staging.
    pub staging: bool,
    /// Contact addresses for the ACME account (`mailto:` is added
    /// when missing).
    pub contacts: Vec<String>,
    /// External Account Binding key id (ZeroSSL). Secret: never
    /// logged, never returned by any API response.
    pub eab_key_id: String,
    /// External Account Binding HMAC key. Secret: never logged,
    /// never returned by any API response.
    pub eab_hmac_key: String,
    /// Renew when the certificate expires within this many days.
    pub renew_days_before_expiry: i64,
    /// Key algorithm for new certificates: `rsa` or `ecdsa`.
    pub key_type: KeyType,
    /// RSA key size in bits. The bundled generator produces
    /// 2048-bit RSA keys, so only 2048 validates.
    pub key_size: u32,
    /// ECDSA curve: `P-256` or `P-384`. Ignored for RSA keys.
    pub curve: String,
    /// Challenge type for issuance: `http-01` or `dns-01`. DNS-01 is
    /// accepted in configuration but ordering with it is rejected
    /// by this client (no DNS provider integration).
    pub challenge: ChallengeKind,
    /// Days before expiry at which expiry alerts fire, for example
    /// `[30, 14, 7, 1]`.
    pub alert_thresholds_days: Vec<i64>,
}

impl Default for AcmeConfig {
    fn default() -> Self {
        Self {
            provider: "letsencrypt".to_string(),
            directory_url: String::new(),
            staging: false,
            contacts: Vec::new(),
            eab_key_id: String::new(),
            eab_hmac_key: String::new(),
            renew_days_before_expiry: DEFAULT_RENEW_DAYS,
            key_type: KeyType::Rsa,
            key_size: 2048,
            curve: "P-256".to_string(),
            challenge: ChallengeKind::Http01,
            alert_thresholds_days: DEFAULT_ALERT_THRESHOLDS.to_vec(),
        }
    }
}

impl AcmeConfig {
    /// Build the view from the existing `acme` config section: the
    /// provider is detected from the directory URL, and staging from
    /// the staging host in the URL. Extended keys (provider name,
    /// EAB, key type, challenge, thresholds) default here until the
    /// config section is extended with them.
    pub fn from_app_config(config: &AppConfig) -> Self {
        let url = config.acme.directory_url.clone();
        let (provider, staging) = if url.contains("letsencrypt") {
            ("letsencrypt", url.contains("staging"))
        } else if url.contains("pki.goog") {
            ("google", url.contains("test-api"))
        } else if url.contains("zerossl") {
            ("zerossl", false)
        } else if url.contains("buypass") {
            ("buypass", url.contains("test.buypass"))
        } else if url.is_empty() {
            ("letsencrypt", false)
        } else {
            ("custom", false)
        };
        Self {
            provider: provider.to_string(),
            directory_url: if provider == "custom" {
                url
            } else {
                String::new()
            },
            staging,
            contacts: config.acme.contacts.clone(),
            ..Self::default()
        }
    }

    /// Resolve the configured provider name to an `AcmeProvider`.
    pub fn resolve_provider(&self) -> UnifiedResult<AcmeProvider> {
        match self.provider.as_str() {
            "letsencrypt" => Ok(AcmeProvider::LetsEncrypt),
            "google" => Ok(AcmeProvider::Google),
            "zerossl" => Ok(AcmeProvider::ZeroSsl),
            "buypass" => Ok(AcmeProvider::BuyPass),
            "custom" => Ok(AcmeProvider::Custom(self.directory_url.clone())),
            other => Err(UnifiedError::bad_request(format!(
                "unknown ACME provider: {other} (expected letsencrypt, google, zerossl, buypass, or custom)"
            ))),
        }
    }

    /// Resolve the directory URL: an explicit URL wins, otherwise the
    /// provider default (or its staging directory).
    pub fn resolve_directory_url(&self) -> UnifiedResult<String> {
        if !self.directory_url.trim().is_empty() {
            return Ok(self.directory_url.clone());
        }
        self.resolve_provider()?
            .directory_url(self.staging)
    }

    /// External Account Binding credentials, when both parts are set.
    /// The returned references borrow this struct; callers must not
    /// copy them into logs or responses.
    pub fn eab(&self) -> Option<(&str, &str)> {
        if self.eab_key_id.is_empty() || self.eab_hmac_key.is_empty() {
            None
        } else {
            Some((&self.eab_key_id, &self.eab_hmac_key))
        }
    }

    /// Validate the configuration. Returns every problem found.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        match self.resolve_provider() {
            Ok(provider) => {
                if provider.requires_eab() && self.eab().is_none() {
                    problems.push(format!(
                        "{} requires External Account Binding (acme.eab_key_id and acme.eab_hmac_key)",
                        provider.name()
                    ));
                }
                if self.staging && provider.directory_url(true).is_err() {
                    problems.push(format!(
                        "{} does not offer a staging directory",
                        provider.name()
                    ));
                }
            }
            Err(error) => problems.push(error.message),
        }
        if !(MIN_RENEW_DAYS..=MAX_RENEW_DAYS).contains(&self.renew_days_before_expiry) {
            problems.push(format!(
                "acme.renew_days_before_expiry must be between {MIN_RENEW_DAYS} and {MAX_RENEW_DAYS}, got {}",
                self.renew_days_before_expiry
            ));
        }
        match self.key_type {
            KeyType::Rsa => {
                if self.key_size != 2048 {
                    problems.push(format!(
                        "acme.key_size must be 2048 for RSA keys (the bundled generator produces 2048-bit RSA keys), got {}",
                        self.key_size
                    ));
                }
            }
            KeyType::Ecdsa => {
                if self.curve != "P-256" && self.curve != "P-384" {
                    problems.push(format!(
                        "acme.curve must be P-256 or P-384 for ECDSA keys, got \"{}\"",
                        self.curve
                    ));
                }
            }
        }
        if self.alert_thresholds_days.is_empty() {
            problems.push("acme.alert_thresholds_days must contain at least one threshold".to_string());
        }
        for threshold in &self.alert_thresholds_days {
            if *threshold < 0 || *threshold > 90 {
                problems.push(format!(
                    "acme.alert_thresholds_days entries must be between 0 and 90, got {threshold}"
                ));
            }
        }
        if self.contacts.iter().any(|contact| contact.trim().is_empty()) {
            problems.push("acme.contacts must not contain empty entries".to_string());
        }
        problems
    }

    /// JSON view safe to return from the API: EAB secrets are never
    /// included, only whether they are configured.
    pub fn redacted(&self) -> Value {
        serde_json::json!({
            "provider": &self.provider,
            "directory_url": self.resolve_directory_url().unwrap_or_default(),
            "staging": self.staging,
            "contacts": &self.contacts,
            "eab_configured": self.eab().is_some(),
            "renew_days_before_expiry": self.renew_days_before_expiry,
            "key_type": self.key_type,
            "key_size": self.key_size,
            "curve": &self.curve,
            "challenge": self.challenge.as_str(),
            "alert_thresholds_days": &self.alert_thresholds_days,
        })
    }
}

impl AcmeClient {
    /// Renew the stored certificate for `domain` when it expires within
    /// `days`, issuing the replacement with the requested key
    /// algorithm. Returns true when a renewal happened.
    pub async fn renew_if_expiring_with_key_type(
        &self,
        storage: &SslStorage,
        domain: &str,
        days: i64,
        key_type: KeyType,
        curve: &str,
    ) -> UnifiedResult<bool> {
        let expiry = storage.cert_expiry(domain).ok_or_else(|| {
            UnifiedError::not_found(format!("no certificate stored for {domain}"))
        })?;
        if expiry > Utc::now() + Duration::days(clamp_renew_days(days)) {
            return Ok(false);
        }
        self.renew_stored_with_key_type(storage, domain, key_type, curve)
            .await?;
        Ok(true)
    }

    /// Renew the stored certificate for `domain` regardless of expiry,
    /// issuing the replacement with the requested key algorithm.
    pub async fn renew_certificate_with_key_type(
        &self,
        storage: &SslStorage,
        domain: &str,
        key_type: KeyType,
        curve: &str,
    ) -> UnifiedResult<()> {
        if storage.cert_expiry(domain).is_none() {
            return Err(UnifiedError::not_found(format!(
                "no certificate stored for {domain}"
            )));
        }
        self.renew_stored_with_key_type(storage, domain, key_type, curve)
            .await
    }

    /// Order a replacement for the stored certificate with the
    /// requested key algorithm and swap it into the live storage. The
    /// replacement is requested for every renewable name on the old
    /// certificate, so a renewal never silently drops a SAN.
    async fn renew_stored_with_key_type(
        &self,
        storage: &SslStorage,
        domain: &str,
        key_type: KeyType,
        curve: &str,
    ) -> UnifiedResult<()> {
        let domains = renewal_domains(storage, domain);
        if domains.is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "stored certificate for {domain} has no DNS names renewable over HTTP-01 (wildcard names need DNS-01)"
            )));
        }
        let (cert_pem, key_pem) = self
            .order_certificate_with_key_type(&domains, key_type, curve)
            .await?;
        storage.renew(domain, cert_pem.as_bytes(), key_pem.as_bytes())
    }

    /// Order a certificate with a caller-chosen key algorithm and
    /// challenge type. DNS-01 is rejected: this client performs
    /// HTTP-01 only (no DNS provider integration), so a DNS-01 order
    /// would fail halfway through. The rest of the flow mirrors
    /// `order_certificate` exactly.
    /// Order a certificate for `domains` via HTTP-01 with a key of
    /// the requested algorithm. Returns `(certificate chain PEM,
    /// private key PEM)`. Wildcard domains are rejected: HTTP-01
    /// cannot validate them (DNS-01 is required).
    pub async fn order_certificate_with_key_type(
        &self,
        domains: &[String],
        key_type: KeyType,
        curve: &str,
    ) -> UnifiedResult<(String, String)> {
        if domains.is_empty() {
            return Err(UnifiedError::bad_request(
                "at least one domain is required for a certificate order",
            ));
        }
        let mut normalized = Vec::with_capacity(domains.len());
        for domain in domains {
            let ascii = to_ascii_name(domain);
            if ascii.starts_with("*.") {
                return Err(UnifiedError::bad_request(format!(
                    "wildcard domain {domain} needs DNS-01 validation, HTTP-01 cannot issue it"
                )));
            }
            if rustls_pki_types::DnsName::try_from(ascii.as_str()).is_err() {
                return Err(UnifiedError::bad_request(format!(
                    "invalid domain name: {domain}"
                )));
            }
            normalized.push(ascii);
        }
        let mut seen = HashSet::new();
        normalized.retain(|domain| seen.insert(domain.clone()));
        let cert_key = generate_key_pair(key_type, curve)?;
        let identifiers: Vec<Identifier> = normalized
            .iter()
            .map(|domain| Identifier::Dns(domain.clone()))
            .collect();
        let mut order = self
            .account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(acme_err)?;
        let mut params = CertificateParams::new(normalized.clone()).map_err(rcgen_err)?;
        params.distinguished_name = DistinguishedName::new();
        let csr = params.serialize_request(&cert_key).map_err(rcgen_err)?;

        let mut tokens = ChallengeTokens::new(self.challenges.clone());
        let mut authorization_index = 0usize;
        let mut authorizations = order.authorizations();
        while let Some(authorization) = authorizations.next().await {
            // `AuthorizationHandle::challenge` borrows the handle for
            // the handle's whole lifetime, so the per-authorization
            // work runs in its own scope: the handle is dropped at
            // the end of the scope, and only then can the iterator be
            // dropped and a fresh handle taken from the order to
            // deactivate a failed authorization.
            let failure: Option<UnifiedError> = {
                let mut authorization = authorization.map_err(acme_err)?;
                let mut challenge = authorization
                    .challenge(ChallengeType::Http01)
                    .ok_or_else(|| {
                        UnifiedError::internal(
                            "ACME server offers no HTTP-01 challenge for this domain",
                        )
                    })?;
                let token = challenge.token.clone();
                let key_authorization = challenge.key_authorization();
                self.challenges
                    .write()
                    .expect("challenge map lock poisoned")
                    .insert(token.clone(), key_authorization.as_str().to_string());
                challenge_store()
                    .write()
                    .expect("challenge store lock poisoned")
                    .insert(token.clone(), key_authorization.as_str().to_string());
                tokens.published.push(token);
                // A challenge that is already valid needs no ready signal.
                let mut set_ready_error = None;
                if challenge.status == ChallengeStatus::Pending {
                    if let Err(error) = challenge.set_ready().await {
                        set_ready_error = Some(error);
                    }
                }
                let problem = challenge.error.clone();
                drop(challenge);
                if let Some(error) = set_ready_error {
                    Some(acme_err(error))
                } else if let Some(problem) = problem {
                    Some(UnifiedError::internal(format!(
                        "ACME challenge failed: {problem:?}"
                    )))
                } else {
                    None
                }
            };
            if let Some(error) = failure {
                drop(authorizations);
                deactivate_authorization(&mut order, authorization_index).await;
                return Err(error);
            }
            authorization_index += 1;
        }
        let status = order
            .poll_ready(&RetryPolicy::new())
            .await
            .map_err(acme_err)?;
        if status != OrderStatus::Ready {
            return Err(UnifiedError::internal(format!(
                "ACME order did not become ready (status: {status:?})"
            )));
        }
        order.finalize_csr(csr.der()).await.map_err(acme_err)?;
        let chain = order
            .certificate()
            .await
            .map_err(acme_err)?
            .ok_or_else(|| {
                UnifiedError::internal("ACME server returned no certificate chain")
            })?;
        Ok((chain, cert_key.serialize_pem()))
    }
}
