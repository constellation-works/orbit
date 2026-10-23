//! Provider launcher lookup refuses parent-directory traversal in the
//! configured program while explicit and bare launchers still resolve.

use tempfile::tempdir;

use super::cli_support::{TestHost, test_agent_loop_spec_for, write_executable};
use orbit_engine::activity_job::cli_runner::{locate_provider_launcher, run_cli_backend};

#[test]
fn launcher_lookup_refuses_parent_directory_traversal() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::launcher_lookup_refuses_parent_directory_traversal"
    )) {
        return;
    }

    let root = tempdir().expect("tempdir");
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).expect("bin dir");
    let launcher = bin.join("fake-agent");
    write_executable(&launcher, "#!/bin/sh\n");

    let explicit = launcher.to_string_lossy().into_owned();
    assert_eq!(
        locate_provider_launcher(&explicit, None),
        Some(launcher.clone()),
        "an explicit launcher path still resolves"
    );
    assert_eq!(
        locate_provider_launcher("bin/fake-agent", Some(root.path())),
        Some(launcher.clone()),
        "a relative explicit launcher still resolves against the dispatch cwd"
    );
    let traversing_cwd = bin.join("..");
    assert_eq!(
        locate_provider_launcher("bin/fake-agent", Some(&traversing_cwd)),
        None,
        "the final lookup must refuse traversal supplied by cwd"
    );
    assert_eq!(
        locate_provider_launcher(&explicit, Some(&traversing_cwd)),
        Some(launcher.clone()),
        "an absolute launcher does not depend on cwd"
    );

    let traversal = bin.join("..").join("bin").join("fake-agent");
    let traversal = traversal.to_string_lossy().into_owned();
    for program in [traversal.as_str(), "..", "../bin/fake-agent"] {
        assert_eq!(
            locate_provider_launcher(program, Some(&bin)),
            None,
            "lookup must not resolve {program:?}"
        );
        let host = TestHost::with_command(program.to_string());
        let error = run_cli_backend(
            &host,
            &test_agent_loop_spec_for("codex", std::time::Duration::from_secs(5)),
            "lookup",
            "lookup",
            std::sync::Arc::new(orbit_engine::V2AuditWriter::new(
                "lookup",
                "codex:test",
                std::sync::Arc::new(orbit_agent::loop_engine::audit::NullSink),
            )),
            &serde_json::json!({"prompt":"lookup", "workspace_path":bin}),
            None,
        )
        .expect_err("dispatch must refuse a traversing launcher");
        assert!(
            error.is_non_retryable(),
            "a traversing launcher is a permanent configuration error"
        );
    }
}
