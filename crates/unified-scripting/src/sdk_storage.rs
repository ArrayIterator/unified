//! Scoped file access for script modules (`Unified.storage`).
//!
//! A module may only touch files below its own directory,
//! `storage_root/<module-id>/`. Paths are validated before any
//! filesystem call: absolute paths and `..` escapes are rejected,
//! and the resolved path is checked to stay inside the module
//! directory. There is no access to the rest of the filesystem,
//! and no `fs`/`process` global exists in the sandbox at all.

use std::path::{Component, Path, PathBuf};
use unified_core::error::{UnifiedError, UnifiedResult};

/// Maximum bytes of one file a module may read or write (10 MiB).
/// Larger files are rejected so a module cannot exhaust disk or RAM
/// through this API.
pub const MAX_STORAGE_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Resolve `user_path` inside `module_dir`, or fail when the path
/// escapes the directory. Symlinks inside the directory are NOT
/// followed for the containment check: the canonicalized parent is
/// verified to stay inside the canonicalized module directory.
pub fn resolve_module_path(module_dir: &Path, user_path: &str) -> UnifiedResult<PathBuf> {
    if user_path.is_empty() {
        return Err(UnifiedError::bad_request("Storage path must not be empty"));
    }
    let requested = Path::new(user_path);
    if requested.is_absolute() {
        return Err(UnifiedError::bad_request(
            "Storage path must be relative to the module directory",
        ));
    }
    let mut joined = module_dir.to_path_buf();
    for component in requested.components() {
        match component {
            Component::Normal(part) => joined.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(UnifiedError::bad_request(
                    "Storage path must not escape the module directory",
                ));
            }
        }
    }
    // Containment after canonicalization: a symlink inside the
    // directory pointing outside is still an escape. The parent is
    // checked for paths that do not exist yet (write targets); an
    // existing path is canonicalized itself, so a symlink *file*
    // pointing outside is caught too (a rename over a symlink
    // replaces the link, so writes are safe either way).
    let base = module_dir
        .canonicalize()
        .unwrap_or_else(|_| module_dir.to_path_buf());
    if joined.exists() {
        let canonical = joined.canonicalize().unwrap_or_else(|_| joined.clone());
        if !canonical.starts_with(&base) {
            return Err(UnifiedError::bad_request(
                "Storage path must not escape the module directory",
            ));
        }
        return Ok(joined);
    }
    let parent = joined.parent().unwrap_or(&joined).to_path_buf();
    let canonical_parent = parent.canonicalize().unwrap_or(parent);
    if !canonical_parent.starts_with(&base) {
        return Err(UnifiedError::bad_request(
            "Storage path must not escape the module directory",
        ));
    }
    Ok(joined)
}

/// Read a text file from the module directory.
pub fn storage_read(module_dir: &Path, user_path: &str) -> UnifiedResult<String> {
    let path = resolve_module_path(module_dir, user_path)?;
    let metadata = std::fs::metadata(&path)
        .map_err(|_| UnifiedError::not_found(format!("No such file: {user_path}")))?;
    if !metadata.is_file() {
        return Err(UnifiedError::bad_request(format!(
            "{user_path} is not a file"
        )));
    }
    if metadata.len() > MAX_STORAGE_FILE_BYTES {
        return Err(UnifiedError::bad_request(format!(
            "File {user_path} exceeds the {MAX_STORAGE_FILE_BYTES}-byte limit"
        )));
    }
    std::fs::read_to_string(&path).map_err(|error| {
        UnifiedError::internal(format!("Cannot read {user_path}: {error}"))
    })
}

/// Write a text file into the module directory, creating parent
/// directories as needed. The write is atomic: the content goes to
/// a temporary sibling and is renamed over the target, so a reader
/// never sees a half-written file.
pub fn storage_write(module_dir: &Path, user_path: &str, data: &str) -> UnifiedResult<()> {
    if data.len() as u64 > MAX_STORAGE_FILE_BYTES {
        return Err(UnifiedError::bad_request(format!(
            "Data exceeds the {MAX_STORAGE_FILE_BYTES}-byte limit"
        )));
    }
    let path = resolve_module_path(module_dir, user_path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            UnifiedError::internal(format!("Cannot create directory: {error}"))
        })?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, data)
        .map_err(|error| UnifiedError::internal(format!("Cannot write {user_path}: {error}")))?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        UnifiedError::internal(format!("Cannot write {user_path}: {error}"))
    })?;
    Ok(())
}

/// List the names inside a directory of the module directory
/// (files and subdirectories, sorted). The root is listed when
/// `user_path` is empty or `"."`.
pub fn storage_list(module_dir: &Path, user_path: &str) -> UnifiedResult<Vec<String>> {
    let dir = if user_path.is_empty() || user_path == "." {
        module_dir.to_path_buf()
    } else {
        resolve_module_path(module_dir, user_path)?
    };
    let entries = std::fs::read_dir(&dir)
        .map_err(|_| UnifiedError::not_found(format!("No such directory: {user_path}")))?;
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|error| UnifiedError::internal(format!("Cannot list directory: {error}")))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

/// Delete one file inside the module directory. Deleting
/// directories is not supported.
pub fn storage_delete(module_dir: &Path, user_path: &str) -> UnifiedResult<bool> {
    let path = resolve_module_path(module_dir, user_path)?;
    if !path.is_file() {
        return Ok(false);
    }
    std::fs::remove_file(&path)
        .map_err(|error| UnifiedError::internal(format!("Cannot delete {user_path}: {error}")))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_module_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "unified-scripting-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn escapes_are_rejected() {
        let dir = temp_module_dir();
        assert!(resolve_module_path(&dir, "../outside.txt").is_err());
        assert!(resolve_module_path(&dir, "/etc/passwd").is_err());
        assert!(resolve_module_path(&dir, "a/../../b.txt").is_err());
        assert!(resolve_module_path(&dir, "").is_err());
        assert!(resolve_module_path(&dir, "ok/nested.txt").is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_read_list_delete_round_trip() {
        let dir = temp_module_dir();
        storage_write(&dir, "notes/hello.txt", "hello").expect("write");
        assert_eq!(storage_read(&dir, "notes/hello.txt").expect("read"), "hello");
        assert_eq!(storage_list(&dir, "notes").expect("list"), vec!["hello.txt"]);
        assert!(storage_delete(&dir, "notes/hello.txt").expect("delete"));
        assert!(!storage_delete(&dir, "notes/hello.txt").expect("delete again"));
        assert!(storage_read(&dir, "notes/hello.txt").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn symlink_escape_is_rejected() {
        let dir = temp_module_dir();
        // The outside file lives in a *different* directory than the
        // module directory, or the containment check would pass.
        let outside_dir = std::env::temp_dir().join(format!(
            "unified-scripting-outside-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&outside_dir);
        fs::create_dir_all(&outside_dir).expect("create outside dir");
        let outside = outside_dir.join("outside.txt");
        fs::write(&outside, "secret").expect("write outside");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dir.join("link.txt")).expect("symlink");
        #[cfg(unix)]
        assert!(resolve_module_path(&dir, "link.txt").is_err());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside_dir);
    }
}
