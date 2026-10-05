use orbit_core::OrbitRuntime;

use super::super::CommandOutput;
use super::super::doctor::fs_access;
use crate::output::payload::{Block, View};

fn payload_parts(output: CommandOutput) -> (serde_json::Value, View) {
    let CommandOutput::Payload(payload) = output else {
        panic!("doctor diagnostics must return a payload");
    };
    payload.into_view()
}

#[test]
fn fs_access_reports_the_shipped_policy_verdicts_for_workspace_relative_paths() {
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");
    for (path, read, modify) in [
        ("src/main.rs", true, true),
        (".orbit/tmp/x", true, true),
        (".orbit/tasks/x", true, false),
        ("src/.env", false, false),
    ] {
        let (json, view) =
            payload_parts(fs_access(&runtime, "implementer", path).expect("fs-access dry-run"));
        assert_eq!(json["policy"], "default", "{json}");
        assert_eq!(json["profile"], "implementer", "{json}");
        assert_eq!(json["path"], path, "{json}");
        assert_eq!(json["read"]["allowed"], read, "{path}: {json}");
        assert_eq!(json["modify"]["allowed"], modify, "{path}: {json}");
        assert!(json["read"]["matched_rule"].is_string(), "{json}");
        let View::Blocks(blocks) = view else {
            panic!("human detail blocks");
        };
        let expected = format!("modify:  {}", if modify { "allowed" } else { "denied" });
        assert!(
            blocks
                .iter()
                .any(|block| matches!(block, Block::Text(text) if text.contains(&expected))),
            "{path}: human output must carry the modify verdict"
        );
    }
}
