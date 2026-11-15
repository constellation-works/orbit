//! Task-pilot receipts and the evidence the Agents › Pilots scorecard grades:
//! each done task a pilot assessed, with its landed diff, the pilot's and the
//! filer's selectors and the paths its run was contested on, and the lock
//! waits other tasks spent on paths each such task held.
//!
//! A task's pilot selectors are the newest task-pilot receipt's
//! `context_files_after`; the filer's are the oldest receipt's
//! `context_files_before`, what the task declared before any pilot ran. The
//! receipt names its pilot job run (`pilot_run_id`), whose record holds the
//! frozen activity crew draw; invocation evidence holds the pilot model. A
//! receipt written before that field is matched to the one task-pilot run
//! active when it was written, or among several, the one whose apply report
//! carries its exact operation id. Models come from the producing worker step
//! when its apply report is available. Without a report, only a single
//! recorded worker can identify the round (including a repair-only run).
//! Task links distinguish fan-out invocations; ambiguous evidence stays unknown.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    InvocationAccountingFact, InvocationAccountingQuery, InvocationQuery, InvocationRecord,
    JobRunQuery,
};
use orbit_types::workflow::{
    ActivityCrewDraw, JobRun, JobV2Step, JobV2StepBody, TASK_PILOT_PIPELINE_JOB,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::facts::{Anchor, BlockCause, CodebaseFacts};
use super::lock_waits::wait_episodes;
use super::regression::RegressionFilter;
use super::sections::{LARGE_FOOTPRINT_CAP, Params, Rollup};
use crate::OrbitRuntime;

/// The actor that writes task-pilot receipts.
const PILOT_ACTOR: &str = "task-pilot";
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

/// The slice of a task-pilot run's pipeline state attribution reads: its
/// frozen crew draws and, per apply step, the operation id and recorded
/// crew of each applied task. A run's state carries every task it assessed
/// in its accumulated pipeline value and per-step outputs, so deserializing
/// the whole checkpoint to read this slice costs a scan over thousands of
/// pilot runs seconds; this projection skips everything else as it parses.
/// A state whose projection does not parse reads as absent, as an
/// unreadable checkpoint did when it was parsed whole.
#[derive(Deserialize)]
struct PilotRunStateView {
    #[serde(default)]
    activity_crew_draws: BTreeMap<String, ActivityCrewDraw>,
    pipeline: BTreeMap<String, PilotStepOutputView>,
}

impl PilotRunStateView {
    /// The applied tasks the step named `step_id` checkpointed into
    /// `pipeline`, as `PipelineState::pipeline_step_output` reads them.
    fn step_tasks(&self, step_id: &str) -> Option<&[PilotTaskRef]> {
        self.pipeline.get(step_id)?.tasks.as_deref()
    }
}

/// One applied task an apply step's output names.
#[derive(Deserialize)]
struct PilotTaskRef {
    #[serde(default)]
    operation_id: Option<String>,
    /// The partition crew the applied result recorded; the field
    /// `PILOT_CREW_FIELD` names.
    #[serde(default, rename = "pilot_crew")]
    crew: Option<String>,
}

/// An apply step's checkpointed output: only its applied tasks. Any other
/// shape — a scalar or array pipeline value, a `tasks` that is no array —
/// reads as no tasks, as indexing the whole value did.
struct PilotStepOutputView {
    tasks: Option<Vec<PilotTaskRef>>,
}

impl<'de> Deserialize<'de> for PilotStepOutputView {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct OutputVisitor;
        impl<'de> serde::de::Visitor<'de> for OutputVisitor {
            type Value = PilotStepOutputView;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a pipeline step output")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut tasks = None;
                while let Some(key) = access.next_key::<String>()? {
                    if key == "tasks" {
                        tasks = access.next_value::<PilotTasks>()?.0;
                    } else {
                        access.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                Ok(PilotStepOutputView { tasks })
            }

            fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while access.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(PilotStepOutputView { tasks: None })
            }
        }
        deserializer.deserialize_any(OutputVisitor)
    }
}

/// An apply output's `tasks`: the array's task refs, or nothing for any
/// other shape.
struct PilotTasks(Option<Vec<PilotTaskRef>>);

impl<'de> Deserialize<'de> for PilotTasks {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TasksVisitor;
        impl<'de> serde::de::Visitor<'de> for TasksVisitor {
            type Value = PilotTasks;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("applied tasks")
            }

            fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut tasks = Vec::new();
                while let Some(task) = access.next_element()? {
                    tasks.push(task);
                }
                Ok(PilotTasks(Some(tasks)))
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                while access
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(PilotTasks(None))
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(PilotTasks(None))
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(PilotTasks(None))
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(PilotTasks(None))
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(PilotTasks(None))
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(PilotTasks(None))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(PilotTasks(None))
            }
        }
        deserializer.deserialize_any(TasksVisitor)
    }
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

