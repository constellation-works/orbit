mod classification;
mod conversions;
mod kinds;
mod orbit_error;

pub use classification::{LOCK_BUSY_ERROR_CODE, is_lock_busy_message};
pub use kinds::{
    ArtifactOrigin, ArtifactOriginMode, CandidateAbsorbed, ClaimRefusalKind, ConflictedFile,
    DecisionRefusalCode, DependencyNotDelivered, ForceOverridableKind, ForceOverridableRefusal,
    ForgePushHoldFailure, FrictionNotLocal, HostRegistryCode, NotFoundKind, NoticeRefusalCode,
    PrivateVcsFailure, PrivateVcsFailureClass, ProviderReadThrottle, RecoverableVcsConflict,
    RemoteToolCode, SourceRepairAuthorityRefusal, SqliteContention, StorageLayer,
    TaskPilotRefusalCode, WorkflowEvidenceUnavailable, WorkspaceClaimHeld,
};
pub use orbit_error::OrbitError;
