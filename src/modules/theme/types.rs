// Theme types: manifest, templates and theme info.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeTemplates {
    pub layout: String,
    pub index: Option<String>,
    pub post: Option<String>,
    pub page: Option<String>,
}

impl Default for ThemeTemplates {
    fn default() -> Self {
        Self {
            layout: "layout.html".to_string(),
            index: None,
            post: None,
            page: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub license: Option<String>,
    #[serde(default)]
    pub templates: ThemeTemplates,
    pub screenshot: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThemeInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub active: bool,
    pub source: String,
    pub path: Option<String>,
    pub screenshot: bool,
    pub templates: Vec<String>,
    pub valid: bool,
    pub problems: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_templates_default_to_layout_html() {
        let templates = ThemeTemplates::default();
        assert_eq!(templates.layout, "layout.html");
        assert_eq!(templates.index, None);
        assert_eq!(templates.post, None);
        assert_eq!(templates.page, None);
    }

    #[test]
    fn theme_manifest_deserializes_minimal_yaml() {
        let manifest: ThemeManifest = serde_yaml::from_str(
            "name: Minimal\nversion: 0.1.0\ndescription: Minimal theme.\n",
        )
        .expect("minimal manifest parses");
        assert_eq!(manifest.name, "Minimal");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.templates.layout, "layout.html");
        assert_eq!(manifest.author, None);
        assert_eq!(manifest.screenshot, None);
    }
}
