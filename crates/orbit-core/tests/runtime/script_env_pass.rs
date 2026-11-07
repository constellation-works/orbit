//! Script steps phase 5 at the runtime's script-step boundary: only the
//! variables a grant's `env_pass` names reach the script, a granted name
//! the job's process lacks stays unset and is recorded, and a passed value
//! the script prints is redacted from the captured log, the step output, the
//! `script_execution` audit entry and the run view `orbit run show` reads. A
//! manifest asking for more network than its grant refuses before spawn.
//!
//! The env cases run in a child process that holds the fixture variables,
//! so no test mutates its own process environment.

use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_core::OrbitRuntime;
use orbit_core::runtime::scripts::ScriptGrantOptions;
use orbit_engine::{RuntimeHost, ScriptStepEnd, ScriptStepRequest, ScriptStepRun};
use orbit_types::workflow::activity_job::{ScriptNetwork, script_refusal};
use serde_json::{Value, json};

const SCRIPT: &str = "envcheck";
const PASSED: &str = "SCRIPT_FIXTURE_TOKEN";
const UNGRANTED: &str = "SCRIPT_FIXTURE_UNGRANTED";
const MISSING: &str = "SCRIPT_FIXTURE_MISSING";
/// No credential shape, so only the literal redaction of passed values can
/// hide it.
const SECRET: &str = "harbor-lantern-4217";
const UNGRANTED_VALUE: &str = "ungranted-value-9931";

struct Fixture {
    root: tempfile::TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    /// Written by the entry when it runs; must stay absent after a refusal.
    ran_marker: PathBuf,
}

