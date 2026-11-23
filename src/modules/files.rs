// File Manager module.
//
// Folders and files with metadata, multipart upload under
// `data/files/<folder>/<record-id>`, streaming download, move and delete.
// Path traversal is rejected, uploads are capped at 100 MB, and the MIME
// type is detected from content with the `infer` crate. The `file.shared`
// hook (from Chat) registers a shared file. Interconnection happens only
// through the hook bus.

mod handlers;
mod helpers;
mod hooks;
pub mod module;
pub mod types;

pub use self::module::{FilesModule, register};
pub use self::types::{CreateFolder, FileRecord, FilesState, MoveRequest};
