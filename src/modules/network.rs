// Network module.
//
// Provides local network information, active-connection counts, Geo
// lookups (delegated to the GEO module through the `geo.lookup` hook
// event), and the DNS verifier library (`DnsVerifier`).
//
// DNS / mail / security wiring (documented here so the other modules can
// rely on it):
//   - `POST /api/v1/network/dns/check` runs a full DNS report and then
//     emits a `dns.check` hook event carrying the report, so the Domains,
//     Mail, and Security modules can observe every check without calling
//     this module directly.
//   - The Domains module verifies a domain by calling `DnsVerifier`
//     directly (async DNS cannot run inside a synchronous hook handler)
//     and emits `domain.verify`; the Mail module can reuse the same
//     SPF / DKIM / DMARC reports for sender authentication, and the
//     Security module can reuse `dns_health` for domain reputation.
//   - Geo lookups are delegated through the `geo.lookup` hook event:
//     the GEO module registers a synchronous handler for it, and this
//     module falls back to `crate::modules::geo::lookup_geo` when no
//     handler answered.

pub mod types;
pub mod stats;
pub mod dns;
mod handlers;
pub mod module;

pub use self::types::*;
pub use self::stats::*;
pub use self::dns::*;
pub use self::handlers::*;
pub use self::module::*;
