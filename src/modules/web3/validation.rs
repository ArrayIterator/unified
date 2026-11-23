// web3 -- Address, secret, number and endpoint validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::net::IpAddr;

use super::constants::*;

// ---------------------------------------------------------------------------
// Private-key rejection and write-only secret handling
// ---------------------------------------------------------------------------

/// True when `value` looks like a private key. Anything matching is
/// rejected at validation time, before it can be stored anywhere:
/// a 64-hex-character secret with or without a `0x` prefix, a
/// wallet-import-format string, a PEM private-key block, or a value
/// that names itself a private key.
pub(crate) fn looks_like_private_key(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains("PRIVATE KEY") {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("private_key") || lower.contains("privatekey") {
        return true;
    }
    let hex_part = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if hex_part.len() == 64 && hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    // Wallet import format: 51 base58 characters starting with 5, K or L.
    if trimmed.len() == 51
        && (trimmed.starts_with('5') || trimmed.starts_with('K') || trimmed.starts_with('L'))
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() && !"0OIl".contains(c))
    {
        return true;
    }
    false
}

pub(crate) fn reject_private_key_like(value: &str, field: &str) -> UnifiedResult<()> {
    if looks_like_private_key(value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} looks like a private key and is never accepted: this server never stores private keys"
        )));
    }
    Ok(())
}

pub(crate) type HmacSha256 = Hmac<Sha256>;

pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// The encryption secret: `app.secret_key`. There is no fallback:
/// when no real secret is configured, storing a provider key is
/// refused and any call needing a stored key fails closed, instead
/// of encrypting with a hard-coded secret anyone holding the repo
/// could reproduce. Same fail-closed rule as the holidays and ai
/// modules.
pub(crate) fn encryption_secret(ctx: &AppContext) -> UnifiedResult<String> {
    let config = ctx.config.read().expect("config lock poisoned");
    let secret = config.app.secret_key.trim().to_string();
    if secret.is_empty() {
        return Err(UnifiedError::internal(
            "app.secret_key is not configured; provider API keys cannot be stored or decrypted until a real secret is configured",
        ));
    }
    Ok(secret)
}

/// Maximum length, in bytes, of a Stellar text memo (28). A memo
/// longer than this cannot be submitted to the network at all.
pub(crate) const STELLAR_TEXT_MEMO_MAX_LEN: usize = 28;

/// Derive the short memo token a Stellar wallet must put in the
/// text memo of its proof transaction for a challenge nonce.
///
/// A challenge nonce is 64 hex characters, which can never fit in a
/// Stellar text memo (28 bytes max), and a Stellar hash memo is
/// base64, not hex, so comparing the memo against the raw nonce can
/// never succeed. The token is the first 28 hex characters of the
/// SHA-256 of the nonce: 112 bits, deterministic from the nonce so
/// the provider and the handler agree without storing extra state,
/// and short enough for a text memo. The full nonce stays in the
/// challenge message; only this token goes on-chain.
pub(crate) fn stellar_memo_token(nonce: &str) -> String {
    let digest = Sha256::digest(nonce.trim().as_bytes());
    let hex: String = {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    };
    hex[..STELLAR_TEXT_MEMO_MAX_LEN].to_string()
}

pub(crate) fn keystream(key: &[u8], nonce: &[u8], len: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(len);
    let mut counter: u32 = 0;
    while out.len() < len {
        let mut message = nonce.to_vec();
        message.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&hmac_sha256(key, &message));
        counter += 1;
    }
    out.truncate(len);
    out
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Encrypt a write-only secret. Output format: `v1.<base64url(nonce ||
/// ciphertext || tag)>`. The tag covers nonce + ciphertext.
pub(crate) fn encrypt_secret(secret: &str, plaintext: &str) -> String {
    let key = Sha256::digest(secret.as_bytes());
    let nonce: [u8; 16] = rand::random();
    let stream = keystream(&key, &nonce, plaintext.len());
    let ciphertext: Vec<u8> = plaintext
        .as_bytes()
        .iter()
        .zip(stream.iter())
        .map(|(plain, mask)| plain ^ mask)
        .collect();
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(&ciphertext);
    let tag = hmac_sha256(&key, &tag_message);
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    blob.extend_from_slice(&tag);
    format!(
        "v1.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob)
    )
}

