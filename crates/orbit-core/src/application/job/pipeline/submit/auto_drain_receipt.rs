use super::super::*;
use crate::application::job::{JobAutoConcurrencyPort, JobDrainPort, ReviewAdmissionHook};

/// One workspace auto-drain submission: the window and the authority this
/// drain carries for every pass.
///
/// Named fields replace the former positional list, where three adjacent
/// `bool`s granted different authority. Fill the options a surface does not
/// offer with `..AutoDrainRequest::default()`.
#[derive(Debug, Clone)]
pub struct AutoDrainRequest<'a> {
    /// Drain window [ORB-10819]; `None` (or zero) means one tick.
    pub for_seconds: Option<u64>,
    /// Concurrency ceiling override; omitted uses the job's own default.
    pub max_active_leaf_runs: Option<u32>,
    pub completion: crate::application::workflow::CompletionPolicy,
    /// Run-scoped crew restriction [ORB-11242]; empty means unrestricted.
    pub allowed_crews: &'a [String],
    pub complexity_crews: orbit_config::ComplexityCrewPools,
    pub actor: Option<&'a str>,
    pub claim_token: Option<&'a str>,
    /// The submitting surface's provenance [ORB-13016].
    pub trigger: JobRunTrigger,
    pub strict_containment: bool,
    /// Approval authority over qualifying `proposed` tasks [ORB-14117].
    pub approve_proposed: bool,
    /// Repair authority over the PRs this drain opens; never merge authority.
    pub auto_fix: bool,
    pub auto_concurrency: Option<orbit_types::workflow::AutoConcurrencyMode>,
    /// Run-scoped `--review` request [ORB-16572].
    pub review: Option<orbit_types::workflow::ReviewLayer>,
}

impl Default for AutoDrainRequest<'_> {
    fn default() -> Self {
        Self {
            for_seconds: None,
            max_active_leaf_runs: None,
            completion: crate::application::workflow::CompletionPolicy::Review,
            allowed_crews: &[],
            complexity_crews: orbit_config::ComplexityCrewPools::default(),
            actor: None,
            claim_token: None,
            trigger: JobRunTrigger::cli(),
            strict_containment: false,
            approve_proposed: false,
            auto_fix: false,
            auto_concurrency: None,
            review: None,
        }
    }
}

