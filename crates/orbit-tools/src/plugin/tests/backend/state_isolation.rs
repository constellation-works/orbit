//! `{{plugin_state}}` is private to its plugin. `state/plugins/` is a denied
//! tree like the callback sessions and grant witnesses, and each backend is
//! granted back its own `state/plugins/<ns>` whole — so a plugin can keep a
//! credential there that no other plugin backend can read.

use super::*;

/// Writes a marker into its own state, reads it back, then probes the other
/// plugin's state (named by `OTHER_STATE`) and the `state/plugins/` tree that
/// holds both. Every refusal the boundary should produce leaves `result` at
/// `ok`; every access it should not allow is appended.
const STATE_PROBE_BACKEND: &str = r#"#!/bin/sh
cat >/dev/null
result=ok
echo "$ORBIT_PLUGIN" > "$ORBIT_PLUGIN_STATE/marker" 2>/dev/null || result="$result,own_write_denied"
[ "$(cat "$ORBIT_PLUGIN_STATE/marker" 2>/dev/null)" = "$ORBIT_PLUGIN" ] || result="$result,own_read_denied"
ls "$ORBIT_PLUGIN_STATE" >/dev/null 2>&1 || result="$result,own_list_denied"
if cat "$OTHER_STATE/secret" >/dev/null 2>&1; then result="$result,other_read"; fi
if ls "$OTHER_STATE" >/dev/null 2>&1; then result="$result,other_listed"; fi
if ls "$(dirname "$ORBIT_PLUGIN_STATE")" >/dev/null 2>&1; then result="$result,state_tree_listed"; fi
printf '{"ok":true,"output":{"result":"%s"}}\n' "$result"
"#;

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

/// The shared spawn path makes the plugin's state before launching an exec
/// backend, including when `fs` was not granted. This starts with a fresh
/// global root so an earlier install or conformance run cannot mask it.
#[cfg(unix)]
#[test]
fn an_exec_call_creates_its_own_state_before_backend_launch() {
    for grants in [
        &[PluginGrant::Fs, PluginGrant::Unsandboxed][..],
        &[PluginGrant::Unsandboxed][..],
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        let global_root = temp.path().join("global");
        let plugin_root = temp.path().join("demo");
        std::fs::create_dir_all(&plugin_root).expect("plugin root");
        let command = stub_backend(
            &plugin_root,
            "#!/bin/sh\ncat >/dev/null\n[ -d \"$ORBIT_PLUGIN_STATE\" ] || exit 3\nprintf '{\"ok\":true,\"output\":{\"result\":\"ok\"}}\\n'\n",
        );
        let mut spec = state_plugin(&global_root, &plugin_root, "demo", "other", grants);
        spec.command = command;
        spec.sandbox = PluginSandbox::None;
        let state = spec.state_dir.clone();
        let profile = spec.sandbox_profile(None).expect("profile");
        if grants.contains(&PluginGrant::Fs) {
            assert_eq!(
                profile.write.as_slice(),
                std::slice::from_ref(&state),
                "fs grants only the own state"
            );
        } else {
            assert!(
                profile.write.is_empty(),
                "state creation does not grant writes"
            );
        }
        assert!(
            !state.exists(),
            "fresh root has no plugin state before the call"
        );

        tool(std::sync::Arc::new(spec), None)
            .execute(&context(&plugin_root), json!({}))
            .expect("exec backend launches with host-created state");

        assert!(
            state.is_dir(),
            "Orbit creates the state before backend launch"
        );
    }
}

