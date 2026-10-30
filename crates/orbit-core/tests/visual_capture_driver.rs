//! Regression check: the visual capture driver runs a UI rule only at the
//! widths the rule declares, as the canonical ux-rules runner does.
#![allow(clippy::unwrap_used)]

use std::process::Command;

use serde_json::{Value, json};

const DRIVER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/visual/capture-driver.mjs"
);

// Loads the real driver and runs its per-mock rule loop for each case with
// stub checks that report the width they ran at. Prints the [rule, width]
// pairs per case, which is what rules.json would record for that mock.
// The driver path comes last so the driver's own entry-point guard stays off.
const PROBE: &str = r#"
import { pathToFileURL } from "node:url";
const cases = JSON.parse(process.argv[1]);
const driver = await import(pathToFileURL(process.argv[2]).href);
const out = [];
for (const { rules, mock } of cases) {
  const modules = rules.map((rule) => ({
    rule,
    check: async (ctx) => ({ rule: rule.id, status: "pass", width: ctx.target.width }),
  }));
  const results = await driver.runRulesForMock(modules, { blocked: [] }, mock, null, {}, "");
  out.push(results.map((result) => [result.rule, result.width]));
}
process.stdout.write(JSON.stringify(out));
"#;

fn mock(width: u64, responsive: bool) -> Value {
    json!({
        "artifact": format!("tasks-list-dark-{width}.png"),
        "view": "tasks-list",
        "state": "default",
        "theme": "dark",
        "width": width,
        "view_def": { "id": "tasks-list", "responsive": responsive },
        "app_def": { "name": "orbit-web" },
    })
}

fn rule(id: &str, widths: Option<Value>) -> Value {
    let mut targets = json!({ "views": "*" });
    if let Some(widths) = widths {
        targets["widths"] = widths;
    }
    json!({ "id": id, "targets": targets })
}

fn run_probe(cases: &[Value]) -> Vec<Value> {
    let output = Command::new("node")
        .arg("--input-type=module")
        .arg("-e")
        .arg(PROBE)
        .arg(Value::Array(cases.to_vec()).to_string())
        .arg(DRIVER)
        .output()
        .unwrap_or_else(|error| panic!("node must be on PATH to load the capture driver: {error}"));
    assert!(
        output.status.success(),
        "rule probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn rules_run_only_at_declared_widths() {
    let two_widths = rule("ui.two-widths", Some(json!([1440, 375])));
    let three_widths = rule("ui.three-widths", Some(json!([1440, 1166, 375])));
    let default_width = rule("ui.default-width", None);
    // (label, rules, mock, expected [rule, width] pairs)
    let cases = [
        (
            "undeclared width 1166 is not run",
            vec![two_widths.clone()],
            mock(1166, true),
            json!([]),
        ),
        (
            "declared width 1440 is run",
            vec![two_widths.clone()],
            mock(1440, true),
            json!([["ui.two-widths", 1440]]),
        ),
        (
            "declared width 375 is run",
            vec![two_widths.clone()],
            mock(375, true),
            json!([["ui.two-widths", 375]]),
        ),
        (
            "width 1166 is run when the rule declares it",
            vec![three_widths],
            mock(1166, true),
            json!([["ui.three-widths", 1166]]),
        ),
        (
            "omitted widths default to 1440, so 375 is not run",
            vec![default_width.clone()],
            mock(375, true),
            json!([]),
        ),
        (
            "omitted widths default to 1440, which is run",
            vec![default_width],
            mock(1440, true),
            json!([["ui.default-width", 1440]]),
        ),
        (
            "non-responsive view is not run at a width past its first declared width",
            vec![two_widths.clone()],
            mock(375, false),
            json!([]),
        ),
        (
            "non-responsive view is run at its first declared width",
            vec![two_widths],
            mock(1440, false),
            json!([["ui.two-widths", 1440]]),
        ),
    ];
    let payload: Vec<Value> = cases
        .iter()
        .map(|(_, rules, mock, _)| json!({ "rules": rules, "mock": mock }))
        .collect();
    let recorded = run_probe(&payload);
    assert_eq!(recorded.len(), cases.len());
    for ((label, _, _, expected), actual) in cases.iter().zip(&recorded) {
        assert_eq!(actual, expected, "{label}");
    }
}
