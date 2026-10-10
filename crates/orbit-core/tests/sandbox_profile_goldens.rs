//! Goldens of the resolved Linux sandbox for representative activity policies.
//!
//! Each case resolves an executor's sandbox through the runtime's public
//! `RuntimeHost::resolve_executor_sandbox` against the default policy, then
//! compiles the Bubblewrap plan through `orbit-exec` as the spawn path does:
//! write anchors prepared for a managed worktree, runtime grants passed as
//! descriptor authority, the agent mask (plugin trees and `clock.env`) mounted
//! last. The golden pins both
//! layers: the resolved `read`/`modify` rules and the argv Bubblewrap gets.
//!
//! The cases: a leaf worker in its managed worktree, a reviewer from an
//! inspection checkout (no source or workspace writes), and a leaf worker
//! whose host cache store is a symlink out of the global root (the redirected
//! store must not reach the plan).
//!
//! Resolution is platform-gated, so these goldens are Linux-only. The compiled
//! SBPL for the same policy shapes is pinned beside the `orbit-exec`
//! compiler (`crates/orbit-exec/tests/sandbox_profile_goldens/macos/`).
//!
//! The fixture runs in a child of this binary with a cleared environment and
//! a fixture `HOME`, so host provider overrides and inherited Orbit authority
//! cannot reach it. Fixture paths render as `<ROOT>`, descriptors as `<fd>`.
//!
//! Regenerate with `make goldens UPDATE=1` (or
//! `ORBIT_UPDATE_SANDBOX_GOLDENS=1 cargo test -p orbit-core --test
//! sandbox_profile_goldens`) and explain every diff in the PR.

#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::print_stderr, clippy::unwrap_used)]
#![cfg(target_os = "linux")]

orbit_common::isolate_test_process!();

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use orbit_core::OrbitRuntime;
use orbit_engine::{ResolvedSandbox, RuntimeHost};
use orbit_exec::{
    LinuxBwrapMask, LinuxBwrapMountAuthority, compile_linux_bwrap_argv_with_authority,
    prepare_linux_bwrap_write_grants,
};
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, ExecutorType};

const UPDATE_ENV: &str = "ORBIT_UPDATE_SANDBOX_GOLDENS";
/// Set on the child only: where it writes the rendered cases.
const OUTPUT_ENV: &str = "ORBIT_TEST_SANDBOX_GOLDEN_OUTPUT";
/// Set on the child only: the fixture root, which also holds its `HOME`.
const ROOT_ENV: &str = "ORBIT_TEST_SANDBOX_GOLDEN_ROOT";
const CHILD_TEST: &str = "render_resolved_sandboxes_in_a_pinned_environment";
const CHILD_DEADLINE: Duration = orbit_common::test_env::CHILD_TEST_DEADLINE;
const CASES: &[&str] = &["leaf_worker", "reviewer", "redirected_global_runtime_store"];

#[test]
fn resolved_linux_sandboxes_match_their_goldens() {
    // Under `/tmp` so the scratch-anchor mounts render the same on every host
    // whatever its `TMPDIR`.
    let root = tempfile::tempdir_in("/tmp").expect("fixture root");
    let output = tempfile::tempdir().expect("render output");
    run_child(root.path(), output.path());

    let mut drifted = Vec::new();
    for case in CASES {
        let name = format!("{case}.txt");
        let actual = std::fs::read_to_string(output.path().join(&name))
            .unwrap_or_else(|error| panic!("the child did not render {name}: {error}"));
        if !golden_matches(&name, &actual) {
            drifted.push(name);
        }
    }
    assert!(
        drifted.is_empty(),
        "{drifted:?} drifted from the checked-in sandbox goldens (diffs above). If the policy \
         change is intended, regenerate with `make goldens UPDATE=1` and explain the diff in \
         the PR."
    );
    for entry in std::fs::read_dir(golden_dir()).expect("golden dir") {
        let name = entry
            .expect("golden")
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert!(
            CASES.iter().any(|case| name == format!("{case}.txt")),
            "{name} has no case any more; delete it"
        );
    }
}

