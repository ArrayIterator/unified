# Unified Pentagonal Project

**High-performance unified management system.**

One control plane for licensing, downloads, storefront, software and asset
management, and the day-to-day operations around them. One login, one
dashboard, many modules.

> Project status: 149 modules registered and wired through the hook/event
> bus (full list: [MODULES.md](MODULES.md)), 122 Diesel migrations (gapless 000001–000122)
> covering 675 tables (1,350 Diesel structs), 732,821 Rust lines under
> `src/` + `crates/` (counted 2026-11-23; the total grows while
> external-module crates are being added), payment security re-checked,
> public API access control with default-deny (see below). Integration
> round 2 (2026-11-22) registered all 137 modules, renumbered migrations
> gapless, and wired the cross-module hook contracts — see
> [INTEGRATION_NOTES_INTEGRATION2.md](docs/integration/INTEGRATION_NOTES_INTEGRATION2.md).
> The implementation milestones are tracked in
> [TODO.md](TODO.md). Compilation is unverified: Rust 1.100.0 is
> installed and `cargo check` has run in this environment, but it is
> OOM-blocked here (rustc needs ~5.3 GiB RSS and is killed repeatedly
> on this 7.7 GiB VM); a Colab run failed with exit 101 on 3 real code
> errors, which are fixed in the local tree and await a re-run after the
> final push. No migration has ever run against a live PostgreSQL and
> no endpoint has been live-tested; see TODO.md for the honest list of
> what is verified by reading only and what is still open.

## What it is

Unified Pentagonal Project is a CRM-like unified management system for:

- **Licensing management** — issue, validate, suspend, and revoke licenses
  for themes, plugins, and software.
- **File downloader** — secure, resumable downloads for licensed files and
  assets.
- **Storefront** — sell themes, plugins, and software, with software and
  asset management behind it.
- **Unified operations** — members and customers, projects and tasks,
  servers and sites, billing, support tickets, and content, in one place.

## Modules

**Full module catalogue: [MODULES.md](MODULES.md).**

