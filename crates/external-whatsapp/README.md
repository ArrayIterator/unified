# external-whatsapp

WhatsApp Business Cloud API connector module for the Unified server:
outbound messaging through the Meta Graph API, a data-driven template
registry, a Meta webhook (verification + HMAC-SHA256 signature check),
a per-tenant message log, and conversation summaries. All state is
kept in memory; nothing is persisted to the database.

## What it does

- `POST /api/v1/whatsapp/send` queues a text or template message for
  one tenant. When `WHATSAPP_ACCESS_TOKEN` and
  `WHATSAPP_PHONE_NUMBER_ID` are configured, a background worker
  attempts delivery via the Meta Graph API; when they are not, the
  message stays `pending_config` with a clear reason — it is never
  reported as sent.
- `GET` / `POST /api/v1/whatsapp/templates` manage the template
  registry (name, language, category, body with `{{n}}` variables).
  Variable counts are validated against the body, both at creation
  and at send time.
- `GET /api/v1/whatsapp/webhook` answers Meta's verification
  challenge (constant-time verify-token compare). `POST
  /api/v1/whatsapp/webhook` verifies the `X-Hub-Signature-256`
  HMAC-SHA256 signature *before* parsing the body, stores inbound
  messages, applies delivery-status updates, and emits the hook
  events `whatsapp.message.received` and `whatsapp.message.status`.
- `GET /api/v1/whatsapp/messages` (paginated, filter by status and
  phone) and `GET /api/v1/whatsapp/messages/{id}` expose the log.
- `GET /api/v1/whatsapp/conversations` groups messages by phone with
  the last message and an unread count.
- `GET /api/v1/whatsapp/status` reports configuration flags and
  counts only — never a secret.
- Hook listener on `store.order.paid` records a notification intent
  (ready only when a matching template *and* credentials are
  configured); it never sends a message. Note: `store.order.paid` is
  not a core event — no core module emits it yet, so the listener
  stays dormant until a store module does.

## Tenancy and security

- Every record carries `tenant_id`. It is required on every write and
  is a mandatory filter on every read; a request without it is
  rejected with 400. A message from another tenant looks exactly like
  a missing message (404). Inbound webhook messages that cannot be
  attributed to a tenant (no prior outbound message to that phone)
  are stored under `tenant_id=unassigned` and are only visible when
  filtering for that value.
- No secret, phone number, or credential is hard-coded, logged, or
  returned in a response.
- Sends are rate-limited per tenant in memory (fixed window, default
  30/minute, `WHATSAPP_RATE_LIMIT_PER_MINUTE`).
- All responses use the standard envelope (`{"data": ...}` on
  success, the standard error shape on failure) via
  `unified_core::json`, except the webhook verification GET, which
  must echo Meta's `hub.challenge` as raw text.

## Configuration (environment variables only)

| Variable | Meaning |
| --- | --- |
| `WHATSAPP_ACCESS_TOKEN` | Meta Graph API access token |
| `WHATSAPP_PHONE_NUMBER_ID` | WhatsApp Business phone number id |
| `WHATSAPP_VERIFY_TOKEN` | Token Meta sends back on webhook verification |
| `WHATSAPP_APP_SECRET` | App secret for `X-Hub-Signature-256` verification |
| `WHATSAPP_RATE_LIMIT_PER_MINUTE` | Sends per tenant per minute (default `30`, clamped 1–1000) |
| `WHATSAPP_GRAPH_API_VERSION` | Graph API version (default `v19.0`) |
| `WHATSAPP_GRAPH_API_BASE` | Graph API base URL (default `https://graph.facebook.com`) |

Values are read at module construction and re-read in `init()`.

## Module contract

Implements `unified_core::modules::Module` (`id = "whatsapp"`,
`is_core() = false`, version `0.1.0`), exposes `register(registry)`
and `module()`, and follows the same shape as
`crates/example-hello-module`. See
`docs/WORKSPACE_AND_EXTERNAL_MODULES.md` sections 3–4.

## Wiring (server-side, not done by this crate)

1. Root `Cargo.toml`: add `crates/external-whatsapp` to the workspace
   members and add `external-whatsapp = { path =
   "crates/external-whatsapp" }` to the dependencies.
2. `src/modules/mod.rs::register_all`: add
   `external_whatsapp::register(registry);` in the external-modules
   section.
3. Rebuild and restart.

## Verify

```sh
# From this crate's directory (standalone workspace until it is
# wired into the server workspace):
cargo check -p external-whatsapp
cargo check -p external-whatsapp --tests
```

## Status and limits

- Delivery against the real Meta API is **unverified by design** —
  no live call was made. The Graph API payload builder is covered by
  unit tests; the worker marks messages `sent`/`failed` from the
  real HTTP response.
- State is in-memory: queued messages, templates, and intents do not
  survive a restart. Persisting them (own Diesel migrations run from
  `init()`) is a follow-up.
- The `store.order.paid` event does not exist in
  `unified_core::hooks::events`; the listener is registered on the
  literal name and fires only when a store module emits it.
