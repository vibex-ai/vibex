use vibex_core::{
    FileMutationRequest, FileReadRequest, FileReadResponse, FileSearchRequest, FileSearchResult,
    FileTreeEntry, FileTreeRequest, FileWriteRequest, WorkspaceId,
};

use crate::{BackendBound, BackendFuture, MutationRequest};

pub trait FileBackend: BackendBound {
    /// Resolves an existing file or directory on the client's own machine.
    ///
    /// Native backends validate the path through the workspace file service.
    /// Remote backends return `None`: a server path is not a local path, even
    /// when a directory with the same name happens to exist on the client.
    fn resolve_local_path(
        &self,
        _workspace_id: WorkspaceId,
        _path: String,
    ) -> BackendFuture<'_, Option<std::path::PathBuf>> {
        Box::pin(async { Ok(None) })
    }

    fn file_tree(&self, request: FileTreeRequest) -> BackendFuture<'_, Vec<FileTreeEntry>>;

    fn search_files(&self, request: FileSearchRequest) -> BackendFuture<'_, Vec<FileSearchResult>>;

    fn read_file(&self, request: FileReadRequest) -> BackendFuture<'_, FileReadResponse>;

    /// Reads bounded raw bytes for a workspace-relative path.
    ///
    /// `read_file` returns a UTF-8 preview, which is enough for the editor but
    /// not for binary surfaces such as Markdown image assets. The desktop
    /// renders those locally from authoritative bytes, so the contract carries
    /// a byte-exact read.
    fn read_file_bytes(
        &self,
        workspace_id: WorkspaceId,
        path: String,
        max_bytes: usize,
    ) -> BackendFuture<'_, Vec<u8>>;

    fn write_file(
        &self,
        request: MutationRequest<FileWriteRequest>,
    ) -> BackendFuture<'_, FileReadResponse>;

    fn create_directory(
        &self,
        request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry>;

    fn copy_path(
        &self,
        request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry>;

    fn rename_path(
        &self,
        request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry>;

    fn delete_path(&self, request: MutationRequest<FileMutationRequest>) -> BackendFuture<'_, ()>;
}
