// Backup module - configuration snapshot and path helpers.
//
// Split from `src/modules/backup.rs`; the module root re-exports the public API of this module.

use unified_core::config::AppConfig;
use unified_core::json;
use path_clean::PathClean;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::constants::{MAX_STDERR_CHARS, SECRET_KEY_FRAGMENTS};
use super::types::{BackupSettings};

// ---------------------------------------------------------------------------
// Helpers: settings, redaction, paths.
// ---------------------------------------------------------------------------

/// Read the `backup` section of the configuration, falling back to
/// defaults for every missing key so the module keeps working before
/// the section is added to the configuration file.
pub(crate) fn backup_settings(config: &AppConfig) -> BackupSettings {
    let defaults = BackupSettings::default();
    let Ok(value) = serde_json::to_value(config) else {
        return defaults;
    };
    let Some(section) = value.get("backup") else {
        return defaults;
    };
    let get_string = |key: &str, fallback: &str| {
        section
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| fallback.to_string())
    };
    let get_u64 = |key: &str, fallback: u64| {
        section
            .get(key)
            .and_then(|value| value.as_u64())
            .unwrap_or(fallback)
    };
    let get_u32 = |key: &str, fallback: u32| {
        section
            .get(key)
            .and_then(|value| value.as_u64())
            .map(|value| value as u32)
            .unwrap_or(fallback)
    };
    BackupSettings {
        base_dir: get_string("base_dir", &defaults.base_dir),
        drill_dir: get_string("drill_dir", &defaults.drill_dir),
        max_total_bytes: get_u64("max_total_bytes", defaults.max_total_bytes),
        max_file_bytes: get_u64("max_file_bytes", defaults.max_file_bytes),
        default_retention_keep_last: get_u32(
            "default_retention_keep_last",
            defaults.default_retention_keep_last,
        ),
        default_retention_keep_days: get_u32(
            "default_retention_keep_days",
            defaults.default_retention_keep_days,
        ),
        verify_interval_hours: get_u64(
            "verify_interval_hours",
            defaults.verify_interval_hours,
        ),
        pg_dump_bin: get_string("pg_dump_bin", &defaults.pg_dump_bin),
        psql_bin: get_string("psql_bin", &defaults.psql_bin),
    }
}

/// Fragments that must match a whole token of the key name (split on
/// non-alphanumeric characters) instead of a substring, so that e.g.
/// `monkey`, `turkey`, `keyboard` and `author` are not treated as
/// secrets while `api_key`, `access_key`, `encryption_key`, `apikey`
/// and `auth` are.
const BOUNDARY_FRAGMENTS: &[&str] = &["key", "auth", "apikey"];

/// Decide whether a configuration key holds a secret value. Long
/// fragments match as substrings; the short fragments in
/// [`BOUNDARY_FRAGMENTS`] match only whole tokens; fragments that
/// contain an underscore (e.g. `api_key`, `database_url`) match as
/// substrings, which is safe because no ordinary word contains them.
pub(crate) fn is_secret_key(key: &str) -> bool {
    let lowered = key.to_lowercase();
    SECRET_KEY_FRAGMENTS.iter().any(|fragment| {
        if BOUNDARY_FRAGMENTS.contains(fragment) {
            lowered
                .split(|c: char| !c.is_alphanumeric())
                .any(|token| token == *fragment)
        } else {
            lowered.contains(fragment)
        }
    })
}

/// True when a string value is a URL carrying credentials in its
/// userinfo part, e.g. `postgres://user:pass@host/db`. Such values
/// are redacted whatever key they sit under.
fn looks_like_credential_url(value: &str) -> bool {
    let Some(after_scheme) = value.split("://").nth(1) else {
        return false;
    };
    let authority = after_scheme.split('/').next().unwrap_or("");
    authority.contains('@')
}

/// Replace every secret value in a configuration document with
/// `********`. A key is secret when [`is_secret_key`] says so; a
/// string value that itself looks like a credential-bearing URL is
/// also redacted whatever key it sits under.
pub(crate) fn redact_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut redacted = serde_json::Map::new();
            for (key, child) in map {
                if is_secret_key(&key) {
                    redacted.insert(key, Value::String("********".to_string()));
                } else {
                    redacted.insert(key, redact_value(child));
                }
            }
            Value::Object(redacted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact_value).collect()),
        Value::String(text) if looks_like_credential_url(&text) => {
            Value::String("********".to_string())
        }
        other => other,
    }
}

/// Build the redacted configuration snapshot for an archive from an
/// already-redacted full configuration document. When `keys` is empty
/// the whole configuration is snapshotted; otherwise only the
/// requested top-level sections or dotted paths are kept.
pub(crate) fn snapshot_config(full: &Value, keys: &[String]) -> Value {
    if keys.is_empty() {
        return full.clone();
    }
    let mut out = serde_json::Map::new();
    for key in keys {
        let segments: Vec<&str> = key.split('.').collect();
        if let Some(found) = get_dotted_value(&full, &segments) {
            set_dotted_value(&mut out, &segments, found.clone());
        }
    }
    Value::Object(out)
}

pub(crate) fn get_dotted_value<'a>(value: &'a Value, segments: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in segments {
        current = current.get(*segment)?;
    }
    Some(current)
}

