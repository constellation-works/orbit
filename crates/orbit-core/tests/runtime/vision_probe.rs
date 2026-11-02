//! `orbit agents vision-probe --crew <able> --crew <incapable>` refuses the
//! whole probe before any canary run is submitted, even though the able crew
//! is named first. Each refusal names the incapable executor it found.
//! Finished probe rows require their own canary check, regardless of other
//! runs for the same crew or the pipeline status.

use orbit_core::application::job::JobRunListParams;
use orbit_core::{OrbitError, OrbitRuntime};
use tempfile::TempDir;

/// The shipped canary job, so a submitted probe run resolves in the fixture.
const PROBE_JOB_YAML: &str = include_str!("../../assets/jobs/image_input_probe_pipeline.yaml");

/// Claude declares `spec.image_input` and has a CLI runtime.
const ABLE: &str = r#"
[workflow]
default_crew = "able"

[crews.able]
provider = "claude"
model = "fixture-able"
"#;

/// Each incapable crew, the executor its refusal names, and the reason fragment.
const INCAPABLE: [(&str, &str, &str); 3] = [
    (
        r#"
[crews.blind]
provider = "codex"
model = "fixture-blind"
vision = false
"#,
        "codex",
        "sets `vision = false`",
    ),
    (
        r#"
[crews.blind]
provider = "gemini"
model = "fixture-blind"
"#,
        "gemini",
        "declares no `spec.image_input`",
    ),
    (
        r#"
[crews.blind]
provider = "openai_compat"
model = "fixture-blind"
"#,
        "openai_compat",
        "has no CLI runtime",
    ),
];

