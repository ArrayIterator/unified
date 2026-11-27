//! Holidays module - global public-holiday synchronization.
//!
//! Split into one file per concern under `holidays/`; this file only
//! declares the submodules and re-exports the public API. The full
//! module design notes live at the top of `holidays/module.rs`.
pub mod events;
mod constants;
pub mod types;
pub mod state;
mod auth;
mod crypto;
mod validation;
mod ical;
mod providers;
mod sync;
mod hooks;
mod handlers_providers;
mod handlers_countries;
mod handlers_configs;
mod handlers_holidays;
pub mod module;

// Re-exports from events.rs.
pub use self::events::EVENT_IS_HOLIDAY;
pub use self::events::EVENT_NON_WORKING_DAYS;
pub use self::events::EVENT_SYNC_REQUESTED;
pub use self::events::EVENT_SYNC_ALL_REQUESTED;
pub use self::events::EVENT_PROVIDER_CREATED;
pub use self::events::EVENT_PROVIDER_UPDATED;
pub use self::events::EVENT_PROVIDER_DELETED;
pub use self::events::EVENT_CONFIG_CREATED;
pub use self::events::EVENT_CONFIG_UPDATED;
pub use self::events::EVENT_CONFIG_DELETED;
pub use self::events::EVENT_SYNC_STARTED;
pub use self::events::EVENT_SYNC_FINISHED;
pub use self::events::EVENT_SYNC_FAILED;
pub use self::events::EVENT_OVERRIDE_CREATED;
pub use self::events::EVENT_OVERRIDE_UPDATED;
pub use self::events::EVENT_OVERRIDE_DELETED;
pub use self::events::PERM_HOLIDAYS_READ;
pub use self::events::PERM_HOLIDAYS_MANAGE;
pub use self::events::PERM_HOLIDAYS_SYNC;

// Re-exports from constants.rs.
pub(crate) use self::constants::DEFAULT_PROVIDER_ID;
pub(crate) use self::constants::DEFAULT_SCHEDULE;
pub(crate) use self::constants::DEFAULT_YEARS_AHEAD;
pub(crate) use self::constants::MIN_SYNC_YEAR;
pub(crate) use self::constants::MAX_SYNC_YEAR;
pub(crate) use self::constants::MAX_YEARS_PER_CONFIG;
pub(crate) use self::constants::MAX_COUNTRIES_PER_CONFIG;
pub(crate) use self::constants::MAX_FALLBACK_PROVIDERS;
pub(crate) use self::constants::MAX_NON_WORKING_RANGE_DAYS;
pub(crate) use self::constants::MAX_HOOK_RANGE_DAYS;
pub(crate) use self::constants::DEFAULT_FETCH_TIMEOUT_SECS;
pub(crate) use self::constants::MAX_RESPONSE_BYTES;
pub(crate) use self::constants::MAX_REDIRECTS;
pub(crate) use self::constants::MAX_RETRY_AFTER;
pub(crate) use self::constants::MAX_FETCH_ATTEMPTS;
pub(crate) use self::constants::RETRY_BASE_DELAY;
pub(crate) use self::constants::RETRY_MAX_DELAY;
pub(crate) use self::constants::MAX_RATE_LIMIT_WAIT;
pub(crate) use self::constants::SCHEDULER_JOB_KIND_SYNC;
pub(crate) use self::constants::SCHEDULER_JOB_KIND_SYNC_ALL;
pub(crate) use self::constants::USER_AGENT;
pub(crate) use self::constants::ISO_COUNTRIES;

// Re-exports from types.rs.
pub use self::types::ProviderKind;
pub use self::types::HolidayProvider;
pub use self::types::HolidayCountry;
pub use self::types::HolidaySyncConfig;
pub use self::types::HolidaySyncRun;
pub use self::types::Holiday;
pub use self::types::HolidayOverride;
pub(crate) use self::types::FetchedHoliday;
pub(crate) use self::types::RateState;
pub(crate) use self::types::SyncSummary;
pub(crate) use self::types::CreateProviderRequest;
pub(crate) use self::types::UpdateProviderRequest;
pub(crate) use self::types::TestProviderRequest;
pub(crate) use self::types::CreateCountryRequest;
pub(crate) use self::types::UpdateCountryRequest;
pub(crate) use self::types::CreateConfigRequest;
pub(crate) use self::types::UpdateConfigRequest;
pub(crate) use self::types::CreateOverrideRequest;
pub(crate) use self::types::UpdateOverrideRequest;
pub(crate) use self::types::IsHolidayQuery;
pub(crate) use self::types::NonWorkingDaysQuery;

