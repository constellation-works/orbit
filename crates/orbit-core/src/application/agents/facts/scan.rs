use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::InvocationQuery;
use orbit_types::task::TaskRelationType;

use super::super::attribution;
use super::super::candidates::Candidates;
use super::super::roles::pilots::grade_all;
use super::attempts::{PulledClaimRunners, implementation_runs};
use super::implementer::mark_implementer_less_attempts;
use super::invocations::AgentInvocationFacts;
use super::launches::AgentLaunchFacts;
use super::model::{AgentFacts, RegressionLink, Runner, SCAN_DAYS};
use super::review::judge_rejects;
use super::stored::StoredFacts;
use crate::OrbitRuntime;
use crate::application::codebase::{
    PilotEvidence, PilotRuns, failed_claim_settlements_of, pilot_evidence,
};

/// Scan the workspace once. A task whose history or comments cannot be read
/// keeps its envelope facts rather than failing the whole view.
/// The horizon reads that do not depend on the task list, read together
/// so a scan can run them beside the task list itself.
struct HorizonTelemetry {
    runs: std::collections::BTreeMap<String, Vec<super::model::RunRecord>>,
    run_costs: std::collections::BTreeMap<String, Option<f64>>,
    leaves: Vec<crate::application::distributed::LeafSettlement>,
    late_claims: Vec<crate::application::distributed::LateClaimAttribution>,
}

/// Read the horizon telemetry: implementation runs, their costs and the
/// claim settlements. Each degrades to empty on a failed read, as it did
/// inline.
fn horizon_telemetry(
    runtime: &OrbitRuntime,
    horizon: DateTime<Utc>,
    computed_at: DateTime<Utc>,
) -> HorizonTelemetry {
    let runs = implementation_runs(runtime, horizon);
    let run_costs = run_costs(runtime, horizon, computed_at);
    let (leaves, late_claims) = runtime.scorecard_claims().unwrap_or_else(|error| {
        tracing::warn!(%error, "agent scorecards: claim settlements unreadable");
        (Vec::new(), Vec::new())
    });
    HorizonTelemetry {
        runs,
        run_costs,
        leaves,
        late_claims,
    }
}

pub(in crate::application::agents) fn scan(
    runtime: &OrbitRuntime,
) -> Result<AgentFacts, OrbitError> {
    let computed_at = Utc::now();
    let horizon = computed_at - Duration::days(SCAN_DAYS);
    // The task list is the scan's longest single read; the horizon
    // telemetry and the task-pilot run list do not depend on it, so they
    // read on their own threads through the store's pooled readers while
    // it lists.
    let (tasks, telemetry, loaded_pilot_runs) = std::thread::scope(|scope| {
        let telemetry = scope.spawn(|| horizon_telemetry(runtime, horizon, computed_at));
        let pilot_load = scope.spawn(|| PilotRuns::load(runtime));
        let tasks = runtime.list_task_metadata();
        let telemetry = telemetry
            .join()
            .unwrap_or_else(|_| horizon_telemetry(runtime, horizon, computed_at));
        let pilot_runs = pilot_load
            .join()
            .unwrap_or_else(|_| PilotRuns::load(runtime));
        (tasks, telemetry, pilot_runs)
    });
    let tasks = tasks?;
    let HorizonTelemetry {
        mut runs,
        run_costs,
        leaves,
        late_claims,
    } = telemetry;
    let tasks_scanned = tasks.len();

    let mut regressions = Vec::new();
    for task in &tasks {
        for relation in &task.relations {
            if relation.relation_type == TaskRelationType::RegressionFrom
                && let Some(classification) = relation.regression.clone()
            {
                regressions.push(RegressionLink {
                    culprit: relation.target.to_string(),
                    regressing: task.id.to_string(),
                    at: task.created_at,
                    classification,
                });
            }
        }
    }

    let settlements = failed_claim_settlements_of(&leaves);
    let mut pulled = PulledClaimRunners::default();
    for (task_id, claim_id, runner) in leaves
        .iter()
        .map(|leaf| {
            let runner = Runner {
                crew: leaf.crew.clone(),
                model: leaf.crew_model.clone(),
                run_id: None,
                reported_effort: leaf.reasoning_effort.clone(),
            };
            (leaf.task_id.as_str(), leaf.claim_id.as_str(), runner)
        })
        .chain(late_claims.iter().map(|late| {
            let runner = Runner {
                crew: late.crew.clone(),
                model: late.crew_model.clone(),
                run_id: None,
                reported_effort: late.reasoning_effort.clone(),
            };
            (late.task_id.as_str(), late.claim_id.as_str(), runner)
        }))
    {
        pulled.crew_models.note_recorded_pair(&runner);
        pulled.by_task.entry(task_id).or_default().push(claim_id);
        pulled.by_claim.insert(claim_id, runner);
    }
    for run in runs.values().flatten() {
        pulled.crew_models.note_recorded_pair(&run.runner);
    }
    let candidates = Candidates::open(runtime);
    let task_ids: Vec<String> = tasks.iter().map(|task| task.id.clone()).collect();
    let mut stored = StoredFacts::load(runtime, &pulled, &task_ids);
    let mut facts = Vec::new();
    for task in &tasks {
        if task.created_at < horizon && task.updated_at < horizon {
            continue;
        }
        // A human crew's task is operator work: its outcome scores no agent
        // crew, ranking or usage row.
        if runtime.is_human_crew(task.crew.as_deref()) {
            continue;
        }
        let id = task.id.to_string();
        let task_settlements = settlements.get(&id).map(Vec::as_slice).unwrap_or_default();
        let task_runs = runs.remove(&id).unwrap_or_default();
        let entry = stored.entry(
            runtime,
            task,
            task_settlements,
            &task_runs,
            &pulled,
            computed_at,
        );
        stored.keep(&id, &entry);
        let mut task_facts = entry.facts;
        judge_rejects(&mut task_facts.reviews, &candidates);
        facts.push(task_facts);
    }
    // The codebase scan below lists the same tasks and stamps the same
    // logs; hand both over instead of paying for them twice.
    let history_stamps = stored.take_stamps();
    stored.save(
        runtime,
        facts
            .iter()
            .filter(|task| {
                task.status.is_terminal()
                    && task.attempts.iter().all(|attempt| attempt.end.is_some())
            })
            .map(|task| task.id.as_str()),
    );

    let mut pilot_runs = Some(loaded_pilot_runs);
    let evidence = pilot_scan(runtime, &mut pilot_runs, tasks, history_stamps);
    let mut pilots = grade_all(evidence);
    for task in &mut facts {
        for assessment in &mut task.warnings.assessments {
            let runs = pilot_runs.get_or_insert_with(|| PilotRuns::load(runtime));
            runs.prefetch_attribution_inputs(runtime);
            assessment.run = runs.attribute(
                runtime,
                &task.id,
                assessment.run_id.as_deref(),
                assessment.at,
                &assessment.operation_id,
                computed_at,
            );
        }
    }
    let scanned: BTreeSet<&str> = facts.iter().map(|task| task.id.as_str()).collect();
    pilots.retain(|graded| scanned.contains(graded.task_id.as_str()));
    let ids = facts.iter().map(|task| task.id.clone()).collect();
    let landed_lines = runtime
        .landed_task_lines_changed(&ids, horizon)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "agent scorecards: landed diff sizes unavailable");
            BTreeMap::new()
        });
    let workspace_id = runtime.stores().jobs().workspace_id().to_string();
    let invocations = runtime.sqlite_store()?.list_invocation_accounting_facts(
        &orbit_store::contracts::InvocationAccountingQuery {
            workspace_id: Some(workspace_id.clone()),
            since: Some(horizon),
            until: computed_at,
        },
    )?;
    // Launch records keep the effort of attempts whose invocation never
    // completed; a failed read leaves those units effort-unknown rather
    // than failing every scorecard, as the other telemetry reads do.
    let launches = runtime
        .sqlite_store()?
        .list_agent_executions(&orbit_store::contracts::AgentExecutionQuery {
            workspace_id: Some(workspace_id),
            since: Some(horizon),
            until: computed_at,
        })
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "agent scorecards: launch records unreadable");
            Vec::new()
        });
    let invocations = AgentInvocationFacts::from(invocations);
    let launches = AgentLaunchFacts::from(launches);
    // Before attribution, so an attempt that launched no implementer is
    // charged no implementer problem.
    mark_implementer_less_attempts(runtime, &mut facts, &invocations, &launches);
    let (charges, credits) = attribution::attribute(&facts, &regressions, &pilots);
    Ok(AgentFacts {
        invocations,
        launches,
        computed_at,
        tasks_scanned,
        tasks: facts,
        charges,
        run_costs,
        credits,
        pilots,
        regressions,
        landed_lines,
        performance_types: runtime.task_types().to_vec(),
    })
}

