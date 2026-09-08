#![allow(missing_docs)]

//! Fan-out / fan-in invariants for `fan_out.rs`: empty-items audit pair,
//! spawn-index-ordered collection, `max_workers` semaphore cap, structural
//! error surfacing under unsatisfied join, `fan_in.collect` alias, and
//! per-worker dispatched->finished audit ordering. See task T20260509-7.

use super::*;

#[test]
fn fanout_empty_items_emits_dispatched_zero_and_joined_zero() {
    let host = ScriptedHost::new([("worker_action", vec![])]);
    let job = job_with_steps(vec![fanout_step(
        "scatter",
        "{{ input.items }}",
        4,
        target_step("worker", "worker_action"),
        JoinMode::All,
        None,
    )]);
    let writer = std::sync::Arc::new(test_writer("run-fanout-empty"));
    let outcome = execute_job(
        &job,
        json!({"items": []}),
        "run-fanout-empty",
        writer.clone(),
        &host,
    )
    .expect("execute_job ok");

    assert!(outcome.success);
    let events = writer.events_snapshot().expect("audit");
    let dispatched_count = events
        .iter()
        .filter_map(|e| match &e.kind {
            V2AuditEventKind::FanoutDispatched { worker_count, .. } => Some(*worker_count),
            _ => None,
        })
        .next()
        .expect("FanoutDispatched event");
    assert_eq!(dispatched_count, 0);
    let joined = events
        .iter()
        .find_map(|e| match &e.kind {
            V2AuditEventKind::FaninJoined {
                collected, failed, ..
            } => Some((*collected, *failed)),
            _ => None,
        })
        .expect("FaninJoined event");
    assert_eq!(joined, (0, 0));

    let pipeline = outcome.pipeline.as_object().expect("pipeline obj");
    assert_eq!(pipeline.get("scatter"), Some(&json!([])));
}

#[test]
fn fanout_empty_items_records_collect_alias() {
    let host = ScriptedHost::new([("worker_action", vec![])]);
    let job = job_with_steps(vec![fanout_step(
        "scatter",
        "{{ input.items }}",
        4,
        target_step("worker", "worker_action"),
        JoinMode::All,
        Some("results"),
    )]);
    let outcome = run_job(&host, &job, json!({"items": []}), "run-fanout-empty-alias");

    assert!(outcome.success);
    let pipeline = outcome.pipeline.as_object().expect("pipeline obj");
    assert_eq!(pipeline.get("scatter"), Some(&json!([])));
    assert_eq!(pipeline.get("results"), Some(&json!([])));
}

