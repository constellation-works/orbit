//! Task-pilot footprint grading: how well each pilot crew and model predicted
//! the files a done task's landed commit changed, beside the filer's own
//! selectors as a baseline.
//!
//! A task's pilot selectors are the newest task-pilot receipt's
//! `context_files_after`; the filer's are the oldest receipt's
//! `context_files_before`, what the task declared before any pilot ran. The
//! receipt names its pilot job run (`pilot_run_id`), whose record holds the
//! crew and model the run resolved. A receipt written before that field is
//! matched to the one task-pilot run active when it was written, or among
//! several, the one whose step outputs carry its operation id.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::path_glob::RelativePathGlob;
use orbit_store::contracts::JobRunQuery;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::facts::{Anchor, CodebaseFacts, TaskFacts};
use super::sections::{LARGE_FOOTPRINT_CAP, Params, merge, rate_fields};
use crate::OrbitRuntime;

/// The actor that writes task-pilot receipts.
const PILOT_ACTOR: &str = "task-pilot";
/// The job every task-pilot run (scheduled, drain or CI sweep) belongs to.
const PILOT_JOB: &str = "task_pilot_pipeline";
/// Tasks listed under a row's misses and over-declarations.
const DRILLDOWN_LIMIT: usize = 10;

/// Sources whose CLI help, CLI output and MCP tool goldens are regenerated
/// from: a golden counts as covered when a selector reaches one of them.
const CLI_MCP_SOURCES: &[&str] = &[
    "crates/orbit-cli/src/",
    "crates/orbit-mcp/src/",
    "crates/orbit-tools/src/",
    "crates/orbit-common/src/governance/",
];

/// Where a generated or mirrored output comes from.
enum Source {
    /// The same relative path under another prefix.
    Mirror(&'static str),
    /// Any file under these prefixes.
    Within(&'static [&'static str]),
}

/// Generated or mirrored outputs, by path prefix (a trailing `/`) or exact
/// path. A landed output counts as covered when a selector covers its source.
const DERIVED: &[(&str, Source)] = &[
    (
        "plugin/skills/",
        Source::Mirror("crates/orbit-core/assets/skills/"),
    ),
    (
        "crates/orbit-cli/tests/help_goldens/",
        Source::Within(CLI_MCP_SOURCES),
    ),
    (
        "crates/orbit-cli/tests/output_goldens/",
        Source::Within(CLI_MCP_SOURCES),
    ),
    (
        "crates/orbit-cli/tests/snapshots/",
        Source::Within(CLI_MCP_SOURCES),
    ),
    ("docs/INDEX.md", Source::Within(&["docs/"])),
];

/// The crew and model of the pilot run behind an assessment, as the run's
/// record resolved them; either is `None` when the run recorded none.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PilotRun {
    crew: Option<String>,
    model: Option<String>,
}

/// What a done task's task-pilot receipts say about its footprint.
#[derive(Debug, Clone)]
pub(super) struct PilotFacts {
    /// The pilot's final selectors.
    after: Vec<String>,
    /// The filer's selectors before any pilot ran.
    author: Vec<String>,
    /// The run behind the newest assessment; `None` when no run is found.
    run: Option<PilotRun>,
}

struct RunRecord {
    run_id: String,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    run: PilotRun,
}

/// The workspace's task-pilot runs, read once per scan without their steps.
pub(super) struct PilotRuns {
    runs: Vec<RunRecord>,
}

impl PilotRuns {
    pub(super) fn load(runtime: &OrbitRuntime) -> Self {
        let runs = runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(PILOT_JOB.to_string()),
                include_steps: false,
                ..Default::default()
            })
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "codebase analytics: task-pilot runs unreadable");
                Vec::new()
            })
            .into_iter()
            .map(|run| RunRecord {
                run_id: run.run_id.to_string(),
                started_at: run.started_at,
                finished_at: run.finished_at,
                run: PilotRun {
                    crew: run.resolved_crew,
                    model: run.crew_model,
                },
            })
            .collect();
        Self { runs }
    }

    /// The run a receipt names, else the one task-pilot run active when it
    /// was written, else among several the one whose step outputs carry its
    /// operation id.
    fn attribute(
        &self,
        runtime: &OrbitRuntime,
        run_id: Option<&str>,
        at: DateTime<Utc>,
        operation_id: &str,
        now: DateTime<Utc>,
    ) -> Option<PilotRun> {
        if let Some(run_id) = run_id {
            return self
                .runs
                .iter()
                .find(|record| record.run_id == run_id)
                .map(|record| record.run.clone());
        }
        let active: Vec<&RunRecord> = self
            .runs
            .iter()
            .filter(|record| {
                record.started_at.is_some_and(|started| started <= at)
                    && record.finished_at.unwrap_or(now) >= at
            })
            .collect();
        let found: Vec<&RunRecord> = if active.len() > 1 {
            active
                .into_iter()
                .filter(|record| {
                    runtime
                        .read_run_state(&record.run_id)
                        .ok()
                        .flatten()
                        .and_then(|state| serde_json::to_string(&state.step_outputs).ok())
                        .is_some_and(|outputs| outputs.contains(operation_id))
                })
                .collect()
        } else {
            active
        };
        match found.as_slice() {
            [record] => Some(record.run.clone()),
            _ => None,
        }
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
    let comments = runtime.get_task_comments(id)?;
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

/// Attribute a task's receipts to the pilot run behind its newest
/// assessment. Runs are read at every scan, not stored with the task: a run
/// still active when the receipt was written may finish later.
pub(super) fn attribute(
    runtime: &OrbitRuntime,
    runs: &PilotRuns,
    inputs: PilotInputs,
    now: DateTime<Utc>,
) -> PilotFacts {
    let run = runs.attribute(
        runtime,
        inputs.run_id.as_deref(),
        inputs.at,
        &inputs.operation_id,
        now,
    );
    PilotFacts {
        after: inputs.after,
        author: inputs.author,
        run,
    }
}

/// One declared selector that can cover files.
struct Declared<'a> {
    raw: &'a str,
    anchor: Anchor,
    /// Set for a selector whose path holds a wildcard.
    glob: Option<RelativePathGlob>,
}

