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

/// Install and enable a plugin that ships job `digest` and a disabled routine
/// `digest` targeting it, the shape a distribution's own plugin has.
fn install_job_plugin(machine: &Machine, work: &Path, namespace: &str) {
    install_job_plugin_impl(machine, work, namespace, false);
}

/// Like [`install_job_plugin`], but the plugin declares a config schema
/// admitting only `mode = "good"`, with that value as its manifest default.
fn install_schema_job_plugin(machine: &Machine, work: &Path, namespace: &str) {
    install_job_plugin_impl(machine, work, namespace, true);
}

fn install_job_plugin_impl(machine: &Machine, work: &Path, namespace: &str, with_schema: bool) {
    let root = machine
        .home
        .join(format!("plugin-sources/{namespace}/.orbit-plugin"));
    let definitions = [
        (
            "activities",
            "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: digest\nspec:\n  type: deterministic\n  description: Fixture activity.\n  action: sleep\n  config: {}\n",
        ),
        (
            "jobs",
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: digest\nspec:\n  state: enabled\n  steps:\n    - id: digest\n      target: activity:digest\n",
        ),
        (
            "routines",
            "schemaVersion: 1\nname: digest\ndescription: Fixture routine.\nenabled: false\ntrigger:\n  cron: '0 3 * * *'\ntarget: job:digest\n",
        ),
    ];
    fs::create_dir_all(root.join("bin")).expect("plugin bin directory");
    let backend = root.join("bin/backend.sh");
    fs::write(
        &backend,
        "#!/bin/sh\nprintf '{\"ok\":true,\"output\":{}}\\n'\n",
    )
    .expect("plugin backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&backend, fs::Permissions::from_mode(0o755)).expect("chmod backend");
    }
    if with_schema {
        fs::write(
            root.join("config.schema.json"),
            "{\"type\":\"object\",\"properties\":{\"mode\":{\"type\":\"string\",\"enum\":[\"good\"]}},\"additionalProperties\":false}\n",
        )
        .expect("plugin config schema");
    }
    let mut manifest = format!(
        "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {namespace}\n  version: 0.1.0\n  description: Preset fixture plugin.\nspec:\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: ping\n      description: Answer.\n      execution_kind: read_only\n      input_schema:\n        type: object\n  definitions:\n"
    );
    for (kind, body) in definitions {
        fs::create_dir_all(root.join(kind)).expect("plugin definition directory");
        fs::write(root.join(kind).join("digest.yaml"), body).expect("plugin definition");
        manifest.push_str(&format!("    {kind}: [{kind}/*.yaml]\n"));
    }
    if with_schema {
        manifest.push_str(
            "  config:\n    schema: config.schema.json\n    defaults:\n      mode: good\n",
        );
    }
    fs::write(root.join("plugin.yaml"), manifest).expect("plugin manifest");
    let added = machine.run(
        work,
        &[
            "plugin",
            "add",
            root.to_str().expect("plugin source"),
            "--enable",
        ],
    );
    assert!(
        added.status.success(),
        "the fixture plugin must install enabled: {}",
        output_text(&added)
    );
}

/// ORB-19267: a routine an enabled plugin seeded, whose target job only that
/// plugin supplies, passes the pre-write check and ends enabled. The same
/// routine is still refused before any write while its plugin is disabled.
#[test]
fn preset_enables_routine_whose_target_job_an_enabled_plugin_supplies() {
    let machine = Machine::new();
    let work = machine.repo("plugin-target");
    let init = machine.run(&work, &["workspace", "init", "--name", "plugin-target"]);
    assert!(
        init.status.success(),
        "the workspace must initialize: {}",
        output_text(&init)
    );
    install_job_plugin(&machine, &work, "digestor");
    let seeded = work.join(".orbit/routines/digestor-digest.yaml");
    let routine = fs::read_to_string(&seeded).expect("the plugin seeds its routine");
    assert!(
        routine.contains("target: job:digest") && routine.contains("enabled: false"),
        "the seeded routine targets the plugin's job, switched off:\n{routine}"
    );
    let preset = machine.repos.join("plugin-target.toml");
    write_preset(&preset, "plugin-target", &["digestor-digest"], &[]);
    let init_with_preset = |machine: &Machine| {
        machine.run(
            &work,
            &[
                "workspace",
                "init",
                "--name",
                "plugin-target",
                "--force",
                "--preset",
                preset.to_str().expect("preset path"),
            ],
        )
    };

    let disabled = machine.run(&work, &["plugin", "disable", "digestor"]);
    assert!(disabled.status.success(), "{}", output_text(&disabled));
    let refused = init_with_preset(&machine);
    let text = output_text(&refused);
    assert!(
        !refused.status.success()
            && text.contains("digestor-digest")
            && text.contains("job:digest"),
        "a target only a disabled plugin supplies must refuse, naming routine and target: {text}"
    );
    assert!(
        !work.join(".orbit/state/workspace-preset.json").exists(),
        "a refused preset must not record provenance: {text}"
    );

    let enabled = machine.run(&work, &["plugin", "enable", "digestor"]);
    assert!(enabled.status.success(), "{}", output_text(&enabled));
    let applied = init_with_preset(&machine);
    let text = output_text(&applied);
    assert!(
        applied.status.success(),
        "a routine targeting an enabled plugin's job must pass the preset check: {text}"
    );
    let routine = fs::read_to_string(&seeded).expect("routine bytes");
    assert!(
        routine.contains("enabled: true"),
        "the preset must enable the plugin-seeded routine:\n{routine}\n{text}"
    );
    let provenance = fs::read_to_string(work.join(".orbit/state/workspace-preset.json"))
        .expect("applied preset provenance");
    assert!(
        provenance.contains("plugin-target"),
        "provenance must name the applied preset:\n{provenance}"
    );
}