#[test]
fn explicit_able_then_incapable_crew_submits_no_probe_run() {
    for (incapable, executor, reason) in INCAPABLE {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        // A submitted canary run spawns this substitute worker, which exits
        // once the root is removed, so a regression cannot reach a provider.
        crate::worker_fixture::install(root.path(), "removed");
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::write(
            jobs_dir.join("image_input_probe_pipeline.yaml"),
            PROBE_JOB_YAML,
        )
        .unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(
            repo.join(".orbit/config.toml"),
            format!("{ABLE}{incapable}"),
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();

        let error = runtime
            .vision_probe(&["able".to_string(), "blind".to_string()], 1)
            .expect_err("a named crew that cannot take images must refuse the probe");
        let OrbitError::InvalidInput(message) = &error else {
            panic!("expected InvalidInput, got {error:?}");
        };
        let prefix = format!("image_input_unavailable: executor `{executor}`:");
        assert!(
            message.starts_with(&prefix) && message.contains(reason),
            "refusal should name the incapable executor and its reason: {message}"
        );

        let runs = runtime.list_job_runs(JobRunListParams::default()).unwrap();
        assert!(
            runs.is_empty(),
            "refused probe submitted {} pipeline run(s) before failing",
            runs.len()
        );
    }
}

#[cfg(unix)]
mod outcomes {
    use chrono::Utc;
    use orbit_core::OrbitRuntime;
    use orbit_core::application::vision_probe::VisionProbeResult;
    use orbit_core::test_support::{
        RUN_ID_PLACEHOLDER, install_substitute_pipeline_worker, pipeline_worker_observer_completed,
    };
    use orbit_store::V2AuditEventInsertParams;
    use orbit_types::workflow::{
        AUDIT_ENVELOPE_SCHEMA_VERSION, ImageInputMode, JobRunState, V2AuditEnvelope, V2AuditEvent,
        V2AuditEventKind,
    };
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    const WORKER: &str = "vision_probe::outcomes::settle_probe_worker";

    #[test]
    fn reports_only_its_own_canary_check() {
        if !crate::dispatch_admission::isolated(
            "vision_probe::outcomes::reports_only_its_own_canary_check",
        ) {
            return;
        }
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        let jobs = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            jobs.join("image_input_probe_pipeline.yaml"),
            super::PROBE_JOB_YAML,
        )
        .unwrap();
        let cases = [
            ("missing_success", VisionProbeResult::Failed),
            ("missing_failed", VisionProbeResult::Failed),
            ("checked_miss", VisionProbeResult::ImageNotSeen),
            ("checked_seen", VisionProbeResult::Seen),
            ("checked_failed", VisionProbeResult::Failed),
        ];
        let mut config = String::from("[workflow]\ndefault_crew = \"missing_success\"\n");
        for (crew, _) in &cases {
            config.push_str(&format!(
                "\n[crews.{crew}]\nprovider = \"claude\"\nmodel = \"fixture\"\n"
            ));
        }
        std::fs::write(workspace.join("config.toml"), config).unwrap();
        orbit_common::test_env::assert_child_test_exists(WORKER);
        install_substitute_pipeline_worker([
            "env".to_string(),
            format!("ORBIT_TEST_PROBE_ROOT={}", root.path().display()),
            format!("ORBIT_TEST_PROBE_RUN={RUN_ID_PLACEHOLDER}"),
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "--exact".to_string(),
            WORKER.to_string(),
            "--ignored".to_string(),
            "--nocapture".to_string(),
        ]);
        let runtime = OrbitRuntime::from_roots(&global, &workspace)
            .unwrap()
            .with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm());
        let crews: Vec<_> = cases.iter().map(|(crew, _)| crew.to_string()).collect();
        let report = runtime.vision_probe(&crews, 60).unwrap();
        // Reap every substitute before inspecting results or dropping its roots.
        let deadline = Instant::now() + Duration::from_secs(60);
        for row in &report.rows {
            let run = runtime
                .show_job_run(row.run_id.as_deref().unwrap())
                .unwrap();
            while !pipeline_worker_observer_completed(
                &runtime.paths().repo_root,
                &run.run_id,
                run.pid.unwrap(),
            ) {
                assert!(
                    Instant::now() < deadline,
                    "probe worker did not exit: {run:?}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(report.rows.len(), cases.len());
        let failures: Vec<_> = report
            .rows
            .iter()
            .zip(cases)
            .filter_map(|(row, (crew, expected))| {
                let missing_detail = crew.starts_with("missing_")
                    && !row
                        .detail
                        .as_deref()
                        .is_some_and(|detail| detail.contains("canary check was not found"));
                if row.crew != crew || row.result != expected || missing_detail {
                    Some(format!("{crew}: expected {expected:?}, got {row:?}"))
                } else {
                    None
                }
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    #[ignore = "substitute worker for the public vision_probe regression"]
    fn settle_probe_worker() {
        let root = std::path::PathBuf::from(std::env::var_os("ORBIT_TEST_PROBE_ROOT").unwrap());
        let run_id = std::env::var("ORBIT_TEST_PROBE_RUN").unwrap();
        let runtime =
            OrbitRuntime::from_roots(&root.join("global"), &root.join("repo/.orbit")).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        let run = jobs.get_job_run(&run_id).unwrap().unwrap();
        let crew = run.input.as_ref().unwrap()["crew"].as_str().unwrap();
        jobs.mark_job_run_running(&run_id, Utc::now(), std::process::id())
            .unwrap();
        if crew.starts_with("checked_") {
            write_check(&runtime, &run_id, crew, crew != "checked_miss");
        }
        // A newer concurrent delivery for this crew must never supply this row.
        write_check(
            &runtime,
            &format!("other-{run_id}"),
            crew,
            crew == "checked_miss",
        );
        let status = if crew.ends_with("failed") {
            JobRunState::Failed
        } else {
            JobRunState::Success
        };
        jobs.finalize_job_run(&run_id, status, Utc::now(), Some(0))
            .unwrap();
    }

    fn write_check(runtime: &OrbitRuntime, run_id: &str, crew: &str, seen: bool) {
        let kind = V2AuditEventKind::ImageCanaryChecked {
            step_id: "probe".into(),
            activity_name: "image_input_probe".into(),
            provider: "claude".into(),
            crew: Some(crew.into()),
            mode: ImageInputMode::NativeRead,
            seen,
        };
        let event = V2AuditEvent {
            envelope: V2AuditEnvelope {
                schema_version: AUDIT_ENVELOPE_SCHEMA_VERSION,
                event_type: kind.event_type().into(),
                event_id: format!("evt-{run_id}"),
                ts: Utc::now(),
                run_id: run_id.into(),
                agent_identity: "fixture".into(),
                parent_event_id: None,
                workspace_path: None,
            },
            kind,
        };
        runtime
            .sqlite_store()
            .unwrap()
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: runtime.workspace_id().unwrap(),
                event_id: event.envelope.event_id.clone(),
                source: "v2_envelope".into(),
                schema_version: event.envelope.schema_version,
                event_type: event.envelope.event_type.clone(),
                ts: event.envelope.ts,
                run_id: run_id.into(),
                agent_identity: "fixture".into(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: serde_json::to_string(&event).unwrap(),
            })
            .unwrap();
    }
}
