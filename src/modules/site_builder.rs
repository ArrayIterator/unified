// Site Builder module.

// Split into per-concern files under `src/modules/site_builder/`; the full
// module documentation is preserved at the top of `module.rs`.

pub mod ai_capabilities;
pub mod constants;
pub mod types;
pub mod store;
mod helpers;
mod validation;
mod rendering;
mod handlers;
pub mod module;

// Re-exported from `constants`.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::EVENT_THEME_EXISTS;
pub use self::constants::EVENT_THEME_RENDER_BLOCK;
pub use self::constants::EVENT_RENDER_PAGE;
pub use self::constants::EVENT_PAGE_PUBLISHED;
pub use self::constants::EVENT_PAGE_UNPUBLISHED;
pub use self::constants::BLOCK_TYPES;
pub use self::constants::SECTION_LAYOUTS;

// Re-exported from `types`.
pub use self::types::Block;
pub use self::types::Section;
pub use self::types::Page;

// Re-exported from `store`.
pub use self::store::SiteBuilderState;

// Re-exported from `module`.
pub use self::module::SiteBuilderModule;
pub use self::module::register;