/// A `/var`-style alias must resolve to the same physical path for the write
/// grant, the state-tree deny, and the plugin's own read carve-out.
#[cfg(unix)]
#[test]
fn seatbelt_state_rules_use_the_physical_path_of_an_aliased_root() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let base = temp.path().canonicalize().expect("physical temp root");
    let real = base.join("real");
    let alias = base.join("alias");
    std::fs::create_dir(&real).expect("real root");
    symlink(&real, &alias).expect("root alias");
    let spec = state_plugin(
        &alias.join("global"),
        &real.join("demo"),
        "demo",
        "other",
        &[PluginGrant::Fs],
    );
    let profile = spec.sandbox_profile(None).expect("profile");
    let state = real.join("global/state/plugins/demo");
    let rules = profile.macos_fs_rules();
    assert_eq!(
        rules.modify,
        [format!("{}/**", state.display())],
        "Seatbelt writes only the physical plugin state path"
    );
    assert!(
        rules.read.contains(&format!("{}/**", state.display())),
        "Seatbelt reads the physical plugin state path"
    );

    let mut text = orbit_exec::compile_macos_sandbox_profile(&rules, "plugin")
        .expect("compile Seatbelt rules");
    orbit_exec::append_macos_read_boundary(
        &mut text,
        &profile.read_denies,
        &profile.readable_denied_trees(),
        &profile.readable_denied_files(),
    );
    assert!(
        text.contains(&format!(
            "(deny file-read* (subpath \"{}\"))",
            real.join("global/state/plugins").display()
        )),
        "Seatbelt must deny the physical parent of all plugin states"
    );
    assert!(
        text.contains(&format!(
            "(allow file-read* (subpath \"{}\"))",
            state.display()
        )),
        "Seatbelt must re-allow only the physical own state"
    );
    assert!(
        !text.contains(&alias.display().to_string()),
        "Seatbelt must not name a symlink alias for plugin state"
    );
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

/// The seatbelt rendering of the same boundary: the state tree is denied
/// after the broad read allow, and the plugin's own namespace is re-allowed
/// as a subpath after that deny (SBPL is last-match-wins). Compiled on every
/// host so Linux CI keeps the macOS half honest.
#[test]
fn the_macos_profile_denies_the_state_tree_and_re_allows_the_own_namespace_after_it() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let spec = state_plugin(
        &global_root,
        &temp.path().join("demo"),
        "demo",
        "other",
        &[PluginGrant::Fs],
    );
    let profile = spec.sandbox_profile(None).expect("profile");
    let mut profile_text =
        orbit_exec::compile_macos_sandbox_profile(&profile.macos_fs_rules(), "plugin")
            .expect("compile seatbelt profile");
    orbit_exec::append_macos_read_boundary(
        &mut profile_text,
        &profile.read_denies,
        &profile.readable_denied_trees(),
        &profile.readable_denied_files(),
    );

    let deny = format!(
        "(deny file-read* (subpath \"{}\"))",
        global_root.join("state/plugins").display()
    );
    let reallow = format!(
        "(allow file-read* (subpath \"{}\"))",
        global_root.join("state/plugins/demo").display()
    );
    let deny_at = profile_text
        .rfind(&deny)
        .unwrap_or_else(|| panic!("the state tree is not denied: {profile_text}"));
    let reallow_at = profile_text
        .rfind(&reallow)
        .unwrap_or_else(|| panic!("the own namespace is not re-allowed: {profile_text}"));
    assert!(
        reallow_at > deny_at,
        "SBPL is last-match-wins: the own-namespace re-allow must follow the deny\n{profile_text}"
    );
    assert!(
        !profile_text.contains("state/plugins/other"),
        "nothing re-allows another plugin's state: {profile_text}"
    );
}

/// The Landlock grants themselves, for a backend holding `orbit_tools` — the
/// grant that makes the global root readable. Another plugin's state and the
/// tree holding it get no grant; the plugin's own state is readable whole.
#[cfg(target_os = "linux")]
#[test]
fn the_landlock_ruleset_confines_each_plugin_to_its_own_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    for name in ["demo", "other"] {
        let state = global_root.join("state/plugins").join(name);
        std::fs::create_dir_all(&state).expect("plugin state");
        std::fs::write(state.join("secret"), name).expect("secret");
    }
    std::fs::write(global_root.join("config.toml"), "").expect("host config");
    let plugin_root = temp.path().join("demo");
    std::fs::create_dir_all(&plugin_root).expect("plugin root");
    let spec = state_plugin(
        &global_root,
        &plugin_root,
        "demo",
        "other",
        &[PluginGrant::Fs, PluginGrant::OrbitTools],
    );
    let grants = landlock_grants(&spec);
    let reads = |path: &Path| orbit_exec::grants_read(&grants, path);

    assert!(reads(&global_root.join("state/plugins/demo/secret")));
    assert!(
        grants
            .iter()
            .any(|grant| grant.writes(&global_root.join("state/plugins/demo/secret"))),
        "the own state stays writable under the `fs.write` grant"
    );
    assert!(!reads(&global_root.join("state/plugins/other/secret")));
    assert!(
        !reads(&global_root.join("state/plugins/other")),
        "another plugin's state directory must not be listable"
    );
    assert!(
        !reads(&global_root.join("state/plugins")),
        "the tree holding every plugin's state must not be listable"
    );
    assert!(reads(&global_root.join("config.toml")));
}

