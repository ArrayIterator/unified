// Form Builder module: form definitions with drag-drop field
// order, embed codes, a submissions inbox, server-side validation,
// spam protection, and submission mapping to CRM leads, support
// tickets, or tasks.
//
// Split into one file per concern. The full route list lives in
// `types.rs`; every public item is re-exported below so
// `crate::modules::forms::<Item>` keeps working unchanged.

pub mod ai_capabilities;
pub mod types;
pub mod store;
mod common;
mod validation;
mod submission;
pub mod module;
mod handlers_forms;
mod handlers_fields;
mod handlers_submissions;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::FormStatus;
pub use self::types::FieldCondition;
pub use self::types::FormMapping;
pub use self::types::Form;
pub use self::types::FormField;
pub use self::types::FormSubmission;
pub use self::types::FormSubmissionValue;
pub use self::types::CreateFieldRequest;
pub use self::types::UpdateFieldRequest;
pub use self::types::MappingRequest;
pub use self::types::CreateFormRequest;
pub use self::types::UpdateFormRequest;
pub use self::types::ReorderFieldsRequest;
pub use self::types::SubmitFormRequest;
pub use self::types::ExportQuery;
pub use self::store::FormsState;
pub use self::module::FormsModule;
pub use self::module::register;