/// Decrypt a value produced by `encrypt_secret`. The error message
/// never includes any part of the stored value.
pub(crate) fn decrypt_secret(secret: &str, stored: &str) -> UnifiedResult<String> {
    let encoded = stored
        .strip_prefix("v1.")
        .ok_or_else(|| UnifiedError::internal("Stored web3 secret has an unknown format"))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored web3 secret cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored web3 secret cannot be decrypted",
        ));
    }
    let (nonce, rest) = blob.split_at(16);
    let (ciphertext, tag) = rest.split_at(rest.len() - 32);
    let key = Sha256::digest(secret.as_bytes());
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(ciphertext);
    let expected = hmac_sha256(&key, &tag_message);
    if !constant_time_eq(&expected, tag) {
        return Err(UnifiedError::internal(
            "Stored web3 secret failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored web3 secret cannot be decrypted"))
}

// ---------------------------------------------------------------------------
// Number helpers (string decimals only -- no float loss)
// ---------------------------------------------------------------------------

/// Convert a `0x`-prefixed hex integer to a decimal string, with
/// arbitrary precision (chain values exceed 128 bits).
pub(crate) fn hex_to_decimal(raw: &str) -> Option<String> {
    let hex = raw.trim().strip_prefix("0x").unwrap_or(raw.trim());
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    // Base-1e9 limbs, least significant first.
    let mut limbs: Vec<u32> = vec![0];
    for c in hex.chars() {
        let digit = c.to_digit(16)?;
        let mut carry: u64 = digit as u64;
        for limb in limbs.iter_mut() {
            let value = (*limb as u64) * 16 + carry;
            *limb = (value % 1_000_000_000) as u32;
            carry = value / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push((carry % 1_000_000_000) as u32);
            carry /= 1_000_000_000;
        }
    }
    let mut out = String::new();
    for (index, limb) in limbs.iter().rev().enumerate() {
        if index == 0 {
            out.push_str(&limb.to_string());
        } else {
            out.push_str(&format!("{limb:09}"));
        }
    }
    Some(out)
}

pub(crate) fn parse_hex_u64(raw: &str) -> Option<u64> {
    let hex = raw.trim().strip_prefix("0x").unwrap_or(raw.trim());
    u64::from_str_radix(hex, 16).ok()
}

/// Shift a raw integer decimal string left by `decimals` places,
/// keeping every fractional digit (trailing zeros trimmed).
/// Dust balances must survive: truncating to 4 fractional digits
/// would turn e.g. 5 stroops / 1 wei into "0". Returns `None` when
/// the input is not a plain integer.
pub(crate) fn decimal_shift(raw: &str, decimals: u32) -> Option<String> {
    let digits = raw.trim();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let decimals = decimals as usize;
    if decimals == 0 {
        return Some(digits.to_string());
    }
    let padded;
    let (int_part, frac_part): (&str, &str) = if digits.len() > decimals {
        let split = digits.len() - decimals;
        (&digits[..split], &digits[split..])
    } else {
        padded = format!("{}{digits}", "0".repeat(decimals - digits.len()));
        ("0", &padded)
    };
    let frac: String = frac_part.chars().collect();
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        Some(int_part.to_string())
    } else {
        Some(format!("{int_part}.{frac}"))
    }
}

// ---------------------------------------------------------------------------
// Endpoint allowlist (SSRF safety)
// ---------------------------------------------------------------------------

pub(crate) fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(|addr| addr.is_loopback())
}

pub(crate) fn is_private_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".local") || lower.ends_with(".internal") {
        return true;
    }
    match lower.parse::<IpAddr>() {
        Ok(IpAddr::V4(addr)) => {
            addr.is_loopback()
                || addr.is_private()
                || addr.is_link_local()
                || addr.is_unspecified()
                || unified_core::net::ipv4_is_reserved(&addr)
        }
        Ok(IpAddr::V6(addr)) => {
            addr.is_loopback() || addr.is_unspecified() || addr.is_unique_local()
        }
        Err(_) => false,
    }
}

pub(crate) fn is_allowed_chain_host(host: &str, extra_hosts: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    if KNOWN_CHAIN_HOSTS.contains(&host.as_str()) {
        return true;
    }
    if ALLOWED_CHAIN_HOST_SUFFIXES
        .iter()
        .any(|suffix| host.ends_with(suffix))
    {
        return true;
    }
    extra_hosts
        .iter()
        .any(|allowed| allowed.to_ascii_lowercase() == host)
}

