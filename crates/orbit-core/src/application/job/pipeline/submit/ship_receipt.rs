use super::super::{PipelineInvokeResult, SubmissionKind, SubmissionNotices};
use crate::application::job::ReviewAdmissionHook;
use crate::{OrbitError, OrbitRuntime};
use serde_json::{Value, json};

/// Surface compatibility profile for a ship submission receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShipSubmissionProfile {
    /// The native CLI retains its generic one-shot dispatch fields.
    Cli,
    /// The `orbit.workflow` ship tool receipt.
    Tool,
    /// The `POST /workflows/ship` HTTP receipt.
    Http,
}

/// One accepted ship submission, projected consistently across surfaces.
#[derive(Debug, Clone, PartialEq)]
pub struct ShipSubmissionReceipt {
    pub job_id: String,
    pub run_id: String,
    pub state: &'static str,
    submitted_at: String,
    review: Value,
    profile: ShipSubmissionProfile,
    notices: SubmissionNotices,
}

impl OrbitRuntime {
    /// Capture the persisted review layer and shared notices for an accepted
    /// ship run. `explicit` distinguishes named tasks from backlog discovery.
    pub fn ship_submission_receipt(
        &self,
        invoke: &PipelineInvokeResult,
        explicit: bool,
        profile: ShipSubmissionProfile,
    ) -> Result<ShipSubmissionReceipt, OrbitError> {
        Ok(ShipSubmissionReceipt {
            job_id: invoke.job_name.clone(),
            run_id: invoke.run_id.clone(),
            state: if invoke.queued { "queued" } else { "submitted" },
            submitted_at: invoke.submitted_at.clone(),
            review: self
                .captured_review_admission(&invoke.run_id)?
                .as_ref()
                .map_or(
                    Value::Null,
                    crate::application::contracts::review_layer_json,
                ),
            profile,
            notices: self.submission_notices(SubmissionKind::Ship { explicit }),
        })
    }
}

impl ShipSubmissionReceipt {
    /// Canonical workflow alias.
    #[must_use]
    pub const fn workflow(&self) -> &'static str {
        crate::application::workflow::SHIP_WORKFLOW_ALIAS
    }

    /// Submission warnings in display order.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        self.notices.warnings()
    }

    /// Informational submission notes in display order.
    #[must_use]
    pub fn notes(&self) -> Vec<String> {
        self.notices.notes()
    }

    /// Project the shared receipt, preserving CLI-only dispatch fields and
    /// rendering notices through their canonical projection.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut receipt = json!({
            "workflow": self.workflow(),
            "job_id": self.job_id,
            "run_id": self.run_id,
            "state": self.state,
            "submitted_at": self.submitted_at,
            "review": self.review,
        });
        if self.profile == ShipSubmissionProfile::Cli {
            receipt["wait_timeout"] = json!(false);
            receipt["attempt"] = json!(1);
            receipt["error_code"] = Value::Null;
            receipt["error_message"] = Value::Null;
        }
        self.notices.insert_into(&mut receipt);
        receipt
    }
}
