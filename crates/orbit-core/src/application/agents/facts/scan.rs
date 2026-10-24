use super::*;

/// Scan the workspace once. A task whose history or comments cannot be read
/// keeps its envelope facts rather than failing the whole view.
pub(crate) fn scan(runtime: &OrbitRuntime) -> Result<AgentFacts, OrbitError> {
    let computed_at = Utc::now();
    let horizon = computed_at - Duration::days(SCAN_DAYS);
    let tasks = runtime.list_task_metadata()?;
    let tasks_scanned = tasks.len();

    let mut regressions = Vec::new();
    for task in &tasks {
        for relation in &task.relations {
            if relation.relation_type == TaskRelationType::RegressionFrom
                && let Some(classification) = relation.regression
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

    let mut runs = implementation_runs(runtime, horizon);
    let run_costs = run_costs(runtime, horizon, computed_at);
    let leaves = runtime.settled_leaves(false).unwrap_or_else(|error| {
        tracing::warn!(%error, "agent scorecards: claim settlements unreadable");
        Vec::new()
    });
    let settlements = failed_claim_settlements_of(&leaves);
    let pulled: HashMap<&str, Runner> = leaves
        .iter()
        .map(|leaf| {
            let runner = Runner {
                crew: leaf.crew.clone(),
                model: leaf.crew_model.clone(),
                run_id: None,
            };
            (leaf.claim_id.as_str(), runner)
        })
        .collect();
    let candidates = Candidates::open(runtime);
    let mut facts = Vec::new();
    for task in tasks {
        if task.created_at < horizon && task.updated_at < horizon {
            continue;
        }
        let id = task.id.to_string();
        let history = runtime.get_task_history(&id).unwrap_or_else(|error| {
            tracing::warn!(task_id = %id, %error, "agent scorecards: task history unreadable");
            Vec::new()
        });
        let comments = runtime.get_task_comments(&id).unwrap_or_else(|error| {
            tracing::warn!(task_id = %id, %error, "agent scorecards: task comments unreadable");
            Vec::new()
        });
        let task_settlements = settlements.get(&id).map(Vec::as_slice).unwrap_or_default();
        let classify = |entry: &TaskHistoryEntry| block_cause(entry, task_settlements);
        let task_runs = runs.remove(&id).unwrap_or_default();
        let mut task_facts = task_facts(
            &task,
            &history,
            &comments,
            &task_runs,
            &pulled,
            &classify,
            computed_at,
        );
        judge_rejects(&mut task_facts.reviews, &candidates);
        facts.push(task_facts);
    }

    let mut pilot_runs = None;
    let mut pilots = grade_all(pilot_scan(runtime, &mut pilot_runs));
    for task in &mut facts {
        for assessment in &mut task.warnings.assessments {
            let runs = pilot_runs.get_or_insert_with(|| PilotRuns::load(runtime));
            assessment.run = runs.attribute(
                runtime,
                assessment.run_id.as_deref(),
                assessment.at,
                &assessment.operation_id,
                computed_at,
            );
        }
    }
    let scanned: BTreeSet<&str> = facts.iter().map(|task| task.id.as_str()).collect();
    pilots.retain(|graded| scanned.contains(graded.task_id.as_str()));
    let (charges, credits) = attribution::attribute(&facts, &regressions, &pilots);
    Ok(AgentFacts {
        computed_at,
        tasks_scanned,
        tasks: facts,
        charges,
        run_costs,
        credits,
        pilots,
    })
}

/// The pilot evidence of the codebase scan, which keeps each task's landed
/// diff and receipts between scans. A scan that fails leaves the graded
/// terms unscored rather than failing every scorecard. `runs` keeps the
/// task-pilot runs once loaded.
pub(super) fn pilot_scan(runtime: &OrbitRuntime, runs: &mut Option<PilotRuns>) -> PilotEvidence {
    match runtime.codebase_scan() {
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
