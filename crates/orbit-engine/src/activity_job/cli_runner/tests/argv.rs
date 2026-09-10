#![allow(missing_docs)]

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::time::Duration;

#[cfg(target_os = "linux")]
use crate::activity_job::load_activity_asset;
#[cfg(target_os = "linux")]
use chrono::Utc;
#[cfg(target_os = "linux")]
use orbit_common::protocol::yaml::parse_policy_resource;
use orbit_exec::sandbox_exec_program_for_audit;
#[cfg(target_os = "linux")]
use orbit_exec::{
    LinuxBwrapPostRunGuard, LinuxBwrapSpawnRequest, bwrap_program_for_audit,
    compile_linux_bwrap_argv, probe_bwrap, spawn_under_linux_bwrap,
};
#[cfg(target_os = "linux")]
use orbit_types::policy::{FsOperation, PolicyDef, UNRESTRICTED_FS_PROFILE};
#[cfg(target_os = "linux")]
use orbit_types::resource::ResourceKind;
#[cfg(target_os = "linux")]
use orbit_types::workflow::{ActivityV2Spec, ExecutorSandboxKind};

#[cfg(target_os = "linux")]
use super::super::argv::try_audit_argv_for_dispatch;
use super::super::argv::{
    apply_provider_runtime_arg_fixups, audit_argv_for_dispatch, neutralize_inner_sandbox,
    rewrite_debug_file_value,
};
use super::test_support::sandbox_for_test;
#[cfg(target_os = "linux")]
use crate::activity_job::ResolvedSandbox;

#[cfg(target_os = "linux")]
const TASK_PILOT_ACTIVITY: &str =
    include_str!("../../../../../orbit-core/assets/activities/task_pilot.yaml");
#[cfg(target_os = "linux")]
const TRIAGE_FAILED_RUNS_ACTIVITY: &str =
    include_str!("../../../../../orbit-core/assets/activities/triage_failed_runs.yaml");
#[cfg(target_os = "linux")]
const DEFAULT_POLICY: &str = include_str!("../../../../../orbit-core/assets/policies/default.yaml");

#[test]
fn audit_argv_for_dispatch_prepends_sandbox_exec_when_sandbox_active() {
    let argv = audit_argv_for_dispatch(
        "/usr/bin/claude",
        &["-p".to_string(), "hello".to_string()],
        Some(&sandbox_for_test()),
    );
    assert_eq!(
        argv,
        vec![
            sandbox_exec_program_for_audit(),
            "-f",
            "<profile.sb>",
            "/usr/bin/claude",
            "-p",
            "hello"
        ]
    );
}

#[test]
fn audit_argv_for_dispatch_returns_bare_when_no_sandbox() {
    let argv = audit_argv_for_dispatch(
        "/usr/bin/claude",
        &["-p".to_string(), "hello".to_string()],
        None,
    );
    assert_eq!(argv, vec!["/usr/bin/claude", "-p", "hello"]);
}

