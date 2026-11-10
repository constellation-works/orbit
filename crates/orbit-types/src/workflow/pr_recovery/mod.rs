//! Durable ledgers for delivery PR recovery.

mod auto_fix;
mod base_update;
mod ci_failure;
mod ci_rerun;
mod completion;
mod infrastructure;

pub use auto_fix::{
    PR_AUTO_FIX_ADMITTED_EVENT, PR_AUTO_FIX_BASE_RED_EVENT, PR_AUTO_FIX_BUDGET,
    PR_AUTO_FIX_CHECK_REPAIR_MARKER, PR_AUTO_FIX_CREWS_KEY, PR_AUTO_FIX_NO_EDIT_RERUN_EVENT,
    PR_AUTO_FIX_PUSHED_EVENT, PR_AUTO_FIX_READ_DEFERRED_EVENT, PrAutoFixAttempt, PrAutoFixBaseRed,
    PrAutoFixBlocker, PrAutoFixClaim, PrAutoFixDecision, PrAutoFixNoEditRerun, PrAutoFixPush,
    PrAutoFixReadDeferral, PrAutoFixReadReset, decide_pr_auto_fix, is_pr_auto_fix_check_repair,
    pr_auto_fix_attempts, pr_auto_fix_no_edit_reruns, pr_auto_fix_pushes,
};
pub use base_update::{
    PR_BASE_UPDATE_BUDGET_SPENT, PR_BASE_UPDATE_RESERVED_EVENT, PrBaseUpdateAdmission,
    PrBaseUpdateReservation, decide_pr_base_update, pr_base_update_reservations,
};
pub use completion::{PR_COMPLETION_MUTATION_STARTED_EVENT, pr_completion_has_mutation_provenance};
pub use infrastructure::{
    PR_INFRASTRUCTURE_RESERVED_EVENT, PrInfrastructureAdmission, PrInfrastructureEvidence,
    PrInfrastructureReservation, RequiredGate, decide_pr_infrastructure,
    pr_infrastructure_reservations,
};

pub use ci_failure::{
    CI_FAILURE_RECLASSIFIED_EVENT, CI_FAILURE_RECORDED_EVENT, CI_FAILURE_SCHEMA_VERSION,
    CiFailureClass, CiFailureClassChange, CiFailureLedgerEntry, CiFailureReclassification,
    CiFailureRecord, ci_failure_ledger,
};
pub use ci_rerun::{
    CI_HANDOFF_STEP_SIGNATURE_PREFIX, CI_RERUN_RESERVED_EVENT, CiInfrastructureRetryPolicy,
    CiRerunAdmission, CiRerunPlan, CiRerunRoot, ci_handoff_step_matches, ci_handoff_step_signature,
    ci_rerun_is_infrastructure, decide_ci_rerun, decide_ci_rerun_with_policy,
};
