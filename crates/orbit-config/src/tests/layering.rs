//! Global-over-workspace layering and source provenance.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use tempfile::tempdir;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;

use super::{roots, write_config};
use crate::load_effective_config;
use crate::resolved::RETIRED_ROUTINES_CONFIG_WARNING;
use crate::{ConfigRoots, ConfigSnapshot, ConfigValueSourceKind, ResolvedConfig};

#[derive(Clone)]
struct CaptureMakeWriter(Arc<Mutex<Vec<u8>>>);

struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl<'writer> MakeWriter<'writer> for CaptureMakeWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        CaptureWriter(Arc::clone(&self.0))
    }
}

impl Write for CaptureWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("capture lock")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn capture_warnings<T>(action: impl FnOnce() -> T) -> (T, Vec<serde_json::Value>) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(CaptureMakeWriter(Arc::clone(&buffer)))
        .with_max_level(LevelFilter::WARN)
        .with_target(false)
        .without_time()
        .finish();
    let result = tracing::subscriber::with_default(subscriber, action);
    let output = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
        .expect("warning output is UTF-8");
    let warnings = output
        .lines()
        .map(|line| serde_json::from_str(line).expect("warning is JSON"))
        .collect();
    (result, warnings)
}

fn routines_warning_paths(warnings: &[serde_json::Value]) -> Vec<&str> {
    warnings
        .iter()
        .filter_map(|warning| {
            let fields = warning.get("fields")?;
            if fields.get("RETIRED_ROUTINES_CONFIG_WARNING")?.as_str()?
                != RETIRED_ROUTINES_CONFIG_WARNING
            {
                return None;
            }
            fields.get("config")?.as_str()
        })
        .collect()
}

#[test]
fn global_only_retired_routines_warning_names_global_config() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(global.path(), "[routines]\nrole = \"source\"\n");
    write_config(workspace.path(), "[scoring]\nenabled = false\n");

    let (result, warnings) =
        capture_warnings(|| ResolvedConfig::load(&roots(global.path(), workspace.path())));

    result.expect("layered config loads");
    let global_path = global.path().join("config.toml");
    assert_eq!(
        routines_warning_paths(&warnings),
        vec![global_path.to_str().expect("UTF-8 path")]
    );
}

#[test]
fn workspace_only_retired_routines_warning_names_workspace_config() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(global.path(), "[scoring]\nenabled = false\n");
    write_config(workspace.path(), "[routines]\nrole = \"source\"\n");

    let (result, warnings) =
        capture_warnings(|| ResolvedConfig::load(&roots(global.path(), workspace.path())));

    result.expect("layered config loads");
    let workspace_path = workspace.path().join("config.toml");
    assert_eq!(
        routines_warning_paths(&warnings),
        vec![workspace_path.to_str().expect("UTF-8 path")]
    );
}

#[test]
fn retired_routines_warning_names_each_layer_that_requires_cleanup_once() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(global.path(), "[routines]\nrole = \"source\"\n");
    write_config(workspace.path(), "[routines]\nrole = \"source\"\n");

    let (result, warnings) =
        capture_warnings(|| ResolvedConfig::load(&roots(global.path(), workspace.path())));

    result.expect("layered config loads");
    let global_path = global.path().join("config.toml");
    let workspace_path = workspace.path().join("config.toml");
    assert_eq!(
        routines_warning_paths(&warnings),
        vec![
            global_path.to_str().expect("UTF-8 path"),
            workspace_path.to_str().expect("UTF-8 path"),
        ]
    );
}

#[test]
fn workspace_single_key_inherits_other_global_keys_then_built_in_defaults() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        global.path(),
        "[workflow]\nbase_branch = \"global-branch\"\n[scoring]\nenabled = false\n",
    );
    write_config(workspace.path(), "[workflow]\nauto_ship = true\n");

    let config = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect("workspace config loads");

    assert!(config.workflow_auto_ship);
    assert_eq!(config.workflow_base_branch, "global-branch");
    assert!(!config.scoring_enabled);
}

#[test]
fn workspace_file_does_not_inherit_security_relevant_global_keys() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        global.path(),
        r#"
[execution.codex]
sandbox = "danger-full-access"
approval_policy = "on-request"

[execution.env]
inherit = true
pass = ["GLOBAL_SECRET"]
"#,
    );
    write_config(workspace.path(), "[scoring]\nenabled = false\n");

    let config = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect("workspace config loads");

    assert_eq!(config.codex_execution.sandbox(), "workspace-write");
    assert_eq!(config.codex_execution.approval_policy(), None);
    assert_eq!(
        config.snapshot.execution_env_pass,
        ConfigSnapshot::default().execution_env_pass
    );
    assert!(!config.execution_env.inherit());
}

/// A caller that hands the same directory in as both roots has no workspace
/// layer, so the replace-only rule must not fire against its own file.
#[test]
fn a_single_root_read_as_both_layers_keeps_its_security_settings() {
    let root = tempdir().expect("root tempdir");
    write_config(
        root.path(),
        "[execution.codex]\nsandbox = \"danger-full-access\"\n",
    );

    let config =
        ResolvedConfig::load(&ConfigRoots::global_only(root.path())).expect("global-only load");

    assert_eq!(config.codex_execution.sandbox(), "danger-full-access");
}

