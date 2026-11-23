// security -- Password policy, check and scan types.

use hmac::Hmac;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use super::constants::*;
use super::handlers::*;

// Security module.
//
// Runs real configuration security checks, exposes the password policy,
// performs a basic TLS-presence scan of a target host, and listens to the
// `auth.failed` hook so repeated failed logins per IP are visible as a
// rate-limit signal.

pub(crate) type HmacSha256 = Hmac<Sha256>;

/// The password policy, as returned by `GET /api/v1/security/policy`.
#[derive(Debug, Clone, Serialize)]
pub struct PasswordPolicy {
    pub min_length: usize,
    pub max_length: usize,
    pub require_uppercase: bool,
    pub require_lowercase: bool,
    pub require_digit: bool,
    pub require_symbol: bool,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            min_length: PASSWORD_MIN_LENGTH,
            max_length: PASSWORD_MAX_LENGTH,
            require_uppercase: PASSWORD_REQUIRE_UPPERCASE,
            require_lowercase: PASSWORD_REQUIRE_LOWERCASE,
            require_digit: PASSWORD_REQUIRE_DIGIT,
            require_symbol: PASSWORD_REQUIRE_SYMBOL,
        }
    }
}

impl PasswordPolicy {
    /// Validate a candidate password against the policy. Returns a list of
    /// problems; an empty list means the password is acceptable.
    pub fn validate(&self, password: &str) -> Vec<String> {
        let mut problems = Vec::new();
        if password.chars().count() < self.min_length {
            problems.push(format!(
                "Password must be at least {} characters long",
                self.min_length
            ));
        }
        if password.chars().count() > self.max_length {
            problems.push(format!(
                "Password must be at most {} characters long",
                self.max_length
            ));
        }
        if self.require_uppercase && !password.chars().any(|c| c.is_uppercase()) {
            problems.push("Password must contain an uppercase letter".to_string());
        }
        if self.require_lowercase && !password.chars().any(|c| c.is_lowercase()) {
            problems.push("Password must contain a lowercase letter".to_string());
        }
        if self.require_digit && !password.chars().any(|c| c.is_ascii_digit()) {
            problems.push("Password must contain a digit".to_string());
        }
        if self.require_symbol && !password.chars().any(|c| !c.is_alphanumeric()) {
            problems.push("Password must contain a symbol".to_string());
        }
        problems
    }
}

/// One security check result: `pass`, `warn`, or `fail`.
#[derive(Debug, Clone, Serialize)]
pub struct SecurityCheck {
    pub id: String,
    pub name: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailedLoginEntry {
    pub ip: String,
    pub attempts: u64,
    pub flagged: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChecksResponse {
    pub overall: String,
    pub checks: Vec<SecurityCheck>,
    pub failed_logins: Vec<FailedLoginEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScanRequest {
    pub target: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanFinding {
    pub severity: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanResponse {
    pub target: String,
    pub host: String,
    pub findings: Vec<ScanFinding>,
    pub scanned_at: chrono::DateTime<chrono::Utc>,
}

// Tests for the password policy in this file.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_rejects_short_passwords() {
        let policy = PasswordPolicy::default();
        assert!(!policy.validate("Short1!").is_empty());
    }

    #[test]
    fn default_policy_requires_uppercase() {
        let policy = PasswordPolicy::default();
        let problems = policy.validate("alllowercase1!");
        assert!(problems.iter().any(|p| p.contains("uppercase")));
    }

    #[test]
    fn default_policy_requires_digit() {
        let policy = PasswordPolicy::default();
        let problems = policy.validate("NoDigitHere!x");
        assert!(problems.iter().any(|p| p.contains("digit")));
    }

    #[test]
    fn default_policy_requires_symbol() {
        let policy = PasswordPolicy::default();
        let problems = policy.validate("NoSymbol1234Ab");
        assert!(problems.iter().any(|p| p.contains("symbol")));
    }

    #[test]
    fn default_policy_accepts_strong_password() {
        let policy = PasswordPolicy::default();
        assert!(policy.validate("ValidPass1!xy").is_empty());
    }

    #[test]
    fn custom_policy_uses_its_own_minimum() {
        let policy = PasswordPolicy {
            min_length: 8,
            max_length: 128,
            require_uppercase: false,
            require_lowercase: false,
            require_digit: false,
            require_symbol: false,
        };
        assert!(!policy.validate("short12").is_empty());
        assert!(policy.validate("short123").is_empty());
    }
}
