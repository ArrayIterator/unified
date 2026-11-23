//! Saved Views module — Saved Filters / Views module.
//!
//! Split into submodules by concern; the public API is
//! re-exported below. The full route map and module notes
//! live in `saved_views/module.rs`.

pub mod types;
pub mod state;
pub mod helpers;
mod validation;
mod service;
mod handlers;
pub mod module;

pub use self::types::{ApplyViewResponse, CreateSavedView, SavedView, UpdateSavedView,
    VISIBILITY_PRIVATE, VISIBILITY_SHARED};
pub use self::state::{SavedViewsState};
pub(crate) use self::helpers::{
    PERM_SAVED_VIEWS_MANAGE, PERM_SAVED_VIEWS_VIEW, caller, caller_company, check_capability,
    require_caller_company, require_permission, require_tenant, require_view_owner,
    require_view_visible,
};
pub(crate) use self::validation::{validate_filter, validate_filter_value, validate_module_id,
    validate_name, validate_new, validate_order, validate_sort, validate_visibility};
pub(crate) use self::service::{get_view, is_visible_to, matches_search, paginate};
pub(crate) use self::handlers::{apply_view, create_view, delete_view, get_default_view,
    get_view_handler, list_views, set_default_view, update_view};
pub use self::module::{SavedViewsModule, register};