// Re-exports from state.rs.
pub use self::state::HolidaysState;
pub(crate) use self::state::seed_providers;

// Re-exports from auth.rs.
pub(crate) use self::auth::resolve_tenant;
pub(crate) use self::auth::Caller;
pub(crate) use self::auth::caller;
pub(crate) use self::auth::platform_caller;
pub(crate) use self::auth::require_permission;
pub(crate) use self::auth::ok;
pub(crate) use self::auth::created;
pub(crate) use self::auth::err;
pub(crate) use self::auth::paginate;
pub(crate) use self::auth::not_found_for_tenant;

// Re-exports from crypto.rs.
pub(crate) use self::crypto::encryption_secret;
pub(crate) use self::crypto::keystream;
pub(crate) use self::crypto::encrypt_api_key;
pub(crate) use self::crypto::decrypt_api_key;
pub(crate) use self::crypto::key_fingerprint;

// Re-exports from validation.rs.
pub(crate) use self::validation::validate_provider_url;
pub(crate) use self::validation::build_fetch_client;
pub(crate) use self::validation::normalize_country_code;
pub(crate) use self::validation::parse_date;
pub(crate) use self::validation::validate_provider_id;
pub(crate) use self::validation::validate_schedule;
pub(crate) use self::validation::validate_years;
pub(crate) use self::validation::default_years;
pub(crate) use self::validation::validate_config_countries;
pub(crate) use self::validation::load_enabled_provider;
pub(crate) use self::validation::validate_fallback_chain;

// Re-exports from ical.rs.
pub(crate) use self::ical::parse_ics;

// Re-exports from providers.rs.
pub(crate) use self::providers::parse_nager_response;
pub(crate) use self::providers::parse_calendarific_response;
pub(crate) use self::providers::parse_abstract_response;
pub(crate) use self::providers::fetch_with_retries;
pub(crate) use self::providers::build_fetch_url;

// Re-exports from sync.rs.
pub(crate) use self::sync::fetch_holidays_for_provider;
pub(crate) use self::sync::run_sync_for_config;

// Re-exports from hooks.rs.
pub(crate) use self::hooks::on_is_holiday_requested;
pub(crate) use self::hooks::answer_is_holiday;
pub(crate) use self::hooks::on_non_working_days_requested;
pub(crate) use self::hooks::on_sync_requested;
pub(crate) use self::hooks::on_sync_all_requested;

// Re-exports from handlers_providers.rs.
pub(crate) use self::handlers_providers::list_providers;
pub(crate) use self::handlers_providers::create_provider;
pub(crate) use self::handlers_providers::get_provider;
pub(crate) use self::handlers_providers::update_provider;
pub(crate) use self::handlers_providers::delete_provider;
pub(crate) use self::handlers_providers::test_provider;

// Re-exports from handlers_countries.rs.
pub(crate) use self::handlers_countries::list_countries;
pub(crate) use self::handlers_countries::create_country;
pub(crate) use self::handlers_countries::update_country;
pub(crate) use self::handlers_countries::delete_country;

// Re-exports from handlers_configs.rs.
pub(crate) use self::handlers_configs::list_configs;
pub(crate) use self::handlers_configs::create_config;
pub(crate) use self::handlers_configs::get_config;
pub(crate) use self::handlers_configs::update_config;
pub(crate) use self::handlers_configs::delete_config;
pub(crate) use self::handlers_configs::sync_config;
pub(crate) use self::handlers_configs::sync_all_configs;
pub(crate) use self::handlers_configs::list_sync_runs;
pub(crate) use self::handlers_configs::get_sync_run;

// Re-exports from handlers_holidays.rs.
pub(crate) use self::handlers_holidays::list_holidays;
pub(crate) use self::handlers_holidays::get_is_holiday;
pub(crate) use self::handlers_holidays::get_non_working_days;
pub(crate) use self::handlers_holidays::list_overrides;
pub(crate) use self::handlers_holidays::create_override;
pub(crate) use self::handlers_holidays::get_override;
pub(crate) use self::handlers_holidays::update_override;
pub(crate) use self::handlers_holidays::delete_override;

// Re-exports from module.rs.
pub use self::module::HolidaysModule;
pub use self::module::register;

