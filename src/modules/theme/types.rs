// Theme types: theme info for the admin API.
//
// The manifest itself lives in the rendering engine
// (`crate::themes::engine::ThemeManifest`): there is exactly one
// `theme.yaml` shape, parsed by the engine. This module used to parse
// a second, incompatible manifest shape (`templates: {layout, ...}`
// as `.html` files); it could never validate a real engine theme,
// so HTTP activation of any disk theme failed. Do not reintroduce a
// second manifest type here.

use crate::themes::engine::ThemeAssets;
use serde::Serialize;

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
    /// Capabilities the theme requires (e.g. `["react"]`).
    pub require: Vec<String>,
    /// UI framework the theme is built on (e.g. `"untitled"`).
    pub framework: String,
    /// Theme-relative JS/CSS libraries the theme loads.
    pub assets: ThemeAssets,
}
