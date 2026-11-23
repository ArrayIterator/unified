// CRM module: contacts and leads, a pipeline grouped by stage,
// lead conversion into a contact, and activities on contacts
// and leads. Interconnection happens only through the hook bus.
//
// Split into one file per concern; every public item is
// re-exported below so `crate::modules::crm::<Item>` keeps
// working unchanged.

pub mod ai_capabilities;
pub mod types;
pub mod store;
mod common;
mod handlers_contacts;
mod handlers_leads;
mod handlers_activities;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::Contact;
pub use self::types::CreateContact;
pub use self::types::UpdateContact;
pub use self::types::Lead;
pub use self::types::CreateLead;
pub use self::types::UpdateLead;
pub use self::types::Activity;
pub use self::types::CreateActivity;
pub use self::store::CrmState;
pub use self::module::CrmModule;
pub use self::module::register;
