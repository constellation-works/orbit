use std::fs;
use std::path::Path;

use orbit_config::{ConfigRoots, admit_config_key, describe_config_key, load_effective_config};
use orbit_core::OrbitRuntime;
use orbit_core::runtime::WorkspaceRuntimeBinding;
use orbit_types::workflow::ShipMode;

use super::super::show::{effective_json, effective_text, scoped_json, scoped_text};
use super::super::support::{ConfigScopeArg, open_store_for_scope};
use super::test_runtime;

#[test]
fn effective_json_attributes_each_merged_value_to_its_source() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        r#"
[workflow]
base_branch = "integration"

[execution.codex]
sandbox = "danger-full-access"
"#,
    )
    .expect("write global config");
    fs::write(
        workspace_root.join("config.toml"),
        "[scoring]\nenabled = false\n",
    )
    .expect("write workspace config");

    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");
    let json = effective_json(&runtime, effective.values());

    assert_eq!(json["settings"]["scoring.enabled"], false);
    assert_eq!(json["provenance"]["scoring.enabled"]["scope"], "workspace");
    assert_eq!(
        json["provenance"]["workflow.base_branch"]["scope"],
        "global"
    );
    assert_eq!(
        json["provenance"]["execution.codex.sandbox"]["scope"],
        "built-in"
    );
    assert_eq!(
        json["provenance"]["workflow.base_branch"]["path"],
        global_root.join("config.toml").to_string_lossy().as_ref()
    );
}

#[test]
fn effective_json_includes_configured_crew_effort_and_omits_unconfigured() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        r#"
[workflow]
default_crew = "sol"

[crews.sol]
model = "gpt-6-sol"
provider = "codex"
effort = "medium"

[crews.opus]
model = "opus"
provider = "claude"
"#,
    )
    .expect("write global config");
    fs::write(
        workspace_root.join("config.toml"),
        "[crews.sol]\neffort = \"high\"\n",
    )
    .expect("write workspace override");

    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");
    let json = effective_json(&runtime, effective.values());

    assert_eq!(json["settings"]["crews.sol.effort"], "high");
    assert_eq!(json["provenance"]["crews.sol.effort"]["scope"], "workspace");
    assert_eq!(
        json["provenance"]["crews.sol.effort"]["path"],
        workspace_root
            .join("config.toml")
            .to_string_lossy()
            .as_ref()
    );
    assert!(
        json["settings"].get("crews.opus.effort").is_none(),
        "omitted effort must not appear in settings: {}",
        json["settings"]
    );
}

#[test]
fn config_show_omits_the_retired_workspace_task_projection() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");

    let json = effective_json(&runtime, effective.values());
    let text = effective_text(&runtime, effective.values(), false);
    let scoped_store =
        open_store_for_scope(&runtime, ConfigScopeArg::Global).expect("open global scoped config");
    let scoped_snapshot = scoped_store.snapshot().expect("read scoped config");
    let scoped_settings = scoped_snapshot.all_values();
    let scoped_json = scoped_json(&runtime, &scoped_store, &scoped_snapshot, &scoped_settings);
    let scoped_text = scoped_text(
        &runtime,
        &scoped_store,
        &scoped_snapshot,
        &scoped_settings,
        false,
    );

    assert!(
        json["persistence"].get("task").is_none(),
        "configuration must not advertise the removed workspace task projection: {}",
        json["persistence"]
    );
    assert!(
        !text.contains("\"task\""),
        "text configuration output must not advertise the removed task store: {text}"
    );
    assert!(
        scoped_json["persistence"].get("task").is_none(),
        "scoped JSON must not advertise the removed task store: {}",
        scoped_json["persistence"]
    );
    assert!(
        !scoped_text.contains("\"task\""),
        "scoped text must not advertise the removed task store: {scoped_text}"
    );
}

