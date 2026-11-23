// Auth Manager module.
//
// Authentication providers registry (OAuth server, OAuth login, SSO,
// Google, Facebook, biometric / WebAuthn, and JWT), per-provider
// enable/disable and namespaced settings, and a JWT manager that issues,
// verifies, and revokes HMAC-SHA256 signed JWT-like tokens.
//
// Provider settings are namespaced per provider and merged into the
// Settings module namespace through the `settings.merge` hook, which the
// Settings module can listen to.
//
// Google sign-in performs a real OAuth 2.0 authorization-code exchange
// with PKCE against Google's token endpoint (see the Google connector
// section below). Other providers keep the registry, configuration
// schema, and settings storage only; their outbound exchange is still
// a documented next milestone.

pub mod providers;
mod jwt;
pub mod state;
mod handlers_providers;
pub mod login;
mod tokens;
pub mod recovery;
mod totp;
pub mod google;
pub mod module;
mod reset;

pub use self::providers::{SettingField, ProviderInfo};
pub use self::state::{AuthManagerState};
pub use self::login::{PendingTotp, IssueTokenInput, TokenInput};
pub use self::recovery::{RECOVERY_CODE_COUNT, RecoveryCodeRecord};
pub use self::google::{GoogleTokenRecord, GoogleAccountView, GoogleOAuthState, GoogleDriveLink};
pub use self::module::{AuthManagerModule, register};
