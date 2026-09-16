use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;

use super::*;
use crate::application::search::federated::{
    MAX_FEDERATED_CONCURRENCY, MAX_FEDERATED_WORKSPACES, apply_workspace_cap,
    describe_model_mismatch, ensure_federated_scope_permitted, ensure_federated_scope_supported,
    federated_worker_count, with_managed_run_override,
};
use crate::runtime::workspace_catalog::{
    FederatedWorkspaceTarget, WorkspaceCatalog, WorkspaceScope,
};

/// A catalog over runtimes the test already built, so the fan-out is exercised
/// without a workspace registry on disk.
struct FakeCatalog {
    entries: Vec<(FederatedWorkspaceTarget, Option<OrbitRuntime>)>,
}

impl FakeCatalog {
    fn new(entries: Vec<(FederatedWorkspaceTarget, Option<OrbitRuntime>)>) -> Arc<Self> {
        Arc::new(Self { entries })
    }
}

impl WorkspaceCatalog for FakeCatalog {
    fn resolve_scope(
        &self,
        _scope: &WorkspaceScope,
    ) -> Result<Vec<FederatedWorkspaceTarget>, OrbitError> {
        Ok(self
            .entries
            .iter()
            .map(|(target, _)| target.clone())
            .collect())
    }

    fn open(&self, target: &FederatedWorkspaceTarget) -> Result<OrbitRuntime, OrbitError> {
        self.entries
            .iter()
            .find(|(candidate, _)| candidate.workspace_id == target.workspace_id)
            .and_then(|(_, runtime)| runtime.clone())
            .ok_or_else(|| {
                OrbitError::WorkspaceError(format!("checkout for '{}' is gone", target.name))
            })
    }
}

/// A catalog whose `open` blocks until `expected` opens are in flight, so a
/// serial fan-out cannot satisfy it and a concurrent one settles immediately.
///
/// The wait is bounded: a regression to serial execution fails the assertion
/// on the observed peak rather than hanging the suite.
struct BarrierCatalog {
    entries: Vec<(FederatedWorkspaceTarget, OrbitRuntime)>,
    expected: usize,
    state: Mutex<InFlight>,
    settled: Condvar,
}

#[derive(Default)]
struct InFlight {
    current: usize,
    peak: usize,
}

impl BarrierCatalog {
    fn new(entries: Vec<(FederatedWorkspaceTarget, OrbitRuntime)>, expected: usize) -> Arc<Self> {
        Arc::new(Self {
            entries,
            expected,
            state: Mutex::new(InFlight::default()),
            settled: Condvar::new(),
        })
    }

    fn peak(&self) -> usize {
        self.state.lock().expect("in-flight state").peak
    }
}

impl WorkspaceCatalog for BarrierCatalog {
    fn resolve_scope(
        &self,
        _scope: &WorkspaceScope,
    ) -> Result<Vec<FederatedWorkspaceTarget>, OrbitError> {
        Ok(self
            .entries
            .iter()
            .map(|(target, _)| target.clone())
            .collect())
    }

    fn open(&self, target: &FederatedWorkspaceTarget) -> Result<OrbitRuntime, OrbitError> {
        let mut state = self.state.lock().expect("in-flight state");
        state.current += 1;
        state.peak = state.peak.max(state.current);
        self.settled.notify_all();
        while state.current < self.expected {
            let (guard, timeout) = self
                .settled
                .wait_timeout(state, Duration::from_secs(5))
                .expect("in-flight state");
            state = guard;
            if timeout.timed_out() {
                break;
            }
        }
        state.current -= 1;
        drop(state);

        self.entries
            .iter()
            .find(|(candidate, _)| candidate.workspace_id == target.workspace_id)
            .map(|(_, runtime)| runtime.clone())
            .ok_or_else(|| {
                OrbitError::WorkspaceError(format!("checkout for '{}' is gone", target.name))
            })
    }
}

fn target(name: &str) -> FederatedWorkspaceTarget {
    FederatedWorkspaceTarget {
        workspace_id: format!("ws_{name}"),
        name: name.to_string(),
        repo_root: PathBuf::from(format!("/checkouts/{name}")),
    }
}

fn seeded_runtime(query: &str, task_count: usize) -> OrbitRuntime {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    for index in 0..task_count {
        add_task_with_status(
            &runtime,
            &format!("{query} task {index:02}"),
            TaskStatus::Backlog,
        );
    }
    runtime
}

fn federated_query(query: &str, limit: usize) -> GlobalSearchParams {
    GlobalSearchParams {
        query: Some(query.to_string()),
        kind: GlobalSearchKind::Task,
        limit,
        workspaces: WorkspaceScope::AllRegistered,
        ..Default::default()
    }
}

fn hub(catalog: Arc<FakeCatalog>) -> OrbitRuntime {
    OrbitRuntime::in_memory()
        .expect("hub runtime")
        .with_workspace_catalog(catalog)
}