#[cfg(target_os = "linux")]
#[test]
fn task_pilot_reviewer_profile_starts_direct_linux_invocation_with_env_denies() {
    let activity = load_activity_asset(TASK_PILOT_ACTIVITY).expect("parse task-pilot activity");
    let profile_name = activity
        .spec
        .fs_profile
        .as_deref()
        .expect("task-pilot must select an explicit fsProfile");
    assert_eq!(profile_name, "reviewer");
    let ActivityV2Spec::AgentLoop(agent) = &activity.spec.spec else {
        panic!("task-pilot must remain an agent-loop activity");
    };
    assert!(!agent.tools.iter().any(|tool| {
        matches!(
            tool.as_str(),
            "fs.write" | "fs.patch" | "orbit.task.update" | "orbit.task.*"
        )
    }));

    let resource =
        parse_policy_resource(DEFAULT_POLICY, "default task-pilot policy").expect("parse policy");
    assert_eq!(resource.kind, ResourceKind::Policy);
    assert_eq!(
        resource.spec.deny_read,
        ["**/.env", "**/.env.*", "**/*.env", "**/*.env.*"]
    );
    assert_eq!(
        resource.spec.deny_modify,
        [
            ".orbit/**",
            "!.orbit/auto_tasks/**",
            "!.orbit/routines/**",
            "!.orbit/config.toml",
            "!.orbit/resources/**",
            "**/.env",
            "**/.env.*",
            "**/*.env",
            "**/*.env.*",
        ]
    );
    let now = Utc::now();
    let policy = PolicyDef {
        name: resource.metadata.name,
        description: resource.spec.description,
        deny_read: resource.spec.deny_read,
        deny_modify: resource.spec.deny_modify,
        fs_profiles: resource.spec.fs_profiles,
        created_at: Some(now),
        updated_at: Some(now),
    };
    assert!(
        policy
            .check_path(profile_name, FsOperation::Read, "src/lib.rs")
            .expect("check workspace read")
            .allowed
    );
    assert!(
        !policy
            .check_path(profile_name, FsOperation::Read, ".env")
            .expect("check env read")
            .allowed
    );
    for path in ["src/lib.rs", ".orbit/tasks/ORB-10584/task.yaml", ".env"] {
        assert!(
            !policy
                .check_path(profile_name, FsOperation::Modify, path)
                .unwrap_or_else(|error| panic!("check modify path {path}: {error}"))
                .allowed,
            "task-pilot reviewer profile must not modify {path}"
        );
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".orbit")).expect("create workspace fixture");
    let workspace = workspace.canonicalize().expect("canonical workspace");
    let workspace_root = workspace.display().to_string();
    let mut effective = policy
        .effective_profile(profile_name)
        .expect("resolve reviewer profile");
    effective.read = effective
        .read
        .iter()
        .map(|rule| absolutize_test_rule(&workspace_root, rule))
        .collect();
    effective.modify = effective
        .modify
        .iter()
        .map(|rule| absolutize_test_rule(&workspace_root, rule))
        .collect();
    assert!(
        effective.modify.iter().all(|rule| rule.starts_with('!')),
        "reviewer profile must not contain a positive workspace modify grant: {:?}",
        effective.modify
    );

    let sandbox = ResolvedSandbox {
        kind: ExecutorSandboxKind::LinuxBwrap,
        fs_profile: effective.clone(),
        allow_fallback: false,
        managed_worktree: false,
        runtime_write_authority: Vec::new(),
    };
    let argv = try_audit_argv_for_dispatch("/bin/true", &[], Some(&sandbox), Some(&workspace))
        .expect("direct task-pilot Bubblewrap plan must compile");
    assert_eq!(
        argv.first().map(String::as_str),
        Some(bwrap_program_for_audit())
    );

    let probe = probe_bwrap();
    if !probe.available {
        return;
    }
    let plan = compile_linux_bwrap_argv(&effective, "/bin/true", &[], Some(&workspace), false)
        .expect("compile task-pilot Bubblewrap invocation");
    let mut child = spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
        plan: &plan,
        env: &[],
        cwd: Some(&workspace),
        stdin: Stdio::null(),
        stdout: Stdio::null(),
        stderr: Stdio::null(),
    })
    .expect("start direct task-pilot Bubblewrap invocation");
    assert!(
        child
            .wait()
            .expect("wait for task-pilot invocation")
            .success()
    );
}

