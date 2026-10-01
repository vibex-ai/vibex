//! The sidebar arrangement, as a backend capability.
//!
//! The Desktop owns how the session tree is arranged: which folders exist,
//! where projects and sessions sit inside them, which sessions are pinned and
//! in what manual order. A compact client that invented its own arrangement
//! would draw a list the reader never arranged and could not explain.
//!
//! The capability is optional by design. A backend that has no arrangement to
//! publish — a client with no runtime yet, a runtime with no shell attached —
//! reports [`BackendError::unsupported`] instead of inventing one, and its
//! caller keeps whatever fallback ordering it has.

use vibex_core::{RemoteSidebarOrganizationMutation, RemoteSidebarOrganizationSnapshot};

use crate::{BackendBound, BackendError, BackendFuture};

pub trait SidebarBackend: BackendBound {
    /// The arrangement the authority draws its sidebar from.
    fn sidebar_organization(&self) -> BackendFuture<'_, RemoteSidebarOrganizationSnapshot> {
        Box::pin(async {
            Err(BackendError::unsupported(
                "sidebar_organization_unavailable",
                "this backend has no sidebar arrangement to publish",
            ))
        })
    }

    /// Applies one arrangement change on the authority and returns the tree it
    /// now holds.
    ///
    /// `expected_revision` is the snapshot the caller rendered. An authority
    /// that has moved on since refuses the change rather than applying it to a
    /// tree the reader is not looking at.
    fn mutate_sidebar_organization(
        &self,
        _mutation: RemoteSidebarOrganizationMutation,
        _expected_revision: Option<u64>,
    ) -> BackendFuture<'_, RemoteSidebarOrganizationSnapshot> {
        Box::pin(async {
            Err(BackendError::unsupported(
                "sidebar_organization_unavailable",
                "this backend cannot rearrange the sidebar",
            ))
        })
    }
}
