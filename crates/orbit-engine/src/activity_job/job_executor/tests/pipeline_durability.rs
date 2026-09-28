#![allow(missing_docs)]

//! Pipeline durability invariants for `exec_ctx.rs` and `fan_out.rs`:
//! cross-step value visibility and snapshot inheritance into fan-out
//! workers. See task T20260509-7.

use super::*;

/// Give a target step a templated `default_input`.
fn with_default_input(mut step: JobV2Step, default_input: Value) -> JobV2Step {
    match &mut step.body {
        JobV2StepBody::Target(target) => target.default_input = Some(default_input),
        _ => unreachable!("target_step must build a target body"),
    }
    step
}

#[test]
fn pipeline_value_from_earlier_step_visible_to_later_step_via_template() {
    // step1 returns {"name": "alice"}; step2 renders that output into its
    // input and echoes it, so the host sees exactly what step2 consumed.
    let host = ScriptedHost::new([
        ("read_name", vec![Action::Ok(json!({"name": "alice"}))]),
        ("downstream", vec![Action::EchoInput]),
    ]);
    let job = job_with_steps(vec![
        target_step("step1", "read_name"),
        with_default_input(
            target_step("step2", "downstream"),
            json!({"greeting": "{{ steps.step1.output.name }}"}),
        ),
    ]);
    let outcome = run_job(&host, &job, Value::Null, "run-pipe-visible");
    assert!(outcome.success);
    let consumed = host.input_for_action("downstream").expect("step2 ran");
    assert_eq!(
        consumed.get("greeting"),
        Some(&json!("alice")),
        "step2 must consume step1's output rendered through the template"
    );
    let pipeline = outcome.pipeline.as_object().expect("pipeline obj");
    assert_eq!(pipeline.get("step1"), Some(&json!({"name": "alice"})));
    assert_eq!(
        pipeline.get("step2").and_then(|out| out.get("greeting")),
        Some(&json!("alice"))
    );
}

#[test]
fn pipeline_snapshot_inherited_by_fanout_workers_at_dispatch_time() {
    // A pre-step writes to the pipeline; each fan_out worker renders that
    // entry through `steps.seed.output.value` and echoes it back, so the
    // collected outputs prove every worker read the inherited snapshot.
    let host = ScriptedHost::new([
        ("seed", vec![Action::Ok(json!({"value": 42}))]),
        ("w", vec![Action::EchoInput, Action::EchoInput]),
    ]);
    let worker = with_default_input(
        target_step("worker", "w"),
        json!({
            "seed": "{{ steps.seed.output.value }}",
            "index": "{{ input.iteration }}",
        }),
    );
    let job = job_with_steps(vec![
        target_step("seed", "seed"),
        fanout_step(
            "scatter",
            "{{ input.items }}",
            2,
            worker,
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
    let consumed: Vec<_> = pipeline
        .get("scatter")
        .and_then(Value::as_array)
        .expect("scatter array")
        .iter()
        .map(|out| (out.get("index").cloned(), out.get("seed").cloned()))
        .collect();
    assert_eq!(
        consumed,
        vec![
            (Some(json!(0)), Some(json!(42))),
            (Some(json!(1)), Some(json!(42))),
        ],
        "every worker must consume the upstream seed from the inherited pipeline"
    );
    // The snapshot taken into workers does not replace the parent pipeline.
    assert_eq!(pipeline.get("seed"), Some(&json!({"value": 42})));
    // Workers share the parent's map by reference at dispatch; a worker's
    // own step record copies on write and never reaches the parent.
    assert!(
        !pipeline.contains_key("worker"),
        "worker-local step output must not leak into the parent pipeline"
    );
}
