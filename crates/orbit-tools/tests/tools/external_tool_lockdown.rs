#![allow(missing_docs)]
// External-tool fixtures use expect/unwrap for setup; production code remains linted.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[cfg(unix)]
mod unix_tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use orbit_common::OrbitError;
    use orbit_tools::external::ExternalTool;
    use orbit_tools::{Tool, ToolContext};
    use serde_json::json;
    use tempfile::tempdir;

    fn external_tool(path: &str) -> ExternalTool {
        ExternalTool {
            name: "print_environment".to_string(),
            path: path.to_string(),
            description: String::new(),
            parameters: Vec::new(),
        }
    }

    const CHILD_MARKER: &str = "ORBIT_TEST_EXTERNAL_TOOL_LOCKDOWN_CHILD";
    const SYNTHETIC_GH_TOKEN: &str = "orbit-test-synthetic-gh-token";
    const TEST_NAME: &str = "external_tool_lockdown::unix_tests::external_tool_drops_ambient_credentials_and_keeps_runtime_context";

    #[test]
    fn external_tool_drops_ambient_credentials_and_keeps_runtime_context() {
        if std::env::var_os(CHILD_MARKER).is_none() {
            // Seed the ambient credential only in the re-executed child, so
            // the parallel test runner's own environment is never mutated.
            // The child asserts the token is present before execution; if
            // ExternalTool ever inherited the ambient environment, the tool
            // script below would observe it and the child would fail.
            let directory = tempdir().expect("temporary directory");
            let mut command = std::process::Command::new(
                std::env::current_exe().expect("current test executable"),
            );
            orbit_common::test_env::clear_inherited_authority(|key| {
                command.env_remove(key);
            });
            command
                .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
                .env(CHILD_MARKER, "1")
                .env("GH_TOKEN", SYNTHETIC_GH_TOKEN);
            let output =
                orbit_common::test_env::run_child_test(&mut command, TEST_NAME, directory.path());
            orbit_common::test_env::assert_child_test_passed(
                TEST_NAME,
                output.status,
                output.stdout,
                output.stderr,
            );
            return;
        }
        assert_eq!(
            std::env::var("GH_TOKEN").expect("synthetic GH_TOKEN seeded in child"),
            SYNTHETIC_GH_TOKEN,
            "the child must hold the ambient credential before execution"
        );
        let directory = tempdir().expect("temporary directory");
        let script_path = directory.path().join("print-environment.sh");
        fs::write(
            &script_path,
            r##"#!/bin/sh
cat >/dev/null
if [ -n "${GH_TOKEN+x}" ]; then gh_token_present=true; else gh_token_present=false; fi
if [ -n "${ORBIT_TOOL_NAME+x}" ]; then name_present=true; else name_present=false; fi
if [ -n "${ORBIT_TOOL_CWD+x}" ]; then cwd_present=true; else cwd_present=false; fi
if [ -n "${ORBIT_TOOL_WORKSPACE_ROOT+x}" ]; then root_present=true; else root_present=false; fi
if [ -n "${ORBIT_TOOL_AGENT_NAME+x}" ]; then agent_present=true; else agent_present=false; fi
if [ -n "${ORBIT_TOOL_MODEL_NAME+x}" ]; then model_present=true; else model_present=false; fi
if [ -n "${ORBIT_TOOL_ALLOWED_TOOLS+x}" ]; then tools_present=true; else tools_present=false; fi
if [ -n "${ORBIT_TOOL_PROC_ALLOWED_PROGRAMS+x}" ]; then programs_present=true; else programs_present=false; fi
printf '{"gh_token_present":%s,"name_present":%s,"cwd_present":%s,"root_present":%s,"agent_present":%s,"model_present":%s,"tools_present":%s,"programs_present":%s}' \
  "$gh_token_present" "$name_present" "$cwd_present" "$root_present" "$agent_present" "$model_present" "$tools_present" "$programs_present"
"##,
        )
        .expect("write environment script");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
            .expect("make environment script executable");

        let program = script_path.to_string_lossy().into_owned();
        let context = ToolContext {
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            workspace_root: Some(directory.path().to_path_buf()),
            agent_name: Some("test-agent".to_string()),
            model_name: Some("test-model".to_string()),
            allowed_tools: vec!["print_environment".to_string()],
            proc_allowed_programs: vec![program.clone()],
            ..ToolContext::default()
        };

        let output = external_tool(&program)
            .execute(&context, json!({}))
            .expect("external tool output");

        assert_eq!(output["gh_token_present"], false);
        assert_eq!(output["name_present"], true);
        assert_eq!(output["cwd_present"], true);
        assert_eq!(output["root_present"], true);
        assert_eq!(output["agent_present"], true);
        assert_eq!(output["model_present"], true);
        assert_eq!(output["tools_present"], true);
        assert_eq!(output["programs_present"], true);
    }

    #[test]
    fn activity_scoped_external_tool_must_be_allowlisted() {
        let directory = tempdir().expect("temporary directory");
        let context = ToolContext {
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            proc_spawn_activity_scoped: true,
            ..ToolContext::default()
        };

        let error = external_tool("/bin/sh")
            .execute(&context, json!({}))
            .expect_err("unallowlisted external tool must be denied");

        assert!(matches!(error, OrbitError::PolicyDenied(_)));
    }

    #[test]
    fn activity_scoped_allowlisted_external_tool_runs() {
        let directory = tempdir().expect("temporary directory");
        let script_path = directory.path().join("print-ok.sh");
        fs::write(
            &script_path,
            r##"#!/bin/sh
cat >/dev/null
printf '{"ok":true}'
"##,
        )
        .expect("write activity-scoped script");
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
            .expect("make activity-scoped script executable");

        let program = script_path.to_string_lossy().into_owned();
        let context = ToolContext {
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            proc_allowed_programs: vec![program.clone()],
            proc_spawn_activity_scoped: true,
            ..ToolContext::default()
        };

        let output = external_tool(&program)
            .execute(&context, json!({}))
            .expect("allowlisted activity-scoped external tool must run");

        assert_eq!(output["ok"], true);
    }
}
