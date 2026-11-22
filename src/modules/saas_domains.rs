// SaaS Domain Management module — preregister reservations, custom
// domain aliases, tenant subdomain allocation, and tenant resolution.
//
// This module extends the Domains (`domains`) and SaaS (`saas`)
// modules through the hook bus only; it never calls into them
// directly (except for the public `company_exists` helper, the same
// direct helper the Domains module itself uses). It owns three record
// kinds, all kept in DashMap stores that are authoritative at runtime;
// the migration `2026-11-22-000070_saas_domains` creates the matching
// PostgreSQL tables as persistence targets.
//
// MODULARITY RULE (standing architecture rule): nothing in this
// module is hard-coded behaviour. Every list, pattern, quota, and
// base domain that shapes behaviour lives in `SaasDomainsSettings`
// (admin-editable through `GET/PUT /api/v1/saas/domains/settings`,
// config keys under the `saas_domains.*` namespace) with a clearly
// marked factory default that an administrator can override without
// touching code. That covers: the wildcard base domain
// (`saas_domains.wildcard_base`), the main-site host
// (`saas_domains.main_host`), the reserved subdomain labels
// (`saas_domains.reserved_labels`), the allowed label pattern
// (`saas_domains.label_pattern`), the per-tenant subdomain quota
// (`saas_domains.max_subdomains_per_tenant`), whether self-assigned
// subdomains need admin approval
// (`saas_domains.require_subdomain_approval`), the default
// reservation lifetime (`saas_domains.default_reservation_ttl_hours`),
// and the allowed alias target types
// (`saas_domains.alias_target_types`). Tenant resolution runs the
// same configurable rules in one place (`resolve_host`): control
// plane first, then tenant subdomain, then verified custom alias,
// then blocked, then no result. The three `blocked` reason codes
// (`unverified_domain`, `suspended`, `released`) are the one
// deliberate exception: they are wire-contract strings the
// tenant-resolution middleware (batch 8, builder B4) matches on
// exactly, so they are stable contract values, not tunables;
// changing them would break the middleware contract.
//
// Product subdomains (batch 15, builder B2) live in this same
// file: each purchased product or package claims ITS OWN subdomain
// (`<label>.<wildcard_base>`), scoped to that product. The surface
// is under `/api/v1/saas/domains/product-subdomains` (see the route
// list below); the record, settings, resolution, and hook contracts
// are documented in `docs/integration/INTEGRATION_NOTES_BATCH15_B2.md`.
// A product subdomain resolves through the same
// `tenant.resolve_requested` contract as a generic allocation, with
// kind `subdomain` (the only kind the tenant middleware accepts)
// plus the extra fields `scope: "product"`, `product_id`,
// `package_id`, and `product_subdomain_id`, so the middleware keeps
// working unchanged while downstream handlers can enforce the
// product scope with `require_product_scope`. A request on a product
// subdomain only ever serves that product's module surfaces; the
// middleware contract and the exact integrator steps are in the
// notes file.
//
// Routes (all under `/api/v1/saas/domains`):
//   POST   /product-subdomains                 - claim a product subdomain
//   POST   /product-subdomains/check           - availability check for a label
//   GET    /product-subdomains                 - paginated product subdomains
//   GET    /product-subdomains/{id}            - one product subdomain
//   POST   /product-subdomains/{id}/verify     - mark verified (verification module must confirm first)
//   POST   /product-subdomains/{id}/activate   - activate a verified product subdomain
//   POST   /product-subdomains/{id}/suspend    - suspend a product subdomain
//   POST   /product-subdomains/{id}/release    - start release (grace period first)
//   POST   /product-subdomains/expire          - finalize releases whose grace ended (admin)
//   POST   /reservations                       - reserve a domain
//   GET    /reservations                       - paginated reservations
//   GET    /reservations/{id}                  - one reservation
//   POST   /reservations/{id}/release          - release a reservation
//   POST   /reservations/{id}/provision        - consume a reservation
//   POST   /reservations/expire                - expire stale reservations
//   POST   /aliases                            - create a custom alias
//   GET    /aliases                            - paginated aliases
//   GET    /aliases/{id}                       - one alias
//   PUT    /aliases/{id}                       - update an alias
//   POST   /aliases/{id}/suspend               - suspend an alias
//   POST   /aliases/{id}/release               - release an alias
//   DELETE /aliases/{id}                       - delete an alias
//   POST   /subdomains                         - allocate a subdomain
//   GET    /subdomains                         - paginated allocations
//   GET    /subdomains/{id}                    - one allocation
//   POST   /subdomains/{id}/approve            - approve a pending allocation
//   POST   /subdomains/{id}/reject             - reject a pending allocation
//   POST   /subdomains/{id}/suspend            - suspend an allocation
//   POST   /subdomains/{id}/release            - release an allocation
//   DELETE /subdomains/{id}                    - delete an allocation
//   GET    /resolve?host=...                   - resolve-debug (admin only)
//   GET    /settings                           - read module settings
//   PUT    /settings                           - update module settings
//
// Capability model (same shape as the Properties module): the caller
// is identified by the `X-User-Id` header (required) and `X-Role`
// carries the role slug. Capability checks use the `role.check`
// hook and fail closed (no answer means not allowed). Admin work
// needs `saas_domains.manage`; reading needs `saas_domains.read`;
// a tenant acting on its own records needs
// `saas_domains.self_assign` plus an `X-Company-Id` header that
// matches the record's company. A caller without the manage
// capability can never see or touch another tenant's records.
//
// Product-subdomain events emitted on the hook bus:
//   - `saas.product_subdomain.claimed`, `.verified`, `.activated`,
//     `.suspended`, `.release_started`, `.released`,
//     `.resolve_scoped`
// Events this module answers for product subdomains:
//   - `saas_packages.purchase.completed` — claim intent carrying an
//     explicit entitlement attestation (see the handler note)
//   - `saas_packages.cancelled` — starts release (grace) for the
//     package's product subdomains
//   - `license.revoked` / `module.entitlement.denied` — suspends the
//     affected product subdomains
//   - `domain.verification.completed` — flips a matching product
//     subdomain to verified (same event as the alias path)
//   - `site.provision_requested` — claim intent from the packages
//     provisioning flow (the packages module validates entitlements
//     before emitting it; see the handler note) and from the
//     tenants provisioning flow (source `tenants`: a generic
//     tenant-level allocation, no product id; see the handler note)
//   - `saas_domains.subdomain.release_requested` — tenant destroy
//     revokes every subdomain of the tenant (generic allocations
//     immediately, product subdomains into release grace)
//   - `whitelabel.domain.verified` — attaches a verified custom
//     domain to the product subdomain, inheriting its product scope
//   - `saas_domains.ai.suggest_subdomain_requested` — AI capability
//     hook, answered with availability-checked label suggestions
//   - `saas.product_subdomain.expire_grace` — scheduler sweep that
//     finalizes releases whose grace period ended
//
// Events emitted on the hook bus:
//   - `saas_domains.reservation.created`, `.released`, `.expired`,
//     `.provisioned`
//   - `site.provisioned` (reservation handoff; the SaaS module owns
//     the site row itself)
//   - `saas_domains.alias.created`, `.updated`, `.suspended`,
//     `.released`
//   - `saas_domains.subdomain.created`, `.approved`, `.rejected`,
//     `.suspended`, `.released`
//   - `saas_domains.settings.updated`
// Events this module answers:
//   - `tenant.resolve_requested` — `{"host": ...}`; see `resolve_host`
//   - `saas_domains.alias.lookup` — `{"domain": ...}` or
//     `{"alias_id": ...}`; returns the alias record or no result
//   - `saas_domains.reservations.expire` — scheduler sweep; returns
//     `{"expired": n, "ids": [...]}` and releases past-due
//     reservations
//   - `domain.verification.completed` — flips a matching alias to
//     `verified` (only the verification module may do this; there is
//     deliberately no API endpoint that sets an alias to verified)

mod caller;
pub mod events;
mod handlers;
mod handlers_aliases;
mod handlers_reservations;
mod handlers_subdomains;
mod hooks;
pub mod module;
mod product_subdomains;
mod requests;
pub mod resolve;
pub mod settings;
mod store;
pub mod types;
mod validation;

pub use self::events::*;
pub use self::module::*;
pub use self::resolve::{
    product_scope_for_host, require_product_scope, resolve_host, resolve_product_host,
};
pub use self::settings::SaasDomainsSettings;
pub use self::types::*;
