// Licensing module -- license key management and validation.
//
// License keys use the format `XXXX-XXXX-XXXX-XXXX` over a 32-character
// alphabet (no ambiguous characters), with the final character acting as
// a checksum over the preceding characters. Validation is real: the key
// must exist, be `active`, not be expired, and have a free activation
// seat; a successful validation auto-registers the activation.
// Revocation emits the `license.revoked` hook event. The expiry scan
// (`check_license_expiry`, below) emits `license.expiring` for
// licenses entering a warning window; it runs on the
// `license.expiry.check_requested` hook and on
// `POST /api/v1/licensing/licenses/expiry-check`.
//
// Licensing as a service (batch 7): a Unified user can sell their own
// licensed products. Per-product settings (`LicenseProduct`) control
// the key format, seat defaults, validity, grace period, renewal
// reminders, offline-token allowance, and distribution channels.
// The public verification API (`POST /api/v1/licensing/public/verify`)
// is authenticated with an API key scoped to the product owner (see
// `crate::modules::apikeys`), never with the license key itself.
// Releases can be distributed through a git channel (signed webhook),
// the verification API itself, or file uploads stored through the
// shared storage abstraction (`StorageRegistry`) with a signed,
// expiring download link that requires a valid license. Every license
// mutation appends a hash-chained entry to the license event log
// (`license_events`), and each append emits `license.anchored` so an
// optional anchoring consumer (for example the Web3 module, which
// listens for `web3.anchor.requested`) can anchor the hash on-chain;
// licensing never calls the Web3 module directly.
//
// Trust model (read before relying on offline verification):
// - An offline token is a bearer credential: it can be replayed by
//   anyone who holds it until it expires, it is valid for exactly one
//   product, and revocation of the license or the token does NOT
//   propagate to verifications that already hold the token. The token
//   is HMAC-SHA256 signed with the server secret
//   (`store.download_signing_secret`, or a random per-process secret
//   when that is unset, in which case tokens stop verifying after a
//   restart). Offline verification performs no database lookup.
// - A git webhook secret is stored raw in memory only (it is needed
//   to verify the provider HMAC); it is never returned by any
//   endpoint and only its SHA-256 hash is kept for audit. After a
//   restart a distribution must be re-configured unless the
//   integrator adds encrypted persistence for the secret.
// - The new SaaS tables live in memory in this round (the same
//   pattern as the rest of this module); the migration ships the
//   PostgreSQL tables and the integrator wires them into
//   crates/unified-core/src/db/schema.rs` / crates/unified-core/src/db/models.rs` for durable persistence.
//
// SDK: FUTURE ONLY. No SDK ships in this round. The planned SDK wraps
// the verification API (`POST /api/v1/licensing/public/verify`) and the
// offline-token flow (`offline-token` issue + `offline-verify`), with
// language targets PHP, JavaScript/TypeScript, Python, and Rust.

pub mod ai_capabilities;
pub mod keys;
pub mod expiry;
mod store;
mod hooks;
mod types;
pub mod validation;
mod handlers;
pub mod products;
pub mod helpers;
mod product_handlers;
mod verify;
mod offline;
mod distribution;
mod releases;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::keys::{generate_license_key, key_checksum_valid};
pub use self::expiry::{LICENSE_EXPIRING, LICENSE_EXPIRY_CHECK_REQUESTED, EXPIRY_ALERT_WINDOW_DAYS, check_license_expiry};
pub use self::validation::{has_module_entitlement, has_subscription_entitlement,
    subscription_entitlement_id, check_module_entitlement};
pub use self::products::{LICENSE_VERIFY_FAILED, LICENSE_RENEWAL_REMINDER, LICENSE_GRACE_STARTED, LICENSE_UPGRADED, LICENSE_ANCHORED, LICENSE_DOWNLOAD, LICENSE_SEATS_CHANGED, LICENSE_DISTRIBUTION_RELEASE, STORE_ORDER_UPGRADED, BILLING_PLAN_CHANGED, DEFAULT_RENEWAL_REMINDER_DAYS, DEFAULT_OFFLINE_MAX_DAYS, MAX_OFFLINE_TOKEN_DAYS, MAX_RELEASE_UPLOAD_BYTES, SIGNED_DOWNLOAD_TTL_SECS, DEFAULT_RELEASE_TARGET, KEY_FORMAT_STANDARD, KEY_FORMAT_CUSTOM, DISTRIBUTION_CHANNEL_GIT, DISTRIBUTION_CHANNEL_API, DISTRIBUTION_CHANNEL_FILE, LicenseProduct, LicenseDistribution, LicenseRelease, LicenseEvent, LicenseOfflineToken, LicenseActivationHistory};
pub use self::helpers::{generate_product_key};
pub use self::module::{LicensingModule, register};
