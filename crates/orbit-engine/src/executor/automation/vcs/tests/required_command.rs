//! Missing-tool classification is combinatorial pure logic over shell,
//! `make`, cargo and guardrail diagnostics; a wrong answer either repairs code
//! that has no defect or hides a real failure behind "install a tool"
//! [ORB-13987].

use super::super::required_command::missing_tool;

const EMPTY_PATH: &str = "/nonexistent-orbit-path";

fn tool(exit_code: i32, output: &str) -> Option<Option<String>> {
    missing_tool(Some(exit_code), output, EMPTY_PATH).map(|missing| missing.tool)
}

#[test]
fn shell_and_launcher_diagnostics_name_the_missing_tool() {
    for (exit_code, output, expected) in [
        (127, "/bin/sh: 1: rg: not found", "rg"),
        (127, "bash: line 1: cargo: command not found", "cargo"),
        (
            2,
            "scripts/ci.sh: line 21: jq: command not found\nmake: *** [ci] Error 2",
            "jq",
        ),
        (127, "zsh: command not found: node", "node"),
        (
            2,
            "make: taplo: No such file or directory\nmake: *** [fmt] Error 127",
            "taplo",
        ),
        (127, "env: 'python3': No such file or directory", "python3"),
        (101, "error: no such command: `nextest`", "cargo-nextest"),
        (
            2,
            "ci-guardrails: ripgrep (rg) is required; install it before running\nmake: *** [ci-fast] Error 1",
            "rg",
        ),
        (
            1,
            "Error: jq is required. Install with: brew install jq",
            "jq",
        ),
    ] {
        assert_eq!(
            tool(exit_code, output),
            Some(Some(expected.to_string())),
            "{output}"
        );
    }
}

#[test]
fn an_unnamed_exit_127_is_still_an_environment_failure() {
    assert_eq!(tool(127, "something went wrong"), Some(None));
    assert_eq!(
        tool(2, "make: *** [Makefile:3: ci] Error 127"),
        Some(None),
        "make reports a recipe whose command was not found"
    );
}

#[test]
fn ordinary_check_failures_are_candidate_failures() {
    for (exit_code, output) in [
        (
            1,
            "test tests::parses ... FAILED\nassertion failed: config.toml: not found",
        ),
        (1, "error: field `name` is required"),
        (2, "src/feature.txt: not formatted"),
        (
            101,
            "thread 'main' panicked at src/lib.rs:3:5:\nworkspace_path is required",
        ),
        (1, "cat: missing.txt: No such file or directory"),
    ] {
        assert_eq!(tool(exit_code, output), None, "{output}");
    }
}

#[test]
fn a_guardrail_for_a_tool_on_path_is_not_a_missing_tool() {
    let output = "check: sh (sh) is required; install it before running";
    assert!(missing_tool(Some(1), output, "/bin:/usr/bin").is_none());
    assert!(missing_tool(Some(1), output, EMPTY_PATH).is_some());
}
