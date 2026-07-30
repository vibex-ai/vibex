use vibex_core::{
    FileMutationRequest, FileReadRequest, FileReadResponse, FileSearchRequest, FileSearchResult,
    FileTreeEntry, FileTreeRequest, FileWriteRequest,
};

use crate::{BackendBound, BackendFuture, MutationRequest};

pub trait FileBackend: BackendBound {
    fn file_tree(&self, request: FileTreeRequest) -> BackendFuture<'_, Vec<FileTreeEntry>>;

    fn search_files(&self, request: FileSearchRequest) -> BackendFuture<'_, Vec<FileSearchResult>>;

    fn read_file(&self, request: FileReadRequest) -> BackendFuture<'_, FileReadResponse>;

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
