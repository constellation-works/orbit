//! Bodyless metadata across the public bound-worker/owner boundary.
use super::claimed_review::{ReviewedLeaf, ToOwner};
use super::*;
use orbit_core::TaskStatus;
use orbit_core::application::task::TaskAddParams;

const CEILING: usize = 6000;

struct MetadataWire {
    owner: OrbitRuntime,
    requests: Mutex<Vec<Value>>,
    responses: Mutex<Vec<Value>>,
    limits: Mutex<(usize, usize)>,
    fault: Mutex<Option<&'static str>>,
}

impl OwnerCoordinator for MetadataWire {
    fn call(
        &self,
        name: &str,
        mut input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.requests.lock().unwrap().push(input.clone());
        if input["_worker_read"] == "history"
            && *self.fault.lock().unwrap() == Some("history-failed")
        {
            return Err(OrbitError::UnreachableDestination(
                "history unavailable".into(),
            ));
        }
        if input["_worker_read"] == "task_metadata"
            && !input["metadata_page"]["cursor"].is_null()
            && *self.fault.lock().unwrap() == Some("scan-change")
        {
            self.owner.add_task(TaskAddParams {
                title: "Deterministic scan mutation".into(),
                ..Default::default()
            })?;
        }
        if input["_worker_read"] == "task_metadata" {
            let (rows, bytes) = *self.limits.lock().unwrap();
            input["metadata_page"]["max_rows"] = json!(rows);
            input["metadata_page"]["max_bytes"] = json!(bytes);
        }
        let continuation =
            input["_worker_read"] == "task_metadata" && !input["metadata_page"]["cursor"].is_null();
        let cursor = input["metadata_page"]["cursor"].clone();
        let mut value = ToOwner(self.owner.clone()).call(name, input, session)?;
        if continuation {
            match *self.fault.lock().unwrap() {
                Some("malformed") => value = json!([]),
                Some("missing-complete") => {
                    value.as_object_mut().unwrap().remove("complete");
                }
                Some("empty") => value["items"] = json!([]),
                Some("premature-complete") => {
                    value["complete"] = json!(true);
                    value["next"] = Value::Null;
                }
                Some("repeat-cursor") => value["next"] = cursor,
                Some("identity") => value["scan_id"] = json!("0".repeat(64)),
                Some("missing-next") => value["next"] = Value::Null,
                Some("count") => value["remaining"] = json!(0),
                Some("body") => value["items"][0]["description"] = json!("BODY_SECRET"),
                Some("unknown") => {
                    return Err(OrbitError::OutcomeUnknown {
                        mcp_call_id: "fixture/metadata".into(),
                        message: "unknown read outcome".into(),
                    });
                }
                Some("failed") => {
                    return Err(OrbitError::UnreachableDestination(
                        "owner unavailable".into(),
                    ));
                }
                Some("tool-error") => {
                    return Ok(
                        json!({"error": {"code": "store_error", "message": "owner read failed"}}),
                    );
                }
                _ => {}
            }
        }
        if serde_json::to_vec(&value).unwrap().len() > CEILING {
            return Err(OrbitError::OutcomeUnknown {
                mcp_call_id: "fixture/metadata".into(),
                message: "deterministic response ceiling exceeded".into(),
            });
        }
        self.responses.lock().unwrap().push(value.clone());
        Ok(value)
    }
}