/// [ORB-11257] Triage is launched against the primary checkout. It must select
/// `reviewer` so default dotenv glob denials compile for a direct Bubblewrap
/// invocation, while source edits stay denied and write-capable profiles still
/// fail closed.
#[cfg(target_os = "linux")]
#[test]
fn triage_reviewer_profile_starts_direct_linux_invocation_and_protects_env_paths() {
    let activity = load_activity_asset(TRIAGE_FAILED_RUNS_ACTIVITY).expect("parse triage activity");
    let profile_name = activity
        .spec
        .fs_profile
        .as_deref()
        .expect("triage must select an explicit fsProfile");
    assert_eq!(profile_name, "reviewer");
    let ActivityV2Spec::AgentLoop(agent) = &activity.spec.spec else {
        panic!("triage must remain an agent-loop activity");
    };
    assert!(
        agent
            .tools
            .iter()
            .any(|tool| tool == "orbit.task.update" || tool == "orbit.task.show")
    );
    assert!(!agent.tools.iter().any(|tool| {
        matches!(
            tool.as_str(),
            "fs.write" | "fs.patch" | "fs.create" | "orbit.pipeline.invoke"
        )
    }));

    let resource =
        parse_policy_resource(DEFAULT_POLICY, "default triage policy").expect("parse policy");
    assert_eq!(resource.kind, ResourceKind::Policy);
    assert_eq!(
        resource
            .spec
            .deny_modify
            .iter()
            .filter(|rule| rule.contains(".env"))
            .count(),
        4,
        "default dotenv denyModify globs must remain: {:?}",
        resource.spec.deny_modify
    );
    let now = Utc::now();
    let policy = PolicyDef {
        name: resource.metadata.name,
        description: resource.spec.description,
        deny_read: resource.spec.deny_read,
        deny_modify: resource.spec.deny_modify,
        fs_profiles: resource.spec.fs_profiles,
        created_at: Some(now),
        updated_at: Some(now),
    };

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).expect("create workspace fixture");
    std::fs::write(workspace.join(".env"), "secret").expect("write existing protected path");
    std::fs::write(workspace.join("src/lib.rs"), "fn main() {}").expect("write source fixture");
    let workspace = workspace.canonicalize().expect("canonical workspace");
    let existing_env = workspace.join(".env");
    let created_env = workspace.join("new.env");
    let source_file = workspace.join("src/lib.rs");
    let workspace_root = workspace.display().to_string();
    let mut effective = policy
        .effective_profile(profile_name)
        .expect("resolve reviewer profile");
    effective.read = effective
        .read
        .iter()
        .map(|rule| absolutize_test_rule(&workspace_root, rule))
        .collect();
    effective.modify = effective
        .modify
        .iter()
        .map(|rule| absolutize_test_rule(&workspace_root, rule))
        .collect();
    assert!(
        effective.modify.iter().all(|rule| rule.starts_with('!')),
        "reviewer profile must not contain a positive workspace modify grant: {:?}",
        effective.modify
    );

    let sandbox = ResolvedSandbox {
        kind: ExecutorSandboxKind::LinuxBwrap,
        fs_profile: effective.clone(),
        allow_fallback: false,
        managed_worktree: false,
        runtime_write_authority: Vec::new(),
    };
    let argv = try_audit_argv_for_dispatch("/bin/true", &[], Some(&sandbox), Some(&workspace))
        .expect("direct triage Bubblewrap plan must compile");
    assert_eq!(
        argv.first().map(String::as_str),
        Some(bwrap_program_for_audit())
    );

    let mut unrestricted = policy
        .effective_profile(UNRESTRICTED_FS_PROFILE)
        .expect("resolve unrestricted profile");
    unrestricted.read = unrestricted
        .read
        .iter()
        .map(|rule| absolutize_test_rule(&workspace_root, rule))
        .collect();
    unrestricted.modify = unrestricted
        .modify
        .iter()
        .map(|rule| absolutize_test_rule(&workspace_root, rule))
        .collect();
    let error = compile_linux_bwrap_argv(&unrestricted, "/bin/true", &[], Some(&workspace), false)
        .expect_err("direct write-capable sandbox must still fail closed");
    assert!(
        error.to_string().contains("non-subtree denyModify"),
        "unsafe profile guard must remain: {error}"
    );

    let script = format!(
        "! printf overwritten > '{existing}'; ! printf created > '{created}'; ! printf mutated > '{source}'; test -r '{existing}'",
        existing = existing_env.display(),
        created = created_env.display(),
        source = source_file.display()
    );
    let plan = compile_linux_bwrap_argv(
        &effective,
        "/bin/sh",
        &["-c".to_string(), script],
        Some(&workspace),
        false,
    )
    .expect("compile triage Bubblewrap invocation");
    assert!(
        !plan.args.iter().any(|arg| arg == "--bind"),
        "triage reviewer profile must not gain a writable bind: {:?}",
        plan.args
    );

    let guard = LinuxBwrapPostRunGuard::capture(&effective)
        .expect("capture")
        .expect("triage reviewer profile still carries non-subtree dotenv denies");
    assert_eq!(
        std::fs::read_to_string(&existing_env).expect("read existing protected path"),
        "secret"
    );
    std::fs::write(&created_env, "secret").expect("host-side new protected path");
    let error = guard
        .verify()
        .expect_err("new matching protected path must remain detectable");
    assert!(error.to_string().contains("before commit"));
    std::fs::remove_file(&created_env).expect("remove host-side protected path before spawn");

    let probe = probe_bwrap();
    if !probe.available {
        return;
    }

    let mut child = spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
        plan: &plan,
        env: &[],
        cwd: Some(&workspace),
        stdin: Stdio::null(),
        stdout: Stdio::null(),
        stderr: Stdio::null(),
    })
    .expect("start direct triage Bubblewrap invocation");
    assert!(child.wait().expect("wait for triage invocation").success());
    assert_eq!(
        std::fs::read_to_string(&existing_env).expect("read existing protected path"),
        "secret"
    );
    assert!(
        !created_env.exists(),
        "newly created matching protected path must stay absent"
    );
    assert_eq!(
        std::fs::read_to_string(&source_file).expect("read source"),
        "fn main() {}"
    );
}

