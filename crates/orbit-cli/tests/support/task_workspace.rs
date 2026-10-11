//! Disposable task fixtures. A batch uses one isolated child and the same
//! runtime task writer as the CLI, while assertions still exercise the CLI.

// Each integration binary uses a different subset of this fixture API.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::{TaskComplexity, TaskStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tempfile::{TempDir, tempdir};

const SEED_REQUEST: &str = "ORBIT_TEST_TASK_SEED_REQUEST";

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct TaskSeed {
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) acceptance_criteria: Vec<String>,
    pub(crate) complexity: TaskComplexity,
    pub(crate) status: TaskStatus,
    pub(crate) tags: Vec<String>,
}

impl TaskSeed {
    pub(crate) fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: "Task fixture item.".into(),
            acceptance_criteria: Vec::new(),
            complexity: TaskComplexity::Low,
            status: TaskStatus::Proposed,
            tags: Vec::new(),
        }
    }
}

pub(crate) struct TestWorkspace {
    _temp: TempDir,
    pub(crate) home: PathBuf,
    pub(crate) work: PathBuf,
    template: TaskSeed,
}

impl TestWorkspace {
    pub(crate) fn new(name: &str, template: TaskSeed) -> Self {
        // A fixture command re-executes the calling case only as an entrypoint.
        // Finish its requested batch before any ordinary fixture setup runs.
        if seed_if_requested() {
            std::process::exit(0);
        }
        let temp = tempdir().expect("task fixture");
        let home = temp.path().join("home");
        let work = home.join("work");
        fs::create_dir_all(&home).expect("create fixture home");
        crate::git_repo::init(&work);
        let workspace = Self {
            _temp: temp,
            home,
            work,
            template,
        };
        workspace.run(
            &["workspace", "init", "--name", name],
            "initialize workspace",
        );
        let shown = workspace.run(
            &["workspace", "show", "--format", "json"],
            "verify fixture routing",
        );
        let routing: Value = serde_json::from_slice(&shown.stdout).expect("workspace JSON");
        assert_eq!(routing["registered"], true);
        assert_eq!(
            routing["checkout"]["repo_root"],
            fs::canonicalize(&workspace.work)
                .unwrap()
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(
            routing["checkout"]["orbit_dir"],
            fs::canonicalize(workspace.work.join(".orbit"))
                .unwrap()
                .to_string_lossy()
                .as_ref()
        );
        workspace
    }

    pub(crate) fn seed_tasks(&self, tasks: &[TaskSeed]) -> Vec<Value> {
        // The runtime reads HOME, so it must never run in the parallel parent.
        let request = tempfile::NamedTempFile::new_in(self._temp.path()).expect("seed request");
        let response = request.path().with_extension("response.json");
        fs::write(
            request.path(),
            serde_json::to_vec(&(tasks, &response)).unwrap(),
        )
        .unwrap();
        let test = std::thread::current()
            .name()
            .expect("fixture called by a test")
            .to_owned();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        test_env::clear_inherited_authority(|name| {
            child.env_remove(name);
        });
        child
            .args(["--exact", &test, "--nocapture", "--test-threads=1"])
            .env(SEED_REQUEST, request.path())
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .current_dir(&self.work);
        let logs = tempdir().unwrap();
        let output = test_env::run_child_test(&mut child, &test, logs.path());
        assert!(
            output.status.success(),
            "seed command failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let seeded: Vec<Value> =
            serde_json::from_slice(&fs::read(response).unwrap()).expect("seeded tasks");
        assert_eq!(
            seeded.len(),
            tasks.len(),
            "seed command completed its whole batch"
        );
        seeded
    }

    pub(crate) fn task_seed(&self, title: &str) -> TaskSeed {
        TaskSeed {
            title: title.into(),
            ..self.template.clone()
        }
    }

    pub(crate) fn add_task(&self, title: &str) -> String {
        let [id] = self.add_tasks([title]);
        id
    }

    /// Seed several template tasks in one batch; ids come back in title order.
    pub(crate) fn add_tasks<const N: usize>(&self, titles: [&str; N]) -> [String; N] {
        let seeds: Vec<TaskSeed> = titles.iter().map(|title| self.task_seed(title)).collect();
        self.seed_tasks(&seeds)
            .iter()
            .map(|task| {
                assert_eq!(task["status"], "proposed");
                task["id"].as_str().expect("task id").to_owned()
            })
            .collect::<Vec<_>>()
            .try_into()
            .expect("one id per title")
    }

    pub(crate) fn add_task_with_tags(&self, title: &str, tags: &[&str]) -> Value {
        let mut seed = self.task_seed(title);
        seed.tags = tags.iter().map(|tag| (*tag).into()).collect();
        self.seed_tasks(&[seed]).remove(0)
    }

    pub(crate) fn add_task_cli_with_tags(&self, title: &str, tags: &[&str]) -> Value {
        let mut args = vec![
            "task",
            "add",
            "--title",
            title,
            "--description",
            &self.template.description,
            "--complexity",
            "low",
            "--json",
        ];
        for tag in tags {
            args.extend(["--tag", tag]);
        }
        serde_json::from_slice(&self.run(&args, "add task through CLI").stdout).unwrap()
    }

    pub(crate) fn run(&self, args: &[&str], label: &str) -> Output {
        self.run_with_stdin(args, None, label)
    }

    pub(crate) fn run_with_stdin(&self, args: &[&str], stdin: Option<&str>, label: &str) -> Output {
        let output = run_orbit(&self.work, &self.home, args, stdin, false);
        assert!(
            output.status.success(),
            "{label} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    pub(crate) fn run_raw(&self, args: &[&str]) -> Output {
        run_orbit(&self.work, &self.home, args, None, false)
    }

    /// Run a governed command as an explicit operator [ORB-10453].
    ///
    /// A test binary is not a terminal, so the capability chokepoint resolves
    /// it as an unidentified caller; claiming the capability is the same
    /// deliberate act the denial message asks for.
    pub(crate) fn run_as_operator(&self, args: &[&str], label: &str) -> Output {
        let output = run_orbit(&self.work, &self.home, args, None, true);
        assert!(
            output.status.success(),
            "{label} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    pub(crate) fn run_merged(&self, args: &[&str], label: &str) -> String {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("orbit"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let status = command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .stdout(file.as_file().try_clone().unwrap())
            .stderr(file.as_file().try_clone().unwrap())
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "{label} failed with status {status:?}");
        fs::read_to_string(file.path()).unwrap()
    }
}

pub(crate) fn run_orbit(
    work: &Path,
    home: &Path,
    args: &[&str],
    stdin: Option<&str>,
    operator: bool,
) -> Output {
    let mut command = cargo_bin_cmd!("orbit");
    // ORB-11300: resetting HOME is not isolation. `ORBIT_REGISTRY_ROOT` and
    // `ORBIT_WORKSPACE` outrank it, so a suite launched from inside a managed
    // Orbit run used to add these fixture tasks to the live workspace.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        // Pin the geometry: width resolution is only consulted for a terminal
        // sink, and table assertions must not depend on the runner's terminal.
        .env("COLUMNS", "120")
        .args(args);
    if operator {
        command.env("ORBIT_OPERATOR", "1");
    }
    if let Some(input) = stdin {
        command.write_stdin(input);
    }
    command.output().expect("run fixture CLI")
}

/// Dispatch only the fixture command, not the calling test's assertions.
fn seed_if_requested() -> bool {
    let Some(request) = std::env::var_os(SEED_REQUEST) else {
        return false;
    };
    let (tasks, response): (Vec<TaskSeed>, PathBuf) =
        serde_json::from_slice(&fs::read(request).unwrap()).unwrap();
    let work = std::env::current_dir().unwrap();
    let roots =
        orbit_cmd::registry_runtime::RegisteredRuntimeFactory::resolve_roots_for_cwd(&work, None)
            .unwrap();
    assert_eq!(roots.local_root, work.join(".orbit"));
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    assert!(
        fs::canonicalize(&roots.global_root)
            .unwrap()
            .starts_with(fs::canonicalize(&home).unwrap()),
        "seed registry {:?} must be beneath fixture home {:?}",
        roots.global_root,
        home
    );
    let runtime = orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_resolved_roots(roots)
        .unwrap()
        .with_actor(orbit_core::ActorIdentity::human("fixture"));
    let seeded: Vec<Value> = tasks
        .into_iter()
        .map(|task| {
            serde_json::to_value(
                runtime
                    .add_task(TaskAddParams {
                        title: task.title,
                        description: task.description,
                        acceptance_criteria: task.acceptance_criteria,
                        complexity: task.complexity,
                        status: Some(task.status),
                        tags: task.tags,
                        ..Default::default()
                    })
                    .expect("seed task through runtime"),
            )
            .unwrap()
        })
        .collect();
    fs::write(response, serde_json::to_vec(&seeded).unwrap()).unwrap();
    true
}
