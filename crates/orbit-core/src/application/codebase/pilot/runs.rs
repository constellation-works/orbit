//! Task-pilot runs and the invocation evidence that attributes a receipt to
//! its run, crew and model.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_store::contracts::{
    InvocationAccountingFact, InvocationAccountingQuery, InvocationQuery, InvocationRecord,
    JobRunQuery,
};
use orbit_types::workflow::{JobRun, TASK_PILOT_PIPELINE_JOB};

use super::inputs::pilot_steps;
use super::wire::PilotRunStateView;
use crate::OrbitRuntime;

/// The pilot run behind an assessment: its id, the crew and model its
/// activity recorded (either `None` when it recorded none) and how long it ran.
#[derive(Debug, Clone, Default, PartialEq)]
pub(in crate::application) struct PilotRun {
    pub(in crate::application) run_id: String,
    pub(in crate::application) crew: Option<String>,
    pub(in crate::application) model: Option<String>,
    /// The worker step that produced the receipt (`pilot`,
    /// `repair_pilot`, …), as invocations record it; `None` when the
    /// producing round cannot be identified.
    pub(in crate::application) activity_id: Option<String>,
    /// Start to finish; `None` while it runs or when either is unrecorded.
    pub(in crate::application) seconds: Option<f64>,
    /// Total tokens the run's recorded invocations spent; `None` when it
    /// recorded none or they could not be read in full.
    pub(in crate::application) tokens: Option<u64>,
}

/// The slice of one invocation record pilot identity reads: its activity,
/// task links, model and token total (`input + output`, as
/// [`InvocationRecord::total_tokens`] is derived).
struct PilotInvocation {
    activity_id: String,
    task_ids: Vec<String>,
    model: Option<String>,
    total_tokens: u64,
}

impl From<InvocationRecord> for PilotInvocation {
    fn from(record: InvocationRecord) -> Self {
        Self {
            activity_id: record.activity_id,
            task_ids: record.task_ids,
            model: record.model,
            total_tokens: record.total_tokens,
        }
    }
}

impl From<InvocationAccountingFact> for PilotInvocation {
    fn from(fact: InvocationAccountingFact) -> Self {
        Self {
            activity_id: fact.activity_id.unwrap_or_default(),
            task_ids: fact.task_ids,
            model: fact.model,
            total_tokens: fact.input_tokens.saturating_add(fact.output_tokens),
        }
    }
}

struct RunRecord {
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    run: PilotRun,
    /// Loaded once per attributed run, shared across its tasks and receipts.
    invocations: RefCell<Option<Vec<PilotInvocation>>>,
    /// The run's pipeline state projection, read once per scan. `None`
    /// until the first attribution touching the run; `Some(None)` when the
    /// state is absent or unreadable, treated as absent without failing
    /// the scan.
    state: RefCell<Option<Option<PilotRunStateView>>>,
}

