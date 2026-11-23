# C12 — registry tables program: scoping brief (task 269)

Status: SCOPING DRAFT — not started. Counts below are from inspection R1
(2026-11-22); re-measure at kickoff, the tree has changed since.

## The problem
The modularity rule is MUTLAK ("nothing hard-coded") and the tree is far
from compliant. R1 counted **156 `pub enum` declarations, 385 `matches!(…)`
occurrences, 339 `https://` literals in Rust**. The pattern everywhere is a
closed enum + 4–8 parallel `match` tables (`as_str` / `from_str` /
`display_name` / `allowed_hosts` / `capabilities_for` / `currencies_for` +
per-provider adapter + dispatch arm). Adding one provider = a code change in
5+ places; an admin cannot do it.

Worst offenders (R1):
- Payments, 12 variants (`providers.rs:14`, `allowed_hosts()` `:106`,
  `capabilities_for()` `:177`, `currencies_for()` `:333`; base URLs in
  `dispatch.rs:68-105`).
- Cloud/VPS, 17 variants (`providers/types.rs:83`).
- Email, 9 variants (`email_providers/provider_kinds.rs:14`). A real
  `register_factory` registry EXISTS (`registry.rs:37,75`) but only the 9
  built-ins are ever registered and there is no admin/config path.
- AI, 4 variants + baked models (`ai/types.rs:16,67,77` — `claude-sonnet-4`,
  `gpt-4o-mini` as defaults).
- Storage, 5 variants in core (`crates/unified-core/src/storage.rs:54`).
- VCS, 3 variants (`vcs/providers.rs:43,79`).
- Notifications: only 3 channels (`notifications/types.rs:114` — no SMS/push
  possible). Google/Facebook bespoke OAuth paths
  (`auth_manager/providers.rs:80`). Holiday provider kinds + 12-country
  Google calendar map in code. Cloudflare cache levels/record types as
  consts. Reference-ID format fixed in core (30 chars, segment sizes not
  configurable). Web3 chains closed enum.

MEDIUM items: role/permission seed matrices in code; 4 baked presets +
closed wizard steps; 35 currencies / 12 languages / CLDR plural rules as
code; ~46 single-line string vocabularies (priority, project status,
methodology lists, discount kinds…); billing `period_days()` magic
day-counts.

## Program shape
Per registry, the same migration, in priority order:
1. **Provider registries first** (payments, cloud/VPS, email, AI, storage,
   VCS): provider definitions become DATA — a registry table (or config
   document) holding id, display name, base URLs, allowed hosts,
   capabilities, currencies, credentials shape — with an admin/config path
   to add one. The existing `register_factory` pattern (email) is the model
   to copy: factories register at startup from the table, not from code.
2. **Then channels + vocabularies** (notifications, OAuth providers,
   holidays, Cloudflare consts, currencies/languages, the ~46 vocabularies):
   same treatment, smaller tables.
3. **Then seeds + magic numbers** (role/permission seeds, presets, wizard
   steps, `period_days`).

Each registry gets: the table (migration), the factory/loader, the admin
surface (or config-file path), and a test proving a new entry works without
a code change.

## Sanctioned exceptions (stay hard-coded — user decisions, do not "fix")
- Standard paper sizes (ISO 216 A0–A6, Letter, Legal, Tabloid, thermal
  58mm/80mm) — user 2026-11-22 12:00; custom sizes addable on top.
- Card-brand IIN/BIN ranges + Luhn + per-brand CVV lengths — user
  2026-11-22 12:17 ("validasi hardcoded boleh sesuai standards").
- Anything else the user has explicitly sanctioned since; ask before
  treating a new constant as an exception.

## Verification per registry
- Add-one-provider test: register a new provider purely through the admin
  surface / config, no code change, and exercise it end-to-end.
- Grep: the old closed enum / match tables for that registry are gone or
  reduced to the sanctioned defaults.
- `cargo test -p unified-core` baseline 54/0 where core is touched;
  root-bin checks need CI/Colab (OOM wall) — state plainly.

## Out of scope
- DB read path (C11 / task 271) — related but separate program.
- The extension registry (documents, task 321) — already done, same pattern.
