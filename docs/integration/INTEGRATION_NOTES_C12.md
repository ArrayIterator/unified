# C12 — provider registries: integration notes (task 269)

Status: PROVIDER-REGISTRIES SLICE DELIVERED 2026-11-23. The full program
(channels, vocabularies, seeds, magic numbers) cannot all land in one
pass; this note covers the delivered slice only. The remainder is
listed as follow-ups at the end, with exact tracker payloads (the
tracker updates themselves are blocked in this worker — see §7).

## 1. What was delivered

Provider definitions are DATA now, not code. One registry table
(`provider_registry_definitions`, migration 000127) holds every
provider for every family, and one in-memory registry
(`unified_core::provider_registry`) is the runtime view of that table.

Single source of truth: `crates/unified-core/provider_registry_seed.json`
(50 built-ins). The Rust registry loads it with `include_str!`; the
migration seed SQL is generated from the same file by
`docs/integration/c12_generate_registry_seed_sql.py` (regenerate, do
not hand-edit the INSERT block).

Per-registry outcome:

| Family | Built-ins | Attribute tables removed | Factory / add-one path | Test |
|---|---|---|---|---|
| Payments (12) | xendit, hyperpay, 2checkout, card, mastercard, google_pay, apple_pay, paypal, stripe, midtrans, custom, bank_transfer | `display_name`, `allowed_hosts`, `capabilities_for`, `currencies_for`, `settings_fields_for`, `dispatch::base` (all base URLs) | `catalog()` + `PaymentProviderKind::definition()`; new id runs through the generic `custom` adapter | `add_one_payment_provider_without_code_change` (payments/providers.rs) — registers `acme_pay`, proves catalog + capabilities + hosts + currencies + credential fields resolve from data |
| Email (9) | mailchimp, brevo, sendgrid, mailgun, postmark, ses, resend, sparkpost, smtp | `display_name`, `allowed_hosts`, `capabilities_for`, `settings_fields` (full expanded lists incl. common sender/webhook/test-mode fields); `has_fixed_hosts` and `has_native_sandbox` derived from data | Existing `register_factory` pattern EXTENDED: `register_factory_by_id` / `build_by_id` / `registered_ids` (email_providers/registry.rs) | `add_one_email_provider_without_code_change` (provider_kinds.rs) + `build_by_id_dispatches_to_a_registered_definition_factory` (registry.rs) |
| Storage (5) | local, s3, r2, google_drive, sftp | `StorageKind::display_name` (new, data) + capabilities via `definition()`; the name-keyed `StorageRegistry` already existed | `StorageTargetConfig.kind_id` (new, overrides `kind`) + `register_storage_kind_factory` / `provider_from_config` consults the factory map first | `add_one_storage_provider_without_code_change` (storage/config.rs) — registers `acme_store` definition + factory, builds a target by `kind_id` only; unknown id fails loudly |
| VCS (3) | github, gitlab, bitbucket | `display_name`, `default_api_base`, `webhook_signature_header`, `auth_scheme`, `auth_header_name/value` | `catalog()` + `Provider::definition()`; `api_style` recorded on the definition (`github`/`gitlab`/`bitbucket`/`generic`) | `add_one_vcs_provider_without_code_change` (vcs/providers.rs) — registers `gitea`, proves base URL + auth scheme + webhook header resolve from data |
| AI (4) | anthropic, openai, openai_compatible, local | `display_name`, `default_base_url`, `default_model`, `allowed_hosts`, `key_required`; the baked models `claude-sonnet-4-20250514` / `gpt-4o-mini` are `models` + `default_model` data now; new `models()` accessor | Same extension as email: `register_factory_by_id` / `build_by_id` / `registered_ids` (ai/providers.rs) | `add_one_ai_provider_without_code_change` (ai/types.rs) + `build_by_id_dispatches_to_a_registered_definition_factory` (ai/providers.rs) |
| Cloud/VPS (17) | hetzner, ovh, tencent, huawei, aws, google, virtualizor, docker, kubernetes, redshift, digitalocean, linode, openstack, proxmox, kvm, xen, custom | `display_name`, `allowed_hosts`, `capabilities_for`; `has_fixed_hosts` derived from the host list | `catalog()` + `ProviderKind::definition()`; new id runs through the generic `custom` adapter (wiring by id is a follow-up, F5) | `add_one_cloud_provider_without_code_change` (providers/types.rs) — registers `acme_cloud`, proves catalog + capabilities + hosts resolve from data |

Core registry (`crates/unified-core/src/provider_registry.rs`,
exported from `unified_core::lib`): `ProviderDefinition`
(family, id, display name, base/sandbox URLs, `api_style`,
`auth_scheme`, `webhook_signature_header`, allowed hosts,
capabilities JSON, currencies, credentials shape, models,
default model, `adapter_kind`, `needs_endpoint`, `needs_delegate`,
notes, enabled, builtin), `ProviderRegistry` (register/get/list/
remove/set_enabled), `load_json` / `load_json_file` (config-document
path), `load_from_rows` (DB-row loader hook), `provider_registry()`
global seeded with built-ins. Built-ins cannot be removed, only
disabled. Ids are validated (`[a-z0-9][a-z0-9_-]*`, max 64) so a
definition id can never smuggle a path, URL, or SQL fragment into a
lookup. Definitions never carry credential *values*, only the shape.

