// Reference Data -- the single HTTP surface for the platform-shared
// reference lists.
//
// The data itself lives in exactly one place per list; this module only
// re-exports it and serves it over HTTP:
//
//   * countries   -- `unified_core::reference_data::countries`
//                    (ISO 3166-1, 249 countries + flag files)
//   * currencies  -- `unified_core::reference_data::currencies`
//                    (full active ISO 4217 catalogue, 169 codes)
//   * timezones   -- `crate::modules::settings::definitions::IANA_TIMEZONES`
//                    (full IANA list, 420 zones; owned by settings)
//   * languages   -- `crate::modules::i18n::languages`
//                    (ISO 639-1 catalogue, 42 languages; owned by i18n)
//
// Platform-shared, NEVER tenant-scoped: the lists are compiled in,
// identical for every tenant, and read-only, so there is no tenant
// data to guard and no C3 tenant guard applies (same kind as the i18n
// read endpoints, which are infra-or-shared by the C3 map). The routes
// are still `RouteAuth::Authenticated` like the i18n reads: only a
// signed-in caller may list reference data. There are no write
// endpoints -- a list changes only by changing its single source in
// code, which is what keeps the four lists from drifting apart.
//
// See docs/integration/REFERENCE_DATA.md for the full contract.

pub mod handlers;
pub mod module;

pub use self::module::ReferenceDataModule;
pub use self::module::register;