#[test]
fn default_scope_keeps_the_single_workspace_response_shape() {
    let query = "unfederated";
    let runtime = seeded_runtime(query, 3);

    let response = runtime
        .global_search(GlobalSearchParams {
            query: Some(query.to_string()),
            kind: GlobalSearchKind::Task,
            limit: 5,
            ..Default::default()
        })
        .expect("search");

    assert!(!response.results.is_empty());
    assert!(response.results.iter().all(|hit| hit.workspace.is_none()));
    assert!(response.workspaces.is_empty());

    // The serialized shape is the compatibility contract for the HTTP adapter
    // and every MCP caller: neither new field may appear by default.
    let json = serde_json::to_value(&response).expect("serialize");
    assert!(json.get("workspaces").is_none());
    assert!(
        json["results"]
            .as_array()
            .expect("results")
            .iter()
            .all(|hit| hit.get("workspace").is_none())
    );
}

#[test]
fn federated_search_fuses_and_attributes_hits_from_every_workspace() {
    let query = "fusewitness";
    let catalog = FakeCatalog::new(vec![
        (target("alpha"), Some(seeded_runtime(query, 4))),
        (target("beta"), Some(seeded_runtime(query, 4))),
    ]);
    let runtime = hub(catalog);

    let response = with_managed_run_override(false, || {
        runtime
            .global_search(federated_query(query, 4))
            .expect("federated search")
    });

    assert_eq!(response.results.len(), 4);
    let names = response
        .results
        .iter()
        .map(|hit| {
            hit.workspace
                .as_ref()
                .expect("every federated hit is attributed")
                .name
                .clone()
        })
        .collect::<Vec<_>>();
    // Rank interleaving, so both workspaces are represented rather than the
    // first one filling the budget.
    assert_eq!(names, vec!["alpha", "beta", "alpha", "beta"]);
    assert_eq!(
        response
            .results
            .iter()
            .filter_map(|hit| hit.workspace.as_ref())
            .map(|workspace| workspace.workspace_id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["ws_alpha", "ws_beta"])
    );
    assert_eq!(
        response
            .workspaces
            .iter()
            .map(|report| (report.name.as_str(), report.hits))
            .collect::<Vec<_>>(),
        vec![("alpha", 4), ("beta", 4)]
    );
}

#[test]
fn a_small_workspace_is_not_crowded_out_by_a_large_one() {
    let query = "crowding";
    let catalog = FakeCatalog::new(vec![
        (target("big"), Some(seeded_runtime(query, 12))),
        (target("small"), Some(seeded_runtime(query, 1))),
    ]);
    let runtime = hub(catalog);

    let response = with_managed_run_override(false, || {
        runtime
            .global_search(federated_query(query, 6))
            .expect("federated search")
    });

    assert_eq!(response.results.len(), 6);
    assert!(
        response
            .results
            .iter()
            .any(|hit| hit.workspace.as_ref().is_some_and(|ws| ws.name == "small")),
        "the single-hit workspace must survive the budget"
    );
}

#[test]
fn an_unopenable_workspace_degrades_to_a_note_without_failing_the_query() {
    let query = "degrading";
    let catalog = FakeCatalog::new(vec![
        (target("healthy"), Some(seeded_runtime(query, 3))),
        (target("stale"), None),
    ]);
    let runtime = hub(catalog);

    let response = with_managed_run_override(false, || {
        runtime
            .global_search(federated_query(query, 5))
            .expect("a broken checkout must not fail the query")
    });

    assert!(!response.results.is_empty());
    assert!(response.results.iter().all(|hit| {
        hit.workspace
            .as_ref()
            .is_some_and(|ws| ws.name == "healthy")
    }));
    let stale = response
        .workspaces
        .iter()
        .find(|report| report.name == "stale")
        .expect("the skipped workspace still reports");
    assert_eq!(stale.hits, 0);
    assert!(
        stale
            .note
            .as_deref()
            .is_some_and(|note| note.contains("skipped"))
    );
    assert!(
        response
            .notes
            .iter()
            .any(|note| note.starts_with("[stale]")),
        "every note names the workspace it came from: {:?}",
        response.notes
    );
}

#[test]
fn federated_scope_without_a_catalog_is_refused_by_name() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");

    let error = with_managed_run_override(false, || {
        runtime
            .global_search(federated_query("nocatalog", 5))
            .expect_err("a catalog-less runtime cannot federate")
    });

    assert!(error.to_string().contains("registry-backed runtime"));
}

#[test]
fn an_empty_scope_returns_no_hits_and_says_so() {
    let catalog = FakeCatalog::new(Vec::new());
    let runtime = hub(catalog);

    let response = with_managed_run_override(false, || {
        runtime
            .global_search(federated_query("nothing", 5))
            .expect("empty scope is not an error")
    });

    assert!(response.results.is_empty());
    assert!(
        response
            .notes
            .iter()
            .any(|note| note.contains("no registered workspace"))
    );
}

