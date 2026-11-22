# Unified Pentagonal Project

**High-performance unified management system.**

One control plane for licensing, downloads, storefront, software and asset
management, and the day-to-day operations around them. One login, one
dashboard, many modules.

> Project status: the core engine is implemented in this batch — the
> JSON API envelope, pagination and filtering, hook/event bus, module
> registry, validated YAML configuration, shared application context,
> and the core modules listed below. The implementation milestones are
> tracked in [TODO.md](TODO.md).

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

## Core modules

Core modules ship inside the binary and cannot be removed:

1. **Service Monitor & Management** — health of sites, services, and
   servers; server information; reload with dry-run support.
2. **Security Management** — security checks, policies, and audit trails.
3. **User Management** — users, roles, and permissions.
4. **Modules / Extensions Manager** — list, enable, disable, and configure
   extensions. Extensions are fully integrated and can interconnect.
5. **Content Management** — posts, categories, and pages, with a site
   builder.
6. **Settings** — site-wide settings (details are defined per setting).
7. **SEO Manager** — SEO management for sites and content.
8. **API Manager** — create and manage public and client APIs with rules,
   versioning, and a registry of registered endpoints. This is separate
   from the core API.
9. **Auth Manager** — authentication with submodules: SSO, OAuth server,
   OAuth login, SSO as provider, social login (for example Google and
   Facebook), biometric login, and a JSON Web Token manager. Submodules can
   inject their own settings into the Auth Manager.
10. **Translations / i18n Manager** — dynamic translations and translation
    management.
11. **Roles Manager** — static default roles plus dynamic, hookable roles
    and a role factory for access checks.
12. **Company Management** — multi-site and multi-company support with an
    immutable site basis.
13. **Domain / Subdomain Manager** — domains and subdomains, interconnected
    with the SSL manager in both directions.
14. **Network Manager** — networking core with a GEO submodule and a DNS
    verifier / DNS checker library, wired into Domain, Mail, and Security.
15. **GEO Location** — GeoLite / MaxMind-compatible database, downloadable
    from MaxMind or a custom URL, a global Geo information variable in the
    core engine, ASN checks, and IP-range-to-country settings overrides
    (IPv4 and IPv6) that win over the database reading by default.
16. **Application Connection & Sync** — connection and sync core for
    desktop app integration.
17. **Chat** — realtime chat, interconnected with Support Tickets and the
    Knowledgebase through hooks.
18. **AI Support** — AI chat with connectors, knowledgebase-grounded
    answers, and handoff to a human agent.
19. **Knowledgebase** — searchable articles that Chat and AI Support
    suggest and that tickets link back to.
20. **Store / Marketplace** — sell themes, plugins, and software; cart and
    checkout feed orders into licensing and billing.
21. **Licensing** — issue, validate, suspend, and revoke licenses;
    licenses gate downloads and store purchases.
22. **Software and Assets** — software products and their assets, behind
    the storefront and the downloader.
23. **Billing / Invoicing** — invoices raised from store orders and
    licenses, with payment tracking.
24. **Support Tickets** — tickets created from chat, AI Support, and CRM,
    with knowledgebase suggestions attached.
25. **Notifications** — in-app and email notifications for events raised
    across modules.
26. **Audit Log** — an append-only record of security-relevant events,
    including Geo and network context.
27. **Scheduler / Queue** — scheduled jobs and background queues, including
    Geo database updates and SSL renewal.
28. **File Manager** — file browsing and management for uploads and
    assets.
29. **CRM Contacts / Leads** — contacts and leads shared by sales, support,
    and billing.
30. **Mail Manager** — SMTP settings, mail connectors, and the default
    mailer used by notifications and billing.
31. **Email Template Builder** — visual template editor (GrapeJS) for the
    emails the Mail Manager sends.
32. **Site Builder** — visual site building for content and pages.
33. **SaaS Multi-Site** — multi-site hosting with the Site Builder
    integrated.
34. **Downloader** — secure, resumable downloads for licensed files and
    assets.

**Theme Management** is also a core module: add themes through a templating
engine, embed themes internally, or load them dynamically, with strict
checking and screenshot support. The engine (`src/themes/engine.rs`) is
Handlebars with `rust-embed`: the default theme ships in the binary from
`templates/default/`, disk themes are discovered from `themes/` and loaded
only after strict validation (manifest must exist and parse, the layout
template must exist, theme ids reject path traversal). The dashboard
(`templates/default/dashboard.hbs`, `assets/css/dashboard.css`,
`assets/js/dashboard.js`) is server-rendered with light/dark mode, a
sidebar, stat cards, a module grid fed live from `/api/v1/extensions`,
and an activity feed that shows an honest empty state when there is no
data yet.

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
- **Frontend:** React (SEO friendly) with a modern Tailwind dashboard UI,
  easy navigation, and Dark / Light mode support.
- **Themes:** Handlebars (HBS) with dynamic loading.
- **Icons:** IcoMoon icon sets, for example Entypo and Material Icons.
- **TLS:** Let's Encrypt ACME, configurable from the dashboard, with SSL
  swapping from the dashboard or CLI, including custom SSL with validation
  and renewal.
- **Listeners:** Unix socket and TCP port listeners, HTTP and HTTPS
  (ports 80 / 443, with validation).
- **Realtime:** WebSocket support where needed.

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

## Security

- Passwords are hashed with **Argon2**.
- Sessions are **stateless**, using a signed session tokenizer.
- **TOTP** two-factor authentication.
- **CSRF** protection on state-changing requests.
- **Session flash** messages kept in memory.

## License

Proprietary. See [LICENSE](LICENSE). This repository is private and for the
project team only.