/// Validate a chain provider endpoint URL against the host allowlist.
/// Only `https` is accepted, except plain `http` for loopback hosts.
/// Userinfo and non-loopback IP-literal hosts are rejected. A
/// localhost or private host is rejected unless `allow_private` is
/// true AND the host is in `allowed_private_hosts` (an explicit admin
/// decision, audit-logged by the caller).
pub fn validate_chain_endpoint_url(
    raw: &str,
    extra_hosts: &[String],
    allow_private: bool,
    allowed_private_hosts: &[String],
) -> UnifiedResult<url::Url> {
    let url = url::Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request("Chain endpoint is not a valid URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Chain endpoint must not contain credentials",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("Chain endpoint has no host"))?
        .to_ascii_lowercase();
    let is_ip = host.parse::<IpAddr>().is_ok();
    if is_ip && !is_loopback_host(&host) {
        return Err(UnifiedError::forbidden(
            "Chain endpoints on IP-literal hosts are not allowed",
        ));
    }
    if is_private_host(&host) {
        let explicitly_allowed = allow_private
            && allowed_private_hosts
                .iter()
                .any(|allowed| allowed.to_ascii_lowercase() == host);
        if !explicitly_allowed {
            return Err(UnifiedError::forbidden(
                "Chain endpoints on localhost or private hosts require an explicit admin allowlist entry",
            ));
        }
        return match url.scheme() {
            "https" => Ok(url),
            "http" if is_loopback_host(&host) => Ok(url),
            _ => Err(UnifiedError::bad_request(
                "Chain endpoint must use https (http is allowed only for loopback hosts)",
            )),
        };
    }
    if !is_allowed_chain_host(&host, extra_hosts) {
        return Err(UnifiedError::forbidden(format!(
            "Chain endpoint host \"{host}\" is not allowlisted"
        )));
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_loopback_host(&host) => Ok(url),
        _ => Err(UnifiedError::bad_request(
            "Chain endpoint must use https (http is allowed only for loopback hosts)",
        )),
    }
}

/// Validate an explorer URL template. It is display-only (never
/// fetched), so it needs a valid `http(s)` URL with a `{tx_hash}` or
/// `{address}` placeholder and no embedded credentials.
pub(crate) fn validate_explorer_template(raw: &str) -> UnifiedResult<String> {
    let template = raw.trim();
    if !template.contains("{tx_hash}") && !template.contains("{address}") {
        return Err(UnifiedError::bad_request(
            "Explorer URL template must contain a {tx_hash} or {address} placeholder",
        ));
    }
    let probe = template.replace("{tx_hash}", "x").replace("{address}", "x");
    let url = url::Url::parse(&probe)
        .map_err(|_| UnifiedError::bad_request("Explorer URL template is not a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UnifiedError::bad_request(
            "Explorer URL template must use http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Explorer URL template must not contain credentials",
        ));
    }
    Ok(template.to_string())
}

