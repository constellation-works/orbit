//! Plain `orbit experiment readout` text when a per-protocol primary table
//! is present.
//!
//! The caption that introduces that table must not call it
//! intention-to-treat. Intention-to-treat metric tables follow under their
//! own headings.

use std::path::PathBuf;
use std::process::Output;

use orbit_core::OrbitRuntime;
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::{TaskComplexity, TaskStatus};
use serde_json::json;

const TEST: &str = "experiment_readout::per_protocol_table_is_not_introduced_as_intention_to_treat";

#[test]
fn per_protocol_table_is_not_introduced_as_intention_to_treat() {
    if crate::isolated_test::run_in_child(TEST) {
        return;
    }
    let fixture = Fixture::workspace();
    let text = fixture.plain_readout();
    let intro = line_containing(&text, "per-protocol (delivered runs only)");
    assert!(
        !intro.to_ascii_lowercase().contains("intention-to-treat"),
        "per-protocol table introduced as intention-to-treat:\n{intro}\n\n{text}"
    );
    let caption_at = text
        .find("per-protocol (delivered runs only)")
        .expect("per-protocol caption");
    let after_caption = &text[caption_at..];
    let per_protocol_header = after_caption
        .lines()
        .find(|line| line.starts_with("ARM\t"))
        .unwrap_or_else(|| panic!("per-protocol table missing after its caption:\n{text}"));
    assert!(
        per_protocol_header.contains("MEAN [95%]") && !per_protocol_header.contains("OPEN"),
        "per-protocol caption does not introduce the per-protocol table:\n{per_protocol_header}\n\n{text}"
    );
    let itt_at = text
        .find("delivery_done_rate (rate, higher is better, rate):")
        .unwrap_or_else(|| panic!("intention-to-treat metric heading missing:\n{text}"));
    assert!(
        caption_at < itt_at,
        "per-protocol table should precede the intention-to-treat metric table:\n{text}"
    );
    let itt_header = text[itt_at..]
        .lines()
        .find(|line| line.starts_with("ARM\t"))
        .unwrap_or_else(|| panic!("intention-to-treat metric table missing:\n{text}"));
    assert!(
        itt_header.contains("OPEN") && itt_header.contains("RATE [95% WILSON]"),
        "intention-to-treat metric table was not rendered:\n{itt_header}\n\n{text}"
    );
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn workspace() -> Self {
        let temp = tempfile::tempdir().expect("fixture tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("fixture home");
        crate::git_repo::init(&work);
        let fixture = Self {
            _temp: temp,
            home,
            work,
        };
        let init = fixture.run(&["workspace", "init", "--name", "readout-caption"]);
        assert!(
            init.status.success(),
            "workspace init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&init.stdout),
            String::from_utf8_lossy(&init.stderr)
        );
        let config = fixture.work.join(".orbit/config.toml");
        let mut existing = std::fs::read_to_string(&config).unwrap_or_default();
        if !existing.is_empty() && !existing.ends_with('\n') {
            existing.push('\n');
        }
        existing.push_str("[experiments]\nenabled = true\n");
        std::fs::write(&config, existing).expect("enable experiments");
        fixture.seed_delivered_and_assigned();
        fixture
    }

    /// A running experiment with a checked treatment arm, and one archived
    /// unforced assignment, so the readout has a per-protocol table and a
    /// settled intention-to-treat metric.
    fn seed_delivered_and_assigned(&self) {
        let runtime =
            OrbitRuntime::from_roots(&self.home.join(".orbit"), &self.work.join(".orbit"))
                .expect("fixture runtime");
        let actor = ExperimentActor {
            actor: "readout-caption".into(),
            reason: None,
        };
        let created = runtime
            .create_experiment(
                "caption-check",
                ExperimentDefinitionInput {
                    arms: Some(json!([
                        {"name": "control", "control": true, "weight": 1},
                        {
                            "name": "treatment",
                            "weight": 1,
                            "overlay": {"instructions": {"implement": false}}
                        }
                    ])),
                    primary_metric: Some("delivery_done_rate".into()),
                    min_sample: Some(1),
                    max_days: Some(30),
                    ..ExperimentDefinitionInput::default()
                },
                actor.clone(),
            )
            .expect("create experiment");
        let started = runtime
            .change_experiment("caption-check", ExperimentChange::Start, actor)
            .expect("start experiment");
        assert_eq!(started.status, "running");
        let task = runtime
            .add_task(TaskAddParams {
                title: "Assigned readout sample".into(),
                description: "Settled so the intention-to-treat table renders.".into(),
                acceptance_criteria: vec!["The readout renders both views.".into()],
                complexity: TaskComplexity::Low,
                status: Some(TaskStatus::Backlog),
                ..TaskAddParams::default()
            })
            .expect("add task");
        runtime.archive_task(&task.id).expect("archive task");
        let workspace_id = runtime.workspace_id().expect("workspace id");
        let digest = started
            .arms
            .iter()
            .find(|arm| arm.name == "control")
            .expect("control arm")
            .overlay_digest
            .clone();
        let segment = started.segment;
        let experiment_id = created.id;
        drop(runtime);
        let db = rusqlite::Connection::open(self.home.join(".orbit/orbit.db"))
            .expect("open fixture store");
        db.execute(
            "INSERT INTO experiment_assignments (
                workspace_id, experiment_id, task_id, arm, segment, overlay_digest,
                assigned_at, forced, exclusion_reason
             ) VALUES (?1, ?2, ?3, 'control', ?4, ?5, ?6, 0, NULL)",
            rusqlite::params![
                workspace_id,
                experiment_id,
                task.id,
                segment,
                digest,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .expect("record unforced assignment");
    }

    fn plain_readout(&self) -> String {
        let output = self.run(&[
            "experiment",
            "readout",
            "caption-check",
            "--format",
            "plain",
            "--header",
        ]);
        assert!(
            output.status.success(),
            "experiment readout failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("readout is utf-8")
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = assert_cmd::cargo::cargo_bin_cmd!("orbit");
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("ORBIT_OPERATOR", "1")
            .env_remove("ORBIT_FORMAT")
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .args(args)
            .output()
            .expect("run orbit")
    }
}

fn line_containing(text: &str, needle: &str) -> String {
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("missing {needle}:\n{text}"))
        .to_string()
}

#[test]
fn step_local_primary_is_refused_and_legacy_readout_is_exploratory() {
    if crate::isolated_test::run_in_child(
        "experiment_readout::step_local_primary_is_refused_and_legacy_readout_is_exploratory",
    ) {
        return;
    }
    let fixture = Fixture::workspace();
    for args in [
        vec![
            "experiment",
            "create",
            "bad-cli",
            "--primary-metric",
            "implement_duration",
        ],
        vec![
            "tool",
            "run",
            "orbit.experiment.create",
            "--input",
            r#"{"name":"bad-tool","primary_metric":"implement_duration"}"#,
        ],
    ] {
        let output = fixture.run(&args);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("step-local")
                && error.contains("cost_usd")
                && error.contains("lead_time"),
            "{error}"
        );
    }
    for metric in ["cost_usd", "lead_time"] {
        let output = fixture.run(&["experiment", "create", metric, "--primary-metric", metric]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let db = rusqlite::Connection::open(fixture.home.join(".orbit/orbit.db")).unwrap();
    db.execute(
        "UPDATE experiments SET primary_metric = 'implement_duration' WHERE name = 'caption-check'",
        [],
    )
    .unwrap();
    drop(db);
    let output = fixture.run(&["experiment", "readout", "caption-check", "--format", "json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let readout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(readout["verdict"]["state"], "withheld");
    assert!(
        readout["verdict"]["reasons"]
            .to_string()
            .contains("primary metric `implement_duration` is step-local")
    );
    let metrics = readout["metrics"].as_array().unwrap();
    for name in [
        "implement_tokens",
        "implement_duration",
        "turns_before_first_edit",
        "result_bytes_before_first_edit",
    ] {
        let metric = metrics
            .iter()
            .find(|metric| metric["metric"] == name)
            .unwrap();
        assert_eq!(metric["exploratory"], true);
        assert!(!metric["arms"].as_array().unwrap().is_empty());
    }
    assert!(
        fixture
            .plain_readout()
            .contains("implement_duration (exploratory)")
    );
}
