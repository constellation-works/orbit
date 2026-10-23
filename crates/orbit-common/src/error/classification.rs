use std::path::Path;

use super::{
    ArtifactOrigin, ClaimRefusalKind, FrictionNotLocal, HostRegistryCode, NotFoundKind, OrbitError,
    PrivateVcsFailure, SqliteContention, StorageLayer, WorkspaceClaimHeld,
};

impl OrbitError {
    pub fn host_registry(code: HostRegistryCode, message: impl Into<String>) -> Self {
        Self::HostRegistry {
            code,
            message: message.into(),
        }
    }

    /// The host-registry code of this error, if it is one.
    pub fn host_registry_code(&self) -> Option<HostRegistryCode> {
        match self {
            Self::HostRegistry { code, .. } => Some(*code),
            _ => None,
        }
    }

    pub fn not_found(kind: NotFoundKind, id: impl Into<String>) -> Self {
        Self::NotFound {
            kind,
            id: id.into(),
        }
    }

    pub fn invalid_input_with_suggestions(
        message: impl Into<String>,
        did_you_mean: Vec<String>,
    ) -> Self {
        if did_you_mean.is_empty() {
            Self::InvalidInput(message.into())
        } else {
            Self::InvalidInputDiagnostic {
                message: message.into(),
                did_you_mean,
            }
        }
    }

    pub fn remote_artifact_unavailable(
        kind: NotFoundKind,
        id: impl Into<String>,
        artifact_origin: ArtifactOrigin,
    ) -> Self {
        Self::RemoteArtifactUnavailable {
            kind,
            id: id.into(),
            artifact_origin,
        }
    }

    pub fn artifact_not_local(
        kind: NotFoundKind,
        id: impl Into<String>,
        artifact_origin: ArtifactOrigin,
    ) -> Self {
        Self::ArtifactNotLocal {
            kind,
            id: id.into(),
            artifact_origin,
        }
    }

    pub fn friction_not_local(
        friction_id: impl Into<String>,
        task_id: impl Into<String>,
        workspace_id: impl Into<String>,
        found_in: Vec<String>,
    ) -> Self {
        Self::FrictionNotLocal(Box::new(FrictionNotLocal {
            friction_id: friction_id.into(),
            task_id: task_id.into(),
            workspace_id: workspace_id.into(),
            found_in,
        }))
    }

    pub fn friction_not_local_details(&self) -> Option<&FrictionNotLocal> {
        match self {
            Self::FrictionNotLocal(details) => Some(details),
            _ => None,
        }
    }

    pub fn dropped_system_tag(&self) -> Option<&str> {
        match self {
            Self::SystemIdentityTagDropped { tag } => Some(tag),
            _ => None,
        }
    }

    pub fn artifact_origin(&self) -> Option<&ArtifactOrigin> {
        match self {
            Self::RemoteArtifactUnavailable {
                artifact_origin, ..
            }
            | Self::ArtifactNotLocal {
                artifact_origin, ..
            } => Some(artifact_origin),
            _ => None,
        }
    }

    pub fn did_you_mean(&self) -> Option<&[String]> {
        match self {
            Self::InvalidInputDiagnostic { did_you_mean, .. } if !did_you_mean.is_empty() => {
                Some(did_you_mean)
            }
            _ => None,
        }
    }

    /// The contended `(task_id, run_id)` of a ship duplicate-dispatch refusal,
    /// so projections (HTTP 409 body, MCP structured error) can name both
    /// without re-parsing the message.
    pub fn ship_run_in_flight(&self) -> Option<(&str, &str)> {
        match self {
            Self::ShipRunInFlight { task_id, run_id } => Some((task_id, run_id)),
            _ => None,
        }
    }

    /// The `(source_run_id, run_id)` of a duplicate-resume refusal: the run the
    /// caller asked to resume and the live run already carrying its lineage.
    pub fn resume_run_in_flight(&self) -> Option<(&str, &str)> {
        match self {
            Self::ResumeRunInFlight {
                source_run_id,
                run_id,
            } => Some((source_run_id, run_id)),
            _ => None,
        }
    }

    /// The refused operation, incumbent holder, claim id, and expiry of a
    /// workspace-claim refusal, so projections (HTTP 409 body, MCP structured
    /// error) can name them without re-parsing the message [ORB-10709].
    pub fn workspace_claim_held(&self) -> Option<&WorkspaceClaimHeld> {
        match self {
            Self::WorkspaceClaimHeld(claim) => Some(claim),
            _ => None,
        }
    }

    pub fn task_bundle_corruption(&self) -> Option<(&str, &str, &str)> {
        match self {
            Self::TaskBundleCorrupt {
                task_id,
                path,
                reason,
            } => Some((task_id, path, reason)),
            _ => None,
        }
    }

    pub fn file_lock_timeout(&self) -> Option<&crate::fs::io::FileLockTimeout> {
        match self {
            Self::FileLockTimeout(timeout) => Some(timeout),
            _ => None,
        }
    }

    pub fn sqlite_contention(&self) -> Option<&SqliteContention> {
        match self {
            Self::SqliteContention(contention) => Some(contention),
            _ => None,
        }
    }

    /// The classified private automation VCS failure, if this is one.
    pub fn private_vcs_failure(&self) -> Option<&PrivateVcsFailure> {
        match self {
            Self::PrivateVcs(failure) => Some(failure),
            _ => None,
        }
    }