### How to add one provider (admin/config path, no code change)

1. Register the definition — JSON config document
   (`ProviderRegistry::load_json` / `load_json_file`, e.g.
   `config/provider-registry.json`), or a row in
   `provider_registry_definitions`, or `provider_registry().register(..)`
   at startup:
   `{"family": "payments", "id": "acme_pay", "display_name": "Acme Pay",
   "base_url": "https://api.acme.test", "allowed_hosts": ["api.acme.test"],
   "capabilities": {"charge": true, "refund": true},
   "currencies": ["USD", "IDR"], "adapter_kind": "custom"}`
2. If the provider needs a wire protocol no generic adapter covers,
   register its factory once at startup (`register_factory_by_id`
   for email/AI, `register_storage_kind_factory` for storage).
   Payments/cloud new ids run through the generic `custom` adapter
   with no factory step.
3. Verify it appears in the family `catalog()` and its capabilities /
   hosts / currencies resolve from the definition (each family's
   `add_one_*` test is the executable form of this check).

## 2. Migration 000127

`crates/unified-core/migrations/2026-11-23-000127_c12_registry_tables/`
(`up.sql` + `down.sql`; no existing migration edited).

- Table `provider_registry_definitions`: `id` UUID PK, `family`
  CHECK (`payments`,`email`,`storage`,`vcs`,`ai`,`cloud`),
  `provider_id` CHECK `^[a-z0-9][a-z0-9_-]{0,63}$`,
  `display_name`, `base_url`, `sandbox_base_url`, `api_style`,
  `auth_scheme`, `webhook_signature_header`, `allowed_hosts` JSONB,
  `capabilities` JSONB, `currencies` JSONB, `credentials_shape`
  JSONB, `models` JSONB, `default_model`, `adapter_kind`,
  `needs_endpoint`, `needs_delegate`, `notes`, `enabled`,
  `builtin`, timestamps; `UNIQUE (family, provider_id)`; indexes
  on family, (family, enabled), adapter_kind.
- Seed: 50 `INSERT ... ON CONFLICT (family, provider_id) DO
  NOTHING` rows (payments 12, email 9, storage 5, vcs 3, ai 4,
  cloud 17), generated from the seed JSON — idempotent, never
  overwrites an admin-edited row on re-run.
- `down.sql` drops the table (custom rows are NOT recoverable from
  the seed — export them before rolling back).

## 3. What the enums still are (deliberate)

The per-family enums (`PaymentProviderKind`, `EmailProviderKind`,
`StorageKind`, `Provider`, `AiProviderKind`, cloud `ProviderKind`)
remain as stable identities for providers with native adapter code.
What changed: every *attribute* table on them is gone. Only the
identity functions remain (`as_str` / `from_str`, incl. legacy
aliases such as `2co`, `googlepay`, `banktransfer`), plus:

- Kept as code (protocol/behaviour, not attributes): payments
  adapter dispatch + per-provider `*_charge` functions; email/AI
  adapter structs; VCS `releases_path` / `tags_path` /
  `parse_release_list` / `parse_tag_list` shapes; storage
  `provider_from_config` native constructors + `validate()`
  per-kind required-field checks; cloud adapter structs +
  `validation.rs` required-credential checks (C3 owns that file).
- Kept as code (prose): email `domain_notes` / `webhook_notes`
  (also copied into the seed `notes` field for the follow-up that
  reads them from data).
- The `&'static` bridge: the old public methods return `&'static`
  data, so each family builds a `OnceLock` attribute cache once
  from registry data (strings leaked once via
  `provider_registry::leak_str` / `leak_str_slice`). New code reads
  the owned `ProviderDefinition` directly. Consequence: a builtin
  overridden *after* first use does not change the cached
  `&'static` view until restart; the string-keyed API always reads
  live. Documented here so nobody is surprised.

## 4. Verification (this run)

- `rustfmt --edition 2024 --emit stdout` (parse): OK on all 11
  changed/new `.rs` files (provider_registry.rs is also
  `--check`-clean). Remaining `--check` diffs in the edited files
  are pre-existing debt (unsorted `use` blocks, double blank
  lines, long pre-existing lines) — no file was reformatted.
- Seed JSON validates: 50 providers, counts 12/9/5/3/4/17,
  unique `(family, id)`, all ids pass the registry id rule.
- Migration seed: 50 INSERTs, quotes balanced, family counts match.
- Grep (after): payments `providers.rs` keeps only `as_str` /
  `from_str` matches; `dispatch.rs` has zero `https://` literals;
  email keeps only identity + prose-note matches; AI keeps only
  identity matches (+ unrelated DraftKind/DraftStatus vocabularies);
  cloud keeps only identity matches (+ per-connection custom-config
  override, not a provider table); VCS keeps only `parse`/`as_str`.
