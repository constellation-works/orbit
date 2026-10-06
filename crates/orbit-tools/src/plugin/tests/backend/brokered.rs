//! An unsandboxed backend is refused to a brokered agent and still runs for
//! the operator.

use std::path::PathBuf;

use orbit_types::plugin::{
    PluginFsPermissions, PluginGrant, PluginGrantSet, PluginPermissions, PluginSandbox,
};
use orbit_types::policy::ResolvedFsProfile;
use serde_json::json;

use super::super::super::backend::{BrokeredCaller, PluginBackendSpec};
use super::super::support::{context, spec, stub_backend, tool};
use crate::{Tool, ToolContext};

/// A checkout, a plugin whose grants reach into it, and a synthetic agent
/// profile that excludes part of what the plugin was granted.
struct Fixture {
    _temp: tempfile::TempDir,
    worktree: PathBuf,
    spec: PluginBackendSpec,
}

impl Fixture {
    fn new(read: &[&str], write: &[&str]) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        // Physical, so a macOS `/var` alias cannot split a rule from the path
        // it is decided against.
        let base = temp.path().canonicalize().expect("canonical tempdir");
        let worktree = base.join("checkout");
        for dir in ["allowed", "denied", "gen/out", "private", "sub"] {
            std::fs::create_dir_all(worktree.join(dir)).expect("worktree dir");
        }
        for (file, contents) in [
            ("public.txt", "public"),
            ("secret.txt", "secret"),
            ("sub/.env", "TOKEN=1"),
            ("private/key.txt", "key"),
            ("denied/existing.txt", "existing"),
        ] {
            std::fs::write(worktree.join(file), contents).expect("worktree file");
        }
        let root = base.join("plugin");
        std::fs::create_dir_all(&root).expect("plugin root");
        let permissions = PluginPermissions {
            fs: PluginFsPermissions {
                read: read.iter().map(|root| (*root).into()).collect(),
                write: write.iter().map(|root| (*root).into()).collect(),
            },
            ..PluginPermissions::default()
        };
        let spec = (*spec(root.join("bin"), &root, permissions, &[PluginGrant::Fs])).clone();
        Self {
            _temp: temp,
            worktree,
            spec,
        }
    }

    fn caller(&self, read: &[&str], modify: &[&str]) -> BrokeredCaller {
        BrokeredCaller {
            worktree: self.worktree.clone(),
            fs_profile: ResolvedFsProfile {
                name: "implementer".to_string(),
                read: read.iter().map(|rule| (*rule).to_string()).collect(),
                modify: modify.iter().map(|rule| (*rule).to_string()).collect(),
            },
            proc_allowed_programs: Vec::new(),
            proc_disallowed_programs: None,
        }
    }
}

/// Reads and then overwrites `$SENTINEL`, reporting what it read.
#[cfg(unix)]
const SENTINEL_BACKEND: &str = "#!/bin/sh\ncat >/dev/null\nseen=$(cat \"$SENTINEL\" 2>/dev/null)\necho overwritten > \"$SENTINEL\" 2>/dev/null\nprintf '{\"ok\":true,\"output\":{\"seen\":\"%s\"}}\\n' \"$seen\"\n";

#[cfg(unix)]
#[test]
fn an_unsandboxed_backend_is_refused_to_an_agent_but_still_runs_for_the_operator() {
    let fixture = Fixture::new(&[], &[]);
    let root = fixture.spec.plugin_root.clone();
    let sentinel = fixture.worktree.join("secret.txt");
    let mut spec = fixture.spec.clone();
    spec.command = stub_backend(&root, SENTINEL_BACKEND);
    spec.sandbox = PluginSandbox::None;
    spec.permissions.env_pass = vec!["SENTINEL".into()];
    spec.grants = PluginGrantSet::from_grants([
        PluginGrant::Fs,
        PluginGrant::Unsandboxed,
        PluginGrant::EnvPass,
    ]);
    let backend = tool(std::sync::Arc::new(spec), None);
    let operator = ToolContext {
        proc_spawn_environment: Some(vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "SENTINEL".to_string(),
                sentinel.to_string_lossy().into_owned(),
            ),
        ]),
        ..context(&fixture.worktree)
    };
    let agent = ToolContext {
        brokered_caller: Some(fixture.caller(&["**", "!secret.txt"], &["**", "!secret.txt"])),
        ..operator.clone()
    };

    let error = backend
        .execute(&agent, json!({}))
        .expect_err("a brokered call cannot run an unsandboxed backend");
    assert!(
        matches!(error, orbit_common::OrbitError::PolicyDenied(_)),
        "{error:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&sentinel).expect("sentinel"),
        "secret",
        "the refused backend never ran, so the agent-denied file is untouched"
    );

    let output = backend
        .execute(&operator, json!({}))
        .expect("a non-brokered call keeps the operator's unsandboxed grant");
    assert_eq!(output["seen"], "secret");
    assert_eq!(
        std::fs::read_to_string(&sentinel).expect("sentinel"),
        "overwritten\n"
    );
}
