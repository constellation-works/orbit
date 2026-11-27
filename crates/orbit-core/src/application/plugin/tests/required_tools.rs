use super::fixture::{PluginFixture, PluginSpecFixture, enter_isolated_child};
use crate::OrbitRuntime;
use crate::application::plugin::{
    PluginAddOptions, PluginEnableOptions, enable_plugin, install_plugin,
};
use crate::application::task::TaskAddParams;
use orbit_engine::Recovery;

#[test]
fn filing_requires_plugin_tools_an_agent_can_call() {
    if !enter_isolated_child(
        module_path!(),
        "filing_requires_plugin_tools_an_agent_can_call",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let manifest_path = source.join("plugin.yaml");
    let manifest = std::fs::read_to_string(&manifest_path).expect("manifest");
    let manifest = format!(
        "{manifest}    - name: operator\n      execution_kind: mutating\n      mcp_scope: workspace\n    - name: propose\n      execution_kind: mutating\n      agent_callable: true\n      mcp_scope: workspace\n"
    );
    std::fs::write(manifest_path, manifest).expect("write manifest");
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("path"),
        &PluginAddOptions::default(),
    )
    .expect("install");
    enable_plugin(&fixture.runtime, "demo", &PluginEnableOptions::default())
        .expect("enable without consent");
    let unconsented =
        OrbitRuntime::from_roots(&fixture.global_root, &fixture.repo_root.join(".orbit"))
            .expect("reload registry");
    assert!(
        unconsented
            .add_task(TaskAddParams {
                title: "Missing consent".into(),
                required_tools: vec!["demo.propose".into()],
                ..Default::default()
            })
            .is_err()
    );
    enable_plugin(
        &fixture.runtime,
        "demo",
        &PluginEnableOptions {
            grants: vec!["agent_tools".into()],
            ..Default::default()
        },
    )
    .expect("enable with consent");
    let runtime = OrbitRuntime::from_roots(&fixture.global_root, &fixture.repo_root.join(".orbit"))
        .expect("reload registry");
    let file = |name: &str| {
        runtime.add_task(TaskAddParams {
            title: format!("Require {name}"),
            required_tools: vec![name.into()],
            ..Default::default()
        })
    };
    let error = file("demo.operator").expect_err("operator-only plugin cannot be required");
    assert!(
        error.to_string().contains("operator or runner capability"),
        "{error}"
    );
    assert!(runtime.list_tasks().expect("tasks").is_empty());
    for name in ["demo.hello", "demo.propose"] {
        let task = file(name).expect("agent-callable plugin requirement is accepted");
        assert_eq!(task.required_tools, [name]);
        let admitted = runtime
            .resolve_activity_tool_denials(None, &[task.id], "agent_implement", &[])
            .expect("dispatch admits the requirement");
        assert!(admitted.effective_tools.iter().any(|tool| tool == name));
    }
}
