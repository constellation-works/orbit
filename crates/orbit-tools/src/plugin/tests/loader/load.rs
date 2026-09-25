use super::*;

#[test]
fn load_plugin_dir_accepts_a_tree_with_no_symbolic_links() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin(temp.path());
    let plugin = load_plugin_dir(temp.path()).expect("load");
    assert_eq!(plugin.namespace(), "demo");
}

#[test]
fn load_refuses_resolved_schema_properties_with_colliding_cli_flags() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin(temp.path());
    let manifest = temp.path().join(MANIFEST_FILE_NAME);
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace(
            "      execution_kind: read_only\n",
            "      execution_kind: read_only\n      input_schema:\n        type: object\n        properties:\n          task_id: { type: string }\n          taskId: { type: string }\n",
        ),
    )
    .expect("write manifest");

    let error = load_plugin_dir(temp.path())
        .expect_err("colliding flags refuse load")
        .to_string();
    assert!(
        error.contains("task_id") && error.contains("taskId"),
        "the load diagnostic names both colliding properties: {error}"
    );
}

/// Write `MANIFEST` with `body` spliced into its single tool.
fn write_plugin_with_tool_keys(root: &Path, keys: &str) {
    write_plugin(root);
    let manifest = root.join(MANIFEST_FILE_NAME);
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace(
            "      execution_kind: read_only\n",
            &format!("      execution_kind: read_only\n{keys}"),
        ),
    )
    .expect("write manifest");
}

/// A `$ref` schema is only properties once it is read from the plugin root,
/// so the CLI checks run there too: otherwise the adapter silently drops both
/// colliding flags and the tool loses them with no diagnostic anywhere.
#[test]
fn a_ref_schema_is_held_to_the_cli_flag_and_positional_rules() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin_with_tool_keys(
        temp.path(),
        "      input_schema: { $ref: schemas/hello.json }\n",
    );
    std::fs::create_dir_all(temp.path().join("schemas")).expect("mkdir");
    let schema = temp.path().join("schemas/hello.json");
    std::fs::write(
        &schema,
        r#"{"type":"object","properties":{"task_id":{"type":"string"},"taskId":{"type":"string"}}}"#,
    )
    .expect("schema");

    let error = load_plugin_dir(temp.path())
        .expect_err("colliding flags behind a $ref refuse load")
        .to_string();
    assert!(
        error.contains("task_id") && error.contains("taskId"),
        "the load diagnostic names both colliding properties: {error}"
    );

    std::fs::write(
        &schema,
        r#"{"type":"object","properties":{"query":{"type":"string"}}}"#,
    )
    .expect("schema");
    load_plugin_dir(temp.path()).expect("a $ref schema with distinct flags loads");

    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin_with_tool_keys(
        temp.path(),
        "      input_schema: { $ref: schemas/hello.json }\n      cli: { positional: [subject] }\n",
    );
    std::fs::create_dir_all(temp.path().join("schemas")).expect("mkdir");
    std::fs::write(
        temp.path().join("schemas/hello.json"),
        r#"{"type":"object","properties":{"query":{"type":"string"}}}"#,
    )
    .expect("schema");
    let error = load_plugin_dir(temp.path())
        .expect_err("a positional outside the resolved schema refuses load")
        .to_string();
    assert!(
        error.contains("hello") && error.contains("subject"),
        "the refusal names the tool and the positional: {error}"
    );
}

/// §4.9: an unresolvable `$ref` refuses the plugin at load. A nested one is
/// found by compiling the schema, which is also where an invalid keyword is
/// found — neither may be left to fail every call instead.
#[test]
fn a_schema_that_cannot_compile_refuses_the_plugin_at_load() {
    for (keys, needle) in [
        (
            "      input_schema:\n        type: object\n        properties:\n          query: { $ref: '#/definitions/missing' }\n",
            "input_schema",
        ),
        (
            "      output_schema:\n        type: object\n        properties:\n          count: { type: whatever }\n",
            "output_schema",
        ),
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        write_plugin_with_tool_keys(temp.path(), keys);
        let error = load_plugin_dir(temp.path())
            .expect_err("a schema that does not compile refuses load")
            .to_string();
        assert!(
            error.contains(needle) && error.contains("hello"),
            "the refusal names the field and the tool: {error}"
        );
    }

    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin_with_tool_keys(
        temp.path(),
        "      input_schema:\n        type: object\n        properties:\n          query: { $ref: '#/definitions/term' }\n        definitions:\n          term: { type: string }\n      output_schema:\n        type: object\n        properties:\n          count: { type: integer }\n",
    );
    let plugin = load_plugin_dir(temp.path()).expect("resolvable schemas load");
    assert!(plugin.tools[0].output_schema.is_some());
}