/// Re-executed by [`resolved_linux_sandboxes_match_their_goldens`] with a
/// cleared environment. Does nothing when run any other way.
#[test]
#[ignore = "child of resolved_linux_sandboxes_match_their_goldens"]
fn render_resolved_sandboxes_in_a_pinned_environment() {
    let (Some(output), Some(root)) = (std::env::var_os(OUTPUT_ENV), std::env::var_os(ROOT_ENV))
    else {
        return;
    };
    let output = PathBuf::from(output);
    let fixture = Fixture::create(Path::new(&root));
    for case in CASES {
        let text = fixture.render(case);
        std::fs::write(output.join(format!("{case}.txt")), text).expect("write case");
    }
}

fn run_child(root: &Path, output: &Path) {
    let root = root.canonicalize().expect("canonical fixture root");
    let logs = tempfile::tempdir().expect("child logs");
    let stdout_path = logs.path().join("stdout.log");
    let stderr_path = logs.path().join("stderr.log");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", root.join("home"))
        .env(OUTPUT_ENV, output)
        .env(ROOT_ENV, &root)
        .args([
            "--exact",
            CHILD_TEST,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(&root)
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&stdout_path).expect("stdout log"))
        .stderr(std::fs::File::create(&stderr_path).expect("stderr log"));
    let mut child = ChildGuard(command.spawn().expect("spawn renderer"));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("poll renderer") {
            break Some(status);
        }
        if started.elapsed() > CHILD_DEADLINE {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(child);
    let read = |path: &Path| {
        let mut text = String::new();
        std::fs::File::open(path)
            .and_then(|mut file| file.read_to_string(&mut text))
            .expect("read child log");
        text
    };
    let (stdout, stderr) = (read(&stdout_path), read(&stderr_path));
    let status = status
        .unwrap_or_else(|| panic!("renderer ran past {CHILD_DEADLINE:?}:\n{stdout}\n{stderr}"));
    orbit_common::test_env::assert_child_test_passed(CHILD_TEST, status, stdout, stderr);
}

/// Kills and reaps the renderer however the parent leaves.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sandbox_profile_goldens/linux")
}

/// Compare with (or, under [`UPDATE_ENV`], overwrite) one golden. Prints a
/// line diff on drift so every mismatching case shows in one run.
fn golden_matches(name: &str, actual: &str) -> bool {
    let path = golden_dir().join(name);
    if std::env::var(UPDATE_ENV).as_deref() == Ok("1") {
        std::fs::create_dir_all(golden_dir()).expect("golden dir");
        std::fs::write(&path, actual)
            .unwrap_or_else(|error| panic!("write golden {}: {error}", path.display()));
        return true;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    if expected == actual {
        return true;
    }
    eprintln!("--- {name} (golden)\n+++ {name} (resolved)");
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    for line in &expected_lines {
        if !actual_lines.contains(line) {
            eprintln!("-{line}");
        }
    }
    for line in &actual_lines {
        if !expected_lines.contains(line) {
            eprintln!("+{line}");
        }
    }
    false
}

struct Fixture {
    root: PathBuf,
    runtime: OrbitRuntime,
    worktree: PathBuf,
    inspection: PathBuf,
}

impl Fixture {
    fn create(root: &Path) -> Self {
        let root = root.canonicalize().expect("canonical root");
        let global = root.join("home/.orbit");
        let repo = root.join("repo");
        let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-golden");
        let inspection = root.join("inspection");
        for dir in [&global, &worktree.join("src"), &inspection.join("src")] {
            std::fs::create_dir_all(dir).expect("fixture dir");
        }
        // Present, so the Linux mask binds `/dev/null` over it.
        std::fs::write(global.join("clock.env"), "").expect("clock credentials");
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).expect("runtime");
        for provider in ["claude", "codex"] {
            seed_linux_executor(&runtime, provider);
        }
        Self {
            root,
            runtime,
            worktree,
            inspection,
        }
    }

    fn render(&self, case: &str) -> String {
        let (provider, fs_profile, cwd) = match case {
            "leaf_worker" => ("codex", Some("implementer"), &self.worktree),
            "reviewer" => ("claude", Some("reviewer"), &self.inspection),
            "redirected_global_runtime_store" => {
                let outside = self.root.join("outside");
                std::fs::create_dir_all(&outside).expect("redirect target");
                let cache = self.runtime.paths().global_dir.join("cache");
                let _ = std::fs::remove_dir(&cache);
                std::os::unix::fs::symlink(&outside, &cache).expect("redirect the host cache");
                ("codex", Some("implementer"), &self.worktree)
            }
            other => panic!("unknown case {other}"),
        };
        let sandbox = self
            .runtime
            .resolve_executor_sandbox(provider, fs_profile, Some(cwd))
            .expect("resolve sandbox")
            .expect("a Linux executor resolves a sandbox");
        let mut lines = vec![
            format!("provider: {provider}"),
            format!("fs_profile: {}", sandbox.fs_profile.name),
            format!("cwd: {}", cwd.display()),
            format!("managed_worktree: {}", sandbox.managed_worktree),
            "read:".to_string(),
        ];
        lines.extend(
            sandbox
                .fs_profile
                .read
                .iter()
                .map(|rule| format!("  {rule}")),
        );
        lines.push("modify:".to_string());
        lines.extend(
            sandbox
                .fs_profile
                .modify
                .iter()
                .map(|rule| format!("  {rule}")),
        );
        lines.push("runtime write authority:".to_string());
        lines.extend(
            sandbox
                .runtime_write_authority
                .iter()
                .map(|grant| format!("  {}", grant.path.display())),
        );
        lines.push("bwrap:".to_string());
        lines.extend(bwrap_lines(&sandbox, cwd));
        normalize(&self.root, &(lines.join("\n") + "\n"))
    }
}

