# external-esignature

E-signature workflow, evidence, and audit layer for the Unified
server. One struct implementing `unified_core::modules::Module`
(`id = "esignature"`, `is_core() = false`, version `0.1.0`), in the
exact shape of `crates/example-hello-module`. Layout:
`src/lib.rs` (contract docs, constants, unit tests) +
`src/types.rs` + `src/crypto.rs` (tokens, validation, hash chain)
+ `src/state.rs` (workflow operations) + `src/handlers.rs` +
`src/module.rs` (trait implementation, `register`, `module`).

## What it does

- **Signature requests** (`POST /api/v1/esignature/requests`):
  a document reference (`document_id` + `document_sha256` +
  `title`), ordered signers (name, email, role, signing order),
  expiry, and an optional callback URL. Emails are validated, at
  least one signer is required, past expiry is rejected, and
  callback URLs must be absolute http/https without credentials or
  a private/loopback IP host. Status lifecycle: `draft → sent →
  partially_signed → completed | declined | expired`. Expiry is
  computed on read — a `sent`/`partially_signed` request past its
  expiry reports `expired`; nothing is ever silently
  auto-completed.
- **Signing links** (`POST .../requests/{id}/send`): one
  HMAC-SHA256 signing URL token per signer. The token is
  `base64url(v1.<request id>.<signer id>.<expiry unix>).hex(hmac)`,
  expires with the request, and is single-purpose (one request, one
  signer). `GET /api/v1/esignature/sign/{token}` returns the signing
  ceremony payload (document hash, signer, fields to sign);
  `POST .../sign/{token}` records the signature (typed name +
  drawn-signature SHA-256 hash + IP + user agent + timestamp) and
  advances the request in signing order; `POST
  .../sign/{token}/decline` declines it. When the last signer
  signs, the request completes and a certificate is issued.
- **Certificate of completion**
  (`GET /api/v1/esignature/requests/{id}/certificate`): a JSON
  certificate (request id, document hash, signers with timestamps,
  hash-chained audit event ids, certificate hash).
  `GET /api/v1/esignature/verify/{certificate_hash}` verifies a
  certificate against its stored hash.
- **Audit trail**: every action (`created`, `sent`, `viewed`,
  `signed`, `declined`, `expired`, `completed`, `reminded`,
  `linked`) appends a hash-chained event — each event stores the
  SHA-256 of the previous event hash plus its own payload.
  `GET .../requests/{id}/audit` returns the chain;
  `GET .../requests/{id}/audit/verify` checks chain integrity and
  reports the first broken index when tampered with.
- **Reminders** (`POST .../requests/{id}/remind`): records a
  reminder, rate-limited to one per hour per request (429
  otherwise), and emits `esignature.reminder.requested` for the
  mail/notifications modules to deliver. This module never sends
  email itself.
- **Hooks**: emits `esignature.request.sent`,
  `esignature.request.completed`, `esignature.request.declined`,
  `esignature.request.expired`, `esignature.reminder.requested`;
  listens to `contracts.signed` (verified from the in-tree
  contracts module, `src/modules/contracts.rs` — the name is not in
  `unified_core::hooks::events`) and links a completed request back
  to the contract when the `contract_id` matches or the sealed
  content hash matches the request's document hash.

## What it does not do

This is the signature workflow, evidence, and audit layer. It does
**not render or modify PDF bytes**: it records that a document
(identified by its SHA-256 hash) was signed, by whom, in what
order, and when, and proves the record afterwards. Producing a
signed PDF from the certificate is a separate rendering step. It
also does not send email: reminders and sent links are emitted as
hook events for the mail/notifications modules.

## Routes

All routes live under `/api/v1/esignature/` and return the standard
success envelope (`{"data": ...}`) or the standard error envelope.
`tenant_id` is required on every tenant-scoped read (query
parameter) and write (request body); a request from another tenant
answers 404, never 403.

