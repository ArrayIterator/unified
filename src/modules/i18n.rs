// Translations / i18n Manager module: built-in language catalogue,
// translation store, locale and plural rules, currencies and gettext
// (.po) catalogues. Split into one file per concern; this file only
// declares the submodules and re-exports the public API.

pub mod ai_capabilities;
pub mod catalog;
pub mod currencies;
pub mod handlers;
pub mod languages;
pub mod locale;
pub mod module;
pub mod state;
pub mod types;
pub mod validation;

// Re-exported from languages.rs.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::languages::Language;
pub use self::languages::builtin_languages;
pub use self::languages::is_builtin_language;
pub use self::languages::language_info;
// Re-exported from types.rs.
pub use self::types::LoadPoCatalogInput;
pub use self::types::TranslateInput;
pub use self::types::TranslatePluralInput;
pub use self::types::Translation;
pub use self::types::UpsertTranslationInput;
// Re-exported from state.rs.
pub use self::state::I18nState;
// Re-exported from locale.rs.
pub use self::locale::contextual_key;
pub use self::locale::language_from_locale;
pub use self::locale::negotiate_language;
pub use self::locale::normalize_locale;
pub use self::locale::parse_accept_language;
pub use self::locale::plural_form_count;
pub use self::locale::plural_form_index;
pub use self::locale::split_plural_forms;
// Re-exported from currencies.rs.
pub use self::currencies::CurrencyInfo;
pub use self::currencies::builtin_currencies;
pub use self::currencies::currency_info;
pub use self::currencies::format_currency;
// Re-exported from catalog.rs.
pub use self::catalog::PoEntry;
pub use self::catalog::PoLoadResult;
pub use self::catalog::parse_po_catalog;
// Re-exported from module.rs.
pub use self::module::I18nModule;
pub use self::module::register;
