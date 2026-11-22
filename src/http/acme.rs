// Let's Encrypt ACME client (HTTP-01) built on instant-acme.
//
// The account key is a P-256 ECDSA key persisted at `data/acme/account.key`
// (instant-acme only supports P-256 account keys). Challenge responses are
// published through a shared token map; the HTTP server must serve
// `/.well-known/acme-challenge/<token>` from it on port 80.

use crate::core::error::{UnifiedError, UnifiedResult};
use crate::http::tls::{to_ascii_name, SslStorage};
use chrono::{Duration, Utc};
use futures_util::StreamExt;
use instant_acme::{
    Account, ChallengeType, Identifier, Key, NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use rcgen::{CertificateParams, DistinguishedName, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

/// Let's Encrypt production directory.
pub const LETS_ENCRYPT_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";
/// Let's Encrypt staging directory (use for testing).
pub const LETS_ENCRYPT_STAGING_URL: &str =
    "https://acme-staging-v02.api.letsencrypt.org/directory";
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
    /// Create an account at `directory_url`, or load the existing one.
    /// The account key is loaded from `data/acme/account.key` when present,
    /// otherwise a new P-256 key is generated, persisted there, and used to
    /// register the account with the given contacts (`mailto:` is added
    /// when missing).
    pub async fn new(directory_url: &str, contacts: &[String]) -> UnifiedResult<Self> {
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
            builder
                .create(
                    &NewAccount {
                        contact: &contact_refs,
                        terms_of_service_agreed: true,
                        only_return_existing: false,
                    },
                    directory_url.to_string(),
                    None,
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

        let mut tokens = Vec::new();
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
            tokens.push(token);
            challenge.set_ready().await.map_err(acme_err)?;
        }
        let status = order
            .poll_ready(&RetryPolicy::new())
            .await
            .map_err(acme_err)?;
        // Validation is over: the challenge responses are no longer needed.
        {
            let mut challenges = self
                .challenges
                .write()
                .expect("challenge map lock poisoned");
            for token in &tokens {
                challenges.remove(token);
            }
        }
        {
            let mut shared = challenge_store()
                .write()
                .expect("challenge store lock poisoned");
            for token in &tokens {
                shared.remove(token);
            }
        }
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
    /// `days`. Returns true when a renewal happened. The `ssl.renew`
    /// hook is not emitted here: this client holds no hook bus, so the
    /// caller should emit it when this returns true.
    pub async fn renew_if_expiring(
        &self,
        storage: &SslStorage,
        domain: &str,
        days: i64,
    ) -> UnifiedResult<bool> {
        let expiry = storage.cert_expiry(domain).ok_or_else(|| {
            UnifiedError::not_found(format!("no certificate stored for {domain}"))
        })?;
        if expiry > Utc::now() + Duration::days(days) {
            return Ok(false);
        }
        let (cert_pem, key_pem) = self.order_certificate(&[domain.to_string()]).await?;
        storage.renew(domain, cert_pem.as_bytes(), key_pem.as_bytes())?;
        Ok(true)
    }
}
