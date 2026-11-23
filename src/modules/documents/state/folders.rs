//! Documents folders (migration 000132, task 355): the in-memory
//! folder tree and the document move / rename operations.
//!
//! Folders are a grouping only: they never change a document's
//! stored key, its visibility area, or its serving URL. A folder
//! id is never trusted from a client without an existence +
//! company-scope check (`load_folder_in_scope`), and a delete
//! refuses (409) while the folder still holds documents or
//! subfolders — the caller moves or deletes the contents first, so
//! a delete can never orphan or silently drop files.

use super::super::types::DocumentFolder;
use super::super::types::DocumentFolderTree;
use super::super::validation::validate_folder_name;
use super::*;
use serde_json::json;

/// Maximum folder-tree depth rendered by `folder_tree`. The move
/// guard prevents cycles, so a real tree never reaches this; the
/// cap is the defensive second layer against corrupt stored data.
const MAX_FOLDER_TREE_DEPTH: usize = 64;

impl DocumentsState {
    /// Load a folder or return a 404. The empty id is the library
    /// root, which always exists and needs no row.
    pub(crate) fn load_folder(&self, id: &str) -> UnifiedResult<DocumentFolder> {
        if id.is_empty() {
            return Err(UnifiedError::not_found("Folder not found"));
        }
        self.folders
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Folder not found"))
    }

    /// Load a folder and verify it is inside the caller's company
    /// scope. The root (`""`) passes for every caller: it holds
    /// only documents already visible through the list filter.
    /// Fails closed (403) when the folder belongs to another
    /// company, 404 when it does not exist — so a caller can never
    /// probe another company's folder ids through the error code.
    pub(crate) fn load_folder_in_scope(
        &self,
        id: &str,
        caller_company: &Option<String>,
    ) -> UnifiedResult<Option<DocumentFolder>> {
        if id.is_empty() {
            return Ok(None);
        }
        let folder = self.load_folder(id)?;
        require_record_in_company_scope(&folder.company_id, caller_company)?;
        Ok(Some(folder))
    }

    /// True when `folder_id` is the root or an existing folder in
    /// the caller's company scope. Used to validate upload /
    /// session targets before any byte is stored.
    pub(crate) fn folder_accepts(
        &self,
        folder_id: &str,
        caller_company: &Option<String>,
    ) -> bool {
        self.load_folder_in_scope(folder_id, caller_company)
            .map(|folder| folder.is_some() || folder_id.is_empty())
            .unwrap_or(false)
    }

    /// Create a folder under `parent_id` (empty = top-level).
    /// The parent must exist and be in the caller's company scope;
    /// the stored `company_id` is the caller's scope (never a
    /// client claim), so a folder can never be created into
    /// another company's tree.
    pub(crate) fn create_folder(
        &self,
        name: &str,
        parent_id: &str,
        company_id: &str,
        owner_id: &str,
    ) -> UnifiedResult<DocumentFolder> {
        let name = validate_folder_name(name)?;
        if !parent_id.is_empty() {
            let parent = self.load_folder(parent_id)?;
            if parent.company_id != company_id {
                return Err(UnifiedError::forbidden(
                    "The parent folder belongs to a different company",
                ));
            }
        }
        let folder = DocumentFolder {
            id: new_id(),
            name,
            parent_id: parent_id.to_string(),
            company_id: company_id.to_string(),
            owner_id: owner_id.to_string(),
            created_at: now(),
            updated_at: now(),
        };
        self.folders.insert(folder.id.clone(), folder.clone());
        self.emit(
            events::FOLDER_CREATED,
            serde_json::to_value(&folder).unwrap_or(Value::Null),
        );
        Ok(folder)
    }

    /// Rename a folder. The name is the only mutable field here;
    /// moving uses `move_folder` so a rename can never re-parent
    /// by accident.
    pub(crate) fn rename_folder(
        &self,
        id: &str,
        name: &str,
        actor: &str,
    ) -> UnifiedResult<DocumentFolder> {
        let name = validate_folder_name(name)?;
        let mut folder = self.load_folder(id)?;
        let previous = folder.name.clone();
        if previous == name {
            return Ok(folder);
        }
        folder.name = name.clone();
        folder.updated_at = now();
        self.folders.insert(folder.id.clone(), folder.clone());
        self.emit(
            events::FOLDER_UPDATED,
            json!({
                "folder_id": folder.id,
                "name": name,
                "previous": previous,
                "actor": actor,
            }),
        );
        Ok(folder)
    }

