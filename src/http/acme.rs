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

use crate::core::error::{UnifiedError, UnifiedResult};
use crate::http::tls::{to_ascii_name, SslStorage};
use chrono::{Duration, Utc};
use futures_util::StreamExt;
use instant_acme::{
    Account, AuthorizationHandle, AuthorizationStatus, ChallengeStatus, ChallengeType,
    ExternalAccountKey, Identifier, Key, NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use rcgen::{CertificateParams, DistinguishedName, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
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

    /// True when the provider can issue wildcard certificates. All
    /// listed providers support wildcards, but only over DNS-01, so
    /// this client (HTTP-01 only) still rejects wildcard orders.
    pub fn supports_wildcard(&self) -> bool {
        !matches!(self, Self::BuyPass)
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
async fn deactivate(authorization: &mut AuthorizationHandle) {
    if matches!(
        authorization.status,
        AuthorizationStatus::Pending | AuthorizationStatus::Valid
    ) {
        authorization.deactivate().await.ok();
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
    std::fs::write(path, key_pair.serialize_pem())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
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

    /// Shared HTTP-01 challenge map. The HTTP server serves
    /// `/.well-known/acme-challenge/<token>` from this map.
    pub fn challenges(&self) -> Arc<RwLock<HashMap<String, String>>> {
        self.challenges.clone()
    }

    /// Key authorization for one challenge token, if currently pending.
    pub fn challenge_response(&self, token: &str) -> Option<String> {
        self.challenges
            .read()
            .ok()?
            .get(token)
            .cloned()
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
        let mut authorizations = order.authorizations();
        while let Some(authorization) = authorizations.next().await {
            let mut authorization = authorization.map_err(acme_err)?;
            let mut challenge = authorization
                .challenge(ChallengeType::Http01)
                .ok_or_else(|| {
                    UnifiedError::internal(
                        "ACME server offers no HTTP-01 challenge for this domain",
                    )
                })?;
            let token = challenge.token.clone();
            let key_authorization = challenge
                .key_authorization(self.account.credentials().key())
                .map_err(acme_err)?;
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
            if challenge.status == ChallengeStatus::Pending {
                if let Err(error) = challenge.set_ready().await {
                    deactivate(&mut authorization).await;
                    return Err(acme_err(error));
                }
            }
            if let Some(problem) = challenge.error.clone() {
                deactivate(&mut authorization).await;
                return Err(UnifiedError::internal(format!(
                    "ACME challenge failed: {problem:?}"
                )));
            }
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

    /// Renew the stored certificate for `domain` regardless of expiry.
    /// Useful for a forced renewal (for example after a key compromise
    /// or an authority-mandated reissue).
    pub async fn renew_certificate(
        &self,
        storage: &SslStorage,
        domain: &str,
    ) -> UnifiedResult<()> {
        if storage.cert_expiry(domain).is_none() {
            return Err(UnifiedError::not_found(format!(
                "no certificate stored for {domain}"
            )));
        }
        self.renew_stored(storage, domain).await
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
