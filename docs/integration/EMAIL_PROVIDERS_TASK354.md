# Email Providers — Task 354: Catalog Expansion (13 → 56)

Task 354 expanded the built-in Mailer catalog from 13 to 56 providers,
added the `email_provider.definitions` hook, and shipped deliverability
headers on the shared send path. Tracker: web-app-software-development-
tracker, project 1, task 354.

## 1. Catalog: 56 built-in providers

Source of truth: `crates/unified-core/provider_registry_seed.json`
(email family, 56 entries) + migration `000131` (durable table sync).
Every entry states its free tier and what it is good for in `notes`;
the catalog endpoint returns it as `description`.

| Group | Count | Transport |
| --- | --- | --- |
| Existing (task 350) | 13 | 8 native API adapters, Gmail/Yahoo/Outlook OAuth2 + SMTP XOAUTH2, generic SMTP, local sendmail |
| Mailbox SMTP presets (new) | 26 | Generic `SmtpAdapter` + preset from the definition (`smtp_host`/`smtp_port`/`smtp_tls`) |
| Dual API/SMTP relays (new) | 13 | Generic `SmtpAdapter` via the relay's SMTP interface (Sender.net, SendPulse, Mailjet, Mailtrap, Elastic Email, SMTP2GO, MailerSend, ZeptoMail, SendLayer, SocketLabs, Pepipost, SMTP.com, Campaign Monitor) |
| Generic HTTP (new) | 4 | Generic `GenericHttpAdapter` driven by the definition's `http_api` (Plunk, Courier, Loops, OneSignal) |
| **Total** | **56** | |

Mailbox presets: Zoho, Zoho EU, Proton (Bridge 127.0.0.1:1025),
Yandex, Fastmail, iCloud, GMX, Mail.com, AOL, QQ, 163, 126, Sina,
Mail.ru, Posteo, Migadu, Mailfence, StartMail, Runbox, Kolab Now,
Infomaniak, OVH, Gandi, Namecheap, DreamHost, MXroute.

`affiliate_url` is empty on every built-in (never invented);
an admin may set one per provider.

### 1.1 What is NOT shipped (10 of the user's 65)

These have no direct single-message send endpoint, so shipping
them as catalog entries would violate "every entry must actually
send":

- Unisender — list-based API, no free-form send.
- Nine campaign/event marketing APIs: MailerLite, Mailchimp
  (marketing), HubSpot, Omnisend, Klaviyo, GetResponse, AWeber,
  Constant Contact, Moosend.
- Campaign Monitor DOES ship — through its Transactional SMTP
  relay (`smtp.api.createsend.com:587`, username = password =
  the same SMTP token).
- MessageBird is NOT a separate entry: it duplicates the existing
  SparkPost/Bird entry.

Follow-up proposal: dedicated multi-step campaign adapters for
the 10 (create campaign → add content → send to a list), as a
separate task.

### 1.2 Free-tier figures and their sources (Nov 2026)

User-verified, used exactly: Sender.net 15,000/mo; SendPulse
12,000/mo; Brevo 300/day; Mailjet 6,000/mo; Resend 3,000/mo;
Mailgun 100/day; SMTP2GO ~1,000/mo; MailerSend ~500/mo;
Postmark 100/mo; SendGrid 60-day trial; Gmail OAuth2.

Verified in-session via web search: Elastic Email 3,000/mo
(100/day, no card); ZeptoMail first 10,000-email credit free,
6-month credit; Mailtrap 4,000/mo; Pepipost/Netcore trial only
(no published free plan); Loops free 4,000 sends/mo to the first
1,000 contacts (vendor site); Courier free 10,000 sends/mo (third-
party comparison, Nov 2026 — stated with that caveat); Plunk free
tier conflicting in sources (3,000 vs 1,000/mo) → "check provider
site"; SocketLabs first month free (40,000), then $39.95/mo;
SMTP.com paid from $25/50,000.

Unverified figures stay marked "Free tier unverified — check the
provider site." in the definition notes.

### 1.3 Amazon SES price — resolved conflict

