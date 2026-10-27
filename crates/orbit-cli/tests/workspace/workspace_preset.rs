#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `orbit workspace init --preset` refuses a plugin-seeded `[enable]` name
//! whose file is missing or does not load before bootstrap, config, registration
//! or provenance. A valid seeded preset still applies and records provenance.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env::{self, FIXTURE_STEP_DEADLINE};
use tempfile::TempDir;

struct Machine {
    _temp: TempDir,
    home: PathBuf,
    repos: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("fixture tempdir");
        let home = temp.path().join("home");
        let repos = temp.path().join("repos");
        fs::create_dir_all(&home).expect("fixture home");
        fs::create_dir_all(&repos).expect("fixture repos");
        let machine = Self {
            _temp: temp,
            home,
            repos,
        };
        let work = machine.repo("machine-init");
        let init = machine.run(
            &work,
            &[
                "init",
                "--non-interactive",
                "--machine-name",
                "preset-host",
                "--task-prefix",
                "PP",
            ],
        );
        assert!(
            init.status.success(),
            "orbit init must create the machine identity: {}",
            output_text(&init)
        );
        machine
    }

    fn repo(&self, name: &str) -> PathBuf {
        let path = self.repos.join(name);
        crate::git_repo::init(&path);
        path
    }

    fn run(&self, work: &Path, args: &[&str]) -> Output {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("ORBIT_SKIP_HOST_PREREQUISITES", "1")
            .env("PATH", work.join("empty-path"))
            .env_remove("RUST_LOG")
            .timeout(FIXTURE_STEP_DEADLINE)
            .args(args)
            .output()
            .expect("run orbit")
    }

    fn registry(&self) -> PathBuf {
        self.home.join(".orbit/workspaces.json")
    }
}

