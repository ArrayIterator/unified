# Modules

Module catalogue for the Unified Pentagonal Project, moved out of README.md to keep the README short. ([← Back to README](README.md))

- [Core modules](#core-modules)
- [External modules](#external-modules)

## Core modules

Core modules ship inside the binary and cannot be removed. The 150 modules
below are grouped by business area, Odoo-style: each top-level category has
subcategories, and every module appears exactly once. Counts are verified
against `src/modules/mod.rs` (`pub mod` lines).

### 1. Platform & Administration (28)

#### Platform & Administration > Core System (15)

- **Service Monitor & Management** — health of sites, services, and
- **Security Management** — security checks, policies, and audit trails.
- **User Management** — users, roles, and permissions.
- **Roles Manager** — static default roles plus dynamic, hookable roles and a role factory for access checks.
- **Modules / Extensions Manager** — list, enable, disable, and configure
- **Settings** — site-wide settings (details are defined per setting).
- **Audit Log** — an append-only record of security-relevant events, including Geo and network context.
- **Scheduler / Queue** — scheduled jobs and background queues, including Geo database updates and SSL renewal.
- **Logs** — central log viewer.
- **Maintenance** — maintenance windows; the status endpoint and the status-page API stay reachable during maintenance.
- **Import / Export** — CSV and JSON data exchange.
- **Presets** — setup presets and a guided setup wizard that applies a preset's module selection and seeds to a company.
- **Saved Views** — saved filters/views for lists.
- **Activity Timeline** — activity timeline across modules.
- **Status Page** — public status page with incidents.

#### Platform & Administration > Identity & Access (4)

- **Auth Manager** — authentication with submodules: SSO, OAuth server,
- **API Manager** — create and manage public and client APIs with rules,
- **API Keys** — enterprise API key management; the single source of truth for API keys (the API Manager delegates to it), with per-key scopes and rate limits.
- **Entitlements** — what a tenant may use: entitlement grants, per-module enable/disable and access checks, denied-attempt logging, history, and package changes.

#### Platform & Administration > Tenancy & SaaS (9)

- **SaaS Multi-Site** — multi-site hosting with the Site Builder integrated.
- **SaaS Domains** — tenant domain handling: domain reservations, aliases with suspend/resume/release, and per-product subdomains.
- **SaaS Packages** — sellable SaaS packages: package versions, purchases, provisioning steps with per-module outcomes, and purchase history.
- **Tenants** — tenant lifecycle: provisioning with per-module grants, suspend/resume, module grant/revoke, and tenant context views.
- **Tenant Destroy** — tenant destruction requests with two-step approval, grace period, export proof, and external-cleanup confirmation; destroyed tenants keep no record of their own.
- **Whitelabel** — per-company whitelabel branding: global and per-company allowances, defaults, branding profiles, and change history.
- **Domain / Subdomain Manager** — domains and subdomains, interconnected with the SSL manager in both directions.
- **Domain Verification** — domain ownership verification: verification tokens, attempts, status, and settings, backing the domain → SSL → site-publish flow.
- **Company Management** — multi-site and multi-company support with an immutable site basis.

### 2. Infrastructure & Network (21)

#### Infrastructure & Network > Network & Domains (7)

- **Network Manager** — networking core with a GEO submodule and a DNS verifier / DNS checker library, wired into Domain, Mail, and Security.
- **Network Tools** — network diagnostics: DNS lookup, ping, traceroute, port check, whois, HTTP check, IP info, blacklist check, certificate watches, uptime monitors, and saved-check history.
- **GEO Location** — GeoLite / MaxMind-compatible database, downloadable from MaxMind or a custom URL, a global Geo information variable in the core engine, ASN checks, and IP-range-to-country settings overrides (IPv4 and IPv6) that win over the database reading by default.
- **Clusters** — multi-cluster connection and mutual sync, with load balancing across nodes.
- **Load Balancer** — load balancing across backends, sharing the one TLS storage with the server.
- **Hosting** — hosting plans and accounts; a module that needs another module auto-activates it (the hosting module requires the cart/sales module) and the module list shows the requirement. Now a submodule of `cloud`; `/api/v1/hosting/*` is kept as a compat alias.
- **Cloudflare** — Cloudflare connector: zones, DNS, and cache.

#### Infrastructure & Network > Storage & Data (6)

- **Storage** — storage abstraction (local, S3, Drive, R2) with signed access.
- **Storage Manager** — storage plans with quotas and metering, buckets and objects with lifecycle and signed URLs, and bring-your-own (BYO) external storage connections with health checks and terms.
- **Backup Manager** — scheduled and on-demand backups to local, S3, Google Drive, and R2 storage, with restore.
- **Cache** — HTTP access to the shared application cache (filesystem and Redis adapters).
- **Downloader** — secure, resumable downloads for licensed files and assets.
- **Third-Party VCS Integrations** — GitHub, GitLab, and Bitbucket integrations: repository connections linked to software records, release import as review drafts (never auto-published), HMAC / token-verified webhooks, allowlisted release-asset downloads with streamed SHA-256 verification, manual sync with rate-limit handling, and sync history. Provider tokens are write-only and never logged or returned; configuration flags are merged into the settings store under `vcs.<provider>`.

#### Infrastructure & Network > Platform Services (10)

- **Panels** — hosting/billing panel connectors (WHMCS, cPanel/WHM, DirectAdmin, Plesk, CyberPanel, generic).
- **Providers** — cloud/VPS/container provider connectors (AWS, DigitalOcean, Linode, Hetzner, Huawei, OVH, Tencent, and others), plus server management for Redshift, OpenStack, KVM, Proxmox, and Xen. Now a submodule of `cloud`; `/api/v1/providers/*` is kept as a compat alias.
- **Cloud** — cloud parent module: hosting plans and accounts, cloud provider connectors (OpenStack, Redshift, Docker, Kubernetes, EC2, Proxmox, KVM, Xen), containers and app deployment (Docker create, Kubernetes write ops), virtualization grouping, and the Cloud SaaS package (package → plans → apps). Sellable package; tenants without it get 403/404. External "cloud panel" modules (SolusVM, Virtualizor, cPanel, DirectAdmin, Plesk, CyberPanel, WHMCS, SSL providers) hook in via `cloud.provider.definitions`, `cloud.panel.definitions`, and `cloud.ssl_provider.definitions`.
- **Webhooks** — outgoing webhook endpoints and deliveries.
- **Routes** — customizable page routes: login, register, admin and user dashboard paths are changeable via admin or config.
- **Global Search** — global search across modules, with a timeline.
- **Translations / i18n Manager** — dynamic translations and translation management.
- **Reference Data** — platform-shared, read-only reference lists: countries with flags (ISO 3166-1), currencies (ISO 4217), timezones (IANA) and languages (ISO 639-1); one single source per list, never tenant-scoped.
- **Application Connection & Sync** — connection and sync core for desktop app integration.
- **Web3** — blockchain integration: chain networks and providers (EVM, Cardano, Stellar, plus a fake provider), watched addresses, transaction records, anchors with verification, and wallet links.

### 3. AI & Intelligence (7)

#### AI & Intelligence > AI (4)

- **AI Module** — provider abstraction (Anthropic, OpenAI, OpenAI-compatible, local): drafts ticket/chat replies, CMS content, and email/newsletter copy from a prompt plus site context, with human approval before anything is sent or published — never auto-send. Credentials write-only and encrypted, per-key rate limits, usage tracking, audit log, per-role capability checks. Consumes the same shared context as the MCP server.
- **MCP Server** — Model Context Protocol access to the site's own context (tools, resources, prompts assembled from the modules' own APIs, never raw table dumps). MVP database access is read-only only: declared tables/queries, parameterized queries, no writes, no DDL, no credential columns. API-key auth with per-key tool allowlists; sellable per server/site through Product Management.
- **AI Support** — AI chat with connectors, knowledgebase-grounded answers, and handoff to a human agent.
- **Advisor** — rule-based advisor: evaluates rules over invoice, payment, license, CRM, and subscription facts and produces suggestions that can be acknowledged, snoozed, or dismissed.

#### AI & Intelligence > Analytics (3)

- **Analytics** — analytics and reports: sales, licenses, usage, uptime, and revenue.
- **BI (Business Intelligence)** — reports, KPI dashboards, and forecasts (revenue, churn, cashflow) computed from cross-module source records.
- **Dashboards** — separate admin, user (client), and reseller dashboards, one shared design system, role/capability enforced server-side per route, with admin "view as user" for support (audited).

### 4. Sales & Commerce (22)

#### Sales & Commerce > Store & Marketplace (8)

- **Store / Marketplace** — sell themes, plugins, and software; products with pricing tiers and editions, one-time and subscription pricing with trials, coupons, cart and checkout, orders with a validated lifecycle, HMAC-signed expiring download links, entitlements, a WordPress-style update API, vendor payout summaries, and an update catalog. Cart and checkout feed orders into licensing and billing.
- **Product Management** — the single source of truth for what is sold: lifecycle (draft → active → deprecated → retired), types (digital, physical, service, subscription, bundle, meeting, hosting), variants and options, bundles with cycle rejection, tiers, per-seat and usage-based pricing, regional pricing, taxes, physical inventory, files and versions linked to the Software Manager, groups/categories/tags, vendor ownership. The store sells what Product Management defines.
- **Pricing** — price engine: money and proration math, discount rules, coupons, bundles, credit notes, reconciliation, and price recommendations.
- **Pricing Tables** — pricing tables with tiers and features, plus a vendor marketplace: commission rules, commissions, payouts, and product approvals.
- **POS** — point of sale: registers with cashier assignment and shifts, sales, checkout with payment methods, receipts, product search, and end-of-day reports.
- **Rental** — rental products: rental orders and contracts, deliveries and returns, late fees, deposits, and availability.
- **Licensing** — issue, validate, suspend, and revoke licenses; licenses gate downloads and store purchases. Orders issue licenses with seats and expiry from the purchased tier; refunds revoke them.
- **Software and Assets** — enterprise software manager: software records with a status lifecycle and vendor-company ownership, versions with semver and platform requirements, releases with a publish flow, per-release files with sha256/sha512 checksum verification, hierarchical groups and categories, tags, and full-text discovery, behind the storefront and the downloader. Releases imported from a connected VCS repository arrive as review drafts and are published manually.

#### Sales & Commerce > Billing & Payments (5)

- **Billing / Invoicing** — invoices raised from store orders and licenses, with payment tracking.
- **Payment Gateway** — a wrapper/framework for many providers (cards, Swift, 2Checkout, HyperPay, Xendit, Mastercard, Google Pay, custom) plus bank transfer and split bills; see the payment security re-check in TODO.md.
- **Bank Transfer** — manual payment verification: proof upload, admin approval/rejection, split bills.
- **Cards** — card validation: brand/IIN rules, custom validation rules, daily counts, and settings.
- **Tax** — tax rules, tax computation with breakdowns, filings with due checks and submission, and external tax connectors.

#### Sales & Commerce > Marketing & CRM (9)

- **CRM Contacts / Leads** — contacts and leads shared by sales, support, and billing.
- **Marketing** — campaigns with A/B variants and scheduling, segments, audience sync, automated sequences with enrollments, and lead scores.
- **Newsletter** — newsletter sending through the email providers.
- **Affiliates** — affiliate program: affiliates, tracking links, clicks, conversions, commission rules with approval/void, and payouts.
- **Loyalty** — loyalty programs with earn rules, tiers, ledger entries, redemptions, and expiry.
- **Surveys** — surveys with questions, distributions, public response pages, and results.
- **Referral** — employee referral: recruitment jobs marked referral-eligible with per-job reward-rule overrides, unique token-based shareable referral links per (employee, job), a fully data-driven pipeline of admin-defined stages, reward rules as data with rewards payable through events for billing/HR to pay out, anti-gaming rules as data, and an append-only audit trail.
- **SMS Marketing** — SMS campaigns: admin-configurable SMS providers as data (a generic HTTP adapter plus a log-only test provider), plain-text templates with {{variable}} substitution and GSM-7/UCS-2 segment counting, contact lists with tags, campaigns with a draft/scheduled/sending/sent/paused/cancelled status machine, per-message delivery tracking, opt-out keywords with a per-tenant opt-out list, and an append-only audit trail.
- **Social Marketing** — social publishing: admin-defined social networks as data, connected accounts with write-only encrypted credentials, scheduled posts with a draft/scheduled/publishing/published/failed/cancelled status machine publishing once per target account, campaigns grouping posts with live post-count and metrics rollups, analytics metrics arriving only through the provider webhook or manual entry, and an append-only audit trail.

### 5. Communication & Support (16)

#### Communication & Support > Messaging (8)

- **Chat** — realtime chat, interconnected with Support Tickets and the Knowledgebase through hooks.
- **Mail Manager** — SMTP settings, mail connectors, and the default mailer used by notifications and billing.
- **Email Providers** — email provider registry (Mailchimp, Brevo, SendGrid, and others) with settings, behind the Mail Manager.
- **Email Template Builder** — visual template editor (GrapeJS) for the emails the Mail Manager sends.
- **Notifications** — a core module: system and per-user notifications with audience targeting (single user, role, department, segment, or broadcast), a notification centre with unread counts, filters and per-user channel/category/digest preferences (including digest off), per-event templates with email delivery through the configured email provider (or the Mail Manager fallback) and GrapeJS email templates, delivery tracking with retries, and listeners for the other modules' important events (orders, payments, backups, clusters, tickets, licenses, incidents, webhooks, security). Announcements compose with audience, channels, scheduling and expiry, fan out into the notification centre and the user dashboard, and can also go out as email.
- **Inbox** — message threads with assignment, replies, canned replies, snooze, merge, and stats.
- **Announcements** — announcements and changelog; audience-targeted, scheduled, expiring, and pinned; fans out into the notification centre and the user dashboard, and can go out as email.
- **Phone** — WebRTC voice and video calling over the web (internet-only): 1:1 and group calls with store-and-forward signalling (media stays peer-to-peer), call queues and routing rules as data, presence, IVR menus and auto-response scripts as data, consent-gated recording references, and AI call summaries.

#### Communication & Support > Support (3)

- **Support Tickets** — tickets created from chat, AI Support, and CRM, with knowledgebase suggestions attached; full status workflow, priorities, assignment, internal notes, customer replies, storage-backed attachments, canned responses, SLA timers with breach alerts and escalation rules, merging and linking to orders, licenses, and servers, routed through the Departments module.
- **Departments (core)** — departments CRUD, agents/staff per department linked to users and roles, routing rules (category, product, keyword, manual), per-department SLA policies, working hours and out-of-hours auto-replies.
- **Knowledgebase** — searchable articles that Chat and AI Support suggest and that tickets link back to.

#### Communication & Support > Community (3)

- **Community** — discussion community: categories, topics, posts, votes, flags, moderation actions, members, and announcements.
- **Forums** — public discussion forums: SEO-friendly slugs, threads with best answers, polls, subscriptions, reputation, and moderation.
- **Gamification** — points ledger, award rules, badges, seasons, rewards, and leaderboards.

#### Communication & Support > Meetings (2)

- **Meetings** — Zoom-like video meetings: rooms (scheduled and instant), WebRTC signalling over the WebSocket hub, screen share, in-meeting chat, recording metadata via storage, host/co-host/ participant roles, join tokens, waiting room, scheduling, history and attendance. Paid rooms check product entitlements at join. Signalling and room management are real; media transport is browser WebRTC — no media server/SFU is built in (see TODO.md).
- **Webinars** — webinars with registrations, attendance, polls, Q&A, in-webinar chat, recordings, follow-ups, analytics, and links to meetings.

### 6. Productivity & Content (21)

#### Productivity & Content > Content & Website (7)

- **Content Management** — posts, categories, and pages, with a site
- **Site Builder** — visual site building for content and pages.
- **Theme Management** — add themes through a templating engine, embed themes internally, or load them dynamically, with strict checking and screenshot support. The engine (`src/themes/engine.rs`) is Handlebars with `rust-embed`: the default theme ships in the binary from `templates/default/`, disk themes are discovered from `themes/` and loaded only after strict validation (manifest must exist and parse, the layout template must exist, theme ids reject path traversal). The dashboard (`templates/default/dashboard.hbs`, `assets/css/dashboard.css`, `assets/js/dashboard.js`) is server-rendered with light/dark mode, a sidebar, stat cards, a module grid fed live from `/api/v1/extensions`, and an activity feed that shows an honest empty state when there is no data yet.
- **Frontend** — public pages: home, sign-in, registration, forgot-password, reset-password, and sign-out, server-rendered through the theme engine with paths from the shared route registry.
- **SEO Manager** — SEO management for sites and content.
- **Portals** — customer portals with menus and public pages, plus per-user views of downloads, invoices, and licenses.
- **Studio** — custom fields: record types, field definitions with validation, values, and layouts for extending any record.

#### Productivity & Content > Documents & Files (6)

- **Documents** — document storage: records with versions, chunked upload sessions, a download allowlist with signed token downloads, quotas, and scanner hooks.
- **File Manager** — file browsing and management for uploads and assets.
- **Forms** — form builder: forms with fields and conditions, public submission pages, submissions with spam marking and remapping, and embeds.
- **Printing** — print jobs with printer profiles and paper sizes, including ESC/POS thermal output, reprint, and cancel/complete/fail flow.
- **Records** — records store behind the audit trail, with retention from config.
- **Spreadsheets** — spreadsheet workbooks with multiple sheets, each a grid of cells in A1 notation (text, number, boolean, date, empty, formula, and error values), a server-side formula evaluator with circular-reference detection, per-workbook sharing, version history, and CSV/JSON import and export.

#### Productivity & Content > Planning & Tasks (8)

- **Tasks** — task management.
- **Todos** — todo lists with todos, subtasks, attachments, shares, reminders, completion logs, and streaks.
- **Calendar** — calendar items aggregated from multiple sources, with feeds (ICS), non-working days, conflict hints, settings, and AI day summaries.
- **Resource Planner** — capacity planning across servers, clusters, and providers.
- **Workflows** — automation workflows: triggers, condition groups, actions, runs with retry, manual run/test, and tokenized webhook hooks.
- **Approvals** — multi-step approval workflows: definitions with steps and approvers, requests, decisions, delegations, escalations, and overdue tracking.
- **Appointments** — bookable services with staff availability and slots; a public booking flow, appointments, reminder rules, settings, and ICS export.
- **OKRs** — objectives and key results in cycles, with check-ins, overrides, and links to other records.

### 7. Operations & Business (23)

#### Operations & Business > Projects & Field Service (5)

- **Project Management** — projects with 15 selectable methodologies (Kanban, Scrum, Waterfall, Scrumban, Agile, Lean, XP, PRINCE2, PMBOK, CPM, CCPM, Six Sigma, Hybrid, Shape Up, SAFe), boards and columns, tasks with subtasks and dependencies, sprints, phases and gates, cycles, program increments, business cases, milestones, time tracking with billable hours, automation rules, templates, and reports, wired into the ERP bus (billing for invoiced time, notifications, audit, and search).
- **Field Service** — field operations: equipment and equipment types, work orders, dispatch board and assignments, parts, maintenance plans, templates, and time/sign-off.
- **Time Tracker** — time tracking: timers and entries, activity reporting, agent heartbeat and screenshots, rates, teams, and staff.
- **Supervision** — site supervision: assignments, inspection templates and inspections, defects with follow-ups, daily logs with sign-off, progress reports, and approvals.
- **Incidents** — incident management: incidents with timeline and responders, on-call rotations, escalation policies, auto-open rules, and postmortems.

#### Operations & Business > Manufacturing & Supply (8)

- **Manufacturing** — bills of materials with versions, work centers, work orders, production runs, stock consumption, scrap, and production reports.
- **Quality** — quality points and criteria, inspections with measurements, alerts, CAPAs, and quality blocks.
- **Procurement** — vendors, purchase orders with lines and approvals, goods receipts, and budgets.
- **Logistics** — shipments, drivers, vehicles, delivery routes, warehouses, rate tables and quotes, tracking, exceptions, and proof of delivery.
- **Properties** — property listings with plans and blocks, location search/reverse-geocoding, public pages with SVG plans and printable output, and sold-out marking.
- **PLM** — Product lifecycle management: lifecycle stages with approval gates, product versions, engineering change orders, and versioned BOM snapshots.
- **Assets** — asset register with assignments, stock levels and adjustments, stock takes, depreciation, and warranty checks.
- **Delivery Notes** — delivery-note (DO) documents generated from logistics shipments, with the shipment's items, sender, receiver, and addresses snapshotted at creation time, data-driven DO numbering, and a pure status machine (draft → issued → in transit → delivered → returned, plus cancelled) with proof-of-delivery gating on delivery, partial and full returns with rental-order linkage, and print-ready documents queued through the printing module.

#### Operations & Business > People & HR (4)

- **HR** — employees, positions, contracts, leave, attendance, payroll, documents, onboarding checklists, reviews, and HR reports.
- **Recruitment** — job postings, applicants with stage history, interviews, evaluations, offers, and hire.
- **Holidays** — holiday providers and countries, sync configs and runs, holidays with overrides, and is-holiday / non-working-day lookups.
- **Expenses** — expense categories, policies, expenses with receipts, reports, reimbursements, currency rates, adjustments, and spend analytics.

#### Operations & Business > Governance & Compliance (6)

- **Compliance** — data-protection compliance: processing records, consent records, retention policies, data-subject deletion requests, exports, breach records with timelines, and checklists.
- **KYC** — Know Your Customer: identity verification cases with AI-assisted document checks, a human admin review queue, risk levels as data, and a hash-chained audit trail.
- **Privacy** — privacy controls: field declarations, consent types and records, erasure and anonymization requests, privacy notices, and an audit trail.
- **Risk** — risk register with mitigation plans and tasks, checklist templates and instances, a risk matrix, and alerts.
- **Auditors** — external auditor accounts with scoped sessions, public share links, activity and access logs, and session revocation.
- **ESG** — sustainability reporting: admin-defined metric and reporting-framework definitions as data, reporting periods with open/closed status, metric values per period per scope with source notes and full value history, per-metric targets with direction and deadline plus exact progress math in basis points, period summaries with JSON and CSV export, and an append-only audit trail.

### 8. Education & Services (11)

#### Education & Services > Education (7)

- **Academic** — academic system: students, lecturers, study programs, subjects, class schedules and rooms, course registration with advisor approval, attendance, grades and GPA, grade appeals, transcripts, academic calendar, thesis tracking, and academic standing.
- **Admissions** — admission periods with quotas and stages; applications with document checklists, scores, decisions, results, fee invoices, enrollment, and a waitlist.
- **Curriculum** — curriculum frameworks, school levels, subjects, core and basic competencies, structures, lesson plans, assessment domains, grade descriptors, interventions, and report cards.
- **E-Learning** — courses with modules and lessons, enrollments, quizzes, learning paths, reviews, and certificates with verification, anchoring, and revocation.
- **Exams** — question banks, exams, attempts with autosave, heartbeat, and proctor signals, grading, results, settings, and paid certification exams.
- **Library** — library catalog, copies, members, loans, holds, fines, loan policies, inventory audits, and tokenized digital access.
- **Certificates** — advanced SSL certificate management via config and ACME, with renewal and lifecycle events.

#### Education & Services > Business Services (4)

- **Events** — events with venues, recurrence, ticket types, registrations, attendees, check-in, waitlist, reminders, and public event pages with ICS.
- **Contracts** — contract templates and contracts with signers, tokenized signing links, activation, renewal, termination, void, expiry checks, and an audit trail.
- **Sign** — Core e-signature: signing envelopes over documents with hashed signing tokens, signature capture (drawn, typed, or uploaded), completion certificates, and public token-gated signing flows.
- **Vault** — secure document vault: folders, documents with versions, share links, access requests with approve/deny, retention, and an access log.

149 registered modules in total, counted from `src/modules/mod.rs`
(`pub mod` lines). The list is grouped by business area; the ordering
inside each subcategory follows the original list order.

## External modules

Native Rust external modules live in `crates/external-*` as separate
workspace crates. They implement the same
`unified_core::modules::Module` contract as the built-ins (see
`crates/example-hello-module` and
`docs/WORKSPACE_AND_EXTERNAL_MODULES.md`), declare `is_core() =
false`, and are disabled like any extension. They are workspace
members but are **not wired into the server binary yet**: enabling one
takes a root `Cargo.toml` dependency entry plus one `register` line in
`src/modules/mod.rs::register_all` (see each crate's README).

- **WhatsApp Business** (`crates/external-whatsapp`, id `whatsapp`) —
  outbound text/template messaging via the Meta Graph API,
  data-driven templates, webhook verification with HMAC-SHA256
  inbound validation, message log, and conversations. Secrets
  (access token, phone-number id, verify token, app secret) come from
  environment variables only; without them messages stay queued as
  `pending_config` and are never fake-sent.
- **E-signature** (`crates/external-esignature`, id `esignature`) —
  signature requests, HMAC-signed expiring signing links, a
  hash-chained audit trail, and completion certificates. Workflow and
  evidence layer only — it does not render or modify PDF bytes.
- **Fleet** (`crates/external-fleet`, id `fleet`) — vehicles, drivers,
  trips with overlap checks, GPS positions, fuel logs with km/litre
  math, maintenance due by odometer/date, and per-vehicle cost
  rollups. Per-tenant, in-memory.
- **IoT** (`crates/external-iot`, id `iot`) — device registry with
  per-device API keys (stored as SHA-256 hashes), HTTP telemetry
  ingest with rate limits and aggregates, device twins, command queue
  with ack/expiry, threshold alert rules, and fleet health. HTTP
  ingestion only; MQTT is a documented topic mapping, no broker
  embedded.
- **BI / data warehouse** (`crates/external-bi`, id `bi`) — datasets
  that declare shape only (never SQL), extracts from
  `bi.dataset.provide` hook providers into immutable snapshots, a
  small query engine (filters, grouping, count/sum/avg/min/max),
  webhook/file destinations, CSV/JSONL exports, and cron schedules
  evaluated on the `bi.schedule.tick` hook.

Verification status: each crate was checked in an isolated scratch
workspace against a stub `unified-core` — BI (11 tests passed), fleet
(15 passed), IoT (12 passed); e-signature (10 tests) and WhatsApp
(13 tests) have tests written but no passing run was reported. The
in-tree `cargo check -p <crate>` is blocked by pre-existing
`unified-db-*` compile errors from the Diesel split (E0425 name
shadowing), not by these crates — re-run it after that fix lands.

TypeScript modules are a separate layer — see "TypeScript /
JavaScript engine" below.
