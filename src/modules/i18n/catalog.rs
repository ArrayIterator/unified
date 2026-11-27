// i18n -- Gettext (.po) catalogue parsing and loading.

use chrono::Utc;
use serde::Serialize;

use super::locale::*;
use super::state::*;
use super::types::*;

// Minimal gettext (.po) catalogue parsing (no extra dependency).
//
// Supported: `msgid`, `msgid_plural`, `msgctxt`, `msgstr`, `msgstr[n]`,
// continuation lines, `\n`/`\t`/`\"`/`\\` escapes, and `#, fuzzy`
// markers (fuzzy entries are skipped). The header entry (`msgid ""`)
// is used only for its `Plural-Forms: nplurals=N` value. This is a
// pragmatic subset for importing translator-produced catalogues, not a
// full .po implementation: obsolete (`#~`) entries and previous-msgid
// (`#|`) comments are ignored.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoEntry {
    pub msgid: String,
    pub msgid_plural: Option<String>,
    pub msgctxt: Option<String>,
    pub msgstr: String,
    pub msgstr_plural: Vec<String>,
    pub fuzzy: bool,
}

pub(crate) fn unescape_po_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Extract the quoted string from a line such as `msgid "Hello"` or a
/// bare continuation line `"more"`. Returns `None` when the line holds
/// no quoted string.
pub(crate) fn po_quoted(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let end = line.rfind('"')?;
    if end <= start {
        return None;
    }
    Some(unescape_po_string(&line[start + 1..end]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PoField {
    Msgid,
    MsgidPlural,
    Msgctxt,
    Msgstr,
    MsgstrPlural(usize),
    None,
}

pub fn parse_po_catalog(content: &str) -> Vec<PoEntry> {
    let mut entries: Vec<PoEntry> = Vec::new();
    let mut current = PoEntry {
        msgid: String::new(),
        msgid_plural: None,
        msgctxt: None,
        msgstr: String::new(),
        msgstr_plural: Vec::new(),
        fuzzy: false,
    };
    let mut has_entry = false;
    let mut field = PoField::None;
    let mut flush = |current: &mut PoEntry, has_entry: &mut bool, entries: &mut Vec<PoEntry>| {
        if *has_entry {
            entries.push(std::mem::replace(
                current,
                PoEntry {
                    msgid: String::new(),
                    msgid_plural: None,
                    msgctxt: None,
                    msgstr: String::new(),
                    msgstr_plural: Vec::new(),
                    fuzzy: false,
                },
            ));
            *has_entry = false;
        }
    };
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            flush(&mut current, &mut has_entry, &mut entries);
            field = PoField::None;
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            if comment.starts_with(',') && comment.contains("fuzzy") {
                current.fuzzy = true;
            }
            continue;
        }
        if line.starts_with('"') {
            if let Some(text) = po_quoted(line) {
                match field {
                    PoField::Msgid => current.msgid.push_str(&text),
                    PoField::MsgidPlural => {
                        current
                            .msgid_plural
                            .get_or_insert_with(String::new)
                            .push_str(&text);
                    }
                    PoField::Msgctxt => {
                        current
                            .msgctxt
                            .get_or_insert_with(String::new)
                            .push_str(&text);
                    }
                    PoField::Msgstr => current.msgstr.push_str(&text),
                    PoField::MsgstrPlural(index) => {
                        while current.msgstr_plural.len() <= index {
                            current.msgstr_plural.push(String::new());
                        }
                        current.msgstr_plural[index].push_str(&text);
                    }
                    PoField::None => {}
                }
            }
            continue;
        }
        if line.starts_with("msgid_plural") {
            field = PoField::MsgidPlural;
            current.msgid_plural = po_quoted(line);
            has_entry = true;
        } else if line.starts_with("msgid") {
            // Flush only when a previous entry actually started: a
            // `msgctxt` line precedes its `msgid` in standard files, so
            // flushing on every `msgid` would split the context off.
            if !current.msgid.is_empty() {
                flush(&mut current, &mut has_entry, &mut entries);
            }
            field = PoField::Msgid;
            current.msgid = po_quoted(line).unwrap_or_default();
            has_entry = true;
        } else if line.starts_with("msgctxt") {
            field = PoField::Msgctxt;
            current.msgctxt = po_quoted(line);
            has_entry = true;
        } else if line.starts_with("msgstr[") {
            let index = line
                .strip_prefix("msgstr[")
                .and_then(|rest| rest.split(']').next())
                .and_then(|digits| digits.parse::<usize>().ok())
                .unwrap_or(0);
            field = PoField::MsgstrPlural(index);
            while current.msgstr_plural.len() <= index {
                current.msgstr_plural.push(String::new());
            }
            current.msgstr_plural[index] = po_quoted(line).unwrap_or_default();
            has_entry = true;
        } else if line.starts_with("msgstr") {
            field = PoField::Msgstr;
            current.msgstr = po_quoted(line).unwrap_or_default();
            has_entry = true;
        }
    }
    flush(&mut current, &mut has_entry, &mut entries);
    entries
}

/// `nplurals` from a header entry's `Plural-Forms` line, if present.
pub(crate) fn po_header_nplurals(header_msgstr: &str) -> Option<usize> {
    for line in header_msgstr.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Plural-Forms:") {
            for part in rest.split(';') {
                let part = part.trim();
                if let Some(value) = part.strip_prefix("nplurals=") {
                    return value.trim().parse::<usize>().ok();
                }
            }
        }
    }
    None
}

