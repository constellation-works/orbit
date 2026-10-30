//! `orbit agents vision-probe --crew <able> --crew <incapable>` refuses the
//! whole probe before any canary run is submitted, even though the able crew
//! is named first. Each refusal names the incapable executor it found.

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
