// Theme state: discovered themes and the active theme id.

use unified_core::error::{UnifiedError, UnifiedResult};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use super::*;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ThemeState {
    pub themes_dir: PathBuf,
    pub active: Arc<RwLock<String>>,
    pub state_file: PathBuf,
}

impl ThemeState {
    pub fn new(themes_dir: PathBuf, state_file: PathBuf) -> Self {
        Self {
            themes_dir,
            active: Arc::new(RwLock::new("default".to_string())),
            state_file,
        }
    }

    pub fn active_id(&self) -> String {
        self.active.read().expect("theme lock poisoned").clone()
    }

    pub fn set_active(&self, id: &str) {
        *self.active.write().expect("theme lock poisoned") = id.to_string();
    }

    /// Load the persisted active theme id. An unknown or malformed id
    /// falls back to the embedded default theme.
    pub fn load_active(&self) {
        let Ok(text) = std::fs::read_to_string(&self.state_file) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        if let Some(id) = value.get("active").and_then(|value| value.as_str()) {
            if validate_theme_id(id).is_ok() {
                self.set_active(id);
            }
        }
    }

    pub async fn persist_active(&self) -> UnifiedResult<()> {
        let text = serde_json::to_string_pretty(&json!({ "active": self.active_id() }))
            .map_err(UnifiedError::from)?;
        if let Some(parent) = self.state_file.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(UnifiedError::from)?;
        }
        let temp = self.state_file.with_extension("json.tmp");
        tokio::fs::write(&temp, text.as_bytes())
            .await
            .map_err(UnifiedError::from)?;
        tokio::fs::rename(&temp, &self.state_file)
            .await
            .map_err(UnifiedError::from)?;
        Ok(())
    }

    /// Synchronous version of `persist_active`, for the synchronous
    /// `theme.activate_requested` hook handler (hook handlers cannot
    /// await). Same format and same atomic write-temp-then-rename.
    pub fn persist_active_sync(&self) -> UnifiedResult<()> {
        let text = serde_json::to_string_pretty(&json!({ "active": self.active_id() }))
            .map_err(UnifiedError::from)?;
        if let Some(parent) = self.state_file.parent() {
            std::fs::create_dir_all(parent).map_err(UnifiedError::from)?;
        }
        let temp = self.state_file.with_extension("json.tmp");
        std::fs::write(&temp, text.as_bytes()).map_err(UnifiedError::from)?;
        std::fs::rename(&temp, &self.state_file).map_err(UnifiedError::from)?;
        Ok(())
    }
}

impl Default for ThemeState {
    fn default() -> Self {
        Self::new(
            PathBuf::from("themes"),
            PathBuf::from("data/theme.json"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn theme_state_defaults_to_default_theme() {
        let state = ThemeState::default();
        assert_eq!(state.active_id(), "default");
        assert_eq!(state.themes_dir, PathBuf::from("themes"));
        assert_eq!(state.state_file, PathBuf::from("data/theme.json"));
    }

    #[test]
    fn theme_state_set_active_changes_active_id() {
        let state = ThemeState::new(PathBuf::from("themes"), PathBuf::from("data/theme.json"));
        state.set_active("alpha");
        assert_eq!(state.active_id(), "alpha");
        state.set_active("default");
        assert_eq!(state.active_id(), "default");
    }

    #[test]
    fn persist_active_sync_writes_active_id_as_json() {
        let dir = std::env::temp_dir().join(format!("unified-theme-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let state_file = dir.join("theme.json");
        let state = ThemeState::new(dir.join("themes"), state_file.clone());
        state.set_active("alpha");
        state.persist_active_sync().expect("persist");
        let text = std::fs::read_to_string(&state_file).expect("read state file");
        let value: serde_json::Value = serde_json::from_str(&text).expect("state file is JSON");
        assert_eq!(value["active"], "alpha");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