impl OrbitRuntime {
    /// Gather persisted run settings, the captured review/forecast mode, and
    /// [`SubmissionNotices`] for an accepted `auto` drain submission, then
    /// return the shared receipt ready for `profile` projection.
    pub fn auto_drain_submission_receipt(
        &self,
        invoke: &PipelineInvokeResult,
        source: AutoDrainSubmissionSource<'_>,
        profile: AutoDrainSubmissionProfile,
    ) -> Result<AutoDrainSubmissionReceipt, OrbitError> {
        let mut completion = crate::application::workflow::CompletionPolicy::Review;
        let mut approve_proposed = false;
        let mut auto_fix = false;
        let mut max_active_leaf_runs = json!(DEFAULT_AUTO_DRAIN_CONCURRENCY);
        let mut allowed_crews = json!([]);
        let mut review = Value::Null;
        let mut auto_concurrency = Value::Null;

        match (source, profile) {
            (AutoDrainSubmissionSource::Local, AutoDrainSubmissionProfile::Cli) => {
                auto_concurrency = self.captured_auto_concurrency(&invoke.run_id)?;
            }
            (AutoDrainSubmissionSource::Local, AutoDrainSubmissionProfile::Tool) => {
                review = self
                    .captured_review_admission(&invoke.run_id)?
                    .as_ref()
                    .map_or(
                        Value::Null,
                        crate::application::contracts::review_layer_json,
                    );
                auto_concurrency = self.captured_auto_concurrency(&invoke.run_id)?;
                let run = self.show_job_run_observed(&invoke.run_id)?;
                completion = recorded_auto_drain_completion(&run);
                approve_proposed = recorded_auto_drain_flag(&run, "approve_proposed");
            }
            (AutoDrainSubmissionSource::Local, AutoDrainSubmissionProfile::Http) => {
                let run = self.show_job_run_observed(&invoke.run_id)?;
                max_active_leaf_runs = self.recorded_auto_drain_limit(&run)?;
                allowed_crews = recorded_auto_drain_allowed_crews(&run);
                completion = recorded_auto_drain_completion(&run);
                approve_proposed = recorded_auto_drain_flag(&run, "approve_proposed");
                auto_fix = recorded_auto_drain_flag(&run, "auto_fix");
                auto_concurrency = self.captured_auto_concurrency(&invoke.run_id)?;
                review = self
                    .captured_review_admission(&invoke.run_id)?
                    .as_ref()
                    .map_or(
                        Value::Null,
                        crate::application::contracts::review_layer_json,
                    );
            }
            (AutoDrainSubmissionSource::Pull { .. }, AutoDrainSubmissionProfile::Cli) => {}
            (
                AutoDrainSubmissionSource::Pull { .. },
                AutoDrainSubmissionProfile::Http | AutoDrainSubmissionProfile::Tool,
            ) => {
                let run = self.show_job_run_observed(&invoke.run_id)?;
                max_active_leaf_runs = self.recorded_auto_drain_limit(&run)?;
                allowed_crews = recorded_auto_drain_allowed_crews(&run);
            }
        }
        let notices = self.submission_notices(match source {
            AutoDrainSubmissionSource::Local => SubmissionKind::Drain,
            AutoDrainSubmissionSource::Pull { .. } => SubmissionKind::PullDrain,
        });
        let mut warnings = notices.warnings();
        if matches!(source, AutoDrainSubmissionSource::Pull { .. }) {
            // [ORB-15622] A replica whose gates drifted from the owner's.
            warnings.extend(self.drain_pull_gate_drift(&invoke.run_id)?);
        }
        let notes = notices.notes();
        Ok(AutoDrainSubmissionReceipt {
            job_id: invoke.job_name.clone(),
            run_id: invoke.run_id.clone(),
            state: if invoke.queued { "queued" } else { "submitted" },
            submitted_at: invoke.submitted_at.clone(),
            profile,
            selector: match source {
                AutoDrainSubmissionSource::Local => None,
                AutoDrainSubmissionSource::Pull { selector } => Some(selector.to_string()),
            },
            completion,
            approve_proposed,
            auto_fix,
            max_active_leaf_runs,
            allowed_crews,
            review,
            auto_concurrency,
            notices,
            warnings,
            notes,
        })
    }

    fn recorded_auto_drain_limit(&self, run: &JobRun) -> Result<Value, OrbitError> {
        let default = self.job_default_input(&run.job_id)?;
        Ok(run
            .input
            .as_ref()
            .and_then(|input| input.get("max_active_leaf_runs"))
            .or_else(|| {
                default
                    .as_ref()
                    .and_then(|input| input.get("max_active_leaf_runs"))
            })
            .cloned()
            .unwrap_or_else(|| json!(DEFAULT_AUTO_DRAIN_CONCURRENCY)))
    }
}

/// Default `max_active_leaf_runs` ceiling when neither the run input nor the
/// drain job definition's `default_input` records one.
const DEFAULT_AUTO_DRAIN_CONCURRENCY: u32 = 5;

/// Whether an `auto` submission started a local owner drain or a replica's
/// pull drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoDrainSubmissionSource<'a> {
    /// A local backlog drain (`orbit run auto`).
    Local,
    /// A replica's pull drain (`orbit run auto --pull <selector>`).
    Pull { selector: &'a str },
}

/// Surface compatibility profile for an `auto` submission receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoDrainSubmissionProfile {
    /// The `orbit run auto` CLI receipt.
    Cli,
    /// The `orbit.workflow` (`action: "auto"`, `mode: "start"`) tool receipt.
    Tool,
    /// The `POST /workflows/auto` full HTTP receipt.
    Http,
}

/// Shared receipt state for an accepted `auto` drain submission across local
/// and pull starts.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoDrainSubmissionReceipt {
    pub job_id: String,
    pub run_id: String,
    pub state: &'static str,
    pub submitted_at: String,
    profile: AutoDrainSubmissionProfile,
    selector: Option<String>,
    pub completion: crate::application::workflow::CompletionPolicy,
    pub approve_proposed: bool,
    pub auto_fix: bool,
    pub max_active_leaf_runs: Value,
    pub allowed_crews: Value,
    pub review: Value,
    pub auto_concurrency: Value,
    notices: SubmissionNotices,
    warnings: Vec<String>,
    notes: Vec<String>,
}

