//! Grouped findings exercised through the public deterministic filer.
use super::{document, file, fixture, result};
use orbit_core::{ActorIdentity, OrbitRuntime, TaskStatus, application::task::TaskUpdateParams};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::{Value, json};

fn variant(width: u32, status: &str, key: &str) -> Value {
    let mut value = result(
        status,
        "high",
        &format!("fp-{width}"),
        json!({"width": width, "gap": width / 100}),
    );
    value["rule"] = json!("ui.layout");
    value["target"] = json!({"id": format!("view-{width}"), "width": width, "theme": "dark"});
    value["defect"] = json!({"version": 1, "key": key});
    value["original_fingerprint"] = json!(format!("original-{width}"));
    value
}

fn ui(entries: Vec<Value>) -> Value {
    let mut doc = document(entries);
    doc["surface"] = json!("ui");
    doc
}

fn comments(outcome: &Value, kind: &str) -> usize {
    outcome["commented"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == kind)
        .count()
}

fn state(runtime: &OrbitRuntime, id: &str) -> String {
    let artifact = runtime
        .get_task_artifact(id, "rule-findings/state.json")
        .unwrap()
        .expect("group registry artifact");
    String::from_utf8(artifact.content).unwrap()
}

pub(super) fn grouped_variants_preserve_observations_and_evidence() {
    let (root, runtime) = fixture("");
    let repo = root.path().join("repo");
    let images: [(&str, &[u8]); 2] = [
        ("wide.png", b"\x89PNG\r\n\x1a\nwide"),
        ("narrow.png", b"\x89PNG\r\n\x1a\nnarrow"),
    ];
    let mut entries = vec![
        variant(1440, "fail", "stable-key"),
        variant(800, "fail", "stable-key"),
    ];
    for (entry, (name, bytes)) in entries.iter_mut().zip(images) {
        std::fs::write(repo.join(name), bytes).unwrap();
        entry["evidence"] = json!([name]);
    }
    // The same fingerprint at a different canonical target is a distinct observation.
    let mut same_fingerprint = variant(600, "fail", "stable-key");
    same_fingerprint["fingerprint"] = json!("fp-1440");
    entries.push(same_fingerprint);
    let path = repo.join("results.json");
    std::fs::write(&path, serde_json::to_vec(&ui(entries)).unwrap()).unwrap();
    let outcome = runtime
        .run_deterministic(
            "file_rule_findings",
            &json!({}),
            &json!({"results_file": path}),
            ToolContext::default(),
        )
        .unwrap();
    assert_eq!(outcome["filed_count"], 1, "{outcome}");
    let id = outcome["filed"][0]["task_id"].as_str().unwrap();
    let registry = state(&runtime, id);
    for expected in [
        "stable-key",
        "fp-1440",
        "fp-800",
        "original-1440",
        "original-800",
        "view-1440",
        "view-800",
        "view-600",
        "original-600",
        "1440",
        "800",
        "wide.png",
        "narrow.png",
    ] {
        assert!(
            registry.contains(expected),
            "missing {expected} in {registry}"
        );
    }
    for (name, bytes) in images {
        assert_eq!(
            runtime
                .get_task_artifact(id, name)
                .unwrap()
                .unwrap()
                .content,
            bytes
        );
    }
}

pub(super) fn grouping_scope_and_cap_apply_to_causes() {
    let (_root, runtime) = fixture("[ux_rules]\nmax_new_tasks = 2\n");
    let outcome = file(
        &runtime,
        ui(vec![
            variant(1440, "fail", "one"),
            variant(800, "fail", "one"),
            variant(900, "fail", "two"),
            variant(700, "fail", "three"),
        ]),
        json!({}),
    );
    assert_eq!(outcome["filed_count"], 2, "{outcome}");
    assert_eq!(
        outcome["skipped_over_cap"].as_array().unwrap().len(),
        1,
        "{outcome}"
    );
    let (_root, runtime) = fixture("");
    let a = variant(1000, "fail", "same");
    let mut b = variant(1001, "fail", "same");
    b["rule"] = json!("ui.other");
    let mut c = variant(1002, "fail", "same");
    c["defect"]["version"] = json!(2);
    let mut d = variant(1003, "fail", "same");
    d["surface"] = json!("cli");
    let outcome = file(&runtime, ui(vec![a, b, c, d]), json!({}));
    assert_eq!(outcome["filed_count"], 4, "{outcome}");
}

