use std::collections::BTreeMap;

use orbit_tools::plugin::load_plugin_dir;

use super::super::panels::web_summaries;

#[test]
fn unresolved_link_templates_remain_visible_and_panels_report_effective_ttl() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("plugin");
    std::fs::create_dir_all(root.join("bin")).expect("plugin dirs");
    std::fs::write(root.join("bin/backend"), "#!/bin/sh\n").expect("backend");
    std::fs::write(
        root.join("plugin.yaml"),
        r#"schemaVersion: 2
kind: Plugin
metadata:
  name: links
  version: 1.0.0
spec:
  backend:
    type: exec
    command: bin/backend
  tools:
    - name: status
      execution_kind: read_only
  web:
    panels:
      - id: status
        source: tool:status
        refresh_ms: 45000
    links:
      - title: State
        url: http://127.0.0.1/{{plugin_state}}/{{config.port}}/status
"#,
    )
    .expect("manifest");

    let plugin = load_plugin_dir(&root).expect("load plugin");
    let config = BTreeMap::from([("port".to_string(), "7890".to_string())]);
    let (panels, links) = web_summaries(&plugin, false, &config);
    assert_eq!(panels[0].refresh_ms, 45_000);
    assert_eq!(
        links[0].url, "http://127.0.0.1/{{plugin_state}}/{{config.port}}/status",
        "an unresolved allowed template keeps the whole URL operator-visible"
    );
}
