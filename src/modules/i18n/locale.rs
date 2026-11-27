// i18n -- Locale handling and plural rules.

use super::languages::*;

// Locale handling, plural rules, currencies, and gettext-style catalogues.

/// Normalise a locale to BCP 47 shape: language lowercase, region
/// uppercase, hyphen separated (`en_US` / `EN-us` -> `en-US`).
/// Script subtags (exactly four letters) are Titlecase (`zh-hant` ->
/// `zh-Hant`), two-letter region subtags are uppercase, and every
/// other subtag (variants, extensions) is left as-is.
pub fn normalize_locale(input: &str) -> String {
    let mut parts = input.trim().split(['-', '_']);
    let Some(language) = parts.next() else {
        return String::new();
    };
    let language = language.to_lowercase();
    let mut out = language;
    for part in parts {
        if part.is_empty() {
            continue;
        }
        out.push('-');
        if part.chars().count() == 4 {
            // Script subtag (e.g. `Hant`, `Latn`): Titlecase. A
            // four-digit subtag has no case, so title-casing it is a
            // no-op and the branch stays safe for those too.
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(&chars.as_str().to_lowercase());
            }
        } else if part.chars().count() == 2 {
            // Region subtag (e.g. `US`, `BR`): uppercase.
            out.push_str(&part.to_uppercase());
        } else {
            out.push_str(part);
        }
    }
    out
}

/// Language subtag of a locale (`en-US` -> `en`, `pt_BR` -> `pt`).
pub fn language_from_locale(locale: &str) -> String {
    locale
        .trim()
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_lowercase()
}

/// Parse an `Accept-Language` header into `(language, quality)` pairs,
/// highest quality first. Entries with `q=0` (explicitly refused) are
/// dropped; a missing quality defaults to 1.0.
pub fn parse_accept_language(header: &str) -> Vec<(String, f32)> {
    let mut parsed: Vec<(String, f32)> = Vec::new();
    for part in header.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mut segments = part.split(';');
        let lang = segments.next().unwrap_or("").trim();
        if lang.is_empty() || lang == "*" {
            continue;
        }
        let mut quality: f32 = 1.0;
        for segment in segments {
            let segment = segment.trim();
            if let Some(value) = segment.strip_prefix("q=") {
                if let Ok(parsed_quality) = value.trim().parse::<f32>() {
                    quality = parsed_quality.clamp(0.0, 1.0);
                }
            }
        }
        if quality > 0.0 {
            parsed.push((normalize_locale(lang), quality));
        }
    }
    // Stable sort: equal qualities keep header order.
    parsed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    parsed
}

/// Best builtin language for an `Accept-Language` header: exact locale
/// match first, then language-only match, then English.
pub fn negotiate_language(header: &str) -> String {
    let available: Vec<String> = builtin_languages()
        .iter()
        .map(|language| language.code.clone())
        .collect();
    for (accepted, _) in parse_accept_language(header) {
        let language = language_from_locale(&accepted);
        if available.iter().any(|code| code == &accepted) {
            return accepted;
        }
        if available.iter().any(|code| code == &language) {
            return language;
        }
    }
    "en".to_string()
}

/// Number of plural forms a locale uses (CLDR families).
pub fn plural_form_count(locale: &str) -> usize {
    match language_from_locale(locale).as_str() {
        "id" | "ms" | "vi" | "zh" | "ja" | "ko" | "th" | "tr" | "lo" | "my" => 1,
        "ar" => 6,
        "ga" => 5,
        "pl" | "sl" | "mt" | "gd" => 4,
        "lv" | "ru" | "uk" | "be" | "sr" | "hr" | "bs" | "cs" | "sk" | "ro" | "lt" => 3,
        _ => 2,
    }
}

