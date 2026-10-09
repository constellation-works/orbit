//! Completed job commands expose unsuccessful outcomes through their exit code.

use std::fs;

use serde_json::{Value, json};

use super::{FAILED, Fixture};

#[test]
fn failing_replay_exits_nonzero_and_preserves_its_result() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::init();
    // Exercise a completed failure (Ok(success=false)), rather than an
    // execution error that already gives the CLI a nonzero exit.
    let provider = fixture.home.join("empty-bin/codex");
    fs::write(
        &provider,
        "#!/bin/sh\nwhile IFS= read -r line; do :; done\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"failed\",\"result\":{},\"error\":{\"code\":\"fixture_failure\",\"message\":\"fixture failed\"}}'\n",
    )
    .unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();
    // This executor runs only the fixed shell substitute, matching the
    // isolated direct-agent fixture in support/secret_sinks.rs.
    fs::write(
        fixture.home.join(".orbit/resources/executors/codex.yaml"),
        json!({
            "schemaVersion": 2, "kind": "Executor", "metadata": {"name": "codex"},
            "spec": {"executor_type": "direct_agent", "command": provider,
                "args": [], "sandbox": "off", "env": {}}
        })
        .to_string(),
    )
    .unwrap();
    let jobs = fixture.home.join(".orbit/resources/jobs");
    fs::create_dir_all(&jobs).unwrap();
    fs::write(
        jobs.join("fixture_job.yaml"),
        json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "fixture_job"},
            "spec": {"state": "enabled", "steps": [{
                "id": "fail",
                "spec": {"type": "agent_loop", "description": "Report a fixture failure",
                    "instruction": "Report a fixture failure", "provider": "codex",
                    "wall_clock_timeout_seconds": 10, "tools": ["orbit.task.show"]}
            }]}
        })
        .to_string(),
    )
    .unwrap();

    for json_output in [true, false] {
        let mut command = fixture.orbit();
        command.args(["job", "replay", FAILED]);
        if json_output {
            command.arg("--json");
        }
        let output = command.assert().code(1).get_output().clone();
        if json_output {
            let replay: Value = serde_json::from_slice(&output.stdout)
                .unwrap_or_else(|error| panic!("replay result: {error}; {output:?}"));
            assert_eq!(replay["success"], false);
            assert_eq!(replay["source_run_id"], FAILED);
            let id = replay["run_id"].as_str().unwrap();
            let shown = fixture.json(&["run", "show", id, "--no-reconcile", "--json"]);
            assert_eq!(shown["run"]["state"], "failed");
            assert_eq!(shown["run"]["retry_source_run_id"], FAILED);
        } else {
            assert!(String::from_utf8_lossy(&output.stdout).contains("success=false"));
        }
    }
}

/// A Git substitute reports a server error for a verified local remote,
/// causing the real engine to hold without contacting a forge or an agent.
#[test]
fn held_run_resume_and_replay_exit_nonzero() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    let fixture = Fixture::init();
    let git = |args: &[&str]| {
        let output = crate::git_repo::command()
            .current_dir(&fixture.work)
            .env("HOME", &fixture.home)
            .env("USERPROFILE", &fixture.home)
            .env("GIT_ALLOW_PROTOCOL", "file")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    assert_eq!(
        fs::canonicalize(git(&["rev-parse", "--show-toplevel"])).unwrap(),
        fs::canonicalize(&fixture.work).unwrap()
    );
    fs::write(
        fixture.home.join(".gitconfig"),
        "[protocol]\nallow = never\n[protocol \"file\"]\nallow = always\n",
    )
    .unwrap();
    git(&[
        "-c",
        "user.name=Orbit",
        "-c",
        "user.email=orbit@example.invalid",
        "commit",
        "--allow-empty",
        "-m",
        "fixture",
    ]);
    let remote = fixture.home.join("remote.git");
    git(&["init", "--bare", remote.to_str().unwrap()]);
    git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    assert_eq!(
        git(&["remote", "get-url", "--push", "--all", "origin"]),
        remote.to_str().unwrap()
    );
    let inherited_path = std::env::var_os("PATH").unwrap();
    let real_git = std::env::split_paths(&inherited_path)
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    let bin = fixture.home.join("forge-bin");
    fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("git");
    let quote = orbit_common::process::shell::quote_posix_arg;
    fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
export GIT_ALLOW_PROTOCOL=file
skip=
operation=
for argument in "$@"; do
  if [ -n "$skip" ]; then skip=; continue; fi
  if [ "$argument" = -c ]; then skip=1; continue; fi
  operation=$argument
  break
done
if [ "$operation" = push ]; then
  urls=$({real_git} remote get-url --push --all origin) || exit 1
  [ "$urls" = {remote} ] || {{ echo 'refusing non-local fixture push' >&2; exit 1; }}
  echo 'remote: Internal Server Error' >&2
  exit 1
fi
exec {real_git} "$@"
"#,
            real_git = quote(real_git.to_str().unwrap()),
            remote = quote(remote.to_str().unwrap()),
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let path =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&inherited_path)))
            .unwrap();

    let jobs = fixture.home.join(".orbit/resources/jobs");
    fs::create_dir_all(&jobs).unwrap();
    fs::write(
        jobs.join("held_fixture.yaml"),
        json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "held_fixture"},
            "spec": {"state": "enabled", "steps": [{
                "id": "push", "default_input": {
                    "workspace_path": fixture.work, "branch": "main",
                    "forge_retry": {"max_attempts": 1, "initial_backoff_ms": 1, "backoff_cap_ms": 1}
                },
                "spec": {"type": "deterministic", "action": "git_push", "config": {}}
            }]}
        })
        .to_string(),
    )
    .unwrap();
    let unsuccessful = |args: &[&str]| {
        let output = fixture
            .orbit()
            .env("PATH", &path)
            .env("GIT_ALLOW_PROTOCOL", "file")
            .args(args)
            .timeout(Duration::from_secs(30))
            .assert()
            .code(1)
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice::<Value>(&output).unwrap()
    };

    let waited = unsuccessful(&["run", "job", "held_fixture", "--wait", "--json"]);
    assert_eq!(waited["waited"], true);
    assert_eq!(waited["state"], "held");
    let source = waited["run_id"].as_str().unwrap();
    let resumed = unsuccessful(&["job", "resume", source, "--wait", "--json"]);
    assert_eq!(resumed["waited"], true);
    assert_eq!(resumed["state"], "held");
    let replay = unsuccessful(&["job", "replay", source, "--json"]);
    assert_eq!(replay["success"], false);
    assert_eq!(replay["source_run_id"], source);
    for result in [&waited, &resumed, &replay] {
        let id = result["run_id"].as_str().unwrap();
        let shown = fixture.json(&["run", "show", id, "--no-reconcile", "--json"]);
        assert_eq!(shown["run"]["state"], "held");
    }
}
