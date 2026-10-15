use serde_json::json;

use super::claimed_base_ref;
use crate::context::ClaimExecutionContext;

// Admission: combinatorial pure ref-selection logic. The public claim action
// refuses invalid sync modes during observation, before its red-base hold
// path; it cannot expose this helper's former silent error-to-local fallback.
#[test]
fn claimed_base_ref_preserves_valid_refs_and_propagates_invalid_modes() {
    for ship_mode in ["local", "pr", "unknown"] {
        let context = ClaimExecutionContext {
            footprint: vec![],
            repair: None,
            workspace_id: String::new(),
            task_id: String::new(),
            claim_id: String::new(),
            machine_id: String::new(),
            run_id: String::new(),
            ship_mode: ship_mode.into(),
            base_branch: "origin/main".into(),
            landing_branch: "main".into(),
            required_commands: vec![],
        };
        for (input, expected) in [
            (json!({"base_sync": " local "}), Some("main")),
            (json!({"base_sync": "remote"}), Some("origin/main")),
            (json!({"base_sync": ""}), Some("origin/main")),
            (json!({"base_sync": "unknown"}), None),
            (
                json!({}),
                match ship_mode {
                    "local" => Some("main"),
                    "pr" => Some("origin/main"),
                    _ => None,
                },
            ),
        ] {
            let result = claimed_base_ref(&context, &input);
            match expected {
                Some(expected) => assert_eq!(result.unwrap(), expected),
                None => assert!(
                    result.is_err(),
                    "invalid input must not select a local base"
                ),
            }
        }
    }
}