/// Index of the plural form for `count` in `locale`, following CLDR
/// plural rules. Counts are treated by magnitude (negative counts use
/// their absolute value); non-finite counts select the last form.
///
/// Families covered, with the corrections the common one/other shortcut
/// gets wrong:
/// - No plural at all (Indonesian, Malay, Vietnamese, Chinese, Japanese,
///   Korean, Thai, Turkish, Lao, Burmese): always form 0.
/// - French and Portuguese: 0 and 1 both take the singular form.
/// - Slavic (Russian, Ukrainian, Belarusian, Serbian, Croatian,
///   Bosnian), Czech/Slovak, Romanian, Lithuanian: three forms
///   with distinct remainders; fractional counts take the last form.
/// - Polish: four CLDR forms (one/few/many/other); fractional counts
///   take `other`.
/// - Latvian: three forms (zero/one/other).
/// - Slovenian: four forms (one/two/few/other); fractional counts
///   take `few`.
/// - Maltese: four forms (one/few/many/other).
/// - Irish: five forms (one/two/few/many/other).
/// - Scottish Gaelic: four forms (one/two/few/other).
/// - Arabic: six forms (zero, one, two, few, many, other).
/// - Anything else: 1 takes form 0, everything else form 1.
///
/// Gettext (`.po`) `nplurals` headers do not always match these CLDR
/// counts, because gettext catalogues predate CLDR and use their own
/// form orders. `load_po_catalog` normalises stored forms into CLDR
/// order (`normalise_plural_form_order` in `catalog.rs`), so the
/// mappings below describe what the loader corrects, not what
/// `translate_plural` has to cope with:
/// - Polish: gettext uses 3 forms (one/few/many); CLDR `other`
///   (fractions only) has no gettext form, so a 3-form catalogue
///   falls back to the caller-supplied plural for fractional counts.
/// - Latvian: gettext commonly uses 3 forms in (one, zero, other)
///   order, while CLDR orders them (zero, one, other) - the first
///   two stored forms are swapped relative to a gettext catalogue.
///   Catalogues using the current standard formula store (one,
///   other, zero) instead; the loader detects that formula from
///   the header and rotates instead.
/// - Irish: gettext maps 0 to `few` (form 2); CLDR maps 0 to `other`
///   (form 4).
/// - Slovenian, Maltese and Scottish Gaelic: gettext uses the same
///   form count and order as CLDR for integer counts.
pub fn plural_form_index(locale: &str, count: f64) -> usize {
    if !count.is_finite() {
        return plural_form_count(locale).saturating_sub(1);
    }
    let n = count.abs();
    let lang = language_from_locale(locale);
    let lang = lang.as_str();
    if matches!(
        lang,
        "id" | "ms" | "vi" | "zh" | "ja" | "ko" | "th" | "tr" | "lo" | "my"
    ) {
        return 0;
    }
    // Integer part only; the fractional part is handled per family below.
    // `as` saturates on overflow, so very large counts stay well-defined.
    let i = n.trunc() as i64;
    let fractional = n.fract() != 0.0;
    match lang {
        "fr" | "pt" => {
            if !fractional && (i == 0 || i == 1) {
                0
            } else {
                1
            }
        }
        "ru" | "uk" | "be" | "sr" | "hr" | "bs" => {
            if fractional {
                2
            } else if i % 10 == 1 && i % 100 != 11 {
                0
            } else if (2..=4).contains(&(i % 10)) && (i % 100 < 10 || i % 100 >= 20) {
                1
            } else {
                2
            }
        }
        "pl" => {
            // CLDR one/few/many/other. `many` is every integer that is
            // not one or few: a units digit of 0 or 1 (other than 1
            // itself), a units digit of 5..9, or a tens remainder of
            // 12..14. Fractions take `other`.
            if fractional {
                3
            } else if i == 1 {
                0
            } else if (2..=4).contains(&(i % 10)) && (i % 100 < 10 || i % 100 >= 20) {
                1
            } else if i % 10 == 0
                || i % 10 == 1
                || (5..=9).contains(&(i % 10))
                || (12..=14).contains(&(i % 100))
            {
                2
            } else {
                3
            }
        }
        "lv" => {
            // CLDR zero/one/other. `zero` is an integer ending in 0
            // or 11..19, or any number whose two visible fraction
            // digits end in 11..19 (`1.11`); `one` is an integer
            // ending in 1 except 11, a two-fraction-digit number
            // whose fraction ends in 1 except 11 (`1.21`), or a
            // fraction of any other width ending in 1 (`1.1`, `0.1`).
            // Everything else takes `other`.
            let (v, f10, f100) = fraction_operands(n);
            if (!fractional && (i % 10 == 0 || (11..=19).contains(&(i % 100))))
                || (v == 2 && (11..=19).contains(&f100))
            {
                0
            } else if (!fractional && i % 10 == 1 && i % 100 != 11)
                || (v == 2 && f10 == 1 && f100 != 11)
                || (v != 2 && f10 == 1)
            {
                1
            } else {
                2
            }
        }
        "sl" => {
            // CLDR one/two/few/other. Fractions take `few`.
            if fractional {
                2
            } else if i % 100 == 1 {
                0
            } else if i % 100 == 2 {
                1
            } else if (3..=4).contains(&(i % 100)) {
                2
            } else {
                3
            }
        }
        "mt" => {
            // CLDR one/few/many/other. Fractions take `other`.
            if fractional {
                3
            } else if i == 1 {
                0
            } else if i == 0 || (2..=10).contains(&(i % 100)) {
                1
            } else if (11..=19).contains(&(i % 100)) {
                2
            } else {
                3
            }
        }
        "ga" => {
            // CLDR one/two/few/many/other. 0 and 11+ take `other`;
            // fractions take `other`.
            if fractional {
                4
            } else if i == 1 {
                0
            } else if i == 2 {
                1
            } else if (3..=6).contains(&i) {
                2
            } else if (7..=10).contains(&i) {
                3
            } else {
                4
            }
        }
        "gd" => {
            // CLDR one/two/few/other. Fractions take `other`.
            if fractional {
                3
            } else if i == 1 || i == 11 {
                0
            } else if i == 2 || i == 12 {
                1
            } else if (3..=10).contains(&i) || (13..=19).contains(&i) {
                2
            } else {
                3
            }
        }
        "cs" | "sk" => {
            if fractional {
                2
            } else if i == 1 {
                0
            } else if (2..=4).contains(&i) {
                1
            } else {
                2
            }
        }
        "ro" => {
            if fractional {
                2
            } else if i == 1 {
                0
            } else if i == 0 || (1..=19).contains(&(i % 100)) {
                1
            } else {
                2
            }
        }
        "lt" => {
            if fractional {
                2
            } else if i % 10 == 1 && i % 100 != 11 {
                0
            } else if (2..=9).contains(&(i % 10)) && (i % 100 < 10 || i % 100 >= 20) {
                1
            } else {
                2
            }
        }
        "ar" => {
            if fractional {
                5
            } else if i == 0 {
                0
            } else if i == 1 {
                1
            } else if i == 2 {
                2
            } else if (3..=10).contains(&(i % 100)) {
                3
            } else if (11..=99).contains(&(i % 100)) {
                4
            } else {
                5
            }
        }
        _ => {
            if n == 1.0 {
                0
            } else {
                1
            }
        }
    }
}