impl RunRecord {
    /// The run's invocation records, read once.
    fn invocations(&self, runtime: &OrbitRuntime) -> std::cell::RefMut<'_, Vec<PilotInvocation>> {
        std::cell::RefMut::map(self.invocations.borrow_mut(), |invocations| {
            invocations.get_or_insert_with(|| self.load_invocations(runtime))
        })
    }

    /// Total tokens of the run's recorded invocations.
    fn pilot_run_tokens(&self, runtime: &OrbitRuntime) -> Option<u64> {
        let invocations = self.invocations(runtime);
        (!invocations.is_empty()).then(|| {
            invocations
                .iter()
                .map(|invocation| invocation.total_tokens)
                .sum()
        })
    }

    /// Read with the run's pipeline state projection, read once per scan
    /// and shared across its tasks and receipts. A failed read is `None`
    /// (absent), never a scan failure, matching the previous per-call
    /// `ok().flatten()` handling. The state is borrowed, not cloned: a
    /// task-pilot run's state carries every task it assessed, so cloning
    /// it per receipt copies the whole batch for each of its tasks.
    fn with_state<R>(
        &self,
        runtime: &OrbitRuntime,
        read: impl FnOnce(Option<&PilotRunStateView>) -> R,
    ) -> R {
        let mut slot = self.state.borrow_mut();
        if slot.is_none() {
            *slot = Some(
                runtime
                    .read_run_state_json(&self.run.run_id)
                    .ok()
                    .flatten()
                    .and_then(|raw| serde_json::from_str(&raw).ok()),
            );
        }
        read(slot.as_ref().and_then(|state| state.as_ref()))
    }

    fn load_invocations(&self, runtime: &OrbitRuntime) -> Vec<PilotInvocation> {
        load_run_invocations(runtime, &self.run.run_id)
    }

    /// The producing worker and its recorded model for one receipt:
    /// `(activity_id, model)`, either `None` when the round or its model
    /// cannot be identified.
    fn invocation_for_pilot_receipt(
        &self,
        runtime: &OrbitRuntime,
        steps: &[(String, String)],
        producing: &[usize],
        task_id: &str,
    ) -> (Option<String>, Option<String>) {
        let invocations = self.invocations(runtime);
        let workers: Vec<_> = steps
            .iter()
            .enumerate()
            .filter_map(|(index, (_, worker))| {
                let matches = match producing {
                    [producing] => *producing == index,
                    [] => invocations.iter().any(|invocation| {
                        invocation.activity_id == *worker
                            && (invocation.task_ids.is_empty()
                                || invocation.task_ids.iter().any(|id| id == task_id))
                    }),
                    _ => false,
                };
                matches.then_some(worker)
            })
            .collect();
        let [worker] = workers.as_slice() else {
            return (None, None);
        };
        let recorded: Vec<_> = invocations
            .iter()
            .filter(|invocation| invocation.activity_id == **worker)
            .collect();
        // Records are newest first. A task-linked retry supersedes its
        // previous invocation, even when its model is unrecorded.
        let model = if let Some(invocation) = recorded
            .iter()
            .find(|invocation| invocation.task_ids.iter().any(|id| id == task_id))
        {
            invocation.model.clone()
        } else {
            // Legacy unlinked evidence is usable only when unique.
            match recorded.as_slice() {
                [invocation] if invocation.task_ids.is_empty() => invocation.model.clone(),
                _ => None,
            }
        };
        (Some((*worker).clone()), model)
    }
}

/// The workspace's task-pilot runs, read once per scan without their steps:
/// what every receipt, held or clean, is attributed to.
pub(in crate::application) struct PilotRuns {
    runs: Vec<RunRecord>,
    steps: Vec<(String, String)>,
    /// Linked receipts resolve their run without scanning every record.
    by_run_id: HashMap<String, usize>,
    /// Whether [`Self::prefetch_attribution_inputs`] already ran for this
    /// load.
    prefetched: std::cell::Cell<bool>,
}

/// The most invocation records one run's identity read accepts; at the
/// limit the read may be truncated, which cannot establish an identity.
const PILOT_INVOCATION_LIMIT: usize = 1000;

/// Every listed run's invocation records from one accounting read of the
/// workspace, grouped by run, instead of one detailed read per run: the
/// accounting facts carry the activity, task links, model and token
/// components identity reads, oldest first. `None` when the batch read
/// fails, so attribution falls back to the per-run detailed read.
fn batch_invocations_by_run(
    runtime: &OrbitRuntime,
    by_run_id: &HashMap<String, usize>,
) -> Option<HashMap<String, Vec<PilotInvocation>>> {
    let workspace_id = runtime.stores().jobs().workspace_id().to_string();
    let facts = runtime
        .sqlite_store()
        .and_then(|store| {
            store.list_invocation_accounting_facts(&InvocationAccountingQuery {
                workspace_id: Some(workspace_id),
                since: None,
                // The detailed read is unbounded; a day of slack covers a
                // clock that recorded an invocation slightly ahead.
                until: Utc::now() + chrono::Duration::days(1),
            })
        })
        .ok()?;
    let mut by_run: HashMap<String, Vec<PilotInvocation>> = HashMap::new();
    for fact in facts {
        if by_run_id.contains_key(&fact.job_run_id) {
            by_run
                .entry(fact.job_run_id.clone())
                .or_default()
                .push(PilotInvocation::from(fact));
        }
    }
    Some(by_run)
}

/// One run's invocation records, newest first. A truncated or failed read
/// cannot establish an identity and reads as none.
fn load_run_invocations(runtime: &OrbitRuntime, run_id: &str) -> Vec<PilotInvocation> {
    match runtime.workspace_invocation_records(InvocationQuery {
        job_run_id: Some(run_id.to_string()),
        limit: PILOT_INVOCATION_LIMIT,
        // Pilot identity reads activity, task links, model and token totals only.
        skip_tool_calls: true,
        ..Default::default()
    }) {
        Ok(records) if records.len() < PILOT_INVOCATION_LIMIT => {
            records.into_iter().map(PilotInvocation::from).collect()
        }
        Ok(_) => {
            tracing::warn!(%run_id, "pilot identity: invocation read truncated");
            Vec::new()
        }
        Err(error) => {
            tracing::warn!(%run_id, %error, "pilot identity: invocations unreadable");
            Vec::new()
        }
    }
}