/// Apply step ids paired with their pilot worker ids, in pipeline order.
/// Read the job definition so renaming a worker or apply step preserves attribution.
fn pilot_steps(runtime: &OrbitRuntime) -> Vec<(String, String)> {
    let job = match runtime.resolved_job_spec(TASK_PILOT_PIPELINE_JOB) {
        Ok(job) => job,
        Err(error) => {
            tracing::warn!(%error, "pilot scorecard: task-pilot definition unreadable");
            return Vec::new();
        }
    };
    job.steps
        .iter()
        .filter_map(|apply| {
            let input = activity_input(apply, "apply_task_pilot_results")?;
            let results = input.get("results")?.as_str()?;
            let worker = job.steps.iter().find_map(|step| {
                let JobV2StepBody::FanOut { fan_out, fan_in } = &step.body else {
                    return None;
                };
                activity_input(&fan_out.worker, "task_pilot")?;
                let collect = fan_in.collect.as_deref().unwrap_or(&step.id);
                (results == format!("{{{{ steps.{collect}.output }}}}"))
                    .then_some(&fan_out.worker.id)
            })?;
            Some((apply.id.clone(), worker.clone()))
        })
        .collect()
}

fn activity_input<'a>(step: &'a JobV2Step, name: &str) -> Option<&'a Value> {
    match &step.body {
        JobV2StepBody::Target(target) if target.activity_name.as_deref() == Some(name) => {
            target.default_input.as_ref()
        }
        JobV2StepBody::TargetRef(target) if target.target == format!("activity:{name}") => {
            target.default_input.as_ref()
        }
        _ => None,
    }
}

