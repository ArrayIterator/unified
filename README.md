# Unified Pentagonal Project

**High-performance unified management system.**

One control plane for licensing, downloads, storefront, software and asset
management, and the day-to-day operations around them. One login, one
dashboard, many modules.

> Project status: 137 modules registered and wired through the hook/event
> bus (see the list below), 107 Diesel migrations covering 672 tables
> (1,344 Diesel structs), 476,228 Rust lines under `src/`, payment
> security re-checked, public API access control with default-deny
> (see below). Integration round 2 (2026-11-22) registered all 137
> modules, renumbered migrations gapless, and wired the cross-module
> hook contracts — see [INTEGRATION_NOTES_INTEGRATION2.md](INTEGRATION_NOTES_INTEGRATION2.md).
> The implementation milestones are tracked in
> [TODO.md](TODO.md). `cargo check` has not run in this environment (no
> toolchain, crates.io blocked) — compilation is unverified; see TODO.md
> for the honest list of what is verified by reading only and what is
> still open.

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
20. **Store / Marketplace** — sell themes, plugins, and software;
    products with pricing tiers and editions, one-time and subscription
    pricing with trials, coupons, cart and checkout, orders with a
    validated lifecycle, HMAC-signed expiring download links,
    entitlements, a WordPress-style update API, vendor payout
    summaries, and an update catalog. Cart and checkout feed orders
    into licensing and billing.
21. **Licensing** — issue, validate, suspend, and revoke licenses;
    licenses gate downloads and store purchases. Orders issue licenses
    with seats and expiry from the purchased tier; refunds revoke
    them.
22. **Software and Assets** — enterprise software manager: software
    records with a status lifecycle and vendor-company ownership,
    versions with semver and platform requirements, releases with a
    publish flow, per-release files with sha256/sha512 checksum
    verification, hierarchical groups and categories, tags, and
    full-text discovery, behind the storefront and the downloader.
    Releases imported from a connected VCS repository arrive as
    review drafts and are published manually.
23. **Billing / Invoicing** — invoices raised from store orders and
    licenses, with payment tracking.
24. **Support Tickets** — tickets created from chat, AI Support, and CRM,
    with knowledgebase suggestions attached; full status workflow,
    priorities, assignment, internal notes, customer replies,
    storage-backed attachments, canned responses, SLA timers with breach
    alerts and escalation rules, merging and linking to orders, licenses,
    and servers, routed through the Departments module.
25. **Notifications** — a core module: system and per-user notifications
    with audience targeting (single user, role, department, segment, or
    broadcast), a notification centre with unread counts, filters and
    per-user channel/category/digest preferences (including digest off),
    per-event templates with email delivery through the configured email
    provider (or the Mail Manager fallback) and GrapeJS email templates,
    delivery tracking with retries, and listeners for the other modules'
    important events (orders, payments, backups, clusters, tickets,
    licenses, incidents, webhooks, security). Announcements compose with
    audience, channels, scheduling and expiry, fan out into the
    notification centre and the user dashboard, and can also go out as
    email.
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
35. **Third-Party VCS Integrations** — GitHub, GitLab, and Bitbucket
    integrations: repository connections linked to software records,
    release import as review drafts (never auto-published), HMAC /
    token-verified webhooks, allowlisted release-asset downloads with
    streamed SHA-256 verification, manual sync with rate-limit
    handling, and sync history. Provider tokens are write-only and
    never logged or returned; configuration flags are merged into the
    settings store under `vcs.<provider>`.
36. **Cache** — HTTP access to the shared application cache (filesystem
    and Redis adapters).
37. **AI Module** — provider abstraction (Anthropic, OpenAI,
    OpenAI-compatible, local): drafts ticket/chat replies, CMS content,
    and email/newsletter copy from a prompt plus site context, with
    human approval before anything is sent or published — never
    auto-send. Credentials write-only and encrypted, per-key rate
    limits, usage tracking, audit log, per-role capability checks.
    Consumes the same shared context as the MCP server.
38. **Analytics** — analytics and reports: sales, licenses, usage,
    uptime, and revenue.
39. **Announcements** — announcements and changelog; audience-targeted,
    scheduled, expiring, and pinned; fans out into the notification
    centre and the user dashboard, and can go out as email.
40. **API Keys** — enterprise API key management; the single source of
    truth for API keys (the API Manager delegates to it), with per-key
    scopes and rate limits.
41. **Backup Manager** — scheduled and on-demand backups to local, S3,
    Google Drive, and R2 storage, with restore.
42. **Bank Transfer** — manual payment verification: proof upload, admin
    approval/rejection, split bills.
43. **Certificates** — advanced SSL certificate management via config and
    ACME, with renewal and lifecycle events.
