//! Native audit persistence across writable and compatible foreign generations.

use super::{Fixture, run_in_child};
use std::fs;

use orbit_common::fs::generation::{Access, GenerationGuard, Participant, ParticipantRole};
use orbit_common::test_env;
use rusqlite::Connection;
use serde_json::{Value, json};

#[test]
fn audit_cli_native_reads_persist_once_in_writable_and_foreign_generations() {
    const TEST: &str = "audit_cli::native_reads::audit_cli_native_reads_persist_once_in_writable_and_foreign_generations";
    if run_in_child(TEST, |_| {}) {
        return;
    }
    let fixture = Fixture::new();
    let workspace = fixture.json(&["workspace", "show", "--format", "json"]);
    assert_eq!(workspace["registered"], true);
    assert_eq!(
        workspace["checkout"]["repo_root"],
        fixture.repo.canonicalize().unwrap().to_str().unwrap()
    );
    let task = fixture.json(&[
        "task",
        "add",
        "--title",
        "Native read audit",
        "--complexity",
        "low",
        "--json",
    ]);
    let id = task["id"].as_str().unwrap();
    let artifact = json!({"ready": true, "detail": "native audit fixture"});
    fixture
        .command(&[
            "task",
            "update",
            id,
            "--artifact",
            &format!("deployment/readiness.json={artifact}"),
        ])
        .assert()
        .success();
    let commands = [
        vec!["task", "artifact", "get", id, "deployment/readiness.json"],
        vec!["task", "show", id, "--fields", "status", "--json"],
        vec!["task", "artifact", "get", id, "deployment/missing.json"],
    ];
    let identity = orbit_core::composition::compiled_compatibility();
    let digest = "a".repeat(64);
    let mut writable_outputs = Vec::new();
    for foreign in [false, true] {
        let _record = foreign.then(|| GenerationGuard::acquire(&fixture.root, &digest).unwrap());
        let _generation = foreign.then(|| {
            GenerationGuard::join(
                &fixture.root,
                &Participant {
                    digest: &digest,
                    identity: &identity,
                    role: ParticipantRole::Dashboard,
                    access: Access::Write,
                    handover: None,
                    in_activity: false,
                },
                std::time::Duration::ZERO,
                || Ok(identity.store_schema.version),
            )
            .unwrap()
        });
        let generation_before = fs::read(fixture.root.join(".generation.lock")).unwrap();
        let db = Connection::open_with_flags(
            fixture.root.join("orbit.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let schema = || {
            db.prepare("SELECT name, sql FROM sqlite_master ORDER BY name")
                .unwrap()
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let schema_before = schema();
        for (index, args) in commands.iter().enumerate() {
            let before = fixture.json(&["audit", "list", "--limit", "1000", "--json"]);
            let latest = before
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["id"].as_i64().unwrap())
                .max()
                .unwrap_or(0);
            let output = fixture
                .command(args)
                .timeout(test_env::FIXTURE_STEP_DEADLINE)
                .output()
                .unwrap();
            let rows = fixture.json(&["audit", "list", "--limit", "1000", "--json"]);
            // Count every new row, including accidental tool-dispatch duplicates.
            let rows: Vec<_> = rows
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["id"].as_i64().unwrap() > latest)
                .collect();
            assert_eq!(
                rows.len(),
                1,
                "exactly one durable native audit row (foreign={foreign}, args={args:?}, output={output:?}): {rows:?}"
            );
            let row = rows[0];
            assert_eq!(row["command"], "task");
            assert_eq!(row["exit_code"], output.status.code().unwrap());
            if index < 2 {
                assert!(output.status.success(), "{output:?}");
                assert!(output.stderr.is_empty(), "{output:?}");
                assert_eq!(row["status"], "success");
                assert!(row["error_message"].is_null());
                let value: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(
                    value,
                    if index == 0 {
                        artifact.clone()
                    } else {
                        task["status"].clone()
                    }
                );
            } else {
                assert_eq!(output.status.code(), Some(1));
                assert!(output.stdout.is_empty());
                let message = String::from_utf8_lossy(&output.stderr);
                assert!(message.contains("deployment/missing.json"), "{message}");
                assert!(
                    !message.contains("failed to write audit event"),
                    "{message}"
                );
                assert_eq!(row["status"], "failure");
                assert!(
                    message.contains(row["error_message"].as_str().unwrap()),
                    "{row}"
                );
            }
            assert_eq!(
                fixture.json(&["audit", "show", &row["id"].to_string(), "--json",]),
                *row
            );
            if foreign {
                let prior: &std::process::Output = &writable_outputs[index];
                assert_eq!(output.status.code(), prior.status.code());
                assert_eq!(output.stdout, prior.stdout);
                assert_eq!(output.stderr, prior.stderr);
            } else {
                writable_outputs.push(output);
            }
        }
        assert_eq!(
            schema(),
            schema_before,
            "audit appends must never migrate schema"
        );
        assert_eq!(
            fs::read(fixture.root.join(".generation.lock")).unwrap(),
            generation_before,
            "native reads must not replace the admitted generation"
        );
    }
}
