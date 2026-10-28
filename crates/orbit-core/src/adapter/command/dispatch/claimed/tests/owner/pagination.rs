//! Regression for the advertised pagination refused by the claimed-owner broker
//! (F2026-10-599). Exercise the real socket and the owner's public task tool,
//! including its bounded projection and validation rather than an echoed request.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use orbit_engine::PluginBrokerHandle;
use serde_json::{Value, json};

use super::super::review::{Fixture, GET, PUT};
use super::broker::{SHOW, TaskOwner, raw_call};

#[test]
fn claimed_task_detail_pages_preserve_broker_scope_and_owner_validation() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &claimed_task_detail_pages_preserve_broker_scope_and_owner_validation,
    )) {
        return;
    }
    let mut fixture = Fixture::new();
    let owner = Arc::new(TaskOwner::over(&fixture.runtime));
    let task = owner
        .execute(
            "orbit.task.add",
            json!({"title": "Paged task", "description": "Broker pagination regression", "complexity": "low"}),
        )
        .unwrap();
    let id = task["id"].as_str().unwrap();
    for index in 0..3 {
        owner
            .execute(
                "orbit.task.update",
                json!({"id": id, "comment": format!("comment {index}")}),
            )
            .unwrap();
        let source = fixture
            .worktree
            .join(format!(".orbit/tmp/page-{index}.txt"));
        std::fs::write(&source, format!("artifact {index}")).unwrap();
        owner
            .execute(
                PUT,
                json!({"id": id, "path": format!("page-{index}.txt"), "source_path": source}),
            )
            .unwrap();
    }
    fixture.binding.task_id = id.into();
    fixture.runtime = fixture
        .runtime
        .without_worker_routing()
        .with_worker_invocation(fixture.binding.clone(), owner.clone())
        .unwrap();
    let mut run = fixture.run("agent_implement");
    run.task_id = Some(id.into());
    let dispatch = Arc::new(
        crate::adapter::command::dispatch::brokered::RunDispatch::new(
            fixture.runtime.clone(),
            run.clone(),
        ),
    );
    let broker = crate::runtime::plugin::broker::PluginBroker::start(
        &fixture.global_root,
        &run.run_id,
        dispatch,
    )
    .unwrap();
    broker.bind_anchor(crate::runtime::plugin::broker::PeerAnchor::Ancestor(
        orbit_common::process::ancestry::process_start_key(std::process::id()).unwrap(),
    ));
    let call = |input| raw_call(&fixture, broker.socket_path(), SHOW, input);
    let first = call(json!({"id": id, "limit": 1})).unwrap();
    assert_eq!(first["task"]["id"], id);
    for field in ["comments", "history", "artifacts"] {
        assert_eq!(first[field].as_array().unwrap().len(), 1, "{field}");
        assert_eq!(first[format!("{field}_pagination")]["next_offset"], 1);
        let offset = format!("{field}_offset");
        let page = call(json!({"id": id, "limit": 1, &offset: 1})).unwrap();
        let direct = owner
            .execute(
                SHOW,
                json!({"id": id, "view": "bounded", "limit": 1, &offset: 1}),
            )
            .unwrap();
        assert_eq!(page[field], direct[field], "{offset} uses the owner's page");
        assert_eq!(page[field].as_array().unwrap().len(), 1, "{offset}");
        assert_ne!(page[field], first[field], "{offset} advances the page");
        assert_eq!(page[format!("{field}_pagination")]["offset"], 1);
        let empty = call(json!({"id": id, "limit": 1, &offset: 1000})).unwrap();
        assert_eq!(empty[field], json!([]), "{offset} past the last row");
        // An offset alone also selects bounded paging with the owner's default limit.
        let default_page = call(json!({"id": id, &offset: 1})).unwrap();
        assert_eq!(default_page[format!("{field}_pagination")]["limit"], 25);
        assert_eq!(default_page[field][0], page[field][0]);
    }
    assert!(call(json!({"id": id, "limit": 50})).is_ok());

    // A page of a task the owner does not hold in the claimed workspace is
    // refused as outside the claim, after the owner's own answer [ORB-19270].
    let error = call(json!({"id": "TSX-404", "limit": 1})).unwrap_err();
    assert!(
        error.contains("claimed_owner_bridge_refused")
            && error.contains("is not a task in the claimed task's workspace"),
        "{error}"
    );
    let before = owner.calls.load(Ordering::SeqCst);
    for (field, value) in [
        ("workspace", json!(fixture.binding.owner_destination)),
        ("snapshot", json!(true)),
        ("view", json!("bounded")),
        ("unknown", json!(true)),
        ("_worker_read", json!("tasks")),
    ] {
        let error = call(json!({"id": id, "limit": 1, field: value})).unwrap_err();
        assert!(
            error.contains(&format!("does not accept `{field}`")),
            "{error}"
        );
    }
    for field in [
        "limit",
        "comments_offset",
        "history_offset",
        "artifacts_offset",
    ] {
        let error = call(json!({"id": id, "field": "delivery", field: 1})).unwrap_err();
        assert!(
            error.contains(&format!("does not accept `{field}`")),
            "{error}"
        );
    }
    for tool in [GET, PUT] {
        let error = raw_call(
            &fixture,
            broker.socket_path(),
            tool,
            json!({"id": id, "path": "page-0.txt", "limit": 1}),
        )
        .unwrap_err();
        assert!(error.contains("does not accept `limit`"), "{tool}: {error}");
    }
    for field in ["field", "fields", "run_id"] {
        let value = if field == "fields" {
            json!(["artifacts"])
        } else {
            json!("artifacts")
        };
        let error = call(json!({"id": id, "limit": 1, field: value})).unwrap_err();
        assert!(
            error.contains("paginated task read accepts only"),
            "{error}"
        );
        for accepted in [
            "id",
            "model",
            "limit",
            "comments_offset",
            "history_offset",
            "artifacts_offset",
        ] {
            assert!(error.contains(accepted), "accepted {accepted}: {error}");
        }
    }
    assert_eq!(
        owner.calls.load(Ordering::SeqCst),
        before,
        "scope refusals never reach the owner"
    );

    for field in [
        "limit",
        "comments_offset",
        "history_offset",
        "artifacts_offset",
    ] {
        let mut invalid = vec![
            json!(-1),
            json!(1.5),
            json!("1"),
            json!(true),
            json!([]),
            json!({}),
            json!(1e30),
        ];
        if field == "limit" {
            invalid.extend([json!(0), json!(51)]);
        }
        // The owner's public argument parser treats optional nulls as omitted.
        let default_page = call(json!({"id": id, field: Value::Null})).unwrap();
        assert_eq!(default_page["comments_pagination"]["limit"], 25);
        assert_eq!(default_page["comments_pagination"]["offset"], 0);
        for value in invalid {
            let input = json!({"id": id, field: value});
            let mut direct = input.clone();
            direct["view"] = json!("bounded");
            let expected = owner.execute(SHOW, direct).unwrap_err().to_string();
            let error = call(input.clone()).unwrap_err();
            assert!(
                error.contains(&expected),
                "{input}: broker {error}; owner {expected}"
            );
        }
    }
}
