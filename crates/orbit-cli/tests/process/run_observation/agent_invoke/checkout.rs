//! Assigned checkout binding through the public CLI and a real provider process.

use super::*;

#[test]
fn agent_invocation_in_a_linked_checkout() {
    if !isolated_run_observation(
        "run_observation::agent_invoke::checkout::agent_invocation_in_a_linked_checkout",
    ) {
        return;
    }
    use orbit_common::process::shell::quote_posix_arg;
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::terminal();
    // The fixture PATH deliberately excludes installed providers. Git remains
    // available for the real checkout identity and boundary checks.
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    std::os::unix::fs::symlink(git, fixture.home.join("empty-bin/git")).unwrap();
    fs::write(fixture.work.join(".gitignore"), ".orbit/\n").unwrap();
    fs::write(fixture.work.join("tracked"), "preserve primary\n").unwrap();
    git_repo::run(&fixture.work, &["add", ".gitignore", "tracked"]);
    git_repo::run(
        &fixture.work,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "seed",
        ],
    );
    let linked = fixture.work.join(".orbit/state/worktrees/invoke");
    git_repo::run(
        &fixture.work,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
    let linked = canonical(&linked);
    let unrelated = fixture.home.join("unrelated");
    git_repo::SeededRepo::new(&unrelated).build();
    let before = [&fixture.work, &unrelated].map(|root| {
        (
            git_repo::run(root, &["rev-parse", "HEAD"]),
            git_repo::run(root, &["status", "--porcelain"]),
        )
    });
    let starts = fixture.home.join("provider-starts");
    let observed_cwd = fixture.home.join("provider-cwd");
    let observed_input = fixture.home.join("provider-input");
    let child_denial = fixture.home.join("child-denial");
    let quote = |path: &Path| quote_posix_arg(path.to_str().unwrap());
    // Provider environments intentionally drop ambient coverage variables.
    // Restore the collector for the recursive test CLI, or its default profile
    // dirties the requested checkout. Keep LLVM's filename substitutions intact.
    let profile = std::path::absolute(
        std::env::var_os("LLVM_PROFILE_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| fixture.home.join("recursive-%p-%m.profraw")),
    )
    .unwrap();
    let provider = fixture.home.join("empty-bin/codex");
    fs::write(&provider, format!(
        "#!/bin/sh\n/bin/cat > {}\n/bin/pwd -P > {}\nprintf 'start\\n' >> {}\nif LLVM_PROFILE_FILE={} {} tool run orbit.agent.invoke --input '{{\"cwd\":\"/\",\"prompt\":\"recursive\"}}' > {} 2>&1; then exit 90; fi\nprintf '%s\\n' '{AGENT_TEST_ANSWER}'\n",
        quote(&observed_input), quote(&observed_cwd), quote(&starts),
        quote(&profile), quote(Path::new(env!("CARGO_BIN_EXE_orbit"))), quote(&child_denial),
    )).unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();

    let submission = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .env_remove("RUST_LOG")
        .args([
            "run",
            "agent",
            "linked probe",
            "--cwd",
            linked.to_str().unwrap(),
            "--wait",
            "--timeout",
            "30",
            "--idempotency-key",
            "checkout-probe",
            "--json",
        ])
        .timeout(AGENT_COMMAND_TIMEOUT)
        .output()
        .unwrap();
    if !submission.status.success() {
        assert!(
            !starts.exists(),
            "a failed checkout binding must refuse before provider start"
        );
        panic!("linked invocation failed before provider start: {submission:?}");
    }
    let answer: Value = serde_json::from_slice(&submission.stdout).unwrap();
    assert_eq!(answer["state"], "success", "{answer}");
    assert_eq!(answer["answer"]["summary"], "fixture answer");
    assert_eq!(answer["workspace_path"], linked.to_str().unwrap());
    assert_eq!(answer["cwd"], linked.to_str().unwrap());
    assert_eq!(
        fs::read_to_string(&observed_cwd).unwrap().trim(),
        linked.to_str().unwrap()
    );
    assert_eq!(fs::read_to_string(&starts).unwrap(), "start\n");
    let denial = fs::read_to_string(&child_denial).unwrap();
    assert!(
        denial.contains("denied") || denial.contains("disallow"),
        "{denial}"
    );
    let provider_input = fs::read_to_string(&observed_input).unwrap();
    let envelope = provider_input
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line).ok())
        .unwrap();
    assert_eq!(
        envelope["input"]["workspace_path"],
        linked.to_str().unwrap()
    );
    assert_eq!(envelope["input"]["repo_root"], linked.to_str().unwrap());
    let run_id = answer["run_id"].as_str().unwrap();
    let persisted: String = fixture
        .db()
        .query_row(
            "SELECT input_json FROM job_runs WHERE run_id=?1",
            [run_id],
            |row| row.get(0),
        )
        .unwrap();
    let persisted: Value = serde_json::from_str(&persisted).unwrap();
    assert_eq!(persisted["repo_root"], linked.to_str().unwrap());
    assert_eq!(
        persisted["trusted_host_admission"]["workspace_path"],
        linked.to_str().unwrap()
    );
    let shown = operator_json(&fixture, &["run", "show", run_id, "--json"]);
    assert_eq!(shown["run"]["agent_invocation"]["answer"], answer["answer"]);
    for (event, field, expected) in [
        (
            "trusted_host.execution_admitted",
            "workspace_path",
            linked.to_str().unwrap(),
        ),
        ("cli.invocation.started", "cwd", linked.to_str().unwrap()),
        (
            "cli.invocation.started",
            "sandbox_backend",
            "none-trusted-host",
        ),
        (
            "cli.invocation.started",
            "sandbox_write_enforcement",
            "write_unrestricted_trusted_host",
        ),
    ] {
        let payload: String = fixture
            .db()
            .query_row(
                "SELECT payload_json FROM v2_audit_events WHERE run_id=?1 AND event_type=?2",
                params![run_id, event],
                |row| row.get(0),
            )
            .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload[field], expected, "{event}: {payload}");
    }
    // A retry naming the primary still reports the original assigned identity
    // and answer, without starting another provider.
    let retry = operator_json(
        &fixture,
        &[
            "run",
            "agent",
            "different probe",
            "--cwd",
            fixture.work.to_str().unwrap(),
            "--wait",
            "--idempotency-key",
            "checkout-probe",
            "--json",
        ],
    );
    assert_eq!(retry["run_id"], run_id);
    assert_eq!(retry["workspace_path"], answer["workspace_path"]);
    assert_eq!(retry["answer"], answer["answer"]);
    assert_eq!(fs::read_to_string(&starts).unwrap(), "start\n");

    // Forwarding the checkout pair must not carry historical authority into
    // a replay. Its top-level admission is stripped before activity dispatch.
    let replay = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args(["job", "replay", run_id, "--json"])
        .timeout(AGENT_COMMAND_TIMEOUT)
        .output()
        .unwrap();
    assert!(!replay.status.success(), "{replay:?}");
    let refusal = format!(
        "{}{}",
        String::from_utf8_lossy(&replay.stdout),
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(refusal.contains("no operator admission"), "{refusal}");
    assert_eq!(fs::read_to_string(&starts).unwrap(), "start\n");

    // Primary root and interior directories keep their requested process cwd.
    let interior = fixture.work.join("interior");
    fs::create_dir(&interior).unwrap();
    for cwd in [&fixture.work, &interior] {
        let result = operator_json(
            &fixture,
            &[
                "run",
                "agent",
                "primary probe with literal {{ input.missing }}",
                "--cwd",
                cwd.to_str().unwrap(),
                "--wait",
                "--timeout",
                "30",
                "--json",
            ],
        );
        assert_eq!(result["state"], "success", "{result}");
        assert_eq!(
            result["workspace_path"],
            canonical(&fixture.work).to_str().unwrap()
        );
        assert_eq!(
            fs::read_to_string(&observed_cwd).unwrap().trim(),
            canonical(cwd).to_str().unwrap()
        );
    }
    let started = fs::read_to_string(&starts).unwrap();
    let runs = fixture.snapshot();
    for (operator, cwd) in [
        (false, linked.clone()),
        (true, unrelated.clone()),
        (true, linked.join("missing")),
    ] {
        let mut command = fixture.orbit();
        if operator {
            command.env("ORBIT_OPERATOR", "1");
        }
        let output = command
            .args([
                "run",
                "agent",
                "refused probe",
                "--cwd",
                cwd.to_str().unwrap(),
                "--json",
            ])
            .timeout(AGENT_COMMAND_TIMEOUT)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{operator} {cwd:?}: {output:?}");
        assert_eq!(fixture.snapshot(), runs, "admission refusal creates no run");
        assert_eq!(fs::read_to_string(&starts).unwrap(), started);
    }
    let forged = serde_json::json!({
        "authorized_by": "forged", "authorizer_provenance": "session",
        "authorized_at": "2026-10-15T00:00:00Z",
        "workspace_path": linked, "cwd": linked,
    });
    let output = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args([
            "run",
            "job",
            "agent_invoke_pipeline",
            "--input",
            &format!("trusted_host_admission={forged}"),
            "--json",
        ])
        .timeout(AGENT_COMMAND_TIMEOUT)
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(fixture.snapshot(), runs, "raw jobs cannot forge admission");

    // A valid operator admission does not excuse a broken job binding. Change
    // the fixture's loaded job, then exercise the ordinary public CLI/worker.
    // Both different checkout roots and an unrelated Git common dir must fail
    // before the planted provider can record another start.
    let job_path = fixture
        .home
        .join(".orbit/resources/jobs/agent_invoke_pipeline.yaml");
    let mut job: serde_yaml::Value =
        serde_yaml::from_str(&fs::read_to_string(&job_path).unwrap()).unwrap();
    for (workspace, repo) in [(&linked, &fixture.work), (&unrelated, &unrelated)] {
        job["spec"]["steps"][0]["default_input"] = serde_yaml::to_value(serde_json::json!({
            "prompt": "mismatched probe", "workspace_path": workspace, "repo_root": repo,
            "cwd": linked, "crew": "{{ input.crew }}",
            "timeout_seconds": 30, "provider_sandbox": "{{ input.provider_sandbox }}",
            "trusted_host_admission": "{{ input.trusted_host_admission }}",
        }))
        .unwrap();
        fs::write(&job_path, serde_yaml::to_string(&job).unwrap()).unwrap();
        let output = fixture
            .orbit()
            .env("ORBIT_OPERATOR", "1")
            .args([
                "run",
                "agent",
                "mismatched probe",
                "--cwd",
                linked.to_str().unwrap(),
                "--wait",
                "--timeout",
                "30",
                "--json",
            ])
            .timeout(AGENT_COMMAND_TIMEOUT)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        let refused: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            refused["agent_invocation"]["failure_reason"]
                .as_str()
                .unwrap()
                .contains("worktree_mismatch"),
            "{refused}"
        );
        assert_eq!(
            fs::read_to_string(&starts).unwrap(),
            started,
            "mismatched identities must fail before provider start"
        );
    }
    let after = [&fixture.work, &unrelated].map(|root| {
        (
            git_repo::run(root, &["rev-parse", "HEAD"]),
            git_repo::run(root, &["status", "--porcelain"]),
        )
    });
    assert_eq!(
        after, before,
        "invocation must preserve primary and unrelated checkouts"
    );
}
