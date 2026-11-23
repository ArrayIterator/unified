//! vault module - Checksum, storage reference and client-list validation.

use unified_core::error::{UnifiedError, UnifiedResult};

use super::constants::MAX_CLIENTS;
use super::constants::MAX_STORAGE_KEY_LEN;
use super::constants::MAX_STORAGE_TARGET_LEN;

/// A SHA-256 checksum is 64 lowercase hex characters.
pub(crate) fn valid_checksum(checksum: &str) -> bool {
    checksum.len() == 64
        && checksum
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

pub(crate) fn validate_storage_ref(target: &str, key: &str) -> UnifiedResult<()> {
    if target.trim().is_empty() || target.len() > MAX_STORAGE_TARGET_LEN {
        return Err(UnifiedError::bad_request(
            "storage_target must not be empty and must be at most 128 characters",
        ));
    }
    if key.trim().is_empty() || key.len() > MAX_STORAGE_KEY_LEN {
        return Err(UnifiedError::bad_request(
            "storage_key must not be empty and must be at most 1024 characters",
        ));
    }
    Ok(())
}

pub(crate) fn validate_client_ids(ids: &[String]) -> UnifiedResult<Vec<String>> {
    if ids.len() > MAX_CLIENTS {
        return Err(UnifiedError::bad_request(format!(
            "shared_with_client_ids must contain at most {MAX_CLIENTS} entries"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for raw in ids {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(UnifiedError::bad_request(
                "shared_with_client_ids must not contain empty entries",
            ));
        }
        if !out.contains(&trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::valid_checksum;
    use super::validate_client_ids;
    use super::validate_storage_ref;

    #[test]
    fn checksum_requires_64_lowercase_hex() {
        assert!(valid_checksum(&"a".repeat(64)));
        assert!(valid_checksum(&"0123456789abcdef".repeat(4)));
        assert!(!valid_checksum(&"A".repeat(64)));
        assert!(!valid_checksum(&"g".repeat(64)));
        assert!(!valid_checksum("abc"));
        assert!(!valid_checksum(""));
    }

    #[test]
    fn storage_ref_accepts_valid_target_and_key() {
        assert!(validate_storage_ref("files", "folder/doc-1").is_ok());
    }

    #[test]
    fn storage_ref_rejects_empty_or_oversized() {
        assert!(validate_storage_ref("", "key").is_err());
        assert!(validate_storage_ref("target", "").is_err());
        assert!(validate_storage_ref(&"t".repeat(129), "key").is_err());
        assert!(validate_storage_ref("target", &"k".repeat(1025)).is_err());
    }

    #[test]
    fn client_ids_trim_and_dedupe() {
        let out =
            validate_client_ids(&["a".to_string(), " b ".to_string(), "a".to_string()]).unwrap();
        assert_eq!(out, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn client_ids_reject_empty_entries() {
        assert!(validate_client_ids(&["".to_string()]).is_err());
        assert!(validate_client_ids(&["  ".to_string()]).is_err());
    }

    #[test]
    fn client_ids_reject_too_many() {
        let ids: Vec<String> = (0..501).map(|n| format!("client-{n}")).collect();
        assert!(validate_client_ids(&ids).is_err());
    }
}
