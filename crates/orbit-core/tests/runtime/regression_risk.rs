//! Regression-risk marks at the host boundary the engine builds implement and
//! review envelopes through: a task's `file:` context paths that regressed
//! often enough in the trailing window come back as a `regression_risk`
//! section, and each prompt that carries marks records them on the task for
//! Analytics › Codebase to compare against the window before marks.

use std::path::{Path, PathBuf};

use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::TaskStatus;
use orbit_core::application::codebase::CodebaseQuery;
use orbit_core::application::task::TaskAddParams;
use orbit_engine::RuntimeHost;
use orbit_types::task::{
    RegressionClassification, RegressionEscape, RegressionNature, TaskRelation, TaskRelationType,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const HOT: &str = "src/hot.rs";
const COLD: &str = "src/cold.rs";
const HUB: &str = "src/hub.rs";
const RARE: &str = "src/rare.rs";
const OLD: &str = "src/old.rs";

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    workspace: PathBuf,
    runtime: OrbitRuntime,
    /// The done task whose prompts are built; it touches every seeded path.
    task: String,
    /// Every seeded task ID, which no rendered mark may show.
    ids: Vec<String>,
}

impl Fixture {
    /// Seed, over the default settings (30 days, support 5, 40%, 3 files):
    ///
    /// - `src/cold.rs`: 5 tasks, all regressed by unclassified links;
    /// - `src/hot.rs`: 5 tasks, 3 regressed (two classified links, one not);
    /// - `src/hub.rs`: those 5 and 5 more, the same 3 regressed (below 40%);
    /// - `src/rare.rs`: 3 tasks, all regressed (below support);
    /// - `src/old.rs`: 5 tasks, all regressed, done 60 days ago.
    ///
    /// The prompted task touches all five, so each count includes it.
    fn new() -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("config.toml"), "").unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let mut fx = Self {
            _root: root,
            global,
            workspace,
            runtime,
            task: String::new(),
            ids: Vec::new(),
        };

        let cold: Vec<String> = (0..5)
            .map(|n| fx.done(&format!("cold {n}"), &[COLD]))
            .collect();
        for regressed in &cold {
            fx.fix(&format!("Reopen {regressed}"), regressed, None);
        }
        let hot: Vec<String> = (0..5)
            .map(|n| fx.done(&format!("hot {n}"), &[HOT, HUB]))
            .collect();
        let broke = Some(RegressionClassification {
            nature: RegressionNature::BrokeExisting,
            escape: RegressionEscape::NoTest,
        });
        fx.fix(
            &format!("Restore the cache key in {}", hot[0]),
            &hot[0],
            broke,
        );
        fx.fix(
            &format!("Restore the eviction in {}", hot[1]),
            &hot[1],
            broke,
        );
        // The newest link supplies the lesson, its IDs removed.
        fx.fix(
            &format!(
                "{}: Guard the retry budget in hot.rs [F2026-10-007]",
                hot[2]
            ),
            &hot[2],
            None,
        );
        for n in 0..5 {
            fx.done(&format!("hub {n}"), &[HUB]);
        }
        for n in 0..3 {
            let regressed = fx.done(&format!("rare {n}"), &[RARE]);
            fx.fix(&format!("Reopen {regressed}"), &regressed, None);
        }
        for n in 0..5 {
            let regressed = fx.done(&format!("old {n}"), &[OLD]);
            fx.backdate(&regressed, Duration::days(60));
            fx.fix(&format!("Reopen {regressed}"), &regressed, None);
        }
        fx.task = fx.done("prompted", &[HOT, COLD, HUB, RARE, OLD]);
        fx
    }

    fn add(&mut self, params: TaskAddParams) -> String {
        let id = self.runtime.add_task(params).unwrap().id.to_string();
        self.ids.push(id.clone());
        id
    }

    fn done(&mut self, title: &str, files: &[&str]) -> String {
        self.add(TaskAddParams {
            title: title.into(),
            description: "regression risk fixture".into(),
            context_files: files.iter().map(|file| format!("file:{file}")).collect(),
            status: Some(TaskStatus::Done),
            ..Default::default()
        })
    }

    /// A regression task filed against `regressed`.
    fn fix(
        &mut self,
        title: &str,
        regressed: &str,
        classification: Option<RegressionClassification>,
    ) -> String {
        self.add(TaskAddParams {
            title: title.into(),
            description: "regression risk fixture".into(),
            relations: vec![TaskRelation {
                relation_type: TaskRelationType::RegressionFrom,
                target: regressed.parse().unwrap(),
                regression: classification,
            }],
            ..Default::default()
        })
    }

    /// Move every history row of `task` back by `age`, so it was done then:
    /// the window reads when a task entered done, which has no public writer.
    fn backdate(&self, task: &str, age: Duration) {
        let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
            &orbit_store::maintenance::task_registry::task_registry_path(&self.global),
        )
        .unwrap();
        let events = registry
            .find_task_binding(task)
            .unwrap()
            .expect("fixture task binding")
            .canonical_path
            .join(orbit_types::task::TASK_EVENTS_FILE_NAME);
        let at = (Utc::now() - age).to_rfc3339();
        let rows: String = std::fs::read_to_string(&events)
            .unwrap()
            .lines()
            .map(|line| {
                let mut row: Value = serde_json::from_str(line).unwrap();
                row["at"] = json!(at);
                format!("{row}\n")
            })
            .collect();
        std::fs::write(&events, rows).unwrap();
    }

    /// Rewrite the workspace config and reopen the runtime over the same
    /// roots and store, as the settings are read at open.
    fn reconfigure(&mut self, config: &str) {
        std::fs::write(self.workspace.join("config.toml"), config).unwrap();
        self.runtime = OrbitRuntime::from_roots(&self.global, &self.workspace).unwrap();
    }

    /// The `regression_risk` section `activity` of `run` would carry.
    fn marks(&self, run: &str, activity: &str) -> Option<Value> {
        RuntimeHost::regression_risk_for_agent_input(
            &self.runtime,
            run,
            activity,
            &json!({ "task_id": self.task }),
        )
    }

    fn marked_paths(&self, run: &str, activity: &str) -> Vec<String> {
        self.marks(run, activity)
            .map(|section| {
                section["marks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|mark| mark["path"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `regression_risk_marked` events on the prompted task, as notes.
    fn recorded(&self) -> Vec<Value> {
        self.runtime
            .get_task_history(&self.task)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.event == "regression_risk_marked")
            .map(|entry| {
                assert_eq!(entry.by, "system", "only the host records marks");
                serde_json::from_str(entry.note.as_deref().unwrap()).unwrap()
            })
            .collect()
    }
}

fn qualifying_paths_are_marked_highest_risk_first_and_recorded_once_per_prompt() {
    let fx = Fixture::new();
    let section = fx
        .marks("run-1", "agent_implement")
        .expect("two context paths qualify");
    assert_eq!(section["window_days"], 30);
    assert_eq!(
        section["marks"],
        json!([
            {
                "path": COLD,
                "regressed": "5 of 6 touching tasks",
                "nature": ["unclassified"],
                "escape": ["unclassified"],
                "lesson": "Reopen",
            },
            {
                "path": HOT,
                "regressed": "3 of 6 touching tasks",
                "nature": ["broke_existing", "unclassified"],
                "escape": ["no_test", "unclassified"],
                "lesson": "Guard the retry budget in hot.rs",
            },
        ]),
        "a hub below the threshold, a path below support and a path whose \
         regressions aged out of the window stay unmarked; unrecorded \
         categories read unclassified"
    );
    let rendered = section.to_string();
    for id in &fx.ids {
        assert!(
            !rendered.contains(&format!("{id}\"")) && !rendered.contains(&format!("{id} ")),
            "no internal task ID reaches the prompt: {id} in {rendered}"
        );
    }
    assert!(!rendered.contains("F2026-10-007"), "nor a friction ID");

    let expected = json!({
        "run_id": "run-1",
        "activity": "agent_implement",
        "paths": [COLD, HOT],
        "window_days": 30,
    });
    assert_eq!(fx.recorded(), std::slice::from_ref(&expected));
    fx.marks("run-1", "agent_implement").unwrap();
    assert_eq!(
        fx.recorded(),
        std::slice::from_ref(&expected),
        "rebuilding the same prompt records nothing new"
    );
    let review = fx
        .marks("run-1", "agent_review_repair")
        .expect("the reviewer sees the same marks");
    assert_eq!(review["marks"], section["marks"]);
    assert_eq!(
        fx.recorded().len(),
        2,
        "the review prompt records its own marks"
    );

    let analytics = fx
        .runtime
        .codebase_analytics(&CodebaseQuery::parse(Some("30d"), Some(1), None).unwrap())
        .unwrap();
    let comparison = &analytics["regressions"]["risk_marks"];
    assert_eq!(comparison["marked_paths"], 2);
    assert_eq!(comparison["window_days"], 30);
    let counts = |rate: &Value| json!([rate["numerator"], rate["denominator"]]);
    assert_eq!(
        counts(&comparison["with_marks"]),
        json!([0, 1]),
        "with marks: the one done task a prompt marked"
    );
    assert_eq!(
        counts(&comparison["before"]),
        json!([8, 11]),
        "before: the distinct done tasks that touched a marked path in the \
         window before the first mark"
    );
    let rows: Vec<Value> = comparison["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            json!([
                row["path"],
                counts(&row["with_marks"]),
                counts(&row["before"])
            ])
        })
        .collect();
    assert_eq!(
        rows,
        [json!([COLD, [0, 1], [5, 6]]), json!([HOT, [0, 1], [3, 6]]),]
    );
}

