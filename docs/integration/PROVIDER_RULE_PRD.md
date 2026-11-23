# Provider Rule PRD — Universal Provider System for Unified

**Status:** DRAFT (task 407, high priority) — audit complete 2026-11-24, Phase 1 spec complete; implementation pending.
**Date:** 2026-11-24
**Scope:** ALL modules — core (`src/modules/`) AND external (`crates/external-*/`).
**Rule (user directive, 2026-11-24):** every module that has 3rd-party support gets a
provider system. A module that can be self-hosted/self-contained STAYS a self module —
and it can also register as a provider (a self-hosted worker/gateway is a provider
alongside the cloud ones). External modules have providers too — the rule applies to
core AND external alike (user confirmed 2026-11-24).

## 0. Summary

Today providers are inconsistent: email has 55 named providers, payments 12, cloud
17 kinds, maps 3+ (OSM/Mapbox/Google) — but SMS has NO named provider at all (only a
generic HTTP adapter + a log-only test adapter) and WhatsApp is a single-vendor
connector (Meta Cloud API only, no provider concept, no Twilio). Twilio is 0 hits
repo-wide. This PRD defines one rule and one contract so every module that talks to
a 3rd-party service exposes it as named, data-driven, admin-editable providers.

## 1. The Provider Rule

1. **Any module that talks to a 3rd-party service MUST expose that service as a
   provider system.** "Provider system" = named providers, not one hard-coded
   integration and not one generic endpoint field.
2. **Providers are data, not code.** A provider is a definition: id, display name,
   family, base URL(s), capabilities, credential shape, webhook signature header,
   allowed hosts. Admins can add/disable providers; operators pick a provider and
   fill credentials. Adding a provider must not require a code deploy (seed registry
   + hooks are the extension points; native adapters are the exception, see §3).
3. **Self-hosted is a provider kind, not a special case.** A self-hosted worker or
   gateway (e.g. a self-hosted WhatsApp gateway, a self-hosted SMS gateway, a local
   SMTP relay) registers as a provider entry with its own endpoint, exactly like a
   cloud provider. A module that is itself self-contained STAYS a self module; it
   ALSO becomes addressable as a provider where another module consumes it.
4. **Credentials are write-only and encrypted.** Never logged, never returned in a
   read response, never stored in plain text. Only a fingerprint / configured-flag
   is readable.
5. **One source of truth for definitions.** The seed registry
   (`crates/unified-core/provider_registry_seed.json`) holds the definitions;
   `providers.rs`-style `expect("every <Kind> has a registry definition")` must
   hold for every named kind. A named kind without a seed entry panics at runtime —
   the seed entry ships in the same change as the kind.
6. **Logos are real marks or clean lettermarks, never raster-in-SVG.** Provenance is
   recorded per logo (public domain / trademarked mark used unaltered for
   identification / open licence). No `<image>`, no base64, no `<text>` in logos.
7. **No breaking changes to the generic path.** A generic HTTP/custom provider must
   keep working alongside named providers.

## 2. Provider Contract (what every provider system MUST have)

| # | Piece | Notes |
|---|-------|-------|
| 1 | Named kind enum | e.g. `EmailProviderKind`, `PaymentProviderKind`, `SmsProviderKind`. `as_str` / `from_str` / `all()`. |
| 2 | Adapter | One `send`/`charge`/etc. per operation. Native adapter when the vendor API shape differs from the generic adapter (form-encoded vs JSON, different auth, different response parsing). Generic adapter when the vendor fits it. |
| 3 | Credentials shape | Named fields with type (text/password), required flag, secret flag, applies-to (live/sandbox/both), description. Validated at save time. |
| 4 | Seed registry entry | family, id, display_name, base_url, sandbox_base_url (or null), webhook_signature_header (or null), allowed_hosts, capabilities map, currencies (payments only), credentials_shape, adapter_kind. |
| 5 | Provider logo | `assets` / module `logos/` + sprite rebuild + demo copy byte-identical. |
| 6 | Hooks / events | Provider definitions hook (`<module>_provider.definitions` style), settings-fields hook, provider lifecycle events (created/updated/disabled/tested). |
| 7 | Docs | Per-provider API shape, auth, credentials, webhook, generic-vs-native note. |
| 8 | Tests / checks | `cargo check -p unified-core` exit 0; kind count assertions; grep counts; no live endpoint tests. |