    /// Translate a filesystem write failure.
    ///
    /// EROFS/EACCES name `path` and hint that this is likely a sandbox or
    /// environment mount, not a store defect — the outer-sandbox counterpart
    /// to `linux_bwrap_write_grant_diagnostic`. Other I/O stays a bare
    /// [`Self::Io`] so read and capacity failures keep their existing text.
    pub fn from_write_io(path: &Path, err: std::io::Error) -> Self {
        match crate::fs::io::write_access_error_message(path, &err) {
            Some(message) => Self::StorageAccessDenied {
                layer: StorageLayer::Io,
                message,
            },
            None => Self::Io(err.to_string()),
        }
    }

    /// An I/O failure described by `message`, keeping whether `err` was a
    /// read-only or access denial. Use it wherever an `io::Error` is wrapped
    /// with context instead of converted with `?`.
    pub fn io_with_context(err: &std::io::Error, message: impl Into<String>) -> Self {
        Self::storage_io(StorageLayer::Io, err, message)
    }

    /// A `layer` failure described by `message` and caused by `err`, keeping
    /// whether `err` was a read-only or access denial.
    pub fn storage_io(
        layer: StorageLayer,
        err: &std::io::Error,
        message: impl Into<String>,
    ) -> Self {
        Self::storage(
            layer,
            crate::fs::io::is_readonly_or_access_error(err),
            message,
        )
    }

    /// A `layer` failure described by `message`, carried as
    /// [`Self::StorageAccessDenied`] when the native error that caused it was
    /// a read-only or access denial.
    pub fn storage(layer: StorageLayer, access_denied: bool, message: impl Into<String>) -> Self {
        let message = message.into();
        if access_denied {
            return Self::StorageAccessDenied { layer, message };
        }
        match layer {
            StorageLayer::Io => Self::Io(message),
            StorageLayer::Store => Self::Store(message),
            StorageLayer::Migration => Self::Migration(message),
        }
    }

    /// The storage family this error reports as — [`Self::Io`],
    /// [`Self::Store`] or [`Self::Migration`], whether or not it carries a
    /// read-only or access class — or `None` for any other error.
    pub fn storage_layer(&self) -> Option<StorageLayer> {
        match self {
            Self::Io(_) => Some(StorageLayer::Io),
            Self::Store(_) => Some(StorageLayer::Store),
            Self::Migration(_) => Some(StorageLayer::Migration),
            Self::StorageAccessDenied { layer, .. } => Some(*layer),
            _ => None,
        }
    }

    /// Whether an operation failed because its persistence target is mounted
    /// read-only or denies writes.
    ///
    /// Answered from the variant alone: the class is decided where the native
    /// I/O or SQLite error is translated, so an error that merely quotes such
    /// text (a child's stderr, a remote message) never counts.
    pub fn is_readonly_or_access_failure(&self) -> bool {
        matches!(self, Self::StorageAccessDenied { .. })
    }

    /// A claim refusal reported with the store's text for `kind`.
    pub fn claim_refused(kind: ClaimRefusalKind) -> Self {
        Self::ClaimRefused {
            kind,
            message: kind.message().to_string(),
        }
    }

    /// The kind of an owner claim or handoff refusal, if this is one.
    pub fn claim_refusal(&self) -> Option<ClaimRefusalKind> {
        match self {
            Self::ClaimRefused { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// Whether this failed only because a lock stayed held past its
    /// acquisition deadline: an advisory file lock or a SQLite database here,
    /// or either on a remote owner, which reports it as
    /// [`LOCK_BUSY_ERROR_CODE`]. Nothing was decided under the lock, so the
    /// same call can succeed once its holder lets go.
    pub fn is_lock_busy(&self) -> bool {
        match self {
            Self::FileLockTimeout(_) | Self::SqliteContention(_) => true,
            Self::RemoteTool { code, .. } => code == LOCK_BUSY_ERROR_CODE,
            _ => false,
        }
    }

    /// Whether a supervised operation exceeded its deadline: a process
    /// timeout, or an execution failure raised for one.
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            Self::ProcessTimeout { .. } | Self::ExecutionTimeout { .. }
        )
    }
}

/// The stable tool error code for a call that lost only to a lock-wait
/// deadline ([`OrbitError::is_lock_busy`]). Retryable: unlike
/// `internal_error`, it says the call can succeed unchanged later.
pub const LOCK_BUSY_ERROR_CODE: &str = "lock_busy";

/// Whether an error's text says it lost only to a lock-wait deadline, for an
/// error that reached this process as text rather than as a typed
/// [`OrbitError`]: SQLite's own `database is locked` / `database is busy` /
/// `database table is locked`, which a store error quotes; the text of a
/// `SQLITE_BUSY` that carried no message (`The database file is locked`);
/// the `code=DatabaseBusy` / `code=DatabaseLocked` markers a store error's
/// detail carries; or the display of [`OrbitError::SqliteContention`] or
/// [`OrbitError::FileLockTimeout`].
///
/// The one place such text is recognised, so the patterns change together
/// with the displays they match.
pub fn is_lock_busy_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("database is locked")
        || message.contains("database is busy")
        || message.contains("database table is locked")
        || message.contains("the database file is locked")
        || message.contains("code=databasebusy")
        || message.contains("code=databaselocked")
        // `OrbitError::SqliteContention`.
        || message.contains("remained locked during")
        // `FileLockTimeout`: "timed out after {ms}ms acquiring {label} lock '{path}'".
        || (message.starts_with("timed out after ")
            && message.contains("ms acquiring ")
            && message.contains(" lock '"))
}
