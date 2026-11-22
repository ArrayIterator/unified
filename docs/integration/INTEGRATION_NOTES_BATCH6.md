# Integration notes — Batch 6 (Documents + Privacy + Web3)

Coordinator-verified by read-back 2026-11-22 ~11:47 WIB. These are the exact
snippets the integrator needs. Nothing in this batch touched
`src/modules/mod.rs`, `src/db/schema.rs`, `src/db/models.rs`, `README.md`,
or `TODO.md` — all of that is integrator work.

## 1. Files added (9 files, nothing else created or edited)

| File | Lines | Notes |
|---|---|---|
| `src/modules/documents.rs` | 3,333 | Module id `documents`, 32 routes, `available_as_product = false`, `requires = ["storage_manager", "roles"]` |
| `src/modules/privacy.rs` | 2,649 | Module id `privacy`, 32 routes, `available_as_product = false`, `requires = ["roles", "audit"]`, 13 unit tests |
| `src/modules/web3.rs` | 4,847 | Module id `web3`, 32 routes, `available_as_product = true`, `product_category = Infrastructure`, 7 tests (6 `#[test]` + 1 `#[tokio::test]`) |
| `migrations/2026-11-22-000064_documents/up.sql` | 233 | 7 tables |
| `migrations/2026-11-22-000064_documents/down.sql` | 10 | Drops in reverse order |
| `migrations/2026-11-22-000065_privacy/up.sql` | 253 | 8 tables |
| `migrations/2026-11-22-000065_privacy/down.sql` | 11 | Drops in reverse order |
| `migrations/2026-11-22-000066_web3/up.sql` | 288 | 7 tables |
| `migrations/2026-11-22-000066_web3/down.sql` | 13 | Drops in reverse order |

Batch totals: **10,829 Rust lines, 96 routes, 22 tables, 20 unit tests.**

## 2. Migration order and numbering state (checked 2026-11-22 ~11:47)

Apply batch 6 after `000063_ai_gating`: `000064_documents` →
`000065_privacy` → `000066_web3`. All three numbers are unique and
sequential at the time of writing.

Numbering observations for the integrator (do not blame batch 6 for these):
- `000058` is currently missing (batch 4's gamification migration had not
  landed when checked; if it appears later as `000058_gamification`, order
  is intact).
- `000100_calculator` (batch 3) is out of sequence — renumber it into the
  main sequence.
- The `000041` collision seen earlier (pricing_marketplace vs workflows)
  is GONE: batch 4 renumbered workflows to `000059_workflows`.
