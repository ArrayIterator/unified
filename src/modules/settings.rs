// Settings module.
//
// Namespaced key-value settings under `/api/v1/settings`, stored in
// memory with typed values (`string`, `number`, `bool`, `json`). Values
// are validated through the hook bus before they are stored: the module
// emits a `config.validate` event, and any handler may mark the value
// invalid (by setting `valid: false` on the payload or contributing a
// result with `valid: false`); the write is then refused with 422.
// After a successful write or delete the module emits a
// `settings.changed` event (`{ namespace, key, type?, value?,
// deleted }`) so owning modules can keep their effective-value
// mirrors in sync; the delete event carries the removed record's
// type and value. All four routes require an authenticated caller
// (`X-User-Id` + `X-Role` headers, capability checked through the
// `role.check` hook, fail-closed): `settings.view` for reads,
// `settings.manage` for writes. No namespace is public.
// The module also listens for `settings.merge` from other modules
// and persists their flags under the `vcs.*`, `auth.*`, and
// `cloudflare` namespaces. Task 351 adds data-driven definitions
// for the `site` namespace (`definitions.rs`: WordPress General
// Settings parity — title, tagline, URLs, admin email, membership,
// default role, language, full IANA timezone, date/time formats,
// week start), discoverable at `GET /api/v1/settings/definitions`
// and enforced on write through the `config.validate` hook.

pub mod types;
pub mod definitions;
mod handlers;
pub mod module;

pub use self::types::{SettingsStore, SettingRecord, PutSettingInput};
pub use self::definitions::{SiteSettingDef, SiteSettingOption, site_definitions, site_definition, validate_site_value, site_definitions_json, IANA_TIMEZONES};
pub use self::module::{SettingsModule, register};