- Tests written: 28 new in total — 16 in core (14 in
  `provider_registry.rs`, 2 in `storage/config.rs`) and 12 in the
  root bin (2 payments, 2 email kinds, 1 email registry, 2 VCS, 2
  AI types, 1 AI registry, 2 cloud); one `add_one_*` test per
  family plus core-registry tests on JSON loading, removal rules,
  bad ids, and DB-row override. NOT RUN: `cargo test` was not run
  in this worker (several workers share this VM; the coordinator
  runs lib-crate checks centrally; root-bin tests are unrun on this
  VM per the OOM wall). Baseline to re-confirm: `cargo test -p
  unified-core` was 54/0 before this change; this change adds 16
  core tests on top of that baseline (root-bin tests are unrun on
  this VM per the OOM wall).

## 5. Sanctioned exceptions — untouched

Standard paper sizes and card-brand IIN/BIN ranges + Luhn +
per-brand CVV lengths were not touched (user decisions).

## 6. What remains (follow-ups)

- F1 — Per-family HTTP admin CRUD for the registry: endpoints to
  list/create/disable/delete definitions and register factories,
  wired to each family's `catalog()`; catalog handlers currently
  still read the enum lists. (Also the startup call that loads
  `config/provider-registry.json` when present.)
- F2 — DB-row startup loader: read `provider_registry_definitions`
  at boot via `load_from_rows`, replacing the embedded seeds (seed
  stays as fallback when the table is empty/unreachable).
- F3 — Email/AI connection model `kind_id`: let a connection carry
  a registry definition id so `build_by_id` works end-to-end
  (today the factory path exists but the connection model still
  keys on the enum; `store.rs` is C11-owned — coordinate with task
  271).
- F4 — VCS generic `api_style` paths: drive `releases_path` /
  `tags_path` / response parsing from the definition's `api_style`
  so a newly registered VCS provider works end-to-end without
  protocol code.
- F5 — Cloud generic-adapter factory by definition id: wire the
  `Custom` adapter to a registry id (like storage's
  `register_storage_kind_factory`) so a new cloud provider can
  create/scale/delete.
- F6 — Program step 2, channels + vocabularies: notifications
  channels, OAuth providers, holidays, Cloudflare consts,
  currencies/languages, the ~46 single-line vocabularies — same
  treatment, smaller tables.
- F7 — Program step 3, seeds + magic numbers: role/permission
  seed matrices, presets, wizard steps, billing `period_days`.

## 7. Tracker status (BLOCKED — needs the parent)

This worker's toolset has no `artifact.invoke_action` /
`createtask` (the artifact namespace here exposes only
restore/share/unshare), so no tracker change was made from here.
Parent/coordinator to apply, project id 1
(web-app-software-development-tracker):

- Task 269 → In progress (`updatetask {"id": 269, "status_id": 2}`),
  then Done (`status_id: 4`) for the delivered slice only, after
  the coordinator's verification run.
- Create F1–F7 as new tasks (createtask, project_id 1, normal
  priority), titles: "C12 follow-up: per-family HTTP admin CRUD
  for the provider registry", "C12 follow-up: DB-row startup
  loader for provider_registry_definitions",
  "C12 follow-up: Email/AI connection model kind_id",
  "C12 follow-up: VCS generic api_style release/tag paths",
  "C12 follow-up: cloud generic-adapter factory by definition id",
  "C12 follow-up: channels + vocabularies registries (program
  step 2)", "C12 follow-up: seeds + magic numbers registries
  (program step 3)". Record the created ids in the 269
  description.
- Note the honest caveats in §3–§4 in the 269 description when
  closing: tests written but not run here; DB-row loader and HTTP
  admin CRUD are follow-ups, not in this slice.

## 8. Files changed

- NEW `crates/unified-core/src/provider_registry.rs` (+ tests)
- NEW `crates/unified-core/provider_registry_seed.json`
- NEW `crates/unified-core/migrations/2026-11-23-000127_c12_registry_tables/{up,down}.sql`
- NEW `docs/integration/c12_generate_registry_seed_sql.py`
- NEW `docs/integration/INTEGRATION_NOTES_C12.md` (this file)
- `crates/unified-core/src/lib.rs` (one `pub mod` line)
- `crates/unified-core/src/storage/config.rs` (kind `definition()` /
  `display_name()`, `kind_id`, factory map, tests)
- `src/modules/payments/providers.rs`, `src/modules/payments/dispatch.rs`
- `src/modules/email_providers/provider_kinds.rs`, `src/modules/email_providers/registry.rs`
- `src/modules/vcs/providers.rs`
- `src/modules/ai/types.rs`, `src/modules/ai/providers.rs`
- `src/modules/providers/types.rs`

NOT touched (other workers' boundaries): `validation.rs` / `auth.rs`,
`* /state.rs` + `*/store.rs`, `src/modules/documents/*`,
`src/modules/data_tools`, any migration other than 000127.