fn metadata_fixture() -> (
    ReviewedLeaf,
    Arc<MetadataWire>,
    OrbitRuntime,
    BTreeSet<String>,
) {
    let leaf = ReviewedLeaf::admit();
    let owner = &leaf.pair.wire.owner;
    let mut ids = BTreeSet::new();
    for n in 0..7 {
        let task = owner
            .add_task(TaskAddParams {
                title: format!("metadata {n}"),
                description: "BODY_SECRET".repeat(100),
                plan: "PLAN_SECRET".repeat(50),
                acceptance_criteria: vec!["CRITERIA_SECRET".repeat(50)],
                context_files: vec!["file:src/hot.rs".into()],
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .unwrap();
        ids.insert(task.id);
    }
    let wire = Arc::new(MetadataWire {
        owner: owner.clone(),
        requests: Mutex::new(Vec::new()),
        responses: Mutex::new(Vec::new()),
        limits: Mutex::new((2, CEILING)),
        fault: Mutex::new(None),
    });
    let bound = leaf
        .bound
        .clone()
        .with_worker_invocation(
            leaf.bound.worker_invocation().unwrap().clone(),
            wire.clone(),
        )
        .unwrap();
    (leaf, wire, bound, ids)
}

fn worker_metadata_is_complete_bodyless_and_bounded() {
    let (leaf, wire, bound, ids) = metadata_fixture();
    let owner = &leaf.pair.wire.owner;
    // The legacy route cannot fit these modest bodies through the same wire.
    assert!(bound.list_tasks().is_err());
    wire.requests.lock().unwrap().clear();
    let metadata = bound
        .list_task_metadata()
        .expect("metadata must fit without forwarding bodies");
    let expected = owner.list_task_metadata().unwrap();
    assert_eq!(
        serde_json::to_value(&metadata).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    for task in &metadata {
        assert!(
            task.description.is_empty()
                && task.plan.is_empty()
                && task.execution_summary.is_empty()
                && task.acceptance_criteria.is_empty()
        );
    }
    assert!(wire.responses.lock().unwrap().len() > 1);
    assert!(
        wire.responses
            .lock()
            .unwrap()
            .iter()
            .all(|page| page["items"].as_array().unwrap().len() <= 2)
    );
    assert!(
        wire.requests
            .lock()
            .unwrap()
            .iter()
            .all(|input| input["_worker_read"] == "task_metadata")
    );
    let selected = BTreeSet::from([
        ids.first().unwrap().clone(),
        ids.last().unwrap().clone(),
        "TSO-99999".into(),
    ]);
    wire.requests.lock().unwrap().clear();
    let targeted = bound.list_task_metadata_for_ids(&selected).unwrap();
    assert_eq!(targeted.len(), 2);
    assert!(targeted.iter().all(|task| selected.contains(&task.id)));
    assert!(
        wire.requests
            .lock()
            .unwrap()
            .iter()
            .all(|input| input["metadata_page"]["ids"].is_array())
    );
    let expected_targeted = owner.list_task_metadata_for_ids(&selected).unwrap();
    assert_eq!(
        serde_json::to_value(
            targeted
                .iter()
                .map(|task| (&task.id, task))
                .collect::<BTreeMap<_, _>>()
        )
        .unwrap(),
        serde_json::to_value(
            expected_targeted
                .iter()
                .map(|task| (&task.id, task))
                .collect::<BTreeMap<_, _>>()
        )
        .unwrap()
    );
    wire.requests.lock().unwrap().clear();
    assert!(
        bound
            .list_task_metadata_for_ids(&BTreeSet::new())
            .unwrap()
            .is_empty()
    );
    assert!(wire.requests.lock().unwrap().is_empty());
    assert!(
        bound
            .list_task_metadata_for_ids(&BTreeSet::from(["TSO-99999".into()]))
            .unwrap()
            .is_empty()
    );

    let mut many_ids = ids.clone();
    many_ids.extend((10000..10260).map(|n| format!("TSO-{n}")));
    wire.requests.lock().unwrap().clear();
    assert_eq!(
        bound.list_task_metadata_for_ids(&many_ids).unwrap().len(),
        ids.len()
    );
    assert!(
        wire.requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request["metadata_page"]["ids"].as_array().unwrap().len() <= 128)
    );

    // Exercise the byte bound independently of the row bound.
    *wire.limits.lock().unwrap() = (128, 1800);
    wire.responses.lock().unwrap().clear();
    assert_eq!(bound.list_task_metadata().unwrap().len(), metadata.len());
    assert!(
        wire.responses
            .lock()
            .unwrap()
            .iter()
            .all(|page| serde_json::to_vec(page).unwrap().len() <= 1800)
    );

    // Ordinary full-task reads retain their bodies on the unbounded fixture.
    let full = leaf.bound.get_task(ids.first().unwrap()).unwrap();
    assert!(
        !full.description.is_empty()
            && !full.plan.is_empty()
            && !full.acceptance_criteria.is_empty()
    );
    assert!(
        leaf.bound
            .run_tool(
                "orbit.task.show",
                json!({"id": leaf.task, "_worker_read": "task_metadata", "metadata_page": {}})
            )
            .is_err()
    );
}

crate::scenario_table!(
    worker_metadata_scenarios,
    worker_metadata_is_complete_bodyless_and_bounded,
    worker_metadata_failures_never_return_a_partial_scan,
    worker_metadata_regression_marks_match_the_owner
);

fn worker_metadata_failures_never_return_a_partial_scan() {
    let (leaf, wire, bound, ids) = metadata_fixture();
    for fault in [
        "malformed",
        "missing-complete",
        "empty",
        "premature-complete",
        "repeat-cursor",
        "identity",
        "missing-next",
        "count",
        "body",
        "unknown",
        "failed",
        "tool-error",
    ] {
        *wire.fault.lock().unwrap() = Some(fault);
        wire.requests.lock().unwrap().clear();
        assert!(
            bound.list_task_metadata().is_err(),
            "{fault} cannot return partial metadata"
        );
        assert_eq!(
            wire.requests.lock().unwrap().len(),
            2,
            "{fault}: no fallback or replay"
        );
        assert!(
            bound.list_task_metadata_for_ids(&ids).is_err(),
            "{fault}: targeted metadata fails too"
        );
        assert_eq!(
            RuntimeHost::regression_risk_for_agent_input(
                &bound,
                "metadata-failure",
                "agent_implement",
                &json!({"task_id": ids.first().unwrap()})
            ),
            None,
            "{fault}: marks remain advisory"
        );
    }
    *wire.fault.lock().unwrap() = Some("scan-change");
    assert!(
        bound
            .list_task_metadata()
            .unwrap_err()
            .to_string()
            .contains("scan identity changed")
    );
    *wire.fault.lock().unwrap() = None;
    let owner = &leaf.pair.wire.owner;
    let oversized = owner
        .add_task(TaskAddParams {
            title: "oversized metadata row ".repeat(500),
            description: "small body".into(),
            ..Default::default()
        })
        .unwrap();
    let error = bound.list_task_metadata().unwrap_err().to_string();
    assert!(error.contains("one metadata row"), "{error}");
    assert!(
        bound
            .list_task_metadata_for_ids(&BTreeSet::from([oversized.id]))
            .is_err()
    );
    assert_eq!(
        bound.list_task_metadata_for_ids(&ids).unwrap().len(),
        ids.len(),
        "unrelated oversized rows cannot affect keyed reads"
    );

    let mut binding = bound.worker_invocation().unwrap().clone();
    binding.owner_workspace_id = "ws_wrong".into();
    let session = ToolSessionContext {
        worker_invocation: Some(binding),
        worker_host_call: true,
        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
        ..Default::default()
    };
    assert!(matches!(
        owner.execute_owner_coordination(
            "orbit.task.show",
            json!({"_worker_read": "task_metadata", "metadata_page": {
                "schema_version": 1, "ids": [ids.first().unwrap()],
                "scan_id": null, "cursor": null, "max_rows": 2, "max_bytes": CEILING,
            }}),
            session,
        ),
        Err(OrbitError::PolicyDenied(_))
    ));
}

fn worker_metadata_regression_marks_match_the_owner() {
    use orbit_types::task::{
        DefectSeverity, RegressionClassification, RegressionEscape, RegressionNature,
        SeveritySource, TaskRelation, TaskRelationType,
    };
    let (leaf, wire, bound, ids) = metadata_fixture();
    let owner = &leaf.pair.wire.owner;
    for (index, target) in ids.iter().take(3).enumerate() {
        owner
            .add_task(TaskAddParams {
                title: if index == 2 {
                    format!("{target}: Restore the bounded retry lesson")
                } else {
                    "Restore metadata classification".into()
                },
                description: "small regression body".into(),
                relations: vec![TaskRelation {
                    relation_type: TaskRelationType::RegressionFrom,
                    target: target.parse().unwrap(),
                    regression: Some(RegressionClassification {
                        nature: RegressionNature::BrokeExisting,
                        escape: RegressionEscape::NoTest,
                        severity: DefectSeverity::Medium,
                        severity_source: SeveritySource::Filer,
                        sites: vec!["file:src/hot.rs".into()],
                    }),
                }],
                ..Default::default()
            })
            .unwrap();
    }
    *wire.fault.lock().unwrap() = Some("history-failed");
    assert_eq!(
        RuntimeHost::regression_risk_for_agent_input(
            &bound,
            "history-failure",
            "agent_implement",
            &json!({"task_id": ids.first().unwrap()})
        ),
        None,
        "a failed history read must not become a successful analytics scan"
    );
    *wire.fault.lock().unwrap() = None;
    // These tasks have recent update stamps but entered done outside the window.
    // The separate history read must exclude them from the denominator.
    for n in 0..5 {
        let old = owner
            .add_task(TaskAddParams {
                title: format!("old hot {n}"),
                description: "small old body".into(),
                context_files: vec!["file:src/hot.rs".into()],
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .unwrap();
        let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
            &orbit_store::maintenance::task_registry::task_registry_path(&owner.global_root()),
        )
        .unwrap();
        let events = registry
            .find_task_binding(&old.id)
            .unwrap()
            .unwrap()
            .canonical_path
            .join(orbit_types::task::TASK_EVENTS_FILE_NAME);
        let at = (Utc::now() - chrono::Duration::days(60)).to_rfc3339();
        let rows: String = std::fs::read_to_string(&events)
            .unwrap()
            .lines()
            .map(|line| {
                let mut row: Value = serde_json::from_str(line).unwrap();
                row["at"] = json!(at);
                format!("{row}\n")
            })
            .collect();
        std::fs::write(events, rows).unwrap();
    }
    let marks = |runtime: &OrbitRuntime| {
        RuntimeHost::regression_risk_for_agent_input(
            runtime,
            "bounded-metadata",
            "agent_implement",
            &json!({"task_id": ids.first().unwrap()}),
        )
    };
    let expected = marks(owner).expect("the owner has one qualifying hotspot");
    wire.requests.lock().unwrap().clear();
    assert_eq!(
        serde_json::to_value(bound.list_task_metadata().unwrap()).unwrap(),
        serde_json::to_value(owner.list_task_metadata().unwrap()).unwrap(),
        "all envelope fields, including regression sites and attribution, survive the wire"
    );
    assert_eq!(marks(&bound), Some(expected.clone()));
    *wire.fault.lock().unwrap() = Some("identity");
    assert_eq!(
        marks(&bound),
        None,
        "a failed metadata scan with a known hotspot omits advisory marks"
    );
    *wire.fault.lock().unwrap() = None;
    assert_eq!(expected["marks"][0]["regressed"], "3 of 7 touching tasks");
    assert_eq!(expected["marks"][0]["nature"], json!(["broke_existing"]));
    assert_eq!(expected["marks"][0]["escape"], json!(["no_test"]));
    assert_eq!(
        expected["marks"][0]["lesson"],
        "Restore the bounded retry lesson"
    );
    let requests = wire.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|request| request["_worker_read"] == "history"),
        "history remains a separate read"
    );
    assert!(
        requests
            .iter()
            .all(|request| request["_worker_read"] != "tasks"),
        "no whole-corpus body read"
    );
}