/// The pilot evidence of the codebase scan, which keeps each task's landed
/// diff and receipts between scans. A scan that fails leaves the graded
/// terms unscored rather than failing every scorecard. `runs` keeps the
/// task-pilot runs once loaded. The caller already listed `tasks` and took
/// their history `stamps` for its own scan, so the codebase scan reuses
/// both instead of listing and stamping again.
pub(super) fn pilot_scan(
    runtime: &OrbitRuntime,
    runs: &mut Option<PilotRuns>,
    tasks: Vec<orbit_types::task::Task>,
    stamps: std::collections::BTreeMap<String, String>,
) -> PilotEvidence {
    match runtime.codebase_scan_with_tasks(tasks, stamps) {
        Ok(codebase) => pilot_evidence(runtime, &codebase, runs),
        Err(error) => {
            tracing::warn!(%error, "agent scorecards: codebase scan failed; pilots unscored");
            PilotEvidence::default()
        }
    }
}

/// The same provider/derived telemetry as Cost, grouped by workspace-local
/// delivery run. Missing prices or a truncated read never become zero spend.
pub(super) fn run_costs(
    runtime: &OrbitRuntime,
    horizon: DateTime<Utc>,
    until: DateTime<Utc>,
) -> BTreeMap<String, Option<f64>> {
    const LIMIT: usize = 100_000;
    let records = match runtime.workspace_invocation_records(InvocationQuery {
        since: Some(horizon),
        until: Some(until),
        limit: LIMIT,
        // Costs read only the invocation rows and their task links.
        skip_tool_calls: true,
        ..Default::default()
    }) {
        Ok(records) if records.len() < LIMIT => records,
        Ok(_) => {
            tracing::warn!("agent scorecards: cost scan truncated; costs unavailable");
            return BTreeMap::new();
        }
        Err(error) => {
            tracing::warn!(%error, "agent scorecards: costs unreadable");
            return BTreeMap::new();
        }
    };
    let mut costs = BTreeMap::new();
    for record in records {
        let cost = record
            .provider_cost_usd
            .or(record.derived_cost_usd)
            .filter(|cost| cost.is_finite() && *cost >= 0.0);
        let total = costs.entry(record.job_run_id).or_insert(Some(0.0));
        *total = total.zip(cost).map(|(sum, cost)| sum + cost);
    }
    costs
}
