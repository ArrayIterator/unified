//! Storage - target config building and pagination.
//!
//! Part of the `storage` module split; see `src/modules/storage.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::pagination::PageResult;
use unified_core::pagination::PaginationParams;
use unified_core::storage::StorageKind;
use unified_core::storage::StorageTargetConfig;
use unified_core::storage::validate_target_name;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

use super::types::CreateTargetRequest;

/// Build a `StorageTargetConfig` from an add-target request: the
/// request `name` and `kind` win, the remaining fields come from the
/// request `config` object.
///
/// The top-level `name` and `kind` always win: the `config` entries
/// are merged first and the validated top-level fields are written
/// over them afterwards, so a `config` of `{"name": "other",
/// "kind": "s3"}` cannot silently rename the target or switch its
/// kind (the previous order merged `config` last and let it
/// override both).
pub(crate) fn target_config_from_request(
    request: &CreateTargetRequest,
) -> UnifiedResult<StorageTargetConfig> {
    validate_target_name(request.name.trim())?;
    let mut object = serde_json::Map::new();
    for (key, value) in &request.config {
        object.insert(key.clone(), value.clone());
    }
    object.insert(
        "name".to_string(),
        Value::String(request.name.trim().to_string()),
    );
    object.insert(
        "kind".to_string(),
        Value::String(request.kind.as_str().to_string()),
    );
    let config: StorageTargetConfig =
        serde_json::from_value(Value::Object(object)).map_err(|error| {
            UnifiedError::bad_request(format!("Invalid storage target config: {error}"))
        })?;
    config.validate()?;
    Ok(config)
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn provider_for(
    ctx: &Arc<AppContext>,
    name: &str,
) -> UnifiedResult<Arc<dyn unified_core::storage::StorageProvider>> {
    ctx.storage_registry
        .get(name)
        .ok_or_else(|| UnifiedError::not_found(format!("Storage target \"{name}\" not found")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_config_from_valid_local_request() {
        let request = CreateTargetRequest {
            name: "main".to_string(),
            kind: StorageKind::Local,
            config: Default::default(),
        };
        let config = target_config_from_request(&request).expect("local target builds");
        assert_eq!(config.name, "main");
        assert_eq!(config.kind, StorageKind::Local);
    }

    #[test]
    fn target_config_rejects_bad_names() {
        let bad = CreateTargetRequest {
            name: "bad name".to_string(),
            kind: StorageKind::Local,
            config: Default::default(),
        };
        assert!(target_config_from_request(&bad).is_err());
        let empty = CreateTargetRequest {
            name: "".to_string(),
            kind: StorageKind::Local,
            config: Default::default(),
        };
        assert!(target_config_from_request(&empty).is_err());
    }

    #[test]
    fn target_config_rejects_incomplete_s3() {
        let request = CreateTargetRequest {
            name: "s3main".to_string(),
            kind: StorageKind::S3,
            config: Default::default(),
        };
        assert!(target_config_from_request(&request).is_err());
    }

    #[test]
    fn target_config_ignores_name_and_kind_inside_config() {
        // A config that tries to rename the target to "other" and
        // switch its kind to S3 must not win over the validated
        // top-level fields.
        let mut config = serde_json::Map::new();
        config.insert("name".to_string(), serde_json::json!("other"));
        config.insert("kind".to_string(), serde_json::json!("s3"));
        let request = CreateTargetRequest {
            name: "main".to_string(),
            kind: StorageKind::Local,
            config,
        };
        let built = target_config_from_request(&request).expect("local target builds");
        assert_eq!(built.name, "main");
        assert_eq!(built.kind, StorageKind::Local);
    }

    #[test]
    fn target_config_ignores_name_and_kind_inside_config_for_s3() {
        // The reverse direction: a top-level S3 target keeps its
        // name and kind even when config tries to downgrade it to
        // a local target named "other".
        let mut config = serde_json::Map::new();
        config.insert("name".to_string(), serde_json::json!("other"));
        config.insert("kind".to_string(), serde_json::json!("local"));
        config.insert(
            "endpoint".to_string(),
            serde_json::json!("https://s3.example.amazonaws.com"),
        );
        config.insert("region".to_string(), serde_json::json!("us-east-1"));
        config.insert("bucket".to_string(), serde_json::json!("bucket"));
        config.insert("access_key".to_string(), serde_json::json!("akid"));
        config.insert("secret_key".to_string(), serde_json::json!("secret"));
        let request = CreateTargetRequest {
            name: "s3main".to_string(),
            kind: StorageKind::S3,
            config,
        };
        let built = target_config_from_request(&request).expect("s3 target builds");
        assert_eq!(built.name, "s3main");
        assert_eq!(built.kind, StorageKind::S3);
        assert_eq!(built.bucket, "bucket");
    }

    #[test]
    fn target_config_still_reads_other_fields_from_config() {
        // Only name/kind are forced from the top level; the rest
        // of the config (base_dir, quotas) is still honoured.
        let mut config = serde_json::Map::new();
        config.insert(
            "base_dir".to_string(),
            serde_json::json!("/data/storage/main"),
        );
        config.insert("quota_bytes".to_string(), serde_json::json!(1024));
        let request = CreateTargetRequest {
            name: "main".to_string(),
            kind: StorageKind::Local,
            config,
        };
        let built = target_config_from_request(&request).expect("local target builds");
        assert_eq!(built.base_dir, "/data/storage/main");
        assert_eq!(built.quota_bytes, Some(1024));
    }

    #[test]
    fn paginate_slices_items() {
        let params = PaginationParams {
            page: Some(1),
            per_page: Some(2),
            sort: None,
            order: None,
            search: None,
            filters: Default::default(),
        };
        let result = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(result.items, vec![1, 2]);
        assert_eq!(result.pagination.total, 5);
        let params = PaginationParams {
            page: Some(3),
            per_page: Some(2),
            sort: None,
            order: None,
            search: None,
            filters: Default::default(),
        };
        assert_eq!(paginate(vec![1, 2, 3, 4, 5], &params).items, vec![5]);
        let params = PaginationParams {
            page: Some(9),
            per_page: Some(2),
            sort: None,
            order: None,
            search: None,
            filters: Default::default(),
        };
        assert!(paginate(vec![1, 2, 3, 4, 5], &params).items.is_empty());
    }
}
