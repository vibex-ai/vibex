//! Signed GitHub Releases update discovery, download, and desktop installation.

mod error;
mod install;
mod manifest;
mod notes;
mod service;
mod source;

pub use error::{AppUpdateError, AppUpdateResult};
pub use install::{Installation, InstallationKind};
pub use manifest::{
    CURRENT_UPDATER_VERSION, InstallMode, UpdateArtifact, UpdateChannel, UpdateManifest,
    VerifiedManifest, verify_manifest,
};
pub use notes::{MAX_RELEASE_NOTES_BYTES, select_notes_section};
pub use service::{
    AppUpdateConfig, AppUpdateService, CheckReason, UpdateFailure, UpdateRelease, UpdateSnapshot,
    UpdateState,
};
pub use source::{GitHubReleaseSource, SignedManifest, UpdateDownloadProgress, UpdateSource};