- The `000066` collision seen at ~11:46 (batch 5's `000066_sharing_auditor`
  vs this batch's `000066_web3`) is GONE: batch 5 renumbered its own
  directory to `000067_sharing_auditor` (observed in the live listing at
  ~11:47). No action needed from batch 6's side.

## 3. `src/modules/mod.rs` — registration snippets

The `pub mod` list is alphabetical. Insert:
```rust
pub mod documents; // after `pub mod departments;`, before `pub mod domains;`
pub mod privacy;   // after `pub mod pricing_tables;`, before `pub mod products;`
pub mod web3;      // last in the list (after `pub mod webhooks;` if present, else at the end)
```
In `register()` add the matching calls (suggested placement; exact
grouping is the integrator's call):
```rust
documents::register(registry); // near files::register (data/files section)
privacy::register(registry);   // near files::register (data/files section)
web3::register(registry);      // near mcp::register / api_manager::register (public-API surface)
```

## 4. `src/db/schema.rs` / `src/db/models.rs` — tables to add

Follow the existing Diesel conventions in those files. Columns are listed
as they appear in the migrations (types are in the up.sql files; `id` is
`UUID PRIMARY KEY DEFAULT gen_random_uuid()` unless noted).

### 000064_documents
- `documents`: id, name, module_id, company_id (FK companies, CASCADE),
  owner_id TEXT, scope (`owner`|`company`|`module`), mime, size BIGINT,
  checksum (SHA-256 hex CHECK), storage_key, current_version,
  version_count, scan_status (`pending`|`clean`|`infected`|`error`|
  `skipped`), scan_threat, created_at, updated_at
- `document_versions`: id, document_id (FK documents, CASCADE), version,
  checksum, storage_key, size, mime, uploader_id TEXT, scan_status,
  scan_threat, created_at, updated_at
- `document_upload_sessions`: id, document_id (FK, nullable — session may
  precede the document), name, module_id, company_id (FK, CASCADE),
  owner_id TEXT, total_size, expected_sha256, chunk_size, part_count,
  status, expires_at, created_at, updated_at
- `document_upload_parts`: id, session_id (FK sessions, CASCADE),
  part_index, size, part_sha256, received_at, created_at, updated_at
- `document_allowlist_rules`: id, module_id, extensions (TEXT[]),
  mime_types (TEXT[]), max_bytes, created_at, updated_at
- `document_downloads`: id, document_id (FK documents, CASCADE), version,
  user_id TEXT, via, scan_status, bytes, downloaded_at, created_at,
  updated_at
- `document_quotas`: id, scope, scope_id, bytes_limit, used_bytes,
  created_at, updated_at

### 000065_privacy
- `privacy_field_declarations`: id, module_id, field_name, purpose,
  required, created_by, created_at, updated_at (UNIQUE module_id+field_name)
- `privacy_anonymizations`: id, user_id TEXT (UNIQUE), token (SHA-256 hex
  CHECK), field_tokens (JSONB), scope_modules (TEXT[]), reason, actor,
  times_applied, anonymized_at, created_at, updated_at
- `privacy_visibility_defaults`: id, module_id (UNIQUE), visibility
  (`private`|`company`|`public`), explicit, configured_by, created_at,
  updated_at
- `privacy_consents`: id, user_id TEXT, consent_type, granted, granted_at,
  withdrawn_at, expires_at, source, created_by, created_at, updated_at
  (UNIQUE user_id+consent_type)
- `privacy_consent_history`: id, consent_id (FK consents, CASCADE),
  user_id TEXT, consent_type, action, actor, created_at
- `privacy_erasure_requests`: id, user_id TEXT, modules (TEXT[]), reason,
  status, requested_by, reviewed_by, reviewed_at, rejection_reason,
  fulfilled_at, anonymization_id (FK anonymizations, SET NULL),
  created_at, updated_at
- `privacy_notices`: id, module_id, text, version, effective_from,
  created_by, created_at, updated_at (UNIQUE module_id+version)
- `privacy_audit_entries`: id, actor, action, resource, details (JSONB),
  created_at

### 000066_web3
- `web3_networks`: id, name, kind (`evm`|`stellar`|`cardano`|`generic`),
  endpoint_url, chain_id, native_asset_symbol, explorer_url_template,
  enabled, indexer_url, verify_endpoint, api_key_encrypted,
  allow_private_endpoint, created_by, created_at, updated_at
- `web3_watched_addresses`: id, network_id (FK networks), address, label,
  user_id TEXT, company_id (FK companies, CASCADE), last_checked_at,
  created_at, updated_at
- `web3_transactions`: id, network_id (FK networks), tx_hash, from_address,
  to_address, amount_raw TEXT, amount NUMERIC(19,4), asset, status,
  confirmations, block_number, direction, watched_address_id (FK watched
  addresses), occurred_at, first_seen_at, last_checked_at, created_at,
  updated_at (UNIQUE network_id+tx_hash)
- `web3_anchors`: id, record_type, record_id, content_sha256, network_id
  (FK networks, RESTRICT — deleting a network with anchors is refused;
  rationale is documented in the migration header), status, tx_hash,
  confirmations, block_number, requested_by, company_id (FK, CASCADE),
  submitted_at, confirmed_at, created_at, updated_at
- `web3_wallets`: id, user_id TEXT, company_id (FK, CASCADE), network_id
  (FK networks), address, label, verification_status, challenge_nonce_hash,
  challenge_expires_at, verified_at, created_at, updated_at (UNIQUE
  user_id+network_id+address)
- `web3_balances`: id, wallet_id (FK wallets, CASCADE), network_id (FK
  networks), address, asset, amount_raw TEXT, amount NUMERIC(19,4),
  decimals, checked_at, created_at, updated_at
- `web3_settings`: id, scope, public_ledger_enabled, allowed_private_hosts
  (TEXT[]), allowed_extra_hosts (TEXT[]), anchoring_relay_url,
  anchoring_key_encrypted, max_poll_transactions, created_at, updated_at

## 5. Audit module — `AUDITED_EVENTS` additions

Add these event names so the actions land in the main audit log:
`privacy.anonymized`, `privacy.consent.changed`, `privacy.erasure.requested`,
`privacy.erasure.fulfilled`, `privacy.erasure.approved`,
`privacy.erasure.rejected`, `privacy.visibility.changed`,
`privacy.fields.changed`, `privacy.notice.published`,
`privacy.redaction.used`, `user.anonymized`,
`web3.network.private_endpoint_allowed`.

## 6. Roles — capabilities to exist

- `documents:read`, `documents:write`, `documents:admin`
- `privacy:read`, `privacy:write`, `privacy:admin`
- `web3:read`, `web3:write`, `web3:admin`

The admin role passes today via its `"*": ["*"]` grant; no other role
grants them yet. Add them to the appropriate seeded roles.

## 7. Scheduler — one arm to add

`web3.poll`: call `web3::poll_watched_addresses(&web3_state,
&encryption_secret).await` on the chosen interval, where
`encryption_secret` is the app secret used to decrypt stored provider API
keys. Get the state via `Web3Module::state()`. The exact snippet is also
in a doc comment above `poll_watched_addresses` in `src/modules/web3.rs`.
No scheduler file was edited by this batch.

## 8. API Management — public ledger rule

`GET /api/v1/web3/public/ledger` is default-deny behind the web3 module
setting `public_ledger_enabled` (admin-only `PATCH /settings`). The API
Management module's allowed-public-access controls are the outer gate and
must also allowlist this route. All other batch-6 routes are authenticated
and versioned via `RouteInfo` + `route_version_support` (default All).

## 9. Cross-module event wiring (hook subscriptions for the integrator)

- **Documents → Storage Management:** documents checks quota by emitting
  `storage.object.upload_requested` with payload
  `{"module": "documents", "owner", "company_id", "key_hint", "bytes",
  "mime"}` and honours `quota_exceeded` (→ 409 + `document.quota.exceeded`).
  When no handler answers it falls back to its local quotas. No edit to
  `storage_manager.rs` was made by this batch.
- **Privacy → Compliance (subscribe, do not edit `compliance.rs`):**
  - `compliance.consent.granted` — `{"user_id", "consent_type",
    "consent_id", "actor_id", "expires_at"}` (nullable)
  - `compliance.consent.withdrawn` — same payload
  - `compliance.erasure.requested` — `{"user_id",
    "erasure_request_id", "modules": [...], "actor_id", "reason"}`
  - `compliance.erasure.fulfilled` — `{"user_id",
    "erasure_request_id", "modules": [...], "anonymization_id", "token",
    "actor_id"}`
- **Privacy → Vault / Files (hook-only):**
  - `user.anonymized` — `{"user_id", "token", "scope_modules": [...],
    "actor_id", "reason", "already_anonymized": bool}` — consumers should
    drop or re-key stored personal data for `user_id` in the listed
    modules, using `token` only as the replacement reference, never to
    recover originals.
  - `privacy.erasure.fulfilled` — `{"user_id", "erasure_request_id",
    "modules": [...], "anonymization_id"}` — same handling trigger, with
    the request id for traceability.
  - `privacy.redaction.used` is informational only; no consumer action.
- **Privacy → Audit:** privacy emits hook events for every action (see §5);
  the audit module only records events in its `AUDITED_EVENTS` list.
- **Web3 events emitted:** `web3.transaction.detected`,
  `web3.anchor.requested`, `web3.anchor.confirmed`, `web3.anchor.failed`,
  `web3.wallet.linked`, `web3.wallet.unlinked`,
  `web3.network.private_endpoint_allowed`. Web3 listens to no events.
- **Documents events emitted:** `document.uploaded`,
  `document.version.created`, `document.downloaded`, `document.deleted`,
  `document.scan.completed`, `document.quota.exceeded`. Documents listens
  to nothing.

## 10. Environment / settings keys

- `UNIFIED_PRIVACY_ANONYMIZATION_KEY` (≥16 chars) — HMAC key for
  anonymization tokens. Without it, tokens are stable only for the process
  lifetime (key generated in memory at startup); rotating the key
  intentionally orphans old tokens; there is no re-keying path yet.
- Web3 provider API keys and the anchoring service key are write-only
  (HMAC-keystream encrypted, modelled on `ai.rs`); only
  `api_key_configured` / `anchoring_key_configured` booleans are ever
  returned or logged.
- Web3 settings: `anchoring_relay_url` + anchoring service key must be
  configured by an admin or `POST /anchors/{id}/submit` returns 422. The
  relay (which sends the on-chain transaction) is external to the module.

## 11. Security rules (absolute — restated for the integrator)

- **Web3:** the server NEVER stores private keys and NEVER signs
  transactions with user funds. There is no private-key field, column, or
  config key anywhere in batch 6. Any private-key-looking value (64-hex
  with/without `0x`, WIF, PEM block, `private_key` naming) is rejected at
  validation (`looks_like_private_key` / `reject_private_key_like` in
  `web3.rs`, applied to provider API keys and the anchoring key).
  Anchoring uses a dedicated anchoring-only service key.
- **Documents:** signed download URLs use HMAC-SHA-256 over
  `<document-id>:<expiry>`, verified with constant-time comparison.
  `GET /download/{token}` is the one route without header auth — the HMAC
  token itself is the capability; the scan gate and download logging still
  apply.
- **Privacy:** anonymization tokens are HMAC-SHA-256, one-way, never
  reversible; the key is write-only and never stored/logged/returned.

## 12. Honest limitations / OPEN list

- `cargo check` / `cargo test` / `cargo fmt` NEVER ran in this VM (no
  toolchain, crates.io blocked). Compilation is UNVERIFIED for all three
  modules. Riskiest unverified surfaces: web3's `async-trait` provider
  trait + `reqwest` usage, documents' hand-rolled constant-time HMAC
  comparison (no `subtle` crate in Cargo.toml), privacy's HMAC key
  handling.
- No live testing: no HTTP run, no PostgreSQL execution of any of the
  three migrations, no end-to-end chunked upload, scan, signed-URL
  redemption, anchor submission, or provider call against a real
  RPC/Horizon/Blockfrost/Koios endpoint.
- **Per-process keys do not survive restart:** the documents signed-URL
  HMAC key is a per-process random 32-byte key (signed URLs issued before
  a restart stop working after it), and the privacy anonymization key
  falls back to an in-memory generated key when the env var is unset
  (tokens do not reproduce after a restart). Both are documented in the
  module headers; a persistent-key design is integrator follow-up.
- **Web3 — EVM signature verification gap:** no secp256k1/k256 crate in
  `Cargo.toml`, so in-process `ecrecover` is impossible. EVM
  `verify_message` delegates to an admin-configured, allowlisted
  verification endpoint; with none configured it returns 422 saying so
  plainly. Adding a secp256k1 dependency (integrator decision) would let
  this be implemented natively.
- **Web3 — Cardano verification is lookup-only:** confirms the address
  exists/active but does not prove control; such wallets land in
  `lookup_only`, never `verified`. A signed-message proof for Cardano is
  future work. Stellar verification proves ownership via a transaction
  from the address whose memo carries the challenge nonce.
- **Web3 — EVM address history needs an indexer endpoint**
  (Etherscan-style); without one, `get_transactions` returns 422. EVM
  token balances are NOT implemented (native asset only).
- **Web3 — SSRF limitation:** the hostname allowlist checks names/IP
  literals only; a hostname resolving to a private IP (DNS rebinding) is
  a documented limitation.
- **Documents — no thumbnail rendering** (explicit follow-up, noted in the
  module docs). Preview metadata only (mime/size/checksum/image dimensions
  from header bytes).
- **Privacy — `redact_text` treats any ≥10-digit run as an ID number**
  (a bare 12-digit phone with no separators redacts as an ID, not a phone
  — still redacted, just a different mask shape). Consent expiry is
  enforced at read time (derived status), not by a background sweeper.
- **Privacy — erasure fulfillment only anonymizes within this module's
  records and emits events;** each owning module must answer
  `user.anonymized` / `compliance.erasure.fulfilled` to erase its own
  copies (see §9).
- **Documents — `can_read` for non-owner company/module-scoped documents
  requires `documents:admin`**, because request headers carry no company
  id to enforce scope precisely; the scope is still recorded for a
  future company-aware caller.
- One of B1's own internal exec checks exited non-zero during its run;
  its report is complete and coordinator read-back passed on every count,
  so this is recorded as a builder-side check hiccup, not a defect.

## 13. README / TODO lines for the integrator

- README: module count +3 (documents, privacy, web3), file count +9,
  Rust lines +10,829, routes +96, tables +22, migrations 000064–000066.
  Web3 is sellable (`available_as_product = true`, category
  Infrastructure); documents and privacy are core utilities (not sellable).
- TODO: record this batch (documents / privacy / web3) with the OPEN list
  in §12, the per-process-key caveat, and the EVM verification gap.
- Do NOT renumber batch 6 migrations: 000064/000065/000066 are unique and
  sequential as written.