## 3. When to build a native adapter vs reuse the generic one

- **Native adapter** when the vendor API shape differs: form-encoded body (Twilio),
  non-JSON auth (Basic auth with AccountSID:AuthToken), vendor-specific response
  parsing (Twilio `sid`/`status`), vendor-specific webhook signature
  (Twilio `X-Twilio-Signature` HMAC-SHA1, Meta HMAC-SHA256).
- **Generic adapter** when the vendor is a plain JSON POST with bearer/api-key/basic
  auth and a standard success/error envelope.
- The generic adapter is never removed; named providers sit on top of it.

## 4. Self-hosted / worker providers

A self-hosted instance is a provider entry, not a code path:

- Kind id: `self_hosted` (or a named worker, e.g. `whatsapp_gateway`).
- Base URL: the operator's own endpoint (admin-configured, allowlisted host).
- Auth: whatever the worker accepts (api key / bearer / basic), declared in the
  credentials shape.
- The module that IS the worker (e.g. the WhatsApp module itself, a self-hosted SMS
  gateway module) stays a self module (`is_core` / `available_as_product` unchanged)
  and is ALSO consumable as a provider by other modules through the provider
  registry / hook contract.

## 5. Rollout order

| Phase | Scope | Status |
|-------|-------|--------|
| 0 | PRD + full module audit (this document, §6) | DONE (this file) |
| 1 | SMS named providers (Twilio first + majors) + WhatsApp providers (Meta default, Twilio, Zoho CPaaS, self-hosted gateway) + AI providers Muse Spark (Meta Model API) + Gemini (Google) | SPEC DONE (§7–§8 + §6.2); implementation pending |
| 2 | Follow-ups from the audit (§6.2): social_marketing named platforms, sign named e-sign providers, tax named filing providers, cloudflare provider abstraction, logistics carriers | TODO (one task per module/family) |
| 3 | Campaign/marketing adapters (the 10 email campaign APIs not shipped in 354) | Proposed, not tasked |

## 6. Module audit — 3rd-party support vs provider system

Audit method (2026-11-24, parent-verified from source): every module in `src/modules/`
(core) and `crates/external-*/` (external). "3rd-party support" = the module talks to
(or is designed to talk to) an external API/service. "Has provider system?" = named,
data-driven providers exist today. Evidence: `fn id` list (151 core module ids) +
per-module grep for `provider` / HTTP-client usage + enum reads for the candidates.
Modules with no external service at all are grouped as "n/a (internal only)".

### 6.1 DONE — has a provider system today

