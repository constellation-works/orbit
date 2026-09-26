//! The friction write surface, exercised through the registered tools
//! [ORB-10590].
//!
//! These are the boundary tests for the record handle: what an author can set,
//! what the surface refuses, and what a caller who sets nothing gets.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::friction::FRICTION_TITLE_MAX_CHARS;
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_store::contracts::StoredFrictionRecord;
use orbit_types::record::{FrictionRecord, FrictionStatus};
use serde_json::{Value, json};

use super::super::friction_tools::record_to_json;
use super::super::test_support::{invalid_input_message, run_tool_as_operator, test_runtime};
use crate::OrbitRuntime;
use crate::runtime::workspace::catalog::{
    FederatedWorkspaceTarget, WorkspaceCatalog, WorkspaceScope,
};

/// A structured report whose opening line labels a section rather than the
/// record — the shape derivation has to see through.
const SECTIONED_BODY: &str = "## What happened\n\nThe worker exited before claiming the run.\n\n\
                              ## Evidence\n\nOne log line.";

fn add(input: Value) -> Result<Value, orbit_common::OrbitError> {
    let (_temp, runtime, _repo) = test_runtime();
    run_tool_as_operator(&runtime, "orbit.friction.add", input)
}

#[test]
fn add_records_the_authors_title() {
    let record = add(json!({
        "body": SECTIONED_BODY,
        "title": "Worker exits before claiming its run",
        "model": TEST_CODEX_MODEL,
    }))
    .expect("add with title");

    assert_eq!(
        record["title"],
        json!("Worker exits before claiming its run")
    );
}

#[test]
fn add_without_a_title_falls_back_to_the_bodys_subject() {
    let record = add(json!({ "body": SECTIONED_BODY, "model": TEST_CODEX_MODEL }))
        .expect("add without title");

    assert_eq!(
        record["title"],
        json!("The worker exited before claiming the run.")
    );
}

#[test]
fn add_normalizes_tag_aliases_and_reports_what_was_stored() {
    let record = add(json!({
        "body": SECTIONED_BODY,
        "tags": ["ci", "testing", "cli"],
        "model": TEST_CODEX_MODEL,
    }))
    .expect("add with tag aliases");

    assert_eq!(record["tags"], json!(["build", "tooling"]));
    assert_eq!(
        record["tag_normalizations"],
        json!([
            {"input": "ci", "stored": "build"},
            {"input": "testing", "stored": "build"},
            {"input": "cli", "stored": "tooling"},
        ])
    );
}

#[test]
fn add_still_rejects_unknown_tags_with_the_canonical_vocabulary() {
    let message = invalid_input_message(add(json!({
        "body": SECTIONED_BODY,
        "tags": ["generation-guard"],
        "model": TEST_CODEX_MODEL,
    })));

    assert!(
        message.contains("unknown friction tag(s): generation-guard"),
        "{message}"
    );
    for canonical in ["automation", "build", "skill-guidance", "tooling"] {
        assert!(message.contains(canonical), "{message}");
    }
}

#[test]
fn add_refuses_a_non_string_task_id() {
    let message = invalid_input_message(add(json!({
        "body": SECTIONED_BODY,
        "task_id": 123,
        "model": TEST_CODEX_MODEL,
    })));

    assert!(message.contains("`task_id`"), "{message}");
}

#[test]
fn add_refuses_a_title_too_long_to_read_in_a_list() {
    let message = invalid_input_message(add(json!({
        "body": SECTIONED_BODY,
        "title": "x".repeat(FRICTION_TITLE_MAX_CHARS + 1),
        "model": TEST_CODEX_MODEL,
    })));

    assert!(message.contains("`title`"), "{message}");
    assert!(
        message.contains(&FRICTION_TITLE_MAX_CHARS.to_string()),
        "{message}"
    );
}

#[test]
fn add_refuses_a_blank_title() {
    let message = invalid_input_message(add(json!({
        "body": SECTIONED_BODY,
        "title": "   ",
        "model": TEST_CODEX_MODEL,
    })));

    assert!(message.contains("must not be blank"), "{message}");
}

#[test]
fn a_multi_line_title_is_stored_as_one_line() {
    let record = add(json!({
        "body": SECTIONED_BODY,
        "title": "Worker exits\nbefore claiming its run",
        "model": TEST_CODEX_MODEL,
    }))
    .expect("add with multi-line title");

    assert_eq!(
        record["title"],
        json!("Worker exits before claiming its run")
    );
}

