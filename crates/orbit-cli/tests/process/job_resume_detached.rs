#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! A CLI resume must hand ownership to a worker before its caller exits.

#[cfg(unix)]
use crate::fixture_crew;

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use assert_cmd::assert::OutputAssertExt;
    use assert_cmd::cargo::cargo_bin_cmd;
    use orbit_common::test_env;
    use serde_json::Value;
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

    struct Fixture {
        _temp: TempDir,
        home: PathBuf,
        repo: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempdir().expect("temporary fixture");
            let home = temp.path().join("home");
            let repo = temp.path().join("repo");
            fs::create_dir_all(&home).expect("home");
            fs::create_dir_all(&repo).expect("repo");
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&repo)
                .status()
                .expect("git init");
            let fixture = Self {
                _temp: temp,
                home,
                repo,
            };
            fixture
                .command()
                .args([
                    "init",
                    "--non-interactive",
                    "--machine-name",
                    "resume-test",
                    "--task-prefix",
                    "RT",
                ])
                .assert()
                .success();
            fixture
                .command()
                .args(["workspace", "init", "--name", "resume-test"])
                .assert()
                .success();
            fixture_crew::configure_sol(&fixture.home.join(".orbit"));
            let jobs = fixture.home.join(".orbit/resources/jobs");
            fs::create_dir_all(&jobs).expect("job catalog");
            fs::write(
                jobs.join("resume_cli_fixture.yaml"),
                "schemaVersion: 2\nkind: Job\nmetadata:\n  name: resume_cli_fixture\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 6\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n",
            )
            .expect("fixture job");
            fixture
        }

        fn command(&self) -> assert_cmd::Command {
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

        fn json(&self, args: &[&str]) -> Value {
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
            self.poll_run(&run_id, "interrupted", Duration::from_secs(10));
            run_id
        }

        fn poll_run(&self, run_id: &str, state: &str, timeout: Duration) -> Value {
            let deadline = Instant::now() + timeout;
            loop {
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
            let jobs = self.home.join(".orbit/resources/jobs");
            fs::write(
                jobs.join(format!("{name}.yaml")),
                format!(
                    "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {name}\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 1\n      spec:\n        type: deterministic\n        action: sleep\n        config: {{}}\n"
                ),
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
    #[test]
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
            &["run", "auto", "--json"][..],
        ] {
            let submitted = fixture.installed_json(&installed, args);
            let run_id = submitted["run_id"].as_str().expect("submitted run id");
            fixture.assert_worker_started(run_id);
            fixture.poll_run(run_id, "success", Duration::from_secs(10));
        }
    }

    /// [ORB-13987] `orbit run auto` still starts a drain whose required
    /// validation cannot use the login shell, and says so at submission.
    #[test]
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
            ("workflow.validation_env.login_shell", "false"),
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
            warning.contains("`workflow.validation_env.login_shell = false`")
                && warning.contains("source: launcher_fallback"),
            "the submission names the disabled resolution and its fallback: {submitted}"
        );
        let run_id = submitted["run_id"]
            .as_str()
            .expect("the drain still starts");
        fixture.poll_run(run_id, "success", Duration::from_secs(10));
    }

    /// ORB-13854: a real gate must explicitly release its reservation before
    /// reporting a failed child, rather than relying on terminal-run cleanup.
    #[test]
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
            assert_eq!(dispatches.len(), 1, "gate must dispatch a real child");
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

    #[test]
    fn resume_returns_while_detached_worker_continues_after_cli_exit() {
        let fixture = Fixture::new();
        let source = fixture.interrupted_source();
        let started = Instant::now();
        let submitted = fixture.json(&["job", "resume", &source, "--json"]);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "resume blocked on the job"
        );
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
        fixture.poll_run(run_id, "success", Duration::from_secs(20));
    }

    #[test]
    fn resume_wait_joins_detached_run_and_reports_success() {
        let fixture = Fixture::new();
        let source = fixture.interrupted_source();
        let started = Instant::now();
        let waited = fixture.json(&["job", "resume", &source, "--wait", "--json"]);
        assert_eq!(waited["waited"], true);
        assert_eq!(waited["state"], "success");
        assert!(
            started.elapsed() >= Duration::from_secs(5),
            "--wait returned before execution"
        );
        let run_id = waited["run_id"].as_str().expect("resumed run id");
        let shown = fixture.json(&["run", "show", run_id, "--json"]);
        assert_eq!(shown["run"]["state"], "success");
        assert_ne!(
            shown["run"]["pid"].as_u64(),
            Some(std::process::id() as u64)
        );
    }

    #[test]
    fn resume_wait_exits_nonzero_for_a_failed_detached_run() {
        let fixture = Fixture::new();
        let source = fixture.interrupted_source();
        let path = fixture
            .home
            .join(".orbit/resources/jobs/resume_cli_fixture.yaml");
        let job = fs::read_to_string(&path).expect("fixture job");
        fs::write(
            &path,
            job.replace("action: sleep", "action: missing_action"),
        )
        .expect("change resumed job definition");

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
}