#[cfg(target_os = "linux")]
fn absolutize_test_rule(workspace_root: &str, rule: &str) -> String {
    let (negated, body) = rule
        .strip_prefix('!')
        .map_or((false, rule), |body| (true, body));
    let body = body.trim_start_matches("./");
    let absolute = format!("{}/{body}", workspace_root.trim_end_matches('/'));
    if negated {
        format!("!{absolute}")
    } else {
        absolute
    }
}

#[test]
fn neutralize_inner_sandbox_pins_codex_to_danger_full_access() {
    let mut config = HashMap::new();
    config.insert("sandbox".to_string(), "workspace-write".to_string());
    let mut args = vec!["exec".to_string(), "--json".to_string()];
    neutralize_inner_sandbox("codex", &mut config, &mut args);
    assert_eq!(
        config.get("sandbox").map(String::as_str),
        Some("danger-full-access"),
        "codex sandbox should be pinned to danger-full-access when outer sandbox is active"
    );
    // Static args are untouched for codex; the sandbox flag flows
    // through provider_config.
    assert_eq!(args, vec!["exec", "--json"]);
}

#[test]
fn neutralize_inner_sandbox_drops_gemini_sandbox_flags() {
    let mut config = HashMap::new();
    let mut args = vec![
        "--approval-mode".to_string(),
        "yolo".to_string(),
        "--sandbox".to_string(),
        "-s".to_string(),
        "-o".to_string(),
        "json".to_string(),
    ];
    neutralize_inner_sandbox("gemini", &mut config, &mut args);
    assert!(
        !args.iter().any(|a| a == "--sandbox" || a == "-s"),
        "gemini sandbox flags should be removed: {args:?}"
    );
    assert!(args.iter().any(|a| a == "--approval-mode"));
    assert!(args.iter().any(|a| a == "json"));
}

