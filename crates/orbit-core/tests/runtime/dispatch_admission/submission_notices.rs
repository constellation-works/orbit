//! What a drain start or a ship submission reports while it proceeds, and
//! what a drain stop returns. CLI, MCP and the dashboard render the same
//! `submission_notices` and `DrainAdmissionsStopResult::to_json`.

use std::path::Path;

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_core::application::job::{SubmissionKind, SubmissionNotices};
use orbit_types::task::HostOs;
use orbit_types::workflow::{ChildDispatch, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{PressureProbe, Seed, as_operator, running_run, seed};

// Not `ORBIT_`-prefixed: those names are never reported unset.
const UNSET_PASS: &str = "FIXTURE_NOTICE_TOKEN";

/// A Linux host under sustained memory pressure, whose operator passes a
/// variable this environment does not hold, configured by `extra`.
fn throttled_runtime(root: &Path, extra: &str) -> OrbitRuntime {
    let global = root.join("home/.orbit");
    let workspace = root.join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        global.join("config.toml"),
        format!(
            "[execution.env]\npass = [\"HOME\", \"PATH\", \"CLAUDE_CODE_OAUTH_TOKEN\", \"{UNSET_PASS}\"]\n\n{extra}"
        ),
    )
    .unwrap();
    let probe = PressureProbe::calm();
    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .unwrap()
        .with_host_resource_probe(probe.clone())
        .with_host_os(Some(HostOs::Linux));
    probe.sustain_memory(&runtime, 95.0);
    runtime
}

fn receipt(notices: &SubmissionNotices) -> Value {
    let mut receipt = json!({});
    notices.insert_into(&mut receipt);
    receipt
}

fn each_submission_kind_carries_its_notices() {
    // This fixture's host holds its worker credential, so the Claude credential
    // warning [ORB-16663] on macOS stays out of the notices it asserts.
    let _credential =
        orbit_common::test_env::scoped([("CLAUDE_CODE_OAUTH_TOKEN", Some("fixture-worker-token"))]);
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = throttled_runtime(root.path(), "");
    let mac = seed(
        &runtime,
        Seed {
            title: "mac only",
            tags: &["os:macos"],
            ..Seed::default()
        },
    );
    let hold = runtime
        .admission_resource_throttle()
        .throttle
        .expect("sustained memory pressure throttles")
        .hold_reason();

    let drain = runtime.submission_notices(SubmissionKind::Drain);
    let warnings = drain.warnings();
    assert_eq!(warnings.len(), 3, "{warnings:#?}");
    assert_eq!(warnings[0], hold);
    assert!(
        warnings[1].contains(&mac.id) && warnings[1].contains("another OS"),
        "{warnings:#?}"
    );
    assert!(
        warnings[2].contains(UNSET_PASS) && warnings[2].contains("execution.env.pass"),
        "the unset pass-listed name is a warning, not a bare stderr line: {warnings:#?}"
    );
    assert_eq!(drain.validation_env_warning, None);
    let notes = drain.notes();
    assert!(
        notes.len() == 1 && notes[0].contains("workflow.required_validation_commands"),
        "{notes:?}"
    );
    let started = receipt(&drain);
    assert_eq!(started["warning"], warnings.join("\n"), "{started}");
    assert_eq!(started["note"], notes[0], "{started}");
    assert_eq!(
        started["resource_throttle"]["resources"][0]["resource"], "memory",
        "a drain's receipt carries the throttle detail: {started}"
    );
    assert!(started.get("validation_env_warning").is_none(), "{started}");

    // The owner orders a pull drain's work, so this host's backlog is not named.
    let pull = runtime.submission_notices(SubmissionKind::PullDrain);
    assert_eq!(
        pull.warnings(),
        [warnings[0].clone(), warnings[2].clone()],
        "{pull:#?}"
    );
    assert_eq!(pull.notes(), notes);

    // An explicit ship proceeds under the throttle and says so; discovery is
    // refused while throttled, so it has nothing to warn about. Neither runs
    // a drain, so neither notes the required-validation configuration.
    let explicit = runtime.submission_notices(SubmissionKind::Ship { explicit: true });
    assert_eq!(
        explicit.warnings(),
        [warnings[0].clone(), warnings[2].clone()]
    );
    assert!(explicit.notes().is_empty());
    let shipped = receipt(&explicit);
    assert!(
        shipped.get("resource_throttle").is_none() && shipped.get("note").is_none(),
        "a ship receipt carries the throttle only as a warning: {shipped}"
    );
    let discovery = runtime.submission_notices(SubmissionKind::Ship { explicit: false });
    assert_eq!(discovery.warnings(), [warnings[2].clone()]);
}