/// The `plural=...` expression from a header entry's `Plural-Forms`
/// line, if present, with whitespace removed for matching.
pub(crate) fn po_header_plural_expr(header_msgstr: &str) -> Option<String> {
    for line in header_msgstr.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Plural-Forms:") {
            for part in rest.split(';') {
                let part = part.trim();
                if let Some(value) = part.strip_prefix("plural=") {
                    let expr: String =
                        value.chars().filter(|c| !c.is_whitespace()).collect();
                    if !expr.is_empty() {
                        return Some(expr);
                    }
                }
            }
        }
    }
    None
}

/// Reorder a catalogue's plural forms into CLDR order, in place.
/// `translate_plural` indexes stored forms with the CLDR index
/// (`plural_form_index`), so a catalogue whose gettext order differs
/// must be normalised at load time.
///
/// - Latvian (`lv`): CLDR orders the three forms (zero, one, other).
///   Gettext catalogues use one of two orders, depending on the
///   `plural=` formula in the header. The older formula
///   `(n%10==1 && n%100!=11 ? 0 : n%10==0 || (n%100>=11 &&
///   n%100<=19) ? 1 : 2)` stores (one, zero, other), so the first
///   two forms are swapped. The current standard formula
///   `(n%10==1 && n%100!=11 ? 0 : n != 0 ? 1 : 2)` stores (one,
///   other, zero), so the forms rotate to (zero, one, other). When
///   the header carries no formula, the older (one, zero, other)
///   order is assumed, matching the order documented in
///   `locale::plural_form_index`.
/// - Irish (`ga`): audited, no normalisation needed - gettext
///   stores (one, two, few, many, other), which is already the
///   CLDR order. The `ga` note in `locale.rs` is about which form
///   count 0 *selects* (CLDR `other`, form 4), not about stored
///   order, and `plural_form_index` already selects it.
/// - Every other language: gettext and CLDR agree on form order
///   for integer counts (Polish's missing CLDR `other` is a
///   fallback case in `translate_plural`, not an ordering one).
pub(crate) fn normalise_plural_form_order(
    lang: &str,
    forms: &mut Vec<String>,
    plural_expr: Option<&str>,
) {
    if language_from_locale(lang) != "lv" || forms.len() != 3 {
        return;
    }
    // Current standard Latvian formula: file order (one, other,
    // zero) -> CLDR (zero, one, other) is a rotation.
    let is_current_formula = plural_expr
        .is_some_and(|expr| expr.contains("n!=0"));
    if is_current_formula {
        forms.rotate_left(2);
    } else {
        // Older formula (or no header): file order (one, zero,
        // other) -> CLDR (zero, one, other) is a swap of the first
        // two forms.
        forms.swap(0, 1);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PoLoadResult {
    pub loaded: usize,
    pub plural_entries: usize,
    pub skipped_fuzzy: usize,
    pub header_nplurals: Option<usize>,
}

impl I18nState {
    /// Load a gettext `.po` catalogue into the store. Fuzzy and empty
    /// entries are skipped; a plural entry stores its forms (singular
    /// first) in the plurals map and its singular form as the plain
    /// translation. Returns per-catalogue counts for the caller.
    pub fn load_po_catalog(&self, lang: &str, content: &str) -> PoLoadResult {
        let entries = parse_po_catalog(content);
        let mut result = PoLoadResult {
            loaded: 0,
            plural_entries: 0,
            skipped_fuzzy: 0,
            header_nplurals: None,
        };
        let mut header_plural_expr: Option<String> = None;
        let now = Utc::now();
        for entry in entries {
            if entry.msgid.is_empty() {
                result.header_nplurals = po_header_nplurals(&entry.msgstr);
                header_plural_expr = po_header_plural_expr(&entry.msgstr);
                continue;
            }
            if entry.fuzzy {
                result.skipped_fuzzy += 1;
                continue;
            }
            let key = match &entry.msgctxt {
                Some(context) => contextual_key(&entry.msgid, context),
                None => entry.msgid.clone(),
            };
            let has_plural = entry.msgid_plural.is_some() && !entry.msgstr_plural.is_empty();
            if has_plural {
                // Plural entries carry no `msgstr`: every form lives
                // in `msgstr[n]`, with `msgstr[0]` already the
                // singular form, so the stored forms are
                // `msgstr_plural` alone. Prepending `msgstr` (which
                // is always empty here) would shift every form by
                // one and leave the singular translation empty.
                // Forms are normalised into CLDR order first:
                // `translate_plural` indexes with the CLDR index,
                // so a Latvian catalogue stored in gettext order
                // would otherwise answer every count with the wrong
                // form (see `normalise_plural_form_order`).
                let mut forms = entry.msgstr_plural.clone();
                normalise_plural_form_order(lang, &mut forms, header_plural_expr.as_deref());
                self.plurals
                    .insert((lang.to_string(), key.clone()), forms.clone());
                result.plural_entries += 1;
                // The plain translation is the form a count of 1
                // selects (CLDR form 1 for Latvian, form 0 for
                // almost everything else), falling back to the
                // first stored form when the catalogue is short.
                let singular_index = plural_form_index(lang, 1.0);
                if let Some(singular) = forms
                    .get(singular_index)
                    .or_else(|| forms.first())
                    .filter(|form| !form.is_empty())
                {
                    self.translations.insert(
                        (lang.to_string(), key.clone()),
                        Translation {
                            lang: lang.to_string(),
                            key,
                            value: singular.clone(),
                            updated_at: now,
                        },
                    );
                    result.loaded += 1;
                }
            } else if !entry.msgstr.is_empty() {
                self.translations.insert(
                    (lang.to_string(), key.clone()),
                    Translation {
                        lang: lang.to_string(),
                        key,
                        value: entry.msgstr,
                        updated_at: now,
                    },
                );
                result.loaded += 1;
            }
        }
        result
    }

    /// Context-aware lookup: tries `context + key` first, then the bare
    /// key. Returns `None` when neither exists.
    pub fn translate_with_context(
        &self,
        lang: &str,
        key: &str,
        context: Option<&str>,
    ) -> Option<String> {
        if let Some(context) = context.filter(|value| !value.is_empty()) {
            let composite = contextual_key(key, context);
            if let Some(entry) = self.translations.get(&(lang.to_string(), composite)) {
                return Some(entry.value().value.clone());
            }
        }
        self.translations
            .get(&(lang.to_string(), key.to_string()))
            .map(|entry| entry.value().value.clone())
    }

    /// Plural-aware translation. Returns `(text, form_index, translated)`:
    /// the stored plural form when one exists for the count, the stored
    /// singular translation when the count selects form 0, and otherwise
    /// the caller-supplied singular/plural fallback, reported as
    /// untranslated.
    pub fn translate_plural(
        &self,
        lang: &str,
        singular: &str,
        plural: &str,
        count: f64,
        context: Option<&str>,
    ) -> (String, usize, bool) {
        let index = plural_form_index(lang, count);
        let key = match context.filter(|value| !value.is_empty()) {
            Some(context) => contextual_key(singular, context),
            None => singular.to_string(),
        };
        if let Some(forms) = self.plurals.get(&(lang.to_string(), key.clone())) {
            let forms = forms.value();
            if index < forms.len() {
                return (forms[index].clone(), index, true);
            }
            if index == 0 {
                return (forms[0].clone(), index, true);
            }
            // Stored forms exist but none covers this count (for
            // example a 3-form gettext catalogue asked for the CLDR
            // fourth Polish form): the caller-supplied plural is
            // returned as an untranslated fallback.
            return (plural.to_string(), index, false);
        }
        if index == 0 {
            if let Some(value) = self.translate_with_context(lang, singular, context) {
                return (value, index, true);
            }
            return (singular.to_string(), index, false);
        }
        (plural.to_string(), index, false)
    }
}

// Tests for the .po catalog parser in this file.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_parses_simple_entry() {
        let entries = parse_po_catalog("msgid \"Hello\"\nmsgstr \"Halo\"\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].msgid, "Hello");
        assert_eq!(entries[0].msgstr, "Halo");
        assert!(!entries[0].fuzzy);
    }

    #[test]
    fn catalog_parses_plural_forms() {
        let content = "msgid \"apple\"\nmsgid_plural \"apples\"\nmsgstr[0] \"apel\"\nmsgstr[1] \"apel-apel\"\n";
        let entries = parse_po_catalog(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].msgid_plural.as_deref(), Some("apples"));
        assert_eq!(
            entries[0].msgstr_plural,
            vec!["apel".to_string(), "apel-apel".to_string()]
        );
    }

    #[test]
    fn catalog_parses_context() {
        let entries = parse_po_catalog("msgctxt \"menu\"\nmsgid \"Open\"\nmsgstr \"Buka\"\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].msgctxt.as_deref(), Some("menu"));
        assert_eq!(entries[0].msgstr, "Buka");
    }

    #[test]
    fn catalog_marks_fuzzy_entries() {
        let entries = parse_po_catalog("#, fuzzy\nmsgid \"x\"\nmsgstr \"y\"\n");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].fuzzy);
    }

    #[test]
    fn catalog_unescapes_strings() {
        let entries = parse_po_catalog("msgid \"a\\nb\"\nmsgstr \"c\\\"d\"\n");
        assert_eq!(entries[0].msgid, "a\nb");
        assert_eq!(entries[0].msgstr, "c\"d");
    }

    #[test]
    fn header_plural_count_is_detected() {
        let content = "msgid \"\"\nmsgstr \"Plural-Forms: nplurals=2; plural=(n != 1);\\n\"\n";
        let entries = parse_po_catalog(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(po_header_nplurals(&entries[0].msgstr), Some(2));
    }

    fn test_state() -> I18nState {
        // Loading never touches disk; the directory only matters for
        // persistence, which these tests do not exercise.
        I18nState::new(std::path::PathBuf::from("data/i18n-test"))
    }

    #[test]
    fn load_po_catalog_stores_plural_forms_without_shift() {
        let state = test_state();
        let content = "msgid \"apple\"\nmsgid_plural \"apples\"\nmsgstr[0] \"apel\"\nmsgstr[1] \"apel-apel\"\n";
        let result = state.load_po_catalog("en", content);
        assert_eq!(result.loaded, 1);
        assert_eq!(result.plural_entries, 1);
        assert_eq!(result.skipped_fuzzy, 0);
        // The singular form is stored as the plain translation.
        assert_eq!(
            state.translate_with_context("en", "apple", None).as_deref(),
            Some("apel")
        );
        // Forms are indexed without the off-by-one shift: form 0 is
        // the singular, form 1 the plural.
        assert_eq!(
            state.translate_plural("en", "apple", "apples", 1.0, None),
            ("apel".to_string(), 0, true)
        );
        assert_eq!(
            state.translate_plural("en", "apple", "apples", 2.0, None),
            ("apel-apel".to_string(), 1, true)
        );
    }

    #[test]
    fn load_po_catalog_plural_with_empty_singular_form_is_not_loaded() {
        let state = test_state();
        let content = "msgid \"apple\"\nmsgid_plural \"apples\"\nmsgstr[0] \"\"\nmsgstr[1] \"apel-apel\"\n";
        let result = state.load_po_catalog("en", content);
        assert_eq!(result.plural_entries, 1);
        assert_eq!(result.loaded, 0);
        assert!(state
            .translate_with_context("en", "apple", None)
            .is_none());
    }

    #[test]
    fn load_po_catalog_empty_context_stores_bare_key() {
        let state = test_state();
        let content = "msgctxt \"\"\nmsgid \"Open\"\nmsgstr \"Buka\"\n";
        let result = state.load_po_catalog("en", content);
        assert_eq!(result.loaded, 1);
        // The entry is reachable under the bare key, including with
        // an empty context, because no unreachable contextual key
        // was created for it.
        assert_eq!(
            state.translate_with_context("en", "Open", None).as_deref(),
            Some("Buka")
        );
        assert_eq!(
            state
                .translate_with_context("en", "Open", Some(""))
                .as_deref(),
            Some("Buka")
        );
    }

    #[test]
    fn load_po_catalog_skips_fuzzy_and_header_entries() {
        let state = test_state();
        let content = "msgid \"\"\nmsgstr \"Plural-Forms: nplurals=2; plural=(n != 1);\\n\"\n\n#, fuzzy\nmsgid \"old\"\nmsgstr \"lama\"\n\nmsgid \"Hello\"\nmsgstr \"Halo\"\n";
        let result = state.load_po_catalog("en", content);
        assert_eq!(result.loaded, 1);
        assert_eq!(result.skipped_fuzzy, 1);
        assert_eq!(result.header_nplurals, Some(2));
        assert!(state
            .translate_with_context("en", "old", None)
            .is_none());
        assert_eq!(
            state.translate_with_context("en", "Hello", None).as_deref(),
            Some("Halo")
        );
    }

    #[test]
    fn translate_plural_out_of_range_fallback_is_untranslated() {
        let state = test_state();
        // A short catalogue: only the singular form was imported.
        state
            .plurals
            .insert(("en".to_string(), "apple".to_string()), vec!["apel".to_string()]);
        // The in-range form is translated.
        assert_eq!(
            state.translate_plural("en", "apple", "apples", 1.0, None),
            ("apel".to_string(), 0, true)
        );
        // The stored forms cover no form for this count, so the
        // caller-supplied plural is returned as untranslated.
        assert_eq!(
            state.translate_plural("en", "apple", "apples", 5.0, None),
            ("apples".to_string(), 1, false)
        );
    }

    #[test]
    fn translate_plural_without_any_forms_falls_back_untranslated() {
        let state = test_state();
        assert_eq!(
            state.translate_plural("en", "apple", "apples", 1.0, None),
            ("apple".to_string(), 0, false)
        );
        assert_eq!(
            state.translate_plural("en", "apple", "apples", 5.0, None),
            ("apples".to_string(), 1, false)
        );
    }

    #[test]
    fn translate_plural_polish_fourth_form_falls_back_on_three_form_catalogue() {
        let state = test_state();
        // A Polish gettext catalogue carries 3 forms (one/few/many);
        // the CLDR fourth form (`other`, fractions only) has no
        // stored form, so a fractional count falls back untranslated.
        let content = "msgid \"miesiac\"\nmsgid_plural \"miesiace\"\nmsgstr[0] \"miesiac\"\nmsgstr[1] \"miesiace\"\nmsgstr[2] \"miesiecy\"\n";
        let result = state.load_po_catalog("pl", content);
        assert_eq!(result.plural_entries, 1);
        assert_eq!(result.loaded, 1);
        assert_eq!(
            state.translate_plural("pl", "miesiac", "miesiace", 5.0, None),
            ("miesiecy".to_string(), 2, true)
        );
        assert_eq!(
            state.translate_plural("pl", "miesiac", "miesiace", 1.5, None),
            ("miesiace".to_string(), 3, false)
        );
    }

    #[test]
    fn load_po_catalog_normalises_latvian_plural_order() {
        let state = test_state();
        // Older Latvian gettext formula stores (one, zero, other);
        // CLDR indexes (zero, one, other). Without normalisation
        // count 0 would answer with the "one" form and count 1
        // with the "zero" form.
        let content = "msgid \"fails\"\nmsgid_plural \"faili\"\nmsgstr[0] \"ONE\"\nmsgstr[1] \"ZERO\"\nmsgstr[2] \"OTHER\"\n";
        let result = state.load_po_catalog("lv", content);
        assert_eq!(result.plural_entries, 1);
        assert_eq!(
            state.translate_plural("lv", "fails", "faili", 0.0, None),
            ("ZERO".to_string(), 0, true)
        );
        assert_eq!(
            state.translate_plural("lv", "fails", "faili", 1.0, None),
            ("ONE".to_string(), 1, true)
        );
        assert_eq!(
            state.translate_plural("lv", "fails", "faili", 2.0, None),
            ("OTHER".to_string(), 2, true)
        );
        // The plain (non-plural) translation is still the "one"
        // form, not the reordered first form.
        assert_eq!(
            state.translate_with_context("lv", "fails", None),
            Some("ONE".to_string())
        );
    }

    #[test]
    fn load_po_catalog_normalises_latvian_current_formula_order() {
        let state = test_state();
        // Current standard Latvian formula stores (one, other,
        // zero); the header declares it, so the loader rotates
        // instead of swapping.
        let content = "msgid \"\"\nmsgstr \"Plural-Forms: nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n != 0 ? 1 : 2);\\n\"\n\nmsgid \"fails\"\nmsgid_plural \"faili\"\nmsgstr[0] \"ONE\"\nmsgstr[1] \"OTHER\"\nmsgstr[2] \"ZERO\"\n";
        let result = state.load_po_catalog("lv", content);
        assert_eq!(result.header_nplurals, Some(3));
        assert_eq!(
            state.translate_plural("lv", "fails", "faili", 0.0, None),
            ("ZERO".to_string(), 0, true)
        );
        assert_eq!(
            state.translate_plural("lv", "fails", "faili", 1.0, None),
            ("ONE".to_string(), 1, true)
        );
        assert_eq!(
            state.translate_plural("lv", "fails", "faili", 2.0, None),
            ("OTHER".to_string(), 2, true)
        );
    }
}
