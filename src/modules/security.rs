// Security module: configuration security checks, password policy,
// TLS-presence scan, failed-login visibility and the hardening HTTP
// surface. Split into one file per concern; this file only declares
// the submodules and re-exports the public API.

pub mod ai_capabilities;
pub mod auth;
pub mod constants;
pub mod handlers;
pub mod handlers_hardening;
pub mod helpers;
pub mod module;
pub mod scan;
pub mod types;

// Re-exported from constants.rs.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::CAP_SECURITY_MANAGE;
pub use self::constants::CAP_SECURITY_VIEW;
pub use self::constants::FAILED_LOGIN_FLAG_THRESHOLD;
pub use self::constants::PASSWORD_MAX_LENGTH;
pub use self::constants::PASSWORD_MIN_LENGTH;
pub use self::constants::PASSWORD_REQUIRE_DIGIT;
pub use self::constants::PASSWORD_REQUIRE_LOWERCASE;
pub use self::constants::PASSWORD_REQUIRE_SYMBOL;
pub use self::constants::PASSWORD_REQUIRE_UPPERCASE;
// Re-exported from types.rs.
pub use self::types::ChecksResponse;
pub use self::types::FailedLoginEntry;
pub use self::types::PasswordPolicy;
pub use self::types::ScanFinding;
pub use self::types::ScanRequest;
pub use self::types::ScanResponse;
pub use self::types::SecurityCheck;
// Re-exported from module.rs.
pub use self::module::SecurityModule;
pub use self::module::register;
// Re-exported from handlers_hardening.rs.
pub use self::handlers_hardening::IpListAddRequest;
pub use self::handlers_hardening::IpListRemoveQuery;
pub use self::handlers_hardening::ReputationFlagRequest;
pub use self::handlers_hardening::ReputationUnflagRequest;
pub use self::handlers_hardening::UnlockRequest;