44. **Cloudflare** — Cloudflare connector: zones, DNS, and cache.
45. **Clusters** — multi-cluster connection and mutual sync, with load
    balancing across nodes.
46. **Dashboards** — separate admin, user (client), and reseller
    dashboards, one shared design system, role/capability enforced
    server-side per route, with admin "view as user" for support
    (audited).
47. **Departments** (core) — departments CRUD, agents/staff per
    department linked to users and roles, routing rules (category,
    product, keyword, manual), per-department SLA policies, working
    hours and out-of-hours auto-replies.
48. **Email Providers** — email provider registry (Mailchimp, Brevo,
    SendGrid, and others) with settings, behind the Mail Manager.
49. **Hosting** — hosting plans and accounts; a module that needs another
    module auto-activates it (the hosting module requires the
    cart/sales module) and the module list shows the requirement.
50. **Import / Export** — CSV and JSON data exchange.
51. **Load Balancer** — load balancing across backends, sharing the one
    TLS storage with the server.
52. **Logs** — central log viewer.
53. **Maintenance** — maintenance windows; the status endpoint and the
    status-page API stay reachable during maintenance.
54. **MCP Server** — Model Context Protocol access to the site's own
    context (tools, resources, prompts assembled from the modules' own
    APIs, never raw table dumps). MVP database access is read-only only:
    declared tables/queries, parameterized queries, no writes, no DDL,
    no credential columns. API-key auth with per-key tool allowlists;
    sellable per server/site through Product Management.
55. **Meetings** — Zoom-like video meetings: rooms (scheduled and
    instant), WebRTC signalling over the WebSocket hub, screen share,
    in-meeting chat, recording metadata via storage, host/co-host/
    participant roles, join tokens, waiting room, scheduling, history
    and attendance. Paid rooms check product entitlements at join.
    Signalling and room management are real; media transport is browser
    WebRTC — no media server/SFU is built in (see TODO.md).
56. **Newsletter** — newsletter sending through the email providers.
57. **Panels** — hosting/billing panel connectors (WHMCS, cPanel/WHM,
    DirectAdmin, Plesk, CyberPanel, generic).
58. **Payment Gateway** — a wrapper/framework for many providers (cards,
    Swift, 2Checkout, HyperPay, Xendit, Mastercard, Google Pay,
    custom) plus bank transfer and split bills; see the payment
    security re-check in TODO.md.
59. **Resource Planner** — capacity planning across servers, clusters,
    and providers.
60. **Product Management** — the single source of truth for what is
    sold: lifecycle (draft → active → deprecated → retired), types
    (digital, physical, service, subscription, bundle, meeting,
    hosting), variants and options, bundles with cycle rejection,
    tiers, per-seat and usage-based pricing, regional pricing, taxes,
    physical inventory, files and versions linked to the Software
    Manager, groups/categories/tags, vendor ownership. The store sells
    what Product Management defines.
61. **Providers** — cloud/VPS/container provider connectors (AWS,
    DigitalOcean, Linode, Hetzner, Huawei, OVH, Tencent, and others),
    plus server management for Redshift, OpenStack, KVM, Proxmox, and
    Xen.
62. **Records** — records store behind the audit trail, with retention
    from config.
63. **Routes** — customizable page routes: login, register, admin and
    user dashboard paths are changeable via admin or config.
64. **Saved Views** — saved filters/views for lists.
65. **Global Search** — global search across modules, with a timeline.
66. **Status Page** — public status page with incidents.
67. **Storage** — storage abstraction (local, S3, Drive, R2) with signed
    access.
68. **Tasks** — task management.
69. **Activity Timeline** — activity timeline across modules.
70. **Webhooks** — outgoing webhook endpoints and deliveries.
71. **Project Management** — projects with 15 selectable
    methodologies (Kanban, Scrum, Waterfall, Scrumban, Agile, Lean,
    XP, PRINCE2, PMBOK, CPM, CCPM, Six Sigma, Hybrid, Shape Up,
    SAFe), boards and columns, tasks with subtasks and dependencies,
    sprints, phases and gates, cycles, program increments, business
    cases, milestones, time tracking with billable hours, automation
    rules, templates, and reports, wired into the ERP bus (billing
    for invoiced time, notifications, audit, and search).

Plus **Theme Management** (described below) — 72 registered modules in
total, counted from `src/modules/mod.rs` (`pub mod` == register calls ==
module files).

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

## Integration round 2 (2026-11-22)

The 65 modules below were built in parallel batch rounds and integrated
in round 2 (registration, gapless migration renumbering 000001–000107,
Diesel schema/models, hook wiring, compile-blocker fixes). Full detail,
the renumbering map, and the honest open list live in
[INTEGRATION_NOTES_INTEGRATION2.md](INTEGRATION_NOTES_INTEGRATION2.md).

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

50 of the 137 modules declare themselves sellable as products
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
