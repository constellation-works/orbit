//! Provider-harness boundary coverage for the pilot's native read surface.
//! Fake providers exercise emitted capabilities and real read/git/rg in the
//! pinned checkout without starting a model session or modifying Orbit state.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use orbit_engine::activity_job::load_activity_asset;
use orbit_engine::{
    DispatchError, ResolvedCliExecutor, RuntimeHost, V2AuditWriter, V2DispatchInput,
    dispatch_v2_activity,
};
use orbit_store::Store;
use orbit_types::workflow::activity_job::{ActivityV2Spec, Provider, V2AuditEventKind};
use serde_json::{Value, json};

#[test]
fn codex_and_claude_pilots_inspect_the_pinned_checkout_with_native_tools() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--quiet"]);
    fs::write(repo.join("candidate.rs"), "pinned candidate\n").unwrap();
    git(&repo, &["add", "candidate.rs"]);
    git(&repo, &["commit", "--quiet", "-m", "source"]);
    let revision = git(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("candidate.rs"), "later candidate\n").unwrap();

    for (provider, name, args) in [
        (Provider::Codex, "codex", vec!["exec", "--json"]),
        (Provider::Claude, "claude", vec!["-p"]),
        (Provider::Claude, "claude", vec!["-p", "--tools", "default"]),
        (Provider::Claude, "claude", vec!["-p", "--tools=Read,Bash"]),
    ] {
        let cli = temp.path().join(name);
        executable(&cli, NATIVE_PROVIDER);
        let host = InspectionHost {
            command: cli,
            args: args.into_iter().map(str::to_string).collect(),
        };
        let mut asset = pilot(provider);
        let ActivityV2Spec::AgentLoop(spec) = &mut asset else {
            panic!("agent loop")
        };
        // No proc.spawn grant is needed for provider-native inspection.
        spec.tool_disallow_list = None;
        spec.tools = vec!["orbit.task.show".into(), "orbit.search".into()];
        let audit = writer(temp.path());
        let outcome = dispatch_v2_activity(V2DispatchInput {
            activity_name: "pilot",
            spec: &asset,
            fs_profile: Some("reviewer"),
            input: json!({
                "task_ids": ["fixture-task"], "partition_index": 0,
                "workspace_path": repo, "base_branch": "agent-main",
                "inspection_revision": revision, "source_revision": revision,
            }),
            audit,
            run_id: "inspection-fixture",
            host: Some(&host),
        })
        .unwrap();
        assert!(outcome.success, "{name}: {:?}", outcome.message);
        assert_eq!(outcome.output["content"], "pinned candidate\n");
        assert_eq!(outcome.output["head"], revision);
        assert_eq!(outcome.output["matches"], "1:pinned candidate");
        let inspected = PathBuf::from(outcome.output["root"].as_str().unwrap());
        assert_ne!(
            inspected, repo,
            "inspection must use its private pinned checkout"
        );
        assert!(
            !inspected.exists(),
            "the invocation lease cleans its checkout"
        );
        assert_eq!(
            fs::read_to_string(repo.join("candidate.rs")).unwrap(),
            "later candidate\n"
        );
    }
}

