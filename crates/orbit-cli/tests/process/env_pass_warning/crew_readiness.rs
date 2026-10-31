use super::*;

#[test]
fn config_set_warns_for_a_crew_source_missing_from_the_shell() {
    let fixture = Fixture::new();
    fixture.crew_env("sol-credits", "OPENAI_API_KEY");
    let output = fixture
        .command(&[])
        .args([
            "config",
            "set",
            "--global",
            "crews.sol-credits.env",
            &format!("[\"OPENAI_API_KEY={CREW_SOURCE}\"]"),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    let warning = body["warning"].as_str().unwrap_or_default();
    assert!(
        warning.contains(CREW_SOURCE) && warning.contains("sol-credits"),
        "{body}"
    );
    assert!(warning.contains("shell"), "{body}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
}

#[cfg(target_os = "linux")]
#[test]
fn config_and_doctor_check_the_service_even_when_the_shell_holds_the_source() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    fixture.crew_env("sol-credits", &format!("OPENAI_API_KEY={CREW_SOURCE}"));
    assert!(
        std::env::var_os(CREW_SOURCE).is_none(),
        "fixture source must be absent from the service stand-in"
    );
    let unit = fixture.home.join(".config/systemd/user/orbit-web.service");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "[Service]\nExecStart=orbit web serve\n").unwrap();
    let bin = fixture.home.join("fixture-bin");
    fs::create_dir_all(&bin).unwrap();
    let systemctl = bin.join("systemctl");
    let script = |pid: &str| {
        format!(
            "#!/bin/sh\nprintf '%s\\n' 'FragmentPath={}' \"MainPID={pid}\"\n",
            unit.display()
        )
    };
    fs::write(&systemctl, script(&std::process::id().to_string())).unwrap();
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let set = [(SET, SECRET), (UNSET, SECRET), (CREW_SOURCE, SECRET)];
    let config_output = fixture
        .command(&set)
        .env("PATH", &path)
        .args([
            "config",
            "set",
            "--global",
            "crews.sol-credits.env",
            &format!("[\"OPENAI_API_KEY={CREW_SOURCE}\"]"),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(config_output.status.success());
    let config: Value = serde_json::from_slice(&config_output.stdout).unwrap();
    let warning = config["warning"].as_str().unwrap_or_default();
    assert!(
        warning.contains(CREW_SOURCE)
            && warning.contains("managed service environment")
            && warning.contains("orbit-web.service"),
        "{config}"
    );
    assert!(!warning.contains("current shell"), "{config}");
    let output = fixture
        .command(&set)
        .env("PATH", &path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["check"] == "env-pass")
        .unwrap();
    assert_eq!(row["status"], "warning", "{row}");
    assert!(
        row["message"].as_str().unwrap().contains(CREW_SOURCE),
        "{row}"
    );
    for bytes in [
        &config_output.stdout,
        &config_output.stderr,
        &output.stdout,
        &output.stderr,
    ] {
        assert!(
            !String::from_utf8_lossy(bytes).contains(SECRET),
            "credential value escaped"
        );
    }
    // The CLI process is a second service stand-in that holds the source.
    // Only this process's env differs; no timing or live host service is needed.
    fs::write(&systemctl, script("$PPID")).unwrap();
    let output = fixture
        .command(&set)
        .env("PATH", &path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["check"] == "env-pass")
        .unwrap();
    assert_eq!(row["status"], "ok", "{row}");
    // A stopped service cannot be inferred from an exported shell source.
    fs::write(&systemctl, script("0")).unwrap();
    let output = fixture
        .command(&set)
        .env("PATH", &path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["check"] == "env-pass")
        .unwrap();
    assert_eq!(row["status"], "warning", "{row}");
    assert!(
        row["message"].as_str().unwrap().contains("cannot verify"),
        "{row}"
    );
}

#[cfg(unix)]
#[test]
fn sustained_pool_warning_reaches_run_show_and_readiness_text() {
    if crate::isolated_test::run_in_child(
        "env_pass_warning::crew_readiness::sustained_pool_warning_reaches_run_show_and_readiness_text",
    ) {
        return;
    }
    use chrono::{Duration, Utc};
    use orbit_types::workflow::{CrewPoolExclusion, PipelineState};
    use serde_json::json;

    let fixture = Fixture::new();
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &fixture.home.join(".orbit"),
        &fixture.repo.join(".orbit"),
    )
    .unwrap();
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({})),
            None,
        )
        .unwrap();
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
    state.pull_drain.crew_pool_exclusions.insert(
        "sol-credits".into(),
        CrewPoolExclusion {
            since: Utc::now() - Duration::minutes(3),
            last_observed_at: Utc::now(),
            passes: 4,
            reason: format!("crew `sol-credits` env source {CREW_SOURCE}"),
        },
    );
    runtime.write_run_state(&run.run_id, &state).unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    for args in [
        vec!["run", "show", run.run_id.as_str(), "--no-reconcile"],
        vec!["run", "readiness"],
    ] {
        let output = fixture.command(&[]).args(&args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains("Warning:") && text.contains("sol-credits") && text.contains(CREW_SOURCE),
            "{args:?}: {text}"
        );
        assert!(!text.contains(SECRET));
    }
}
