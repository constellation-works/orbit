//! The read models the dashboard takes of runs, their invocations and their
//! audit trail, and the one invocation write it sends. Each calls the runtime
//! method the dashboard's handler called before.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_store::contract::LandedDiffStats;
use orbit_store::{
    ActivityInvocationMetrics, InvocationInsertParams, InvocationQuery, InvocationRecord,
    TaskInvocationMetrics, ToolInvocationMetrics, V2AuditEventFilter, V2AuditEventRow,
};
use orbit_types::telemetry::AuditEvent;
use orbit_types::workflow::JobRun;
use serde_json::Value;

use orbit_config::{AutoConcurrencySettings, ExperimentSettings};
use orbit_store::TaskDefectFiling;
use orbit_store::contract::{
    FailureIncidentQuery, TaskCandidates, TaskChangeStamp, TaskListFilter,
};
use orbit_types::task::{DecisionRequest, Task, TaskArtifact, TaskHistoryEntry};
use orbit_types::workflow::JobRunTrigger;

use super::{Command, Operation};
use crate::app::codebase::{self, TaskRiskinessModel};
use crate::app::experiment::{
    self, ExperimentActor, ExperimentChange, ExperimentDefinitionInput, ExperimentProgress,
    ExperimentReadout, ExperimentView,
};
use crate::app::search::GlobalSearchResponse;
use crate::application::change_watch::RunStateStamp;
use crate::application::job::scripts::grant_request::ScriptGrantOverview;
use crate::application::job::{
    EscalationTriageSubmitError, TaskAutoFixError, TaskPilotSubmission, TaskPilotSubmitError,
};
use crate::application::task::{DecisionListProfile, TerminalTaskDates};
use crate::runtime::audit::run::RunAuditStep;
use crate::runtime::engine::ConfiguredCrewRegistryProjection;
use crate::runtime::engine::{FirstImplementEdits, OrchestratorInvocationMetrics};
use crate::runtime::host_resource::HostResourceMonitor;
use crate::runtime::host_resource::accuracy::{AccuracySelection, ForecastAccuracy};
use crate::runtime::host_resource::forecast::HostCapacity;
use crate::runtime::host_resource::profile::ProfileSet;
use crate::runtime::run::cost::InvocationSpend;
use crate::{
    BacklogForecastHistory, FailureIncidentReport, FleetReadinessFacts, OrbitError,
    PipelineInvokeResult, SearchRequest, SearchRequestProfile,
};

