//! Version, URL, checksum, slug and taxonomy-tree helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpResponse};
use chrono::{DateTime, Utc};
use semver::Version;
use serde::{Serialize};
use serde_json::{json};
use sha2::{Digest, Sha256, Sha512};
use uuid::Uuid;
use super::types::{SOFTWARE_KINDS, TaxonomyNode, TaxonomyTreeNode, VersionRequirements};
use super::state::SoftwareState;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

pub(crate) fn validate_version_string(version: &str) -> UnifiedResult<()> {
    Version::parse(version).map(|_| ()).map_err(|_| {
        UnifiedError::bad_request(format!(
            "Version \"{version}\" is not a valid semantic version (expected MAJOR.MINOR.PATCH)"
        ))
    })
}

/// Validate a partial version such as `6.5` or `8.1` used for
/// WordPress / PHP minimum requirements: one to three dot-separated
/// numeric components.
pub(crate) fn validate_partial_version(value: &str, field: &str) -> UnifiedResult<()> {
    let parts: Vec<&str> = value.trim().split('.').collect();
    let valid = !parts.is_empty()
        && parts.len() <= 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()));
    if valid {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "{field} \"{value}\" is not a valid version (expected e.g. 6.5 or 8.1)"
        )))
    }
}

pub(crate) fn validate_requirements(requirements: &VersionRequirements) -> UnifiedResult<()> {
    if let Some(wordpress) = &requirements.wordpress {
        validate_partial_version(wordpress, "WordPress version")?;
    }
    if let Some(php) = &requirements.php {
        validate_partial_version(php, "PHP version")?;
    }
    if let Some(platform) = &requirements.platform {
        if platform.trim().is_empty() {
            return Err(UnifiedError::bad_request("Platform must not be empty"));
        }
    }
    for dependency in &requirements.dependencies {
        if dependency.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Dependencies must not contain empty entries",
            ));
        }
    }
    Ok(())
}

pub(crate) fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

pub(crate) fn validate_kind(kind: &str) -> UnifiedResult<()> {
    if SOFTWARE_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Kind must be one of: {}",
            SOFTWARE_KINDS.join(", ")
        )))
    }
}

pub(crate) fn validate_status_transition(from: &str, to: &str) -> UnifiedResult<()> {
    if from == to {
        return Err(UnifiedError::conflict(format!(
            "Software is already {from}"
        )));
    }
    let allowed = match from {
        "draft" => ["published", "archived"].as_slice(),
        "published" => ["deprecated", "archived", "draft"].as_slice(),
        "deprecated" => ["published", "archived", "draft"].as_slice(),
        "archived" => ["draft"].as_slice(),
        _ => &[],
    };
    if allowed.contains(&to) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Cannot move software from {from} to {to}"
        )))
    }
}

pub(crate) fn validate_url(value: &str, field: &str) -> UnifiedResult<()> {
    if value.starts_with("http://") || value.starts_with("https://") {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "{field} must start with http:// or https://"
        )))
    }
}

pub(crate) fn validate_hex_checksum(value: &str, expected_len: usize, field: &str) -> UnifiedResult<()> {
    let valid = value.len() == expected_len
        && value.chars().all(|ch| ch.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "{field} must be {expected_len} hex characters"
        )))
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(crate) fn sha512_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha512::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(crate) fn taxonomy_path(state: &SoftwareState, parent_id: Option<&str>, slug: &str) -> (i64, String) {
    match parent_id.and_then(|id| state.taxonomy.get(id)) {
        Some(parent) => (
            parent.depth + 1,
            format!("{}/{}", parent.path, slug),
        ),
        None => (0, format!("/{slug}")),
    }
}

/// Recompute depth and path for a node and every descendant after a move.
pub(crate) fn recompute_subtree(state: &SoftwareState, root_id: &str) {
    let mut queue: Vec<String> = vec![root_id.to_string()];
    while let Some(id) = queue.pop() {
        let (parent_id, slug) = match state.taxonomy.get(&id) {
            Some(node) => (node.parent_id.clone(), node.slug.clone()),
            None => continue,
        };
        let (depth, path) = taxonomy_path(state, parent_id.as_deref(), &slug);
        if let Some(mut node) = state.taxonomy.get_mut(&id) {
            node.depth = depth;
            node.path = path;
            node.updated_at = now();
        }
        let children: Vec<String> = state
            .taxonomy
            .iter()
            .filter(|entry| entry.value().parent_id.as_deref() == Some(id.as_str()))
            .map(|entry| entry.key().clone())
            .collect();
        queue.extend(children);
    }
}