#[test]
fn a_nested_unresolvable_ref_in_an_output_schema_refuses_load() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin_with_tool_keys(
        temp.path(),
        "      output_schema:\n        type: object\n        properties:\n          result:\n            type: object\n            properties:\n              value: { $ref: '#/$defs/missing' }\n",
    );

    let error = load_plugin_dir(temp.path())
        .expect_err("a nested unresolved output ref must refuse load")
        .to_string();
    assert!(
        error.contains("output_schema") && error.contains("hello"),
        "the refusal names the schema field and tool: {error}"
    );
}

#[test]
fn a_conformance_case_for_an_undeclared_tool_refuses_load() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin(temp.path());
    let manifest = temp.path().join(MANIFEST_FILE_NAME);
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace("  tools:\n", "  tests: [tests/*.yaml]\n  tools:\n"),
    )
    .expect("declare conformance tests");
    std::fs::create_dir_all(temp.path().join("tests")).expect("create tests directory");
    std::fs::write(
        temp.path().join("tests/undeclared.yaml"),
        "schemaVersion: 1\nkind: PluginTest\ntests:\n  - name: typo\n    tool: missing\n    expect:\n      output: {}\n",
    )
    .expect("write conformance case");

    let error = load_plugin_dir(temp.path())
        .expect_err("an undeclared conformance tool must refuse the plugin directory")
        .to_string();
    assert!(
        error.contains("missing") && error.contains("does not declare"),
        "the refusal names the undeclared tool: {error}"
    );
}

/// A plugin root that does not exist is ordinary bad input (a wrong path
/// given to `orbit plugin add`), not a policy refusal, so it keeps mapping to
/// `InvalidInput` — the `std::io::ErrorKind` carried on `PluginLoadError::Io`
/// is what lets the `OrbitError` conversion tell this apart from the
/// `PermissionDenied` case, which does map to `PolicyDenied` [ORB-12837].
#[test]
fn load_plugin_dir_reports_a_missing_root_as_invalid_input() {
    let temp = tempfile::tempdir().expect("tempdir");
    let missing = temp.path().join("absent");
    let error = load_plugin_dir(&missing).expect_err("a missing root must refuse load");
    assert!(matches!(
        orbit_common::OrbitError::from(error),
        orbit_common::OrbitError::InvalidInput(_)
    ));
}

#[cfg(unix)]
#[test]
fn load_plugin_dir_refuses_an_installed_tree_that_contains_a_symlink() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_plugin(temp.path());
    let secret = temp.path().parent().expect("parent").join("secret");
    std::fs::write(&secret, "SECRET-CONTENT-OUTSIDE-PLUGIN-TREE").expect("secret");
    std::os::unix::fs::symlink(&secret, temp.path().join("env")).expect("symlink");

    let error = load_plugin_dir(temp.path()).expect_err("a planted symlink must refuse load");
    // A symlink refusal is fail-closed security policy (§4.9), not a
    // malformed-manifest problem, so it must surface as `PolicyDenied`
    // rather than `InvalidInput` [ORB-12837].
    assert!(
        matches!(
            orbit_common::OrbitError::from(error.clone()),
            orbit_common::OrbitError::PolicyDenied(_)
        ),
        "{error:?}"
    );
    let error = error.to_string();
    assert!(
        error.contains("env"),
        "refusal must name the offending entry: {error}"
    );
    assert!(
        error.contains("symbolic link"),
        "refusal must say why: {error}"
    );
}
