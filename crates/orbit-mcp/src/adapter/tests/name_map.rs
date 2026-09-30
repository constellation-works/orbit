use super::super::name_map::{
    advertise_tool_names, advertise_tool_names_in_schema, build_name_map, sanitize_tool_name,
};
use serde_json::Value;

use super::super::test_support::tool_schema;

#[test]
fn sanitize_tool_name_replaces_dots_with_underscores() {
    assert_eq!(sanitize_tool_name("orbit.task.add"), "orbit_task_add");
    assert_eq!(
        sanitize_tool_name("orbit.task.artifact.put"),
        "orbit_task_artifact_put"
    );
    assert_eq!(sanitize_tool_name("orbit_task_add"), "orbit_task_add");
}

#[test]
fn build_name_map_keys_are_advertised_names() {
    let schemas = vec![
        tool_schema("orbit.task.add"),
        tool_schema("orbit.task.artifact.put"),
    ];
    let map = build_name_map(&schemas).expect("unique advertised names");
    assert_eq!(
        map.get("orbit_task_add").map(String::as_str),
        Some("orbit.task.add")
    );
    assert_eq!(
        map.get("orbit_task_artifact_put").map(String::as_str),
        Some("orbit.task.artifact.put")
    );
}

#[test]
fn build_name_map_rejects_sanitized_name_collisions() {
    let schemas = vec![tool_schema("foo.bar"), tool_schema("foo_bar")];
    let err = build_name_map(&schemas).expect_err("sanitized names must be unique");
    assert_eq!(err.advertised_name, "foo_bar");
    assert_eq!(
        err.canonical_names,
        vec!["foo.bar".to_string(), "foo_bar".to_string()]
    );

    let mcp_err = err.into_mcp_error();
    assert!(mcp_err.message.contains("foo_bar"));
    let data = mcp_err.data.as_ref().expect("structured error data");
    assert_eq!(
        data.get("code").and_then(Value::as_str),
        Some("tool_name_collision")
    );
    assert_eq!(
        data.get("advertised_name").and_then(Value::as_str),
        Some("foo_bar")
    );
}

const NAMES: &[&str] = &[
    "orbit.task.artifact.get",
    "orbit.task.show",
    "orbit.drain.probe",
];

#[test]
fn advertise_tool_names_rewrites_only_whole_known_tool_names() {
    assert_eq!(
        advertise_tool_names(
            "List with `orbit.task.show` and `orbit.task.artifact.get`.",
            NAMES
        ),
        "List with `orbit_task_show` and `orbit_task_artifact_get`."
    );
    // Sentence punctuation after a name is not part of it.
    assert_eq!(
        advertise_tool_names(
            "Call orbit.task.show. Then orbit.drain.probe, or orbit.task.show",
            NAMES
        ),
        "Call orbit_task_show. Then orbit_drain_probe, or orbit_task_show"
    );
    // Not a tool name: a metadata key, a longer path, a tool that is not advertised.
    for untouched in [
        "`_meta.orbit.workspace` selects the workspace",
        "orbit.task.show.extra is not a tool",
        "xorbit.task.show is not a tool",
        "`orbit tool run orbit.task.reject` is a CLI verb",
    ] {
        assert_eq!(advertise_tool_names(untouched, NAMES), untouched);
    }
}

#[test]
fn advertise_tool_names_in_schema_reaches_nested_descriptions_only() {
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "fields": {
                "description": "Names `orbit.task.show` accepts.",
                "items": { "description": "See orbit.drain.probe" }
            },
            "orbit.task.show": { "type": "string" }
        }
    })
    .as_object()
    .cloned()
    .expect("schema object");
    advertise_tool_names_in_schema(&mut schema, NAMES);
    assert_eq!(
        schema["properties"]["fields"]["description"],
        "Names `orbit_task_show` accepts."
    );
    assert_eq!(
        schema["properties"]["fields"]["items"]["description"],
        "See orbit_drain_probe"
    );
    assert!(
        schema["properties"]
            .as_object()
            .expect("properties")
            .contains_key("orbit.task.show"),
        "a property key is a wire name, not prose"
    );
}

#[test]
fn build_name_map_rejects_duplicate_canonical_names() {
    let schemas = vec![tool_schema("orbit.task.add"), tool_schema("orbit.task.add")];
    let err = build_name_map(&schemas).expect_err("canonical names must be unique");
    assert_eq!(err.advertised_name, "orbit_task_add");
    assert_eq!(err.canonical_names, vec!["orbit.task.add".to_string()]);
}
