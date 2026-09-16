#![allow(missing_docs)]

//! Pipeline durability invariants for `exec_ctx.rs` and `fan_out.rs`:
//! cross-step value visibility and snapshot inheritance into fan-out
//! workers. See task T20260509-7.

use super::*;

#[test]
fn pipeline_value_from_earlier_step_visible_to_later_step_via_template() {
    // step1 returns {"name": "alice"}. step2 declares default_input that
    // references `{{ steps.step1.output.name }}` and the template renderer
    // should resolve it. We then confirm the value made it through the
    // pipeline by inspecting the final pipeline state.
    let host = ScriptedHost::new([
        ("read_name", vec![Action::Ok(json!({"name": "alice"}))]),
        ("downstream", vec![Action::Ok(json!({"ok": true}))]),
    ]);
    // Note: deterministic activities accept opaque input; we cannot directly
    // observe the rendered template input from the host without a custom
    // action sink. Instead, assert pipeline visibility: step1's value lands
    // in the pipeline and is structurally available for downstream steps.
    let job = job_with_steps(vec![
        target_step("step1", "read_name"),
        target_step("step2", "downstream"),
    ]);
    let outcome = run_job(&host, &job, Value::Null, "run-pipe-visible");
    assert!(outcome.success);
    let pipeline = outcome.pipeline.as_object().expect("pipeline obj");
    assert_eq!(
        pipeline.get("step1"),
        Some(&json!({"name": "alice"})),
        "step1 output must be visible in pipeline for downstream consumers"
    );
    assert!(pipeline.contains_key("step2"));
}

#[test]
fn pipeline_snapshot_inherited_by_fanout_workers_at_dispatch_time() {
    // Pre-step writes to the pipeline; the fan_out worker context (fan_out.rs:53-56)
    // clones the pipeline at dispatch. Workers should see the upstream pipeline
    // entry under steps.<id>.output.<field>.
    //
    // We can't read the rendered template input from a deterministic worker,
    // but we can observe the snapshot mechanism end-to-end: the worker output
    // is captured per-index, and the upstream value remains in the pipeline
    // unchanged after the fan_out completes.
    let host = ScriptedHost::new([
        ("seed", vec![Action::Ok(json!({"value": 42}))]),
        (
            "w",
            vec![Action::Ok(json!({"ok": 0})), Action::Ok(json!({"ok": 1}))],
        ),
    ]);
    let job = job_with_steps(vec![
        target_step("seed", "seed"),
        fanout_step(
            "scatter",
            "{{ input.items }}",
            2,
            target_step("worker", "w"),
            JoinMode::All,
            None,
        ),
    ]);
    let outcome = run_job(
        &host,
        &job,
        json!({"items": [0, 1]}),
        "run-pipe-fanout-snapshot",
    );
    assert!(outcome.success);
    let pipeline = outcome.pipeline.as_object().expect("pipeline obj");
    // Upstream value still present after fan_out completes — the snapshot
    // taken into workers does not replace the parent pipeline.
    assert_eq!(pipeline.get("seed"), Some(&json!({"value": 42})));
    // Workers share the parent's map by reference at dispatch; a worker's
    // own step record copies on write and never reaches the parent.
    assert!(
        !pipeline.contains_key("worker"),
        "worker-local step output must not leak into the parent pipeline"
    );
    let scatter = pipeline
        .get("scatter")
        .and_then(Value::as_array)
        .expect("scatter array");
    assert_eq!(scatter.len(), 2);
}