#[test]
fn update_retitles_a_record_without_touching_its_body() {
    let (_temp, runtime, _repo) = test_runtime();
    let seeded = run_tool_as_operator(
        &runtime,
        "orbit.friction.add",
        json!({ "body": SECTIONED_BODY, "model": TEST_CODEX_MODEL }),
    )
    .expect("seed record");
    let id = seeded["id"].as_str().expect("record id");

    let updated = run_tool_as_operator(
        &runtime,
        "orbit.friction.update",
        json!({ "id": id, "title": "Worker exits before claiming its run" }),
    )
    .expect("retitle");

    assert_eq!(
        updated["title"],
        json!("Worker exits before claiming its run")
    );
    assert_eq!(updated["body"], seeded["body"]);
}

#[test]
fn an_empty_update_title_restores_derivation() {
    let (_temp, runtime, _repo) = test_runtime();
    let seeded = run_tool_as_operator(
        &runtime,
        "orbit.friction.add",
        json!({ "body": SECTIONED_BODY, "title": "Set by hand", "model": TEST_CODEX_MODEL }),
    )
    .expect("seed record");
    let id = seeded["id"].as_str().expect("record id");

    let updated = run_tool_as_operator(
        &runtime,
        "orbit.friction.update",
        json!({ "id": id, "title": "" }),
    )
    .expect("clear title");

    assert_eq!(
        updated["title"],
        json!("The worker exited before claiming the run.")
    );
}

#[test]
fn list_default_is_always_the_legacy_array() {
    let (_temp, runtime, _repo) = test_runtime();
    let seeded = run_tool_as_operator(
        &runtime,
        "orbit.friction.add",
        json!({
            "title": "EnvGuard dropped its restore",
            "body": "workspace_init lost the parallel env snapshot while EnvGuard was armed.",
            "tags": ["tooling"],
            "model": TEST_CODEX_MODEL,
        }),
    )
    .expect("seed open friction");
    let id = seeded["id"].as_str().expect("record id");

    let month = seeded["created_at"]
        .as_str()
        .and_then(|created_at| created_at.get(..7))
        .expect("record month");
    let filtered_hit = run_tool_as_operator(
        &runtime,
        "orbit.friction.list",
        json!({
            "model": TEST_CODEX_MODEL,
            "status": "open",
            "tag": "tooling",
            "month": month,
            "q": "EnvGuard",
            "from": "2000-01-01T00:00:00Z",
            "to": "2100-01-01T00:00:00Z",
            "limit": 10,
            "offset": 0,
        }),
    )
    .expect("list with every filter");
    let records = filtered_hit.as_array().expect("default record array");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"], json!(id));

    for input in [
        json!({ "q": "EnvGuardUniqueMiss" }),
        json!({ "q": "EnvGuard workspace_init" }),
        json!({ "status": "resolved" }),
    ] {
        let result = run_tool_as_operator(&runtime, "orbit.friction.list", input)
            .expect("default empty list");
        assert_eq!(result, json!([]), "every default empty result is an array");
    }
}

#[test]
fn list_with_notes_is_one_stable_envelope_for_hits_and_misses() {
    let (_temp, runtime, _repo) = test_runtime();
    let seeded = run_tool_as_operator(
        &runtime,
        "orbit.friction.add",
        json!({
            "body": "workspace_init lost the parallel env snapshot while EnvGuard was armed.",
            "model": TEST_CODEX_MODEL,
        }),
    )
    .expect("seed friction");

    let hit = run_tool_as_operator(
        &runtime,
        "orbit.friction.list",
        json!({ "q": "EnvGuard", "response_mode": "with_notes" }),
    )
    .expect("notes-mode hit");
    assert_eq!(hit["records"][0]["id"], seeded["id"]);
    assert_eq!(hit["notes"], json!([]));

    let one_word_miss = run_tool_as_operator(
        &runtime,
        "orbit.friction.list",
        json!({ "q": "UniqueMiss", "response_mode": "with_notes" }),
    )
    .expect("notes-mode one-word miss");
    assert_eq!(one_word_miss, json!({ "records": [], "notes": [] }));

    let multi_word_miss = run_tool_as_operator(
        &runtime,
        "orbit.friction.list",
        json!({ "q": "EnvGuard workspace_init", "response_mode": "with_notes" }),
    )
    .expect("notes-mode multi-word miss");
    assert_eq!(multi_word_miss["records"], json!([]));
    let note = multi_word_miss["notes"][0].as_str().expect("note text");
    assert!(note.contains("single case-insensitive substring"), "{note}");
    assert!(note.contains("not proof the corpus is empty"), "{note}");
}