/// Split a NUL-separated plural-forms string (`"one\0other\0many"`).
pub fn split_plural_forms(forms: &str) -> Vec<String> {
    if forms.is_empty() {
        return Vec::new();
    }
    forms.split('\0').map(str::to_string).collect()
}

/// Visible fraction operands of `n` for CLDR rules that distinguish
/// fraction widths (Latvian): `(v, f % 10, f % 100)`, where `v` is
/// the number of visible fraction digits and `f` is those digits as
/// an integer. Trailing zeros carry no information in an `f64`, so
/// they are dropped (`1.10` reads as `1.1`, with `v = 1`).
fn fraction_operands(n: f64) -> (usize, i64, i64) {
    let text = format!("{n}");
    let Some((_, fraction)) = text.split_once('.') else {
        return (0, 0, 0);
    };
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        return (0, 0, 0);
    }
    let f10 = fraction
        .chars()
        .last()
        .and_then(|digit| digit.to_digit(10))
        .unwrap_or(0) as i64;
    let f100 = if fraction.len() >= 2 {
        fraction[fraction.len() - 2..].parse::<i64>().unwrap_or(0)
    } else {
        fraction.parse::<i64>().unwrap_or(0)
    };
    (fraction.len(), f10, f100)
}

/// Gettext-style composite key: context and key joined with the standard
/// msgctxt separator (U+0004), so the same source string can have
/// different translations in different contexts. An empty context
/// returns the bare key: lookups filter out empty contexts, so a
/// contextual key built from one could never be looked up again.
pub fn contextual_key(key: &str, context: &str) -> String {
    if context.is_empty() {
        key.to_string()
    } else {
        format!("{context}\u{4}{key}")
    }
}

