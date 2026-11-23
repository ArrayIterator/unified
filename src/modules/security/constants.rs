// security -- Capability names and password-policy constants.

/// Capability that grants read access to the security surface
/// (checks, policy, failed logins, and every hardening status list).
pub const CAP_SECURITY_VIEW: &str = "security:view";

/// Capability that grants write access to the security surface
/// (network scan, settings and header updates, unlock, IP-list
/// changes, reputation flagging).
pub const CAP_SECURITY_MANAGE: &str = "security:manage";

/// Password policy enforced by the application. User creation and password
/// changes must satisfy these rules.
pub const PASSWORD_MIN_LENGTH: usize = 12;

pub const PASSWORD_MAX_LENGTH: usize = 128;

pub const PASSWORD_REQUIRE_UPPERCASE: bool = true;

pub const PASSWORD_REQUIRE_LOWERCASE: bool = true;

pub const PASSWORD_REQUIRE_DIGIT: bool = true;

pub const PASSWORD_REQUIRE_SYMBOL: bool = true;

/// Failed-login count at which an IP is flagged as a rate-limit signal.
pub const FAILED_LOGIN_FLAG_THRESHOLD: u64 = 10;
