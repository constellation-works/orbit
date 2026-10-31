#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! A CLI resume must hand ownership to a worker before its caller exits.

#[cfg(unix)]
use crate::fixture_crew;

#[cfg(unix)]
mod lifecycle;

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use crate::workflow_gate::WorkflowGate;
    use assert_cmd::assert::OutputAssertExt;
    use assert_cmd::cargo::cargo_bin_cmd;
    use orbit_common::test_env;
    use serde_json::{Value, json};
    use tempfile::{TempDir, tempdir};

    use super::fixture_crew;

    /// Run a freshly copied test binary, absorbing the parallel-fork `ETXTBSY`
    /// race described in [`orbit_common::test_process`]. Other errors return
    /// immediately. Non-Linux launches stay direct, matching `update::output_of`.
    fn installed_output(
        command: &mut assert_cmd::Command,
    ) -> std::io::Result<std::process::Output> {
        #[cfg(target_os = "linux")]
        {
            orbit_common::test_process::retry_executable_busy(|| command.output())
        }
        #[cfg(not(target_os = "linux"))]
        {
            command.output()
        }
    }

    pub(super) struct Fixture {
        _temp: TempDir,
        pub(super) gate: WorkflowGate,
        pub(super) home: PathBuf,
        pub(super) repo: PathBuf,
    }

    impl Fixture {
        pub(super) fn new() -> Self {
            if crate::workflow_gate::wait_if_requested() {
                std::process::exit(0);
            }
            let temp = tempdir().expect("temporary fixture");
            let home = temp.path().join("home");
            let repo = temp.path().join("repo");
            fs::create_dir_all(&home).expect("home");
            fs::create_dir_all(&repo).expect("repo");
            crate::git_repo::init(&repo);
            let fixture = Self {
                _temp: temp,
                gate: WorkflowGate::new(),
                home,
                repo,
            };
            // Workspace init materializes the global catalog and registers the
            // fresh owner. Prepare its explicit inputs without seeding twice.
            let root = fixture.home.join(".orbit");
            orbit_common::fs::io::create_private_dir_all(&root).expect("private fixture root");
            orbit_config::seed_default_config(&root.join("config.toml"), None)
                .expect("fixture config");
            orbit_registry::ensure_machine_identity(&root, || {
                Ok(orbit_registry::NewMachineIdentity {
                    name: "resume-test".into(),
                    task_prefix: "RT".into(),
                })
            })
            .expect("fixture machine identity");
            // Freeze the fixture crew registry before init chooses review crews;
            // replacing it afterwards would invalidate the seeded workspace pool.
            fixture_crew::configure_sol(&root);
            fixture
                .command()
                .args(["workspace", "init", "--name", "resume-test"])
                .assert()
                .success();
            let jobs = fixture.home.join(".orbit/resources/jobs");
            fs::create_dir_all(&jobs).expect("job catalog");
            let job = json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "resume_cli_fixture"},
                "spec": {"state": "enabled", "kind": "workflow", "steps": [fixture.gate.step("nap")]}});
            fs::write(jobs.join("resume_cli_fixture.yaml"), job.to_string()).expect("fixture job");
            fixture
        }

        pub(super) fn command(&self) -> assert_cmd::Command {
            let mut command = cargo_bin_cmd!("orbit");
            test_env::clear_inherited_authority(|name| {
                command.env_remove(name);
            });
            command
                .current_dir(&self.repo)
                .env("HOME", &self.home)
                .env("USERPROFILE", &self.home);
            command
        }

        pub(super) fn json(&self, args: &[&str]) -> Value {
            let output = self
                .command()
                .args(args)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone();
            serde_json::from_slice(&output).expect("CLI JSON")
        }

        fn installed_json(&self, program: &Path, args: &[&str]) -> Value {
            let mut command = assert_cmd::Command::new(program);
            test_env::clear_inherited_authority(|name| {
                command.env_remove(name);
            });
            command
                .current_dir(&self.repo)
                .env("HOME", &self.home)
                .env("USERPROFILE", &self.home)
                .args(args);
            // Same bounded launch as `update::output_of`: only ExecutableFileBusy
            // is retried. A started process still has to exit successfully.
            let output = installed_output(&mut command).unwrap_or_else(|error| {
                panic!("Failed to spawn {command:?}: {error}");
            });
            let stdout = output.assert().success().get_output().stdout.clone();
            serde_json::from_slice(&stdout).expect("installed CLI JSON")
        }

        fn interrupted_source(&self) -> String {
            let source = self.json(&[
                "run",
                "job",
                "resume_cli_fixture",
                "--input",
                "crew=sol",
                "--json",
            ]);
            let run_id = source["run_id"].as_str().expect("source run id").to_owned();
            self.gate.wait_ready();
            let running = self.poll_run(&run_id, "running", Duration::from_secs(10));
            let pid = running["run"]["pid"].as_u64().expect("worker pid") as i32;
            assert_ne!(
                pid as u32,
                std::process::id(),
                "source is owned by a worker"
            );
            // The worker is deliberately interrupted after it has claimed the
            // run. Its catalog job can then be resumed through the public CLI.
            assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
            self.gate.release();
            self.poll_run(&run_id, "interrupted", Duration::from_secs(10));
            run_id
        }

        pub(super) fn poll_run(&self, run_id: &str, state: &str, timeout: Duration) -> Value {
            let deadline = Instant::now() + timeout;
            // Workers persist their own success/failed outcomes. Keep running
            // and interrupted reads on the CLI path, including SIGKILL repair.
            let observer = matches!(state, "success" | "failed").then(|| {
                orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_resolved_roots(
                    orbit_core::runtime::OrbitRuntimeRoots {
                        global_root: self.home.join(".orbit"),
                        shared_root: self.repo.join(".orbit"),
                        local_root: self.repo.join(".orbit"),
                    },
                )
                .expect("registered terminal observer")
            });
            loop {
                if let Some(observer) = &observer {
                    let shown = observer
                        .show_job_run_observed(run_id)
                        .expect("observed run");
                    if shown.state.to_string() == state {
                        let shown = self.json(&["run", "show", run_id, "--json"]);
                        assert_eq!(shown["run"]["state"], state);
                        return shown;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "run did not reach {state}: {shown:?}"
                    );
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let shown = self.json(&["run", "show", run_id, "--json"]);
                if shown["run"]["state"] == state {
                    return shown;
                }
                assert!(
                    Instant::now() < deadline,
                    "run did not reach {state}: {shown}"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        fn write_pipeline_job(&self, name: &str) {
            let job = json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": name},
                "spec": {"state": "enabled", "kind": "workflow", "steps": [{"id": "finish",
                    "default_input": {"result": {"run_id": "fixture", "status": "success"}},
                    "spec": {"type": "deterministic", "action": "pipeline_success_guard", "config": {}}}]}});
            fs::write(
                self.home
                    .join(".orbit/resources/jobs")
                    .join(format!("{name}.yaml")),
                job.to_string(),
            )
            .expect("write workflow job");
        }

        fn assert_worker_started(&self, run_id: &str) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let shown = self.json(&["run", "show", run_id, "--json"]);
                if let Some(pid) = shown["run"]["pid"].as_u64() {
                    assert_ne!(pid, std::process::id() as u64, "worker is a child process");
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "worker never started for {run_id}: {shown}"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    /// `orbit web serve` is launched by this same CLI main. Both CLI workflow
    /// entry points must mark the binary before the shared worker spawn path.
    fn ship_and_auto_cli_commands_launch_workers() {
        let fixture = Fixture::new();
        fixture.write_pipeline_job("task_auto_pipeline");
        fixture.write_pipeline_job("workspace_auto_pipeline");
        let installed = fixture.home.join(".orbit/bin/orbit");
        fs::create_dir_all(installed.parent().expect("installation directory"))
            .expect("create installation directory");
        fs::copy(env!("CARGO_BIN_EXE_orbit"), &installed).expect("install tested binary");

        for args in [
            &["run", "ship", "--mode", "local", "--json"][..],
            &["run", "auto", "--auto-concurrency", "enforce", "--json"][..],
        ] {
            let submitted = fixture.installed_json(&installed, args);
            let run_id = submitted["run_id"].as_str().expect("submitted run id");
            if args[1] == "auto" {
                assert!(
                    submitted["note"].is_string(),
                    "a drain with no required validation commands is noted, not refused: \
                     {submitted}"
                );
            }
            if args[1] == "auto" {
                assert_eq!(submitted["auto_concurrency"]["mode"], "enforce");
                let detail = fixture.json(&["run", "show", run_id, "--no-reconcile", "--json"]);
                assert_eq!(detail["run"]["auto_concurrency"]["mode"], "enforce");
            }
            fixture.assert_worker_started(run_id);
            fixture.poll_run(run_id, "success", Duration::from_secs(10));
        }
    }

    /// [ORB-14117] Only the owner approves work, so a replica pull drain
    /// refuses `--approve-proposed` before anything is submitted.
    fn auto_refuses_approve_proposed_with_pull() {
        let fixture = Fixture::new();
        let output = fixture
            .command()
            .args([
                "run",
                "auto",
                "--pull",
                "hm_owner/ws_orbit",
                "--approve-proposed",
            ])
            .assert()
            .code(2)
            .get_output()
            .stderr
            .clone();
        let stderr = String::from_utf8_lossy(&output);
        assert!(
            stderr.contains("--approve-proposed") && stderr.contains("--pull"),
            "the refusal names both flags: {stderr}"
        );
        for args in [
            vec![
                "run",
                "auto",
                "--auto-concurrency",
                "enforce",
                "--pull",
                "hm_owner/ws_orbit",
            ],
            vec!["run", "auto", "--auto-concurrency", "off", "--stop"],
            vec!["run", "auto", "--auto-concurrency", "false"],
        ] {
            fixture.command().args(args).assert().code(2);
        }
        let history = fixture.json(&["run", "history", "--json"]);
        assert_eq!(
            history["runs"].as_array().map(Vec::len),
            Some(0),
            "nothing was submitted: {history}"
        );
    }

    /// [ORB-14174] `--allow-crew` restricts a pull drain: an undefined crew is
    /// refused by name before anything is submitted, and a defined one is
    /// accepted alongside `--pull` and reaches the replica check.
    fn auto_pull_validates_its_crew_restriction_before_submitting() {
        let fixture = Fixture::new();
        let pull = |crews: &str| {
            let output = fixture
                .command()
                .args([
                    "run",
                    "auto",
                    "--pull",
                    "hm_owner/ws_orbit",
                    "--allow-crew",
                    crews,
                ])
                .assert()
                .failure()
                .get_output()
                .clone();
            assert_ne!(output.status.code(), Some(2), "clap accepts the pair");
            String::from_utf8_lossy(&output.stderr).into_owned()
        };

        let unknown = pull("orbit-test-no-such-crew");
        assert!(
            unknown.contains("orbit-test-no-such-crew"),
            "the refusal names the crew: {unknown}"
        );
        let known = pull("sol");
        assert!(
            !known.contains("--allow-crew") && !known.contains("sol"),
            "a defined crew passes validation: {known}"
        );
        let history = fixture.json(&["run", "history", "--json"]);
        assert_eq!(
            history["runs"].as_array().map(Vec::len),
            Some(0),
            "nothing was submitted: {history}"
        );
    }

    /// [ORB-13987] `orbit run auto` still starts a drain whose required
    /// validation cannot use the login shell, and says so at submission.
    fn auto_warns_when_required_validation_cannot_use_the_login_shell() {
        let fixture = Fixture::new();
        fixture.write_pipeline_job("workspace_auto_pipeline");
        let installed = fixture.home.join(".orbit/bin/orbit");
        fs::create_dir_all(installed.parent().expect("installation directory"))
            .expect("create installation directory");
        fs::copy(env!("CARGO_BIN_EXE_orbit"), &installed).expect("install tested binary");
        for (key, value) in [
            (
                "workflow.required_validation_commands",
                r#"["make ci-fast"]"#,
            ),
            ("workflow.validation_env.shell_probe", "off"),
        ] {
            fixture
                .command()
                .args(["config", "set", "--seed-from-global", key, value])
                .assert()
                .success();
        }

        let submitted = fixture.installed_json(&installed, &["run", "auto", "--json"]);

        let warning = submitted["warning"].as_str().unwrap_or_default();
        assert!(
            warning.contains("`workflow.validation_env.shell_probe = off`")
                && warning.contains("source: launcher_fallback"),
            "the submission names the disabled resolution and its fallback: {submitted}"
        );
        assert!(
            submitted["note"].is_null(),
            "configured required validation needs no note: {submitted}"
        );
        let run_id = submitted["run_id"]
            .as_str()
            .expect("the drain still starts");
        fixture.poll_run(run_id, "success", Duration::from_secs(10));
    }

    /// ORB-13854: a real gate must explicitly release its reservation before
    /// reporting a failed child, rather than relying on terminal-run cleanup.
    fn gate_releases_reservation_after_child_failure_or_success() {
        let fixture = Fixture::new();
        fs::write(fixture.repo.join("reserved.rs"), "fixture\n").expect("context file");
        let task = fixture.json(&[
            "task",
            "add",
            "--title",
            "gate release fixture",
            "--description",
            "Exercise reservation release after a child finishes.",
            "--acceptance-criteria",
            "The gate releases its reservation.",
            "--plan",
            "Run the deterministic fixture child.",
            "--complexity",
            "low",
            "--status",
            "backlog",
            "--context",
            "file:reserved.rs",
            "--tag",
            "delivery:gate_release_fixture",
            "--json",
        ]);
        let task_input = format!("task_ids={}", serde_json::json!([task["id"]]));

        for child_status in ["failed", "success"] {
            fs::write(
                fixture
                    .home
                    .join(".orbit/resources/jobs/gate_release_fixture.yaml"),
                format!(
                    r#"schemaVersion: 2
kind: Job
metadata:
  name: gate_release_fixture
spec:
  state: enabled
  kind: workflow
  task_delivery:
    modes: [pr]
  steps:
    - id: fixture_result
      target: activity:pipeline_success_guard
      default_input:
        context: gate release fixture
        result:
          run_id: fixture-result
          status: {child_status}
"#
                ),
            )
            .expect("fixture child job");
            let submitted = fixture.json(&[
                "run",
                "job",
                "task_gate_pipeline",
                "--input",
                &task_input,
                "--json",
            ]);
            let gate_id = submitted["run_id"].as_str().expect("gate run id");
            let gate = fixture.poll_run(gate_id, child_status, Duration::from_secs(30));
            let dispatches = gate["run"]["child_dispatches"]
                .as_array()
                .expect("child dispatches");
            assert_eq!(
                dispatches.len(),
                1,
                "gate must dispatch a real child: {gate:#}"
            );
            let child_id = dispatches[0]["child_run_id"]
                .as_str()
                .expect("child run id");
            fixture.poll_run(child_id, child_status, Duration::from_secs(10));

            let release = fixture.json(&[
                "run",
                "show",
                gate_id,
                "-s",
                "release_reservation",
                "--json",
            ]);
            assert_eq!(
                release["step"]["state"], "success",
                "ORB-13854: release step must run successfully: {gate}"
            );
            assert_eq!(
                release["step_output"]["released"], true,
                "ORB-13854: release must free a granted reservation before terminal cleanup"
            );
            let locks = fixture.json(&["task", "locks", "list", "--json"]);
            assert_eq!(
                locks["total_reservations"], 0,
                "finished gate left a reservation: {locks}"
            );
            if child_status == "failed" {
                let guard = fixture.json(&[
                    "run",
                    "show",
                    gate_id,
                    "-s",
                    "require_child_success",
                    "--json",
                ]);
                assert!(
                    matches!(guard["step"]["state"].as_str(), Some("failed" | "error")),
                    "gate must still report its child's failure: {guard}"
                );
            }
        }
    }

    /// `run show -s` must return the named step's own checkpoint when an
    /// earlier step was skipped by `when:`. The audit trail numbers only
    /// steps that started, so its `step_index` runs one behind the YAML
    /// position checkpoints are keyed by once a step is skipped.
    fn run_show_step_output_survives_an_earlier_when_skipped_step() {
        let fixture = Fixture::new();
        fs::write(
            fixture
                .home
                .join(".orbit/resources/jobs/skipped_step_fixture.yaml"),
            r#"schemaVersion: 2
kind: Job
metadata:
  name: skipped_step_fixture
spec:
  state: enabled
  kind: workflow
  steps:
    - id: first
      target: activity:pipeline_success_guard
      default_input:
        context: first context
        result:
          run_id: first-result
          status: success
    - id: skipped
      when: "{{ steps.first.output.checked_count }} == 99"
      target: activity:pipeline_success_guard
      default_input:
        context: skipped context
        result:
          run_id: skipped-result
          status: success
    - id: third
      target: activity:pipeline_success_guard
      default_input:
        context: third context
        results:
          - run_id: third-result
            status: success
          - run_id: third-result-2
            status: success
"#,
        )
        .expect("fixture job");
        let submitted = fixture.json(&["run", "job", "skipped_step_fixture", "--json"]);
        let run_id = submitted["run_id"].as_str().expect("run id");
        fixture.poll_run(run_id, "success", Duration::from_secs(30));

        let output_of = |step: &str| {
            let shown = fixture.json(&["run", "show", run_id, "-s", step, "--json"]);
            (
                shown["step"]["step_index"].clone(),
                shown["step_output"].clone(),
            )
        };
        let (_, first) = output_of("first");
        let (_, third) = output_of("third");
        assert_eq!(
            first["checked_count"], 1,
            "the step before the skip reads its own output: {first}"
        );
        assert_eq!(
            third["checked_count"], 2,
            "a step after a `when:`-skipped one must read its own output, not the previous step's: {third}"
        );
        assert_ne!(first, third);
    }

    fn resume_returns_while_detached_worker_continues_after_cli_exit() {
        let fixture = Fixture::new();
        let source = fixture.interrupted_source();
        let submitted = fixture.json(&["job", "resume", &source, "--json"]);
        // The CLI has returned while execution is held at an explicit gate.
        // This proves detachment without a five-second timing proxy.
        fixture.gate.wait_ready();
        assert_eq!(submitted["waited"], false);
        let run_id = submitted["run_id"].as_str().expect("resumed run id");
        let running = fixture.poll_run(run_id, "running", Duration::from_secs(10));
        let pid = running["run"]["pid"].as_u64().expect("detached worker pid") as u32;
        assert_ne!(
            pid,
            std::process::id(),
            "CLI/test process cannot own the run"
        );
        assert_eq!(
            unsafe { libc::kill(pid as i32, 0) },
            0,
            "worker survives CLI exit"
        );
        fixture.gate.release();
        fixture.poll_run(run_id, "success", Duration::from_secs(20));
    }

    fn resume_wait_joins_detached_run_and_reports_success() {
        let fixture = Fixture::new();
        let source = fixture.interrupted_source();
        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("orbit"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output_file = tempfile::NamedTempFile::new().unwrap();
        let mut child = crate::child_guard::ChildGuard::new(
            command
                .current_dir(&fixture.repo)
                .env("HOME", &fixture.home)
                .env("USERPROFILE", &fixture.home)
                .args(["job", "resume", &source, "--wait", "--json"])
                .stdout(output_file.as_file().try_clone().unwrap())
                .spawn()
                .unwrap(),
        );
        fixture.gate.wait_ready();
        assert!(
            child.try_wait().unwrap().is_none(),
            "--wait returned before execution"
        );
        fixture.gate.release();
        assert!(child.wait().unwrap().success());
        let waited: Value = serde_json::from_slice(&fs::read(output_file.path()).unwrap()).unwrap();
        assert_eq!(waited["waited"], true);
        assert_eq!(waited["state"], "success");
        let run_id = waited["run_id"].as_str().expect("resumed run id");
        let shown = fixture.json(&["run", "show", run_id, "--json"]);
        assert_eq!(shown["run"]["state"], "success");
        assert_ne!(
            shown["run"]["pid"].as_u64(),
            Some(std::process::id() as u64)
        );
    }

    fn resume_wait_exits_nonzero_for_a_failed_detached_run() {
        let fixture = Fixture::new();
        let source = fixture.interrupted_source();
        let path = fixture
            .home
            .join(".orbit/resources/jobs/resume_cli_fixture.yaml");
        let mut job: Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).expect("fixture job");
        job["spec"]["steps"][0]["spec"]["action"] = json!("missing_action");
        fs::write(&path, job.to_string()).expect("change resumed job definition");

        let output = fixture
            .command()
            .args(["job", "resume", &source, "--wait", "--json"])
            .assert()
            .failure()
            .get_output()
            .stdout
            .clone();
        let waited: Value = serde_json::from_slice(&output).expect("failed wait JSON");
        assert_eq!(waited["waited"], true);
        assert_eq!(waited["state"], "failed");
        let run_id = waited["run_id"].as_str().expect("resumed run id");
        let shown = fixture.json(&["run", "show", run_id, "--json"]);
        assert_eq!(shown["run"]["state"], "failed");
    }

    #[test]
    fn worker_command_contracts_0() {
        if crate::isolated_test::run_in_child(
            "job_resume_detached::unix::worker_command_contracts_0",
        ) {
            return;
        }
        for (name, check) in [
            (
                "ship_and_auto_cli_commands_launch_workers",
                ship_and_auto_cli_commands_launch_workers as fn(),
            ),
            (
                "auto_refuses_approve_proposed_with_pull",
                auto_refuses_approve_proposed_with_pull as fn(),
            ),
            (
                "auto_pull_validates_its_crew_restriction_before_submitting",
                auto_pull_validates_its_crew_restriction_before_submitting as fn(),
            ),
        ] {
            tracing::info!(case = name, "CLI contract row");
            check();
        }
    }

    #[test]
    fn worker_command_contracts_1() {
        if crate::isolated_test::run_in_child(
            "job_resume_detached::unix::worker_command_contracts_1",
        ) {
            return;
        }
        for (name, check) in [
            (
                "auto_warns_when_required_validation_cannot_use_the_login_shell",
                auto_warns_when_required_validation_cannot_use_the_login_shell as fn(),
            ),
            (
                "gate_releases_reservation_after_child_failure_or_success",
                gate_releases_reservation_after_child_failure_or_success as fn(),
            ),
            (
                "run_show_step_output_survives_an_earlier_when_skipped_step",
                run_show_step_output_survives_an_earlier_when_skipped_step as fn(),
            ),
        ] {
            tracing::info!(case = name, "CLI contract row");
            check();
        }
    }

    #[test]
    fn worker_command_contracts_2() {
        // The fixture's real local_shell worker re-enters this identity only
        // to wait at its gate. Dispatch that mode before libtest isolation.
        if crate::workflow_gate::wait_if_requested() {
            std::process::exit(0);
        }
        if crate::isolated_test::run_in_child(
            "job_resume_detached::unix::worker_command_contracts_2",
        ) {
            return;
        }
        for (name, check) in [
            (
                "resume_returns_while_detached_worker_continues_after_cli_exit",
                resume_returns_while_detached_worker_continues_after_cli_exit as fn(),
            ),
            (
                "resume_wait_joins_detached_run_and_reports_success",
                resume_wait_joins_detached_run_and_reports_success as fn(),
            ),
            (
                "resume_wait_exits_nonzero_for_a_failed_detached_run",
                resume_wait_exits_nonzero_for_a_failed_detached_run as fn(),
            ),
        ] {
            tracing::info!(case = name, "CLI contract row");
            check();
        }
    }
}