fn strings(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|values| {
        values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

/// What a task's task-pilot receipts record, before the newest one is
/// attributed to a run: kept with the task's stored facts, since only its
/// comments change it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PilotInputs {
    /// The newest assessment's final selectors.
    after: Vec<String>,
    /// The filer's selectors before any pilot ran.
    author: Vec<String>,
    /// The pilot run the newest assessment names, when it names one.
    run_id: Option<String>,
    /// When the newest assessment was written, and its operation id: what a
    /// receipt without a run id is matched by.
    at: DateTime<Utc>,
    operation_id: String,
}

/// Read a task's task-pilot receipts: `None` when no receipt carries the
/// pilot's final selectors, an error when the comments cannot be read.
pub(super) fn inputs(runtime: &OrbitRuntime, id: &str) -> Result<Option<PilotInputs>, OrbitError> {
    let comments = runtime.get_task_pilot_comments(id)?;
    // Comments are stored oldest first.
    let receipts: Vec<_> = comments
        .iter()
        .filter(|comment| comment.by == PILOT_ACTOR)
        .filter_map(|comment| {
            let receipt = comment.receipt()?;
            let body: Value = serde_json::from_str(receipt.body).ok()?;
            Some((comment.at, receipt.operation_id, body))
        })
        .collect();
    let Some((at, operation_id, newest)) = receipts
        .iter()
        .rev()
        .find(|(_, _, body)| body["assessment"]["context_files_after"].is_array())
    else {
        return Ok(None);
    };
    let Some(after) = strings(&newest["assessment"]["context_files_after"]) else {
        return Ok(None);
    };
    let author = receipts
        .first()
        .and_then(|(_, _, oldest)| {
            strings(&oldest["context_files_before"])
                .or_else(|| strings(&oldest["assessment"]["context_files_before"]))
        })
        .unwrap_or_default();
    Ok(Some(PilotInputs {
        after,
        author,
        run_id: newest["pilot_run_id"].as_str().map(str::to_string),
        at: *at,
        operation_id: operation_id.to_string(),
    }))
}

/// One done task a pilot assessed: what its receipts and landed diff say,
/// and the pilot run behind its newest assessment.
#[derive(Debug, Clone)]
pub(in crate::application) struct Assessment {
    pub(in crate::application) task_id: String,
    /// When the newest assessment was written: the window it falls in.
    pub(in crate::application) at: DateTime<Utc>,
    /// `None` when no task-pilot run is found for the receipt.
    pub(in crate::application) run: Option<PilotRun>,
    /// The pilot's final selectors.
    pub(in crate::application) after: Vec<String>,
    /// The filer's selectors before any pilot ran.
    pub(in crate::application) author: Vec<String>,
    /// The files the landed commit changed.
    pub(in crate::application) landed: BTreeSet<String>,
    /// Paths the task's own work was contested on: those a base-conflict
    /// block recorded and those a refused lock request of the task named,
    /// each with when and which.
    pub(in crate::application) contested: Vec<Contest>,
}

/// One path a task's work was contested on.
#[derive(Debug, Clone)]
pub(in crate::application) struct Contest {
    pub(in crate::application) at: DateTime<Utc>,
    pub(in crate::application) path: Anchor,
    /// `base conflict` or `lock conflict`.
    pub(in crate::application) kind: &'static str,
}

/// One lock-wait episode another task spent on a path an assessed task held.
#[derive(Debug, Clone)]
pub(in crate::application) struct HeldWait {
    /// The assessed task holding the path.
    pub(in crate::application) holder: String,
    pub(in crate::application) waiter: String,
    pub(in crate::application) path: Anchor,
    pub(in crate::application) started_at: DateTime<Utc>,
    pub(in crate::application) wait_ms: i64,
    /// How the wait ended: `admitted`, `left_backlog` or `waiting`.
    pub(in crate::application) end: &'static str,
}

/// What the pilots scorecard reads from one codebase scan.
#[derive(Debug, Clone, Default)]
pub(in crate::application) struct PilotEvidence {
    pub(in crate::application) assessments: Vec<Assessment>,
    pub(in crate::application) waits: Vec<HeldWait>,
}

/// Every done task with a landing receipt and a pilot assessment, apart
/// from bulk tasks (more than [`LARGE_FOOTPRINT_CAP`] landed files) and
/// `no-diff-expected` ones, each attributed to its pilot run, and every lock
/// wait, whenever it happened, on a path one of them held. `runs` is loaded
/// on first use and left for the caller to attribute other receipts with.
pub(in crate::application) fn pilot_evidence(
    runtime: &OrbitRuntime,
    facts: &CodebaseFacts,
    runs: &mut Option<PilotRuns>,
) -> PilotEvidence {
    // Denials by the task they named, in recorded order, so each assessed
    // task reads its own instead of scanning every denial.
    let mut denials_by_task: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, denial) in facts.lock_denials.iter().enumerate() {
        for task_id in &denial.task_ids {
            denials_by_task
                .entry(task_id.as_str())
                .or_default()
                .push(index);
        }
    }
    let mut assessments = Vec::new();
    for task in &facts.tasks {
        let (Some(landed), Some(inputs)) = (&task.landed, &task.pilot) else {
            continue;
        };
        if task.no_diff || landed.paths.len() > LARGE_FOOTPRINT_CAP {
            continue;
        }
        let runs = runs.get_or_insert_with(|| PilotRuns::load(runtime));
        runs.prefetch_attribution_inputs(runtime);
        let run = runs.attribute(
            runtime,
            &task.id,
            inputs.run_id.as_deref(),
            inputs.at,
            &inputs.operation_id,
            facts.computed_at,
        );
        let mut contested: Vec<Contest> = task
            .blocks
            .iter()
            .filter_map(|block| match &block.cause {
                BlockCause::BaseConflict(paths) => Some((block.at, paths)),
                _ => None,
            })
            .flat_map(|(at, paths)| {
                paths.iter().map(move |path| Contest {
                    at,
                    path: path.clone(),
                    kind: "base conflict",
                })
            })
            .collect();
        for denial in denials_by_task
            .get(task.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|&index| &facts.lock_denials[index])
        {
            contested.extend(denial.files.iter().map(|path| Contest {
                at: denial.at,
                path: path.clone(),
                kind: "lock conflict",
            }));
        }
        assessments.push(Assessment {
            task_id: task.id.clone(),
            at: inputs.at,
            run,
            after: inputs.after.clone(),
            author: inputs.author.clone(),
            landed: landed.paths.clone(),
            contested,
        });
    }
    let assessed: BTreeSet<&str> = assessments
        .iter()
        .map(|assessment| assessment.task_id.as_str())
        .collect();
    let every_wait = Params {
        since: None,
        min_support: 1,
        rollup: Rollup::File,
        filter: RegressionFilter::default(),
    };
    let mut waits = Vec::new();
    for (key, episode) in wait_episodes(facts, &every_wait) {
        let path = Anchor {
            path: key.trim_end_matches('/').to_string(),
            dir: key.ends_with('/'),
        };
        for holder in episode
            .holders
            .iter()
            .filter(|holder| assessed.contains(holder.as_str()))
        {
            waits.push(HeldWait {
                holder: holder.clone(),
                waiter: episode.task_id.clone(),
                path: path.clone(),
                started_at: episode.started_at,
                wait_ms: episode.wait_ms(),
                end: episode.end.as_str(),
            });
        }
    }
    PilotEvidence { assessments, waits }
}
