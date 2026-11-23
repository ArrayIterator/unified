// Software / Assets Manager module.
//
// Enterprise software asset manager: software records with a status
// lifecycle (draft -> published -> deprecated / archived), versions
// with semver validation and platform requirements, releases with a
// publish flow, per-release files with sha256/sha512 checksums and
// integrity verification, hierarchical taxonomy (groups and
// categories), tags, full-text discovery, and license-gated downloads:
// a release with `license_required` asks the licensing module through
// the `license.validate_requested` hook, and denies the download
// honestly when no validator answers or the key is invalid.
// Interconnection happens only through the hook bus.
//
// Persistence is in-memory (DashMap) in this module; the Diesel
// tables for durable storage are listed in the module report and are
// applied by a later wiring step.

pub mod ai_capabilities;
pub mod types;
pub mod state;
mod validation;
mod helpers;
mod handlers_versions;
mod handlers_software;
mod handlers_files;
mod handlers_taxonomy;
mod hooks;
mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::SOFTWARE_KINDS;
pub use self::types::SOFTWARE_STATUSES;
pub use self::types::TAXONOMY_KINDS;
pub use self::types::INTEGRITY_STATUSES;
pub use self::types::PERM_SOFTWARE_READ;
pub use self::types::PERM_SOFTWARE_CREATE;
pub use self::types::PERM_SOFTWARE_MANAGE;
pub use self::types::VersionRequirements;
pub use self::types::SoftwareVersion;
pub use self::types::CreateVersion;
pub use self::types::UpdateVersion;
pub use self::types::Release;
pub use self::types::CreateRelease;
pub use self::types::Software;
pub use self::types::CreateSoftware;
pub use self::types::UpdateSoftware;
pub use self::types::SoftwareDetail;
pub use self::types::ReleaseFile;
pub use self::types::CreateReleaseFile;
pub use self::types::UpdateReleaseFile;
pub use self::types::VerifyFile;
pub use self::types::TaxonomyNode;
pub use self::types::TaxonomyTreeNode;
pub use self::types::CreateTaxonomyNode;
pub use self::types::UpdateTaxonomyNode;
pub use self::types::MoveTaxonomyNode;
pub use self::types::Tag;
pub use self::types::CreateTag;
pub use self::types::UpdateTag;
pub use self::types::TagCount;
pub use self::types::AttachTag;
pub use self::types::LatestRelease;
pub use self::types::SoftwareReleaseHistory;
pub use self::state::SoftwareState;
pub use self::module::SoftwareModule;
pub use self::module::register;