pub(super) fn grouped_reruns_reconcile_only_all_known_variants() {
    let (_root, runtime) = fixture("");
    let a = variant(1440, "fail", "one");
    let b = variant(800, "fail", "one");
    let first = file(&runtime, ui(vec![a.clone(), b.clone()]), json!({}));
    assert_eq!(first["filed_count"], 1, "{first}");
    let id = first["filed"][0]["task_id"].as_str().unwrap();
    let repeat = file(&runtime, ui(vec![b.clone(), a.clone()]), json!({}));
    assert_eq!(repeat["filed_count"], 0, "{repeat}");
    assert_eq!(repeat["commented"].as_array().unwrap().len(), 0, "{repeat}");
    let mut changed = a.clone();
    changed["measurements"]["gap"] = json!(99);
    let new = variant(600, "fail", "one");
    let updated = file(
        &runtime,
        ui(vec![changed.clone(), b.clone(), new.clone()]),
        json!({}),
    );
    assert_eq!(updated["filed_count"], 0, "{updated}");
    assert_eq!(comments(&updated, "measurements_changed"), 1, "{updated}");
    let again = file(
        &runtime,
        ui(vec![new.clone(), b.clone(), changed.clone()]),
        json!({}),
    );
    assert_eq!(again["commented"].as_array().unwrap().len(), 0, "{again}");
    let mut pass_a = changed;
    pass_a["status"] = json!("pass");
    let mut pass_b = b;
    pass_b["status"] = json!("pass");
    let mut pass_new = new;
    pass_new["status"] = json!("pass");
    for incomplete in [
        vec![pass_a.clone(), pass_b.clone()],
        {
            let mut skipped = pass_new.clone();
            skipped["status"] = json!("skipped");
            vec![pass_a.clone(), pass_b.clone(), skipped]
        },
        {
            let mut error = pass_new.clone();
            error["status"] = json!("error");
            vec![pass_a.clone(), pass_b.clone(), error]
        },
    ] {
        let out = file(&runtime, ui(incomplete), json!({}));
        assert_eq!(comments(&out, "pass"), 0, "{out}");
    }
    let all = ui(vec![pass_a, pass_b, pass_new]);
    let passing = file(&runtime, all.clone(), json!({}));
    assert_eq!(comments(&passing, "pass"), 1, "{passing}");
    assert_eq!(
        passing["commented"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["kind"] == "pass")
            .unwrap()["task_id"],
        id
    );
    let repeated = file(&runtime, all, json!({}));
    assert_eq!(comments(&repeated, "pass"), 0, "{repeated}");
    assert_eq!(runtime.get_task(id).unwrap().status, TaskStatus::Proposed);
}