#[test]
fn per_workspace_queries_run_concurrently() {
    let query = "concurrent";
    let entries = (0..4)
        .map(|index| (target(&format!("ws{index}")), seeded_runtime(query, 2)))
        .collect::<Vec<_>>();
    let expected = entries.len();
    let catalog = BarrierCatalog::new(entries, expected);
    let runtime = OrbitRuntime::in_memory()
        .expect("hub runtime")
        .with_workspace_catalog(Arc::clone(&catalog) as Arc<dyn WorkspaceCatalog>);

    let response = with_managed_run_override(false, || {
        runtime
            .global_search(federated_query(query, 8))
            .expect("federated search")
    });

    assert_eq!(
        catalog.peak(),
        expected,
        "every workspace query must be able to run at once under the pool bound"
    );
    // Ordering is the target order, not the completion order.
    assert_eq!(
        response
            .workspaces
            .iter()
            .map(|report| report.name.as_str())
            .collect::<Vec<_>>(),
        vec!["ws0", "ws1", "ws2", "ws3"]
    );
}

#[test]
fn the_fan_out_pool_is_bounded_and_never_wider_than_the_scope() {
    assert_eq!(federated_worker_count(0), 0);
    assert_eq!(federated_worker_count(1), 1);
    assert_eq!(federated_worker_count(3), 3);
    assert_eq!(
        federated_worker_count(MAX_FEDERATED_WORKSPACES),
        MAX_FEDERATED_CONCURRENCY,
        "the widest possible scope still runs on a bounded pool"
    );
    const { assert!(MAX_FEDERATED_CONCURRENCY <= MAX_FEDERATED_WORKSPACES) };
}

#[test]
fn workspace_fan_out_cap_is_announced_never_silent() {
    let mut none = (0..MAX_FEDERATED_WORKSPACES)
        .map(|index| target(&format!("ws{index:02}")))
        .collect::<Vec<_>>();
    assert!(apply_workspace_cap(&mut none).is_none());
    assert_eq!(none.len(), MAX_FEDERATED_WORKSPACES);

    let mut over = (0..MAX_FEDERATED_WORKSPACES + 3)
        .map(|index| target(&format!("ws{index:02}")))
        .collect::<Vec<_>>();
    let note = apply_workspace_cap(&mut over).expect("truncation must be reported");
    assert_eq!(over.len(), MAX_FEDERATED_WORKSPACES);
    assert!(note.contains(&MAX_FEDERATED_WORKSPACES.to_string()));
    assert!(note.contains('3'));
}

#[test]
fn model_mismatch_is_reported_and_a_shared_model_is_not() {
    let indexed = BTreeSet::from(["minilm-l6".to_string()]);
    let note = describe_model_mismatch(&indexed, "bge-small").expect("mismatch must be reported");
    assert!(note.contains("minilm-l6"));
    assert!(note.contains("bge-small"));
    assert!(note.contains("cosine"));

    assert!(describe_model_mismatch(&indexed, "minilm-l6").is_none());
    // An unindexed workspace is not a mismatch — it simply has no vectors, and
    // the existing hybrid fallback already narrates that.
    assert!(describe_model_mismatch(&BTreeSet::new(), "bge-small").is_none());
}

#[test]
fn federated_scope_is_denied_inside_a_managed_run() {
    let error = ensure_federated_scope_permitted(true)
        .expect_err("a managed run may only read its own workspace index");
    assert!(error.to_string().contains("Orbit-managed run"));
    assert!(ensure_federated_scope_permitted(false).is_ok());
}

#[test]
fn neighbor_and_path_lookups_refuse_a_federated_scope() {
    let semantic = GlobalSearchParams {
        semantic: Some("ORB-00001".to_string()),
        workspaces: WorkspaceScope::AllRegistered,
        ..Default::default()
    };
    assert!(
        ensure_federated_scope_supported(&semantic)
            .expect_err("neighbor lookup is single-workspace")
            .to_string()
            .contains("`semantic`")
    );

    let path = GlobalSearchParams {
        path: Some("src/main.rs".to_string()),
        workspaces: WorkspaceScope::AllRegistered,
        ..Default::default()
    };
    assert!(
        ensure_federated_scope_supported(&path)
            .expect_err("path lookup is single-workspace")
            .to_string()
            .contains("`path`")
    );

    assert!(ensure_federated_scope_supported(&federated_query("plain", 5)).is_ok());
}

#[test]
fn workspace_scope_reads_the_two_surface_inputs() {
    assert_eq!(
        WorkspaceScope::from_inputs(Vec::new(), false),
        WorkspaceScope::Current
    );
    // A blank selector must not silently federate.
    assert_eq!(
        WorkspaceScope::from_inputs(vec!["  ".to_string()], false),
        WorkspaceScope::Current
    );
    assert_eq!(
        WorkspaceScope::from_inputs(vec![" polaris ".to_string()], false),
        WorkspaceScope::Selectors(vec!["polaris".to_string()])
    );
    // "everything registered" wins over an explicit list.
    assert_eq!(
        WorkspaceScope::from_inputs(vec!["polaris".to_string()], true),
        WorkspaceScope::AllRegistered
    );
    assert!(!WorkspaceScope::Current.is_federated());
    assert!(WorkspaceScope::AllRegistered.is_federated());
}