fn fixture(requests: &str, grant: ScriptGrantOptions) -> Fixture {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let marker_dir = root.path().join("marker");
    for dir in [&global, &repo.join(".orbit/resources/scripts"), &marker_dir] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let ran_marker = marker_dir.join("ran");
    std::fs::write(
        repo.join("envcheck.sh"),
        format!(
            "#!/bin/sh\n\
             true > '{marker}' 2>/dev/null\n\
             echo \"log token=${PASSED}\" >&2\n\
             printf '{{\"token\":\"%s\",\"ungranted\":\"%s\",\"missing\":\"%s\"}}' \
             \"${PASSED}\" \"${{{UNGRANTED}:-absent}}\" \"${{{MISSING}-unset}}\"\n",
            marker = ran_marker.display()
        ),
    )
    .unwrap();
    std::fs::write(
        repo.join(format!(".orbit/resources/scripts/{SCRIPT}.yaml")),
        format!(
            "schemaVersion: 1\nkind: Script\nmetadata:\n  name: {SCRIPT}\nspec:\n  \
             entry: envcheck.sh\n  interpreter: [sh]\n  requests:\n{requests}"
        ),
    )
    .unwrap();
    std::fs::write(
        repo.join("envcheck-job.yaml"),
        format!(
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: envcheck\nspec:\n  state: enabled\n  \
             kind: workflow\n  steps:\n    - id: run\n      target: script:{SCRIPT}\n"
        ),
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    runtime
        .grant_script(
            SCRIPT,
            &ScriptGrantOptions {
                // The marker dir lies under the bulk temp dir, which the
                // sandbox's private `/dev` hides, so it is bound writable.
                write_roots: vec![marker_dir.clone()],
                ..grant
            },
        )
        .expect("grant the fixture script");
    Fixture {
        root,
        runtime,
        repo,
        ran_marker,
    }
}

fn request() -> ScriptStepRequest {
    ScriptStepRequest {
        run_id: "run-env-pass".to_string(),
        step_id: "run".to_string(),
        script: SCRIPT.to_string(),
        input: json!({}),
        workspace_path: None,
        timeout_seconds: Some(60),
    }
}

/// Every file under `root` whose bytes hold `needle`.
fn files_holding(root: &Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file()
                && std::fs::read(&path)
                    .is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
            {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn a_network_request_beyond_the_grant_refuses_before_spawn() {
    if !super::dispatch_admission::isolated(
        "script_env_pass::a_network_request_beyond_the_grant_refuses_before_spawn",
    ) {
        return;
    }
    let fixture = fixture(
        "    network: any\n",
        ScriptGrantOptions {
            network: Some(ScriptNetwork::Https),
            ..Default::default()
        },
    );
    let ScriptStepRun::Refused {
        code,
        message,
        record,
    } = RuntimeHost::run_script_step(&fixture.runtime, &request())
    else {
        panic!("a manifest asking for more network than its grant must refuse");
    };
    assert_eq!(code, script_refusal::GRANT_INSUFFICIENT, "{message}");
    assert!(message.contains("network `any`"), "{message}");
    assert!(
        record.argv.is_empty() && record.profile.is_none(),
        "{record:?}"
    );
    assert!(!fixture.ran_marker.exists(), "nothing ran");
}

/// Spawns [`granted_variables_alone_reach_the_script_and_their_values_are_redacted_in_child`]
/// with the fixture variables set, and one granted name absent.
#[test]
fn granted_variables_alone_reach_the_script_and_their_values_are_redacted() {
    let child = "script_env_pass::granted_variables_alone_reach_the_script_and_their_values_are_redacted_in_child";
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", child, "--ignored", "--nocapture"])
        .env(PASSED, SECRET)
        .env(UNGRANTED, UNGRANTED_VALUE)
        .env_remove(MISSING);
    let output = command.output().unwrap();
    orbit_common::test_env::assert_child_test_passed(
        child,
        output.status,
        &output.stdout,
        &output.stderr,
    );
}

#[test]
#[ignore = "isolated child entry point"]
fn granted_variables_alone_reach_the_script_and_their_values_are_redacted_in_child() {
    assert_eq!(std::env::var(PASSED).as_deref(), Ok(SECRET));
    let fixture = fixture(
        &format!("    env_pass: [{PASSED}, {MISSING}]\n"),
        ScriptGrantOptions::default(),
    );

    let ran = match RuntimeHost::run_script_step(&fixture.runtime, &request()) {
        ScriptStepRun::Refused {
            code,
            message,
            record,
        } if code == script_refusal::SANDBOX_UNAVAILABLE => {
            // The passed names are settled before the host probe, so even a
            // host without the profile records them, and never a value.
            assert_eq!(record.env_pass, [PASSED], "{record:?}");
            assert_eq!(record.env_pass_unset, [MISSING], "{record:?}");
            assert!(!format!("{record:?}{message}").contains(SECRET));
            assert!(!fixture.ran_marker.exists(), "nothing ran");
            orbit_exec::report_bwrap_deferral(
                "granted_variables_alone_reach_the_script_and_their_values_are_redacted",
                &message,
            );
            return;
        }
        ScriptStepRun::Refused { code, message, .. } => {
            panic!("unexpected refusal {code}: {message}")
        }
        ScriptStepRun::Ran(ran) => ran,
    };
    assert_eq!(
        ran.end,
        ScriptStepEnd::Exited(0),
        "{}",
        String::from_utf8_lossy(&ran.log)
    );
    let output: Value = serde_json::from_slice(&ran.stdout).unwrap();
    assert_eq!(
        output,
        json!({"token": "[REDACTED]", "ungranted": "absent", "missing": "unset"}),
        "the granted variable reached the script and its echoed value is redacted; \
         an ungranted variable never reached it; a granted name the process lacks is unset"
    );
    let log = String::from_utf8_lossy(&ran.log);
    assert!(log.contains("log token=[REDACTED]"), "{log}");
    assert!(!log.contains(SECRET), "{log}");
    assert_eq!(ran.record.env_pass, [PASSED]);
    assert_eq!(ran.record.env_pass_unset, [MISSING]);
    assert!(!format!("{:?}", ran.record).contains(SECRET));

    // Through a job run: the step output, the audit entry `orbit run show`
    // lists, and every file the run left in the workspace state.
    let result = fixture
        .runtime
        .run_job_v2_from_yaml(&fixture.repo.join("envcheck-job.yaml"), json!({}))
        .expect("run the job");
    assert!(result.success, "{result:?}");
    let pipeline = result.pipeline.to_string();
    assert!(pipeline.contains("[REDACTED]"), "{pipeline}");
    assert!(!pipeline.contains(SECRET), "{pipeline}");
    let view = fixture
        .runtime
        .collect_run_audit_view(&result.run_id)
        .expect("run audit view");
    let [execution] = view.script_executions.as_slice() else {
        panic!("one script execution: {:?}", view.script_executions);
    };
    let shown = execution.to_json().to_string();
    assert!(
        shown.contains(PASSED),
        "the passed name is recorded: {shown}"
    );
    assert!(!shown.contains(SECRET), "{shown}");
    assert!(execution.record.log_blob_ref.is_some(), "{shown}");
    let leaked = files_holding(fixture.root.path(), SECRET.as_bytes());
    assert!(
        leaked.is_empty(),
        "the passed value is stored nowhere: {leaked:?}"
    );
}