/// Define dashboard read commands: each is a constructor on the `Command`
/// whose result it names, running `$body` over the bound runtime.
macro_rules! dashboard_reads {
    ($(
        $(#[$doc:meta])*
        $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $out:ty = $subcommand:literal,
        |$rt:ident| $body:expr;
    )*) => {$(
        impl<'a> Command<'a, $out> {
            $(#[$doc])*
            pub fn $name($($arg: $ty),*) -> Self {
                Self::new(Operation::dashboard_read("run", $subcommand), move |core| {
                    let $rt = core.runtime();
                    $body
                })
            }
        }
    )*};
}

dashboard_reads! {
    /// Invocation counts and durations per activity.
    activity_metrics() -> Vec<ActivityInvocationMetrics> = "activity-metrics",
        |rt| rt.activity_invocation_metrics();
    /// Invocation counts and durations per tool.
    tool_metrics() -> Vec<ToolInvocationMetrics> = "tool-metrics",
        |rt| rt.tool_invocation_metrics();
    /// One task's invocation counts and durations.
    task_metrics(task_id: &'a str) -> TaskInvocationMetrics = "task-metrics",
        |rt| rt.task_invocation_metrics(task_id);
    /// Managed-execution accounting grouped by orchestrator ownership.
    orchestrator_metrics(since: Option<DateTime<Utc>>, until: Option<DateTime<Utc>>)
        -> OrchestratorInvocationMetrics = "orchestrator-metrics",
        |rt| rt.orchestrator_invocation_metrics(since, until);
    /// The bound workspace's invocation records matching `query`.
    invocations(query: InvocationQuery) -> Vec<InvocationRecord> = "invocations",
        |rt| rt.workspace_invocation_records(query);
    /// Invocation spend per task since `since`.
    invocation_spend(since: Option<DateTime<Utc>>) -> Vec<InvocationSpend> = "invocation-spend",
        |rt| rt.invocation_spend(since);
    /// Token totals per task since `since`.
    task_token_totals(since: Option<DateTime<Utc>>)
        -> BTreeMap<String, super::TaskTokenTotals> = "task-token-totals",
        |rt| rt.task_token_totals(since);
    /// The first edit of each implementation invocation since `since`.
    first_implement_edits(since: Option<DateTime<Utc>>) -> FirstImplementEdits
        = "first-implement-edits",
        |rt| rt.first_implement_edits(since);
    /// The diff statistics of deliveries landed since `since`.
    landed_diff_stats(since: Option<DateTime<Utc>>) -> Vec<LandedDiffStats> = "landed-diff-stats",
        |rt| codebase::landed_diff_stats(rt, since);
    /// One page of version-2 audit events.
    audit_events(filter: V2AuditEventFilter) -> Vec<V2AuditEventRow> = "audit-events",
        |rt| rt.list_v2_audit_events(filter);
    /// The audit events with the given ids.
    audit_events_by_ids(ids: &'a [i64]) -> Vec<AuditEvent> = "audit-events-by-ids",
        |rt| crate::app::audit::list_audit_events_by_ids(rt, ids, None);
    /// A run's audit steps, one per attempt.
    audit_step_attempts(run_id: &'a str) -> Vec<RunAuditStep> = "audit-step-attempts",
        |rt| rt.collect_run_audit_step_attempts(run_id);
    /// The change stamp of every run updated since `since`.
    state_stamps(since: DateTime<Utc>) -> Vec<RunStateStamp> = "state-stamps",
        |rt| rt.run_state_stamps(since);
    /// One run's change stamp.
    state_stamp(run_id: &'a str) -> Option<RunStateStamp> = "state-stamp",
        |rt| rt.run_state_stamp(run_id);
    /// The keys whose change means an executor's readiness changed.
    executor_readiness_keys() -> Vec<String> = "executor-readiness-keys",
        |rt| rt.executor_readiness_change_keys();
    /// The phase of the workspace's drain.
    drain_phase() -> &'static str = "drain-phase",
        |rt| rt.workspace_drain_phase();
}

dashboard_reads! {
    /// Whether a plugin lifecycle verb rewrote the host's plugin rows after
    /// this workspace was opened, so a server must reopen it.
    plugin_installed_changed() -> bool = "plugin-installed-changed",
        |rt| crate::app::plugin::plugin_installed_changed(rt);
    /// The host resource monitor the workspace samples.
    host_resource_monitor() -> Arc<HostResourceMonitor> = "host-resource-monitor",
        |rt| Ok(rt.host_resource_monitor());
    /// The global Orbit root the workspace was opened under.
    global_root() -> PathBuf = "global-root",
        |rt| Ok(rt.global_root());
    /// The paths whose volumes hold the workspace: its repository and its
    /// worktrees.
    host_disk_paths() -> [PathBuf; 2] = "host-disk-paths",
        |rt| Ok([
            rt.paths().repo_root.clone(),
            rt.shared_root().join("state/worktrees"),
        ]);
    /// The host's provider usage readings and weekly account and crew
    /// pressures as of `now`, or why they cannot be read.
    provider_limits(now: DateTime<Utc>) -> Result<Value, String> = "provider-limits",
        |rt| {
            let view = rt.provider_limits_view(now);
            Ok(match view.error {
                Some(error) => Err(error),
                None => Ok(serde_json::json!({
                    "provider_limits": view.readings,
                    // The bar list, not readiness's `providers`: it adds the
                    // default-login pressure the merged per-model row shows.
                    "provider_pressures": view.bar_pressures,
                    "crew_pressures": view.crew_pressures,
                })),
            })
        };
    /// The workspace's id.
    workspace_id() -> String = "workspace-id",
        |rt| rt.workspace_id();
}

impl<'a> Command<'a, ()> {
    /// Record one invocation under the serving workspace, whatever workspace
    /// the payload claims. An ordinary write: it requires no operator and
    /// writes no audit row.
    pub fn ingest_invocation(params: &'a InvocationInsertParams) -> Self {
        Self::new(
            Operation::dashboard_read("run", "ingest-invocation"),
            move |core| core.runtime().insert_invocation_trace_record(params),
        )
    }
}

dashboard_reads! {
    /// The crews this host configures.
    crew_registry() -> ConfiguredCrewRegistryProjection = "crew-registry",
        |rt| Ok(rt.configured_crew_registry_projection());
    /// The riskiness model task rows are estimated from.
    task_riskiness_model() -> TaskRiskinessModel = "task-riskiness-model",
        |rt| codebase::task_riskiness_model(rt);
    /// Decision requests across the workspace, in the dashboard's profile.
    decision_requests(gate: Option<&'a str>, task_id: Option<&'a str>, include_closed: bool)
        -> Value = "decision-requests",
        |rt| rt.list_decision_requests_projected(
            gate,
            task_id,
            None,
            include_closed,
            DecisionListProfile::Dashboard,
        );
    /// One decision request.
    decision_request(id: &'a str) -> Option<DecisionRequest> = "decision-request",
        |rt| rt.get_decision_request(id);
    /// The directory holding the audit trail's blobs.
    audit_blob_root() -> PathBuf = "audit-blob-root",
        |rt| Ok(rt.data_root().join("state").join("audit").join("blobs"));
    /// The backlog forecast.
    backlog_forecast(
        history: BacklogForecastHistory,
        concurrency: Option<u32>,
        arrivals: Option<&'a str>,
        refresh: bool,
    ) -> Value = "backlog-forecast",
        |rt| rt.serve_backlog_forecast(history, concurrency, arrivals, refresh);
    /// When each terminal task reached its terminal status.
    task_terminal_dates(since: Option<DateTime<Utc>>) -> TerminalTaskDates = "task-terminal-dates",
        |rt| rt.task_terminal_dates(since);
    /// Defect task filings since `since`.
    task_defect_filings(since: Option<DateTime<Utc>>) -> Vec<TaskDefectFiling>
        = "task-defect-filings",
        |rt| rt.task_defect_filings(since);
    /// Every task's metadata.
    task_metadata() -> Vec<Task> = "task-metadata",
        |rt| rt.list_task_metadata();
    /// The metadata of the tasks in `ids`, from the settled snapshot.
    task_metadata_for_ids(ids: &'a BTreeSet<String>) -> Vec<Task> = "task-metadata-for-ids",
        |rt| rt.observe_task_metadata_for_ids(ids);
    /// One task's history.
    task_history(id: &'a str) -> Vec<TaskHistoryEntry> = "task-history",
        |rt| rt.get_task_history(id);
    /// Each task's complexity.
    task_complexity_by_id() -> BTreeMap<String, String> = "task-complexity",
        |rt| rt.task_complexity_by_id();
    /// The stored record of one of this workspace's runs.
    stored_run(run_id: &'a str) -> Option<JobRun> = "stored-run",
        |rt| {
            let workspace_id = rt.workspace_id()?;
            rt.sqlite_store()?
                .read()
                .and_then(|snapshot| snapshot.job_run().get(&workspace_id, run_id))
        };
    /// One task artifact.
    task_artifact(id: &'a str, path: &'a str) -> Option<TaskArtifact> = "task-artifact",
        |rt| rt.get_task_artifact(id, path);
    /// The task candidates a filter observes.
    task_candidates(filter: &'a TaskListFilter, limit: usize) -> TaskCandidates
        = "task-candidates",
        |rt| rt.observe_task_candidates(filter, limit);
    /// A global search in one request profile.
    search(profile: SearchRequestProfile, request: SearchRequest) -> GlobalSearchResponse
        = "search",
        |rt| crate::app::search::search_with_profile(rt, profile, request);
    /// Each workspace script's grant state.
    script_grant_overview() -> Vec<ScriptGrantOverview> = "script-grant-overview",
        |rt| rt.script_grant_overview();
    /// Whether a task's pull request can be auto-fixed now.
    task_pr_auto_fix_readiness(task_id: &'a str, completion_done: bool) -> Value
        = "task-pr-auto-fix-readiness",
        |rt| rt.task_pr_auto_fix_readiness(task_id, completion_done);
    /// Start the auto-fix of a task's pull request.
    start_task_pr_auto_fix(task_id: &'a str, completion_done: bool, actor: Option<&'a str>)
        -> Result<Value, TaskAutoFixError> = "task-pr-auto-fix",
        |rt| Ok(rt.start_task_pr_auto_fix(task_id, completion_done, actor));
    /// Submit one task to the pilot.
    submit_task_pilot(task_id: &'a str, actor: Option<&'a str>, trigger: JobRunTrigger)
        -> Result<TaskPilotSubmission, TaskPilotSubmitError> = "task-pilot",
        |rt| Ok(rt.submit_explicit_task_pilot(task_id, actor, trigger));
    /// Submit one task to escalation triage.
    submit_escalation_triage(task_id: &'a str, actor: Option<&'a str>, trigger: JobRunTrigger)
        -> Result<PipelineInvokeResult, EscalationTriageSubmitError> = "escalation-triage",
        |rt| Ok(rt.submit_explicit_escalation_triage(task_id, actor, trigger));
    /// Whether this checkout may write coordination tasks, and the machine
    /// identity its automation writes under.
    task_write_identity() -> Option<String> = "task-write-identity",
        |rt| {
            rt.ensure_coordination_task_write_permitted()?;
            Ok(rt.automation_machine_identity().map(str::to_string))
        };
}

dashboard_reads! {
    /// The active triage or recovery binding of each task that has one.
    active_task_triage() -> BTreeMap<String, Value> = "active-task-triage",
        |rt| crate::application::task::active_task_triage_map(rt);
    /// What the experiment authoring form offers: the workspace's
    /// experiment settings and the crews this host configures.
    experiment_authoring() -> (ExperimentSettings, ConfiguredCrewRegistryProjection)
        = "experiment-authoring",
        |rt| Ok((
            rt.experiment_settings().clone(),
            rt.configured_crew_registry_projection(),
        ));
    /// Each experiment's progress.
    experiment_progress() -> Vec<ExperimentProgress> = "experiment-progress",
        |rt| experiment::experiment_progress(rt);
    /// One experiment.
    experiment_view(id_or_name: &'a str) -> ExperimentView = "experiment-view",
        |rt| experiment::experiment_view(rt, id_or_name);
    /// One experiment's readout.
    experiment_readout(id_or_name: &'a str, metric: Option<&'a str>) -> ExperimentReadout
        = "experiment-readout",
        |rt| experiment::experiment_readout(rt, id_or_name, metric);
    /// Create an experiment.
    create_experiment(name: &'a str, input: ExperimentDefinitionInput, by: ExperimentActor)
        -> ExperimentView = "experiment-create",
        |rt| experiment::create_experiment(rt, name, input, by);
    /// Replace an experiment's definition.
    update_experiment(id_or_name: &'a str, input: ExperimentDefinitionInput, by: ExperimentActor)
        -> ExperimentView = "experiment-update",
        |rt| experiment::update_experiment(rt, id_or_name, input, by);
    /// Change an experiment's state.
    change_experiment(id_or_name: &'a str, change: ExperimentChange, by: ExperimentActor)
        -> ExperimentView = "experiment-change",
        |rt| experiment::change_experiment(rt, id_or_name, change, by);
    /// The auto-concurrency settings.
    auto_concurrency_settings() -> AutoConcurrencySettings = "auto-concurrency-settings",
        |rt| Ok(rt.auto_concurrency_settings());
    /// The forecast's accuracy over a selection, reported at `report_at`.
    forecast_accuracy(selection: AccuracySelection, report_at: DateTime<Utc>) -> ForecastAccuracy
        = "forecast-accuracy",
        |rt| rt.forecast_accuracy_selected(selection, report_at);
    /// The host's capacity, when it was probed.
    host_capacity() -> Option<HostCapacity> = "host-capacity",
        |rt| Ok(rt.host_capacity());
    /// The resource profiles, over `window` samples when one is named.
    resource_profiles(window: Option<usize>) -> Arc<ProfileSet> = "resource-profiles",
        |rt| match window {
            Some(window) => rt.resource_profiles_over(window),
            None => rt.resource_profiles(),
        };
    /// The change stamp of each task.
    task_change_stamps() -> Arc<Vec<TaskChangeStamp>> = "task-change-stamps",
        |rt| rt.task_change_stamps();
    /// The keys of the active task reservations.
    task_reservation_keys() -> Vec<String> = "task-reservation-keys",
        |rt| rt.active_task_reservation_keys();
    /// The failure incidents the audit trail records.
    failure_incidents(query: &'a FailureIncidentQuery) -> FailureIncidentReport
        = "failure-incidents",
        |rt| crate::app::audit::audit_failure_incidents(rt, query);
    /// How many audit events were recorded since `since`.
    audit_event_count(since: Option<DateTime<Utc>>) -> i64 = "audit-event-count",
        |rt| crate::app::audit::audit_event_count(rt, since);
    /// The facts fleet readiness is judged from.
    fleet_readiness_facts() -> FleetReadinessFacts = "fleet-readiness-facts",
        |rt| Ok(rt.fleet_readiness_facts());
    /// This workspace's fleet readiness under `facts`, and the machine that
    /// owns it when this checkout is a replica.
    fleet_readiness(facts: &'a FleetReadinessFacts) -> (Result<Value, OrbitError>, Option<String>)
        = "fleet-readiness",
        |rt| Ok((
            rt.workspace_fleet_readiness(facts),
            rt.replica_owner_machine().map(str::to_string),
        ));
    /// Whether the workspace grants leases.
    leases_enabled() -> bool = "leases-enabled",
        |rt| Ok(rt.leases_enabled());
}
