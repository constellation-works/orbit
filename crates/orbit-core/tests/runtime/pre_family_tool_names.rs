//! Stored pre-family tool names converge to family actions [ORB-23552]:
//! once on upgrade, and again wherever Orbit meets one (admission, auto-task
//! mint), without turning a managed definition into a fork.

use std::path::{Path, PathBuf};

use orbit_core::OrbitRuntime;
use orbit_core::application::auto_tasks::AutoTaskBody;
use orbit_core::application::task::TaskAddParams;
use orbit_core::application::workspace_sync::reconcile_workspace_managed_artifacts;
use orbit_engine::RuntimeHost;
use orbit_types::plugin::InstalledPlugin;
use orbit_types::task::TASK_ENVELOPE_FILE_NAME;
use tempfile::TempDir;

/// Family actions and the operation names 0.46 stored for them.
const RENAMES: &[(&str, &str)] = &[
    ("orbit.task.artifact:put", "orbit.task.artifact.put"),
    ("orbit.task.artifact:get", "orbit.task.artifact.get"),
    ("orbit.decision:raise", "orbit.decision.raise"),
];

const MIGRATION_MARKER: &str = "tool-family-names-v1.migrated";
const MIGRATION_LEDGER: &str = "tool-name-migrations.jsonl";
const MANAGED_MANIFEST: &str = ".orbit-managed-assets.json";

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(workspace.join("auto_tasks")).unwrap();
        Self {
            _root: root,
            global,
            workspace,
        }
    }

    fn open(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global, &self.workspace).unwrap()
    }

    fn auto_task(&self, name: &str) -> PathBuf {
        self.workspace.join(format!("auto_tasks/{name}.yaml"))
    }

    /// Forget the upgrade pass, as a store 0.46 wrote never ran it.
    fn forget_migration(&self) {
        for state in [self.global.join("state"), self.workspace.join("state")] {
            let marker = state.join(MIGRATION_MARKER);
            if marker.exists() {
                std::fs::remove_file(marker).unwrap();
            }
        }
    }
}

/// `text` with every family action a block list names renamed to the
/// operation 0.46 stored; prose is left alone.
fn pre_family(text: &str) -> String {
    RENAMES
        .iter()
        .fold(text.to_string(), |text, (family, operation)| {
            text.replace(&format!("- {family}\n"), &format!("- {operation}\n"))
        })
}

fn sha256(bytes: &[u8]) -> String {
    orbit_common::security::release::sha256_hex(bytes)
}

/// Record `name`'s current bytes as the ones Orbit wrote, as 0.46's sync did.
fn track_as_managed(workspace: &Path, name: &str) {
    let dir = workspace.join("auto_tasks");
    let manifest_path = dir.join(MANAGED_MANIFEST);
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let digest = sha256(&std::fs::read(dir.join(format!("{name}.yaml"))).unwrap());
    let assets = manifest
        .as_object_mut()
        .unwrap()
        .values_mut()
        .find(|value| value.get(name).is_some())
        .unwrap_or_else(|| panic!("the manifest tracks {name}: {manifest_path:?}"));
    assets[name] = serde_json::Value::String(digest);
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn task_envelope(global: &Path, id: &str) -> PathBuf {
    fn find(dir: &Path, id: &str) -> Option<PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.file_name().is_some_and(|name| name == id)
                && path.join(TASK_ENVELOPE_FILE_NAME).is_file()
            {
                return Some(path.join(TASK_ENVELOPE_FILE_NAME));
            }
            if let Some(found) = find(&path, id) {
                return Some(found);
            }
        }
        None
    }
    find(global, id).unwrap_or_else(|| panic!("task {id} has a bundle under {global:?}"))
}

/// Write `required` and `granted` into the task's stored envelope, as 0.46
/// stored them.
fn store_pre_family_task_tools(global: &Path, id: &str, required: &[&str], granted: &[&str]) {
    let path = task_envelope(global, id);
    let mut envelope: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let list = |names: &[&str]| {
        serde_yaml::Value::Sequence(
            names
                .iter()
                .map(|name| serde_yaml::Value::String((*name).to_string()))
                .collect(),
        )
    };
    envelope["required_tools"] = list(required);
    envelope["granted_tools"] = list(granted);
    std::fs::write(&path, serde_yaml::to_string(&envelope).unwrap()).unwrap();
}

