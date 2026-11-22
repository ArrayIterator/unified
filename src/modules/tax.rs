// Tax Management module.
//
// Tax rules per jurisdiction (country / region code, rate, thresholds,
// effective dates, inclusive vs exclusive, compound flag), per-company
// settings (rates, thresholds, fiscal year start, filing frequency),
// pure tax computation, tax reports, filing reminders, and a generic
// HTTP connector framework for tax reporting / filing providers.
//
// Company scoping: every record belongs to one company (`company_id`).
// Callers identify themselves with the `X-User-Id` / `X-Role` headers
// and the company they act for with `X-Company-Id` (falling back to
// the `user.company_requested` hook, the same shape the AI module
// uses). A caller whose company differs from the target company's
// records is refused with 403. Capability checks run on every route
// through the `role.check` hook and fail closed: `tax:view` for reads,
// `tax:manage` for rule / settings / filing writes, `tax:file` for
// filing submission, `tax:manage_connectors` for connector writes.
//
// Interconnection happens only through the hook bus — this module
// never calls another module directly.
//
// Events emitted:
//   - `tax.rule_created`, `tax.rule_updated`, `tax.rule_deleted`
//   - `tax.computed` — after a tax computation, with the source id
//     and the line-by-line breakdown. The source document (invoice
//     or order) is never mutated.
//   - `tax.settings_updated`
//   - `tax.filing_created`, `tax.filing_due`, `tax.filing_submitted`
//   - `tax.connector_created`, `tax.connector_updated`,
//     `tax.connector_deleted`, `tax.connector_tested`
//   - Audit-shaped events: every mutation also emits its `tax.*`
//     event with `actor_id`, `resource`, and `id` in the payload, in
//     the same shape the AI module's `emit_audit` uses. The audit
//     module only records events listed in its `AUDITED_EVENTS`, so
//     the integrator must add the `tax.*` names there (see
//     docs/integration/INTEGRATION_NOTES_BATCH3_B3.md). Every change is also recorded
//     in this module's own `tax_audit` table, which is always
//     available regardless of the audit module's list.
//
// Events consumed:
//   - `invoice.created` — the real event the billing module emits
//     when an invoice is created. The brief also names
//     `billing.invoice_created`; that name does not exist in the
//     billing module, so this module subscribes to both spellings
//     and de-duplicates by source id, so a computation is stored
//     exactly once no matter which spelling arrives.
//   - `order.created` — the real event the store / billing flow
//     emits. `billing.order_created` is subscribed as an alias in
//     the same way.
//   - `tax.filing.check_requested` — the filing-reminder check. The
//     scheduler module only dispatches its own fixed job kinds, so
//     there is no scheduler kind to register for this check; the
//     check runs through this event and through
//     `POST /api/v1/tax/filings/check-due`.
//
// Credentials are write-only, following the AI module's documented
// pattern: a connector credential (API key or signing secret) is
// accepted once, encrypted with a key derived from
// `app.secret_key`, and stored only as ciphertext. The encryption is
// a versioned (`v1.`) HMAC keystream with an HMAC tag — not an AEAD
// cipher — because no AEAD crate is in `Cargo.toml` yet; when one is
// added, replace `encrypt_tax_credential` / `decrypt_tax_credential`
// and bump the version prefix so old values can be re-encrypted on
// next write. No response, log line, hook payload, or audit record
// ever contains a credential or a fragment of one; read views expose
// only `credential_configured` and a short fingerprint prefix.
//
// Connector SSRF protection: an endpoint URL must use HTTPS, must
// carry no userinfo, and its host must be listed in the connector's
// `allowed_hosts`. The host is resolved and every resolved address
// is rejected when it is private, loopback, link-local, multicast,
// unspecified, broadcast, benchmarking, or reserved. The check runs
// at connector create / update time and again immediately before
// every outbound call.
//
// Money: amounts are integer minor units (`i64` cents for
// two-decimal currencies, whole units for zero-decimal ones) in
// Rust, converted at the JSON / hook boundary by the
// currency-aware `major_to_minor` / `minor_to_major` helpers and
// by `unified_core::money::serde_major` on struct fields (which
// assumes two decimal places). DB columns stay `NUMERIC(19,4)`;
// there is no live persistence wiring in this module yet, so the
// `PgNumeric` conversion lives in `unified_core::db::money` for
// when it lands. Rates are percentages (11.0 means 11%) and stay
// `f64`, rounded to 4 decimal places by `round_tax`.
//
// Reports are exportable as JSON and as CSV. CSV is produced
// directly by the export endpoint; the import/export module's
// `export.requested` event is not used because this module's export
// is a synchronous read of its own in-memory store.
//
// The in-memory DashMap store is authoritative; the migration
// `2026-11-22-000038_tax` is the persistence target (see TODO.md
// conventions in the other modules).

// Tax module root.
//
// The module is split into one file per concern under
// `src/modules/tax/`. The public API is unchanged: every item
// previously reachable as `crate::modules::tax::Item` is
// re-exported here.

pub mod ai_capabilities;
pub mod computation;
mod connectors;
pub mod constants;
mod crypto;
mod handlers_connectors;
mod handlers_filings;
mod handlers_reports;
mod handlers_rules;
mod hooks;
pub mod module;
mod periods;
pub mod types;
mod validation;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::computation::*;
pub(crate) use self::computation::*;
pub(crate) use self::connectors::*;
pub use self::constants::*;
pub(crate) use self::constants::*;
pub(crate) use self::crypto::*;
pub(crate) use self::handlers_connectors::*;
pub(crate) use self::handlers_filings::*;
pub(crate) use self::handlers_reports::*;
pub(crate) use self::handlers_rules::*;
pub(crate) use self::hooks::*;
pub use self::module::*;
pub(crate) use self::module::*;
pub(crate) use self::periods::*;
pub use self::types::*;
pub(crate) use self::types::*;
pub(crate) use self::validation::*;
