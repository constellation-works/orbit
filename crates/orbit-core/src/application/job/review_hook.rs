//! The review admission job submission and resume capture and check
//! [ORB-17924].
//!
//! Every delivery submission, replay and source-repair resume installs the
//! review admission a run carries, and resume compares the captured one with
//! today's policy. `review` owns those rules, and it also drives job runs,
//! so job reaches them only through this hook: `review` registers by
//! implementing it on [`OrbitRuntime`](crate::OrbitRuntime), and composing
//! the runtime binds it.

use orbit_common::OrbitError;
use orbit_types::workflow::{JobRun, ReviewAdmission, ReviewLayer};
use serde_json::Value;

/// Review admission operations job submission and resume call, implemented
/// by `review`.
pub(in crate::application) trait ReviewAdmissionHook {
    /// Install the review admission a submission of `job_name` carries into
    /// `input`, or refuse a submission that tries to supply one.
    /// `parent_run_id` is set for a parent-authorized child; `resuming`
    /// keeps persisted input as it is.
    fn install_review_admission(
        &self,
        job_name: &str,
        input: &mut Value,
        parent_run_id: Option<&str>,
        resuming: bool,
    ) -> Result<(), OrbitError>;

    /// Record a run's `--review` request in `input`, refusing it before any
    /// run exists when it could not be admitted.
    fn admit_review_request(
        &self,
        input: &mut Value,
        review: Option<ReviewLayer>,
        local_route: bool,
    ) -> Result<(), OrbitError>;

    /// The refusal for a `--review` request on the local-only delivery route.
    fn local_route_review_refusal(&self, layer: ReviewLayer) -> String;

    /// The review admission run `run_id` was admitted under, from its
    /// persisted input.
    fn captured_review_admission(
        &self,
        run_id: &str,
    ) -> Result<Option<ReviewAdmission>, OrbitError>;

    /// Why an automatically resumed delivery run cannot keep its captured
    /// review admission, or `None` when it can.
    fn upgrade_resume_review_mismatch(&self, run: &JobRun) -> Option<String>;

    /// Why completion observation of `run`'s unchanged candidate cannot keep
    /// its captured review admission, or `None` when it can.
    fn completion_review_mismatch(&self, run: &JobRun) -> Option<String>;
}