fn add_task(runtime: &OrbitRuntime, title: &str) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn history_events(runtime: &OrbitRuntime, id: &str) -> Vec<(String, String)> {
    runtime
        .get_task_history(id)
        .unwrap()
        .into_iter()
        .map(|entry| (entry.event, entry.note.unwrap_or_default()))
        .collect()
}

const LOCAL_DEFINITION: &str = "\
schemaVersion: 1
name: perf-review
description: Workspace-local review
enabled: false
# keep this note
schedule:
  cron: 0 * * * *
template:
  title: Perf review
  description: Review performance.
  required_tools:
    - orbit.task.show
    - orbit.task.artifact.put  # attach the report
  tags: [perf]
";

#[test]
fn upgrade_from_a_pre_family_store_rewrites_every_stored_tool_name() {
    if !super::dispatch_admission::isolated(
        "pre_family_tool_names::upgrade_from_a_pre_family_store_rewrites_every_stored_tool_name",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let runtime = fixture.open();
    reconcile_workspace_managed_artifacts(&fixture.global, &fixture.workspace, None, "main", false)
        .unwrap();

    // A managed default whose bundled body 0.46 shipped with operation
    // names, a fork of another default, and a workspace-local definition.
    let managed = fixture.auto_task("code-review");
    let shipped = std::fs::read_to_string(&managed).unwrap();
    assert_ne!(
        pre_family(&shipped),
        shipped,
        "code-review requires a family action"
    );
    std::fs::write(&managed, pre_family(&shipped)).unwrap();
    track_as_managed(&fixture.workspace, "code-review");
    assert_eq!(
        runtime.auto_task_layering("code-review").unwrap().body,
        AutoTaskBody::Managed
    );
    let forked = fixture.auto_task("qa-full-sweep");
    let shipped_fork = std::fs::read_to_string(&forked).unwrap();
    std::fs::write(
        &forked,
        format!("# operator note\n{}", pre_family(&shipped_fork)),
    )
    .unwrap();
    let forked_fields_before = runtime
        .auto_task_layering("qa-full-sweep")
        .unwrap()
        .forked_fields;
    std::fs::write(fixture.auto_task("perf-review"), LOCAL_DEFINITION).unwrap();

    // Tasks minted before tool families, with a requirement and a grant.
    let task = add_task(&runtime, "Minted by 0.46");
    store_pre_family_task_tools(
        &fixture.global,
        &task,
        &["orbit.task.artifact.put", "orbit.task.show"],
        &["orbit.decision.raise"],
    );
    let current = add_task(&runtime, "Minted by 0.47");

    // An installed plugin whose manifest 0.46 recorded under its digest.
    let install = fixture.global.join("plugins/callback/1.0.0");
    std::fs::create_dir_all(&install).unwrap();
    let manifest = "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: callback\n  version: 1.0.0\nspec:\n  permissions:\n    # callbacks it makes\n    orbit_tools: [orbit.task.show, orbit.task.artifact.get]\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: hello\n      execution_kind: read_only\n      mcp_scope: workspace\n";
    std::fs::write(install.join("plugin.yaml"), manifest).unwrap();
    let plugins = orbit_store::compose::plugin_store_sqlite(runtime.sqlite_store().unwrap());
    plugins
        .upsert_plugin(&InstalledPlugin {
            name: "callback".into(),
            version: "1.0.0".into(),
            source: "fixture".into(),
            install_path: install.to_string_lossy().into_owned(),
            archive_digest: None,
            manifest_digest: orbit_tools::plugin::manifest_digest(manifest.as_bytes()),
            enabled: true,
            grants: vec!["orbit_tools".into()],
            first_party: false,
            certified_orbit_version: None,
            build: None,
            installed_at: String::new(),
            updated_at: String::new(),
        })
        .unwrap();
    drop(runtime);
    fixture.forget_migration();

    // The upgrade: the next writable open converges everything.
    let runtime = fixture.open();

    let managed_after = std::fs::read_to_string(&managed).unwrap();
    assert_eq!(
        managed_after, shipped,
        "the managed body holds family actions"
    );
    assert_eq!(
        runtime.auto_task_layering("code-review").unwrap().body,
        AutoTaskBody::Managed,
        "the rename keeps a managed definition managed"
    );
    let fork_after = std::fs::read_to_string(&forked).unwrap();
    assert_eq!(fork_after, format!("# operator note\n{shipped_fork}"));
    let fork_layering = runtime.auto_task_layering("qa-full-sweep").unwrap();
    assert_eq!(fork_layering.body, AutoTaskBody::Forked);
    assert!(
        fork_layering
            .forked_fields
            .iter()
            .all(|field| forked_fields_before.contains(field)),
        "the rename adds no forked field: {:?} then {:?}",
        forked_fields_before,
        fork_layering.forked_fields
    );
    let local_after = std::fs::read_to_string(fixture.auto_task("perf-review")).unwrap();
    assert_eq!(
        local_after,
        LOCAL_DEFINITION.replace("orbit.task.artifact.put", "orbit.task.artifact:put"),
        "a workspace-local definition is rewritten in place, comments kept"
    );
    assert_eq!(
        runtime
            .auto_task_show("perf-review")
            .unwrap()
            .unwrap()
            .template
            .required_tools,
        vec!["orbit.task.artifact:put", "orbit.task.show"]
    );

    let migrated = runtime.get_task(&task).unwrap();
    assert_eq!(
        migrated.required_tools,
        vec!["orbit.task.artifact:put", "orbit.task.show"]
    );
    assert_eq!(migrated.granted_tools, vec!["orbit.decision:raise"]);
    assert!(
        history_events(&runtime, &task)
            .iter()
            .any(|(event, note)| event == "tool_names_migrated"
                && note.contains("orbit.task.artifact.put")
                && note.contains("orbit.task.artifact:put")),
        "the rewrite is recorded in the task's history"
    );
    assert!(
        !history_events(&runtime, &current)
            .iter()
            .any(|(event, _)| event == "tool_names_migrated"),
        "a task already holding family actions is untouched"
    );

    let manifest_after = std::fs::read_to_string(install.join("plugin.yaml")).unwrap();
    assert_eq!(
        manifest_after,
        manifest.replace("orbit.task.artifact.get", "orbit.task.artifact:get")
    );
    assert_eq!(
        plugins
            .get_plugin("callback")
            .unwrap()
            .unwrap()
            .manifest_digest,
        orbit_tools::plugin::manifest_digest(manifest_after.as_bytes()),
        "the install record follows the rewritten manifest"
    );

    let workspace_ledger =
        std::fs::read_to_string(fixture.workspace.join("state").join(MIGRATION_LEDGER)).unwrap();
    for name in ["code-review", "qa-full-sweep", "perf-review"] {
        assert!(
            workspace_ledger.contains(&format!("\"name\":\"{name}\"")),
            "{name} rewrite recorded: {workspace_ledger}"
        );
    }
    let global_ledger =
        std::fs::read_to_string(fixture.global.join("state").join(MIGRATION_LEDGER)).unwrap();
    assert!(global_ledger.contains("\"name\":\"callback\""));

    // Workspace sync then finds nothing left to fork or rename.
    let report = reconcile_workspace_managed_artifacts(
        &fixture.global,
        &fixture.workspace,
        None,
        "main",
        true,
    )
    .unwrap();
    assert!(
        !report.actions.iter().any(|action| action
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("pre-family"))),
        "{report:?}"
    );
}