fn seed_linux_executor(runtime: &OrbitRuntime, provider: &str) {
    runtime
        .upsert_executor_def(&ExecutorDef {
            name: provider.to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some(provider.to_string()),
            args: Vec::new(),
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            auth_probe: None,
            env: Default::default(),
            sandbox: Some(ExecutorSandboxKind::LinuxBwrap),
            allow_fallback: false,
            created_at: None,
            updated_at: None,
        })
        .expect("seed executor");
}

/// The plan the spawn path compiles for `sandbox`, one option per line.
fn bwrap_lines(sandbox: &ResolvedSandbox, cwd: &Path) -> Vec<String> {
    if sandbox.managed_worktree {
        prepare_linux_bwrap_write_grants(&sandbox.fs_profile, cwd).expect("prepare write anchors");
    }
    let authority = sandbox
        .runtime_write_authority
        .iter()
        .map(|grant| LinuxBwrapMountAuthority {
            destination: grant.path.clone(),
            source: grant.handle.clone(),
        })
        .collect();
    let mask = sandbox.mask.as_ref().map(|mask| LinuxBwrapMask {
        sentinel: mask.sentinel.clone(),
        targets: mask.targets.clone(),
        files: mask.files.clone(),
    });
    let plan = compile_linux_bwrap_argv_with_authority(
        &sandbox.fs_profile,
        "/usr/bin/true",
        &[],
        Some(cwd),
        sandbox.managed_worktree,
        authority,
        mask.as_ref(),
    )
    .expect("compile Bubblewrap plan");
    let mut lines: Vec<String> = Vec::new();
    let mut after_separator = false;
    let mut previous = "";
    for arg in &plan.args {
        let starts_option = !after_separator && arg.starts_with("--");
        if arg == "--" {
            after_separator = true;
        }
        // A descriptor number depends on what else the process has open.
        let rendered = if previous == "--bind-fd" {
            "<fd>"
        } else {
            arg.as_str()
        };
        match lines.last_mut() {
            Some(line) if !starts_option && !after_separator => {
                line.push(' ');
                line.push_str(rendered);
            }
            _ => lines.push(format!("  {rendered}")),
        }
        previous = arg;
    }
    for dropped in &plan.dropped_grants {
        lines.push(format!(
            "  dropped grant: {} (anchor {})",
            dropped.rule,
            dropped.anchor.display()
        ));
    }
    lines
}

fn normalize(root: &Path, text: &str) -> String {
    text.replace(&root.display().to_string(), "<ROOT>")
}
