//! tenants - constants.

use super::{vocabulary::*, events::*, settings::*, records::*, state::*, auth::*, helpers::*, provisioning::*, destroy::*, handlers::*, module::*, tenancy::*};


/// Module id, as registered in the module registry.
pub const MODULE_ID: &str = "tenants";


/// Capabilities enforced on every route (via the `role.check` hook,
/// fail closed: no answer means not allowed).
pub const PERM_TENANTS_VIEW: &str = "tenants:view";

pub const PERM_TENANTS_MANAGE: &str = "tenants:manage";

pub const PERM_TENANTS_PROVISION: &str = "tenants:provision";

pub const PERM_TENANTS_SUSPEND: &str = "tenants:suspend";