#[test]
fn list_rejects_unknown_response_modes() {
    let (_temp, runtime, _repo) = test_runtime();
    let message = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.friction.list",
        json!({ "response_mode": "sometimes" }),
    ));

    assert!(message.contains("`response_mode`"), "{message}");
    assert!(message.contains("`with_notes`"), "{message}");
}

#[test]
fn update_still_requires_at_least_one_mutable_field() {
    let (_temp, runtime, _repo) = test_runtime();
    let seeded = run_tool_as_operator(
        &runtime,
        "orbit.friction.add",
        json!({ "body": SECTIONED_BODY, "model": TEST_CODEX_MODEL }),
    )
    .expect("seed record");
    let id = seeded["id"].as_str().expect("record id");

    let message = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.friction.update",
        json!({ "id": id }),
    ));

    assert!(message.contains("`title`"), "{message}");
}

fn stored_record(title: Option<&str>, body: &str) -> StoredFrictionRecord {
    StoredFrictionRecord {
        record: FrictionRecord {
            id: "F2026-05-007".to_string(),
            title: title.map(ToString::to_string),
            model: "codex".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 5, 17, 4, 5, 0).unwrap(),
            status: FrictionStatus::Resolved,
            tags: vec!["tooling".to_string()],
            resolved_at: Some(Utc.with_ymd_and_hms(2026, 5, 17, 4, 10, 0).unwrap()),
            during_task: None,
            resolved_by_task: Some("ORB-00093".to_string()),
            rehome_to: None,
            body: body.to_string(),
        },
        path: Some("frictions/2026-05/F007.md".into()),
    }
}

#[test]
fn record_to_json_includes_resolved_by_task() {
    let value = record_to_json(stored_record(None, "Resolved by task")).unwrap();

    assert_eq!(value["resolved_by_task"], json!("ORB-00093"));
}

#[test]
fn record_to_json_prefers_the_stored_title() {
    let value = record_to_json(stored_record(
        Some("Queued runs never reach a worker"),
        "## What happened\n\nSomething else entirely.",
    ))
    .unwrap();

    assert_eq!(value["title"], json!("Queued runs never reach a worker"));
}

/// A record written before the field existed still projects a usable
/// handle, so the corpus needs no rewrite to become readable.
#[test]
fn record_to_json_derives_a_title_for_a_record_without_one() {
    let value = record_to_json(stored_record(
        None,
        "## What happened\n\nThe worker exited before claiming the run.\n\n## Evidence\n\nOne log line.",
    ))
    .unwrap();

    assert_eq!(
        value["title"],
        json!("The worker exited before claiming the run.")
    );
}

/// A registry of one owning workspace, so the re-home path resolves its target
/// the way a registered runtime does without a registry on disk.
struct OwnerCatalog {
    target: FederatedWorkspaceTarget,
    owner: OrbitRuntime,
}

impl WorkspaceCatalog for OwnerCatalog {
    fn resolve_scope(
        &self,
        scope: &WorkspaceScope,
    ) -> Result<Vec<FederatedWorkspaceTarget>, OrbitError> {
        match scope {
            WorkspaceScope::Selectors(selectors)
                if selectors
                    .iter()
                    .all(|s| *s == self.target.name || *s == self.target.workspace_id) =>
            {
                Ok(vec![self.target.clone()])
            }
            _ => Err(OrbitError::WorkspaceError(format!(
                "unknown workspace selector: {scope:?}"
            ))),
        }
    }

    fn open(&self, _target: &FederatedWorkspaceTarget) -> Result<OrbitRuntime, OrbitError> {
        Ok(self.owner.clone())
    }
}