#[test]
fn neutralize_inner_sandbox_drops_antigravity_sandbox_flag() {
    let mut config = HashMap::new();
    let mut args = vec![
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--sandbox".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];
    neutralize_inner_sandbox("antigravity", &mut config, &mut args);
    assert!(
        !args.iter().any(|a| a == "--sandbox"),
        "antigravity --sandbox should be removed: {args:?}"
    );
    assert!(args.iter().any(|a| a == "--input-format"));
}

#[test]
fn neutralize_inner_sandbox_drops_grok_sandbox_flag_and_value() {
    let mut config = HashMap::new();
    let mut args = vec![
        "--permission-mode".to_string(),
        "bypassPermissions".to_string(),
        "--sandbox".to_string(),
        "workspace-write".to_string(),
        "--output-format".to_string(),
        "json".to_string(),
        "--sandbox=another-profile".to_string(),
    ];
    neutralize_inner_sandbox("grok", &mut config, &mut args);
    assert_eq!(
        args,
        vec![
            "--permission-mode".to_string(),
            "bypassPermissions".to_string(),
            "--output-format".to_string(),
            "json".to_string(),
        ],
        "grok sandbox flags should be removed with their values"
    );
    assert!(
        config.is_empty(),
        "grok provider_config must remain untouched"
    );
}

#[test]
fn rewrite_debug_file_value_replaces_relative_path() {
    let mut args = vec![
        "-p".to_string(),
        "--debug-file".to_string(),
        ".orbit/state/logs/claude-debug.log".to_string(),
        "--tools".to_string(),
        "Read".to_string(),
    ];
    rewrite_debug_file_value(&mut args, std::path::Path::new("/Users/test/.claude"));
    assert_eq!(
        args,
        vec![
            "-p".to_string(),
            "--debug-file".to_string(),
            "/Users/test/.claude/claude-debug.log".to_string(),
            "--tools".to_string(),
            "Read".to_string(),
        ],
        "claude --debug-file value should be rewritten to <state_dir>/<basename>"
    );
}

#[test]
fn rewrite_debug_file_value_handles_bare_filename() {
    let mut args = vec!["--debug-file".to_string(), "claude-debug.log".to_string()];
    rewrite_debug_file_value(&mut args, std::path::Path::new("/Users/test/.claude"));
    assert_eq!(args[1], "/Users/test/.claude/claude-debug.log");
}

#[test]
fn rewrite_debug_file_value_no_op_without_flag() {
    let mut args = vec!["-p".to_string(), "--tools".to_string(), "Read".to_string()];
    let original = args.clone();
    rewrite_debug_file_value(&mut args, std::path::Path::new("/Users/test/.claude"));
    assert_eq!(
        args, original,
        "args without --debug-file should be untouched"
    );
}

#[test]
fn rewrite_debug_file_value_rewrites_every_occurrence() {
    let mut args = vec![
        "--debug-file".to_string(),
        "first.log".to_string(),
        "--other".to_string(),
        "x".to_string(),
        "--debug-file".to_string(),
        "nested/dir/second.log".to_string(),
    ];
    rewrite_debug_file_value(&mut args, std::path::Path::new("/Users/test/.claude"));
    assert_eq!(args[1], "/Users/test/.claude/first.log");
    assert_eq!(args[5], "/Users/test/.claude/second.log");
}

#[test]
fn rewrite_debug_file_value_falls_back_when_value_has_no_basename() {
    let mut args = vec!["--debug-file".to_string(), "/".to_string()];
    rewrite_debug_file_value(&mut args, std::path::Path::new("/Users/test/.claude"));
    assert_eq!(args[1], "/Users/test/.claude/claude-debug.log");
}

#[test]
fn rewrite_debug_file_value_ignores_dangling_flag() {
    let mut args = vec!["-p".to_string(), "--debug-file".to_string()];
    let original = args.clone();
    rewrite_debug_file_value(&mut args, std::path::Path::new("/Users/test/.claude"));
    assert_eq!(
        args, original,
        "trailing --debug-file with no value must not panic or rewrite"
    );
}

#[test]
fn apply_provider_runtime_arg_fixups_sets_antigravity_print_timeout() {
    let mut args = vec![
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];
    apply_provider_runtime_arg_fixups("antigravity", &mut args, Duration::from_secs(3 * 60 * 60));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--print-timeout", "2h59m30s"]),
        "long budgets must raise --print-timeout above the 5m default: {args:?}"
    );
    assert_eq!(
        args.iter()
            .filter(|arg| arg.as_str() == "--print-timeout" || arg.starts_with("--print-timeout="))
            .count(),
        1
    );
}

#[test]
fn apply_provider_runtime_arg_fixups_keeps_a_shorter_executor_print_timeout() {
    let mut args = vec!["--print-timeout".to_string(), "45s".to_string()];
    apply_provider_runtime_arg_fixups("antigravity", &mut args, Duration::from_secs(3 * 60 * 60));
    assert_eq!(args, vec!["--print-timeout".to_string(), "45s".to_string()]);
}

#[test]
fn apply_provider_runtime_arg_fixups_ignores_other_providers() {
    let mut args = vec!["--json".to_string()];
    apply_provider_runtime_arg_fixups("codex", &mut args, Duration::from_secs(3600));
    assert_eq!(args, vec!["--json".to_string()]);
}

#[test]
fn neutralize_inner_sandbox_leaves_claude_args_unchanged() {
    let mut config = HashMap::new();
    let mut args = vec![
        "-p".to_string(),
        "--permission-mode".to_string(),
        "bypassPermissions".to_string(),
        "--tools".to_string(),
        "Read,Write,Edit,Bash".to_string(),
    ];
    let original = args.clone();
    neutralize_inner_sandbox("claude", &mut config, &mut args);
    assert_eq!(
        args, original,
        "claude args must be unchanged by neutralization"
    );
    assert!(
        config.is_empty(),
        "claude provider_config must remain untouched"
    );
}
