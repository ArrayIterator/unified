//! extensions - types.

use unified_core::modules::ExternalLibraryRequirement;
use crate::modules::service_monitor::{library_notice_message, LibraryCheckResult};
use serde::Serialize;

/// One required module and its current state, as reported by the
/// module-list and requirement-check endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct RequiredModuleInfo {
    pub id: String,
    /// Display name when the required module is registered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `"enabled"`, `"disabled"`, or `"missing"`.
    pub status: String,
    /// False when no module with this id is registered.
    pub exists: bool,
}

/// One admin-UI warning about a module's requirements. The `code` and
/// the `required_modules` / `will_auto_activate` lists are shaped so
/// the admin UI can render a notice or a popup before enabling, and
/// can tell the operator exactly what enabling would auto-activate.
#[derive(Debug, Clone, Serialize)]
pub struct DependencyWarning {
    /// `"missing_dependency"` or `"disabled_dependency"`.
    pub code: String,
    pub message: String,
    pub required_modules: Vec<String>,
    pub will_auto_activate: Vec<String>,
}

/// One admin-UI notice about a required external library, built from
/// the Service Monitor's latest check results. Only failing results
/// produce a notice; a library that is installed and new enough, or
/// one that has never been checked, produces none. The `message` is
/// the human-readable form, for example
/// `"ImageMagick 7+ required, 6.9.11 found"`.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryNotice {
    pub library: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_version: Option<String>,
    /// `"missing"` or `"outdated"`.
    pub status: String,
    pub message: String,
}

/// Notices for one module from the latest local library check
/// results. The `results` argument is the shared cache read, so
/// every caller sees the same data as the Service Monitor endpoints.
pub(crate) fn library_notices_for(
    module_id: &str,
    requirements: &[ExternalLibraryRequirement],
    results: &[LibraryCheckResult],
) -> Vec<LibraryNotice> {
    let mut notices: Vec<LibraryNotice> = Vec::new();
    for requirement in requirements {
        let Some(result) = results
            .iter()
            .find(|result| result.library.eq_ignore_ascii_case(requirement.name))
            .filter(|result| result.required_by.iter().any(|id| id == module_id))
        else {
            continue;
        };
        if let Some(message) = library_notice_message(result) {
            notices.push(LibraryNotice {
                library: result.library.clone(),
                required_version: result.required_version.clone(),
                found_version: result.found_version.clone(),
                status: result.status.clone(),
                message,
            });
        }
    }
    notices.sort_by(|a, b| a.library.cmp(&b.library));
    notices
}

/// Module summary plus its enabled state and dependency information.
#[derive(Debug, Clone, Serialize)]
pub struct ExtensionInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub is_core: bool,
    /// Merged required module ids (`requires` plus legacy
    /// `dependencies`).
    pub dependencies: Vec<String>,
    /// Each required module with its current status.
    pub requires: Vec<RequiredModuleInfo>,
    pub requires_libraries: Vec<String>,
    /// Declared external system libraries / binaries.
    pub requires_external_libraries: Vec<ExternalLibraryRequirement>,
    /// Admin-UI notices for required external libraries that the
    /// latest check found missing or outdated (empty when everything
    /// required is installed and new enough, or not yet checked).
    pub library_notices: Vec<LibraryNotice>,
    /// Required module ids that are not registered at all.
    pub missing_required: Vec<String>,
    /// Admin-UI warnings for this module's requirements.
    pub warnings: Vec<DependencyWarning>,
    pub enabled: bool,
}

/// Response of a successful enable: the module plus every module
/// that was auto-activated as one of its requirements.
#[derive(Debug, Clone, Serialize)]
pub struct EnableResponse {
    pub module: ExtensionInfo,
    pub auto_activated: Vec<String>,
}

/// Response of a successful disable: the module plus every dependent
/// module that was disabled along with it (`force=true` only).
/// Note the known limitation documented on `disable_with_check`: a
/// successful disable only records the module as disabled -- its
/// routes stay mounted and keep serving until the process restarts.
/// A reloadable route table that unmounts routes on disable is a
/// follow-up; it is not part of this response.
#[derive(Debug, Clone, Serialize)]
pub struct DisableResponse {
    pub module: ExtensionInfo,
    pub disabled_dependents: Vec<String>,
}

/// Full requirement check for one module, before enabling it.
#[derive(Debug, Clone, Serialize)]
pub struct RequirementCheck {
    pub id: String,
    pub name: String,
    /// `"ok"` when every requirement is registered and enabled,
    /// `"missing"` when at least one requirement is not registered,
    /// `"disabled"` when requirements exist but at least one is
    /// currently disabled.
    pub status: String,
    pub requires: Vec<RequiredModuleInfo>,
    pub requires_libraries: Vec<String>,
    /// Declared external system libraries / binaries.
    pub requires_external_libraries: Vec<ExternalLibraryRequirement>,
    /// Admin-UI notices for required external libraries that the
    /// latest check found missing or outdated.
    pub library_notices: Vec<LibraryNotice>,
    pub missing_required: Vec<String>,
    pub disabled_required: Vec<String>,
    /// Modules enabling this module would auto-activate, in
    /// activation order (deepest dependency first).
    pub will_auto_activate: Vec<String>,
    pub warnings: Vec<DependencyWarning>,
}

/// One hook event and the modules that handle it.
#[derive(Debug, Clone, Serialize)]
pub struct HookEventInfo {
    pub event: String,
    pub owners: Vec<String>,
    pub handler_count: usize,
}
