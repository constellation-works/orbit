mod classification;
mod conversions;
mod kinds;
mod orbit_error;

pub use classification::LOCK_BUSY_ERROR_CODE;
pub use kinds::{
    ArtifactOrigin, ArtifactOriginMode, ClaimRefusalKind, ConflictedFile, DependencyNotDelivered,
    FrictionNotLocal, HostRegistryCode, NotFoundKind, RecoverableVcsConflict, SqliteContention,
    StorageLayer, WorkspaceClaimHeld,
};
pub use orbit_error::OrbitError;
