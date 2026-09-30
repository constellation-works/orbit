use serde_json::json;

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use orbit_exec::StdinMode;
use orbit_policy::PolicyEngine;
use orbit_types::policy::{FsProfile, PolicyDef};

use super::{MAX_TIMEOUT_MS, enforce_program_allowlist, proc_spawn_timeout_ms, spawn_request};
use crate::{TIMEOUT_DEFAULT_MS, ToolContext};
use orbit_common::OrbitError;

// Used only by the child-execution test below, which runs a real Unix `cat`.
#[cfg(unix)]
use super::ProcSpawnTool;
#[cfg(unix)]
use crate::Tool;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[test]
fn missing_proc_spawn_timeout_uses_default_timeout() {
    assert_eq!(proc_spawn_timeout_ms(&json!({})), TIMEOUT_DEFAULT_MS);
}

#[test]
fn explicit_proc_spawn_timeout_is_preserved() {
    assert_eq!(proc_spawn_timeout_ms(&json!({ "timeout_ms": 42 })), 42);
}

#[test]
fn proc_spawn_timeout_at_the_maximum_is_preserved() {
    assert_eq!(
        proc_spawn_timeout_ms(&json!({ "timeout_ms": MAX_TIMEOUT_MS })),
        MAX_TIMEOUT_MS
    );
}

#[test]
fn proc_spawn_timeout_above_the_maximum_is_clamped_and_logged() {
    let mut resolved = 0;
    let logs = captured_warnings(|| {
        resolved = proc_spawn_timeout_ms(&json!({ "timeout_ms": u64::MAX }));
    });

    assert_eq!(resolved, MAX_TIMEOUT_MS);
    assert!(
        logs.contains("orbit.tool.proc_spawn")
            && logs.contains(&format!("requested_timeout_ms={}", u64::MAX))
            && logs.contains(&format!("timeout_ms={MAX_TIMEOUT_MS}")),
        "clamping an oversized timeout must be visible in the logs, got: {logs}"
    );
}

#[test]
fn spawned_children_run_with_stdin_closed() {
    let request = spawn_request(&ToolContext::default(), "cat".to_string(), vec![], 1_000);
    assert_eq!(request.stdin_mode, StdinMode::Null);
}

/// `cat` reads until stdin reaches EOF. With an inherited stdin it would hold
/// the operator's terminal until the deadline; with stdin closed it sees EOF
/// at once and exits with no output.
#[cfg(unix)]
#[test]
fn stdin_reading_program_returns_without_waiting_for_the_deadline() {
    let requested_timeout_ms = 10_000;
    let started = Instant::now();
    let result = ProcSpawnTool
        .execute(
            &ToolContext::default(),
            json!({ "program": "cat", "timeout_ms": requested_timeout_ms }),
        )
        .expect("proc.spawn cat");
    let elapsed = started.elapsed();

    assert_eq!(result["success"], json!(true));
    assert_eq!(result["stdout"], json!(""));
    assert!(
        elapsed < Duration::from_millis(requested_timeout_ms / 2),
        "`cat` took {elapsed:?}, which suggests it waited on an open stdin"
    );
}

