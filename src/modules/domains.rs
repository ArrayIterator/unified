// Domains module - domain and subdomain management.
//
// Domains are verified with the Network module's DNS verifier (SPF /
// DKIM / DMARC / MX / A records) plus a live TLS probe on port 443 that
// reads the certificate expiry. SSL lifecycle interconnects with the SSL
// manager through the hook bus:
//   - `POST /{id}/ssl-renew` emits `ssl.renew` for the SSL manager.
//   - This module listens to `ssl.renew` (completed renewals carrying
//     `ssl_expires_at`) and `ssl.swap` and updates the domain's
//     `ssl_status` / `ssl_expires_at` accordingly.
//   - Every verification emits `domain.verify` with the full report, so
//     the Mail and Security modules can observe it.

pub mod ai_capabilities;
mod types;
mod tls;
mod state;
mod service;
mod handlers;
pub mod routes;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::routes::DomainsModule;
pub use self::routes::register;
