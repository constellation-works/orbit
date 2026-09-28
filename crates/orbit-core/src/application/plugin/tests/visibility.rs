//! The one visibility rule for plugin-seeded definitions: which definitions
//! are inactive, where, and whether a listing shows them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::super::{InactivePluginScope, PluginActivity, inactive_plugin, is_listed};

/// A workspace view with `graph` in one of three states.
#[derive(Default)]
struct Activity {
    active: BTreeSet<&'static str>,
    switched_off: BTreeSet<&'static str>,
}

impl PluginActivity for Activity {
    fn is_active(&self, namespace: &str) -> bool {
        self.active.contains(namespace)
    }

    fn is_disabled_in_workspace(&self, namespace: &str) -> bool {
        self.switched_off.contains(namespace)
    }
}

fn active() -> Activity {
    Activity {
        active: BTreeSet::from(["graph"]),
        ..Activity::default()
    }
}

fn workspace_disabled() -> Activity {
    Activity {
        switched_off: BTreeSet::from(["graph"]),
        ..Activity::default()
    }
}

fn host_disabled() -> Activity {
    Activity::default()
}

fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write definition");
    path
}

fn seeded(dir: &Path) -> PathBuf {
    write(
        dir,
        "graph-reindex.yaml",
        "# provenance: plugin:graph@1.0.0\nschemaVersion: 1\nname: graph-reindex\n",
    )
}

#[test]
fn a_definition_seeded_by_an_active_plugin_is_live_and_listed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seeded(dir.path());

    let inactive = inactive_plugin(&path, &active());

    assert_eq!(inactive, None);
    assert!(is_listed(inactive.is_some(), false));
}

#[test]
fn a_workspace_disabled_plugin_hides_its_definition_and_names_the_workspace_enable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seeded(dir.path());

    let inactive = inactive_plugin(&path, &workspace_disabled()).expect("inactive");

    assert_eq!(inactive.namespace, "graph");
    assert_eq!(inactive.version, "1.0.0");
    assert_eq!(inactive.scope, InactivePluginScope::Workspace);
    assert!(!is_listed(true, false), "hidden by default");
    assert!(is_listed(true, true), "listed on request");
    let reason = inactive.reason(&path, None);
    assert!(
        reason.contains("switched off in this workspace")
            && reason.contains("orbit plugin enable graph --scope workspace")
            && reason.contains(&path.display().to_string()),
        "{reason}"
    );
    let named = inactive.reason(&path, Some("alpha"));
    assert!(
        named.contains("switched off in workspace 'alpha'"),
        "{named}"
    );
}

#[test]
fn a_host_disabled_plugin_hides_its_definition_and_names_the_host_enable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seeded(dir.path());

    let inactive = inactive_plugin(&path, &host_disabled()).expect("inactive");

    assert_eq!(inactive.scope, InactivePluginScope::Host);
    let reason = inactive.reason(&path, Some("alpha"));
    assert!(
        reason.contains("not enabled on this host")
            && reason.contains("`orbit plugin enable graph`")
            && !reason.contains("--scope workspace"),
        "{reason}"
    );
}

#[test]
fn a_user_authored_definition_is_never_hidden_whatever_the_plugin_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let authored = write(
        dir.path(),
        "chore.yaml",
        "schemaVersion: 1\nname: chore\n# provenance: plugin:graph@1.0.0\n",
    );

    for activity in [active(), workspace_disabled(), host_disabled()] {
        assert_eq!(
            inactive_plugin(&authored, &activity),
            None,
            "a provenance comment after the document starts is prose, not a claim"
        );
    }
    assert_eq!(
        inactive_plugin(&dir.path().join("missing.yaml"), &host_disabled()),
        None
    );
}