#[test]
fn config_show_marks_absent_workspace_layer_and_lists_every_settable_key() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        "[workflow]\nbase_branch = \"integration\"\n",
    )
    .expect("write global config");

    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective");
    let json = effective_json(&runtime, effective.values());
    let text = effective_text(&runtime, effective.values(), true);

    // 1. Marks absent workspace layer
    assert_eq!(json["source"]["workspace_exists"], false);
    assert_eq!(json["source"]["global_exists"], true);
    assert!(text.contains("workspace"));
    assert!(
        text.contains("(absent)"),
        "text must mark absent layer: {text}"
    );

    // 2. `--all` lists every settable key from CONFIG_KEY_REGISTRY, under its
    //    section and with its registry description.
    for descriptor in orbit_config::CONFIG_KEY_REGISTRY {
        assert!(
            json["settings"].get(descriptor.key).is_some(),
            "settings must contain {}",
            descriptor.key
        );
        let label = match descriptor.section.key_prefix() {
            Some(prefix) => descriptor
                .key
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('.'))
                .unwrap_or(descriptor.key),
            None => descriptor.key,
        };
        assert!(
            text.contains(label),
            "text must list {} as {label}",
            descriptor.key
        );
        assert!(
            text.contains(descriptor.description),
            "text must explain {}",
            descriptor.key
        );
        assert!(
            text.contains(descriptor.section.title()),
            "text must render the section of {}",
            descriptor.key
        );
    }

    // 3. Value state is three-way and never doubles up
    assert!(
        text.contains("id_start") && text.contains("unset"),
        "an unset key must say so: {text}"
    );
    assert!(
        !text.contains("[unset]") && !text.contains("[built-in]"),
        "the retired bracketed provenance must be gone: {text}"
    );

    // 4. Scoped show for workspace also marks absent
    let scoped_store =
        open_store_for_scope(&runtime, ConfigScopeArg::Workspace).expect("open workspace store");
    let scoped_snapshot = scoped_store.snapshot().expect("snapshot");
    let scoped_settings = scoped_snapshot.all_values();
    let s_json = scoped_json(&runtime, &scoped_store, &scoped_snapshot, &scoped_settings);
    let s_text = scoped_text(
        &runtime,
        &scoped_store,
        &scoped_snapshot,
        &scoped_settings,
        true,
    );

    assert_eq!(s_json["source"]["exists"], false);
    assert!(s_text.contains("(absent)"));
    assert!(s_text.contains("unset"));
    assert!(
        !s_text.contains("[unset]"),
        "scoped text must use the three-way state: {s_text}"
    );
}

#[test]
fn every_effective_settings_key_is_readable_by_config_get() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();

    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");
    let json = effective_json(&runtime, effective.values());
    let text = effective_text(&runtime, effective.values(), false);

    let settings = json["settings"].as_object().expect("settings object");
    assert!(
        !settings.is_empty(),
        "expected at least one settings key to check"
    );
    for key in settings.keys() {
        admit_config_key(key).unwrap_or_else(|err| {
            panic!("settings key {key} must be readable by config get: {err}")
        });
    }

    // `execution.env.inherit` is a derived invariant, not an admitted config
    // key: it must not appear in `settings` (ORB-12339), and it must still
    // surface as the top-level `execution_env_inherit` field and, in text, as
    // a `derived` row inside the section it governs.
    assert!(
        !settings.contains_key("execution.env.inherit"),
        "settings must not list the non-admitted execution.env.inherit key: {settings:?}"
    );
    assert_eq!(json["execution_env_inherit"], false);
    assert!(
        text.contains("env.inherit") && text.contains("derived"),
        "text output must surface the derived env.inherit invariant: {text}"
    );
}

#[test]
fn effective_json_adds_section_description_state_and_shadowed_layers() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        r#"
[workflow]
base_branch = "main"

[execution.codex]
sandbox = "danger-full-access"
"#,
    )
    .expect("write global config");
    fs::write(
        workspace_root.join("config.toml"),
        "[workflow]\nbase_branch = \"agent-main\"\n",
    )
    .expect("write workspace config");

    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");
    let json = effective_json(&runtime, effective.values());

    let base_branch = &json["provenance"]["workflow.base_branch"];
    assert_eq!(base_branch["section"], "delivery");
    assert_eq!(base_branch["state"], "set");
    assert_eq!(base_branch["scope"], "workspace");
    // The registry owns the wording; asserting a copy of it here only breaks
    // this test when a description is reworded (ORB-12714 did exactly that).
    assert_eq!(
        base_branch["description"],
        describe_config_key("workflow.base_branch")
            .expect("workflow.base_branch is a registry key")
            .description
    );
    assert_eq!(base_branch["shadowed_by"][0]["layer"], "global");
    assert_eq!(base_branch["shadowed_by"][0]["value"], "main");
    assert_eq!(base_branch["shadowed_by"][0]["reason"], "overridden");

    // The security exception: global sets it, the workspace file does not
    // restate it, so the built-in default applies and the global value is
    // reported as not inherited rather than silently dropped.
    let sandbox = &json["provenance"]["execution.codex.sandbox"];
    assert_eq!(sandbox["section"], "execution");
    assert_eq!(sandbox["state"], "default");
    assert_eq!(sandbox["shadowed_by"][0]["reason"], "not-inherited");
    assert_eq!(
        sandbox["shadowed_by"][0]["value"], "danger-full-access",
        "the value that was not inherited must be named: {sandbox}"
    );

    // An unset key is unset, not defaulted.
    assert_eq!(json["provenance"]["tasks.id_start"]["state"], "unset");
    assert_eq!(
        json["provenance"]["tasks.id_start"]["section"],
        "housekeeping"
    );
    assert_eq!(
        json["provenance"]["tasks.id_start"]["shadowed_by"],
        serde_json::json!([])
    );

    // Unregistered checkouts have no registry binding to report.
    assert_eq!(json["workspace_binding"], serde_json::Value::Null);
}