fn write_plugin_config_preset(path: &Path, name: &str, namespace: &str, mode: &str) {
    fs::write(
        path,
        format!(
            "name = \"{name}\"\n\n[enable]\nroutines = [\"{namespace}-digest\"]\n\n[config.plugins.{namespace}]\nmode = \"{mode}\"\n"
        ),
    )
    .expect("write preset");
}

/// ORB-19283: a preset setting a plugin value its schema rejects must be
/// refused against the staged config, before any write, naming the setting.
#[test]
fn preset_refuses_schema_invalid_plugin_config_before_init_writes() {
    let machine = Machine::new();
    let work = machine.repo("plugin-schema-bad");
    let init = machine.run(&work, &["workspace", "init", "--name", "plugin-schema-bad"]);
    assert!(init.status.success(), "{}", output_text(&init));
    install_schema_job_plugin(&machine, &work, "delta");
    let seeded = work.join(".orbit/routines/delta-digest.yaml");
    let routine_before = fs::read_to_string(&seeded).expect("seeded routine");
    assert!(
        routine_before.contains("enabled: false"),
        "seeded routine starts disabled:\n{routine_before}"
    );
    let config_before = fs::read(work.join(".orbit/config.toml")).expect("workspace config");
    let registry_before = fs::read(machine.registry()).unwrap_or_default();
    let preset = machine.repos.join("plugin-schema-bad.toml");
    write_plugin_config_preset(&preset, "plugin-schema-bad", "delta", "bad");

    let refused = machine.run(
        &work,
        &[
            "workspace",
            "init",
            "--name",
            "plugin-schema-bad",
            "--force",
            "--preset",
            preset.to_str().expect("preset path"),
        ],
    );
    let text = output_text(&refused);
    assert!(
        !refused.status.success() && text.contains("plugins.delta.mode"),
        "a schema-invalid staged plugin setting must refuse, naming it: {text}"
    );
    assert_eq!(
        fs::read(work.join(".orbit/config.toml")).expect("workspace config after"),
        config_before,
        "a refused preset must not change workspace config"
    );
    assert_eq!(
        fs::read(machine.registry()).unwrap_or_default(),
        registry_before,
        "a refused preset must not change registration"
    );
    assert_eq!(
        fs::read_to_string(&seeded).expect("routine after"),
        routine_before,
        "a refused preset must not change the seeded routine"
    );
    assert!(
        !work.join(".orbit/state/workspace-preset.json").exists(),
        "a refused preset must not record provenance: {text}"
    );
}

/// ORB-19283: one valid preset repairs a schema-invalid plugin value already
/// on disk and enables that plugin's seeded routine in the same application.
#[test]
fn preset_repairs_plugin_config_and_enables_routine_in_one_application() {
    let machine = Machine::new();
    let work = machine.repo("plugin-schema-repair");
    let init = machine.run(
        &work,
        &["workspace", "init", "--name", "plugin-schema-repair"],
    );
    assert!(init.status.success(), "{}", output_text(&init));
    install_schema_job_plugin(&machine, &work, "delta");
    let config_path = work.join(".orbit/config.toml");
    let mut config = fs::read_to_string(&config_path).expect("workspace config");
    config.push_str("\n[plugins.delta]\nmode = \"bad\"\n");
    fs::write(&config_path, config).expect("write invalid plugin config");
    let preset = machine.repos.join("plugin-schema-repair.toml");
    write_plugin_config_preset(&preset, "plugin-schema-repair", "delta", "good");

    let applied = machine.run(
        &work,
        &[
            "workspace",
            "init",
            "--name",
            "plugin-schema-repair",
            "--force",
            "--preset",
            preset.to_str().expect("preset path"),
        ],
    );
    let text = output_text(&applied);
    assert!(
        applied.status.success(),
        "a valid preset must repair the plugin config and enable its routine: {text}"
    );
    let config = fs::read_to_string(&config_path).expect("workspace config after");
    assert!(
        config.contains("good") && !config.contains("bad"),
        "the repaired config must hold the accepted value:\n{config}"
    );
    let routine =
        fs::read_to_string(work.join(".orbit/routines/delta-digest.yaml")).expect("seeded routine");
    assert!(
        routine.contains("enabled: true"),
        "the preset must enable the plugin-seeded routine:\n{routine}\n{text}"
    );
    let provenance = fs::read_to_string(work.join(".orbit/state/workspace-preset.json"))
        .expect("applied preset provenance");
    assert!(
        provenance.contains("plugin-schema-repair"),
        "provenance must name the applied preset:\n{provenance}"
    );
}
