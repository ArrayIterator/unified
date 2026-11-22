// Argon2 password hashing.
//
// Passwords are hashed with Argon2id (OWASP recommended parameters:
// 19 MiB memory, 2 iterations, 1 lane, 32 byte output) and stored as
// PHC strings, so the parameters travel with the hash and old hashes can
// be detected for rehashing. Passwords are never logged.

use crate::core::error::{UnifiedError, UnifiedResult};
use argon2::password_hash::{ParamsString, PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng};
use argon2::{Algorithm, Argon2, Params, Version};

/// Default Argon2 memory cost in KiB (19 MiB, OWASP recommendation).
pub const DEFAULT_M_COST: u32 = 19_456;
/// Default Argon2 time cost (iterations).
pub const DEFAULT_T_COST: u32 = 2;
/// Default Argon2 parallelism (lanes).
pub const DEFAULT_P_COST: u32 = 1;
/// Default Argon2 output length in bytes.
pub const DEFAULT_OUTPUT_LEN: usize = 32;

/// Hash a password with the default Argon2id parameters.
///
/// Returns the PHC-encoded hash string (for example
/// `$argon2id$v=19$m=19456,t=2,p=1$...$...`).
pub fn hash(password: &str) -> UnifiedResult<String> {
    hash_with_params(password, DEFAULT_M_COST, DEFAULT_T_COST, DEFAULT_P_COST)
}

/// Hash a password with explicit Argon2id parameters.
///
/// `m_cost` is the memory cost in KiB, `t_cost` the iteration count, and
/// `p_cost` the degree of parallelism. The recommended values are
/// m=19456, t=2, p=1.
pub fn hash_with_params(
    password: &str,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
) -> UnifiedResult<String> {
    if password.is_empty() {
        return Err(UnifiedError::bad_request("Password must not be empty"));
    }
    let params = Params::new(m_cost, t_cost, p_cost, DEFAULT_OUTPUT_LEN).map_err(|error| {
        UnifiedError::internal(format!("Invalid Argon2 parameters: {error}"))
    })?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let salt = SaltString::generate(&mut OsRng);
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|error| UnifiedError::internal(format!("Password hashing failed: {error}")))?;
    Ok(hash.to_string())
}

/// Verify a password against a stored PHC hash.
///
/// Returns `false` for any malformed hash or mismatching password. The
/// check runs in constant time relative to the hash comparison performed
/// by the Argon2 verifier.
pub fn verify(password: &str, hash: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(parsed) => parsed,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// True when a stored hash was produced with parameters or an algorithm
/// version different from the current defaults, meaning the password
/// should be rehashed on the next successful login.
///
/// Unparseable hashes also return `true` so they are never silently
/// treated as current.
pub fn needs_rehash(hash: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(parsed) => parsed,
        Err(_) => return true,
    };
    if parsed.algorithm.as_str() != "argon2id" {
        return true;
    }
    if parsed.version != Some(0x13) {
        return true;
    }
    let m_cost = decimal_param(&parsed.params, "m").unwrap_or(0);
    let t_cost = decimal_param(&parsed.params, "t").unwrap_or(0);
    let p_cost = decimal_param(&parsed.params, "p").unwrap_or(0);
    m_cost != DEFAULT_M_COST || t_cost != DEFAULT_T_COST || p_cost != DEFAULT_P_COST
}

/// Read one decimal parameter from a PHC parameter string.
fn decimal_param(params: &ParamsString, name: &str) -> Option<u32> {
    params.get_decimal(name).and_then(|result| result.ok())
}