#[test]
fn admission_and_mint_rewrite_pre_family_names_they_meet() {
    if !super::dispatch_admission::isolated(
        "pre_family_tool_names::admission_and_mint_rewrite_pre_family_names_they_meet",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let runtime = fixture.open();

    // Admission: a task stored with an operation name admits as its family
    // action, and the stored task is rewritten.
    let task = add_task(&runtime, "Stored before tool families");
    store_pre_family_task_tools(&fixture.global, &task, &["orbit.task.artifact.put"], &[]);
    let runtime = fixture.open();
    let admitted = runtime
        .resolve_activity_tools(std::slice::from_ref(&task), &[])
        .unwrap();
    assert_eq!(admitted.requested_tools, vec!["orbit.task.artifact:put"]);
    assert_eq!(
        runtime.get_task(&task).unwrap().required_tools,
        vec!["orbit.task.artifact:put"]
    );
    assert!(
        history_events(&runtime, &task)
            .iter()
            .any(|(event, _)| event == "tool_names_migrated")
    );

    // Mint: a definition written with operation names is rewritten, and
    // the task it mints holds family actions.
    std::fs::write(fixture.auto_task("perf-review"), LOCAL_DEFINITION).unwrap();
    let minted = runtime.auto_task_mint("perf-review").unwrap();
    assert_eq!(
        minted.required_tools,
        vec!["orbit.task.artifact:put", "orbit.task.show"]
    );
    assert_eq!(
        std::fs::read_to_string(fixture.auto_task("perf-review")).unwrap(),
        LOCAL_DEFINITION.replace("orbit.task.artifact.put", "orbit.task.artifact:put")
    );
    let ledger =
        std::fs::read_to_string(fixture.workspace.join("state").join(MIGRATION_LEDGER)).unwrap();
    assert!(ledger.contains("\"name\":\"perf-review\""), "{ledger}");
}

#[test]
fn workspace_sync_rewrites_pre_family_names_in_a_forked_managed_definition() {
    if !super::dispatch_admission::isolated(
        "pre_family_tool_names::workspace_sync_rewrites_pre_family_names_in_a_forked_managed_definition",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let runtime = fixture.open();
    reconcile_workspace_managed_artifacts(&fixture.global, &fixture.workspace, None, "main", false)
        .unwrap();

    // A fork written after the upgrade pass ran, so only sync can find it.
    let forked = fixture.auto_task("qa-full-sweep");
    let shipped = std::fs::read_to_string(&forked).unwrap();
    let old = pre_family(&shipped);
    assert_ne!(old, shipped, "qa-full-sweep requires a family action");
    std::fs::write(&forked, format!("# operator note\n{old}")).unwrap();
    let manifest_before =
        std::fs::read(fixture.workspace.join("auto_tasks").join(MANAGED_MANIFEST)).unwrap();
    let forked_fields_before = runtime
        .auto_task_layering("qa-full-sweep")
        .unwrap()
        .forked_fields;
    drop(runtime);

    let check = reconcile_workspace_managed_artifacts(
        &fixture.global,
        &fixture.workspace,
        None,
        "main",
        true,
    )
    .unwrap();
    assert!(
        check
            .actions
            .iter()
            .any(|action| action.name == "qa-full-sweep"
                && action
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("pre-family"))),
        "--check lists the pending rewrite: {check:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&forked).unwrap(),
        format!("# operator note\n{old}"),
        "--check writes nothing"
    );

    reconcile_workspace_managed_artifacts(&fixture.global, &fixture.workspace, None, "main", false)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&forked).unwrap(),
        format!("# operator note\n{shipped}"),
        "sync rewrites the list items and keeps the operator's edit"
    );
    assert_eq!(
        std::fs::read(fixture.workspace.join("auto_tasks").join(MANAGED_MANIFEST)).unwrap(),
        manifest_before,
        "the manifest still records Orbit's bytes, so the file stays a fork"
    );
    let runtime = fixture.open();
    let layering = runtime.auto_task_layering("qa-full-sweep").unwrap();
    assert_eq!(layering.body, AutoTaskBody::Forked);
    assert!(
        layering
            .forked_fields
            .iter()
            .all(|field| forked_fields_before.contains(field)),
        "the rename adds no forked field: {forked_fields_before:?} then {:?}",
        layering.forked_fields
    );
}
