// i18n -- Shared in-memory state.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::Utc;
use dashmap::DashMap;
use serde_json::Map;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::languages::*;
use super::types::*;

/// Monotonic counter mixed into temporary persist file names so two
/// concurrent persists for the same language never share a temp file.
static PERSIST_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temporary sibling for `target`: the process id plus a
/// process-wide counter, so concurrent persists for the same language
/// (and the same target) always write different files before the
/// atomic rename. A stale temp file keeps a `.tmp` suffix, so
/// `load_from_disk` (which only reads `.json`) never mistakes one
/// for a translation file.
fn temp_path_for(target: &PathBuf) -> PathBuf {
    let unique = PERSIST_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(&format!(".{}.{unique}.tmp", std::process::id()));
    target.with_file_name(name)
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct I18nState {
    /// Keyed by (language code, translation key).
    pub translations: Arc<DashMap<(String, String), Translation>>,
    /// Plural forms, keyed by (language code, singular key). Forms are
    /// stored in CLDR plural-form order; a contextual entry uses the same
    /// `\x04`-joined key as `contextual_key`. The first form is the
    /// singular form (`msgstr[0]` in gettext terms).
    pub plurals: Arc<DashMap<(String, String), Vec<String>>>,
    pub dir: PathBuf,
}

impl I18nState {
    pub fn new(dir: PathBuf) -> Self {
        let state = Self {
            translations: Arc::new(DashMap::new()),
            plurals: Arc::new(DashMap::new()),
            dir,
        };
        state.seed_defaults();
        state
    }

    /// A small set of built-in English strings so the default language
    /// is usable before any override is stored. Other languages start
    /// empty until overrides or files provide them.
    pub(crate) fn seed_defaults(&self) {
        let defaults: &[(&str, &str)] = &[
            ("app.name", "Unified"),
            ("nav.dashboard", "Dashboard"),
            ("nav.settings", "Settings"),
            ("common.save", "Save"),
            ("common.cancel", "Cancel"),
            ("common.search", "Search"),
            ("common.delete", "Delete"),
            ("auth.login", "Sign in"),
            ("auth.logout", "Sign out"),
        ];
        let now = Utc::now();
        for (key, value) in defaults {
            self.translations.insert(
                ("en".to_string(), (*key).to_string()),
                Translation {
                    lang: "en".to_string(),
                    key: (*key).to_string(),
                    value: (*value).to_string(),
                    updated_at: now,
                },
            );
        }
    }

    pub(crate) fn file_path(&self, lang: &str) -> PathBuf {
        self.dir.join(format!("{lang}.json"))
    }

    /// Load every `<lang>.json` file from the translations directory.
    /// Files win over the built-in seed values.
    pub fn load_from_disk(&self) -> UnifiedResult<()> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(UnifiedError::from(error)),
        };
        for entry in entries {
            let entry = entry.map_err(UnifiedError::from)?;
            let path = entry.path();
            let is_json = path
                .extension()
                .is_some_and(|extension| extension == "json");
            if !is_json {
                continue;
            }
            let Some(lang) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if !is_builtin_language(&lang) {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(UnifiedError::from)?;
            let values: Map<String, Value> =
                serde_json::from_str(&text).map_err(UnifiedError::from)?;
            let now = Utc::now();
            for (key, value) in values {
                if let Some(text) = value.as_str() {
                    self.translations.insert(
                        (lang.clone(), key.clone()),
                        Translation {
                            lang: lang.clone(),
                            key,
                            value: text.to_string(),
                            updated_at: now,
                        },
                    );
                }
            }
            // Plural forms live in a sidecar file so a NUL-joined value
            // never leaks into a singular lookup.
            let plurals_path = self.dir.join(format!("{lang}.plurals.json"));
            if let Ok(text) = std::fs::read_to_string(&plurals_path) {
                if let Ok(plurals) = serde_json::from_str::<Map<String, Value>>(&text) {
                    for (key, value) in plurals {
                        if let Some(forms) = value.as_array() {
                            let forms: Vec<String> = forms
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect();
                            if !forms.is_empty() {
                                self.plurals.insert((lang.clone(), key), forms);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Persist one language to its JSON file, atomically enough for this
    /// store: write a temporary file, then rename it over the target.
    /// The temporary file name is unique per call (process id plus a
    /// counter), so concurrent persists for the same language cannot
    /// overwrite each other's in-flight file.
    pub async fn persist_lang(&self, lang: &str) -> UnifiedResult<()> {
        let mut values: BTreeMap<String, String> = BTreeMap::new();
        for entry in self.translations.iter() {
            let translation = entry.value();
            if translation.lang == lang {
                values.insert(translation.key.clone(), translation.value.clone());
            }
        }
        let text = serde_json::to_string_pretty(&values).map_err(UnifiedError::from)?;
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(UnifiedError::from)?;
        let target = self.file_path(lang);
        let temp = temp_path_for(&target);
        if let Err(error) = tokio::fs::write(&temp, text.as_bytes()).await {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(UnifiedError::from(error));
        }
        if let Err(error) = tokio::fs::rename(&temp, &target).await {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(UnifiedError::from(error));
        }
        // Persist plural forms to the `<lang>.plurals.json` sidecar.
        let mut plural_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in self.plurals.iter() {
            let (entry_lang, key) = entry.key();
            if entry_lang == lang {
                plural_values.insert(key.clone(), entry.value().clone());
            }
        }
        let plurals_target = self.dir.join(format!("{lang}.plurals.json"));
        if plural_values.is_empty() {
            let _ = tokio::fs::remove_file(&plurals_target).await;
        } else {
            let plurals_text =
                serde_json::to_string_pretty(&plural_values).map_err(UnifiedError::from)?;
            let plurals_temp = temp_path_for(&plurals_target);
            if let Err(error) = tokio::fs::write(&plurals_temp, plurals_text.as_bytes()).await {
                let _ = tokio::fs::remove_file(&plurals_temp).await;
                return Err(UnifiedError::from(error));
            }
            if let Err(error) = tokio::fs::rename(&plurals_temp, &plurals_target).await {
                let _ = tokio::fs::remove_file(&plurals_temp).await;
                return Err(UnifiedError::from(error));
            }
        }
        Ok(())
    }
}

impl Default for I18nState {
    fn default() -> Self {
        Self::new(PathBuf::from("data/i18n"))
    }
}

// Tests for persistence in this file.
#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("unified-i18n-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn no_temp_files_left(dir: &PathBuf) -> bool {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries.flatten().all(|entry| {
                    !entry
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".tmp")
                })
            })
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn persist_lang_writes_file_and_leaves_no_temp_file() {
        let dir = test_dir("write");
        let state = I18nState::new(dir.clone());
        state.persist_lang("en").await.expect("persist succeeds");
        let text = std::fs::read_to_string(dir.join("en.json")).expect("file exists");
        let values: Map<String, Value> = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(values.get("app.name").and_then(Value::as_str), Some("Unified"));
        assert!(no_temp_files_left(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn persist_lang_concurrent_persists_do_not_collide() {
        let dir = test_dir("concurrent");
        let state = I18nState::new(dir.clone());
        // Unique temp names mean both in-flight writes land in
        // different files; the renames serialize and both succeed.
        let (first, second) =
            tokio::join!(state.persist_lang("en"), state.persist_lang("en"));
        assert!(first.is_ok());
        assert!(second.is_ok());
        let text = std::fs::read_to_string(dir.join("en.json")).expect("file exists");
        let values: Map<String, Value> = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(values.get("app.name").and_then(Value::as_str), Some("Unified"));
        assert!(no_temp_files_left(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn temp_paths_are_unique_per_call() {
        let target = PathBuf::from("/tmp/i18n/en.json");
        let first = temp_path_for(&target);
        let second = temp_path_for(&target);
        assert_ne!(first, second);
        assert!(first.to_string_lossy().ends_with(".tmp"));
        assert!(first.to_string_lossy().starts_with("/tmp/i18n/en.json."));
    }
}
