# Reference Data — shared single-source-of-truth lists (task 361)

Status: built 2026-11-23. No migration (all four lists are compiled-in
reference data and static assets; no table backs them).
Module id: `reference_data` (`src/modules/reference_data.rs` +
`src/modules/reference_data/`).

## What it is

The SINGLE place the platform reads its four shared reference lists
from. No module keeps its own copy of any of them. The lists are
platform-shared and NEVER tenant-scoped: identical for every tenant,
read-only, no per-tenant rows, no admin editing at runtime. A list
changes only by changing its single source in code.

| List | Single source | Entries | Consumers import |
| --- | --- | --- | --- |
| Countries (ISO 3166-1) | `unified_core::reference_data::countries` | 249 | `unified_core::reference_data::countries::{builtin_countries, country_info}` |
| Currencies (ISO 4217) | `unified_core::reference_data::currencies` | 169 | `unified_core::reference_data::currencies::{builtin_currencies, currency_info, format_currency}` |
| Timezones (IANA) | `settings::definitions::IANA_TIMEZONES` | 420 | `crate::modules::settings::definitions::IANA_TIMEZONES` |
| Languages (ISO 639-1) | `i18n::languages::builtin_languages` | 42 | `crate::modules::i18n::languages::{builtin_languages, is_builtin_language, language_info}` |

## 1. Countries + flags (new)

There was no country list before this task: the geo module only
resolves countries from IP lookups (MaxMind / admin overrides) and
never owned a list, so nothing had to be migrated.

- Registry: `crates/unified-core/src/reference_data/countries.rs`
  (`ISO_3166_COUNTRIES`, `CountryInfo { code, numeric_code, name,
  flag_4x3, flag_1x1 }`). Names are CLDR display names (short common
  names: "United States", not the formal "United States of America").
- Flag files: `assets/icons/flags/4x3/<code>.svg` and
  `assets/icons/flags/1x1/<code>.svg` (lowercase alpha-2 code), served
  at `/assets/icons/flags/...` by the existing `/assets/{path:.*}`
  route. Both variants come from flag-icons (lipis/flag-icons, MIT
  license — see `assets/icons/flags/LICENSE.flag-icons`, copyright (c)
  2013 Panayiotis Lipiridis). 271 files per variant: 249 ISO countries
  plus 22 non-ISO entities flag-icons ships (`eu`, `gb-eng`, `gb-sct`,
  `gb-wls`, `arab`, `asean`, `cefta`, `un`, ...). The extras stay on
  disk for direct use only; they are NOT in the registry.
- Preview: `assets/icons/flags/PREVIEW.md` lists all 249 countries
  with both variants.
- HTTP: `GET /api/v1/reference-data/countries`,
  `GET /api/v1/reference-data/countries/{code}` (404 for unknown codes).

## 2. Currencies (consolidated onto the central catalogue)

A central list already existed (`i18n/currencies.rs`) but it was a
35-code display table with no numeric codes. It is now the full
active ISO 4217 set (169 codes), moved to unified-core so external
modules can consume it too, with columns (code, numeric code, name,
symbol, minor digits):

- Catalogue: `crates/unified-core/src/reference_data/currencies.rs`
  (`ISO_4217_CURRENCIES`, `CurrencyInfo { code, numeric_code, name,
  symbol, minor_digits }`, `currency_info`, `format_currency`,
  `is_zero_decimal_currency`, `currency_minor_digits`). Names are the
  ISO 4217 English names; symbols are conventional display symbols
  and are NOT part of ISO 4217. Withdrawn codes are absent (HRK —
  euro 2023; BGN — euro 2026; ANG — replaced by XCG 2025);
  supranational / funds codes (XAU, XAG, XDR, XBA–XBD, XTS, XXX, CHE,
  CHW, COU, MXV, UYW) are present. Numeric codes and minor digits
  were cross-checked against the ISO 4217 columns of the
  datasets/country-codes table during generation.
- `i18n/currencies.rs` is now a thin re-export of the unified-core
  catalogue: the `GET /api/v1/i18n/currencies[/<code>]` endpoints and
  every existing caller keep working unchanged.
- Zero-decimal behaviour is derived from the catalogue, not from
  local lists: `unified_core::db::money::is_zero_decimal_currency`
  delegates to `catalogue minor_digits == 0`, and the local copies in
  analytics, billing, products, store, procurement, tax, pricing,
  bank_transfer and payments (`is_zero_decimal`) all delegate to it.
  The exponent-0 set is BIF, CLP, DJF, GNF, ISK, JPY, KMF, KRW, PYG,
  RWF, UGX, VND, VUV, XAF, XOF, XPF. The pricing
  `ZERO_DECIMAL_CURRENCIES` const was removed (no usages outside
  pricing itself).
- HTTP: `GET /api/v1/reference-data/currencies`,
  `GET /api/v1/reference-data/currencies/{code}` (404 for unknown codes).

Behaviour changes from the consolidation (all ISO-correct, all
flagged here rather than silently shipped):