#[test]
fn fanout_collected_outputs_are_index_ordered_even_when_workers_finish_out_of_order() {
    // Each worker renders its own spawn index and sleep duration into the
    // deterministic action input. The first worker sleeps long enough for the
    // second worker to finish first, without coupling the expected output to
    // whichever thread reaches ScriptedHost first.
    let host = ScriptedHost::new([(
        "w",
        vec![
            Action::SleepInputMsThenEcho {
                ms_field: "sleep_ms",
            },
            Action::SleepInputMsThenEcho {
                ms_field: "sleep_ms",
            },
        ],
    )]);
    let mut worker = target_step("worker", "w");
    match &mut worker.body {
        JobV2StepBody::Target(target) => {
            target.default_input = Some(json!({
                "sleep_ms": "{{ input.item.sleep_ms }}",
                "spawn_index": "{{ input.iteration }}",
            }));
        }
        _ => unreachable!("target_step must build a target body"),
    }
    let job = job_with_steps(vec![fanout_step(
        "scatter",
        "{{ input.items }}",
        4, // both run concurrently
        worker,
        JoinMode::All,
        None,
    )]);
    let outcome = run_job(
        &host,
        &job,
        json!({"items": [{"sleep_ms": 80}, {"sleep_ms": 0}]}),
        "run-fanout-order",
    );

    assert!(outcome.success);
    let pipeline = outcome.pipeline.as_object().expect("pipeline obj");
    let collected = pipeline.get("scatter").expect("scatter pipeline");
    let arr = collected.as_array().expect("collected array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0].get("spawn_index"), Some(&json!(0)));
    assert_eq!(arr[1].get("spawn_index"), Some(&json!(1)));
}

#[test]
fn fanout_max_workers_caps_concurrent_workers() {
    // Invariant: `max_workers=2` must cap the in-flight worker count even
    // when 8 items are dispatched. The host tracks peak in-flight and we
    // assert it never exceeds the cap. Each worker sleeps briefly so several
    // workers actually overlap.
    let host = ScriptedHost::new([(
        "w",
        (0..8)
            .map(|i| Action::SleepOk {
                ms: 40,
                value: json!({"i": i}),
            })
            .collect(),
    )]);
    let job = job_with_steps(vec![fanout_step(
        "scatter",
        "{{ input.items }}",
        2,
        target_step("worker", "w"),
        JoinMode::All,
        None,
    )]);
    let outcome = run_job(
        &host,
        &job,
        json!({"items": [0, 1, 2, 3, 4, 5, 6, 7]}),
        "run-fanout-cap",
    );

    assert!(outcome.success);
    assert_eq!(host.call_count("w"), 8);
    assert!(
        host.peak_in_flight() <= 2,
        "peak in-flight exceeded max_workers=2: {}",
        host.peak_in_flight()
    );
    // Sanity: must actually have observed concurrency.
    assert!(
        host.peak_in_flight() >= 2,
        "expected concurrent workers; saw peak={}",
        host.peak_in_flight()
    );
}

#[test]
fn fanout_first_error_surfaces_when_join_unsatisfied() {
    // Invariant: with JoinMode::All and one worker error, the block returns
    // the structural error rather than `Ok(StepOutcome { success: false })`.
    let host = ScriptedHost::new([(
        "w",
        vec![
            Action::Ok(json!({"i": 0})),
            Action::Err(DispatchError::DeterministicActionFailed {
                action: "w".into(),
                message: "broken".into(),
            }),
            Action::Ok(json!({"i": 2})),
        ],
    )]);
    let job = job_with_steps(vec![fanout_step(
        "scatter",
        "{{ input.items }}",
        4,
        target_step("worker", "w"),
        JoinMode::All,
        None,
    )]);
    let writer = std::sync::Arc::new(test_writer("run-fanout-err"));
    let err = execute_job(
        &job,
        json!({"items": [0, 1, 2]}),
        "run-fanout-err",
        writer,
        &host,
    )
    .expect_err("first error surfaces under JoinMode::All");
    assert!(matches!(
        err,
        DispatchError::DeterministicActionFailed { ref action, .. } if action == "w"
    ));
}

#[test]
fn fanout_panic_is_collected_as_a_failed_worker() {
    let host = ScriptedHost::new([(
        "w",
        vec![Action::PanicOnInputIteration {
            field: "worker_index",
            iteration: 1,
        }],
    )]);
    let mut worker = target_step("worker", "w");
    match &mut worker.body {
        JobV2StepBody::Target(target) => {
            target.default_input = Some(json!({"worker_index": "{{ input.iteration }}"}));
        }
        _ => unreachable!("target_step must build a target body"),
    }
    let job = job_with_steps(vec![fanout_step(
        "scatter",
        "{{ input.items }}",
        3,
        worker,
        JoinMode::All,
        None,
    )]);
    let writer = std::sync::Arc::new(test_writer("run-fanout-panic"));
    let outcome = execute_job(
        &job,
        json!({"items": [0, 1, 2]}),
        "run-fanout-panic",
        writer.clone(),
        &host,
    )
    .expect("a panicked worker becomes a failed fan-out outcome");

    assert!(!outcome.success);
    let pipeline = outcome.pipeline.as_object().expect("pipeline obj");
    assert_eq!(
        pipeline.get("scatter"),
        Some(&json!([{"action": "w"}, null, {"action": "w"}]))
    );
    let events = writer.events_snapshot().expect("audit");
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        V2AuditEventKind::FaninJoined {
            collected: 2,
            failed: 1,
            ..
        }
    )));
}

#[test]
fn fanout_collect_alias_writes_collected_value_under_collect_key() {
    // Invariant: when `fan_in.collect = "results"`, the collected_value is
    // stored under both `pipeline["results"]` and `pipeline[step.id]`.
    let host = ScriptedHost::new([(
        "w",
        vec![Action::Ok(json!({"i": 0})), Action::Ok(json!({"i": 1}))],
    )]);
    let job = job_with_steps(vec![fanout_step(
        "scatter",
        "{{ input.items }}",
        4,
        target_step("worker", "w"),
        JoinMode::All,
        Some("results"),
    )]);
    let outcome = run_job(&host, &job, json!({"items": [0, 1]}), "run-fanout-collect");
    assert!(outcome.success);
    let pipeline = outcome.pipeline.as_object().expect("pipeline obj");
    let by_id = pipeline.get("scatter").expect("scatter key");
    let by_alias = pipeline.get("results").expect("results key");
    assert_eq!(by_id, by_alias);
}

#[test]
fn fanout_emits_dispatched_then_finished_worker_state_per_worker() {
    let host = ScriptedHost::new([("w", vec![Action::Ok(json!(0)), Action::Ok(json!(1))])]);
    let job = job_with_steps(vec![fanout_step(
        "scatter",
        "{{ input.items }}",
        4,
        target_step("worker", "w"),
        JoinMode::All,
        None,
    )]);
    let writer = std::sync::Arc::new(test_writer("run-fanout-worker-state"));
    let _ = execute_job(
        &job,
        json!({"items": [0, 1]}),
        "run-fanout-worker-state",
        writer.clone(),
        &host,
    )
    .expect("execute_job ok");
    let events = writer.events_snapshot().expect("audit");
    let mut by_worker: HashMap<u32, Vec<String>> = HashMap::new();
    for e in &events {
        if let V2AuditEventKind::WorkerState {
            worker_index,
            state,
            ..
        } = &e.kind
        {
            by_worker
                .entry(*worker_index)
                .or_default()
                .push(state.clone());
        }
    }
    for idx in 0..2u32 {
        let states = by_worker.get(&idx).cloned().unwrap_or_default();
        assert_eq!(
            states,
            vec!["dispatched".to_string(), "finished".to_string()],
            "worker {idx} state sequence"
        );
    }
}