/// A product workspace and the platform workspace that owns its friction,
/// sharing one host store as registered checkouts do.
fn product_and_owner() -> (tempfile::TempDir, OrbitRuntime, OrbitRuntime) {
    let root = tempfile::tempdir().expect("tempdir");
    let global_root = root.path().join("global");
    let product_orbit = root.path().join("product").join(".orbit");
    let owner_orbit = root.path().join("platform").join(".orbit");
    for dir in [&global_root, &product_orbit, &owner_orbit] {
        std::fs::create_dir_all(dir).expect("create root");
    }
    let owner = OrbitRuntime::from_roots(&global_root, &owner_orbit).expect("owner runtime");
    let product = OrbitRuntime::from_roots(&global_root, &product_orbit)
        .expect("product runtime")
        .with_workspace_catalog(Arc::new(OwnerCatalog {
            target: FederatedWorkspaceTarget {
                workspace_id: "ws_platform".to_string(),
                name: "platform".to_string(),
                repo_root: root.path().join("platform"),
            },
            owner: owner.clone(),
        }));
    (root, product, owner)
}

#[test]
fn update_records_and_clears_the_rehome_disposition() {
    let (_temp, runtime, _repo) = test_runtime();
    let seeded = run_tool_as_operator(
        &runtime,
        "orbit.friction.add",
        json!({ "body": SECTIONED_BODY, "model": TEST_CODEX_MODEL }),
    )
    .expect("seed record");
    let id = seeded["id"].as_str().expect("record id");

    let recorded = run_tool_as_operator(
        &runtime,
        "orbit.friction.update",
        json!({ "id": id, "rehome_to": " ws_orbit " }),
    )
    .expect("record the owning workspace");
    assert_eq!(recorded["rehome_to"], json!("ws_orbit"));
    assert_eq!(recorded["status"], json!("open"));

    let cleared = run_tool_as_operator(
        &runtime,
        "orbit.friction.update",
        json!({ "id": id, "rehome_to": "" }),
    )
    .expect("clear the disposition");
    assert!(cleared.get("rehome_to").is_none(), "{cleared}");
}

#[test]
fn rehome_moves_a_friction_into_the_registered_owner() {
    let (_temp, product, owner) = product_and_owner();
    let seeded = run_tool_as_operator(
        &product,
        "orbit.friction.add",
        json!({
            "body": SECTIONED_BODY,
            "title": "Worker exits before claiming its run",
            "tags": ["tooling"],
            "during_task": "DANI-10691",
            "model": TEST_CODEX_MODEL,
        }),
    )
    .expect("seed record");
    let id = seeded["id"].as_str().expect("record id");

    let moved = run_tool_as_operator(
        &product,
        "orbit.friction.rehome",
        json!({ "id": id, "to_workspace": "platform" }),
    )
    .expect("rehome");

    assert_eq!(moved["status"], json!("resolved"));
    assert_eq!(moved["rehome_to"], json!("ws_platform"));
    let new_id = moved["rehomed_as"]["id"].as_str().expect("new id");
    assert!(
        moved["body"].as_str().unwrap_or_default().contains(new_id),
        "the source points at the moved record: {moved}"
    );

    let owned = run_tool_as_operator(&owner, "orbit.friction.list", json!({ "status": "open" }))
        .expect("list owner");
    let owned = owned.as_array().expect("record array");
    assert_eq!(owned.len(), 1, "{owned:?}");
    assert_eq!(owned[0]["id"], json!(new_id));
    assert_eq!(owned[0]["title"], seeded["title"]);
    assert_eq!(owned[0]["created_at"], seeded["created_at"]);
    assert_eq!(owned[0]["during_task"], json!("DANI-10691"));
    assert!(
        owned[0]["body"]
            .as_str()
            .unwrap_or_default()
            .starts_with(SECTIONED_BODY),
        "{}",
        owned[0]
    );
}

#[test]
fn rehome_refuses_an_unregistered_target_and_a_runtime_without_a_registry() {
    let (_temp, product, _owner) = product_and_owner();
    let seeded = run_tool_as_operator(
        &product,
        "orbit.friction.add",
        json!({ "body": SECTIONED_BODY, "model": TEST_CODEX_MODEL }),
    )
    .expect("seed record");
    let id = seeded["id"].as_str().expect("record id");

    run_tool_as_operator(
        &product,
        "orbit.friction.rehome",
        json!({ "id": id, "to_workspace": "nowhere" }),
    )
    .expect_err("an unknown workspace is refused");

    let (_bare_temp, bare, _repo) = test_runtime();
    let bare_seed = run_tool_as_operator(
        &bare,
        "orbit.friction.add",
        json!({ "body": SECTIONED_BODY, "model": TEST_CODEX_MODEL }),
    )
    .expect("seed bare record");
    let error = run_tool_as_operator(
        &bare,
        "orbit.friction.rehome",
        json!({ "id": bare_seed["id"], "to_workspace": "platform" }),
    )
    .expect_err("a standalone runtime has no registry to resolve against");
    assert!(matches!(error, OrbitError::WorkspaceError(_)), "{error:?}");

    let untouched =
        run_tool_as_operator(&product, "orbit.friction.list", json!({ "status": "open" }))
            .expect("list product");
    assert_eq!(untouched.as_array().map(Vec::len), Some(1));
}

