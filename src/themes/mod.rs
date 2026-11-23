// Server-rendered theme shell: Handlebars templates, embedded theme
// assets, and the static asset structure used by the dashboard.
// The dashboard itself is Handlebars pages plus TypeScript sources
// (`assets/js/*.ts`), transpiled to JavaScript in-memory at startup.

pub mod assets;
pub mod engine;
pub mod module_icons;
pub mod module_templates;
pub mod validity;
pub mod variables;

pub use engine::ThemeEngine;
pub use module_templates::{
    MODULE_TEMPLATE_PREFIX, ModuleTemplateRegistry, compile_template_source,
    module_template_key, parse_module_template_key, validate_module_template_name,
};
pub use validity::{ThemeIssue, ThemeValidationReport, check_template_syntax, validate_theme_directory};
pub use variables::{
    PageInfo, SiteInfo, TenantInfo, UserInfo, VariableContext, VariableDescriptor,
    VariableFailure, VariableRegistry, VariableResolver,
};