- Seven currencies became zero-decimal in the shared money helpers
  (`db::money`, pricing, payments, analytics, billing, products,
  store, procurement, tax): BIF, DJF, GNF, KMF, PYG, RWF, VUV. They
  were already correct in bank_transfer; the other helpers treated
  them as two-decimal (amounts x100). The payments adapter previously
  listed only JPY, KRW, VND, CLP.
- `format_currency("IDR", ...)` now renders `Rp15000.00` instead of
  `Rp15000`: IDR is 2 minor digits per ISO 4217 (the old 35-code
  table listed 0). The money-math helpers already treated IDR as
  two-decimal, so only this display helper changes.
- Zero-decimal lookups are now case-insensitive (the catalogue
  normalises the code). One pricing test was updated for this
  (`"jpy"` now resolves as zero-decimal).
- KNOWN DEVIATION, NOT consolidated: `properties::listeners`
  treats MGA as zero-decimal, but ISO 4217 lists the Malagasy ariary
  with 2 minor digits. Its local list and test stay as-is until the
  coordinator confirms the MGA behaviour change for property
  purchases. Do not copy that list anywhere else.

What was deliberately NOT rewritten: the per-module currency
*format* validators (`validate_currency` / `valid_currency` /
`require_currency` in bank_transfer, pricing, elearning,
properties, contracts, events, bi, manufacturing, analytics,
logistics, admissions, hr, assets, pos, timetracker) only check
that a code is three uppercase letters — they are shape checks, not
lists, so there is nothing to consolidate. Any future change that
makes validation catalogue-strict (rejecting syntactically valid but
non-ISO codes) is a separate decision. The ad-hoc `"USD"` defaults
in bank_transfer, elearning, pos and properties settings are default
*values*, not lists; they now resolve against the central catalogue.

## 3. Timezones (already existed — exposed, not rebuilt)

The full IANA list (420 zones, UTC first) already lived in
`src/modules/settings/definitions.rs` as `IANA_TIMEZONES`. No new
list was created and none is duplicated anywhere:

- `GET /api/v1/reference-data/timezones` serves the same static.
- The site `timezone` setting's option list used to be a second,
  hand-kept 420-entry literal in the same file; it is now derived
  from `IANA_TIMEZONES` (`timezone_options()` in
  `definitions.rs`), so validation and the definitions endpoint can
  never drift from the source list again.

## 4. Languages (existed — extended and unified)

`i18n/languages.rs` had 12 languages; it now has 42
(`builtin_languages`, `is_builtin_language`, new `language_info`).
Added: hi, it, tr, vi, th, ms, uk, pl, sv, da, fi, nb, he, fa, ur,
bn, ta, ro, hu, cs, el, sr, hr, bg, ca, sk, sl, lt, lv, et — each
with its correct ISO 639-1 code, English name, native name and rtl
flag (he, fa, ur are rtl). The original twelve are unchanged.

- This catalogue is THE single shared list: `i18n::validation::
  validate_lang` already checked against it, so extending the
  catalogue automatically extended translation validation.
- The site `site_language` setting's option list used to be a second,
  hand-kept 12-entry literal in `settings/definitions.rs`; it is now
  derived from `builtin_languages()` (`language_options()`), with
  labels "Name (Native)" ("Indonesian (Bahasa Indonesia)", plain
  "English" when both names match).
- `GET /api/v1/reference-data/languages` serves the catalogue.
- Not a duplicate list: the 8-code `languages` default in
  `LibrarySettings::default` (`library/types.rs`) is a per-module
  default *value* (admin-editable), not a catalogue — it stays.

## HTTP surface (module `reference_data`, core, read-only)

All routes are `RouteAuth::Authenticated`. There are no write
endpoints and no tenant guard: the lists are platform-shared, not
tenant data (infra-or-shared kind in the C3 map, like the i18n read
endpoints), so there is no tenant to resolve. The envelope is the
standard one: `{"data": ...}` on success,
`{"error": {"message", "code", "metadata"}}` on error.

| Method | Path | Result |
| --- | --- | --- |
| GET | `/api/v1/reference-data` | Index: the four lists, counts, sources |
| GET | `/api/v1/reference-data/countries` | 249 countries with flag paths |
| GET | `/api/v1/reference-data/countries/{code}` | One country, 404 if unknown |
| GET | `/api/v1/reference-data/currencies` | 169 currencies |
| GET | `/api/v1/reference-data/currencies/{code}` | One currency, 404 if unknown |
| GET | `/api/v1/reference-data/timezones` | 420 IANA zones |
| GET | `/api/v1/reference-data/languages` | 42 languages |

The older `GET /api/v1/i18n/languages` and
`GET /api/v1/i18n/currencies[/<code>]` endpoints still exist and
serve the same single sources; new consumers should use
`/api/v1/reference-data/...`.

## Rules for future changes

- Add a currency, country, language or timezone by editing its
  single source only (the files in the table above). Never add a
  second list in a consuming module; import the source instead.
- A withdrawn ISO code is removed from the catalogue; never keep a
  withdrawn code "for compatibility" without a dated comment.
- Zero-decimal behaviour comes from the catalogue's `minor_digits`
  column. Never reintroduce a local zero-decimal list.
- Flag files: when flag-icons ships a new flag, copy both variants
  into `assets/icons/flags/` and update `PREVIEW.md` and the
  registry in the same change.
