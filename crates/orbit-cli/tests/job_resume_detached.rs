#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! A CLI resume must hand ownership to a worker before its caller exits.

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use assert_cmd::cargo::cargo_bin_cmd;
    use orbit_common::test_env;
    use serde_json::Value;
    use tempfile::{TempDir, tempdir};

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
            let config_path = fixture.home.join(".orbit/config.toml");
            let mut config = fs::read_to_string(&config_path).expect("global config");
            if !config.contains("[crews.sol]") {
                config.push_str(
                    "\n[crews.sol]\nprovider = \"codex\"\nmodel = \"gpt-6-sol\"\nbackend = \"cli\"\n",
                );
            }
            // `orbit init` only seeds `[workflow].default_crew` when it detects an
            // agent CLI on PATH. On a host with none (e.g. CI), the appended
            // `[crews.sol]` above would otherwise leave every crew undeclared as
            // the default, which config validation refuses. Force it here so the
            // fixture is independent of the host's PATH.
            if !config.contains("default_crew = ") {
                let marker = "[workflow]\n";
                let insertion = config.find(marker).expect("workflow table") + marker.len();
                config.insert_str(insertion, "default_crew = \"sol\"\n");
            }
            fs::write(config_path, config).expect("configure crew");
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
