// Global Search module.

// Split into per-concern files under `src/modules/search/`; the full
// module documentation is preserved at the top of `module.rs`.

pub mod ai_capabilities;
pub mod constants;
pub mod types;
pub mod store;
mod service;
mod handlers;
mod hooks;
pub mod module;

// Re-exported from `constants`.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::SEARCH_KINDS;
pub use self::constants::MAX_DOCUMENTS;

// Re-exported from `types`.
pub use self::types::SearchDocument;
pub use self::types::IndexDocumentRequest;
pub use self::types::SearchHit;
pub use self::types::KindGroup;
pub use self::types::SearchResponse;
pub use self::types::SuggestResponse;
pub use self::types::SearchStats;

// Re-exported from `store`.
pub use self::store::SearchState;

// Re-exported from `module`.
pub use self::module::SearchModule;
pub use self::module::register;
