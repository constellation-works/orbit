//! The proposals board is a read. Recently closed merges virtual expiries
//! and decisions by effective close time, and listing writes nothing.
//! An operator's task grant admits that task's run to read any space.

use chrono::{DateTime, Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::application::crew_memory::{MemoryFile, MemoryToolCaller, MemoryWrite};
use orbit_core::application::task::TaskAddParams;
use orbit_store::contracts::{
    CrewMemoryClosedProposalRow, CrewMemoryEventRow, CrewMemoryProposalDecision,
    CrewMemoryProposalOutcome, CrewMemoryProposalRow, CrewMemoryStoreBackend,
};
use serde_json::Value;

const RECENTLY_CLOSED_LIMIT: usize = 20;

fn runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("proposal board tempdir");
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::create_dir_all(&workspace).expect("workspace root");
    std::fs::write(
        workspace.join("config.toml"),
        "[crew_memory]\nenabled = true\n",
    )
    .expect("enable crew memory");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("runtime");
    assert!(
        runtime.current_crew_memory_enabled(),
        "the board is empty unless crew memory is enabled"
    );
    (root, runtime)
}

fn open_proposal(
    store: &orbit_store::Store,
    workspace_id: &str,
    id: &str,
    created_at: DateTime<Utc>,
) {
    let outcome = store
        .open_crew_memory_proposal(
            &CrewMemoryProposalRow {
                workspace_id: workspace_id.to_string(),
                id: id.to_string(),
                proposing_key: "model-a".to_string(),
                proposing_run_id: None,
                text: format!("note {id}"),
                evidence_json: "[]".to_string(),
                status: "open".to_string(),
                created_at,
            },
            &audit(workspace_id, created_at, "proposal_opened", id),
            u32::MAX,
        )
        .expect("seed proposal");
    assert!(
        matches!(outcome, CrewMemoryProposalOutcome::Opened),
        "seeded proposal {id} must open"
    );
}

fn audit(workspace_id: &str, at: DateTime<Utc>, action: &str, id: &str) -> CrewMemoryEventRow {
    CrewMemoryEventRow {
        workspace_id: workspace_id.to_string(),
        memory_key: "global".to_string(),
        at,
        action: action.to_string(),
        actor: "fixture".to_string(),
        revision: None,
        detail_json: serde_json::json!({ "proposal_id": id }).to_string(),
    }
}

struct StoredMemory {
    open: Vec<CrewMemoryProposalRow>,
    closed: Vec<CrewMemoryClosedProposalRow>,
    events: Vec<CrewMemoryEventRow>,
}

fn stored_memory(store: &orbit_store::Store, workspace_id: &str) -> StoredMemory {
    StoredMemory {
        open: store
            .crew_memory_open_proposals(workspace_id)
            .expect("read open proposals"),
        closed: store
            .crew_memory_closed_proposals(workspace_id, 100)
            .expect("read decided proposals"),
        events: store
            .crew_memory_events(workspace_id, "global")
            .expect("read audit events"),
    }
}

fn closed_ids(board: &Value) -> Vec<String> {
    board["recently_closed"]
        .as_array()
        .expect("recently_closed is an array")
        .iter()
        .map(|row| row["id"].as_str().expect("proposal id").to_string())
        .collect()
}

/// The public propose path stamps `Utc::now()`, so this writes the owner
/// store directly. Close times, newest first: an expiry one day ago, an
/// acceptance two days ago, two expiries three days ago, then seventeen
/// older expiries one day apart.
fn seed_lapsed_and_acceptance(store: &orbit_store::Store, workspace_id: &str) {
    let now = Utc::now();
    let lapse = Duration::weeks(4);
    open_proposal(
        store,
        workspace_id,
        "lapsed-recent",
        now - lapse - Duration::days(1),
    );
    let decided_at = now - Duration::days(2);
    open_proposal(store, workspace_id, "accepted-mid", decided_at);
    store
        .decide_crew_memory_proposal(
            &CrewMemoryProposalDecision {
                workspace_id: workspace_id.to_string(),
                id: "accepted-mid".to_string(),
                status: "accepted".to_string(),
                actor: "operator".to_string(),
                at: decided_at,
                reason: "kept".to_string(),
            },
            &audit(
                workspace_id,
                decided_at,
                "proposal_accepted",
                "accepted-mid",
            ),
            None,
        )
        .expect("seed acceptance");
    let tie_at = now - lapse - Duration::days(3);
    open_proposal(store, workspace_id, "lapsed-tie-a", tie_at);
    open_proposal(store, workspace_id, "lapsed-tie-b", tie_at);
    for index in 0..17 {
        open_proposal(
            store,
            workspace_id,
            &format!("lapsed-{index:02}"),
            tie_at - Duration::days(index as i64 + 1),
        );
    }
}

