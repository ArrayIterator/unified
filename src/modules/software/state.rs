//! In-memory software store.

use unified_core::hooks::{HookBus};
use unified_core::json;
use dashmap::DashMap;
use semver::Version;
use serde_json::{json, Value};
use std::collections::{HashSet};
use std::sync::{Arc, RwLock};
use super::types::{Release, ReleaseFile, Software, SoftwareDetail, SoftwareVersion, Tag, TaxonomyNode};
use super::handlers_versions::release_files;

// State

pub struct SoftwareState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) software: DashMap<String, Software>,
    pub(crate) versions: DashMap<String, SoftwareVersion>,
    pub(crate) releases: DashMap<String, Release>,
    pub(crate) release_files: DashMap<String, ReleaseFile>,
    pub(crate) taxonomy: DashMap<String, TaxonomyNode>,
    pub(crate) tags: DashMap<String, Tag>,
    /// software id -> assigned tag ids.
    pub(crate) software_tags: DashMap<String, HashSet<String>>,
}

impl SoftwareState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            software: DashMap::new(),
            versions: DashMap::new(),
            releases: DashMap::new(),
            release_files: DashMap::new(),
            taxonomy: DashMap::new(),
            tags: DashMap::new(),
            software_tags: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    pub(crate) fn software_detail(&self, software: &Software) -> SoftwareDetail {
        let tag_ids = self
            .software_tags
            .get(&software.id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let mut tags: Vec<Tag> = tag_ids
            .iter()
            .filter_map(|id| self.tags.get(id).map(|entry| entry.value().clone()))
            .collect();
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        let versions: Vec<SoftwareVersion> = self
            .versions
            .iter()
            .filter(|entry| entry.value().software_id.as_deref() == Some(software.id.as_str()))
            .map(|entry| entry.value().clone())
            .collect();
        let version_ids: HashSet<&str> = versions.iter().map(|v| v.id.as_str()).collect();
        let latest_version = versions
            .iter()
            .filter_map(|v| Version::parse(&v.version).ok().map(|parsed| (parsed, v.version.clone())))
            .max()
            .map(|(_, version)| version);
        let mut release_count = 0u64;
        let mut total_downloads = 0u64;
        for entry in self.releases.iter() {
            if version_ids.contains(entry.value().version_id.as_str()) {
                release_count += 1;
                total_downloads += entry.value().download_count;
            }
        }
        SoftwareDetail {
            software: software.clone(),
            tags,
            latest_version,
            release_count,
            total_downloads,
        }
    }

    /// True when a version with the same (software_id, product,
    /// version) already exists.
    pub(crate) fn has_version(
        &self,
        software_id: Option<&str>,
        product: &str,
        version: &str,
    ) -> bool {
        self.versions.iter().any(|entry| {
            let existing = entry.value();
            existing.software_id.as_deref() == software_id
                && existing.product == product
                && existing.version == version
        })
    }

    /// True when the referenced company exists. When no
    /// `company.exists` handler is registered the reference is
    /// accepted unchecked, because this module cannot see the
    /// company store directly.
    pub(crate) fn vendor_exists(&self, company_id: &str) -> bool {
        if !self.bus().has("company.exists") {
            return true;
        }
        let context = self.bus().emit(
            "company.exists",
            json!({ "company_id": company_id }),
        );
        context.results.iter().any(|result| {
            result.get("exists").and_then(|v| v.as_bool()).unwrap_or(false)
        })
    }
}