The original brief said $0.16/1,000; the user's list said
$0.10/1,000. Verified against the AWS pricing page + analyses:
**$0.16/1,000 is the current list price** (Essentials plan, the
default for new accounts since 21 July 2026); **$0.10/1,000 is the
legacy a-la-carte rate** for accounts already sending (switchable
in the console, takes effect immediately). The SES note says both.
The SES-specific free tier (3,000/mo for 12 months) closed to new
customers on 21 July 2026.

### 1.4 SMTP hosts / ports — verification status

Vendor-documented and used as presets: Sender.net
`smtp.sender.net:587`, SendPulse `smtp-pulse.com:587`, Mailjet
`in-v3.mailjet.com:587`, Elastic Email `smtp.elasticemail.com:587`,
ZeptoMail `smtp.zeptomail.com:587`, Mailtrap `live.smtp.mailtrap.io:587`
(production; `smtp.mailtrap.io:2525` sandbox), MailerSend
`smtp.mailersend.net:587`, SendLayer `smtp.sendlayer.net:587`,
SMTP2GO `mail.smtp2go.com:587`, Campaign Monitor
`smtp.api.createsend.com:587`, Pepipost/Netcore
`smtp.netcorecloud.net:587`.

**Not re-verified in-session** (vendor-documented only, flag if a
preset fails in the field): SocketLabs `smtp.socketlabs.com`,
SMTP.com `smtp.smtp.com`.

### 1.5 Generic HTTP endpoints — verification status

Verified from vendor docs (not live-tested — no live credentials):

- Plunk: `POST https://api.useplunk.com/v1/send`, Bearer `sk_`,
  `{to, subject, body, from, name, headers}`, response `{success}`.
- Loops: `POST https://app.loops.so/api/v1/transactional`, Bearer,
  `{email, transactionalId, dataVariables}`, `200 {success: true}`.
  Template-first: there is NO raw-HTML option — a Loops send
  requires a `template_id`, and a message with a `template_id`
  for any other provider is refused.
- Courier: `POST https://api.courier.com/send`, Bearer,
  `{message: {to: {email}, content: {title, body}, routing: {method:
  "email", channels: ["email"]}}}`, response `{requestId}`.
- OneSignal: `POST https://api.onesignal.com/notifications`,
  `Authorization: Basic <REST API key>` (raw key, NOT base64),
  `{app_id, include_email_tokens: [to], email_subject, email_body,
  email_from_name, email_from_address}`, response `{id}`. The App ID
  is stored in the connection `domain` field (documented in the
  definition notes, the settings field help, and the domain notes).

## 2. Provider hook: `email_provider.definitions`

Emitted by `list_catalog` with `{}`. A module answers with one
provider definition object or an array of them (a
`ProviderDefinition` JSON object: `id`, `display_name`,
`adapter_kind`, plus the fields the adapter needs — `smtp_host` /
`smtp_port` / `smtp_tls` for the generic SMTP adapter, `http_api`
for the generic HTTP adapter, `credentials_shape` for the settings
page). Validation: valid id, non-empty display name, non-empty
adapter kind; malformed answers are skipped; a built-in wins on id
collision. Same discipline as the widgets `widgets.widget_types`
hook.

The catalog itself now iterates `catalog()` (the registry), not
the `EmailProviderKind` enum — a provider added through the
registry appears with no code change. Hook-provided definitions
appear in the catalog with settings fields and description, but
they have no enum kind: sending through one needs a factory
registered with `register_factory_by_id` under the same id (see
`registry.rs`).

The `email_provider.settings_fields` hook is unchanged; hook-only
definitions get their fields through the same path (by-definition
variant in `handlers.rs`).

## 3. Deliverability headers

`src/modules/email_providers/deliverability.rs` — one helper,
`deliverability_headers(message, connection, smtp)`, used by the
shared send path so ALL transports emit the same headers:

- Bulk sends (`bulk: true`, including every `/send-batch` call):
  full set — `List-Unsubscribe` (mailto:sender, plus
  `, <https-url>` when the connection has `unsubscribe_url`),
  `List-Unsubscribe-Post: List-Unsubscribe=One-Click` (RFC 8058),
  `Precedence: bulk`, `X-Auto-Response-Suppress: All`, and
  `Feedback-ID: 1:unified:<connection-id-first-8>:<sender-domain>`
  (SMTP only).
