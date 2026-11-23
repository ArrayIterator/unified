// Content Management module.
//
// Posts, categories, and pages with pagination, filtering, and search,
// slug generation with uniqueness guarantees, a draft -> published flow
// that emits the `content.published` hook, and automatic excerpt
// generation from the body text.

pub mod ai_capabilities;
mod categories;
pub mod helpers;
pub mod module;
mod pages;
mod posts;
pub mod state;
pub mod types;

// Re-exported from `types`.
pub use self::types::Category;
pub use self::types::ContentStatus;
pub use self::types::CreateCategoryInput;
pub use self::types::CreatePageInput;
pub use self::types::CreatePostInput;
pub use self::types::Page;
pub use self::types::Post;
pub use self::types::UpdateCategoryInput;
pub use self::types::UpdatePageInput;
pub use self::types::UpdatePostInput;
// Re-exported from `state`.
pub use self::state::ContentState;
// Re-exported from `helpers`.
pub use self::helpers::make_excerpt;
pub use self::helpers::slugify;
// Re-exported from `ai_capabilities`.
pub use self::ai_capabilities::AI_CAPABILITIES;
// Re-exported from `module`.
pub use self::module::register;
pub use self::module::ContentModule;