/// Twenty stored-open proposals are already past four weeks, and one
/// acceptance sits between a newer virtual expiry and those older ones.
fn recent_decision_stays_ahead_of_older_expiries() {
    let (_root, runtime) = runtime();
    let store = runtime.sqlite_store().expect("store");
    let workspace_id = runtime.workspace_id().expect("workspace");
    seed_lapsed_and_acceptance(&store, &workspace_id);

    let before = stored_memory(&store, &workspace_id);
    assert_eq!(
        before.open.len(),
        RECENTLY_CLOSED_LIMIT,
        "the limit is already filled by stored-open proposals past four weeks"
    );
    assert!(
        before.open.iter().all(|row| row.status == "open"),
        "lapsed proposals stay stored as open until an operator list or decision"
    );
    assert_eq!(before.closed.len(), 1);
    assert_eq!(before.closed[0].proposal.id, "accepted-mid");
    assert_eq!(before.closed[0].proposal.status, "accepted");

    let board = runtime
        .crew_memory_proposal_board()
        .expect("proposal board");
    let after = stored_memory(&store, &workspace_id);
    assert_eq!(
        after.open, before.open,
        "listing the board must not change stored proposal rows"
    );
    assert_eq!(
        after.closed, before.closed,
        "listing the board must not decide or withdraw proposals"
    );
    assert_eq!(
        after.events, before.events,
        "listing the board must not append audit events"
    );

    assert_eq!(board["enabled"].as_bool(), Some(true));
    assert_eq!(
        board["open"].as_array().map(Vec::len),
        Some(0),
        "lapsed proposals leave the open list without being withdrawn"
    );

    let mut expected = vec![
        "lapsed-recent".to_string(),
        "accepted-mid".to_string(),
        "lapsed-tie-b".to_string(),
        "lapsed-tie-a".to_string(),
    ];
    for index in 0..16 {
        expected.push(format!("lapsed-{index:02}"));
    }
    assert_eq!(expected.len(), RECENTLY_CLOSED_LIMIT);
    let ids = closed_ids(&board);
    assert_eq!(
        ids, expected,
        "recently_closed merges expires_at and decided_at, newest first, id descending on a tie, then keeps twenty"
    );
    assert!(
        !ids.iter().any(|id| id == "lapsed-16"),
        "the oldest expiry loses the last slot to the newer decision"
    );

    let closed = board["recently_closed"]
        .as_array()
        .expect("recently_closed");
    for row in closed {
        let id = row["id"].as_str().expect("id");
        if id == "accepted-mid" {
            assert_eq!(row["status"], "accepted");
            assert_eq!(row["decision_reason"], "kept");
            continue;
        }
        assert_eq!(row["status"], "expired", "{id}");
        assert!(row["decided_at"].is_null(), "{id}");
        assert!(row["decided_by"].is_null(), "{id}");
        assert_eq!(row["decision_reason"], "expired after 4 weeks", "{id}");
    }
}

crate::scenario_table!(
    proposal_board,
    recent_decision_stays_ahead_of_older_expiries,
);

/// A task that is neither dreaming nor in a memory arm, whose operator grant
/// names `orbit.memory.show`, reads a model space that is not its run's own.
fn granted_run_reads_another_models_space() {
    let (_root, runtime) = runtime();
    runtime
        .crew_memory_write(
            "model-b",
            MemoryWrite {
                files: vec![MemoryFile {
                    path: "notes.md".to_string(),
                    text: "Check the lockfile before changing a dependency.\n".to_string(),
                }],
                evidence: Vec::new(),
            },
            None,
        )
        .expect("operator seeds model-b's space");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Consolidate crew memory".into(),
            granted_tools: vec!["orbit.memory.show".into()],
            ..Default::default()
        })
        .expect("task with a memory read grant");
    let run = MemoryToolCaller {
        agent: true,
        scope_task_id: Some(task.id.to_string()),
        scope_run_id: Some("jrun-granted-read".to_string()),
        model: Some("model-a".to_string()),
        ..Default::default()
    };

    let shown = runtime
        .crew_memory_tool_show(&run, Some("model-b"), None)
        .expect("the grant admits the run to another model's space");
    assert_eq!(shown["key"], "model-b");
    assert_eq!(shown["revision"], 1);
    assert_eq!(shown["files"][0]["path"], "notes.md");
}

crate::scenario_table!(memory_read_grant, granted_run_reads_another_models_space,);