- Transactional sends: no headers by default; `List-Unsubscribe`
  only when the caller opts in (`include_unsubscribe: true`).
- `deliverability_opt_out: true` suppresses every header.

The https unsubscribe URL comes from the new optional
`unsubscribe_url` on `EmailConnection` — validated as an http(s)
URL with a host and no embedded credentials at create/update
(400 otherwise). No URL is ever invented. Connections are
DashMap-only in-memory, so no migration was needed for this field.

Emission points:

- SMTP family (generic SMTP, OAuth SMTP, sendmail): shared
  `build_lettre_message` via `raw_header`.
- API adapters with a documented custom-headers mechanism:
  Mailchimp (`message.headers`, merged with Reply-To), Brevo
  (`headers`, merged with metadata), SendGrid (top-level
  `headers`), Mailgun (`h:Name` form fields), Postmark
  (`Headers` array), Resend (`headers`), SparkPost
  (`content.headers`).
- SES SendEmail API: NO custom-header support — skipped with a
  documented comment (use the SES SMTP interface instead).
- Generic HTTP: only Plunk's send API documents a `headers`
  object, so only its template carries `{deliverability_headers}`;
  Courier, Loops, and OneSignal have no documented custom-header
  field on their send endpoints.

## 4. Logos

One SVG per provider in `src/modules/email_providers/logos/`
(56 files + `sprite.svg`, 56 symbols), registered in `logos.rs`
and the sprite. The catalog returns the logo stem only, never a
URL. The 13 original logos are unchanged; the 43 new ones are
simple lettermarks (initials on a rounded rect, brand-ish
colours, Arial/Helvetica bold) — NOT trademarked artwork. This is
the one documented exception to the "no text elements" rule in
`logos.rs` and `logos/README.md` (the original logos already use
letterforms as paths). Demo copies live in `demo/assets/logos/`.

## 5. Demo

`demo/assets/demo.js` `DEMO_MAIL_PROVIDERS` is now 56 entries
(the 13 existing notes updated to the free-tier versions, the 43
new ones appended in a delimited `/* Task 354: ... */` section).
The picker renders logo + name + note (free tier + best-for) and
the provider's own settings fields. The "like PHP mail()"
wording is gone from the sendmail note and comment.

## 6. Migration 000131

`crates/unified-core/migrations/2026-11-23-000131_email_providers_task354/`:

- `ALTER TABLE provider_registry_definitions ADD COLUMN`
  `smtp_host TEXT`, `smtp_port INTEGER CHECK (1–65535)`,
  `smtp_tls TEXT CHECK IN ('starttls','implicit','none')`,
  `http_api JSONB`, `affiliate_url TEXT`.
- 13 `UPDATE`s (existing notes) + 43 generated `INSERT`s
  (`ON CONFLICT DO NOTHING`).
- `down.sql` deletes the 43 ids and drops the 5 columns; it does
  NOT revert the 13 notes (old wording is not preserved in SQL).

`docs/integration/c12_generate_registry_seed_sql.py` now emits
the 5 new columns and accepts `--ids family:id,...` (a bare id
that matches more than one family — e.g. `ovh` exists in both
`email` and `cloud` — is an error; qualify it).

No migration has ever run on live PostgreSQL.

## 7. Verification (task 354)

- `cargo check -p unified-core` — exit 0 (see the task report
  for the log tail).
- `cargo test -p unified-core --lib` — 73 passed / 0 failed.
- Root bin: `cargo check -p unified` run once; the log is under
  `~/workspace/unified-check/` — the bin has never reached exit 0
  on this VM (OOM-killed child rustc), so the email_providers
  unit tests in the bin crate are written but unrun; verified by
  rustfmt parse + review only. Do not claim the root bin passes.
- `npm run check` exit 0; `node --check demo/assets/demo.js`;
  demo static-served + touched pages curled 200.
- Greps: no Indonesian strings in code, no invented affiliate
  URLs (all empty), no free-tier number without a source/date,
  no provider entry that cannot send, no "TrayDigita" and no
  WordPress anywhere in Unified code.
