//! Presets + Setup Wizard module. - SEED DATA - the four built-in presets as initial rows. This table is read exactly once, at startup (`init`), to insert rows into the preset store, and is never read again: every other line of this module reads presets from the store only. The same four rows, with the same ids, are the `INSERT` statements in migration `000070_presets`, so the database and the in-memory store agree. Editing a built-in preset happens by cloning it through the API; the seeded rows themselves are never special after startup. Every module id below was verified against the registered module list in `src/modules/mod.rs` (the modules the registry actually knows). Module ids named in the original request that are NOT registered there were dropped, and are listed here so the gap is visible instead of silent: - "properties" and "web3" (Property Seller): files exist on disk with those ids, but neither is registered in `mod.rs`, so the registry cannot enable them. - "timetracker", "todos", and "invoices" (Freelancer): "timetracker" and "todos" exist as files but are not registered in `mod.rs`; no "invoices" module exists at all, and "billing" is the real billing/invoice module. The registered "tasks" module stands in as the optional task-tracking module for freelancers. - "portals" and "workflows" (Agency): files exist on disk but are not registered in `mod.rs`. - "network_tools" (SaaS Seller): the file exists on disk but is not registered in `mod.rs`; the registered "network" module is a different module and was not substituted. Once those modules are registered in `mod.rs`, an admin can add them to any preset (or a clone) through the API - no code change.
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::apply::*;
use super::events::*;
use super::helpers::*;
use super::identity::*;
use super::lookups::*;
use super::preset_handlers::*;
use super::records::*;
use super::requests::*;
use super::stores::*;
use super::wizard_handlers::*;
use super::wizard_steps::*;

use chrono::Utc;
use serde_json::{Map, Value};
use uuid::Uuid;

/// Fixed ids for the seeded presets, shared with the migration
/// `INSERT`s so the database rows and the in-memory rows match.
pub const SEED_PROPERTY_SELLER_ID: &str = "10000000-0000-4000-8000-000000000001";
pub const SEED_FREELANCER_ID: &str = "10000000-0000-4000-8000-000000000002";
pub const SEED_AGENCY_ID: &str = "10000000-0000-4000-8000-000000000003";
pub const SEED_SAAS_SELLER_ID: &str = "10000000-0000-4000-8000-000000000004";

pub(super) fn seed_preset(
    id: &str,
    slug: &str,
    name: &str,
    description: &str,
    business_type: &str,
    modules: &[(&str, bool)],
) -> Preset {
    let now = Utc::now();
    Preset {
        id: Uuid::parse_str(id).expect("seed preset id is a valid UUID"),
        slug: slug.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        business_type: business_type.to_string(),
        built_in: true,
        archived: false,
        modules: modules
            .iter()
            .enumerate()
            .map(|(index, (module_id, required))| PresetModuleEntry {
                module_id: (*module_id).to_string(),
                required: *required,
                sort_order: index as i32,
            })
            .collect(),
        settings: Value::Object(Map::new()),
        steps: DEFAULT_WIZARD_STEPS
            .iter()
            .map(|step| (*step).to_string())
            .collect(),
        created_by: None,
        created_at: now,
        updated_at: now,
    }
}

/// The built-in presets as seed rows. See the block comment above.
pub(super) fn builtin_preset_seeds() -> Vec<Preset> {
    vec![
        seed_preset(
            SEED_PROPERTY_SELLER_ID,
            "property-seller",
            "Property Seller",
            "For businesses selling property: customer relations, billing, and notifications.",
            "property_seller",
            &[("crm", true), ("billing", true), ("notifications", false)],
        ),
        seed_preset(
            SEED_FREELANCER_ID,
            "freelancer",
            "Freelancer",
            "For independent professionals: projects, billing, and task tracking.",
            "freelancer",
            &[("projects", true), ("billing", true), ("tasks", false)],
        ),
        seed_preset(
            SEED_AGENCY_ID,
            "agency",
            "Agency",
            "For agencies: projects, customer relations, and support tickets.",
            "agency",
            &[("projects", true), ("crm", true), ("tickets", false)],
        ),
        seed_preset(
            SEED_SAAS_SELLER_ID,
            "saas-seller",
            "SaaS Seller",
            "For software sellers: store, licensing, billing, and the public API.",
            "saas_seller",
            &[
                ("store", true),
                ("licensing", true),
                ("billing", true),
                ("api_manager", false),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_preset_seeds_are_four_known_presets() {
        let seeds = builtin_preset_seeds();
        assert_eq!(seeds.len(), 4);
        let slugs: Vec<&str> = seeds.iter().map(|preset| preset.slug.as_str()).collect();
        assert!(slugs.contains(&"property-seller"));
        assert!(slugs.contains(&"freelancer"));
        assert!(slugs.contains(&"agency"));
        assert!(slugs.contains(&"saas-seller"));
        for preset in &seeds {
            assert!(!preset.modules.is_empty());
            assert!(!preset.name.is_empty());
            assert!(!preset.business_type.is_empty());
            assert!(valid_slug(&preset.slug));
        }
    }

    #[test]
    fn seed_ids_are_distinct() {
        let seeds = builtin_preset_seeds();
        let mut ids: Vec<Uuid> = seeds.iter().map(|preset| preset.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 4);
    }
}
