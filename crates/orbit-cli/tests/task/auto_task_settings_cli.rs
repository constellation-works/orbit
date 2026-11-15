//! Operator settings on bundled auto-tasks stay outside the managed body, so
//! the body keeps refreshing; settings-only forks migrate back under
//! management, and body forks are preserved and reported [ORB-14909].

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::security::release::sha256_hex;
use orbit_core::application::automation::{consumer_key, evaluate_auto_task};
use orbit_types::workflow::automation::{Delivery, SourceRevision, UNATTRIBUTED_NO_LANDING_TASK};
use serde_json::{Value, json};

use crate::isolated_cli_fixture::Fixture;

const SETTINGS_FILE: &str = ".orbit-auto-task-settings.json";
const MANIFEST_FILE: &str = ".orbit-managed-assets.json";

fn definition_path(fixture: &Fixture, name: &str) -> PathBuf {
    let shown = fixture.json(&["auto-task", "show", name, "--json"]);
    PathBuf::from(shown["definition_source"]["path"].as_str().unwrap())
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Rewrite the managed manifest's digest for `name`; `None` drops the entry,
/// leaving the file as untracked as an older hand-merged fork.
fn set_manifest_digest(dir: &Path, name: &str, digest: Option<String>) {
    let path = dir.join(MANIFEST_FILE);
    let mut manifest = read_json(&path);
    let assets = manifest["assets"].as_object_mut().unwrap();
    match digest {
        Some(digest) => assets.insert(name.to_string(), Value::String(digest)),
        None => assets.remove(name),
    };
    fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
}

fn sync_outcome(sync: &Value, name: &str) -> String {
    sync["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| action["kind"] == "auto_task" && action["name"] == name)
        .map(|action| action["outcome"].as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn auto_task_doctor_row(fixture: &Fixture) -> Value {
    // Other checks may fail in a disposable home; only the row matters here.
    let output = fixture.command(&["doctor", "--json"]).output().unwrap();
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "doctor JSON: {error}; {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    rows.into_iter()
        .find(|row| row["check"] == "artifacts-auto-tasks")
        .expect("artifacts-auto-tasks row")
}

fn template_flags_preserve_unnamed_fields_and_full_tool_templates_still_replace() {
    let fixture = Fixture::new();
    let added = fixture.json(&[
        "auto-task",
        "add",
        "--name",
        "field-patch",
        "--every-minutes",
        "60",
        "--title",
        "Original title",
        "--body",
        "Retained body",
        "--criterion",
        "Retain unnamed fields",
        "--type",
        "chore",
        "--tag",
        "retained-tag",
        "--required-tools",
        "orbit.task.show",
        "--complexity",
        "low",
        "--crew",
        "opus",
        "--status",
        "backlog",
        "--json",
    ]);
    let mut expected = added["template"].clone();
    expected["title"] = Value::String("Updated title".into());
    let title = fixture.json(&[
        "auto-task",
        "update",
        "field-patch",
        "--title",
        "Updated title",
        "--json",
    ]);
    assert_eq!(title["template"], expected);

    expected["priority"] = Value::String("high".into());
    let priority = fixture.json(&[
        "auto-task",
        "update",
        "field-patch",
        "--priority",
        "high",
        "--json",
    ]);
    assert_eq!(priority["template"], expected);
    let shown = fixture.json(&["auto-task", "show", "field-patch", "--json"]);
    assert_eq!(shown["template"], expected);

    // An explicitly empty required-tools flag clears it without changing the
    // remaining fields. Whole-template tool input retains its older contract.
    expected.as_object_mut().unwrap().remove("required_tools");
    let cleared = fixture.json(&[
        "auto-task",
        "update",
        "field-patch",
        "--required-tools",
        "",
        "--json",
    ]);
    assert_eq!(cleared["template"], expected);
    let replaced = fixture.json(&[
        "tool",
        "run",
        "orbit.auto_task",
        "--input",
        r#"{"action":"update","name":"field-patch","template":{"title":"Replacement title"}}"#,
        "--json",
    ]);
    let template = &replaced["template"];
    assert_eq!(template["title"], "Replacement title");
    assert_eq!(template["description"], "");
    assert_eq!(template["acceptance_criteria"], serde_json::json!([]));
    assert_eq!(template["tags"], serde_json::json!([]));
    assert_eq!(template["priority"], "medium");
    assert!(template["crew"].is_null());
    assert!(template["complexity"].is_null());
}

fn bundled_auto_task_settings_stay_outside_the_body_and_survive_a_body_refresh() {
    let fixture = Fixture::new();
    let path = definition_path(&fixture, "security-review");
    let dir = path.parent().unwrap().to_path_buf();
    let bundled = fs::read(&path).unwrap();

    fixture.json(&["auto-task", "toggle", "security-review", "on", "--json"]);
    fixture.json(&[
        "auto-task",
        "update",
        "security-review",
        "--crew",
        "opus",
        "--json",
    ]);

    assert_eq!(
        fs::read(&path).unwrap(),
        bundled,
        "settings edits must leave the managed body byte-identical"
    );
    let settings = read_json(&dir.join(SETTINGS_FILE));
    let entry = &settings["definitions"]["security-review"];
    assert_eq!(entry["enabled"], true);
    assert_eq!(entry["crew"], "opus");
    let shown = fixture.json(&["auto-task", "show", "security-review", "--json"]);
    assert_eq!(shown["enabled"], true);
    assert_eq!(shown["template"]["crew"], "opus");
    assert_eq!(shown["layering"]["body"], "managed");

    // Simulate an upgrade: the on-disk body and its recorded digest belong to
    // an older release, so the current bundled body is a new rendered digest.
    let older =
        String::from_utf8(bundled.clone())
            .unwrap()
            .replacen("title: ", "title: Older release ", 1);
    fs::write(&path, &older).unwrap();
    set_manifest_digest(&dir, "security-review", Some(sha256_hex(older.as_bytes())));

    let sync = fixture.json(&["workspace", "sync", "--json"]);
    assert_eq!(sync_outcome(&sync, "security-review"), "refreshed");
    assert_eq!(fs::read(&path).unwrap(), bundled);

    let shown = fixture.json(&["auto-task", "show", "security-review", "--json"]);
    assert_eq!(shown["enabled"], true);
    assert_eq!(shown["template"]["crew"], "opus");
    let minted = fixture.json(&["auto-task", "mint", "security-review", "--json"]);
    assert_eq!(
        minted["crew"], "opus",
        "settings apply to the next minted task"
    );
}

fn settings_only_fork_migrates_and_a_body_fork_is_preserved_and_reported() {
    let fixture = Fixture::new();
    let settings_fork = definition_path(&fixture, "doc-duties");
    let dir = settings_fork.parent().unwrap().to_path_buf();
    let bundled = fs::read_to_string(&settings_fork).unwrap();

    // The shape the on-call operator found: an untracked file that enabled
    // the default and re-crewed it, colliding with the bundled name.
    let forked = bundled
        .replacen("enabled: false", "enabled: true", 1)
        .replacen(
            "  complexity: low\n",
            "  complexity: low\n  crew: opus\n",
            1,
        );
    assert_ne!(forked, bundled);
    fs::write(&settings_fork, &forked).unwrap();
    set_manifest_digest(&dir, "doc-duties", None);

    let before = auto_task_doctor_row(&fixture);
    assert_eq!(before["status"], "warning");
    let message = before["message"].as_str().unwrap();
    assert!(message.contains("`doc-duties`"), "{message}");
    assert!(message.contains("stale"), "{message}");

    let sync = fixture.json(&["workspace", "sync", "--json"]);
    assert_eq!(sync_outcome(&sync, "doc-duties"), "migrated");
    assert_eq!(fs::read_to_string(&settings_fork).unwrap(), bundled);
    let shown = fixture.json(&["auto-task", "show", "doc-duties", "--json"]);
    assert_eq!(shown["enabled"], true);
    assert_eq!(shown["template"]["crew"], "opus");
    assert_eq!(shown["layering"]["body"], "managed");
    assert_eq!(auto_task_doctor_row(&fixture)["status"], "ok");

    // A changed acceptance criterion is a body edit: preserved, not migrated.
    let body_fork = definition_path(&fixture, "code-review");
    let original = fs::read_to_string(&body_fork).unwrap();
    let edited = original
        .replacen(
            "acceptance_criteria:\n  - ",
            "acceptance_criteria:\n  - Locally reworded. ",
            1,
        )
        .replacen("enabled: false", "enabled: true", 1);
    assert_ne!(edited, original, "fixture must change a criterion");
    fs::write(&body_fork, &edited).unwrap();

    let sync = fixture.json(&["workspace", "sync", "--json"]);
    assert_eq!(sync_outcome(&sync, "code-review"), "preserved");
    assert_eq!(fs::read_to_string(&body_fork).unwrap(), edited);

    let shown = fixture.json(&["auto-task", "show", "code-review", "--json"]);
    assert_eq!(shown["layering"]["body"], "forked");
    assert_eq!(
        shown["layering"]["forked_fields"],
        serde_json::json!(["template.acceptance_criteria"])
    );
    assert_eq!(
        shown["layering"]["settings_fields"],
        serde_json::json!(["enabled"])
    );

    let row = auto_task_doctor_row(&fixture);
    assert_eq!(row["status"], "warning");
    let message = row["message"].as_str().unwrap();
    assert!(message.contains("forked: `code-review`"), "{message}");
    assert!(
        message.contains("template.acceptance_criteria"),
        "{message}"
    );
    let remediation = row["remediation"].as_str().unwrap();
    assert!(
        !remediation.contains("Move") && !remediation.contains("rename"),
        "a body fork's remedy must not discard its settings by moving the file: {remediation}"
    );
}

fn older_shipped_bodies_upgrade_with_settings_and_custom_bodies_stay_forked() {
    let fixture = Fixture::new();
    let sources: Value = serde_json::from_str(include_str!(
        "../fixtures/auto-task-body-history/sources.json"
    ))
    .unwrap();
    // This shipped body was omitted when its successor landed [ORB-17408].
    let doc_duties = sources["doc-duties"]["669d9bbdc91fd9e639394e35c826f26492a5a847"]
        .as_str()
        .unwrap();
    for (name, older) in [
        ("doc-duties", doc_duties),
        (
            "run-failure-patterns",
            include_str!("../fixtures/auto-task-body-history/run-failure-patterns.yaml"),
        ),
        (
            "delivery-code-review",
            include_str!("../fixtures/auto-task-body-history/delivery-code-review.yaml"),
        ),
        (
            "security-review",
            include_str!("../fixtures/auto-task-body-history/security-review.yaml"),
        ),
    ] {
        let path = definition_path(&fixture, name);
        let bundled = fs::read_to_string(&path).unwrap();
        let dir = path.parent().unwrap();
        let mut edited = if name == "doc-duties" {
            older.replacen("  priority: medium", "  priority: high", 1)
        } else {
            older.replacen("crew: system", "crew: opus", 1)
        };
        if name == "delivery-code-review" {
            edited = edited
                .replacen("enabled: false", "enabled: true", 1)
                .replacen("threshold: 3", "threshold: 2", 1)
                .replacen("max_wait_minutes: 360", "max_wait_minutes: 120", 1)
                .replacen("retries: 0", "retries: 1", 1)
                .replacen(
                    "  - delivery-code-review\n",
                    "  - delivery-code-review\n  - os:linux\n",
                    1,
                );
            // CRUD reserialization drops YAML comments and changes formatting;
            // shipped-body recognition must still use the parsed body.
            let definition = orbit_common::protocol::yaml::parse_auto_task_yaml(&edited).unwrap();
            edited = serde_yaml::to_string(&definition).unwrap();
        }
        assert_ne!(edited, older, "fixture must change a setting");
        fs::write(&path, &edited).unwrap();
        // Stale operator copies can be untracked, or carry the old raw digest.
        set_manifest_digest(
            dir,
            name,
            if name == "security-review" {
                Some(sha256_hex(older.as_bytes()))
            } else {
                None
            },
        );

        let row = auto_task_doctor_row(&fixture);
        let message = row["message"].as_str().unwrap();
        assert!(
            message.contains(&format!(
                "`{name}` has a stale shipped body (will upgrade on sync)"
            )),
            "{message}"
        );
        assert!(!message.contains("body fork"), "{message}");
        let preview = fixture
            .command(&["workspace", "sync", "--check", "--json"])
            .assert()
            .code(3)
            .get_output()
            .stdout
            .clone();
        let preview: Value = serde_json::from_slice(&preview).unwrap();
        assert_eq!(sync_outcome(&preview, name), "migrated");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            edited,
            "check mode must preserve the file"
        );
        assert!(
            !dir.join(SETTINGS_FILE).exists()
                || read_json(&dir.join(SETTINGS_FILE))["definitions"][name].is_null()
        );

        let sync = fixture.json(&["workspace", "sync", "--json"]);
        assert_eq!(sync_outcome(&sync, name), "migrated");
        assert_eq!(fs::read_to_string(&path).unwrap(), bundled);
        let shown = fixture.json(&["auto-task", "show", name, "--json"]);
        assert_eq!(shown["layering"]["body"], "managed");
        if name == "doc-duties" {
            assert_eq!(shown["layering"]["settings"]["priority"], "high");
            assert_eq!(shown["template"]["priority"], "high");
        } else {
            assert_eq!(shown["layering"]["settings"]["crew"], "opus");
            assert_eq!(shown["template"]["crew"], "opus");
        }
        if name == "delivery-code-review" {
            assert_eq!(shown["enabled"], true);
            let settings = &shown["layering"]["settings"];
            let trigger = &settings["schedule"]["deliveries_landed"];
            assert_eq!(trigger["threshold"], 2);
            assert_eq!(trigger["max_wait_minutes"], 120);
            assert_eq!(trigger["retries"], 1);
            assert_eq!(settings["tags"], serde_json::json!(["os:linux"]));
            assert!(
                settings["complexity"].is_null(),
                "an unchanged old default must not pin the new complexity"
            );
        }
        fixture
            .command(&["auto-task", "show", name])
            .assert()
            .success()
            .stdout(predicates::str::contains("body: managed bundled default"));
        assert_eq!(auto_task_doctor_row(&fixture)["status"], "ok");
    }

    let path = definition_path(&fixture, "run-failure-patterns");
    let old = include_str!("../fixtures/auto-task-body-history/run-failure-patterns.yaml");
    for edited in [
        old.replacen("title: ", "title: Operator body edit ", 1),
        format!("# Operator note must survive\n{old}"),
        old.replacen("  - run-failure-patterns\n", "", 1),
        old.replacen("  crew: system\n", "", 1),
    ] {
        assert_ne!(edited, old);
        fs::write(&path, &edited).unwrap();
        let row = auto_task_doctor_row(&fixture);
        let message = row["message"].as_str().unwrap();
        assert!(
            message.contains("`run-failure-patterns` is a body fork"),
            "{message}"
        );
        assert!(!message.contains("stale shipped body"), "{message}");
        let sync = fixture.json(&["workspace", "sync", "--json"]);
        assert_eq!(sync_outcome(&sync, "run-failure-patterns"), "preserved");
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);
        let shown = fixture.json(&["auto-task", "show", "run-failure-patterns", "--json"]);
        assert_eq!(shown["layering"]["body"], "forked");
    }
}

#[test]
fn stale_run_failure_contract_holds_mint_and_adopt_preserves_customizations() {
    let fixture = Fixture::new();
    let path = definition_path(&fixture, "run-failure-patterns");
    let dir = path.parent().unwrap().to_path_buf();
    let bundled = fs::read_to_string(&path).unwrap();

    // A customized body that dedupes by bare leaf id: the bundled host-aware
    // rule is omitted, while operator title/crew/criterion edits are intended.
    let mut custom = orbit_common::protocol::yaml::parse_auto_task_yaml(&bundled).unwrap();
    custom.template.title = "Operator scan title".to_string();
    custom.template.crew = Some("opus".to_string());
    custom.template.description =
        "Operator custom scan.\n\nDedupe follower settlements by bare leaf id against this host's history; count the rest."
            .to_string();
    custom.template.acceptance_criteria =
        vec!["Operator criterion: count follower settlements once by leaf id.".to_string()];
    fs::write(&path, serde_yaml::to_string(&custom).unwrap()).unwrap();
    set_manifest_digest(&dir, "run-failure-patterns", None);

    let shown = fixture.json(&["auto-task", "show", "run-failure-patterns", "--json"]);
    assert_eq!(shown["layering"]["body"], "forked");
    assert!(
        shown["layering"]["stale_contract"]["contract"]
            .as_str()
            .unwrap()
            .contains("machine_id"),
        "{shown}"
    );
    assert!(
        shown["layering"]["stale_contract"]["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|missing| missing == "task_id"),
        "{shown}"
    );
    let listed = fixture.json(&["auto-task", "list", "--json"]);
    let row = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "run-failure-patterns")
        .unwrap();
    assert!(row["stale_contract"].is_object(), "{row}");
    let doctor = auto_task_doctor_row(&fixture);
    assert!(
        doctor["message"]
            .as_str()
            .unwrap()
            .contains("Stale contract"),
        "{doctor}"
    );

    // Minting is held, not silently minted as a reconciled scan.
    fixture
        .command(&["auto-task", "mint", "run-failure-patterns"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is held"))
        .stderr(predicates::str::contains("--adopt-bundled-contract"));

    // Operator-directed reconciliation appends the bundled rule and keeps
    // every intended customization.
    fixture.json(&[
        "auto-task",
        "update",
        "run-failure-patterns",
        "--adopt-bundled-contract",
        "--json",
    ]);
    let shown = fixture.json(&["auto-task", "show", "run-failure-patterns", "--json"]);
    assert!(shown["layering"]["stale_contract"].is_null(), "{shown}");
    assert_eq!(shown["template"]["title"], "Operator scan title");
    assert_eq!(shown["template"]["crew"], "opus");
    let description = shown["template"]["description"].as_str().unwrap();
    assert!(
        description.contains("Operator custom scan."),
        "{description}"
    );
    assert!(description.contains("machine_id"), "{description}");
    assert!(description.contains("leaf_run_id"), "{description}");
    let criteria = shown["template"]["acceptance_criteria"].clone();
    assert!(
        criteria
            .as_array()
            .unwrap()
            .iter()
            .any(|criterion| criterion.as_str().unwrap().contains("Operator criterion")),
        "{criteria}"
    );
    assert!(
        criteria
            .as_array()
            .unwrap()
            .iter()
            .any(|criterion| criterion.as_str().unwrap().contains("task_ids")),
        "{criteria}"
    );

    // A scan minted after reconciliation carries the triple-match rule, so
    // two settlements sharing a leaf id on different hosts both count.
    let minted = fixture.json(&["auto-task", "mint", "run-failure-patterns", "--json"]);
    assert_eq!(minted["title"], "[auto-task] Operator scan title");
    let minted_body = minted["description"].as_str().unwrap();
    assert!(minted_body.contains("machine_id"), "{minted_body}");
    assert!(minted_body.contains("leaf_run_id"), "{minted_body}");
    assert!(minted_body.contains("task_id"), "{minted_body}");

    // A customized body that already states the rule in its own wording
    // remains usable, and an unrelated fork is not held.
    fixture.json(&[
        "auto-task",
        "update",
        "run-failure-patterns",
        "--title",
        "Compliant custom title",
        "--json",
    ]);
    let minted = fixture.json(&["auto-task", "mint", "run-failure-patterns", "--json"]);
    assert_eq!(minted["title"], "[auto-task] Compliant custom title");

    // The same rule in the operator's own wording is not stale either.
    fixture.json(&[
        "auto-task",
        "update",
        "run-failure-patterns",
        "--body",
        "Count a follower as a duplicate only when host, leaf run id and task id all match; otherwise count it.",
        "--criterion",
        "Dedupe only when host, leaf run id and task id all match.",
        "--json",
    ]);
    let shown = fixture.json(&["auto-task", "show", "run-failure-patterns", "--json"]);
    assert!(shown["layering"]["stale_contract"].is_null(), "{shown}");
    fixture.json(&["auto-task", "mint", "run-failure-patterns", "--json"]);

    let unrelated = definition_path(&fixture, "security-review");
    let mut edited = orbit_common::protocol::yaml::parse_auto_task_yaml(
        &fs::read_to_string(&unrelated).unwrap(),
    )
    .unwrap();
    edited.template.title = "Operator security title".to_string();
    fs::write(&unrelated, serde_yaml::to_string(&edited).unwrap()).unwrap();
    let minted = fixture.json(&["auto-task", "mint", "security-review", "--json"]);
    assert_eq!(minted["title"], "[auto-task] Operator security title");
}

const RUN_FAILURE: &str = "run-failure-patterns";
const DELIVERY_TRIGGER: &str = r#"{"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"landed_code_review_v1","max_items":20,"retries":0}"#;
const UNRELATED_TRIGGER: &str = r#"{"branch":"unrelated-delivery","threshold":1,"max_wait_minutes":60,"coverage":"landed_code_review_v1","max_items":20,"retries":0}"#;

/// Job-step admission, a condition fire and a due delivery all hold a stale
/// run-failure body with the manual mint's error, and mint again once that
/// body carries the bundled contract. An unrelated definition keeps minting.
#[test]
fn stale_run_failure_contract_holds_job_step_and_automation_mints() {
    const TEST: &str =
        "auto_task_settings_cli::stale_run_failure_contract_holds_job_step_and_automation_mints";
    if crate::isolated_test::run_in_child(TEST) {
        return;
    }
    let _authority = orbit_common::test_env::unset(
        orbit_common::test_env::INHERITED_AUTHORITY_ENV
            .iter()
            .copied(),
    );
    let fixture = Fixture::new();
    // `orbit init` selects a default crew from host CLIs; CI may select one
    // that is disabled there, preventing the detached worker from reaching
    // the job-step admission behavior under test.
    crate::fixture_crew::configure_sol(&fixture.root);
    write_stale_run_failure_body(&fixture);
    auto_task_tool(
        &fixture,
        json!({
            "action": "update",
            "name": RUN_FAILURE,
            "dedupe": "always",
            "enabled": true,
        }),
    );
    let shown = show(&fixture, RUN_FAILURE);
    assert_eq!(shown["enabled"], true, "{shown}");
    assert_eq!(shown["dedupe"], "always", "{shown}");
    assert!(shown["layering"]["stale_contract"].is_object(), "{shown}");

    let parked = run_auto_task_step(&fixture, "stale-hold", RUN_FAILURE);
    assert_stale_hold(&parked, "job step");
    assert!(tasks_for(&fixture, RUN_FAILURE).is_empty(), "{parked}");

    let subject = add_subject(&fixture, "Stale condition subject");
    set_condition(&fixture, RUN_FAILURE, "'stale-scan' in task.tags");
    assert_eq!(show(&fixture, RUN_FAILURE)["enabled"], true);
    tick_named(&fixture, RUN_FAILURE);
    assert!(tasks_for(&fixture, RUN_FAILURE).is_empty());
    tag_task(&fixture, &subject, "stale-scan");
    tick_named(&fixture, RUN_FAILURE);
    assert!(
        tasks_for(&fixture, RUN_FAILURE).is_empty(),
        "a stale condition fire must not mint"
    );
    assert_stale_hold(&fire_details(&fixture, RUN_FAILURE), "condition fire");

    set_deliveries(&fixture, RUN_FAILURE, DELIVERY_TRIGGER);
    let git = crate::auto_task_lifecycle_cli::git;
    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    commit_file(&fixture, "baseline.txt", "baseline\n");
    crate::auto_task_lifecycle_cli::publish_origin_if_configured(&fixture);
    let runtime = open_runtime(&fixture);
    evaluate_named(&runtime, RUN_FAILURE);
    land_delivery(
        &fixture,
        &runtime,
        RUN_FAILURE,
        "fixture-delivery",
        "landed.txt",
    );
    let held = evaluate_named(&runtime, RUN_FAILURE);
    assert_stale_hold(&held.reason, "delivery");
    assert!(
        held.state
            .as_ref()
            .and_then(|state| state.active.as_ref())
            .is_none(),
        "a stale delivery must not claim a batch: {}",
        held.reason
    );
    assert!(
        tasks_for(&fixture, RUN_FAILURE).is_empty(),
        "{}",
        held.reason
    );

    fixture.json(&[
        "auto-task",
        "update",
        RUN_FAILURE,
        "--adopt-bundled-contract",
        "--json",
    ]);
    assert!(
        show(&fixture, RUN_FAILURE)["layering"]["stale_contract"].is_null(),
        "adopt must clear the stale contract"
    );
    let runtime = open_runtime(&fixture);
    let minted = evaluate_named(&runtime, RUN_FAILURE);
    let action_id = minted
        .state
        .as_ref()
        .and_then(|state| state.active.as_ref())
        .and_then(|active| active.action_id.clone());
    assert!(
        action_id.is_some(),
        "reconciled delivery should mint: reason={} refusals={:?}",
        minted.reason,
        minted.refusals
    );
    let delivered = tasks_for(&fixture, RUN_FAILURE);
    assert_eq!(delivered.len(), 1, "{delivered:?}");
    assert_contract_present(&fixture, &delivered[0]);

    set_interval(&fixture, RUN_FAILURE);
    let parked = run_auto_task_step(&fixture, "reconciled-scan", RUN_FAILURE);
    assert!(
        parked.contains("minted task"),
        "reconciled job step should mint: {parked}"
    );
    assert!(!parked.contains("is held"), "{parked}");
    assert_eq!(tasks_for(&fixture, RUN_FAILURE).len(), 2);

    let reconciled_subject = add_subject(&fixture, "Reconciled condition subject");
    set_condition(&fixture, RUN_FAILURE, "'reconciled-scan' in task.tags");
    tick_named(&fixture, RUN_FAILURE);
    assert_eq!(tasks_for(&fixture, RUN_FAILURE).len(), 2);
    tag_task(&fixture, &reconciled_subject, "reconciled-scan");
    tick_named(&fixture, RUN_FAILURE);
    let reconciled = tasks_for(&fixture, RUN_FAILURE);
    assert_eq!(reconciled.len(), 3, "{reconciled:?}");
    assert_contract_present(&fixture, reconciled.last().unwrap());

    add_unrelated(&fixture);
    let parked = run_auto_task_step(&fixture, "unrelated-job", "unrelated-scan");
    assert!(
        parked.contains("minted task"),
        "unrelated job step should mint: {parked}"
    );
    assert_eq!(tasks_for(&fixture, "unrelated-scan").len(), 1);

    let unrelated_subject = add_subject(&fixture, "Unrelated condition subject");
    set_condition(&fixture, "unrelated-scan", "'unrelated-edge' in task.tags");
    tick_named(&fixture, "unrelated-scan");
    assert_eq!(tasks_for(&fixture, "unrelated-scan").len(), 1);
    tag_task(&fixture, &unrelated_subject, "unrelated-edge");
    tick_named(&fixture, "unrelated-scan");
    assert_eq!(tasks_for(&fixture, "unrelated-scan").len(), 2);

    set_deliveries(&fixture, "unrelated-scan", UNRELATED_TRIGGER);
    fixture.json(&["auto-task", "toggle", "unrelated-scan", "on", "--json"]);
    git(&fixture, &["checkout", "-b", "unrelated-delivery"]);
    commit_file(&fixture, "unrelated-baseline.txt", "baseline\n");
    crate::auto_task_lifecycle_cli::publish_origin_if_configured(&fixture);
    let runtime = open_runtime(&fixture);
    evaluate_named(&runtime, "unrelated-scan");
    land_delivery(
        &fixture,
        &runtime,
        "unrelated-scan",
        "unrelated-delivery",
        "unrelated-landed.txt",
    );
    let minted = evaluate_named(&runtime, "unrelated-scan");
    assert!(
        minted
            .state
            .as_ref()
            .and_then(|state| state.active.as_ref())
            .and_then(|active| active.action_id.as_ref())
            .is_some(),
        "unrelated delivery should mint: reason={} refusals={:?}",
        minted.reason,
        minted.refusals
    );
    assert_eq!(tasks_for(&fixture, "unrelated-scan").len(), 3);
}

fn write_stale_run_failure_body(fixture: &Fixture) {
    let path = definition_path(fixture, RUN_FAILURE);
    let dir = path.parent().unwrap().to_path_buf();
    let bundled = fs::read_to_string(&path).unwrap();
    let mut custom = orbit_common::protocol::yaml::parse_auto_task_yaml(&bundled).unwrap();
    custom.template.title = "Operator scan title".to_string();
    custom.template.description = "Operator custom scan.\n\nDedupe follower settlements by bare \
         leaf id against this host's history; count the rest."
        .to_string();
    custom.template.acceptance_criteria =
        vec!["Operator criterion: count follower settlements once by leaf id.".to_string()];
    fs::write(&path, serde_yaml::to_string(&custom).unwrap()).unwrap();
    set_manifest_digest(&dir, RUN_FAILURE, None);
}

fn show(fixture: &Fixture, name: &str) -> Value {
    fixture.json(&["auto-task", "show", name, "--json"])
}

fn auto_task_tool(fixture: &Fixture, input: Value) -> Value {
    fixture.json(&[
        "tool",
        "run",
        "orbit.auto_task",
        "--input",
        &input.to_string(),
        "--format",
        "json",
    ])
}

fn set_condition(fixture: &Fixture, name: &str, when: &str) {
    auto_task_tool(
        fixture,
        json!({
            "action": "update",
            "name": name,
            "schedule": {"condition": {"scope": "task", "when": when}},
        }),
    );
}

fn set_interval(fixture: &Fixture, name: &str) {
    auto_task_tool(
        fixture,
        json!({"action": "update", "name": name, "schedule": {"every_minutes": 60}}),
    );
}

fn set_deliveries(fixture: &Fixture, name: &str, trigger: &str) {
    fixture.json(&[
        "auto-task",
        "update",
        name,
        "--deliveries-landed",
        trigger,
        "--json",
    ]);
}

fn add_unrelated(fixture: &Fixture) {
    auto_task_tool(
        fixture,
        json!({
            "action": "add",
            "name": "unrelated-scan",
            "description": "unrelated",
            "schedule": {"every_minutes": 60},
            "template": {
                "title": "Unrelated scan",
                "description": "Unrelated body",
                "acceptance_criteria": ["Unrelated criterion"],
            },
            "dedupe": "always",
        }),
    );
}

fn add_subject(fixture: &Fixture, title: &str) -> String {
    fixture.json(&[
        "task",
        "add",
        "--title",
        title,
        "--complexity",
        "low",
        "--acceptance-criteria",
        "Condition subject",
        "--json",
    ])["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn tag_task(fixture: &Fixture, id: &str, tag: &str) {
    fixture
        .command(&["task", "update", id, "--tag", tag, "--json"])
        .assert()
        .success();
}

fn tasks_for(fixture: &Fixture, name: &str) -> Vec<Value> {
    let listed = fixture.json(&[
        "task",
        "list",
        "--tag",
        &format!("auto-task:{name}"),
        "--json",
    ]);
    listed.as_array().cloned().unwrap_or_else(|| {
        listed
            .get("tasks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("task list: {listed}"))
    })
}

fn tick_named(fixture: &Fixture, name: &str) -> Value {
    let report = fixture.json(&["clock", "tick", "--format", "json"]);
    report["reports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("{name} missing from clock tick: {report}"))
        .clone()
}

fn fire_details(fixture: &Fixture, name: &str) -> String {
    let explained = fixture.json(&["automation", "explain", name, "--json"]);
    let fires = explained["tracking"]["recent_fires"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let text = fires
        .iter()
        .filter_map(|fire| fire["detail"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.is_empty(), "no fire detail: {explained}");
    text
}

fn assert_stale_hold(text: &str, path: &str) {
    assert!(
        text.contains("is held"),
        "{path} must carry the stale-contract hold: {text}"
    );
    assert!(
        text.contains("--adopt-bundled-contract"),
        "{path} must name the reconciliation: {text}"
    );
}

fn assert_contract_present(fixture: &Fixture, task: &Value) {
    let id = task["id"].as_str().unwrap();
    let shown = fixture.json(&["task", "show", id, "--json"]);
    let body = shown["description"].as_str().unwrap_or("");
    assert!(
        body.contains("machine_id") && body.contains("leaf_run_id") && body.contains("task_id"),
        "minted scan must carry the bundled contract: {body}"
    );
}

fn open_runtime(fixture: &Fixture) -> orbit_core::OrbitRuntime {
    use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;

    let roots = RegisteredRuntimeFactory::resolve_roots_for_cwd(&fixture.repo, Some(&fixture.root))
        .unwrap_or_else(|error| panic!("resolve roots: {error}"));
    RegisteredRuntimeFactory::open_resolved_roots(roots)
        .unwrap_or_else(|error| panic!("runtime: {error}"))
        .with_actor(orbit_core::ActorIdentity::human("fixture"))
}

fn runtime_roots(fixture: &Fixture) -> (PathBuf, PathBuf) {
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    (
        fixture.root.clone(),
        PathBuf::from(shown["orbit_root"].as_str().unwrap()),
    )
}

/// Kills and reaps an unrelated bystander process on drop.
struct Bystander(std::process::Child);

impl Drop for Bystander {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only probes for the process; nothing is delivered.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn stalled_worker_pid(guard: &AutoTaskWorkerGuard<'_>) -> u32 {
    guard
        .worker_pid(Duration::from_secs(30))
        .unwrap_or_else(|| panic!("no worker pid for {}", guard.run_id))
}

fn assert_worker_terminated_and_reaped(runtime: &orbit_core::OrbitRuntime, run_id: &str, pid: u32) {
    assert!(
        orbit_core::test_support::pipeline_worker_observer_completed(
            &runtime.paths().repo_root,
            run_id,
            pid
        ),
        "worker {pid} of {run_id} was not reaped"
    );
    assert!(!process_alive(pid), "worker {pid} of {run_id} survived");
}

#[test]
fn stalled_auto_task_worker_is_terminated_and_reaped_on_timeout_and_unwind() {
    const TEST: &str = "auto_task_settings_cli::stalled_auto_task_worker_is_terminated_and_reaped_on_timeout_and_unwind";
    if crate::isolated_test::run_in_child(TEST) {
        return;
    }
    let _authority = orbit_common::test_env::unset(
        orbit_common::test_env::INHERITED_AUTHORITY_ENV
            .iter()
            .copied(),
    );
    let fixture = Fixture::new();
    let (global, workspace) = runtime_roots(&fixture);
    // The stalled worker carries the run marker in its environment only, as
    // the real worker does after `env` execs it.
    install_auto_task_worker_command(
        &global,
        [
            format!(
                "{AUTO_TASK_WORKER_RUN_ENV}={}",
                orbit_core::test_support::RUN_ID_PLACEHOLDER
            ),
            "/bin/sleep".to_string(),
            "600".to_string(),
        ],
    );
    let runtime = orbit_core::OrbitRuntime::from_roots(&global, &workspace)
        .unwrap_or_else(|error| panic!("runtime: {error}"));
    let bystander = Bystander(
        std::process::Command::new("/bin/sleep")
            .arg("600")
            .spawn()
            .unwrap_or_else(|error| panic!("bystander: {error}")),
    );
    let bystander_pid = bystander.0.id();

    // Timeout: the fixture terminates and reaps the stalled worker.
    let path = write_auto_task_job(&fixture, "stalled-timeout", RUN_FAILURE);
    let guard = submit_auto_task_job(&runtime, &global, &path, "stalled-timeout", RUN_FAILURE);
    let run_id = guard.run_id.clone();
    let pid = stalled_worker_pid(&guard);
    assert!(process_alive(pid), "stalled worker {pid} must be running");
    let timed_out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        await_auto_task_park(
            &runtime,
            "stalled-timeout",
            &guard,
            Duration::from_millis(300),
        )
    }));
    assert!(
        timed_out.is_err(),
        "a stalled worker must time out the fixture"
    );
    assert_worker_terminated_and_reaped(&runtime, &run_id, pid);
    drop(guard);

    // Unwind after submission: dropping the guard terminates and reaps.
    let path = write_auto_task_job(&fixture, "stalled-unwind", RUN_FAILURE);
    let unwound_run = std::cell::RefCell::new(None);
    let unwound_pid = std::cell::RefCell::new(None);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let guard = submit_auto_task_job(&runtime, &global, &path, "stalled-unwind", RUN_FAILURE);
        *unwound_run.borrow_mut() = Some(guard.run_id.clone());
        *unwound_pid.borrow_mut() = Some(stalled_worker_pid(&guard));
        panic!("fixture failed after worker submission");
    }));
    assert!(unwound.is_err());
    let unwound_run = unwound_run.into_inner().expect("submitted run");
    let unwound_pid = unwound_pid.into_inner().expect("worker pid");
    assert_worker_terminated_and_reaped(&runtime, &unwound_run, unwound_pid);

    assert!(
        process_alive(bystander_pid),
        "cleanup must not signal unrelated processes"
    );
}

/// Foreground `run_job_v2_from_yaml` refuses auto-task steps. Submit the job
/// the way an operator does, and let this process's substitute worker execute
/// the detached run until the step parks.
const AUTO_TASK_JOB_WORKER: &str = "auto_task_settings_cli::execute_submitted_auto_task_job";

const AUTO_TASK_WORKER_RUN_ENV: &str = "ORBIT_TEST_AUTO_TASK_RUN";

/// How long a submitted auto-task step may take to park before the fixture
/// terminates its worker.
const AUTO_TASK_PARK_TIMEOUT: Duration = Duration::from_secs(90);

fn install_auto_task_job_worker(global: &Path, workspace: &Path) {
    orbit_common::test_env::assert_child_test_exists(AUTO_TASK_JOB_WORKER);
    let exe = std::env::current_exe().unwrap_or_else(|error| panic!("test exe: {error}"));
    install_auto_task_worker_command(
        global,
        [
            format!("ORBIT_TEST_AUTO_TASK_GLOBAL={}", global.display()),
            format!("ORBIT_TEST_AUTO_TASK_WORKSPACE={}", workspace.display()),
            // `env` execs the worker, so the run marker lives in its environment
            // (not argv) and the recorded worker PID is the executing process.
            format!(
                "{AUTO_TASK_WORKER_RUN_ENV}={}",
                orbit_core::test_support::RUN_ID_PLACEHOLDER
            ),
            exe.to_string_lossy().into_owned(),
            "--exact".to_string(),
            AUTO_TASK_JOB_WORKER.to_string(),
            "--ignored".to_string(),
            "--nocapture".to_string(),
        ],
    );
}

/// Records the worker's own PID before `exec`, so the fixture knows the
/// identity of the child it launched even if the worker stalls before it
/// claims its run (the run records a PID only on that claim).
const AUTO_TASK_WORKER_LAUNCH: &str = r#"echo $$ > "$0.tmp" && mv "$0.tmp" "$0" && exec "$@""#;

fn auto_task_worker_pid_file(global: &Path, run_id: &str) -> PathBuf {
    global.join(format!("{run_id}.auto-task-worker.pid"))
}

fn install_auto_task_worker_command(
    global: &Path,
    env_and_command: impl IntoIterator<Item = String>,
) {
    orbit_core::test_support::install_substitute_pipeline_worker(
        [
            "/bin/sh".to_string(),
            "-c".to_string(),
            AUTO_TASK_WORKER_LAUNCH.to_string(),
            auto_task_worker_pid_file(global, orbit_core::test_support::RUN_ID_PLACEHOLDER)
                .to_string_lossy()
                .into_owned(),
            "env".to_string(),
        ]
        .into_iter()
        .chain(env_and_command),
    );
}

#[test]
#[ignore = "entry point for a detached auto-task job step"]
fn execute_submitted_auto_task_job() {
    let global = std::env::var_os("ORBIT_TEST_AUTO_TASK_GLOBAL").expect("worker global root");
    let workspace =
        std::env::var_os("ORBIT_TEST_AUTO_TASK_WORKSPACE").expect("worker workspace root");
    let run = std::env::var("ORBIT_TEST_AUTO_TASK_RUN").expect("worker run id");
    let runtime = orbit_core::OrbitRuntime::from_roots(Path::new(&global), Path::new(&workspace))
        .unwrap_or_else(|error| panic!("worker runtime: {error}"));
    runtime
        .execute_pipeline_run_worker(&run)
        .unwrap_or_else(|error| panic!("worker {run}: {error}"));
}

fn write_auto_task_job(fixture: &Fixture, job: &str, auto_task: &str) -> PathBuf {
    let path = fixture.repo.join(format!("{job}.yaml"));
    fs::write(
        &path,
        format!(
            "schemaVersion: 3\nkind: Job\nmetadata:\n  name: {job}\nspec:\n  state: enabled\n  expire_when: \"now >= job.started_at + duration('24h')\"\n  steps:\n    - id: scan\n      target: auto-task:{auto_task}\n"
        ),
    )
    .unwrap();
    path
}

fn run_auto_task_step(fixture: &Fixture, job: &str, auto_task: &str) -> String {
    let path = write_auto_task_job(fixture, job, auto_task);
    let (global, workspace) = runtime_roots(fixture);
    install_auto_task_job_worker(&global, &workspace);
    let runtime = orbit_core::OrbitRuntime::from_roots(&global, &workspace)
        .unwrap_or_else(|error| panic!("{job} runtime: {error}"));
    let guard = submit_auto_task_job(&runtime, &global, &path, job, auto_task);
    await_auto_task_park(&runtime, job, &guard, AUTO_TASK_PARK_TIMEOUT)
}

/// Submit the job and own its worker from the moment the run exists, so an
/// unwind anywhere after submission still terminates and reaps it.
fn submit_auto_task_job<'a>(
    runtime: &'a orbit_core::OrbitRuntime,
    global: &Path,
    path: &Path,
    job: &str,
    auto_task: &str,
) -> AutoTaskWorkerGuard<'a> {
    let submitted = runtime
        .submit_job_run(
            path.to_str().unwrap_or_else(|| panic!("{job} path")),
            json!({}),
            Some("test"),
        )
        .unwrap_or_else(|error| panic!("{job} ({auto_task}) submit: {error}"));
    AutoTaskWorkerGuard {
        runtime,
        pid_file: auto_task_worker_pid_file(global, &submitted.run_id),
        run_id: submitted.run_id,
    }
}

fn await_auto_task_park(
    runtime: &orbit_core::OrbitRuntime,
    job: &str,
    guard: &AutoTaskWorkerGuard<'_>,
    timeout: Duration,
) -> String {
    let run_id = guard.run_id.as_str();
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(reason) = parked_after_worker_exit(runtime, job, run_id) {
            return reason;
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let reaped = guard.terminate_and_reap();
    let run = runtime
        .show_job_run(run_id)
        .unwrap_or_else(|error| panic!("{job} show {run_id}: {error}"));
    panic!(
        "{job} run {run_id} did not park within {timeout:?} ({:?}); worker terminated and reaped: {reaped}; worker log:\n{}",
        run.state,
        worker_log(runtime, run_id)
    );
}

/// Owns the detached substitute worker of one submitted run. Termination is
/// bound to the PID the launched worker recorded for itself, never to a
/// command-line pattern, so it cannot signal another run's worker. Dropping the
/// guard (including on unwind) terminates and reaps that worker.
struct AutoTaskWorkerGuard<'a> {
    runtime: &'a orbit_core::OrbitRuntime,
    run_id: String,
    pid_file: PathBuf,
}

impl AutoTaskWorkerGuard<'_> {
    /// The launched worker's PID, once it has recorded one.
    fn worker_pid(&self, wait: Duration) -> Option<u32> {
        let deadline = Instant::now() + wait;
        loop {
            let recorded = fs::read_to_string(&self.pid_file)
                .ok()
                .and_then(|pid| pid.trim().parse().ok());
            if let Some(pid) = recorded {
                return Some(pid);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// SIGKILL the worker's process group if this process has not reaped it
    /// yet, then wait for the launch observer to reap it. Returns whether the
    /// worker is reaped. Idempotent, and never panics (it also runs while
    /// unwinding).
    fn terminate_and_reap(&self) -> bool {
        let Some(pid) = self.worker_pid(Duration::from_secs(10)) else {
            return false;
        };
        let workspace = &self.runtime.paths().repo_root;
        let reaped = || {
            orbit_core::test_support::pipeline_worker_observer_completed(
                workspace,
                &self.run_id,
                pid,
            )
        };
        // Until the observer reaps the child, its PID cannot be reused, so
        // these signals can only reach the worker this fixture launched. The
        // worker is its own session (and group) leader, so the group signal
        // also ends anything it spawned.
        if !reaped() {
            // SAFETY: `pid` is this run's unreaped child; SIGKILL only ends it
            // and its own process group.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while !reaped() {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
        true
    }
}

impl Drop for AutoTaskWorkerGuard<'_> {
    fn drop(&mut self) {
        self.terminate_and_reap();
    }
}

/// The park text, once this process has reaped the substitute worker.
/// A worker that exits without parking fails the test.
fn parked_after_worker_exit(
    runtime: &orbit_core::OrbitRuntime,
    job: &str,
    run_id: &str,
) -> Option<String> {
    let run = runtime
        .show_job_run(run_id)
        .unwrap_or_else(|error| panic!("{job} show {run_id}: {error}"));
    let pid = run.pid?;
    if !orbit_core::test_support::pipeline_worker_observer_completed(
        &runtime.paths().repo_root,
        run_id,
        pid,
    ) {
        return None;
    }
    let parked = runtime
        .read_run_state(run_id)
        .unwrap_or_else(|error| panic!("{job} state {run_id}: {error}"))
        .and_then(|state| state.parked_wait().map(|park| park.reason.clone()));
    Some(parked.unwrap_or_else(|| {
        panic!(
            "{job} worker exited without a park ({:?}); worker log:\n{}",
            run.state,
            worker_log(runtime, run_id)
        )
    }))
}

fn worker_log(runtime: &orbit_core::OrbitRuntime, run_id: &str) -> String {
    match runtime.read_pipeline_worker_log(run_id) {
        Ok(Some(snapshot)) => snapshot
            .content
            .unwrap_or_else(|| "(empty worker log)".to_string()),
        Ok(None) => "(no worker log)".to_string(),
        Err(error) => format!("worker log unreadable: {error}"),
    }
}

fn evaluate_named(
    runtime: &orbit_core::OrbitRuntime,
    name: &str,
) -> orbit_types::workflow::automation::AutomationDiagnostic {
    let definition = runtime.auto_task_show(name).unwrap().expect(name);
    evaluate_auto_task(runtime, &definition, false, Utc::now())
        .unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn commit_file(fixture: &Fixture, name: &str, content: &str) -> SourceRevision {
    let git = crate::auto_task_lifecycle_cli::git;
    fs::write(fixture.repo.join(name), content).unwrap();
    git(fixture, &["add", name]);
    git(fixture, &["commit", "-m", name]);
    SourceRevision {
        commit: git(fixture, &["rev-parse", "HEAD"]),
        tree: git(fixture, &["rev-parse", "HEAD^{tree}"]),
    }
}

fn land_delivery(
    fixture: &Fixture,
    runtime: &orbit_core::OrbitRuntime,
    name: &str,
    branch: &str,
    file: &str,
) {
    let consumer = consumer_key(runtime, "auto-task", name).unwrap();
    let store = runtime.automation_store().unwrap();
    let state = store
        .automation_state(&consumer)
        .unwrap()
        .unwrap_or_else(|| panic!("{name} has no delivery state after baseline"));
    let landed = commit_file(fixture, file, "landed\n");
    store
        .automation_record_delivery_intent(&Delivery {
            key: format!("direct:{}:{branch}:{}", state.repository, landed.commit),
            repository: state.repository.clone(),
            branch: branch.to_string(),
            before: state.observed.clone(),
            after: landed.clone(),
            commits: vec![landed.commit.clone()],
            task_ids: Vec::new(),
            unattributed: Some(UNATTRIBUTED_NO_LANDING_TASK.into()),
            evidence_reference: format!("run:fixture-run:{}", landed.commit),
            evidence_digest: "fixture-digest".into(),
            landed_at: Utc::now(),
        })
        .unwrap();
}

#[test]
fn adopt_on_a_compliant_managed_body_writes_nothing() {
    let fixture = Fixture::new();
    // The shipped body already carries the host-aware contract, so adopting it
    // is a no-op: it must not record an operator edit stamp or settings entry.
    fixture.json(&[
        "auto-task",
        "update",
        "run-failure-patterns",
        "--adopt-bundled-contract",
        "--json",
    ]);
    let shown = fixture.json(&["auto-task", "show", "run-failure-patterns", "--json"]);
    assert_eq!(shown["layering"]["body"], "managed", "{shown}");
    assert!(shown["layering"]["settings"].is_null(), "{shown}");
}

#[test]
fn auto_task_settings_and_managed_bodies() {
    for (name, check) in [
        (
            "template_flags_preserve_unnamed_fields_and_full_tool_templates_still_replace",
            template_flags_preserve_unnamed_fields_and_full_tool_templates_still_replace as fn(),
        ),
        (
            "bundled_auto_task_settings_stay_outside_the_body_and_survive_a_body_refresh",
            bundled_auto_task_settings_stay_outside_the_body_and_survive_a_body_refresh as fn(),
        ),
        (
            "settings_only_fork_migrates_and_a_body_fork_is_preserved_and_reported",
            settings_only_fork_migrates_and_a_body_fork_is_preserved_and_reported as fn(),
        ),
        (
            "older_shipped_bodies_upgrade_with_settings_and_custom_bodies_stay_forked",
            older_shipped_bodies_upgrade_with_settings_and_custom_bodies_stay_forked as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}

/// The host-health body this change supersedes is already in body history
/// (enabled-by-default revision). An install holding those bytes converges,
/// and a copy an operator edited does not.
#[test]
fn host_health_previous_managed_body_converges_and_an_operator_edit_stays() {
    let sources: Value = serde_json::from_str(include_str!(
        "../fixtures/auto-task-body-history/sources.json"
    ))
    .unwrap();
    let previous = sources["host-health"]["d0f99d14459ed86fb0873a5b49c18a3f13c22cc3"]
        .as_str()
        .expect("previous host-health body");
    let fixture = Fixture::new();
    let path = definition_path(&fixture, "host-health");
    let bundled = fs::read_to_string(&path).unwrap();
    assert_ne!(
        previous, bundled,
        "the shipped body must differ from the recorded previous body"
    );
    let seeded = fixture.json(&["auto-task", "show", "host-health", "--json"]);
    assert_eq!(seeded["enabled"], true, "host-health still ships enabled");

    let settings_edit = previous.replacen("  priority: medium", "  priority: high", 1);
    assert_ne!(settings_edit, previous, "fixture must change a setting");
    fs::write(&path, &settings_edit).unwrap();
    let sync = fixture.json(&["workspace", "sync", "--json"]);
    assert_eq!(sync_outcome(&sync, "host-health"), "migrated");
    assert_eq!(fs::read_to_string(&path).unwrap(), bundled);
    let shown = fixture.json(&["auto-task", "show", "host-health", "--json"]);
    assert_eq!(shown["layering"]["body"], "managed");
    assert_eq!(shown["enabled"], true);
    assert_eq!(shown["template"]["priority"], "high");

    let operator_edit = format!("# operator note: local procedure\n{previous}");
    fs::write(&path, &operator_edit).unwrap();
    let sync = fixture.json(&["workspace", "sync", "--json"]);
    assert_eq!(sync_outcome(&sync, "host-health"), "preserved");
    assert_eq!(fs::read_to_string(&path).unwrap(), operator_edit);
    let shown = fixture.json(&["auto-task", "show", "host-health", "--json"]);
    assert_eq!(shown["layering"]["body"], "forked");
}