pub(crate) fn set_dotted_value(map: &mut serde_json::Map<String, Value>, segments: &[&str], value: Value) {
    if segments.is_empty() {
        return;
    }
    if segments.len() == 1 {
        map.insert(segments[0].to_string(), value);
        return;
    }
    let entry = map
        .entry(segments[0].to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(child) = entry {
        set_dotted_value(child, &segments[1..], value);
    }
}

/// Resolve a possibly relative path against the working directory and
/// clean it lexically (no filesystem access).
pub(crate) fn absolute_clean(path: &str) -> PathBuf {
    let raw = Path::new(path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(raw)
    };
    joined.clean()
}

/// Lexical containment check used at job creation time: the candidate
/// path must be the base directory itself or live inside it.
pub(crate) fn path_inside_base(candidate: &str, base_dir: &str) -> bool {
    let base = absolute_clean(base_dir);
    let candidate = absolute_clean(candidate);
    candidate == base || candidate.starts_with(&base)
}

// ---------------------------------------------------------------------------
// Helpers: external binaries.
// ---------------------------------------------------------------------------

/// Find an executable by name on `PATH`, or use it directly when it is
/// already a path. Returns `None` when no usable file is found.
pub(crate) fn find_binary(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if name.contains('/') {
        if candidate.is_file() {
            return Some(candidate.to_path_buf());
        }
        return None;
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub(crate) fn truncate_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.chars().count() > MAX_STDERR_CHARS {
        text.chars().take(MAX_STDERR_CHARS).collect()
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_value_hides_secrets() {
        let value = redact_value(json!({
            "api_key": "x",
            "name": "y",
            "nested": {"password": "p"},
            "list": [{"token": "t"}, 1]
        }));
        assert_eq!(value["api_key"], json!("********"));
        assert_eq!(value["name"], json!("y"));
        assert_eq!(value["nested"]["password"], json!("********"));
        assert_eq!(value["list"][0]["token"], json!("********"));
        assert_eq!(value["list"][1], json!(1));
    }

    #[test]
    fn redact_value_hides_key_variants() {
        let value = redact_value(json!({
            "api_key": "x",
            "API_KEY": "x",
            "access_key": "x",
            "encryption_key": "x",
            "signing_key": "x",
            "apikey": "x",
            "api-key": "x",
            "auth": "x",
            "key": "x",
            "database_url": "postgres://u:p@h/db",
            "dsn": "postgres://u:p@h/db",
            "connection_string": "Server=h;User=u;Password=p",
            "monkey": "keep",
            "turkey": "keep",
            "keyboard": "keep",
            "author": "keep",
            "name": "keep"
        }));
        for key in [
            "api_key",
            "API_KEY",
            "access_key",
            "encryption_key",
            "signing_key",
            "apikey",
            "api-key",
            "auth",
            "key",
            "database_url",
            "dsn",
            "connection_string",
        ] {
            assert_eq!(value[key], json!("********"), "key {key} was not redacted");
        }
        for key in ["monkey", "turkey", "keyboard", "author", "name"] {
            assert_eq!(value[key], json!("keep"), "key {key} was wrongly redacted");
        }
    }

    #[test]
    fn redact_value_hides_credential_urls() {
        let value = redact_value(json!({
            "endpoint": "postgres://user:pass@db.internal:5432/app",
            "website": "https://example.com/no-auth",
            "list": ["mysql://root:secret@db/x", "plain"]
        }));
        assert_eq!(value["endpoint"], json!("********"));
        assert_eq!(value["website"], json!("https://example.com/no-auth"));
        assert_eq!(value["list"][0], json!("********"));
        assert_eq!(value["list"][1], json!("plain"));
    }

    #[test]
    fn snapshot_config_selects_keys() {
        let full = json!({"a": {"b": 1}, "c": 2});
        assert_eq!(snapshot_config(&full, &[]), full);
        assert_eq!(
            snapshot_config(&full, &["a.b".to_string()]),
            json!({"a": {"b": 1}})
        );
        assert_eq!(snapshot_config(&full, &["missing".to_string()]), json!({}));
    }

    #[test]
    fn dotted_value_round_trip() {
        let value = json!({"a": {"b": 42}});
        assert_eq!(get_dotted_value(&value, &["a", "b"]), Some(&json!(42)));
        assert_eq!(get_dotted_value(&value, &["a", "missing"]), None);
        let mut map = serde_json::Map::new();
        set_dotted_value(&mut map, &["a", "b"], json!(7));
        assert_eq!(map["a"]["b"], json!(7));
        set_dotted_value(&mut map, &[], json!(9));
        assert_eq!(map["a"]["b"], json!(7));
    }

    #[test]
    fn path_inside_base_checks_containment() {
        assert!(path_inside_base("/tmp/base/file.txt", "/tmp/base"));
        assert!(path_inside_base("/tmp/base", "/tmp/base"));
        assert!(!path_inside_base("/tmp/base2/x", "/tmp/base"));
        assert!(!path_inside_base("/etc/passwd", "/tmp/base"));
    }

    #[test]
    fn truncate_stderr_limits_length() {
        assert_eq!(truncate_stderr(b"oops"), "oops");
        assert_eq!(truncate_stderr(b"  padded  "), "padded");
        assert_eq!(truncate_stderr(&vec![b'x'; 600]).len(), 500);
    }
}