pub(super) fn grouped_legacy_owner_reuse_excludes_terminal_tasks() {
    let (_root, runtime) = fixture("");
    let mut old_a = variant(1440, "fail", "one");
    old_a.as_object_mut().unwrap().remove("defect");
    let mut old_b = variant(800, "fail", "one");
    old_b.as_object_mut().unwrap().remove("defect");
    let first = file(&runtime, ui(vec![old_a, old_b]), json!({}));
    assert_eq!(first["filed_count"], 2, "{first}");
    let ids: Vec<String> = first["filed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["task_id"].as_str().unwrap().to_owned())
        .collect();
    runtime
        .clone()
        .with_actor(ActorIdentity::human("human:operator"))
        .force_update_task_with_identity(
            &ids[0],
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    let grouped = ui(vec![
        variant(1440, "fail", "one"),
        variant(800, "fail", "one"),
    ]);
    let out = file(&runtime, grouped.clone(), json!({}));
    assert_eq!(out["filed_count"], 0, "{out}");
    assert!(
        runtime
            .get_task_artifact(&ids[0], "rule-findings/state.json")
            .unwrap()
            .is_none()
    );
    let registry = state(&runtime, &ids[1]);
    assert!(
        registry.contains("fp-1440") && registry.contains("fp-800"),
        "{registry}"
    );
    let rerun = file(&runtime, grouped, json!({}));
    assert_eq!(rerun["filed_count"], 0, "{rerun}");
}

/// The same public report can be filed by independent runtimes without
/// duplicating its owner, change comment, or evidence attachment.
pub(super) fn grouped_independent_processes_share_owner() {
    const CHILD: &str = "ORBIT_GROUP_TEST_CHILD";
    if let Ok(child) = std::env::var(CHILD) {
        let root = std::path::PathBuf::from(std::env::var_os("ORBIT_GROUP_TEST_ROOT").unwrap());
        let runtime =
            OrbitRuntime::from_roots(&root.join("home/.orbit"), &root.join("repo/.orbit")).unwrap();
        std::fs::write(root.join(format!("ready-{child}")), "ready").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !root.join("ready-1").exists() || !root.join("ready-2").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "other child did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let outcome = runtime
            .run_deterministic(
                "file_rule_findings",
                &json!({}),
                &json!({"results_file": root.join("repo/results.json")}),
                ToolContext::default(),
            )
            .unwrap();
        std::fs::write(
            root.join(format!("outcome-{child}.json")),
            serde_json::to_vec(&outcome).unwrap(),
        )
        .unwrap();
        return;
    }
    let (root, runtime) = fixture("");
    let mut legacy = variant(1440, "fail", "one");
    legacy.as_object_mut().unwrap().remove("defect");
    let first = file(&runtime, ui(vec![legacy]), json!({}));
    let id = first["filed"][0]["task_id"].as_str().unwrap();
    let before = runtime.get_task_comments(id).unwrap().len();
    let image = b"\x89PNG\r\n\x1a\nconcurrent";
    std::fs::write(root.path().join("repo/screen.png"), image).unwrap();
    let mut changed = variant(1440, "fail", "one");
    changed["measurements"]["gap"] = json!(99);
    changed["evidence"] = json!(["screen.png"]);
    std::fs::write(
        root.path().join("repo/results.json"),
        serde_json::to_vec(&ui(vec![changed])).unwrap(),
    )
    .unwrap();
    let mut children = Vec::new();
    for child in ["1", "2"] {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "rule_findings_sweep::rule_findings_sweep_scenarios",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(
                    "ORBIT_RUNTIME_SCENARIO_ROW",
                    "grouped_independent_processes_share_owner",
                )
                .env(CHILD, child)
                .env("ORBIT_GROUP_TEST_ROOT", root.path())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success(), "filer child failed");
    }
    let outcomes: Vec<Value> = ["1", "2"]
        .iter()
        .map(|child| {
            serde_json::from_slice(
                &std::fs::read(root.path().join(format!("outcome-{child}.json"))).unwrap(),
            )
            .unwrap()
        })
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .map(|out| out["filed_count"].as_u64().unwrap())
            .sum::<u64>(),
        0,
        "{outcomes:?}"
    );
    assert_eq!(
        outcomes
            .iter()
            .map(|out| comments(out, "measurements_changed"))
            .sum::<usize>(),
        1,
        "{outcomes:?}"
    );
    assert_eq!(
        runtime
            .list_tasks_by_tags(&["ux-rules".to_string()])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(runtime.get_task_comments(id).unwrap().len(), before + 1);
    assert_eq!(
        runtime
            .get_task_artifact(id, "screen.png")
            .unwrap()
            .unwrap()
            .content,
        image
    );
}

pub(super) fn grouped_existing_owner_receives_new_variant_evidence() {
    let (root, runtime) = fixture("");
    let mut legacy = variant(1440, "fail", "one");
    legacy.as_object_mut().unwrap().remove("defect");
    let first = file(&runtime, ui(vec![legacy]), json!({}));
    let id = first["filed"][0]["task_id"].as_str().unwrap();
    let image = b"\x89PNG\r\n\x1a\nnew-variant";
    std::fs::write(root.path().join("repo/new.png"), image).unwrap();
    let mut new = variant(800, "fail", "one");
    new["evidence"] = json!(["new.png"]);
    let path = root.path().join("repo/results.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&ui(vec![variant(1440, "fail", "one"), new])).unwrap(),
    )
    .unwrap();
    let run = || {
        runtime
            .run_deterministic(
                "file_rule_findings",
                &json!({}),
                &json!({"results_file": path}),
                ToolContext::default(),
            )
            .unwrap()
    };
    let updated = run();
    assert_eq!(updated["filed_count"], 0, "{updated}");
    assert_eq!(
        runtime
            .get_task_artifact(id, "new.png")
            .unwrap()
            .unwrap()
            .content,
        image
    );
    assert!(state(&runtime, id).contains("view-800"));
    let replay = run();
    assert_eq!(replay["filed_count"], 0, "{replay}");
    assert_eq!(replay["commented"].as_array().unwrap().len(), 0, "{replay}");
}

pub(super) fn grouped_terminal_only_legacy_creates_new_owner() {
    let (_root, runtime) = fixture("");
    let mut legacy = variant(1440, "fail", "one");
    legacy.as_object_mut().unwrap().remove("defect");
    let first = file(&runtime, ui(vec![legacy]), json!({}));
    let old = first["filed"][0]["task_id"].as_str().unwrap();
    runtime
        .clone()
        .with_actor(ActorIdentity::human("human:operator"))
        .force_update_task_with_identity(
            old,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    let comments_before = runtime.get_task_comments(old).unwrap().len();
    let next = file(&runtime, ui(vec![variant(1440, "fail", "one")]), json!({}));
    assert_eq!(next["filed_count"], 1, "{next}");
    assert_ne!(next["filed"][0]["task_id"], old);
    assert_eq!(runtime.get_task(old).unwrap().status, TaskStatus::Done);
    assert_eq!(
        runtime.get_task_comments(old).unwrap().len(),
        comments_before
    );
    assert!(
        runtime
            .get_task_artifact(old, "rule-findings/state.json")
            .unwrap()
            .is_none()
    );
    // The terminal match is a read-only historical reference, never an owner.
    assert_eq!(
        next["historical_references"],
        json!([{"group_fingerprint": next["filed"][0]["fingerprint"], "task_id": old, "status": "done"}]),
        "{next}"
    );
}

/// A crash after the filer created a group's task but before it attached the
/// evidence and registry leaves only the task. Replaying the report repairs
/// the attachments on that owner without a second task or a comment.
pub(super) fn replay_repairs_interrupted_group_attachments() {
    let (root, runtime) = fixture("");
    let repo = root.path().join("repo");
    std::fs::write(repo.join("wide.png"), b"\x89PNG\r\n\x1a\nwide").unwrap();
    let mut wide = variant(1440, "fail", "one");
    wide["evidence"] = json!(["wide.png"]);
    let report = ui(vec![wide, variant(375, "fail", "one")]);
    let path = repo.join("results.json");
    std::fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
    let run = |runtime: &OrbitRuntime| {
        runtime
            .run_deterministic(
                "file_rule_findings",
                &json!({}),
                &json!({"results_file": path}),
                ToolContext::default(),
            )
            .unwrap()
    };
    // Reference filing in a separate workspace gives the exact task the
    // interrupted filer would have created.
    let (_reference_root, reference) = fixture("");
    let reference_repo = _reference_root.path().join("repo");
    std::fs::write(reference_repo.join("wide.png"), b"\x89PNG\r\n\x1a\nwide").unwrap();
    let reference_path = reference_repo.join("results.json");
    std::fs::write(&reference_path, serde_json::to_vec(&report).unwrap()).unwrap();
    let filed = reference
        .run_deterministic(
            "file_rule_findings",
            &json!({}),
            &json!({"results_file": reference_path}),
            ToolContext::default(),
        )
        .unwrap();
    let created = reference
        .get_task(filed["filed"][0]["task_id"].as_str().unwrap())
        .unwrap();
    let task = runtime
        .add_task(orbit_core::application::task::TaskAddParams {
            title: created.title.clone(),
            description: created.description.clone(),
            tags: created.tags.clone(),
            status: Some(TaskStatus::Proposed),
            system_created: true,
            rule_fingerprint: created.rule_fingerprint.clone(),
            ..Default::default()
        })
        .unwrap();
    assert!(
        runtime
            .get_task_artifact_manifest(&task.id)
            .unwrap()
            .is_empty()
    );
    let replay = run(&runtime);
    assert_eq!(replay["filed_count"], 0, "{replay}");
    assert_eq!(replay["commented"].as_array().unwrap().len(), 0, "{replay}");
    let manifest = runtime.get_task_artifact_manifest(&task.id).unwrap();
    for name in ["wide.png", "rule-findings/state.json"] {
        assert!(
            manifest.iter().any(|entry| entry.path == name),
            "{name}: {manifest:?}"
        );
    }
    assert!(state(&runtime, &task.id).contains("view-375"));
    let again = run(&runtime);
    assert_eq!(again["filed_count"], 0, "{again}");
    assert_eq!(
        runtime.get_task_artifact_manifest(&task.id).unwrap().len(),
        manifest.len()
    );
}

/// A legacy aggregate owner bound to a group is repaired only by the group:
/// the old aggregate passing while another width still fails posts nothing.
pub(super) fn bound_legacy_owner_ignores_aggregate_pass() {
    let (_root, runtime) = fixture("");
    let aggregate = |status: &str| {
        let mut value = result(status, "high", "original-1440", json!({"width": 1440}));
        value["rule"] = json!("ui.layout");
        value["target"] = json!({"id": "view-1440", "width": 1440, "theme": "dark"});
        value
    };
    let legacy = file(&runtime, ui(vec![aggregate("fail")]), json!({}));
    let id = legacy["filed"][0]["task_id"].as_str().unwrap().to_string();
    let bound = file(
        &runtime,
        ui(vec![
            variant(1440, "fail", "one"),
            variant(375, "fail", "one"),
        ]),
        json!({}),
    );
    assert_eq!(bound["filed_count"], 0, "{bound}");
    assert!(state(&runtime, &id).contains("view-375"));
    let repaired_wide = || {
        ui(vec![
            aggregate("pass"),
            variant(1440, "pass", "one"),
            variant(375, "fail", "one"),
        ])
    };
    let third = file(&runtime, repaired_wide(), json!({}));
    assert_eq!(comments(&third, "pass"), 0, "{third}");
    let fourth = file(&runtime, repaired_wide(), json!({}));
    assert_eq!(comments(&fourth, "pass"), 0, "{fourth}");
    assert_eq!(fourth["commented"].as_array().unwrap().len(), 0, "{fourth}");
    assert_eq!(fourth["filed_count"], 0, "{fourth}");
}

/// An interrupted attach whose screenshots a later run re-rendered under
/// new names reports the lost evidence and attaches the current evidence.
pub(super) fn fresh_evidence_rerun_after_interrupted_attach() {
    let (root, runtime) = fixture("");
    let (_reference_root, reference) = fixture("");
    let reference_repo = _reference_root.path().join("repo");
    std::fs::write(reference_repo.join("wide.png"), b"\x89PNG\r\n\x1a\nwide").unwrap();
    let mut wide = variant(1440, "fail", "one");
    wide["evidence"] = json!(["wide.png"]);
    let reference_path = reference_repo.join("results.json");
    std::fs::write(
        &reference_path,
        serde_json::to_vec(&ui(vec![wide, variant(375, "fail", "one")])).unwrap(),
    )
    .unwrap();
    let filed = reference
        .run_deterministic(
            "file_rule_findings",
            &json!({}),
            &json!({"results_file": reference_path}),
            ToolContext::default(),
        )
        .unwrap();
    let created = reference
        .get_task(filed["filed"][0]["task_id"].as_str().unwrap())
        .unwrap();
    let task = runtime
        .add_task(orbit_core::application::task::TaskAddParams {
            title: created.title.clone(),
            description: created.description.clone(),
            tags: created.tags.clone(),
            status: Some(TaskStatus::Proposed),
            system_created: true,
            rule_fingerprint: created.rule_fingerprint.clone(),
            ..Default::default()
        })
        .unwrap();
    let repo = root.path().join("repo");
    std::fs::write(repo.join("fresh.png"), b"\x89PNG\r\n\x1a\nfresh").unwrap();
    let mut rerendered = variant(1440, "fail", "one");
    rerendered["evidence"] = json!(["fresh.png"]);
    let path = repo.join("results.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&ui(vec![rerendered, variant(375, "fail", "one")])).unwrap(),
    )
    .unwrap();
    let run = || {
        runtime
            .run_deterministic(
                "file_rule_findings",
                &json!({}),
                &json!({"results_file": path}),
                ToolContext::default(),
            )
            .unwrap()
    };
    let rerun = run();
    assert_eq!(rerun["filed_count"], 0, "{rerun}");
    assert_eq!(
        rerun["evidence_gaps"],
        json!([{"task_id": task.id, "fingerprint": created.rule_fingerprint, "missing_evidence": ["wide.png"]}]),
        "{rerun}"
    );
    assert!(
        runtime
            .get_task_artifact(&task.id, "fresh.png")
            .unwrap()
            .is_some()
    );
    let registry = state(&runtime, &task.id);
    assert!(
        registry.contains("fresh.png") && !registry.contains("wide.png"),
        "{registry}"
    );
    let again = run();
    assert_eq!(again["evidence_gaps"], json!([]), "{again}");
    assert_eq!(again["filed_count"], 0, "{again}");
}

/// The producer's owner context lists only open owners' groups, read-only.
pub(super) fn owner_context_lists_open_owner_groups() {
    let (_root, runtime) = fixture("");
    let prepare = || {
        runtime
            .run_deterministic(
                "prepare_rule_findings",
                &json!({}),
                &json!({}),
                ToolContext::default(),
            )
            .unwrap()
    };
    assert_eq!(prepare(), json!({"schema_version": 1, "groups": []}));
    let mut wide = variant(1440, "fail", "one");
    wide["surface"] = json!("ui");
    let first = file(
        &runtime,
        ui(vec![
            wide,
            variant(800, "fail", "one"),
            variant(900, "fail", "two"),
        ]),
        json!({}),
    );
    assert_eq!(first["filed_count"], 2, "{first}");
    let context = prepare();
    let groups = context["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{context}");
    let keys: Vec<_> = groups.iter().map(|g| g["defect"]["key"].clone()).collect();
    assert!(
        keys.contains(&json!("one")) && keys.contains(&json!("two")),
        "{context}"
    );
    assert!(groups.iter().all(|g| g["rule"] == "ui.layout"), "{context}");
    let comments_before: usize = first["filed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            runtime
                .get_task_comments(f["task_id"].as_str().unwrap())
                .unwrap()
                .len()
        })
        .sum();
    let done = groups[0]["task_id"].as_str().unwrap();
    runtime
        .clone()
        .with_actor(ActorIdentity::human("human:operator"))
        .force_update_task_with_identity(
            done,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    let after = prepare();
    assert_eq!(after["groups"].as_array().unwrap().len(), 1, "{after}");
    assert_ne!(after["groups"][0]["task_id"], done, "{after}");
    let comments_after: usize = first["filed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            runtime
                .get_task_comments(f["task_id"].as_str().unwrap())
                .unwrap()
                .len()
        })
        .sum();
    assert_eq!(comments_before, comments_after, "prepare must not write");
}

/// max_new_tasks = 1 files the highest-severity group with every variant
/// and screenshot; equal image bytes are one artifact for both variants.
pub(super) fn group_cap_files_one_whole_group() {
    let (root, runtime) = fixture("[ux_rules]\nmax_new_tasks = 1\n");
    let repo = root.path().join("repo");
    let shared = b"\x89PNG\r\n\x1a\nshared";
    std::fs::write(repo.join("shared.png"), shared).unwrap();
    let mut wide = variant(1440, "fail", "worst");
    let mut narrow = variant(375, "fail", "worst");
    for entry in [&mut wide, &mut narrow] {
        entry["severity"] = json!("critical");
        entry["evidence"] = json!(["shared.png"]);
    }
    let mut other = variant(900, "fail", "other");
    other["severity"] = json!("medium");
    let path = repo.join("results.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&ui(vec![other, narrow, wide])).unwrap(),
    )
    .unwrap();
    let outcome = runtime
        .run_deterministic(
            "file_rule_findings",
            &json!({}),
            &json!({"results_file": path}),
            ToolContext::default(),
        )
        .unwrap();
    assert_eq!(outcome["filed_count"], 1, "{outcome}");
    assert_eq!(
        outcome["skipped_over_cap"].as_array().unwrap().len(),
        1,
        "{outcome}"
    );
    assert_eq!(
        outcome["skipped_over_cap"][0]["severity"], "medium",
        "{outcome}"
    );
    let id = outcome["filed"][0]["task_id"].as_str().unwrap();
    let registry: Value = serde_json::from_str(&state(&runtime, id)).unwrap();
    let observations = registry["observations"].as_array().unwrap();
    assert_eq!(observations.len(), 2, "{registry}");
    assert!(
        observations
            .iter()
            .all(|o| o["evidence"] == json!(["shared.png"])),
        "{registry}"
    );
    let manifest = runtime.get_task_artifact_manifest(id).unwrap();
    assert_eq!(
        manifest.iter().filter(|e| e.path == "shared.png").count(),
        1
    );
}

/// Archive descriptors are checked and summarized, never inflated; a
/// malformed descriptor refuses filing before any task is written.
pub(super) fn observation_archives_are_reported_not_inflated() {
    let (_root, runtime) = fixture("");
    let mut doc = ui(vec![variant(1440, "fail", "one")]);
    doc["summary"] = json!({"observations": 29});
    doc["archives"] = json!([{
        "surface": "ui", "encoding": "gzip", "observation_count": 29,
        "uncompressed_bytes": 10, "uncompressed_sha256": "u", "gzip_sha256": "g",
        "parts": [{"name": "part.gz", "bytes": 3, "sha256": "p"}],
    }]);
    let outcome = file(&runtime, doc.clone(), json!({}));
    assert_eq!(outcome["observation_count"], 29, "{outcome}");
    assert_eq!(outcome["reported_count"], 1, "{outcome}");
    assert_eq!(
        outcome["archives"][0]["parts"],
        json!(["part.gz"]),
        "{outcome}"
    );
    assert_eq!(outcome["archives"][0]["verified"], false, "{outcome}");
    let (_root, runtime) = fixture("");
    doc["archives"][0]["parts"] = json!([]);
    let refused = runtime.run_deterministic(
        "file_rule_findings",
        &json!({}),
        &json!({"results": doc}),
        ToolContext::default(),
    );
    assert!(refused.is_err(), "{refused:?}");
    assert!(
        runtime
            .list_tasks_by_tags(&["ux-rules".to_string()])
            .unwrap()
            .is_empty()
    );
}

#[test]
#[ignore = "requires retained actual-native browser capture in UX_GROUPING_NATIVE_REPORT"]
fn retained_native_browser_report_files_groups() {
    let report_path = std::path::PathBuf::from(
        std::env::var_os("UX_GROUPING_NATIVE_REPORT")
            .expect("set UX_GROUPING_NATIVE_REPORT to the retained native capture"),
    );
    let report_dir = report_path.parent().unwrap();
    let mut report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    let failures: Vec<Value> = report["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["status"] == "fail")
        .cloned()
        .collect();
    // `failures:causes`; the retained native layout capture is 4:2.
    let expected = std::env::var("UX_GROUPING_NATIVE_EXPECT").unwrap_or_else(|_| "4:2".into());
    let (expected_failures, expected_causes) = expected
        .split_once(':')
        .map(|(f, c)| (f.parse::<usize>().unwrap(), c.parse::<usize>().unwrap()))
        .expect("UX_GROUPING_NATIVE_EXPECT is failures:causes");
    assert_eq!(
        failures.len(),
        expected_failures,
        "expected the retained native failures"
    );
    let scopes: std::collections::BTreeSet<_> = failures
        .iter()
        .map(|entry| {
            (
                entry["surface"].to_string(),
                entry["rule"].to_string(),
                entry["defect"].to_string(),
            )
        })
        .collect();
    assert_eq!(scopes.len(), expected_causes, "expected the native causes");
    let (root, runtime) = fixture("[ux_rules]\nfile_at = \"low\"\n");
    let repo = root.path().join("repo");
    for entry in &failures {
        for evidence in entry["evidence"].as_array().unwrap() {
            let relative = std::path::Path::new(evidence.as_str().unwrap());
            assert!(
                relative
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
                "evidence path must be relative"
            );
            assert_eq!(
                relative.extension().and_then(|ext| ext.to_str()),
                Some("png")
            );
            let bytes = std::fs::read(report_dir.join(relative)).unwrap();
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
            let destination = repo.join(relative);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::write(destination, bytes).unwrap();
        }
    }
    report["results"] = json!(failures);
    let path = repo.join("results.json");
    std::fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
    let run = || {
        runtime
            .run_deterministic(
                "file_rule_findings",
                &json!({}),
                &json!({"results_file": path}),
                ToolContext::default(),
            )
            .unwrap()
    };
    let first = run();
    assert_eq!(first["filed_count"], expected_causes, "{first}");
    let ids: Vec<_> = first["filed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["task_id"].as_str().unwrap().to_owned())
        .collect();
    assert_ne!(ids[0], ids[1]);
    let registries: Vec<Value> = ids
        .iter()
        .map(|id| serde_json::from_str(&state(&runtime, id)).unwrap())
        .collect();
    for failure in &failures {
        let owners: Vec<_> = registries
            .iter()
            .enumerate()
            .filter(|(_, registry)| {
                registry["observations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|observation| {
                        observation["fingerprint"] == failure["fingerprint"]
                            && observation["target"] == failure["target"]
                            && observation["measurements"] == failure["measurements"]
                    })
            })
            .collect();
        assert_eq!(
            owners.len(),
            1,
            "native observation must appear in exactly one owner: {failure}"
        );
        let id = &ids[owners[0].0];
        for evidence in failure["evidence"].as_array().unwrap() {
            let name = evidence.as_str().unwrap();
            assert_eq!(
                runtime
                    .get_task_artifact(id, name)
                    .unwrap()
                    .unwrap()
                    .content,
                std::fs::read(report_dir.join(name)).unwrap(),
                "native screenshot {name}"
            );
        }
    }
    let repeat = run();
    assert_eq!(repeat["filed_count"], 0, "{repeat}");
    assert_eq!(repeat["commented"].as_array().unwrap().len(), 0, "{repeat}");
    report["results"].as_array_mut().unwrap().reverse();
    std::fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
    let reordered = run();
    assert_eq!(reordered["filed_count"], 0, "{reordered}");
    assert_eq!(
        reordered["commented"].as_array().unwrap().len(),
        0,
        "{reordered}"
    );
}
