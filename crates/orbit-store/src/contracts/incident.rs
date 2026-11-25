//! Record shapes and query parameters for failure incident reports.

pub use orbit_types::telemetry::incident::{
    CauseBasis, DEFAULT_SCAN_LIMIT, DOCTOR_FINDINGS_MESSAGE_PREFIX, FailureClass, FailureIncident,
    FailureIncidentQuery, FailureIncidentReport, IncidentCause, IncidentEventRef,
    JOB_RUN_LIFECYCLE_LABEL, LIFECYCLE_DIAGNOSTIC_LABEL, PropagationLink, RUN_DEFERRED_SURFACE,
    is_failure_only_diagnostic_surface,
};