impl AutoDrainSubmissionReceipt {
    /// Canonical workflow alias (`"auto"`).
    #[must_use]
    pub const fn workflow(&self) -> &'static str {
        crate::application::workflow::AUTO_WORKFLOW_ALIAS
    }

    /// Submission warnings in display order, including pull gate drift
    /// after standard [`SubmissionNotices`] warnings on a pull start.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Informational submission notes in display order.
    #[must_use]
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Captured `(mode, source)` summary for a local drain's forecast
    /// controller, or `None` for a pull drain.
    #[must_use]
    pub fn auto_concurrency_summary(&self) -> Option<(&str, &str)> {
        (!self.auto_concurrency.is_null()).then(|| {
            (
                self.auto_concurrency["mode"].as_str().unwrap_or("off"),
                self.auto_concurrency["source"]
                    .as_str()
                    .unwrap_or("configured"),
            )
        })
    }

    /// Project the receipt for its selected [`AutoDrainSubmissionProfile`],
    /// preserving each surface's historical JSON keys and warning/note policy.
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self.profile {
            AutoDrainSubmissionProfile::Cli => {
                let mut value = json!({
                    "workflow": self.workflow(),
                    "job_id": self.job_id,
                    "run_id": self.run_id,
                    "state": self.state,
                    "wait_timeout": false,
                    "attempt": 1,
                    "error_code": Value::Null,
                    "error_message": Value::Null,
                });
                if !self.warnings.is_empty() {
                    value["warning"] = json!(self.warnings.join("\n"));
                }
                if !self.notes.is_empty() {
                    value["note"] = json!(self.notes.join("\n"));
                }
                if !self.auto_concurrency.is_null() {
                    value["auto_concurrency"] = self.auto_concurrency.clone();
                }
                value
            }
            AutoDrainSubmissionProfile::Tool => {
                let mut value = json!({
                    "action": "start",
                    "run_id": self.run_id,
                    "state": self.state,
                    "completion": self.completion.as_input_value(),
                    "approve_proposed": self.approve_proposed,
                    "submitted_at": self.submitted_at,
                    "review": self.review,
                    "auto_concurrency": self.auto_concurrency,
                });
                self.insert_notices_into(&mut value);
                value
            }
            AutoDrainSubmissionProfile::Http => {
                let mut value = match &self.selector {
                    None => json!({
                        "workflow": self.workflow(),
                        "source": "local",
                        "job_id": self.job_id,
                        "run_id": self.run_id,
                        "state": self.state,
                        "submitted_at": self.submitted_at,
                        "completion": self.completion.as_input_value(),
                        "approve_proposed": self.approve_proposed,
                        "auto_fix": self.auto_fix,
                        "max_active_leaf_runs": self.max_active_leaf_runs,
                        "allowed_crews": self.allowed_crews,
                        "auto_concurrency": self.auto_concurrency,
                        "review": self.review,
                    }),
                    Some(selector) => json!({
                        "workflow": self.workflow(),
                        "source": "pull",
                        "selector": selector,
                        "job_id": self.job_id,
                        "run_id": self.run_id,
                        "state": self.state,
                        "submitted_at": self.submitted_at,
                        "max_active_leaf_runs": self.max_active_leaf_runs,
                        "allowed_crews": self.allowed_crews,
                    }),
                };
                self.insert_notices_into(&mut value);
                value
            }
        }
    }

    fn insert_notices_into(&self, receipt: &mut Value) {
        self.notices.insert_into(receipt);
        if !self.warnings.is_empty() {
            receipt["warning"] = json!(self.warnings.join("\n"));
        }
    }
}

fn recorded_auto_drain_allowed_crews(run: &JobRun) -> Value {
    run.input
        .as_ref()
        .and_then(|input| input.get("allowed_crews"))
        .cloned()
        .unwrap_or_else(|| json!([]))
}

fn recorded_auto_drain_completion(run: &JobRun) -> crate::application::workflow::CompletionPolicy {
    match run
        .input
        .as_ref()
        .and_then(|input| input.get("completion"))
        .and_then(Value::as_str)
    {
        Some("done") => crate::application::workflow::CompletionPolicy::Done,
        _ => crate::application::workflow::CompletionPolicy::Review,
    }
}

fn recorded_auto_drain_flag(run: &JobRun, field: &str) -> bool {
    run.input
        .as_ref()
        .and_then(|input| input.get(field))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}
