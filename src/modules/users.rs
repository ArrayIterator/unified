// Users module.
//
// Full user CRUD under `/api/v1/users`, with pagination, filtering and
// search. Passwords are hashed with `crate::auth::password::hash` and the
// password policy from the security module is enforced on create and on
// password change.
//
// Persistence: when a database pool is attached to the `AppContext`, users
// are stored in the Diesel `users` table (and role assignments in the
// `user_roles` table). When no pool is present, users live in an in-memory
// store instead - the fallback is documented here and in the module
// description so a deployment without a database still works, while a
// deployment with a database never silently loses data.
//
// Interconnection: `user.created` and `user.updated` hooks are emitted on
// create/update (and `user.deleted` on delete); assigning a role emits a
// `role.check` hook so the roles module can validate the role.

pub mod ai_capabilities;
pub mod types;
pub mod module;
pub mod requests;
mod validation;
mod hooks;
mod handlers;

// Public API (unchanged): re-exported from the split files.
// From types.rs:
pub use self::ai_capabilities::AI_CAPABILITIES;
// Role lookup for the auth middleware (crate-internal): the
// middleware attaches server-resolved role slugs to the verified
// caller. Mirrors the `pub(crate) use self::hooks::{...}` pattern
// of the domain_verification module.
pub(crate) use self::hooks::db_role_slugs;
// Company lookup for the auth middleware (crate-internal): the
// middleware attaches the server-resolved company id to the
// verified caller, mirroring `db_role_slugs` above.
pub(crate) use self::hooks::db_user_company;
pub use self::types::StoredUser;
pub use self::types::UserResponse;
pub use self::types::UsersStore;
// From module.rs:
pub use self::module::UsersModule;
pub use self::module::register;
// From requests.rs:
pub use self::requests::AssignRoleInput;
pub use self::requests::CreateUserInput;
pub use self::requests::UpdateUserInput;
pub use self::requests::UserRolesResponse;
