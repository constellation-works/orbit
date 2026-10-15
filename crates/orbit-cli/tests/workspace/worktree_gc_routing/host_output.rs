//! Host-output controls driven by the existing public CLI and GC routine.

use super::*;
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::OrbitRuntimeRoots;
use orbit_engine::RuntimeHost;
use orbit_types::workflow::JobRunState;

pub(super) struct HostOutput {
    eligible: PathBuf,
    estimated_bytes: u64,
    retained_source: PathBuf,
    source_head: String,
    primary_head: String,
    controls: Vec<(PathBuf, &'static str)>,
    runtime: OrbitRuntime,
    active_run: String,
    owner: crate::child_guard::ChildGuard,
    _lease: orbit_engine::review_gate::ScratchHostTargetLease,
}

impl HostOutput {
    pub(super) fn seed(workspace: &Workspace, home: &Path) -> Self {
        run_success(
            &workspace.repo,
            home,
            &[
                "config",
                "set",
                "workflow.validation_env.build_output_env",
                "['BUILD_OUTPUT_DIR']",
                "--seed-from-global",
            ],
        );
        // Retain undeclared output of kept sources, while removed sources
        // remain eligible. Only this disposable workspace is configured.
        run_success(
            &workspace.repo,
            home,
            &[
                "config",
                "set",
                "worktree.reclaim",
                "[]",
                "--seed-from-global",
            ],
        );
        let runtime = RegisteredRuntimeFactory::open_resolved_roots(OrbitRuntimeRoots {
            global_root: home.join(".orbit"),
            shared_root: workspace.orbit_dir.clone(),
            local_root: workspace.orbit_dir.clone(),
        })
        .unwrap();
        let build = |checkout: &Path| {
            let result = orbit_engine::review_gate::run_required_validation(
                &runtime, checkout,
                "mkdir -p \"$BUILD_OUTPUT_DIR/debug\" && printf 'fixture host output' > \"$BUILD_OUTPUT_DIR/debug/output\"",
            ).unwrap();
            assert!(result.passed, "{}", result.output);
            PathBuf::from(
                result.environment["build_output"]["directory"]
                    .as_str()
                    .unwrap(),
            )
        };
        let detach = |checkout: &Path| {
            crate::git_repo::run(
                &workspace.repo,
                &["worktree", "remove", "--force", checkout.to_str().unwrap()],
            );
        };
        let (source, _) = seed_eligible_worktree(workspace, home, "host-orphan");
        let eligible = build(&source);
        detach(&source);
        assert!(!source.exists());

        // A separate attributed fixture exercises explicit public confirmation
        // before the routine gets its own untouched eligible output.
        let (confirm_source, _) = seed_eligible_worktree(workspace, home, "host-confirm");
        let confirm = build(&confirm_source);
        let confirm_run = confirm_source
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("orbit-")
            .unwrap();
        detach(&confirm_source);
        let preview = run_json(
            &workspace.repo,
            home,
            &[
                "gc",
                "worktrees",
                "--run",
                confirm_run,
                "--reclaim",
                "--json",
            ],
        );
        let measured = preview["bytes_reclaimed"].as_u64().unwrap();
        assert!(measured >= 19, "{preview}");
        assert!(confirm.join("debug/output").exists());
        let output = command(&workspace.repo, home)
            .env("ORBIT_OPERATOR", "1")
            .args([
                "gc",
                "worktrees",
                "--run",
                confirm_run,
                "--reclaim",
                "--confirm",
                "--json",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let applied: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(applied["bytes_reclaimed"], measured, "{applied}");
        assert!(!confirm.exists());
        test_env::record_scenario_pass(&format!(
            "host-output-confirm source={} measured_bytes={measured}",
            confirm_source.display()
        ));

        // Both runs have real passive owners. The retained source has no task
        // lineage, so the whole-worktree collector must keep it as evidence.
        let (retained_run, retained_owner) = start_run(&runtime);
        let retained_source = attach_run(&runtime, workspace, &retained_run);
        let retained = build(&retained_source);
        finish_run(&runtime, &retained_run, retained_owner);
        fs::create_dir_all(retained_source.join(".orbit/tmp")).unwrap();
        fs::write(
            retained_source.join(".orbit/tmp/evidence.json"),
            "retained evidence",
        )
        .unwrap();
        let (active_run, owner) = start_run(&runtime);
        let active_source = attach_run(&runtime, workspace, &active_run);
        let active = build(&active_source);

        let unknown_source = workspace.repo.join(".orbit/state/worktrees/unknown-output");
        crate::git_repo::run(
            &workspace.repo,
            &[
                "worktree",
                "add",
                "--detach",
                unknown_source.to_str().unwrap(),
                "HEAD",
            ],
        );
        let unknown = build(&unknown_source);
        detach(&unknown_source);
        let leased_source = workspace
            .repo
            .join(".orbit/state/recovery-checkouts/leased-output");
        crate::git_repo::run(
            &workspace.repo,
            &[
                "worktree",
                "add",
                "--detach",
                leased_source.to_str().unwrap(),
                "HEAD",
            ],
        );
        let lease = orbit_engine::review_gate::lease_scratch_host_target(
            &leased_source,
            orbit_engine::review_gate::ScratchHostTarget::Recovery,
        )
        .unwrap();
        let leased = build(&leased_source);
        detach(&leased_source);
        let source_head = crate::git_repo::run(&retained_source, &["rev-parse", "HEAD"]);
        let primary_head = crate::git_repo::run(&workspace.repo, &["rev-parse", "HEAD"]);
        let mut fixture = Self {
            eligible,
            estimated_bytes: 0,
            retained_source,
            source_head,
            primary_head,
            controls: vec![
                (active, "skipped:run_not_terminal"),
                (retained, "skipped:source_retained"),
                (unknown, "skipped:unattributed_host_target"),
                (leased, "skipped:host_target_in_use"),
            ],
            runtime,
            active_run,
            owner,
            _lease: lease,
        };
        let before = run_json(
            &workspace.repo,
            home,
            &["gc", "worktrees", "--reclaim", "--json"],
        );
        fixture.estimated_bytes = fixture.reclaimed_bytes(&before);
        fixture.assert_reports(&before, "would_reclaim");
        fixture.assert_controls();
        test_env::record_scenario_pass(&format!("host-output-before {before}"));
        fixture
    }

    pub(super) fn assert_reports(&self, result: &Value, eligible_action: &str) {
        let reports = result["reports"].as_array().unwrap();
        let eligible = reports
            .iter()
            .find(|report| report["path"] == self.eligible.to_string_lossy().as_ref());
        if eligible_action == "absent" {
            assert!(eligible.is_none(), "{result}");
        } else {
            let report =
                eligible.unwrap_or_else(|| panic!("missing attributable host output: {result}"));
            assert_eq!(report["action"], eligible_action, "{report}");
            assert_eq!(report["bytes_reclaimed"], self.estimated_bytes, "{report}");
            assert!(self.estimated_bytes >= 19, "{report}");
        }
        for (path, action) in &self.controls {
            let report = reports
                .iter()
                .find(|report| report["path"] == path.to_string_lossy().as_ref())
                .unwrap_or_else(|| panic!("missing control {}: {result}", path.display()));
            assert_eq!(report["action"], *action, "{report}");
            assert_eq!(report["bytes_reclaimed"], 0, "{report}");
        }
    }

    pub(super) fn assert_controls(&self) {
        for (path, _) in &self.controls {
            assert_eq!(
                fs::read_to_string(path.join("debug/output")).unwrap(),
                "fixture host output"
            );
        }
        let active_run = self.runtime.show_job_run(&self.active_run).unwrap();
        assert_eq!(active_run.state, JobRunState::Running);
        assert_eq!(
            fs::read_to_string(self.retained_source.join(".orbit/tmp/evidence.json")).unwrap(),
            "retained evidence"
        );
        assert!(self.retained_source.join(".git").is_file());
        assert_eq!(
            crate::git_repo::run(&self.retained_source, &["rev-parse", "HEAD"]),
            self.source_head
        );
        assert_eq!(
            crate::git_repo::run(&self.retained_source, &["rev-parse", "refs/heads/main"]),
            self.primary_head
        );
    }

    pub(super) fn reclaimed_bytes(&self, result: &Value) -> u64 {
        result["reports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|report| report["path"] == self.eligible.to_string_lossy().as_ref())
            .unwrap()["bytes_reclaimed"]
            .as_u64()
            .unwrap()
    }
}

fn attach_run(runtime: &OrbitRuntime, workspace: &Workspace, run_id: &str) -> PathBuf {
    let run = runtime.show_job_run(run_id).unwrap();
    let source = orbit_engine::run_worktree_paths(&workspace.repo, &run)
        .unwrap()
        .remove(0);
    crate::git_repo::run(
        &workspace.repo,
        &[
            "worktree",
            "add",
            "--detach",
            source.to_str().unwrap(),
            "HEAD",
        ],
    );
    source
}

fn start_run(runtime: &OrbitRuntime) -> (String, crate::child_guard::ChildGuard) {
    let run = runtime
        .insert_job_run("gc_fixture_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    let mut child = std::process::Command::new("/bin/sh");
    test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    let owner = crate::child_guard::ChildGuard::new(
        child
            .args(["-c", "read -r release; exit 0"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert!(
        runtime
            .mark_job_run_running(&run.run_id, Utc::now(), owner.id())
            .unwrap()
            .owns_execution()
    );
    (run.run_id, owner)
}

fn finish_run(runtime: &OrbitRuntime, run_id: &str, mut owner: crate::child_guard::ChildGuard) {
    assert!(
        runtime
            .finalize_job_run(run_id, JobRunState::Success, Utc::now(), Some(0))
            .unwrap()
    );
    drop(owner.stdin.take());
    assert!(owner.wait().unwrap().success());
}

impl Drop for HostOutput {
    fn drop(&mut self) {
        // Settle the passive active control before the routine-worker guard
        // inventories all fixture runs. ChildGuard also reaps on an unwind.
        let _ = self.runtime.finalize_job_run(
            &self.active_run,
            JobRunState::Success,
            Utc::now(),
            Some(0),
        );
        drop(self.owner.stdin.take());
        let _ = self.owner.wait();
    }
}