- [Core modules](MODULES.md#core-modules) — 149 modules inside the binary, grouped by business area (Odoo-style, 8 categories).
- [External modules](MODULES.md#external-modules) — 5 native Rust crates (`crates/external-*`) implementing the same module contract.

## Integration round 2 (2026-11-22)

The 65 modules below were built in parallel batch rounds and integrated
in round 2 (registration, gapless migration renumbering 000001–000107,
Diesel schema/models, hook wiring, compile-blocker fixes). Full detail,
the renumbering map, and the honest open list live in
[INTEGRATION_NOTES_INTEGRATION2.md](docs/integration/INTEGRATION_NOTES_INTEGRATION2.md).

The complete categorized list of all 149 modules is in "Core modules"
above; the groups below are the round-2 batch history, not a second
list.

- **Projects & time:** projects (15 methodologies), timetracker,
  todos, okrs.
- **Commerce:** pos, pricing, pricing_tables, marketing, appointments,
  field_service, store digital products, entitlements, saas_packages,
  cards (card validation + payment reference ids).
- **Education:** academic, curriculum, admissions,
  exams, library, elearning, webinars.
- **Operations:** manufacturing, quality, expenses, approvals, studio,
  printing, assets, procurement, calculator, advisor, risk, hr, tax.
- **Platform:** tenants, tenant_destroy, calendar, holidays,
  network_tools, licensing (SaaS), saas_domains, presets, whitelabel,
  documents, privacy, web3, community, compliance, gamification,
  workflows, bi, affiliates, loyalty, portals, forms, contracts,
  incidents, inbox, surveys, vault, logistics, properties,
  supervision, events, storage_manager, auditors, domain_verification.

63 of the 149 modules declare themselves sellable as products
(`available_as_product`, see `src/core/modules.rs`); the rest are
infrastructure or core.

## ERP interconnection

Modules never call each other directly. All interconnection goes through
the hook/event bus (`src/core/hooks.rs`), so any module can observe or
change a flow by registering a hook handler. The main flows are:

- **Chat → Ticket → Knowledgebase:** a chat conversation can be escalated
  into a support ticket, and the ticket is answered with knowledgebase
  article suggestions.
- **Order → License → Download → Invoice:** a store order issues a
  license, the license unlocks the download, and billing raises the
  invoice for the order.
- **Refund → License revocation:** refunding an order revokes the
  licenses it issued (licensing listens to `store.order.refunded`)
  and revokes the order's download entitlements; the store's own
  mirror of the licenses is suspended.
- **Release published → Store catalog:** publishing a software
  release refreshes the matching store catalog entry (latest version,
  changelog, WordPress / PHP compatibility) when one exists for that
  software slug; software that is not sold through the store is
  untouched.
- **VCS import → Software draft → Manual publish:** a release
  imported from GitHub, GitLab, or Bitbucket creates a draft software
  version (idempotent per tag); publishing stays a manual step in the
  software module.
- **VCS settings → Settings store:** provider configuration flags
  (`token_configured`, `webhook_configured`, API base, GitLab hosts)
  are merged into the settings store under `vcs.<provider>` through
  the `settings.merge` hook; tokens themselves never leave the VCS
  module's write-only store.
- **Company → Domain → SSL → Site:** a company owns domains, domains get
  SSL certificates from the SSL manager, and sites are published on the
  secured domains.
- **Geo → Security → Audit:** a Geo lookup enriches the request context,
  security rules act on the Geo result, and the audit log records the
  decision with its Geo context.
- **AI Support → Knowledgebase → Ticket:** AI Support answers from the
  knowledgebase first and hands off to a human ticket when it cannot
  answer confidently.
- **Domain check → DNS verifier → SSL issue → Site publish:** a domain is
  verified with the DNS checker, the certificate is issued, and only
  then is the site published on that domain.

## Stack

- **Backend:** Rust, Actix-web, PostgreSQL (Diesel by default).
- **Frontend:** Handlebars (SEO friendly) with a modern Tailwind dashboard UI,
  easy navigation, and Dark / Light mode support. Dashboard scripts are
  TypeScript (`assets/js/*.ts`), transpiled to JavaScript in-memory at
  startup — no `.js` files in the repository.
- **Themes:** Handlebars (HBS) with dynamic loading.
- **Icons:** IcoMoon icon sets, for example Entypo and Material Icons.
- **TLS:** Let's Encrypt ACME, configurable from the dashboard, with SSL
  swapping from the dashboard or CLI, including custom SSL with validation
  and renewal.
- **Listeners:** Unix socket and TCP port listeners, HTTP and HTTPS
  (ports 80 / 443, with validation).
- **Realtime:** WebSocket support where needed.

## TypeScript / JavaScript engine

External modules can also be authored in **TypeScript** — no Rust
needed. The server embeds a **QuickJS** runtime
(`crates/unified-scripting`, via `rquickjs`): TypeScript sources
are transpiled to JavaScript in-memory with OXC and executed in an
isolated QuickJS context per module, with memory/time limits. There
is deliberately no V8 here: QuickJS is small (~1 MB) and
musl-friendly, while V8 would break the static musl binary.

Every TS module sees one `Unified` global object holding the whole
SDK (hooks, routes, events, tenant-scoped DB, read-only config, log,
allowlisted HTTP, scoped storage) — the same shape as Bun's `Bun`
global. The type declarations are the canonical contract:
`crates/unified-scripting/unified.d.ts` (type-check a module with
`tsc --noEmit`). A working example lives at
`modules/example-ts/module.ts`. Native Rust external modules
(`crates/external-*`) stay as-is — TypeScript is a second plugin
layer on top of them.

TODO: publish the `Unified` SDK types as an `@types`-style SDK
package, so external TS/JS modules can depend on the standardized
public API types instead of copying `unified.d.ts`.

## API standard

All JSON APIs follow the same shape and standard HTTP status codes.

Success responses contain only the `data` key:

```json
{
  "data": "..."
}
```

Error responses contain only the `error` key:

```json
{
  "error": {
    "message": "...",
    "code": 404,
    "metadata": {}
  }
}
```

The `error` object has exactly three keys: `message`, `code`, and
`metadata`. In debug / development mode only, an optional `trace` key is
added.

Every module exposes its API routes and registers them in the versioned
public API registry (API Manager) with real version support — no module
is dashboard-only unless it is explicitly internal. Public API routing
has versioning with version settings (v1, v2, v3, or all versions, or a
multi-select per endpoint; new versions can be created; the global
prefix is configurable, e.g. `v`). The API Manager controls allowed
public access: per-endpoint and per-version allow/deny, per API key /
client scopes (which endpoints, versions, and methods a key may call),
per-key rate limits, and public-access on/off per endpoint, with an
audit log of access-control changes. A public endpoint that is not
explicitly allowed is denied by default, enforced in the request path
by middleware — not just recorded as data. Access rules and the audit
trail persist in the database (`api_access_rules`, `api_access_audit`).

## Security

- Passwords are hashed with **Argon2**.
- Sessions are **stateless**, using a signed session tokenizer.
- **TOTP** two-factor authentication.
- **CSRF** protection on state-changing requests.
- **Session flash** messages kept in memory.

## License

Proprietary. See [LICENSE](LICENSE). This repository is private and for the
project team only.