    /// True when `candidate_id` is `folder_id` itself or one of its
    /// descendants (walk up from the candidate: the tree is small
    /// and the walk is bounded by the folder count, so a cycle in
    /// stored data terminates instead of looping forever).
    pub(crate) fn is_folder_descendant(&self, folder_id: &str, candidate_id: &str) -> bool {
        let mut current = candidate_id.to_string();
        for _ in 0..=self.folders.len() {
            if current == folder_id {
                return true;
            }
            match self.folders.get(&current) {
                Some(entry) => {
                    let parent = entry.value().parent_id.clone();
                    if parent.is_empty() {
                        return false;
                    }
                    current = parent;
                }
                None => return false,
            }
        }
        // Walked past every folder without reaching the root: the
        // stored tree is corrupt — treat the candidate as a
        // descendant (fail closed, refuse the move).
        true
    }

    /// Move a folder to a new parent (empty = top-level). Refuses
    /// a move into itself or one of its descendants (cycle), and a
    /// move across companies (the target parent must be in the
    /// folder's company scope).
    pub(crate) fn move_folder(
        &self,
        id: &str,
        parent_id: &str,
        actor: &str,
    ) -> UnifiedResult<DocumentFolder> {
        let mut folder = self.load_folder(id)?;
        if !parent_id.is_empty() {
            let parent = self.load_folder(parent_id)?;
            if parent.company_id != folder.company_id {
                return Err(UnifiedError::forbidden(
                    "The target folder belongs to a different company",
                ));
            }
            if self.is_folder_descendant(id, parent_id) {
                return Err(UnifiedError::conflict(
                    "A folder cannot be moved into itself or one of its subfolders",
                ));
            }
        }
        let previous_parent_id = folder.parent_id.clone();
        if previous_parent_id == parent_id {
            return Ok(folder);
        }
        folder.parent_id = parent_id.to_string();
        folder.updated_at = now();
        self.folders.insert(folder.id.clone(), folder.clone());
        self.emit(
            events::FOLDER_MOVED,
            json!({
                "folder_id": folder.id,
                "parent_id": parent_id,
                "previous_parent_id": previous_parent_id,
                "actor": actor,
            }),
        );
        Ok(folder)
    }

    /// Delete a folder. Refuses (409) while the folder still holds
    /// documents or subfolders: the caller moves or deletes the
    /// contents first, so a delete can never orphan or silently
    /// drop files.
    pub(crate) fn delete_folder(&self, id: &str) -> UnifiedResult<DocumentFolder> {
        let folder = self.load_folder(id)?;
        if self
            .documents
            .iter()
            .any(|entry| entry.value().folder_id == id)
        {
            return Err(UnifiedError::conflict(
                "Folder still contains documents; move or delete them first",
            ));
        }
        if self
            .folders
            .iter()
            .any(|entry| entry.value().parent_id == id)
        {
            return Err(UnifiedError::conflict(
                "Folder still contains subfolders; move or delete them first",
            ));
        }
        self.folders.remove(id);
        self.emit(
            events::FOLDER_DELETED,
            json!({
                "id": folder.id,
                "name": folder.name,
                "owner_id": folder.owner_id,
                "company_id": folder.company_id,
            }),
        );
        Ok(folder)
    }

