// Company module - multi-site, multi-company management.
//
// A company owns sites. A site's domain is IMMUTABLE after creation:
// any update that tries to change it is rejected with HTTP 409.
// Exactly one site per company is primary: marking a site primary
// demotes every other site of the same company.

// Company module root.
//
// The module is split into one file per concern under
// `src/modules/company/`. The public API is unchanged: every item
// previously reachable as `crate::modules::company::Item` is
// re-exported here.

pub mod store;
mod requests;
mod handlers;
pub mod module;

pub use self::store::*;
pub use self::module::*;
pub(crate) use self::store::*;
pub(crate) use self::requests::*;
pub(crate) use self::handlers::*;
pub(crate) use self::module::*;
