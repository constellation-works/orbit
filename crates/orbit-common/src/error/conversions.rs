use super::OrbitError;

impl From<std::io::Error> for OrbitError {
    fn from(err: std::io::Error) -> Self {
        if let Some(timeout) = err
            .get_ref()
            .and_then(|source| source.downcast_ref::<crate::fs::io::FileLockTimeout>())
        {
            return OrbitError::FileLockTimeout(Box::new(timeout.clone()));
        }
        OrbitError::io_with_context(&err, err.to_string())
    }
}

use orbit_types::identity::{IdentityError, ProviderModelError};
use orbit_types::plugin::{ArchiveDigestError, PluginGrantError, PluginPinError};
use orbit_types::policy::PolicyError;
use orbit_types::record::RecordError;
use orbit_types::resource::ResourceError;
use orbit_types::task::TaskError;
use orbit_types::tool::WorkerBindingError;
use orbit_types::workflow::{
    FinalRecoveryError, JobRunStateError, ProviderSandboxError, RetiredBackendError,
    ReviewAdmissionError, ReviewHistoryError, ReviewReportError, WorkflowError,
};

// Translators for `orbit-types` errors. The orphan rule puts each `From` impl
// in this crate, beside `OrbitError`; `scripts/check-error-translation.sh`
// registers them, and callers convert with `?` or `OrbitError::from`.

impl From<IdentityError> for OrbitError {
    fn from(error: IdentityError) -> Self {
        match error {
            IdentityError::Invalid(message) => Self::InvalidInput(message),
            IdentityError::InvalidWithSuggestions {
                message,
                did_you_mean,
            } => Self::invalid_input_with_suggestions(message, did_you_mean),
        }
    }
}

impl From<TaskError> for OrbitError {
    fn from(error: TaskError) -> Self {
        match error {
            TaskError::Invalid(message) => Self::InvalidInput(message),
            TaskError::SystemIdentityTagDropped { tag } => Self::SystemIdentityTagDropped { tag },
            unassessed @ TaskError::UnassessedComplexity => {
                Self::InvalidInput(unassessed.to_string())
            }
        }
    }
}

impl From<WorkflowError> for OrbitError {
    fn from(error: WorkflowError) -> Self {
        match error {
            WorkflowError::Invalid(message) => Self::InvalidInput(message),
            WorkflowError::JobValidation(message) => Self::JobValidation(message),
            WorkflowError::SkillValidation(message) => Self::SkillValidation(message),
        }
    }
}

impl From<PolicyError> for OrbitError {
    fn from(error: PolicyError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<ResourceError> for OrbitError {
    fn from(error: ResourceError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<RecordError> for OrbitError {
    fn from(error: RecordError) -> Self {
        match error {
            RecordError::Invalid(message) => Self::InvalidInput(message),
        }
    }
}

impl From<ProviderModelError> for OrbitError {
    fn from(error: ProviderModelError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<PluginGrantError> for OrbitError {
    fn from(error: PluginGrantError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<ArchiveDigestError> for OrbitError {
    fn from(error: ArchiveDigestError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<PluginPinError> for OrbitError {
    fn from(error: PluginPinError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

/// Binding defects are invalid input; a new task that strays outside the
/// claimed task is a policy refusal.
impl From<WorkerBindingError> for OrbitError {
    fn from(error: WorkerBindingError) -> Self {
        match error {
            WorkerBindingError::IncompleteBinding | WorkerBindingError::ArgumentConflict { .. } => {
                Self::InvalidInput(error.to_string())
            }
            WorkerBindingError::NotSpawnedFromClaim
            | WorkerBindingError::ForeignRelation
            | WorkerBindingError::ForeignFindingRelation => Self::PolicyDenied(error.to_string()),
        }
    }
}

impl From<JobRunStateError> for OrbitError {
    fn from(error: JobRunStateError) -> Self {
        Self::JobRunStateTransition(error.to_string())
    }
}

impl From<ProviderSandboxError> for OrbitError {
    fn from(error: ProviderSandboxError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<RetiredBackendError> for OrbitError {
    fn from(error: RetiredBackendError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<FinalRecoveryError> for OrbitError {
    fn from(error: FinalRecoveryError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<ReviewReportError> for OrbitError {
    fn from(error: ReviewReportError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

/// A stored history that cannot be read is a store fault; a report revision
/// the history refuses is the submitter's to correct.
impl From<ReviewHistoryError> for OrbitError {
    fn from(error: ReviewHistoryError) -> Self {
        match error {
            ReviewHistoryError::Unreadable { .. }
            | ReviewHistoryError::UnsupportedVersion { .. } => Self::Store(error.to_string()),
            ReviewHistoryError::AttemptFull { .. }
            | ReviewHistoryError::RecordIdsMissing { .. }
            | ReviewHistoryError::RecordIdReused { .. }
            | ReviewHistoryError::RecordDropped { .. } => Self::InvalidInput(error.to_string()),
        }
    }
}

impl From<ReviewAdmissionError> for OrbitError {
    fn from(error: ReviewAdmissionError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}
