//! `{{plugin_state}}` is private to its plugin. `state/plugins/` is a denied
//! tree like the callback sessions and grant witnesses, and each backend is
//! granted back its own `state/plugins/<ns>` whole — so a plugin can keep a
//! credential there that no other plugin backend can read.

use super::*;

/// A plugin named `name` under `global_root`, whose manifest writes its own
/// state and — trying to buy the other plugin's state back — asks to read
/// `{{plugin_state}}/../<other>`.
fn state_plugin(
    global_root: &Path,
    plugin_root: &Path,
    name: &str,
    other: &str,
    grants: &[PluginGrant],
) -> PluginBackendSpec {
    let permissions = PluginPermissions {
        fs: PluginFsPermissions {
            read: vec![format!("{{{{plugin_state}}}}/../{other}")],
            write: vec!["{{plugin_state}}".into()],
        },
        ..PluginPermissions::default()
    };
    let mut spec = (*spec(
        plugin_root.join("backend.sh"),
        plugin_root,
        permissions,
        grants,
    ))
    .clone();
    spec.provenance.name = name.to_string();
    spec.global_root = global_root.to_path_buf();
    spec.state_dir = global_root.join("state/plugins").join(name);
    spec
}

/// The profile shape both platforms compile from: `state/plugins/` denied,
/// the plugin's own state re-allowed, and a manifest read root that resolves
/// into another plugin's state never reaching the profile.
#[test]
fn a_profile_denies_the_state_tree_and_re_allows_only_its_own_namespace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    std::fs::create_dir_all(global_root.join("state/plugins/other")).expect("other state");
    for grants in [
        &[PluginGrant::Fs][..],
        &[PluginGrant::Fs, PluginGrant::OrbitTools][..],
    ] {
        let spec = state_plugin(
            &global_root,
            &temp.path().join("demo"),
            "demo",
            "other",
            grants,
        );
        let profile = spec.sandbox_profile(None).expect("profile");
        let own_state = global_root.join("state/plugins/demo");

        assert!(
            profile
                .read_denies
                .contains(&global_root.join("state/plugins")),
            "every plugin's state is a host-owned tree: {:?}",
            profile.read_denies
        );
        assert!(
            profile.read.contains(&own_state),
            "the plugin's own state is re-allowed: {:?}",
            profile.read
        );
        assert!(
            !profile
                .read
                .iter()
                .any(|path| physical_with_missing_tail(path).ends_with("state/plugins/other")),
            "a manifest root cannot buy back another plugin's state: {:?}",
            profile.read
        );
        assert_eq!(profile.readable_denied_trees(), vec![own_state.clone()]);
        assert!(
            !profile
                .readable_denied_files()
                .iter()
                .any(|path| path.starts_with(global_root.join("state/plugins"))),
            "state is re-allowed as a tree, never as a literal: {:?}",
            profile.readable_denied_files()
        );
    }
}
