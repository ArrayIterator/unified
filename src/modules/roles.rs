// Roles module - the Role Manager.

// Split into one file per concern under `src/modules/roles/`;
// this root only declares the submodules and re-exports the
// public API so every item stays reachable as `crate::modules::roles::Item`.

pub mod store;
pub mod types;
pub mod module;
pub mod capabilities;
mod seeds;
mod validation;
mod handlers;

pub use self::store::{check_role, find_role, permission_allowed};
pub use self::types::{CheckRoleInput, CheckRoleResponse, CreateRoleInput, RoleRecord, RolesStore, UpdateRoleInput};
pub use self::module::{RolesModule, register};
pub use self::capabilities::{KNOWN_CAPABILITIES, capability_defined};
pub(crate) use self::seeds::{default_roles};
pub(crate) use self::validation::{empty_permissions, valid_slug, validate_permissions};
pub(crate) use self::handlers::{check_permission, create_role, delete_role, get_role, get_role_permissions, list_roles, update_role};