fn output_text(output: &Output) -> String {
    format!(
        "status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn write_manifest(dir: &Path, names: &[&str]) {
    fs::create_dir_all(dir).expect("seeded definition directory");
    let assets = names
        .iter()
        .map(|name| {
            format!(
                "    \"{name}.yaml\": {{ \"plugin\": \"probe\", \"version\": \"1.0.0\", \"digest\": \"sha256:abc\" }}"
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    fs::write(
        dir.join(".orbit-managed-plugin-assets.json"),
        format!("{{\n  \"schemaVersion\": 1,\n  \"assets\": {{\n{assets}\n  }}\n}}\n"),
    )
    .expect("write plugin asset manifest");
}

fn routine_yaml(name: &str) -> String {
    routine_yaml_with_target(name, "task_pilot_pipeline")
}

fn routine_yaml_with_target(name: &str, target_job: &str) -> String {
    format!(
        "schemaVersion: 1\nname: {name}\ndescription: Probe routine.\nenabled: false\ntrigger:\n  cron: \"0 3 * * *\"\ntarget: job:{target_job}\n"
    )
}

fn auto_task_yaml(name: &str) -> String {
    format!(
        "schemaVersion: 1\nname: {name}\ndescription: Probe auto-task.\nenabled: false\nschedule:\n  every_minutes: 60\ndedupe: always\ntemplate:\n  title: Probe preset auto-task\n"
    )
}

fn write_preset(path: &Path, name: &str, routines: &[&str], auto_tasks: &[&str]) {
    let list = |names: &[&str]| {
        names
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    fs::write(
        path,
        format!(
            "name = \"{name}\"\n\n[enable]\nroutines = [{}]\nauto_tasks = [{}]\n",
            list(routines),
            list(auto_tasks)
        ),
    )
    .expect("write preset");
}

fn assert_refused_before_writes(machine: &Machine, work: &Path, preset: &Path, name: &str) {
    let registry = machine.registry();
    let registry_before = fs::read(&registry).unwrap_or_default();
    let refused = machine.run(
        work,
        &[
            "workspace",
            "init",
            "--name",
            "preset-probe",
            "--preset",
            preset.to_str().expect("preset path"),
        ],
    );
    let text = output_text(&refused);
    assert!(
        !refused.status.success(),
        "preset must refuse an unloadable definition: {text}"
    );
    assert!(
        text.contains(name) && text.contains("does not load"),
        "the refusal must name the definition: {text}"
    );
    assert!(
        !work.join(".orbit/config.toml").exists(),
        "a refused preset must not write workspace config: {text}"
    );
    assert!(
        !work.join(".orbit/state/workspace-preset.json").exists(),
        "a refused preset must not record provenance: {text}"
    );
    assert_eq!(
        fs::read(&registry).unwrap_or_default(),
        registry_before,
        "a refused preset must not register the workspace"
    );
}

#[test]
fn preset_refuses_a_missing_or_unloadable_seeded_definition_before_init_writes() {
    let machine = Machine::new();

    let missing_routine = machine.repo("missing-routine");
    write_manifest(&missing_routine.join(".orbit/routines"), &["probe-routine"]);
    let preset = machine.repos.join("missing-routine.toml");
    write_preset(&preset, "missing-routine", &["probe-routine"], &[]);
    assert_refused_before_writes(&machine, &missing_routine, &preset, "probe-routine");

    let missing_task = machine.repo("missing-task");
    write_manifest(&missing_task.join(".orbit/auto_tasks"), &["probe-task"]);
    let preset = machine.repos.join("missing-task.toml");
    write_preset(&preset, "missing-task", &[], &["probe-task"]);
    assert_refused_before_writes(&machine, &missing_task, &preset, "probe-task");

    let bad_routine = machine.repo("bad-routine");
    let routines = bad_routine.join(".orbit/routines");
    write_manifest(&routines, &["probe-routine"]);
    fs::write(routines.join("probe-routine.yaml"), "schemaVersion: [\n").expect("bad routine");
    let preset = machine.repos.join("bad-routine.toml");
    write_preset(&preset, "bad-routine", &["probe-routine"], &[]);
    assert_refused_before_writes(&machine, &bad_routine, &preset, "probe-routine");

    let bad_task = machine.repo("bad-task");
    let tasks = bad_task.join(".orbit/auto_tasks");
    write_manifest(&tasks, &["probe-task"]);
    fs::write(tasks.join("probe-task.yaml"), "schemaVersion: [\n").expect("bad auto-task");
    let preset = machine.repos.join("bad-task.toml");
    write_preset(&preset, "bad-task", &[], &["probe-task"]);
    assert_refused_before_writes(&machine, &bad_task, &preset, "probe-task");
}

#[test]
fn preset_with_one_valid_and_one_missing_definition_enables_neither() {
    let machine = Machine::new();
    let work = machine.repo("mixed");
    let routines = work.join(".orbit/routines");
    write_manifest(&routines, &["probe-routine"]);
    fs::write(
        routines.join("probe-routine.yaml"),
        routine_yaml("probe-routine"),
    )
    .expect("routine");
    write_manifest(&work.join(".orbit/auto_tasks"), &["probe-task"]);
    let preset = machine.repos.join("mixed.toml");
    write_preset(&preset, "mixed-preset", &["probe-routine"], &["probe-task"]);
    assert_refused_before_writes(&machine, &work, &preset, "probe-task");
    let routine = fs::read_to_string(routines.join("probe-routine.yaml")).expect("routine bytes");
    assert!(
        routine.contains("enabled: false"),
        "the valid definition must stay disabled when its preset also names a missing one:\n{routine}"
    );
}

#[test]
fn preset_refuses_routine_with_unresolved_job_target_before_init_writes() {
    let machine = Machine::new();
    let work = machine.repo("unresolved-target");
    let routines = work.join(".orbit/routines");
    let tasks = work.join(".orbit/auto_tasks");
    write_manifest(&routines, &["probe-routine"]);
    write_manifest(&tasks, &["probe-task"]);
    fs::write(
        routines.join("probe-routine.yaml"),
        routine_yaml_with_target("probe-routine", "no_such_review_job"),
    )
    .expect("routine");
    fs::write(tasks.join("probe-task.yaml"), auto_task_yaml("probe-task")).expect("auto-task");
    let preset = machine.repos.join("unresolved-target.toml");
    fs::write(
        &preset,
        "name = \"unresolved-target\"\n\n[seeding]\nroutines = \"none\"\nauto_tasks = \"none\"\n\n[enable]\nroutines = [\"probe-routine\"]\nauto_tasks = [\"probe-task\"]\n\n[config]\n\"review.minutes\" = 33\n",
    )
    .expect("write preset");
    assert_refused_before_writes(&machine, &work, &preset, "probe-routine");
    let routine = fs::read_to_string(routines.join("probe-routine.yaml")).expect("routine bytes");
    let task = fs::read_to_string(tasks.join("probe-task.yaml")).expect("auto-task bytes");
    assert!(
        routine.contains("enabled: false"),
        "the unresolved routine must stay disabled:\n{routine}"
    );
    assert!(
        task.contains("enabled: false"),
        "the valid auto-task in the same preset must stay disabled:\n{task}"
    );
}

#[test]
fn valid_plugin_seeded_preset_applies_and_records_provenance() {
    let machine = Machine::new();
    let work = machine.repo("valid");
    let routines = work.join(".orbit/routines");
    let tasks = work.join(".orbit/auto_tasks");
    write_manifest(&routines, &["probe-routine"]);
    write_manifest(&tasks, &["probe-task"]);
    fs::write(
        routines.join("probe-routine.yaml"),
        routine_yaml("probe-routine"),
    )
    .expect("routine");
    fs::write(tasks.join("probe-task.yaml"), auto_task_yaml("probe-task")).expect("auto-task");
    let preset = machine.repos.join("valid.toml");
    write_preset(&preset, "valid-preset", &["probe-routine"], &["probe-task"]);

    let applied = machine.run(
        &work,
        &[
            "workspace",
            "init",
            "--name",
            "preset-probe",
            "--preset",
            preset.to_str().expect("preset path"),
        ],
    );
    let text = output_text(&applied);
    assert!(
        applied.status.success(),
        "a loadable plugin-seeded preset must apply: {text}"
    );
    let provenance = fs::read_to_string(work.join(".orbit/state/workspace-preset.json"))
        .expect("applied preset provenance");
    assert!(
        provenance.contains("valid-preset"),
        "provenance must name the applied preset:\n{provenance}"
    );
    let routine = fs::read_to_string(routines.join("probe-routine.yaml")).expect("routine bytes");
    let task = fs::read_to_string(tasks.join("probe-task.yaml")).expect("auto-task bytes");
    assert!(
        routine.contains("enabled: true"),
        "the preset must enable the routine:\n{routine}\n{text}"
    );
    assert!(
        task.contains("enabled: true"),
        "the preset must enable the auto-task:\n{task}\n{text}"
    );
}