/// The path selectors among `raw`, one per anchor. Module and command
/// selectors name no files and are left out.
fn declared(raw: &[String]) -> Vec<Declared<'_>> {
    let mut seen = BTreeSet::new();
    raw.iter()
        .filter_map(|raw| {
            let anchor = Anchor::parse(raw)?;
            if !seen.insert(anchor.clone()) {
                return None;
            }
            let glob = anchor
                .path
                .contains(['*', '?'])
                .then(|| {
                    let pattern = if anchor.dir {
                        format!("{}/**", anchor.path)
                    } else {
                        anchor.path.clone()
                    };
                    RelativePathGlob::new(&pattern).ok()
                })
                .flatten();
            Some(Declared { raw, anchor, glob })
        })
        .collect()
}

impl Declared<'_> {
    /// A file selector covers its file, a directory selector the files
    /// beneath it, and a wildcard selector the files its pattern matches.
    fn covers(&self, path: &str) -> bool {
        if let Some(glob) = &self.glob {
            return glob.matches(path);
        }
        self.anchor.path == path
            || (self.anchor.dir
                && path
                    .strip_prefix(&self.anchor.path)
                    .is_some_and(|rest| rest.starts_with('/')))
    }

    /// Whether the selector reaches any file under `prefix`.
    fn reaches(&self, prefix: &str) -> bool {
        self.anchor.path.starts_with(prefix)
            || (self.anchor.dir && prefix.starts_with(&format!("{}/", self.anchor.path)))
    }

    /// [`Self::covers`], or covers the source a generated output comes from.
    fn covers_landed(&self, path: &str) -> bool {
        self.covers(path)
            || DERIVED.iter().any(|(output, source)| {
                let rest = if output.ends_with('/') {
                    path.strip_prefix(output)
                } else {
                    (path == *output).then_some("")
                };
                rest.is_some_and(|rest| match source {
                    Source::Mirror(prefix) => self.covers(&format!("{prefix}{rest}")),
                    Source::Within(prefixes) => prefixes.iter().any(|prefix| self.reaches(prefix)),
                })
            })
    }
}

/// Sums behind recall and precision over one or more tasks.
#[derive(Default)]
struct Score {
    /// Landed files some selector covered.
    covered: usize,
    landed: usize,
    /// Selectors that covered at least one landed file.
    useful: usize,
    declared: usize,
}

impl Score {
    fn add(&mut self, other: &Score) {
        self.covered += other.covered;
        self.landed += other.landed;
        self.useful += other.useful;
        self.declared += other.declared;
    }

    fn recall(&self) -> f64 {
        ratio(self.covered, self.landed)
    }

    fn precision(&self) -> f64 {
        ratio(self.useful, self.declared)
    }

    /// The harmonic mean of precision and recall; 0 when both are.
    fn f1(&self) -> f64 {
        let (precision, recall) = (self.precision(), self.recall());
        if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        }
    }

    fn json(&self) -> Value {
        json!({
            "recall": rate_fields(self.covered, self.landed),
            "precision": rate_fields(self.useful, self.declared),
            "f1": self.f1(),
        })
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// Grade `selectors` against a landed diff: the score, the landed files no
/// selector covered and the selectors that covered none.
fn grade<'a>(
    selectors: &'a [String],
    landed: &BTreeSet<String>,
) -> (Score, Vec<String>, Vec<&'a str>) {
    let declared = declared(selectors);
    let mut useful = vec![false; declared.len()];
    let mut missed = Vec::new();
    for path in landed {
        let mut covered = false;
        for (index, selector) in declared.iter().enumerate() {
            if selector.covers_landed(path) {
                useful[index] = true;
                covered = true;
            }
        }
        if !covered {
            missed.push(path.clone());
        }
    }
    let unused = declared
        .iter()
        .zip(&useful)
        .filter(|(_, useful)| !**useful)
        .map(|(selector, _)| selector.raw)
        .collect();
    let score = Score {
        covered: landed.len() - missed.len(),
        landed: landed.len(),
        useful: useful.iter().filter(|useful| **useful).count(),
        declared: declared.len(),
    };
    (score, missed, unused)
}