/// A missing record is a not-found error, as a missing task is — not a
/// malformed request — so every surface reports it with its not-found code.
#[test]
fn show_of_a_missing_record_is_not_found() {
    let (_temp, runtime, _repo) = test_runtime();
    let error = run_tool_as_operator(
        &runtime,
        "orbit.friction.show",
        json!({ "id": "F2099-01-001" }),
    )
    .expect_err("no such record");
    assert!(
        matches!(
            &error,
            OrbitError::NotFound { kind: orbit_common::NotFoundKind::Friction, id } if id == "F2099-01-001"
        ),
        "{error:?}"
    );
}

#[test]
fn update_of_a_missing_record_is_not_found_and_invalid_input_is_preserved() {
    let (_temp, runtime, _repo) = test_runtime();

    let missing = run_tool_as_operator(
        &runtime,
        "orbit.friction.update",
        json!({ "id": "F2099-01-001", "status": "triaged" }),
    )
    .expect_err("no such record");
    assert!(
        matches!(
            &missing,
            OrbitError::NotFound { kind: orbit_common::NotFoundKind::Friction, id } if id == "F2099-01-001"
        ),
        "{missing:?}"
    );

    let malformed_id = run_tool_as_operator(
        &runtime,
        "orbit.friction.update",
        json!({ "id": "malformed-id", "status": "triaged" }),
    )
    .expect_err("malformed id");
    assert!(
        matches!(malformed_id, OrbitError::InvalidInput(_)),
        "{malformed_id:?}"
    );

    let invalid_field = run_tool_as_operator(
        &runtime,
        "orbit.friction.update",
        json!({ "id": "F2099-01-001", "status": "bogus-status" }),
    )
    .expect_err("invalid field value");
    assert!(
        matches!(invalid_field, OrbitError::InvalidInput(_)),
        "{invalid_field:?}"
    );
}

#[test]
fn resolve_of_a_missing_record_is_not_found_and_malformed_id_is_invalid_input() {
    let (_temp, runtime, _repo) = test_runtime();

    let missing = run_tool_as_operator(
        &runtime,
        "orbit.friction.resolve",
        json!({ "id": "F2099-01-001" }),
    )
    .expect_err("no such record");
    assert!(
        matches!(
            &missing,
            OrbitError::NotFound { kind: orbit_common::NotFoundKind::Friction, id } if id == "F2099-01-001"
        ),
        "{missing:?}"
    );

    let malformed = run_tool_as_operator(
        &runtime,
        "orbit.friction.resolve",
        json!({ "id": "malformed-id" }),
    )
    .expect_err("malformed id");
    assert!(
        matches!(malformed, OrbitError::InvalidInput(_)),
        "{malformed:?}"
    );
}

#[test]
fn rehome_of_a_missing_record_is_not_found_and_malformed_id_is_invalid_input() {
    let (_temp, product, _owner) = product_and_owner();

    let missing = run_tool_as_operator(
        &product,
        "orbit.friction.rehome",
        json!({ "id": "F2099-01-001", "to_workspace": "platform" }),
    )
    .expect_err("no such record");
    assert!(
        matches!(
            &missing,
            OrbitError::NotFound { kind: orbit_common::NotFoundKind::Friction, id } if id == "F2099-01-001"
        ),
        "{missing:?}"
    );

    let malformed = run_tool_as_operator(
        &product,
        "orbit.friction.rehome",
        json!({ "id": "malformed-id", "to_workspace": "platform" }),
    )
    .expect_err("malformed id");
    assert!(
        matches!(malformed, OrbitError::InvalidInput(_)),
        "{malformed:?}"
    );
}
