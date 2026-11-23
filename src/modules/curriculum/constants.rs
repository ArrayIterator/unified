//! Curriculum module — competency-based school curricula. — Module constants.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::assessment_domains::*;
use super::basic_competencies::*;
use super::core_competencies::*;
use super::frameworks::*;
use super::grade_component_mappings::*;
use super::grade_descriptors::*;
use super::helpers::*;
use super::hooks::*;
use super::inputs::*;
use super::interventions::*;
use super::lesson_plans::*;
use super::levels::*;
use super::module::*;
use super::report_card_entries::*;
use super::report_cards::*;
use super::settings::*;
use super::state::*;
use super::structures::*;
use super::subjects::*;
use super::types::*;

/// Maximum lengths enforced on user supplied text.
pub(super) const MAX_CODE_LEN: usize = 64;
pub(super) const MAX_NAME_LEN: usize = 200;
pub(super) const MAX_TITLE_LEN: usize = 300;
pub(super) const MAX_TEXT_LEN: usize = 5000;
pub(super) const MAX_REF_LEN: usize = 128;

/// Scores are standard 0..=100 values. This is a standard constant,
/// not a business rule.
pub(super) const MIN_SCORE: f64 = 0.0;
pub(super) const MAX_SCORE: f64 = 100.0;

/// Capabilities checked through the `role.check` hook (fail closed).
pub(super) const PERM_CURRICULUM_VIEW: &str = "curriculum:view";
pub(super) const PERM_CURRICULUM_MANAGE: &str = "curriculum:manage";
pub(super) const PERM_CURRICULUM_TEACH: &str = "curriculum:teach";

/// Seeded default framework code. CONFIGURABLE DEFAULT: the admin
/// can change it with `POST /frameworks/{id}/set-default`; it only
/// picks which seeded framework row new records default to.
pub(super) const DEFAULT_FRAMEWORK_CODE: &str = "curriculum-2013";
/// Seeded default paper size for print requests. CONFIGURABLE
/// DEFAULT: editable through `PATCH /settings` (`paper_sizes` list
/// plus `default_paper_size`).
pub(super) const DEFAULT_PAPER_SIZE: &str = "a4";
