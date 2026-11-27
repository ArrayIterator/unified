// Content Management module.
//
// Posts, categories, tags, and pages with pagination, filtering, and
// search, slug generation with uniqueness guarantees, a draft ->
// published flow that emits the `content.published` hook, and
// automatic excerpt generation from the body text. adds
// admin-definable custom post types (one generic entry store, dynamic
// field definitions validated on write) and the tags taxonomy shared
// by posts and custom entries.

pub mod ai_capabilities;
mod categories;
mod custom_types;
pub mod helpers;
pub mod module;
mod pages;
mod posts;
pub mod state;
mod tags;
pub mod types;

// Re-exported from `types`.
pub use self::types::AssignTagsInput;
pub use self::types::Category;
pub use self::types::ContentStatus;
pub use self::types::CreateCategoryInput;
pub use self::types::CreateCustomEntryInput;
pub use self::types::CreatePageInput;
pub use self::types::CreatePostInput;
pub use self::types::CreatePostTypeInput;
pub use self::types::CreateTagInput;
pub use self::types::CustomEntry;
pub use self::types::CustomFieldDef;
pub use self::types::CustomFieldType;
pub use self::types::Page;
pub use self::types::Post;
pub use self::types::PostTagLink;
pub use self::types::PostTypeDefinition;
pub use self::types::PostTypeVisibility;
pub use self::types::Tag;
pub use self::types::UpdateCategoryInput;
pub use self::types::UpdateCustomEntryInput;
pub use self::types::UpdatePageInput;
pub use self::types::UpdatePostInput;
pub use self::types::UpdatePostTypeInput;
pub use self::types::UpdateTagInput;
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