pub(crate) fn build_tree(
    state: &SoftwareState,
    parent_id: Option<&str>,
    kind: Option<&str>,
) -> Vec<TaxonomyTreeNode> {
    let mut nodes: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .filter(|entry| {
            entry.value().parent_id.as_deref() == parent_id
                && kind.is_none_or(|kind| entry.value().kind == kind)
        })
        .map(|entry| entry.value().clone())
        .collect();
    nodes.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    nodes
        .into_iter()
        .map(|node| TaxonomyTreeNode {
            children: build_tree(state, Some(node.id.as_str()), kind),
            node,
        })
        .collect()
}

pub(crate) fn is_descendant_of(state: &SoftwareState, node_id: &str, ancestor_id: &str) -> bool {
    let mut current = state
        .taxonomy
        .get(node_id)
        .and_then(|node| node.parent_id.clone());
    while let Some(id) = current {
        if id == ancestor_id {
            return true;
        }
        current = state
            .taxonomy
            .get(&id)
            .and_then(|node| node.parent_id.clone());
    }
    false
}

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>, debug: bool) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requirements(wordpress: Option<&str>, php: Option<&str>) -> VersionRequirements {
        VersionRequirements {
            wordpress: wordpress.map(str::to_string),
            php: php.map(str::to_string),
            platform: None,
            dependencies: Vec::new(),
        }
    }

    #[test]
    fn validate_version_string_accepts_semver() {
        assert!(validate_version_string("1.2.3").is_ok());
        assert!(validate_version_string("0.0.1").is_ok());
        assert!(validate_version_string("1.2.3-beta.1").is_ok());
        assert!(validate_version_string("1.2").is_err());
        assert!(validate_version_string("v1.2.3").is_err());
        assert!(validate_version_string("").is_err());
    }

    #[test]
    fn validate_partial_version_accepts_one_to_three_components() {
        assert!(validate_partial_version("6.5", "WordPress version").is_ok());
        assert!(validate_partial_version("8", "PHP version").is_ok());
        assert!(validate_partial_version("8.1.2", "PHP version").is_ok());
        assert!(validate_partial_version("6.5.4.1", "WordPress version").is_err());
        assert!(validate_partial_version("6.x", "WordPress version").is_err());
        assert!(validate_partial_version("", "WordPress version").is_err());
    }

    #[test]
    fn validate_requirements_rejects_bad_entries() {
        assert!(validate_requirements(&requirements(Some("6.5"), Some("8.1"))).is_ok());
        assert!(validate_requirements(&requirements(Some("6.x"), None)).is_err());
        let mut bad = requirements(None, None);
        bad.dependencies = vec!["".to_string()];
        assert!(validate_requirements(&bad).is_err());
        let mut empty_platform = requirements(None, None);
        empty_platform.platform = Some("   ".to_string());
        assert!(validate_requirements(&empty_platform).is_err());
    }

    #[test]
    fn slugify_normalizes_names() {
        assert_eq!(slugify("My Plugin v2"), "my-plugin-v2");
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn validate_kind_accepts_known_kinds() {
        for kind in SOFTWARE_KINDS {
            assert!(validate_kind(kind).is_ok(), "kind {kind}");
        }
        assert!(validate_kind("app").is_err());
    }

    #[test]
    fn validate_status_transition_rules() {
        assert!(validate_status_transition("draft", "published").is_ok());
        assert!(validate_status_transition("published", "draft").is_ok());
        assert!(validate_status_transition("archived", "draft").is_ok());
        assert!(validate_status_transition("published", "deprecated").is_ok());
        assert!(validate_status_transition("archived", "published").is_err());
        assert!(validate_status_transition("draft", "draft").is_err());
    }

    #[test]
    fn validate_url_requires_http_scheme() {
        assert!(validate_url("https://example.com", "homepage").is_ok());
        assert!(validate_url("http://example.com", "homepage").is_ok());
        assert!(validate_url("ftp://example.com", "homepage").is_err());
        assert!(validate_url("example.com", "homepage").is_err());
    }

    #[test]
    fn validate_hex_checksum_checks_length_and_digits() {
        assert!(validate_hex_checksum("aabbcc", 6, "sha256").is_ok());
        assert!(validate_hex_checksum("abc", 6, "sha256").is_err());
        assert!(validate_hex_checksum("zz", 2, "sha256").is_err());
    }

    #[test]
    fn validate_hex_checksum_accepts_uppercase_and_names_hex_not_lowercase() {
        // The check accepts any ASCII hex digit, so the message must
        // say "hex characters", not "lowercase hex characters".
        assert!(validate_hex_checksum("AABBCC", 6, "sha256").is_ok());
        let error = validate_hex_checksum("zz", 6, "sha256").unwrap_err();
        assert!(error.message.contains("6 hex characters"));
        assert!(!error.message.contains("lowercase"));
    }

    #[test]
    fn sha_helpers_match_known_digests() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha512_hex(b""),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        assert_eq!(
            sha512_hex(b"abc"),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }
}