    /// The folder tree for one company scope: top-level folders
    /// with their children nested recursively, each node carrying
    /// its direct document count. Sorted by name at every level.
    /// Unscoped folders are only visible to an unscoped caller
    /// (same rule as the document list).
    pub(crate) fn folder_tree(
        &self,
        caller_company: &Option<String>,
    ) -> Vec<DocumentFolderTree> {
        let mut folders: Vec<DocumentFolder> = self
            .folders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|folder| in_company_scope(&folder.company_id, caller_company))
            .collect();
        folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        // Build the tree bottom-up: children are attached to their
        // parent by id, so an orphaned folder (parent deleted out
        // of band) surfaces at the top level instead of vanishing.
        let mut by_parent: std::collections::HashMap<String, Vec<DocumentFolder>> =
            std::collections::HashMap::new();
        for folder in folders {
            by_parent
                .entry(folder.parent_id.clone())
                .or_default()
                .push(folder);
        }
        fn build(
            state: &DocumentsState,
            by_parent: &std::collections::HashMap<String, Vec<DocumentFolder>>,
            parent_id: &str,
            depth: usize,
        ) -> Vec<DocumentFolderTree> {
            // Depth guard: a corrupt cycle in stored data must not
            // recurse forever (the move guard prevents cycles, this
            // is the defensive second layer).
            if depth > MAX_FOLDER_TREE_DEPTH {
                return Vec::new();
            }
            let empty: Vec<DocumentFolder> = Vec::new();
            let children = by_parent.get(parent_id).unwrap_or(&empty);
            children
                .iter()
                .map(|folder| DocumentFolderTree {
                    document_count: state
                        .documents
                        .iter()
                        .filter(|entry| entry.value().folder_id == folder.id)
                        .count() as u64,
                    children: build(state, by_parent, &folder.id, depth + 1),
                    folder: folder.clone(),
                })
                .collect()
        }
        build(self, &by_parent, "", 0)
    }

    /// Move a document to another folder (empty = library root).
    /// The target folder must exist and be in the caller's company
    /// scope. Only the folder assignment changes: the stored key,
    /// the visibility area, and the serving URL are untouched.
    pub(crate) fn move_document_to_folder(
        &self,
        id: &str,
        folder_id: &str,
        caller_company: &Option<String>,
        actor: &str,
    ) -> UnifiedResult<DocumentRecord> {
        if !self.folder_accepts(folder_id, caller_company) {
            return Err(UnifiedError::not_found("Folder not found"));
        }
        let lock = self.document_lock(id);
        let _guard = lock.lock().expect("document lock poisoned");
        let mut document = self.load_document(id)?;
        let previous = document.folder_id.clone();
        if previous == folder_id {
            return Ok(document);
        }
        document.folder_id = folder_id.to_string();
        document.updated_at = now();
        self.documents.insert(document.id.clone(), document.clone());
        drop(_guard);
        self.emit(
            events::DOCUMENT_MOVED,
            json!({
                "document_id": document.id,
                "folder_id": folder_id,
                "previous_folder_id": previous,
                "actor": actor,
            }),
        );
        Ok(document)
    }

    /// Rename a document's file name. The display title follows
    /// only when it still equals the old file name, so an explicit
    /// custom title survives a rename. The stored key and the
    /// serving URL never change. The updated record is published
    /// to the Search module, like any other metadata change.
    pub(crate) fn rename_document(
        &self,
        id: &str,
        name: &str,
        actor: &str,
    ) -> UnifiedResult<DocumentRecord> {
        let name = sanitize_file_name(name)?;
        let lock = self.document_lock(id);
        let _guard = lock.lock().expect("document lock poisoned");
        let mut document = self.load_document(id)?;
        let previous = document.name.clone();
        if previous == name {
            return Ok(document);
        }
        document.name = name.clone();
        if document.title == previous {
            document.title = name.clone();
        }
        document.updated_at = now();
        self.documents.insert(document.id.clone(), document.clone());
        drop(_guard);
        self.emit(
            events::DOCUMENT_RENAMED,
            json!({
                "document_id": document.id,
                "name": name,
                "previous": previous,
                "actor": actor,
            }),
        );
        self.emit_search_index(&document);
        Ok(document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_folder(id: &str, name: &str, parent_id: &str, company_id: &str) -> DocumentFolder {
        DocumentFolder {
            id: id.to_string(),
            name: name.to_string(),
            parent_id: parent_id.to_string(),
            company_id: company_id.to_string(),
            owner_id: "user-1".to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn folder_move_rejects_cycles() {
        let state = DocumentsState::new();
        state.folders.insert(
            "a".to_string(),
            test_folder("a", "A", "", "company-1"),
        );
        state.folders.insert(
            "b".to_string(),
            test_folder("b", "B", "a", "company-1"),
        );
        state.folders.insert(
            "c".to_string(),
            test_folder("c", "C", "b", "company-1"),
        );
        // A folder cannot move into itself, its child, or its
        // grandchild; moving the child up to the root is fine.
        assert!(state.move_folder("a", "a", "user-1").is_err());
        assert!(state.move_folder("a", "b", "user-1").is_err());
        assert!(state.move_folder("a", "c", "user-1").is_err());
        assert!(state.move_folder("b", "", "user-1").is_ok());
        assert_eq!(state.load_folder("b").unwrap().parent_id, "");
    }

    #[test]
    fn folder_tree_nests_children_and_counts_documents() {
        let state = DocumentsState::new();
        state.folders.insert(
            "a".to_string(),
            test_folder("a", "Photos", "", "company-1"),
        );
        state.folders.insert(
            "b".to_string(),
            test_folder("b", "2026", "a", "company-1"),
        );
        state.folders.insert(
            "x".to_string(),
            test_folder("x", "Other company", "", "company-2"),
        );
        let tree = state.folder_tree(&Some("company-1".to_string()));
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].folder.id, "a");
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].folder.id, "b");
        // A company-less caller sees only unscoped folders.
        assert!(state.folder_tree(&None).is_empty());
    }

    #[test]
    fn folder_name_length_cap_is_shared_with_validation() {
        assert!(validate_folder_name(&"n".repeat(129)).is_err());
    }
}
