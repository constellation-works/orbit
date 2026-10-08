//! Exhaustive parser coverage includes hidden leaves that the binary's help
//! cannot enumerate, and plugin schemas that might collide with global flags.
//! This combinatorial command-tree invariant has no economical help golden.

use clap::Command;
use orbit_core::adapter::command::{PluginCliGroup, PluginCliVerb};

#[test]
fn every_assembled_leaf_accepts_json_and_lists_domain_input_exclusions() {
    let plugin = PluginCliGroup {
        namespace: "fixture".into(),
        version: "1.0.0".into(),
        description: "Command-tree fixture".into(),
        verbs: vec![PluginCliVerb {
            verb: "show".into(),
            tool_name: "fixture.show".into(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
            positional: Vec::new(),
            mutating: false,
        }],
    };
    let mut plugin = plugin;
    let mut domain_input = plugin.verbs[0].clone();
    domain_input.verb = "input".into();
    domain_input.tool_name = "fixture.input".into();
    domain_input.input_schema = serde_json::json!({
        "type": "object",
        "properties": {"json": {"type": "boolean"}}
    });
    plugin.verbs.push(domain_input);
    let mut tree = crate::cli_command(&[plugin.clone()]);
    tree.build();
    let mut paths = Vec::new();
    check_leaves(&tree, "orbit", &mut paths);
    assert!(paths.iter().any(|path| path == "orbit fixture show"));
    assert!(paths.iter().any(|path| path == "orbit workspace list"));
    assert!(paths.iter().any(|path| path == "orbit audit export"));
    // The only exclusion preserves a plugin tool's domain input. Its caller
    // can put output --json before the verb, or use --format json on the leaf.
    let matches = tree
        .try_get_matches_from(["orbit", "--json", "fixture", "input", "--json"])
        .expect("global output and local tool-input flags coexist");
    let (_, json) = crate::requested_output(&matches).expect("output options");
    assert!(json);
    let invocation =
        crate::plugin_cli::invocation_from_matches(&[plugin], &matches).expect("plugin invocation");
    assert_eq!(
        invocation.tool_run.parsed_input().expect("tool input"),
        serde_json::json!({"json": true})
    );
}

fn check_leaves(command: &Command, path: &str, paths: &mut Vec<String>) {
    if command.get_subcommands().next().is_none() {
        // Missing required command inputs are irrelevant to output-option
        // coverage. Parse the real option while tolerating those omissions.
        let matches = command
            .clone()
            .ignore_errors(true)
            .try_get_matches_from([command.get_name(), "--json"])
            .unwrap_or_else(|error| panic!("{path} rejected --json: {error}"));
        let json_arg = command
            .get_arguments()
            .find(|arg| arg.get_long() == Some(crate::JSON_ARG_ID))
            .unwrap_or_else(|| panic!("{path} must accept --json"));
        if json_arg.get_id().as_str() != crate::JSON_ARG_ID {
            assert_eq!(
                path, "orbit fixture input",
                "unlisted exclusion: --json must select output unless it is a plugin tool input"
            );
        }
        assert!(
            matches!(
                matches.try_get_one::<bool>(json_arg.get_id().as_str()),
                Ok(Some(true))
            ),
            "{path} must parse its --json flag"
        );
        paths.push(path.into());
    }
    for subcommand in command.get_subcommands() {
        check_leaves(
            subcommand,
            &format!("{path} {}", subcommand.get_name()),
            paths,
        );
    }
}