// Tests for the pure validation helpers in this file.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_symbol_is_uppercased_and_validated() {
        assert_eq!(validate_asset_symbol("eth").unwrap(), "ETH");
        assert_eq!(validate_asset_symbol("USDC").unwrap(), "USDC");
        assert!(validate_asset_symbol("TOOLONGSYMBOL1234567").is_err());
        assert!(validate_asset_symbol("E T H").is_err());
    }

    #[test]
    fn slug_requires_lowercase_path_characters() {
        assert!(require_slug("my-network").is_ok());
        assert!(require_slug("a_b-c.d").is_ok());
        assert!(require_slug("My Network").is_err());
        assert!(require_slug("").is_err());
    }

    #[test]
    fn sha256_hex_accepts_uppercase_and_lowercases() {
        let upper = "AB".repeat(32);
        assert_eq!(validate_sha256_hex(&upper).unwrap(), upper.to_lowercase());
        assert!(validate_sha256_hex(&"ab".repeat(31)).is_err());
        assert!(validate_sha256_hex(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn hex_to_decimal_converts_prefixed_and_bare_values() {
        assert_eq!(hex_to_decimal("0xff"), Some("255".to_string()));
        assert_eq!(hex_to_decimal("0x0"), Some("0".to_string()));
        assert_eq!(hex_to_decimal("ff"), Some("255".to_string()));
        assert_eq!(hex_to_decimal("xyz"), None);
        assert_eq!(hex_to_decimal("0x"), None);
    }

    #[test]
    fn parse_hex_u64_handles_prefix_and_invalid_input() {
        assert_eq!(parse_hex_u64("0x10"), Some(16));
        assert_eq!(parse_hex_u64("10"), Some(16));
        assert_eq!(parse_hex_u64(""), None);
        assert_eq!(parse_hex_u64("0xg"), None);
    }

    #[test]
    fn decimal_shift_places_the_decimal_point() {
        assert_eq!(decimal_shift("1000000", 6), Some("1".to_string()));
        assert_eq!(decimal_shift("1500000", 6), Some("1.5".to_string()));
        assert_eq!(decimal_shift("1", 6), Some("0.000001".to_string()));
        assert_eq!(decimal_shift("123456", 0), Some("123456".to_string()));
        assert_eq!(decimal_shift("abc", 6), None);
    }

    #[test]
    fn decimal_shift_keeps_every_fractional_digit() {
        // Dust balances must survive: no truncation to 4 digits.
        assert_eq!(decimal_shift("5", 6), Some("0.000005".to_string()));
        assert_eq!(
            decimal_shift("123456789", 6),
            Some("123.456789".to_string())
        );
        assert_eq!(
            decimal_shift("1", 18),
            Some("0.000000000000000001".to_string())
        );
        assert_eq!(decimal_shift("1000001", 6), Some("1.000001".to_string()));
    }

    #[test]
    fn stellar_memo_token_fits_a_text_memo() {
        let nonce = "a".repeat(64);
        let token = stellar_memo_token(&nonce);
        assert_eq!(token.len(), STELLAR_TEXT_MEMO_MAX_LEN);
        assert!(token.len() <= 28);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        // Deterministic from the nonce, and distinct per nonce.
        assert_eq!(stellar_memo_token(&nonce), token);
        assert_ne!(stellar_memo_token(&"b".repeat(64)), token);
        // The token is the first 28 hex chars of SHA-256(nonce).
        let digest = Sha256::digest(nonce.as_bytes());
        let hex: String = {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut out = String::with_capacity(digest.len() * 2);
            for &byte in digest.iter() {
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
            out
        };
        assert_eq!(token, hex[..28]);
    }

    #[test]
    fn encryption_secret_refuses_an_unconfigured_secret() {
        use unified_core::config::AppConfig;
        use unified_core::context::AppContext;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        use std::sync::Arc;
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        // AppConfig::default() ships with an empty secret_key: no
        // hard-coded fallback may stand in for it.
        assert!(encryption_secret(&ctx).is_err());
        ctx.config
            .write()
            .expect("config lock poisoned")
            .app
            .secret_key = "a-real-configured-secret".to_string();
        assert_eq!(
            encryption_secret(&ctx).unwrap(),
            "a-real-configured-secret"
        );
    }

    #[test]
    fn allowed_chain_host_matches_known_hosts_suffixes_and_extras() {
        assert!(is_allowed_chain_host("horizon.stellar.org", &[]));
        assert!(is_allowed_chain_host("HORIZON.STELLAR.ORG", &[]));
        assert!(is_allowed_chain_host("eth-mainnet.g.alchemy.com", &[]));
        assert!(!is_allowed_chain_host("evil.com", &[]));
        assert!(is_allowed_chain_host(
            "my-rpc.internal",
            &["my-rpc.internal".to_string()]
        ));
        // Extra hosts match exactly only; they do not act as suffixes.
        assert!(!is_allowed_chain_host(
            "sub.my-rpc.internal",
            &["my-rpc.internal".to_string()]
        ));
    }

    #[test]
    fn explorer_template_requires_placeholder_and_safe_url() {
        assert!(validate_explorer_template("https://explorer.example.com/tx/{tx_hash}").is_ok());
        assert!(
            validate_explorer_template("https://explorer.example.com/address/{address}").is_ok()
        );
        assert!(validate_explorer_template("https://explorer.example.com/").is_err());
        assert!(validate_explorer_template("ftp://explorer.example.com/tx/{tx_hash}").is_err());
        assert!(
            validate_explorer_template("https://user:pass@explorer.example.com/tx/{tx_hash}")
                .is_err()
        );
    }
    #[test]
    fn stellar_memo_token_matches_known_sha256_prefix() {
        // SHA-256("abc") starts with ba7816bf8f01cfea414140de5dae2.
        assert_eq!(
            stellar_memo_token("abc"),
            "ba7816bf8f01cfea414140de5dae2"
        );
    }

}