#[test]
fn scoped_json_reports_section_and_per_file_state() {
    let (_root, runtime, global_root, _workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        "[workflow]\nbase_branch = \"integration\"\n",
    )
    .expect("write global config");

    let store =
        open_store_for_scope(&runtime, ConfigScopeArg::Global).expect("open global scoped config");
    let snapshot = store.snapshot().expect("snapshot");
    let settings = snapshot.all_values();
    let json = scoped_json(&runtime, &store, &snapshot, &settings);

    assert_eq!(json["settings"]["workflow.base_branch"], "integration");
    assert_eq!(json["provenance"]["workflow.base_branch"]["state"], "set");
    assert_eq!(
        json["provenance"]["workflow.base_branch"]["scope"],
        "global"
    );
    assert_eq!(
        json["provenance"]["workflow.base_branch"]["section"],
        "delivery"
    );
    assert_eq!(json["provenance"]["scoring.enabled"]["state"], "default");
    assert_eq!(json["provenance"]["tasks.id_start"]["state"], "unset");
    assert_eq!(
        json["provenance"]["tasks.id_start"]["path"],
        serde_json::Value::Null
    );
}

#[test]
fn workspace_line_reports_the_registry_base_branch_and_ship_mode() {
    let (root, _runtime, global_root, workspace_root) = test_runtime();
    let repo_root = root.path().join("repo");
    let runtime = OrbitRuntime::from_roots_with_binding(
        &global_root,
        &workspace_root,
        WorkspaceRuntimeBinding {
            logical_workspace_id: "ws_show".to_string(),
            // The partition id is the checkout identity the first runtime
            // wrote; a binding that restates a different one is refused.
            task_partition_id: configured_workspace_id(&workspace_root),
            owner_machine_id: None,
            repo_root: repo_root.clone(),
            ship_mode: ShipMode::Pr,
            base_branch: Some("agent-main".to_string()),
        },
    )
    .expect("build bound runtime");

    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");
    let text = effective_text(&runtime, effective.values(), false);
    let json = effective_json(&runtime, effective.values());

    assert!(
        text.contains("registered base branch: agent-main"),
        "the registry base branch delivery uses must be visible: {text}"
    );
    assert!(
        text.contains("ship mode: pr"),
        "the registry ship mode must be visible: {text}"
    );
    assert!(
        text.contains("from workspace registry, not config.toml"),
        "the registry source must be labelled: {text}"
    );
    assert_eq!(json["workspace_binding"]["base_branch"], "agent-main");
    assert_eq!(json["workspace_binding"]["ship_mode"], "pr");
    assert_eq!(json["workspace_binding"]["source"], "workspace-registry");
}

#[test]
fn all_unset_sections_collapse_until_all_is_passed() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");

    let collapsed = effective_text(&runtime, effective.values(), false);
    assert!(
        collapsed.contains("keys unset (pass --all to list them)"),
        "an all-unset section must collapse to one line: {collapsed}"
    );
    assert!(
        !collapsed.contains("review_reviewer_starts"),
        "a collapsed section must not print its rows: {collapsed}"
    );

    let expanded = effective_text(&runtime, effective.values(), true);
    assert!(
        expanded.contains("review_reviewer_starts"),
        "--all must restore every row: {expanded}"
    );
    assert!(
        !expanded.contains("pass --all to list them"),
        "--all must not also print the collapsed summary: {expanded}"
    );
}

#[test]
fn crews_render_as_one_row_each_with_their_referencing_keys() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    fs::write(
        global_root.join("config.toml"),
        r#"
[workflow]
default_crew = "sol"
system_crew = "luna"

[crews.sol]
model = "gpt-6-sol"
provider = "codex"
effort = "medium"
tags = ["deep"]

[crews.luna]
model = "gpt-6-luna"
provider = "codex"
"#,
    )
    .expect("write global config");

    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");
    let text = effective_text(&runtime, effective.values(), false);

    // sol and luna come from the file; `system` is seeded for the bounded
    // system lane, so three crews resolve from a two-crew file.
    for name in ["sol", "luna"] {
        let rows = text
            .lines()
            .filter(|line| line.trim_start().starts_with(&format!("{name} ")))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "one row per crew, not one per field: {text}");
        assert!(
            rows[0].contains("global"),
            "a file-defined crew must report its layer: {}",
            rows[0]
        );
    }
    assert!(
        text.contains("3 defined, from built-in and global"),
        "the crew section must summarise how many and from where: {text}"
    );
    assert!(
        !text.contains("crews.sol.model"),
        "per-field crew keys must not be listed individually: {text}"
    );
    assert!(
        text.contains("← workflow.default_crew"),
        "the crew the default points at must be annotated: {text}"
    );
    assert!(
        text.contains("← workflow.system_crew"),
        "the crew the system lane points at must be annotated: {text}"
    );
}