| Module | Kind | Providers today |
|--------|------|-----------------|
| email_providers | core | 55 named email providers (SMTP + native APIs + OAuth2) |
| payments | core | 12 named gateways (Stripe, PayPal, Xendit, Midtrans, Hyperswitch, …) |
| cloud (providers/) | core | 17 cloud kinds (AWS, OpenStack, Docker, K8s, …) |
| maps | core | Tile/geocode provider registry (OSM/Leaflet default, Mapbox, Google, admin-definable custom) |
| ai | core | `AiProviderKind`: Anthropic, OpenAI, OpenAI-compatible, **Local** (self-hosted — already matches the self-hosted rule). MISSING: Muse Spark (Meta) + Gemini (Google) — 0 hits repo-wide; see §6.2 |
| certificates | core | ACME provider definitions (Let's Encrypt, Google, ZeroSSL, Cloudflare, custom ACME) |
| panels | core | Panel kinds, config-driven (cPanel/WHM, DirectAdmin, Plesk, CyberPanel, WHMCS, Generic) |
| storage (+ storage_manager) | core | Backends: local, S3, S3-compatible, R2 (provider layer adds the root) |

### 6.2 GAP — 3rd-party support, NO provider system (follow-ups)

| Module | Kind | Gap | Phase / follow-up |
|--------|------|-----|-------------------|
| ai (missing providers) | core | Provider system EXISTS (4 kinds) but **Muse Spark (Meta) + Gemini (Google) are missing — 0 hits repo-wide** (user caught it 2026-11-24). Muse Spark IS available via the public **Meta Model API** (base `https://api.meta.ai/v1`, Responses API `POST /v1/responses`, `MODEL_API_KEY`, models `muse-spark-1.3` / `1.2` / `1.1`, plus `muse-wonder-1.0` and `muse-ember-1.0`; open-weight `Muse Spark Open` + `Muse Glimmer` Apache 2.0). Gemini API: `POST https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent`, header `x-goog-api-key`. Both verified from official docs 2026-11-24. | Phase 1 (407) — add `Muse` + `Gemini` kinds + native adapters |
| sms_marketing | core | NO named provider — generic HTTP + LogOnly only; Twilio 0 hits repo-wide | Phase 1 (this task, §7) |
| whatsapp | external | Meta Cloud API ONLY — no provider concept, no Twilio | Phase 1 (this task, §8) |
| social_marketing | core | SAME gap as SMS: `ProviderKind` is only Http + LogOnly (verified `types.rs:489`) — no named platforms (Facebook, Instagram, X, LinkedIn, TikTok) | Phase 2 |
| cloudflare | core | Single-vendor Cloudflare connector (6 HTTP files, 0 provider files) — no provider abstraction | Phase 2 |
| sign | core | `provider` is a free-text string naming the signing provider (verified `types.rs:792-806`) — no named providers, no provider behaviour | Phase 2 (DocuSign, Adobe Sign, …) |
| tax | core | Filing-provider connection exists (`types.rs:280`) but no named provider kinds found | Phase 2 (verify named kinds first) |
| logistics | core | 1 provider mention only, no named carrier kinds found | Phase 2 (verify carriers first) |

### 6.3 n/a — internal only (no external service)

The remaining ~130 core modules have no 3rd-party service: academic, admissions,
advisor, affiliates, ai_support, analytics, announcements, api_manager, apikeys,
app_sync, appointments, approvals, assets, audit, auditors, auth_manager, backup
(uses storage), bank_transfer, bi, billing, cache, calendar, cards, chat, clusters,
community, company, compliance, content, contracts, crm, curriculum, dashboards,
data_tools, delivery_notes, departments, documents, domain_verification, domains,
downloader (generic URL fetch, not a provider), elearning, email_templates,
entitlements, esg, events, exams, expenses, extensions, field_service, files, forms,
forums, frontend, gamification, geo, holidays, hr, i18n, import_export, inbox,
incidents, knowledgebase, kyc, library, licensing, load_balancer, logs, loyalty, mail,
maintenance, manufacturing, marketing, mcp, meetings, network, network_tools (DNS
tools), notifications (consumes `in_app`/`email`/`webhook` channels from other
modules — not its own providers), okrs, phone (no provider files, no HTTP files),
planner, plm, portals, pos, presets, pricing, pricing_tables, printing, privacy,
procurement, products, projects, properties, quality, records, recruitment,
reference_data, referral, rental, risk, roles, routes, saas, saas_domains,
saas_packages, saved_views, scheduler, search, security, seo, service-monitor
(monitors Unified itself), settings, site-builder, software, spreadsheets,
statuspage, studio, supervision, surveys, tasks, theme, tickets, timeline,
timetracker, todos, users, vault, vcs, web3, webhooks (generic outbound delivery),
webinars, widgets.

External crates: external-bi (internal BI: datasets/extracts/snapshots — no external
service), external-esignature (self-contained signing workflow: HMAC links, audit
trail — no vendor; follow-up: DocuSign etc. as providers), external-fleet (internal
fleet/vehicle records), external-iot (self-hosted device gateway: devices talk TO it
— it IS the provider side).

## 7. Phase 1 — SMS providers

### 7.1 Twilio (MANDATORY, first) — native adapter

Verified from Twilio's official docs (`twilio.com/docs/messaging/api/message-resource`,
2026-11-24): `POST https://api.twilio.com/2010-04-01/Accounts/{AccountSid}/Messages.json`,
encoding `application/x-www-form-urlencoded`, Basic auth (`AccountSid:AuthToken`),
body `To` + `From` (+ `Body` or `MediaUrl` or `ContentSid`), response JSON with `sid`
(`MM…`) + `status` (`queued` / `sent` / `delivered` / `read` / `failed` /
`undelivered`). Twilio does NOT fit the generic JSON adapter (form-encoded body,
Basic auth, vendor-specific response) — it needs a native adapter. Credentials:
`account_sid` (text, required) + `auth_token` (password, required, secret).
Webhook: `StatusCallback` URL per message (delivery status posts).

