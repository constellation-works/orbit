#![allow(missing_docs)]

use super::*;

#[test]
fn render_input_supports_legacy_batch_id_from_worktree_output() {
    let mut steps = HashMap::new();
    steps.insert(
        "worktree".to_string(),
        json!({
            "output": {
                "job_run_id": "jrun-template",
                "batch_id": "jrun-template",
            }
        }),
    );
    let tctx = TemplateContext {
        steps,
        ..TemplateContext::default()
    };
    let default_input = json!({
        "batch_id": "{{ steps.worktree.output.batch_id }}",
    });

    let rendered = render_input(Some(&default_input), &Value::Null, &tctx, None).unwrap();

    assert_eq!(rendered, json!({ "batch_id": "jrun-template" }));
}

#[test]
fn shipped_task_pilot_binds_omitted_optional_base_branch_and_preserves_override() {
    let asset = load_job_asset(include_str!(
        "../../../../../orbit-core/assets/jobs/task_pilot_pipeline.yaml"
    ))
    .expect("load shipped task-pilot job");
    assert_eq!(
        asset
            .spec
            .default_input
            .as_ref()
            .and_then(|input| input.get("base_branch")),
        None,
        "the shipped job must omit base_branch"
    );

    let activity = crate::activity_job::load_activity_asset(include_str!(
        "../../../../../orbit-core/assets/activities/prepare_task_pilot.yaml"
    ))
    .expect("load shipped prepare activity");
    let mut catalog = V2ActivityCatalog::new();
    catalog.insert(activity.name, activity.spec);

    for (run_id, input, expected_branch) in [
        ("run-task-pilot-default-branch", json!({}), ""),
        (
            "run-task-pilot-explicit-branch",
            json!({ "base_branch": "release-candidate" }),
            "release-candidate",
        ),
    ] {
        let mut job = asset.spec.clone();
        job.steps.truncate(1);
        resolve_job_catalog_refs_for_execution(&mut job, &catalog)
            .expect("resolve shipped prepare activity");
        let host = ScriptedHost::new([("prepare_task_pilot", vec![Action::EchoInput])]);
        let outcome = execute_job(
            &job,
            input,
            run_id,
            std::sync::Arc::new(test_writer(run_id)),
            &host,
        )
        .expect("execute shipped prepare binding");

        assert!(outcome.success);
        assert_eq!(
            outcome.pipeline["prepare"]["base_branch"],
            json!(expected_branch)
        );
        assert_eq!(host.call_count("prepare_task_pilot"), 1);
    }
}

#[test]
fn missing_required_input_stays_a_template_error() {
    let default_input = json!({ "required_name": "{{ input.required_name }}" });
    let schema = json!({
        "type": "object",
        "required": ["required_name"],
        "properties": { "required_name": { "type": "string" } }
    });
    let tctx = TemplateContext {
        input: json!({}),
        ..TemplateContext::default()
    };

    let error = render_input(Some(&default_input), &tctx.input, &tctx, Some(&schema))
        .expect_err("required input must remain absent");

    assert!(
        error
            .to_string()
            .contains("missing input value for 'required_name'")
    );
}

#[test]
fn misspelled_optional_input_path_stays_a_template_error() {
    let default_input = json!({ "base_branch": "{{ input.base_barnch }}" });
    let schema = json!({
        "type": "object",
        "properties": { "base_branch": { "type": "string" } }
    });
    let tctx = TemplateContext {
        input: json!({}),
        ..TemplateContext::default()
    };

    let error = render_input(Some(&default_input), &tctx.input, &tctx, Some(&schema))
        .expect_err("a path outside the contract must remain absent");

    assert!(
        error
            .to_string()
            .contains("missing input value for 'base_barnch'")
    );
}