#[test]
fn paths_replace_the_single_line_persistence_blob() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");
    let text = effective_text(&runtime, effective.values(), false);

    assert!(text.contains("\nPaths\n"), "Paths section missing: {text}");
    assert!(
        !text.contains("derived:"),
        "the derived: block is replaced by Paths: {text}"
    );
    assert!(
        !text.contains("{\""),
        "persistence must be expanded into rows, not printed as JSON: {text}"
    );
    for label in ["global root", "workspace root", "audit db", "semantic db"] {
        assert!(text.contains(label), "Paths must list {label}: {text}");
    }
    assert!(
        text.contains("activities, executors, jobs, policies"),
        "resource dirs that share a parent must be folded into one row: {text}"
    );
}

/// The `workspace_id` the runtime recorded in `.orbit/config.yaml`.
fn configured_workspace_id(workspace_root: &Path) -> String {
    let identity =
        fs::read_to_string(workspace_root.join("config.yaml")).expect("read workspace identity");
    identity
        .lines()
        .find_map(|line| line.strip_prefix("workspace_id:"))
        .map(|value| value.trim().trim_matches('"').to_string())
        .expect("workspace identity must declare workspace_id")
}

/// Snapshot coverage for the four layering shapes an operator actually meets.
/// Regenerate with `ORBIT_UPDATE_CONFIG_SNAPSHOTS=1 cargo test -p orbit-cli
/// config::tests::show` and review the diff: a snapshot that moved without an
/// intended rendering change is the regression this catches.
const SNAPSHOT_UPDATE_ENV: &str = "ORBIT_UPDATE_CONFIG_SNAPSHOTS";

#[test]
fn effective_text_snapshot_bare_defaults() {
    assert_effective_snapshot("bare_defaults", None, None);
}

#[test]
fn effective_text_snapshot_global_only() {
    assert_effective_snapshot(
        "global_only",
        Some(
            r#"[workflow]
base_branch = "main"
default_crew = "sol"

[scoring]
enabled = false

[crews.sol]
model = "gpt-6-sol"
provider = "codex"
effort = "medium"
"#,
        ),
        None,
    );
}

#[test]
fn effective_text_snapshot_workspace_overrides_global() {
    assert_effective_snapshot(
        "workspace_overrides_global",
        Some(
            r#"[workflow]
base_branch = "main"
default_crew = "sol"

[crews.sol]
model = "gpt-6-sol"
provider = "codex"
effort = "medium"
"#,
        ),
        Some(
            r#"[workflow]
base_branch = "agent-main"

[crews.sol]
effort = "high"
"#,
        ),
    );
}

#[test]
fn effective_text_snapshot_execution_not_inherited() {
    assert_effective_snapshot(
        "execution_not_inherited",
        Some(
            r#"[execution.codex]
sandbox = "danger-full-access"
approval_policy = "never"

[execution.env]
pass = ["HOME", "PATH"]
"#,
        ),
        Some("[scoring]\nenabled = true\n"),
    );
}

fn assert_effective_snapshot(name: &str, global: Option<&str>, workspace: Option<&str>) {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    if let Some(global) = global {
        fs::write(global_root.join("config.toml"), global).expect("write global config");
    }
    if let Some(workspace) = workspace {
        fs::write(workspace_root.join("config.toml"), workspace).expect("write workspace config");
    }

    let effective = load_effective_config(&ConfigRoots::new(&global_root, &workspace_root))
        .expect("load effective layered config");
    let rendered = normalize_snapshot(
        &effective_text(&runtime, effective.values(), false),
        &global_root,
        &workspace_root,
    );

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/command/config/tests/snapshots")
        .join(format!("show_effective_{name}.txt"));
    if std::env::var(SNAPSHOT_UPDATE_ENV).is_ok() {
        fs::write(&path, &rendered).expect("update snapshot");
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "missing snapshot {}: {err} (regenerate with {SNAPSHOT_UPDATE_ENV}=1)",
            path.display()
        )
    });
    assert_eq!(
        rendered, expected,
        "`config show` text changed for {name}; regenerate with {SNAPSHOT_UPDATE_ENV}=1 after \
         reviewing the diff"
    );
}

/// Make one rendering comparable across machines: temp roots become stable
/// placeholders, and the macOS-only entry in the default `execution.env.pass`
/// list is dropped so the same snapshot holds on both platforms.
fn normalize_snapshot(text: &str, global_root: &Path, workspace_root: &Path) -> String {
    text.replace(&workspace_root.display().to_string(), "<workspace-root>")
        .replace(&global_root.display().to_string(), "<global-root>")
        .replace(", __CF_USER_TEXT_ENCODING", "")
}