#[test]
fn unavailable_inspection_surface_is_a_typed_non_retryable_dispatch_error() {
    let temp = tempfile::tempdir().unwrap();
    let calls = temp.path().join("calls");
    let cases = [
        (Provider::Gemini, "gemini", vec![], Some("reviewer")),
        (
            Provider::Claude,
            "claude",
            vec!["--tools", ""],
            Some("reviewer"),
        ),
        (
            Provider::Claude,
            "claude",
            vec!["--tools=Read"],
            Some("reviewer"),
        ),
        (
            Provider::Claude,
            "claude",
            vec!["--tools", "default", "--tools", ""],
            Some("reviewer"),
        ),
        (
            Provider::Claude,
            "claude",
            vec!["--disallowedTools", "Bash"],
            Some("reviewer"),
        ),
        (
            Provider::Claude,
            "claude",
            vec!["--disallowed-tools=Read"],
            Some("reviewer"),
        ),
        (Provider::Codex, "codex", vec![], Some("implementer")),
    ];
    for (provider, name, args, profile) in cases {
        let cli = temp.path().join(name);
        executable(&cli, &format!("#!/bin/sh\ntouch '{}'\n", calls.display()));
        let host = InspectionHost {
            command: cli,
            args: args.iter().map(|arg| (*arg).into()).collect(),
        };
        let audit = writer(temp.path());
        let error = dispatch_v2_activity(V2DispatchInput {
            activity_name: "pilot",
            spec: &pilot(provider),
            fs_profile: profile,
            // Even non-Git pilots need a read surface.
            input: json!({"inspection_revision": null, "partition_index": 0}),
            audit: audit.clone(),
            run_id: "inspection-fixture",
            host: Some(&host),
        })
        .unwrap_err();
        assert!(
            matches!(&error, DispatchError::InspectionToolsUnavailable { provider, reason } if provider == name && !reason.is_empty()),
            "{error:?}"
        );
        assert!(
            error.is_non_retryable(),
            "configuration failures must not spend retries"
        );
        assert!(
            !error.allows_recovery(),
            "configuration needs correction before dispatch"
        );
        assert!(
            !calls.exists(),
            "a provider with no inspection surface must never start"
        );
        assert!(
            audit
                .events_snapshot()
                .unwrap()
                .iter()
                .all(|event| !matches!(event.kind, V2AuditEventKind::CliInvocationStarted { .. }))
        );
    }
}

fn pilot(provider: Provider) -> ActivityV2Spec {
    let asset = load_activity_asset(include_str!(
        "../../../orbit-core/assets/activities/task_pilot.yaml"
    ))
    .unwrap();
    let ActivityV2Spec::AgentLoop(mut spec) = asset.spec.spec else {
        panic!("agent loop")
    };
    spec.provider = provider;
    spec.model = None;
    ActivityV2Spec::AgentLoop(spec)
}

fn writer(root: &Path) -> Arc<V2AuditWriter> {
    V2AuditWriter::with_disk_sinks(
        &root.join("audit"),
        Arc::new(Store::open_in_memory().unwrap()),
        "fixture",
        "inspection-fixture",
        "pilot",
        None,
    )
    .unwrap()
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(root: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .current_dir(root)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.test",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

struct InspectionHost {
    command: PathBuf,
    args: Vec<String>,
}

impl RuntimeHost for InspectionHost {
    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.command.display().to_string(),
            args: self.args.clone(),
        })
    }
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        _input: &Value,
        _ctx: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        Err(DispatchError::DeterministicActionNotRegistered(
            action.into(),
        ))
    }
    fn tool_context_for_activity(
        &self,
        _run: Option<&str>,
        _profile: Option<&str>,
        _audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        _programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_tools::ToolContext::default()
    }
}

const NATIVE_PROVIDER: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
args = sys.argv[1:]
provider = pathlib.Path(sys.argv[0]).name
if provider == 'codex':
    config = dict(arg.split('=', 1) for i, arg in enumerate(args) if i and args[i-1] in ('--config', '-c'))
    assert config['features.shell_tool'] == 'true'
    assert config['features.unified_exec'] == 'true'
else:
    selected = []
    for i, arg in enumerate(args):
        if arg == '--tools': selected = args[i+1].split(',')
        elif arg.startswith('--tools='): selected = arg.split('=', 1)[1].split(',')
    assert 'default' in selected or {'Read', 'Bash'} <= set(selected)
envelope = json.loads(sys.stdin.read().split('Execution envelope:\n', 1)[1])
assert 'proc.spawn' not in envelope['tools']
root = pathlib.Path(envelope['input']['workspace_path']).resolve()
assert root == pathlib.Path.cwd().resolve()
assert envelope['input']['repo_root'] == str(root)
content = pathlib.Path('candidate.rs').read_text()
head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
matches = subprocess.check_output(['rg', '-n', 'pinned', 'candidate.rs'], text=True).strip()
assert head == envelope['input']['source_revision']
print(json.dumps({'schemaVersion': 1, 'status': 'success', 'result': {'content': content, 'head': head, 'matches': matches, 'root': str(root)}, 'error': None}))
"#;
