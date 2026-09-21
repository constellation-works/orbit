use super::super::pin::{PluginPin, PluginPinFile};

#[test]
fn pin_file_rejects_unknown_schema_and_duplicates() {
    let mut file = PluginPinFile::default();
    file.plugins.push(PluginPin {
        name: "graph".into(),
        version: Some("0.4.x".into()),
        source: None,
        enabled: true,
    });
    assert_eq!(
        file.validate().unwrap_err(),
        "plugins[0].version: invalid version '0.4.x': expected MAJOR.MINOR.PATCH"
    );
    file.plugins[0].version = Some("^0.4.1".into());
    file.plugins.push(file.plugins[0].clone());
    assert!(file.validate().unwrap_err().contains("plugins[1].name"));
    file.plugins.pop();
    file.schema_version = 2;
    assert!(file.validate().unwrap_err().starts_with("schemaVersion"));
}

#[test]
fn pin_file_parses_the_design_example() {
    let raw = "schemaVersion: 1\nplugins:\n  - name: graph\n    version: \"^0.4.1\"\n    source: git+https://github.com/constellation-works/orbit-graph#v0.4.1\n    enabled: true\n";
    let file: PluginPinFile = serde_yaml::from_str(raw).expect("parse");
    file.validate().expect("valid");
    assert_eq!(file.plugins[0].name, "graph");
}