#[test]
fn workspace_crew_field_override_keeps_global_crew_fields_and_other_crews() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        global.path(),
        r#"
[workflow]
default_crew = "build"

[crews.build]
model = "global-model"
provider = "codex"
backend = "cli"

[crews.review]
model = "review-model"
provider = "claude"
backend = "cli"
"#,
    );
    write_config(
        workspace.path(),
        r#"
[crews.build]
model = "workspace-model"
"#,
    );

    let config = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect("layered crew config loads");

    let build = config.crews.get("build").expect("overridden crew remains");
    assert_eq!(build.assignment.model, "workspace-model");
    assert_eq!(build.assignment.provider, "codex");
    assert_eq!(
        config
            .crews
            .get("review")
            .expect("global-only crew remains")
            .assignment
            .model,
        "review-model"
    );
}

#[test]
fn layered_config_rejects_legacy_global_crew_before_flat_workspace_override() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        global.path(),
        r#"
[workflow]
default_crew = "build"

[crews.build]
planner = { model = "old-model", provider = "codex", backend = "cli" }
implementer = { model = "old-model", provider = "codex", backend = "cli" }
reviewer = { model = "old-model", provider = "codex", backend = "cli" }
"#,
    );
    write_config(
        workspace.path(),
        r#"
[crews.build]
model = "new-model"
"#,
    );

    let error = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect_err("legacy global crew must not be masked by workspace fields");
    let message = error.to_string();

    assert!(message.contains("[crews.build]"), "{message}");
    assert!(
        message.contains("planner/implementer/reviewer"),
        "{message}"
    );
}

#[test]
fn effective_config_attributes_values_to_workspace_global_and_built_in_sources() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        global.path(),
        r#"
[workflow]
base_branch = "integration"
default_crew = "build"

[execution.codex]
sandbox = "danger-full-access"

[crews.build]
model = "global-model"
provider = "codex"
backend = "cli"
"#,
    );
    write_config(
        workspace.path(),
        r#"
[scoring]
enabled = false

[crews.build]
model = "workspace-model"
"#,
    );

    let effective = load_effective_config(&roots(global.path(), workspace.path()))
        .expect("effective config loads");
    let values = effective
        .values()
        .iter()
        .map(|entry| (entry.key.as_str(), entry))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        values["scoring.enabled"].source.kind(),
        ConfigValueSourceKind::Workspace
    );
    assert_eq!(
        values["workflow.base_branch"].source.kind(),
        ConfigValueSourceKind::Global
    );
    assert_eq!(
        values["execution.codex.sandbox"].source.kind(),
        ConfigValueSourceKind::BuiltIn
    );
    assert_eq!(
        values["crews.build.model"].source.kind(),
        ConfigValueSourceKind::Workspace
    );
    assert_eq!(
        values["crews.build.provider"].source.kind(),
        ConfigValueSourceKind::Global
    );
    let workspace_config_path = workspace.path().join("config.toml");
    assert_eq!(
        values["scoring.enabled"].source.path(),
        Some(workspace_config_path.as_path())
    );
    let global_config_path = global.path().join("config.toml");
    assert_eq!(
        values["crews.build.model"].source.path(),
        Some(workspace_config_path.as_path())
    );
    assert_eq!(
        values["crews.build.provider"].source.path(),
        Some(global_config_path.as_path())
    );
    assert!(
        !values.contains_key("crews.build.effort"),
        "omitted effort must not appear as a configured value"
    );
}

#[test]
fn effective_config_projects_configured_crew_effort_with_layer_provenance() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        global.path(),
        r#"
[workflow]
default_crew = "sol"

[crews.sol]
model = "gpt-5.6-sol"
provider = "codex"
effort = "medium"

[crews.opus]
model = "opus"
provider = "claude"
"#,
    );
    write_config(
        workspace.path(),
        r#"
[crews.sol]
effort = "high"
"#,
    );

    let effective = load_effective_config(&roots(global.path(), workspace.path()))
        .expect("effective config loads");
    let values = effective
        .values()
        .iter()
        .map(|entry| (entry.key.as_str(), entry))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(values["crews.sol.effort"].value, serde_json::json!("high"));
    assert_eq!(
        values["crews.sol.effort"].source.kind(),
        ConfigValueSourceKind::Workspace
    );
    assert_eq!(
        values["crews.sol.effort"].source.path(),
        Some(workspace.path().join("config.toml").as_path())
    );
    assert_eq!(
        values["crews.sol.model"].source.kind(),
        ConfigValueSourceKind::Global
    );
    assert!(
        !values.contains_key("crews.opus.effort"),
        "omitted Claude effort must not invent a configured value"
    );
    assert_eq!(
        effective.value_for("crews.sol.effort"),
        Some(serde_json::json!("high"))
    );
    assert_eq!(
        effective.value_for("crews.opus.effort"),
        Some(serde_json::Value::Null)
    );
}

#[test]
fn crate_and_user_docs_share_the_layering_contract() {
    const CONTRACT: &str = "Ordinary settings inherit per key: workspace values override global values, global values fill omissions, and built-in defaults fill remaining gaps.";
    let crate_docs = include_str!("../lib.rs");
    let user_docs = include_str!("../../../../docs/CONFIG.md");

    assert!(crate_docs.contains(CONTRACT));
    assert!(user_docs.contains(CONTRACT));
}
