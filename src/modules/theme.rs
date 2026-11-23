// Theme Management module.
//
// Split into submodules; the full module documentation lives in `module.rs`.

pub mod types;
pub mod state;
pub mod validation;
mod handlers;
pub mod module;

pub use self::types::{ThemeTemplates, ThemeManifest, ThemeInfo};
pub use self::state::{ThemeState};
pub use self::validation::{validate_theme_id, validate_theme};
pub(crate) use self::validation::{SCREENSHOT_EXTENSIONS, check_template_file, screenshot_present,
    template_files, embedded_default_info, discover_themes};
pub(crate) use self::handlers::{list_themes, get_theme, activate_theme, get_active_theme,
    on_theme_exists, on_theme_render_block, on_theme_activate_requested};
pub use self::module::{ThemeModule, register};
