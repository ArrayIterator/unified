//! email templates - render.

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

pub const EVENT_TEMPLATE_RENDER_REQUESTED: &str = "template.render_requested";

pub const EVENT_MAIL_SEND_REQUESTED: &str = "mail.send_requested";

/// `{{ name }}`, `{{ first-name }}` and `{{ user.name }}` placeholders.
/// Dotted paths resolve into nested variable objects; hyphens are part
/// of a variable name, so a hyphenated placeholder is substituted (or
/// recorded as missing) instead of being silently left untouched.
pub(crate) fn placeholder_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"\{\{\s*([a-zA-Z0-9_.-]+)\s*\}\}")
            .expect("placeholder regex must compile")
    })
}

/// Escape a substituted value so user-supplied data can never break out
/// of the template HTML.
pub(crate) fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Resolve a dotted variable path (`user.name`) against the variables map.
pub(crate) fn lookup_variable<'a>(variables: &'a HashMap<String, Value>, path: &str) -> Option<&'a Value> {
    let mut current: Option<&Value> = None;
    for (index, part) in path.split('.').enumerate() {
        current = if index == 0 {
            variables.get(part)
        } else {
            current.and_then(|value| value.get(part))
        };
        if current.is_none() {
            return None;
        }
    }
    current
}

/// Render a JSON value as plain text for substitution.
pub(crate) fn value_as_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Find every placeholder in a text, sorted and de-duplicated.
pub(crate) fn extract_variables(text: &str) -> Vec<String> {
    let mut vars: Vec<String> = placeholder_regex()
        .captures_iter(text)
        .filter_map(|caps| caps.get(1).map(|m| m.as_str().to_string()))
        .collect();
    vars.sort();
    vars.dedup();
    vars
}

/// Substitute placeholders in `text`. Unknown placeholders are left
/// untouched and recorded in `missing`.
pub(crate) fn substitute(
    text: &str,
    variables: &HashMap<String, Value>,
    missing: &mut Vec<String>,
) -> String {
    let regex = placeholder_regex();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in regex.captures_iter(text) {
        let whole = caps.get(0).expect("placeholder match");
        out.push_str(&text[last..whole.start()]);
        let key = caps.get(1).expect("placeholder group").as_str();
        match lookup_variable(variables, key) {
            Some(value) => out.push_str(&escape_html(&value_as_text(value))),
            None => {
                if !missing.iter().any(|item| item == key) {
                    missing.push(key.to_string());
                }
                out.push_str(whole.as_str());
            }
        }
        last = whole.end();
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn variables(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    #[test]
    fn escape_html_escapes_markup_characters() {
        assert_eq!(escape_html("<a>&\"'"), "&lt;a&gt;&amp;&quot;&#x27;");
    }

    #[test]
    fn escape_html_leaves_plain_text_alone() {
        assert_eq!(escape_html("hello"), "hello");
    }

    #[test]
    fn lookup_variable_resolves_nested_paths() {
        let data = variables(&[("user", json!({"name": "Ana"}))]);
        assert_eq!(lookup_variable(&data, "user.name"), Some(&json!("Ana")));
        assert_eq!(lookup_variable(&data, "user.missing"), None);
        assert_eq!(lookup_variable(&data, "missing"), None);
    }

    #[test]
    fn value_as_text_renders_each_json_kind() {
        assert_eq!(value_as_text(&Value::Null), "");
        assert_eq!(value_as_text(&json!("hi")), "hi");
        assert_eq!(value_as_text(&json!(true)), "true");
        assert_eq!(value_as_text(&json!(42)), "42");
        assert_eq!(value_as_text(&json!([1, 2])), "[1,2]");
    }

    #[test]
    fn extract_variables_finds_sorted_unique_placeholders() {
        assert_eq!(
            extract_variables("{{ b }} and {{a}} and {{ b }}"),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn extract_variables_finds_none_in_plain_text() {
        assert!(extract_variables("hello").is_empty());
    }

    #[test]
    fn substitute_replaces_known_placeholders_and_escapes_values() {
        let data = variables(&[("name", json!("<Ana>"))]);
        let mut missing = Vec::new();
        let out = substitute("Hi {{ name }}!", &data, &mut missing);
        assert_eq!(out, "Hi &lt;Ana&gt;!");
        assert!(missing.is_empty());
    }

    #[test]
    fn substitute_leaves_unknown_placeholders_and_records_them() {
        let data = variables(&[]);
        let mut missing = Vec::new();
        let out = substitute("Hi {{ name }}!", &data, &mut missing);
        assert_eq!(out, "Hi {{ name }}!");
        assert_eq!(missing, vec!["name".to_string()]);
    }

    #[test]
    fn substitute_records_each_missing_placeholder_once() {
        let data = variables(&[]);
        let mut missing = Vec::new();
        let out = substitute("{{ a }} {{ a }}", &data, &mut missing);
        assert_eq!(out, "{{ a }} {{ a }}");
        assert_eq!(missing, vec!["a".to_string()]);
    }

    #[test]
    fn placeholder_regex_matches_dotted_paths_with_spaces() {
        let regex = placeholder_regex();
        let caps = regex.captures("{{ user.name }}").expect("placeholder matches");
        assert_eq!(caps.get(1).unwrap().as_str(), "user.name");
    }

    #[test]
    fn extract_variables_finds_hyphenated_placeholders() {
        assert_eq!(
            extract_variables("Hello {{ first-name }}, see {{ user.first-name }}"),
            vec!["first-name".to_string(), "user.first-name".to_string()]
        );
    }

    #[test]
    fn substitute_replaces_known_hyphenated_placeholders() {
        let data = variables(&[("first-name", json!("Ana"))]);
        let mut missing = Vec::new();
        let out = substitute("Hi {{ first-name }}!", &data, &mut missing);
        assert_eq!(out, "Hi Ana!");
        assert!(missing.is_empty());
    }

    #[test]
    fn substitute_records_unknown_hyphenated_placeholders_as_missing() {
        let data = variables(&[]);
        let mut missing = Vec::new();
        let out = substitute("Hi {{ first-name }}!", &data, &mut missing);
        assert_eq!(out, "Hi {{ first-name }}!");
        assert_eq!(missing, vec!["first-name".to_string()]);
    }

    #[test]
    fn placeholder_regex_matches_hyphenated_dotted_paths() {
        let regex = placeholder_regex();
        let caps = regex
            .captures("{{ user.first-name }}")
            .expect("placeholder matches");
        assert_eq!(caps.get(1).unwrap().as_str(), "user.first-name");
    }
}