struct Graded<'a> {
    task: &'a TaskFacts,
    missed: Vec<String>,
    unused: Vec<&'a str>,
}

#[derive(Default)]
struct PilotRow<'a> {
    pilot: Score,
    author: Score,
    widenings: usize,
    tasks: Vec<Graded<'a>>,
}

/// The task-pilot ranking: per pilot crew and model, the recall, precision
/// and F1 of the pilot's final selectors against each assessed task's landed
/// diff, the same for the filer's selectors, and mid-run widenings. Rows at
/// or above the minimum support (assessed tasks) rank by F1, then support.
pub(super) fn task_pilot(facts: &CodebaseFacts, params: &Params) -> Value {
    let mut rows: BTreeMap<PilotRun, PilotRow> = BTreeMap::new();
    let mut excluded_large = 0;
    let mut excluded_no_diff = 0;
    let mut without_assessment = 0;
    let mut unattributed = 0;
    for task in &facts.tasks {
        let Some(landed) = &task.landed else {
            continue;
        };
        if !params.within(landed.at) {
            continue;
        }
        if task.no_diff {
            excluded_no_diff += 1;
            continue;
        }
        if landed.paths.len() > LARGE_FOOTPRINT_CAP {
            excluded_large += 1;
            continue;
        }
        let Some(pilot) = &task.pilot else {
            without_assessment += 1;
            continue;
        };
        let Some(run) = &pilot.run else {
            unattributed += 1;
            continue;
        };
        let row = rows.entry(run.clone()).or_default();
        let (score, missed, unused) = grade(&pilot.after, &landed.paths);
        row.pilot.add(&score);
        row.author.add(&grade(&pilot.author, &landed.paths).0);
        row.widenings += task.widenings.len();
        row.tasks.push(Graded {
            task,
            missed,
            unused,
        });
    }
    let assessed: usize = rows.values().map(|row| row.tasks.len()).sum();
    let mut below_support = 0;
    let mut ranked: Vec<(PilotRun, PilotRow)> = rows
        .into_iter()
        .filter(|(_, row)| {
            let supported = row.tasks.len() >= params.min_support;
            below_support += usize::from(!supported);
            supported
        })
        .collect();
    ranked.sort_by(|(left_run, left), (right_run, right)| {
        right
            .pilot
            .f1()
            .total_cmp(&left.pilot.f1())
            .then_with(|| right.tasks.len().cmp(&left.tasks.len()))
            .then_with(|| left_run.cmp(right_run))
    });
    let rows: Vec<Value> = ranked
        .into_iter()
        .map(|(run, row)| row_json(run, row))
        .collect();
    json!({
        "rows": rows,
        "below_support": below_support,
        "assessed": assessed,
        "unattributed": unattributed,
        "without_assessment": without_assessment,
        "excluded_large": excluded_large,
        "excluded_no_diff": excluded_no_diff,
        "large_footprint_cap": LARGE_FOOTPRINT_CAP,
        "drilldown_limit": DRILLDOWN_LIMIT,
    })
}

fn row_json(run: PilotRun, mut row: PilotRow) -> Value {
    let support = row.tasks.len();
    let task = |task: &TaskFacts| json!({ "task_id": task.id, "title": task.title });
    row.tasks.sort_by(|left, right| {
        right
            .missed
            .len()
            .cmp(&left.missed.len())
            .then_with(|| left.task.id.cmp(&right.task.id))
    });
    let misses: Vec<Value> = row
        .tasks
        .iter()
        .filter(|graded| !graded.missed.is_empty())
        .take(DRILLDOWN_LIMIT)
        .map(|graded| {
            let mut value = task(graded.task);
            value["paths"] = json!(graded.missed);
            value
        })
        .collect();
    row.tasks.sort_by(|left, right| {
        right
            .unused
            .len()
            .cmp(&left.unused.len())
            .then_with(|| left.task.id.cmp(&right.task.id))
    });
    let over_declarations: Vec<Value> = row
        .tasks
        .iter()
        .filter(|graded| !graded.unused.is_empty())
        .take(DRILLDOWN_LIMIT)
        .map(|graded| {
            let mut value = task(graded.task);
            value["selectors"] = json!(graded.unused);
            value
        })
        .collect();
    let lift = json!({
        "recall": row.pilot.recall() - row.author.recall(),
        "precision": row.pilot.precision() - row.author.precision(),
        "f1": row.pilot.f1() - row.author.f1(),
    });
    let extra = json!({
        "crew": run.crew,
        "model": run.model,
        "support": support,
        "author": row.author.json(),
        "lift": lift,
        "widenings": row.widenings,
        "widenings_per_task": ratio(row.widenings, support),
        "missed_files": row.pilot.landed - row.pilot.covered,
        "unused_selectors": row.pilot.declared - row.pilot.useful,
        "misses": misses,
        "over_declarations": over_declarations,
    });
    merge(row.pilot.json(), extra)
}