fn support_cap_and_switch_are_configurable_and_off_marks_nothing() {
    let mut fx = Fixture::new();
    fx.reconfigure("[workflow.regression_risk]\nmax_files = 1\n");
    assert_eq!(
        fx.marked_paths("run-cap", "agent_implement"),
        [COLD],
        "the cap keeps the highest-risk path"
    );

    fx.reconfigure("[workflow.regression_risk]\nmin_support = 3\n");
    assert_eq!(
        fx.marked_paths("run-support", "agent_implement"),
        [COLD, RARE, HOT],
        "a lower support admits the rarely touched path"
    );

    fx.reconfigure("[workflow.regression_risk]\nthreshold_percent = 60\n");
    assert_eq!(
        fx.marked_paths("run-threshold", "agent_implement"),
        [COLD],
        "a higher threshold drops 3 of 6"
    );

    fx.reconfigure("[workflow.regression_risk]\nwindow_days = 90\n");
    assert_eq!(
        fx.marked_paths("run-window", "agent_implement"),
        [COLD, OLD, HOT],
        "a longer window brings the older regressions back"
    );

    let recorded = fx.recorded().len();
    fx.reconfigure("[workflow.regression_risk]\nenabled = false\n");
    assert_eq!(
        fx.marks("run-off", "agent_implement"),
        None,
        "with marks off there is no section"
    );
    assert_eq!(fx.recorded().len(), recorded, "and nothing is recorded");
}

fn shipped_implement_and_review_activities_ask_for_marks() {
    for name in [
        "agent_implement",
        "agent_review_repair",
        "review_reconciliation_review",
    ] {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/activities/{name}.yaml"));
        let asset = orbit_engine::load_activity_asset(&std::fs::read_to_string(path).unwrap())
            .unwrap_or_else(|err| panic!("{name} loads: {err}"));
        let orbit_types::workflow::activity_job::ActivityV2Spec::AgentLoop(spec) = asset.spec.spec
        else {
            panic!("{name} must dispatch an agent");
        };
        assert!(
            spec.regression_risk_instruction
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty()),
            "{name}: declaring the instruction is what asks the host for marks"
        );
    }
}

crate::scenario_table!(
    regression_risk_scenarios,
    qualifying_paths_are_marked_highest_risk_first_and_recorded_once_per_prompt,
    support_cap_and_switch_are_configurable_and_off_marks_nothing,
    shipped_implement_and_review_activities_ask_for_marks
);