// Tests for the pure locale and plural-rule helpers in this file.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_is_normalized_to_bcp47_form() {
        assert_eq!(normalize_locale("en_US"), "en-US");
        assert_eq!(normalize_locale("EN"), "en");
        assert_eq!(normalize_locale("pt-BR"), "pt-BR");
        assert_eq!(normalize_locale(""), "");
    }

    #[test]
    fn locale_script_subtags_are_titlecase_not_uppercase() {
        assert_eq!(normalize_locale("zh-Hant"), "zh-Hant");
        assert_eq!(normalize_locale("zh-hant"), "zh-Hant");
        assert_eq!(normalize_locale("zh-HANT"), "zh-Hant");
        assert_eq!(normalize_locale("sr-Latn-RS"), "sr-Latn-RS");
        assert_eq!(normalize_locale("sr-latn-rs"), "sr-Latn-RS");
        assert_eq!(normalize_locale("en-us"), "en-US");
        // Variants and numeric subtags are left as-is.
        assert_eq!(normalize_locale("sl-rozaj"), "sl-rozaj");
        assert_eq!(normalize_locale("de-1996"), "de-1996");
    }

    #[test]
    fn language_is_extracted_from_locale() {
        assert_eq!(language_from_locale("en-US"), "en");
        assert_eq!(language_from_locale("pt_BR"), "pt");
        assert_eq!(language_from_locale("id"), "id");
    }

    #[test]
    fn accept_language_parses_qualities_and_drops_zero() {
        assert_eq!(
            parse_accept_language("en-US,en;q=0.9,fr;q=0"),
            vec![("en-US".to_string(), 1.0f32), ("en".to_string(), 0.9f32),]
        );
        assert_eq!(parse_accept_language(""), Vec::<(String, f32)>::new());
        assert_eq!(parse_accept_language("*"), Vec::<(String, f32)>::new());
        assert_eq!(
            parse_accept_language("de;q=0.5,fr;q=0.8"),
            vec![("fr".to_string(), 0.8f32), ("de".to_string(), 0.5f32),]
        );
    }

    #[test]
    fn negotiation_prefers_the_first_supported_language() {
        assert_eq!(negotiate_language("id, en;q=0.8"), "id");
        assert_eq!(negotiate_language("xx"), "en");
        assert_eq!(negotiate_language("en-US"), "en");
        assert_eq!(negotiate_language(""), "en");
    }

    #[test]
    fn plural_form_counts_follow_cldr_rules() {
        assert_eq!(plural_form_count("id"), 1);
        assert_eq!(plural_form_count("ja"), 1);
        assert_eq!(plural_form_count("ar"), 6);
        assert_eq!(plural_form_count("en"), 2);
        assert_eq!(plural_form_count("ru"), 3);
        assert_eq!(plural_form_count("xx"), 2);
        // CLDR families added after the split: Polish has four forms
        // (one/few/many/other), not three.
        assert_eq!(plural_form_count("pl"), 4);
        assert_eq!(plural_form_count("lv"), 3);
        assert_eq!(plural_form_count("sl"), 4);
        assert_eq!(plural_form_count("mt"), 4);
        assert_eq!(plural_form_count("ga"), 5);
        assert_eq!(plural_form_count("gd"), 4);
    }

    #[test]
    fn plural_form_index_selects_the_right_form() {
        assert_eq!(plural_form_index("en", 1.0), 0);
        assert_eq!(plural_form_index("en", 2.0), 1);
        assert_eq!(plural_form_index("id", 5.0), 0);
        assert_eq!(plural_form_index("fr", 0.0), 0);
        assert_eq!(plural_form_index("fr", 2.0), 1);
        assert_eq!(plural_form_index("ru", 1.0), 0);
        assert_eq!(plural_form_index("ru", 2.0), 1);
        assert_eq!(plural_form_index("ru", 5.0), 2);
        assert_eq!(plural_form_index("ar", 0.0), 0);
        assert_eq!(plural_form_index("ar", 1.0), 1);
        assert_eq!(plural_form_index("ar", 2.0), 2);
        assert_eq!(plural_form_index("en", f64::NAN), 1);
    }

    #[test]
    fn plural_form_index_covers_polish_four_forms() {
        assert_eq!(plural_form_index("pl", 1.0), 0);
        assert_eq!(plural_form_index("pl", 2.0), 1);
        assert_eq!(plural_form_index("pl", 22.0), 1);
        assert_eq!(plural_form_index("pl", 5.0), 2);
        assert_eq!(plural_form_index("pl", 0.0), 2);
        assert_eq!(plural_form_index("pl", 12.0), 2);
        assert_eq!(plural_form_index("pl", 111.0), 2);
        assert_eq!(plural_form_index("pl", 101.0), 2);
        // Fractions take `other`, the fourth form.
        assert_eq!(plural_form_index("pl", 1.5), 3);
        assert_eq!(plural_form_index("pl", f64::NAN), 3);
    }

    #[test]
    fn plural_form_index_covers_latvian_zero_one_other() {
        assert_eq!(plural_form_index("lv", 0.0), 0);
        assert_eq!(plural_form_index("lv", 10.0), 0);
        assert_eq!(plural_form_index("lv", 11.0), 0);
        assert_eq!(plural_form_index("lv", 20.0), 0);
        assert_eq!(plural_form_index("lv", 1.0), 1);
        assert_eq!(plural_form_index("lv", 21.0), 1);
        assert_eq!(plural_form_index("lv", 101.0), 1);
        assert_eq!(plural_form_index("lv", 2.0), 2);
        assert_eq!(plural_form_index("lv", 5.0), 2);
        assert_eq!(plural_form_index("lv", 22.0), 2);
        // Fraction widths: 1.1 takes `one`, 1.11 takes `zero`,
        // 1.21 takes `one`, 1.5 takes `other`.
        assert_eq!(plural_form_index("lv", 1.1), 1);
        assert_eq!(plural_form_index("lv", 1.11), 0);
        assert_eq!(plural_form_index("lv", 1.21), 1);
        assert_eq!(plural_form_index("lv", 1.5), 2);
    }

    #[test]
    fn plural_form_index_covers_slovenian_four_forms() {
        assert_eq!(plural_form_index("sl", 1.0), 0);
        assert_eq!(plural_form_index("sl", 101.0), 0);
        assert_eq!(plural_form_index("sl", 2.0), 1);
        assert_eq!(plural_form_index("sl", 102.0), 1);
        assert_eq!(plural_form_index("sl", 3.0), 2);
        assert_eq!(plural_form_index("sl", 4.0), 2);
        assert_eq!(plural_form_index("sl", 103.0), 2);
        assert_eq!(plural_form_index("sl", 5.0), 3);
        assert_eq!(plural_form_index("sl", 0.0), 3);
        // Fractions take `few`.
        assert_eq!(plural_form_index("sl", 1.5), 2);
    }

    #[test]
    fn plural_form_index_covers_maltese_four_forms() {
        assert_eq!(plural_form_index("mt", 1.0), 0);
        assert_eq!(plural_form_index("mt", 0.0), 1);
        assert_eq!(plural_form_index("mt", 2.0), 1);
        assert_eq!(plural_form_index("mt", 10.0), 1);
        assert_eq!(plural_form_index("mt", 102.0), 1);
        assert_eq!(plural_form_index("mt", 11.0), 2);
        assert_eq!(plural_form_index("mt", 19.0), 2);
        assert_eq!(plural_form_index("mt", 111.0), 2);
        assert_eq!(plural_form_index("mt", 20.0), 3);
        assert_eq!(plural_form_index("mt", 100.0), 3);
        assert_eq!(plural_form_index("mt", 1.5), 3);
    }

    #[test]
    fn plural_form_index_covers_irish_five_forms() {
        assert_eq!(plural_form_index("ga", 1.0), 0);
        assert_eq!(plural_form_index("ga", 2.0), 1);
        assert_eq!(plural_form_index("ga", 3.0), 2);
        assert_eq!(plural_form_index("ga", 6.0), 2);
        assert_eq!(plural_form_index("ga", 7.0), 3);
        assert_eq!(plural_form_index("ga", 10.0), 3);
        // 0 and 11+ take `other`, unlike the gettext convention
        // that maps 0 to `few`.
        assert_eq!(plural_form_index("ga", 0.0), 4);
        assert_eq!(plural_form_index("ga", 11.0), 4);
        assert_eq!(plural_form_index("ga", 1.5), 4);
    }

    #[test]
    fn plural_form_index_covers_scottish_gaelic_four_forms() {
        assert_eq!(plural_form_index("gd", 1.0), 0);
        assert_eq!(plural_form_index("gd", 11.0), 0);
        assert_eq!(plural_form_index("gd", 2.0), 1);
        assert_eq!(plural_form_index("gd", 12.0), 1);
        assert_eq!(plural_form_index("gd", 3.0), 2);
        assert_eq!(plural_form_index("gd", 10.0), 2);
        assert_eq!(plural_form_index("gd", 13.0), 2);
        assert_eq!(plural_form_index("gd", 19.0), 2);
        assert_eq!(plural_form_index("gd", 0.0), 3);
        assert_eq!(plural_form_index("gd", 20.0), 3);
        assert_eq!(plural_form_index("gd", 1.5), 3);
    }

    #[test]
    fn plural_forms_split_on_nul() {
        assert_eq!(
            split_plural_forms("one\0other"),
            vec!["one".to_string(), "other".to_string()]
        );
        assert!(split_plural_forms("").is_empty());
        assert_eq!(split_plural_forms("single"), vec!["single".to_string()]);
    }

    #[test]
    fn contextual_key_joins_with_unit_separator() {
        assert_eq!(contextual_key("key", "ctx"), "ctx\u{4}key");
        assert_eq!(contextual_key("key", ""), "key");
    }
}
