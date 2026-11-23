//! Map & Tagging — provider abstraction.
//!
//! A provider is one tile source behind ONE interface: a tile
//! URL template with `{z}` / `{x}` / `{y}` placeholders (plus
//! an optional `{s}` subdomain placeholder and an optional
//! `{key}` browser-key placeholder), an attribution string, a
//! maximum zoom, an optional subdomain list, and a free-form
//! options object. The built-ins are OpenStreetMap (Leaflet
//! default, free, no key), Mapbox, and Google Maps — but the
//! registry is data, not a switch: an admin adds any
//! open-source tile server or fully custom tile API as a
//! provider row through the providers API, and a tenant picks
//! its active provider in settings. No provider, URL, or key
//! is hard-coded anywhere a request is served: the runtime
//! reads the `map_providers` table (falling back to the
//! built-in definitions only when the table has no row for a
//! slug) and the tenant's `map_settings` row.
//!
//! Tile fetching itself is a browser-to-provider request: the
//! server only resolves WHICH template to use and substitutes
//! the stored browser key. That is also why the default tile
//! host (tile.openstreetmap.org) is a runtime dependency worth
//! naming: a tenant that must not depend on it runs its own
//! tile server and points a custom provider row at it (see
//! docs/integration/MAP_TAGGING.md).

use unified_core::error::{UnifiedError, UnifiedResult};

/// Resolve a tile URL template for the browser: substitute the
/// stored browser key into `{key}`. Returns `None` when the
/// provider needs a key and none is stored — the caller then
/// serves `tile_url: null` and the map shows an honest "this
/// provider needs an API key" note instead of broken tiles.
/// `{s}` is deliberately left in place: the tile client
/// rotates it through the provider's subdomains.
pub(crate) fn resolve_tile_url(
    template: &str,
    api_key: Option<&str>,
    requires_api_key: bool,
) -> Option<String> {
    if requires_api_key && api_key.is_none() {
        return None;
    }
    match api_key {
        Some(key) => Some(template.replace("{key}", key)),
        None => Some(template.to_string()),
    }
}

/// Parse a provider's subdomain list into the values the tile
/// client rotates `{s}` through. Both shapes are accepted: a
/// comma-separated list (`a,b,c`) and the compact Leaflet
/// spelling (`abc`).
pub(crate) fn parse_subdomains(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.contains(',') {
        trimmed
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        trimmed.chars().map(|c| c.to_string()).collect()
    }
}

/// Validate a subdomain list: every token must be short and
/// made of letters, digits, or `-`. An empty list is valid
/// (the provider has no `{s}` placeholder).
pub(crate) fn validate_subdomains(raw: &str) -> UnifiedResult<()> {
    for token in parse_subdomains(raw) {
        let valid = !token.is_empty()
            && token.len() <= 32
            && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !valid {
            return Err(UnifiedError::bad_request(
                "subdomains must be single characters or a comma-separated list of short names (letters, digits, '-')",
            ));
        }
    }
    Ok(())
}

/// Validate a provider maximum zoom: 0..=22.
pub(crate) fn validate_max_zoom(value: i32) -> UnifiedResult<()> {
    if !(0..=22).contains(&value) {
        return Err(UnifiedError::bad_request(
            "max_zoom must be between 0 and 22",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdomain_parsing_accepts_both_spellings() {
        assert_eq!(parse_subdomains("abc"), vec!["a", "b", "c"]);
        assert_eq!(parse_subdomains("a,b,c"), vec!["a", "b", "c"]);
        assert!(parse_subdomains("").is_empty());
        assert!(validate_subdomains("abc").is_ok());
        assert!(validate_subdomains("a-b,node2").is_ok());
        assert!(validate_subdomains("has space").is_err());
    }

    #[test]
    fn tile_url_resolution_needs_a_key_when_required() {
        let template = "https://example.com/{z}/{x}/{y}?key={key}";
        assert!(resolve_tile_url(template, None, true).is_none());
        assert_eq!(
            resolve_tile_url(template, Some("secret"), true).unwrap(),
            "https://example.com/{z}/{x}/{y}?key=secret"
        );
        // A keyless provider resolves with the template untouched.
        assert_eq!(
            resolve_tile_url("https://tile.example.org/{z}/{x}/{y}.png", None, false).unwrap(),
            "https://tile.example.org/{z}/{x}/{y}.png"
        );
    }
}