impl PilotRuns {
    pub(in crate::application) fn load(runtime: &OrbitRuntime) -> Self {
        let runs = runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(TASK_PILOT_PIPELINE_JOB.to_string()),
                include_steps: false,
                ..Default::default()
            })
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "pilot scorecard: task-pilot runs unreadable");
                Vec::new()
            });
        Self::from_runs(runtime, runs)
    }

    /// Detail reads with linked receipts need only their recorded runs. A
    /// legacy receipt without a run id still needs the overlapping-run scan.
    pub(in crate::application) fn load_for_receipts<'a>(
        runtime: &OrbitRuntime,
        run_ids: impl Iterator<Item = Option<&'a str>>,
    ) -> Self {
        let Some(ids) = run_ids.collect::<Option<BTreeSet<_>>>() else {
            return Self::load(runtime);
        };
        let runs = ids
            .into_iter()
            .filter_map(|id| match runtime.get_job_run_backend(id) {
                Ok(Some(run)) if run.job_id == TASK_PILOT_PIPELINE_JOB => Some(run),
                Ok(_) => None,
                Err(error) => {
                    tracing::warn!(run_id = id, %error, "pilot identity: run unreadable");
                    None
                }
            })
            .collect();
        Self::from_runs(runtime, runs)
    }

    fn from_runs(runtime: &OrbitRuntime, runs: Vec<JobRun>) -> Self {
        // Worker ids, rather than activity catalog names, name invocations.
        let steps = pilot_steps(runtime);
        let runs: Vec<RunRecord> = runs
            .into_iter()
            .map(|run| RunRecord {
                started_at: run.started_at,
                finished_at: run.finished_at,
                run: PilotRun {
                    run_id: run.run_id.to_string(),
                    // Resolved from the run's state when a receipt is
                    // attributed to it; reading every pilot run's state up
                    // front parses the whole fleet's states for the crews
                    // of runs no receipt names.
                    crew: None,
                    model: None,
                    activity_id: None,
                    seconds: run.started_at.zip(run.finished_at).map(|(start, end)| {
                        (end - start).num_milliseconds().max(0) as f64 / 1000.0
                    }),
                    tokens: None,
                },
                invocations: RefCell::new(None),
                state: RefCell::new(None),
            })
            .collect();
        let by_run_id = runs
            .iter()
            .enumerate()
            .map(|(index, record)| (record.run.run_id.clone(), index))
            .collect();
        Self {
            runs,
            steps,
            by_run_id,
            prefetched: std::cell::Cell::new(false),
        }
    }

    /// Read every run's state projection and invocation records up front
    /// instead of one run at a time as receipts attribute. A scan
    /// attributes thousands of receipts, and each state is a serialized
    /// checkpoint carrying every task its run assessed, so the
    /// per-receipt lazy reads pay their queries and whole-text scans in
    /// sequence. Threads fetch and parse their own slices concurrently —
    /// the store's pooled readers serve the reads, and the parses are
    /// independent by run. A failed state batch leaves its slots empty,
    /// so attribution falls back to the per-run read, which treats a
    /// failed read as absent as before. Runs a receipt never names still
    /// cost their inputs; detail reads that loaded only their receipts'
    /// runs therefore skip this.
    pub(in crate::application) fn prefetch_attribution_inputs(&self, runtime: &OrbitRuntime) {
        if self.prefetched.get() {
            return;
        }
        self.prefetched.set(true);
        const CHUNK: usize = 250;
        let pending: Vec<(usize, String)> = self
            .runs
            .iter()
            .enumerate()
            .filter(|(_, record)| {
                record.state.borrow().is_none() || record.invocations.borrow().is_none()
            })
            .map(|(index, record)| (index, record.run.run_id.clone()))
            .collect();
        let fetch_parse = |part: &[(usize, String)]| -> Vec<(usize, Option<PilotRunStateView>)> {
            let mut loaded = Vec::new();
            for chunk in part.chunks(CHUNK) {
                let ids: Vec<String> = chunk.iter().map(|(_, run_id)| run_id.clone()).collect();
                let Ok(mut texts) = runtime.read_run_state_jsons(&ids) else {
                    continue;
                };
                for (index, run_id) in chunk {
                    let view = texts
                        .remove(run_id)
                        .flatten()
                        .and_then(|raw| serde_json::from_str(&raw).ok());
                    loaded.push((*index, view));
                }
            }
            loaded
        };
        let threads = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(4)
            .clamp(1, 8);
        // The invocation batch is one sequential read; it runs on its own
        // thread while the state slices fetch and parse.
        let needs_invocations = pending
            .iter()
            .any(|&(index, _)| self.runs[index].invocations.borrow().is_none());
        let by_run_id = &self.by_run_id;
        let (loaded, mut invocations_by_run) = std::thread::scope(|scope| {
            let invocations = scope.spawn(|| {
                if needs_invocations {
                    batch_invocations_by_run(runtime, by_run_id)
                } else {
                    None
                }
            });
            let loaded = if threads == 1 || pending.len() < 32 {
                fetch_parse(&pending)
            } else {
                let slice = pending.len().div_ceil(threads);
                std::thread::scope(|scope| {
                    let handles: Vec<_> = pending
                        .chunks(slice)
                        .map(|part| scope.spawn(|| fetch_parse(part)))
                        .collect();
                    handles
                        .into_iter()
                        .flat_map(|handle| handle.join().unwrap_or_default())
                        .collect::<Vec<_>>()
                })
            };
            (loaded, invocations.join().unwrap_or_default())
        });
        for (index, view) in loaded {
            let record = &self.runs[index];
            if record.state.borrow().is_none() {
                *record.state.borrow_mut() = Some(view);
            }
        }
        if let Some(by_run) = &mut invocations_by_run {
            for record in &self.runs {
                if record.invocations.borrow().is_some() {
                    continue;
                }
                let mut invocations = by_run.remove(&record.run.run_id).unwrap_or_default();
                if invocations.len() >= PILOT_INVOCATION_LIMIT {
                    tracing::warn!(run_id = %record.run.run_id, "pilot identity: invocation read truncated");
                    invocations.clear();
                } else {
                    // Accounting reads oldest first; identity reads newest first.
                    invocations.reverse();
                }
                *record.invocations.borrow_mut() = Some(invocations);
            }
        }
    }

    /// Explicit run ids win. Legacy receipts use a unique active run, or
    /// disambiguate overlapping runs by exact operation id in an apply report.
    pub(in crate::application) fn attribute(
        &self,
        runtime: &OrbitRuntime,
        task_id: &str,
        run_id: Option<&str>,
        at: DateTime<Utc>,
        operation_id: &str,
        now: DateTime<Utc>,
    ) -> Option<PilotRun> {
        let candidates: Vec<&RunRecord> = if let Some(run_id) = run_id {
            self.by_run_id
                .get(run_id)
                .map(|&index| &self.runs[index])
                .into_iter()
                .collect()
        } else {
            self.runs
                .iter()
                .filter(|record| {
                    record.started_at.is_some_and(|started| started <= at)
                        && record.finished_at.unwrap_or(now) >= at
                })
                .collect()
        };
        let mut found = candidates.iter().filter_map(|record| {
            record.with_state(runtime, |state| {
                let producing: Vec<_> = self
                    .steps
                    .iter()
                    .enumerate()
                    .filter_map(|(index, (step, _))| {
                        let tasks = state?.step_tasks(step)?;
                        let matches = tasks
                            .iter()
                            .any(|task| task.operation_id.as_deref() == Some(operation_id));
                        matches.then_some(index)
                    })
                    .collect();
                if candidates.len() > 1 && producing.len() != 1 {
                    return None;
                }
                let mut run = record.run.clone();
                // A run that drew a `<crew>@auto` pilot ran each partition at
                // its own effort; the applied result names that partition's
                // crew. Else the run's pilots ran as the crew draw its state
                // froze, the lookup the load used to batch-read every state
                // for.
                run.crew = self
                    .steps
                    .iter()
                    .find_map(|(step, _)| {
                        state?
                            .step_tasks(step)?
                            .iter()
                            .find(|task| task.operation_id.as_deref() == Some(operation_id))?
                            .crew
                            .clone()
                    })
                    .or_else(|| {
                        state.and_then(|state| {
                            crate::application::distributed::pilot_run_crew_from_draws(
                                &state.activity_crew_draws,
                            )
                        })
                    });
                (run.activity_id, run.model) =
                    record.invocation_for_pilot_receipt(runtime, &self.steps, &producing, task_id);
                run.tokens = record.pilot_run_tokens(runtime);
                Some(run)
            })
        });
        let run = found.next()?;
        found.next().is_none().then_some(run)
    }
}