/// A managed CLI worker already has an OS sandbox. Its `proc.spawn` children
/// must inherit that worker's read view, including benign files outside the
/// linked worktree that build tools discover while running.
#[cfg(unix)]
#[test]
fn scoped_proc_spawn_inherits_parent_read_view_for_direct_and_indirect_reads() {
    let host = tempfile::tempdir().expect("benign host fixture");
    let workspace = host.path().join("worktrees/task");
    std::fs::create_dir_all(&workspace).expect("nested worktree");
    let outside_file = host.path().join("config.toml");
    std::fs::write(&outside_file, b"parent-config\n").expect("host fixture");
    let cargo_config = host.path().join(".cargo/config.toml");
    std::fs::create_dir_all(cargo_config.parent().expect("config directory"))
        .expect("parent Cargo config directory");
    std::fs::write(
        &cargo_config,
        b"[alias]\norbit-parent-config-probe = 'version'\n",
    )
    .expect("parent Cargo config");

    let policy = PolicyEngine::from_def(&PolicyDef {
        name: "test".to_string(),
        description: None,
        deny_read: vec![],
        deny_modify: vec![],
        fs_profiles: HashMap::from([(
            "implementer".to_string(),
            FsProfile {
                read: vec!["./**".to_string()],
                modify: vec!["./**".to_string()],
            },
        )]),
        created_at: None,
        updated_at: None,
    })
    .expect("valid policy");
    let ctx = ToolContext {
        workspace_root: Some(workspace),
        policy_engine: Some(Arc::new(policy)),
        fs_profile: Some("implementer".to_string()),
        proc_spawn_activity_scoped: true,
        proc_disallowed_programs: Some(vec![]),
        proc_spawn_environment: Some(vec![
            (
                "PATH".to_string(),
                std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
            ),
            (
                "HOST_READ_TARGET".to_string(),
                outside_file.display().to_string(),
            ),
        ]),
        ..Default::default()
    };

    for input in [
        json!({ "program": "/bin/cat", "args": [outside_file] }),
        json!({ "program": "/bin/sh", "args": ["-c", "cat \"$HOST_READ_TARGET\""] }),
    ] {
        let result = ProcSpawnTool
            .execute(&ctx, input)
            .expect("scoped child runs");
        assert_eq!(result["success"], json!(true), "{result}");
        assert_eq!(result["stdout"], json!("parent-config\n"));
    }

    // Cargo discovers aliases through ancestor .cargo/config.toml files.
    // The original nested-worktree failure denied this parent read before
    // Cargo could resolve the command.
    let result = ProcSpawnTool
        .execute(
            &ctx,
            json!({ "program": "cargo", "args": ["orbit-parent-config-probe"] }),
        )
        .expect("nested-worktree Cargo alias runs");
    assert_eq!(result["success"], json!(true), "{result}");
    assert!(
        result["stdout"]
            .as_str()
            .unwrap_or_default()
            .starts_with("cargo "),
        "Cargo did not load its parent config: {result}"
    );
}

#[test]
fn disallow_mode_denies_only_listed_programs_and_legacy_allowlist_still_denies_all() {
    let deny = ToolContext {
        proc_spawn_activity_scoped: true,
        proc_disallowed_programs: Some(vec!["sudo".to_string(), "ssh".to_string()]),
        ..Default::default()
    };
    for program in ["sudo", "/usr/bin/ssh"] {
        let error = enforce_program_allowlist(&deny, "proc.spawn", program)
            .expect_err("listed program denied");
        assert!(matches!(error, OrbitError::PolicyDenied(message)
            if message.contains("activity disallow list") && message.contains("sudo, ssh")));
    }
    enforce_program_allowlist(&deny, "proc.spawn", "uv").expect("granted uv is not listed");
    enforce_program_allowlist(&deny, "plugin tool", "cargo").expect("ordinary tool admitted");

    let legacy = ToolContext {
        proc_spawn_activity_scoped: true,
        ..Default::default()
    };
    assert!(enforce_program_allowlist(&legacy, "proc.spawn", "uv").is_err());
}

#[cfg(unix)]
#[test]
fn disallow_mode_checks_canonical_symlink_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("sudo");
    std::fs::write(&target, b"fixture").expect("target");
    let link = dir.path().join("alias");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let ctx = ToolContext {
        proc_disallowed_programs: Some(vec!["sudo".to_string()]),
        ..Default::default()
    };
    let error = enforce_program_allowlist(&ctx, "proc.spawn", link.to_str().expect("utf8 path"))
        .expect_err("alias to listed executable must be denied");
    assert!(matches!(error, OrbitError::PolicyDenied(_)));
}

/// Run `action` with a warning-level subscriber installed for this thread and
/// return everything it recorded.
fn captured_warnings(action: impl FnOnce()) -> String {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&buffer);
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || SharedBuffer(Arc::clone(&sink)))
        .finish();

    tracing::subscriber::with_default(subscriber, action);

    let recorded = buffer.lock().expect("captured log buffer").clone();
    String::from_utf8(recorded).expect("log output is utf-8")
}

struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("captured log buffer")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