| Method | Path | Description |
| --- | --- | --- |
| POST | `/api/v1/esignature/requests` | Create a draft signature request |
| GET | `/api/v1/esignature/requests?tenant_id=...` | List requests for one tenant (optional `status` filter) |
| GET | `/api/v1/esignature/requests/{id}?tenant_id=...` | Get one request |
| POST | `/api/v1/esignature/requests/{id}/send?tenant_id=...` | Send a draft; returns the signing links |
| POST | `/api/v1/esignature/requests/{id}/remind?tenant_id=...` | Record a reminder (max 1/hour) |
| GET | `/api/v1/esignature/requests/{id}/audit?tenant_id=...` | Hash-chained audit trail |
| GET | `/api/v1/esignature/requests/{id}/audit/verify?tenant_id=...` | Verify audit chain integrity |
| GET | `/api/v1/esignature/requests/{id}/certificate?tenant_id=...` | Certificate of completion |
| GET | `/api/v1/esignature/verify/{certificate_hash}` | Verify a certificate |
| GET | `/api/v1/esignature/sign/{token}` | Signing ceremony payload |
| POST | `/api/v1/esignature/sign/{token}` | Record a signature |
| POST | `/api/v1/esignature/sign/{token}/decline` | Decline a request |

## Configuration

`ESIGNATURE_SECRET` (required, at least 32 characters): the HMAC
secret for signing-link tokens. There is no default and no
fallback — a missing or too-short secret makes token issuance fail
with 500. The secret is never logged and never appears in any
response. Set it in the server environment before starting:

```sh
export ESIGNATURE_SECRET="$(openssl rand -hex 32)"
```

## Wiring (server-side, done by whoever builds the server binary)

1. Root `Cargo.toml`: add `external-esignature = { path =
   "crates/external-esignature" }` under `[dependencies]` and add
   `crates/external-esignature` to `[workspace] members`.
2. **Remove the temporary standalone `[workspace]` table** from
   this crate's `Cargo.toml` (it exists only so the crate is
   checkable before wiring; a workspace member must not declare
   its own `[workspace]`). While there, switch the literal versions
   to `version.workspace = true` / `dep.workspace = true` like
   `crates/unified-calculator`.
3. `src/modules/mod.rs::register_all`: add one line in the
   external-modules section: `external_esignature::register(registry);`
4. Rebuild and restart. The module declares
   `requires = ["contracts"]` for the `contracts.signed` link-back;
   the rest of the module works without the contracts module.

## Persistence

In-memory DashMap store, authoritative for the running process
(the same pattern as the calculator module). A database migration
is the persistence target for requests, audit events, and
certificates; until it lands, a restart loses the in-memory state.

## Security notes

- No hard-coded secrets; HMAC secret from `ESIGNATURE_SECRET`
  only. Tokens expire with the request and are single-purpose.
- Tenant isolation on every read/write (404 across tenants).
- Input validation: emails, 64-hex document/signature hashes,
  callback URL allowlist (http/https only, no credentials, no
  private/loopback/link-local IP host). A hostname that *resolves*
  to a private IP is not rejected (no DNS lookup is done);
  deployments that follow callback URLs must resolve them through
  a DNS allowlist of their own.
- Client IP is taken from `X-Forwarded-For` (first entry) when
  present, else the peer address; the recorded IP is evidence, not
  an authentication signal.
- Routes rely on the server's authentication/authorization layer
  (like every other module); this crate performs no auth itself.
  Route catalog entries declare it: the tenant-scoped
  `/requests/*` routes are `RouteAuth::Authenticated`, while the
  signing routes (`/sign/{token}` GET/POST/decline) and
  `/verify/{certificate_hash}` are `RouteAuth::Public` — their
  credential is the HMAC signing token / the certificate hash
  itself, because external signers have no account.

## Verify

```sh
cd crates/external-esignature
cargo check -p external-esignature
cargo check -p external-esignature --tests
cargo test -p external-esignature
```

The standalone `[workspace]` table in `Cargo.toml` is what makes
the check above work before wiring; after wiring, run the check
from the workspace root instead.
