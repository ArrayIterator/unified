// Reference Data -- HTTP handlers.
//
// Read-only handlers for the four platform-shared lists. Every handler
// returns the API envelope (`{"data": ...}`) through
// `api_json::success_response`, and unknown codes are a 404 with the
// error envelope (`{"error": {"message", "code", "metadata"}}`).
//
// No tenant guard: these lists are platform-shared, not tenant data
// (see the module docs in `reference_data.rs`). No caller resolution
// either: the routes are `RouteAuth::Authenticated` and the reads carry
// no per-user data, exactly like the i18n list endpoints.

use unified_core::error::UnifiedError;
use unified_core::json as api_json;
use unified_core::reference_data::countries::{builtin_countries, country_info};
use unified_core::reference_data::currencies::{builtin_currencies, currency_info};
use actix_web::{HttpResponse, web};
use serde_json::json;

use crate::modules::i18n::languages::builtin_languages;
use crate::modules::settings::definitions::IANA_TIMEZONES;

/// `GET /api/v1/reference-data` -- index: what lists exist, how many
/// entries each holds, and where each list is owned.
pub(crate) async fn index() -> HttpResponse {
    api_json::success_response(
        json!({
            "lists": [
                {
                    "key": "countries",
                    "path": "/api/v1/reference-data/countries",
                    "count": builtin_countries().len(),
                    "source": "unified_core::reference_data::countries (ISO 3166-1)",
                },
                {
                    "key": "currencies",
                    "path": "/api/v1/reference-data/currencies",
                    "count": builtin_currencies().len(),
                    "source": "unified_core::reference_data::currencies (ISO 4217)",
                },
                {
                    "key": "timezones",
                    "path": "/api/v1/reference-data/timezones",
                    "count": IANA_TIMEZONES.len(),
                    "source": "settings::definitions::IANA_TIMEZONES (IANA)",
                },
                {
                    "key": "languages",
                    "path": "/api/v1/reference-data/languages",
                    "count": builtin_languages().len(),
                    "source": "i18n::languages::builtin_languages (ISO 639-1)",
                },
            ],
        }),
        200,
    )
}

/// `GET /api/v1/reference-data/countries` -- all 249 ISO 3166-1
/// countries with both flag file paths.
pub(crate) async fn list_countries() -> HttpResponse {
    api_json::success_response(json!({ "countries": builtin_countries() }), 200)
}

/// `GET /api/v1/reference-data/countries/{code}` -- one country by
/// ISO 3166-1 alpha-2 code (case-insensitive); 404 for unknown codes.
pub(crate) async fn get_country(path: web::Path<String>) -> HttpResponse {
    let code = path.into_inner();
    match country_info(&code) {
        Some(country) => api_json::success_response(json!({ "country": country }), 200),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("unknown country code \"{code}\"")),
            false,
        ),
    }
}

/// `GET /api/v1/reference-data/currencies` -- the full active ISO 4217
/// catalogue (code, numeric code, name, symbol, minor digits).
pub(crate) async fn list_currencies() -> HttpResponse {
    api_json::success_response(json!({ "currencies": builtin_currencies() }), 200)
}

/// `GET /api/v1/reference-data/currencies/{code}` -- one currency by
/// ISO 4217 alphabetic code (case-insensitive); 404 for unknown codes.
pub(crate) async fn get_currency(path: web::Path<String>) -> HttpResponse {
    let code = path.into_inner();
    match currency_info(&code) {
        Some(currency) => api_json::success_response(json!({ "currency": currency }), 200),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("unknown currency code \"{code}\"")),
            false,
        ),
    }
}

/// `GET /api/v1/reference-data/timezones` -- the full IANA timezone
/// list (420 zones, UTC first), the same single source the site
/// `timezone` setting validates against.
pub(crate) async fn list_timezones() -> HttpResponse {
    api_json::success_response(json!({ "timezones": IANA_TIMEZONES }), 200)
}

/// `GET /api/v1/reference-data/languages` -- the ISO 639-1 language
/// catalogue (code, English name, native name, rtl flag), the same
/// single source the site `site_language` setting validates against.
pub(crate) async fn list_languages() -> HttpResponse {
    api_json::success_response(json!({ "languages": builtin_languages() }), 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_counts_match_the_single_sources() {
        assert_eq!(builtin_countries().len(), 249);
        assert_eq!(builtin_currencies().len(), 169);
        assert_eq!(IANA_TIMEZONES.len(), 420);
        assert_eq!(builtin_languages().len(), 42);
    }
}
