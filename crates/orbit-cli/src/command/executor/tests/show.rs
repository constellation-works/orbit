use orbit_core::OrbitRuntime;
use orbit_types::resource::ExecutorResource;
use orbit_types::workflow::ExecutorDef;

use super::super::show::ExecutorShowArgs;
use crate::command::{CommandOutput, Execute};
use crate::output::payload::{Block, View};

#[test]
fn executor_show_reports_explicit_off_in_json_and_human_output() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let resource: ExecutorResource = serde_yaml::from_str(
        "schemaVersion: 2\nkind: Executor\nmetadata:\n  name: codex\nspec:\n  executor_type: direct_agent\n  command: codex\n  sandbox: off\n",
    ).expect("operator YAML");
    let def = ExecutorDef::from_resource_spec(
        resource.metadata.name,
        resource.spec.clone(),
        resource.spec.created_at,
        resource.spec.updated_at,
    );
    runtime.upsert_executor_def(&def).expect("store executor");

    let CommandOutput::Payload(payload) = (ExecutorShowArgs {
        name: "codex".to_string(),
        json: true,
    })
    .execute(&runtime)
    .expect("show executor") else {
        panic!("executor show must return a payload");
    };
    let (json, view) = payload.into_view();
    assert_eq!(json["sandbox"], "off");
    assert_eq!(json["allow_fallback"], false);
    let View::Blocks(blocks) = view else {
        panic!("human detail blocks")
    };
    assert!(
        blocks
            .iter()
            .any(|block| matches!(block, Block::Text(text) if text.contains("Sandbox:   off")))
    );
}