### 7.2 Other majors — named providers on the generic HTTP adapter

These vendors are plain JSON POSTs with bearer/api-key/basic auth, so they fit the
generic HTTP adapter — they still get NAMED kinds (named ≠ native adapter):

| Provider | API shape (verify from official docs before coding) | Auth |
|----------|-----------------------------------------------------|------|
| Vonage (Nexmo) | JSON POST to Vonage SMS API | api_key + api_secret |
| MessageBird / Bird | JSON POST, Bearer | access key |
| Plivo | JSON POST | Basic (auth id + token) |
| Infobip | JSON POST | API key header |
| Sinch | JSON POST | Basic / OAuth |
| Clickatell | JSON POST | API key |
| Indonesian gateways (Zenziva, NusaSMS, RajaSMS, …) | candidates ONLY if their HTTP API is publicly documented — verify, else skip and record | per vendor |

Each named provider gets: kind enum variant + credentials shape + seed registry entry
+ logo + docs row. The generic Http + LogOnly providers keep working unchanged.

## 8. Phase 1 — WhatsApp providers

The `whatsapp` external module gains a provider concept. Meta Cloud API stays the
default/existing provider; providers are per-tenant data/config, never hard-coded.

| Provider | API shape (verified 2026-11-24) | Auth / webhook |
|----------|----------------------------------|----------------|
| Meta Cloud API (default) | Graph API (existing) | access_token, app_secret; HMAC-SHA256 webhook |
| Twilio | Same Messages API as SMS: `POST https://api.twilio.com/2010-04-01/Accounts/{AccountSid}/Messages.json`, form-encoded; `From`/`To` use the `whatsapp:+E.164` prefix; `Body` (free-form, only inside the 24h service window) or `ContentSid` + `ContentVariables` (template, anytime); response `sid` + `status` | Basic auth (AccountSid:AuthToken); webhook `X-Twilio-Signature` = HMAC-SHA1 of (URL + sorted POST params) with the Auth Token |
| Zoho CPaaS | REAL, verified from zoho.com: `POST https://cpaas.zoho.com/v1.1/whatsapp`, JSON body (`from`, `to`, `template_key`, `merge_info`), one POST per template send; connects the operator's Meta-approved WABA | `Authorization` header token |
| Self-hosted gateway | Provider kind with the operator's own endpoint (admin-configured, allowlisted host); auth as declared in its credentials shape | per gateway |

Note: Zoho CPaaS is the renamed ZeptoMail (same rename as the email audit, 2026-11-24).
Templates stay Meta-approved in all providers (template name/language/variables as in
the existing module). Do NOT break the existing Meta Cloud API path.

## 9. Verification

- `cargo check -p unified-core --offline` exit 0.
- Full bin check: known pre-existing errors only (content/state.rs E0432,
  content/custom_types.rs E0382 — NOT fixed here, separate task).
- `grep -ri twilio` counts; provider counts before/after.
- No live endpoint tests; no migration run on live PostgreSQL.
- rustfmt NOT run (repo is not rustfmt-clean at HEAD).

## 10. Open decisions

1. SMS provider list beyond Twilio — which majors + which Indonesian gateways?
   (Proposed in §7.2; implementation verifies each from official docs.)
2. Zoho WhatsApp — RESOLVED 2026-11-24: REAL, via **Zoho CPaaS**
   (`POST https://cpaas.zoho.com/v1.1/whatsapp`, JSON, Authorization token,
   `template_key` + `merge_info`). Zoho CPaaS is the renamed ZeptoMail.
3. Provider naming: kebab-case ids (email/payments convention) everywhere.
4. Self-hosted provider discovery: how does a tenant register its own gateway
   endpoint (admin UI form vs seed entry)?