/// A `state/plugins/` that does not exist yet at spawn is still carved out:
/// a long-lived backend must not gain a readable ancestor over a second
/// plugin's first state directory, created while it runs.
#[cfg(target_os = "linux")]
#[test]
fn an_absent_state_tree_is_still_carved_out_of_the_global_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    std::fs::create_dir_all(global_root.join("state/logs")).expect("log store");
    std::fs::write(global_root.join("config.toml"), "").expect("host config");
    let plugin_root = temp.path().join("demo");
    std::fs::create_dir_all(&plugin_root).expect("plugin root");
    let mut spec = state_plugin(
        &global_root,
        &plugin_root,
        "demo",
        "other",
        &[PluginGrant::OrbitTools],
    );
    spec.permissions = PluginPermissions::default();
    let grants = landlock_grants(&spec);
    let reads = |path: &Path| orbit_exec::grants_read(&grants, path);

    assert!(
        !global_root.join("state/plugins").exists(),
        "fixture: the state tree is absent at compile time"
    );
    assert!(
        !reads(&global_root.join("state")),
        "the parent of an absent denied tree is not granted, so the tree has no \
         readable ancestor once it is created"
    );
    assert!(!reads(&global_root.join("state/plugins/other/secret")));
    assert!(reads(&global_root.join("state/logs")));
    assert!(reads(&global_root.join("config.toml")));
}

#[cfg(target_os = "linux")]
fn landlock_grants(spec: &PluginBackendSpec) -> Vec<orbit_exec::LandlockPathGrant> {
    use orbit_exec::{EnvironmentMode, ExecRequest, LandlockBoundary, StdinMode};

    let profile = spec.sandbox_profile(None).expect("profile");
    let request = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "true".to_string()],
        current_dir: None,
        timeout_ms: Some(1_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(vec![(
            "PATH".to_string(),
            "/usr/bin:/bin".to_string(),
        )]),
        debug: false,
    };
    let boundary = LandlockBoundary {
        read: profile.read.clone(),
        read_denies: profile.read_denies.clone(),
        write: profile.write.clone(),
        write_files: profile.write_files.clone(),
        deny_tcp: true,
    };
    orbit_exec::linux_landlock_boundary_grants(&request, &boundary).expect("compile grants")
}

/// Two plugins through the real exec path, with and without `orbit_tools`:
/// each reads and writes its own `{{plugin_state}}`, and each is refused the
/// other's state — file read and directory listing — and the tree that holds
/// both, even though its manifest asks to read the other's directory.
#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn each_plugin_backend_reads_its_own_state_and_is_refused_the_others() {
    require_sandbox();
    for grants in [
        &[PluginGrant::Fs][..],
        &[PluginGrant::Fs, PluginGrant::OrbitTools][..],
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        // `sandbox-exec` matches resolved paths, and macOS temp dirs sit
        // behind the `/var` -> `/private/var` alias.
        let base = temp.path().canonicalize().expect("canonical tempdir");
        let global_root = base.join("global");
        for (name, other) in [("demo", "other"), ("other", "demo")] {
            let state = global_root.join("state/plugins").join(name);
            std::fs::create_dir_all(&state).expect("plugin state");
            std::fs::write(state.join("secret"), name).expect("secret");
            let plugin_root = base.join(name);
            std::fs::create_dir_all(&plugin_root).expect("plugin root");
            stub_backend(&plugin_root, STATE_PROBE_BACKEND);
            let spec = state_plugin(&global_root, &plugin_root, name, other, grants);
            let ctx = ToolContext {
                proc_spawn_environment: Some(vec![
                    ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                    (
                        "OTHER_STATE".to_string(),
                        global_root
                            .join("state/plugins")
                            .join(other)
                            .to_string_lossy()
                            .into_owned(),
                    ),
                ]),
                ..context(&base)
            };
            let output = tool(std::sync::Arc::new(spec), None)
                .execute(&ctx, json!({}))
                .expect("backend runs");
            assert_eq!(
                output["result"], "ok",
                "plugin {name} with grants {grants:?}: own state read/write, other state refused"
            );
            assert_eq!(
                std::fs::read_to_string(state.join("marker")).expect("marker written"),
                format!("{name}\n")
            );
        }
    }
}
