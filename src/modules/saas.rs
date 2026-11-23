// SaaS multi-site module.
//
// Each site belongs to a company, has its own domain, theme, SSL status,
// and per-site settings namespace. Creating a site runs the provisioning
// flow: company check, DNS verification, site creation, then wiring of
// theme and SSL -- all through the hook bus, never through direct calls
// to other modules.
//
// Routes (all under `/api/v1/saas`):
//   GET    /sites                          - paginated sites (filter by company_id, status, domain_status; search)
//   POST   /sites                          - provision a site {company_id, name, domain, theme_id}
//   GET    /sites/{id}                     - one site (domain status, theme, SSL status)
//   PUT    /sites/{id}                     - update a site
//   DELETE /sites/{id}                     - delete a site
//   POST   /sites/{id}/suspend            - suspend a site
//   POST   /sites/{id}/activate           - activate a suspended site
//   GET    /sites/{id}/settings            - per-site settings namespace
//   PUT    /sites/{id}/settings            - merge per-site settings
//
// Hook contracts:
//   Provisioning emits `company.exists` (Company module) and `dns.verify`
//   (DNS verifier). When no handler answers, the site is still created
//   and the response carries a `warnings` array saying so, honestly:
//   a missing company handler means the company was accepted without
//   verification; a missing DNS handler means the domain was accepted
//   unchecked (`domain_status` becomes `unchecked`).
//   After creation the module emits `site.provisioned`, `ssl.renew`
//   (core event) for the domain, and `theme.activate_requested` for the
//   theme. Suspend / activate emit `site.suspended` / `site.activated`.

pub mod ai_capabilities;
pub mod types;
mod validation;
pub mod state;
mod handlers;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::*;
pub use self::validation::*;
pub use self::state::*;
pub use self::handlers::*;
pub use self::module::*;