fn a_validation_environment_warning_reaches_every_kind() {
    let _credential =
        orbit_common::test_env::scoped([("CLAUDE_CODE_OAUTH_TOKEN", Some("fixture-worker-token"))]);
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = throttled_runtime(
        root.path(),
        "[workflow]\nrequired_validation_commands = [\"make ci-fast\"]\n\n\
         [workflow.validation_env]\nlogin_shell = false\n",
    );
    let warning = runtime
        .validation_env_preflight_warning()
        .expect("disabled login-shell resolution warns");
    for kind in [
        SubmissionKind::Drain,
        SubmissionKind::PullDrain,
        SubmissionKind::Ship { explicit: true },
        SubmissionKind::Ship { explicit: false },
    ] {
        let notices = runtime.submission_notices(kind);
        assert!(
            notices.warnings().contains(&warning),
            "{kind:?}: {notices:#?}"
        );
        assert!(
            notices.notes().is_empty(),
            "{kind:?}: configured required validation needs no note"
        );
        let receipt = receipt(&notices);
        assert_eq!(receipt["validation_env_warning"], warning, "{kind:?}");
        assert!(
            receipt["warning"]
                .as_str()
                .is_some_and(|lines| lines.split('\n').any(|line| line == warning)),
            "{kind:?}: {receipt}"
        );
    }
}

/// MCP returns the stop projection CLI and the dashboard return, inside its
/// own envelope.
fn an_mcp_stop_returns_the_shared_projection() {
    let (_root, runtime, _repo) = super::runtime();
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    let child = running_run(&runtime, "task_auto_pipeline", json!({}));
    let mut state = PipelineState::new(drain.clone(), "workspace_auto_pipeline".into(), json!({}));
    state.record_child_dispatch(ChildDispatch::submitted(
        child.clone(),
        "task_auto_pipeline".into(),
        "invoke_detached".into(),
        false,
        false,
        Utc::now(),
    ));
    runtime.write_run_state(&drain, &state).unwrap();
    let workspace = runtime.workspace_id().unwrap();

    let stopped = as_operator(
        &runtime,
        "orbit.workflow.auto",
        json!({"workspace": workspace, "action": "stop"}),
    );
    assert_eq!(stopped["action"], "stop", "{stopped}");
    assert_eq!(stopped["schema_version"], 1, "{stopped}");
    assert_eq!(stopped["outcome"], "stopped", "{stopped}");
    assert_eq!(stopped["pull_settlements"], json!([]), "{stopped}");
    assert_eq!(
        stopped["coordinators"],
        json!([{
            "run_id": drain,
            "job_id": "workspace_auto_pipeline",
            "outcome": "stopped",
            "remaining_children": [{
                "run_id": child,
                "job_name": "task_auto_pipeline",
                "phase": stopped["coordinators"][0]["remaining_children"][0]["phase"],
                "child_status": stopped["coordinators"][0]["remaining_children"][0]["child_status"],
            }],
            "forced_runs": [],
        }]),
        "{stopped:#}"
    );
    assert!(
        stopped["coordinators"][0]["remaining_children"][0]["phase"].is_string(),
        "{stopped:#}"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    each_submission_kind_carries_its_notices_scenarios,
    each_submission_kind_carries_its_notices,
    a_validation_environment_warning_reaches_every_kind,
    an_mcp_stop_returns_the_shared_projection
);
