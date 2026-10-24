//! The agent sandbox hides clock credentials under the configured global root
//! and Linux SSH masters under `~/.orbit`, even when those roots differ.
//!
//! The clock tick hands a workspace only the `clock.env` names its
//! `execution.env.pass` admits, so the file itself must stay out of every
//! sandboxed agent's reach. The compiled plans are pinned by the sandbox
//! goldens; this checks the resolution through the public runtime API.

use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, ExecutorType};

#[test]
fn agent_mask_hides_clock_env_under_a_relocated_global_root() {
    const TEST: &str =
        "agent_sandbox_mask::agent_mask_hides_clock_env_under_a_relocated_global_root";
    const CHILD: &str = "ORBIT_AGENT_MASK_FIXTURE";
    #[cfg(target_os = "linux")]
    if let Some(socket) = std::env::var_os("ORBIT_AGENT_MASK_SOCKET") {
        let home = std::env::var_os("ORBIT_AGENT_MASK_HOME").expect("fixture home");
        std::env::set_current_dir(home).expect("confined fixture cwd");
        let error = std::os::unix::net::UnixStream::connect(socket)
            .expect_err("the agent must not connect to a host SSH master");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        let sibling = std::env::var_os("ORBIT_AGENT_MASK_SIBLING").expect("sibling path");
        assert_eq!(
            std::fs::read(sibling).expect("unmasked sibling"),
            b"visible"
        );
        return;
    }
    if std::env::var_os(CHILD).is_none() {
        // Keep the fixture visible through the read-only root mount rather
        // than putting it under Bubblewrap's private /tmp.
        let home = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("isolated home");
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("HOME", home.path());
        let output =
            orbit_common::process::run_bounded(&mut command, std::time::Duration::from_secs(120))
                .expect("isolated fixture");
        orbit_common::test_env::assert_child_test_passed(
            TEST,
            output.status,
            &output.stdout,
            &output.stderr,
        );
        return;
    }
    let home = orbit_common::fs::path::home_dir().expect("home");
    std::env::set_current_dir(&home).expect("fixture cwd");
    let root = tempfile::tempdir_in(&home).expect("tempdir");
    let global = root.path().join("relocated/orbit-global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::create_dir_all(&workspace).expect("workspace root");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("runtime");
    // Resolve the same host-managed invocation shape as the agent launcher,
    // keeping every policy deny and the corresponding post-run guard.
    let worktree = workspace.join("state/worktrees/orbit-jrun-agent-mask");
    std::fs::create_dir_all(&worktree).expect("managed fixture checkout");
    #[cfg(target_os = "linux")]
    let probe_binary = {
        // A target directory may be under /tmp, which Bubblewrap makes
        // private. Keep the probe executable in the visible fixture root.
        let probe_binary = root.path().join("agent-mask-probe");
        std::fs::copy(std::env::current_exe().expect("test binary"), &probe_binary)
            .expect("copy confined probe");
        probe_binary
    };
    let clock_env = global
        .canonicalize()
        .expect("canonical global root")
        .join(orbit_common::security::operator_env::CLOCK_ENV_FILE_NAME);
    let sandbox = if cfg!(target_os = "linux") {
        ExecutorSandboxKind::LinuxBwrap
    } else {
        ExecutorSandboxKind::MacosSandboxExec
    };

    for provider in ["claude", "codex"] {
        seed_executor(&runtime, provider, sandbox);
        for fs_profile in [None, Some("reviewer")] {
            let resolved = runtime
                .resolve_executor_sandbox(provider, fs_profile, Some(&worktree))
                .unwrap_or_else(|error| panic!("{provider} sandbox must resolve: {error}"))
                .unwrap_or_else(|| panic!("{provider} must resolve a sandbox"));
            let mask = resolved
                .mask
                .as_ref()
                .unwrap_or_else(|| panic!("{provider} sandbox must carry the agent mask"));
            assert_eq!(
                mask.files,
                vec![clock_env.clone()],
                "{provider} ({fs_profile:?}) must mask clock.env under the configured global root"
            );

            #[cfg(target_os = "linux")]
            {
                use orbit_exec::{
                    LinuxBwrapMask, LinuxBwrapMountAuthority, LinuxBwrapSpawnRequest,
                    prepare_linux_bwrap_write_grants,
                };
                use std::os::unix::net::{UnixListener, UnixStream};
                use std::process::Stdio;

                let sockets = orbit_common::fs::path::ssh_socket_dir().expect("socket directory");
                let sockets = sockets
                    .canonicalize()
                    .expect("host pre-creates socket directory");
                assert!(
                    mask.targets.contains(&sockets),
                    "the actual SSH socket directory must be hidden"
                );
                assert_ne!(
                    sockets.parent(),
                    Some(global.as_path()),
                    "fixture must relocate the runtime root"
                );
                // A relative pathname also exercises long-home SSH fallback.
                let socket = std::path::PathBuf::from(".orbit/ssh/master");
                let listener = UnixListener::bind(&socket).expect("host listener");
                UnixStream::connect(&socket).expect("host can connect to the live socket");
                let sibling = sockets.parent().expect("socket parent").join("visible.txt");
                std::fs::write(&sibling, b"visible").expect("unmasked sibling");
                let probe = orbit_exec::probe_bwrap();
                if probe.available {
                    let linux_mask = LinuxBwrapMask {
                        sentinel: mask.sentinel.clone(),
                        targets: mask.targets.clone(),
                        files: mask.files.clone(),
                    };
                    assert!(resolved.managed_worktree, "fixture must resolve as managed");
                    let mut profile = resolved.fs_profile.clone();
                    // Even an explicit grant cannot reopen the master.
                    profile.modify.push(format!("{}/**", sockets.display()));
                    prepare_linux_bwrap_write_grants(&profile, &worktree)
                        .expect("prepare managed write anchors");
                    let authority = resolved
                        .runtime_write_authority
                        .iter()
                        .map(|grant| LinuxBwrapMountAuthority {
                            destination: grant.path.clone(),
                            source: grant.handle.clone(),
                        })
                        .collect();
                    let mut plan = orbit_exec::compile_linux_bwrap_argv_with_authority(
                        &profile,
                        probe_binary.to_str().expect("UTF-8 probe"),
                        &[
                            "--exact".to_string(),
                            TEST.to_string(),
                            "--nocapture".to_string(),
                        ],
                        Some(&worktree),
                        resolved.managed_worktree,
                        authority,
                        Some(&linux_mask),
                    )
                    .expect("compile agent plan");
                    let guard = plan.take_post_run_guard();
                    let output = orbit_exec::spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
                        plan: &plan,
                        env: &[
                            (
                                "ORBIT_AGENT_MASK_HOME".to_string(),
                                home.display().to_string(),
                            ),
                            (
                                "ORBIT_AGENT_MASK_SOCKET".to_string(),
                                socket.display().to_string(),
                            ),
                            (
                                "ORBIT_AGENT_MASK_SIBLING".to_string(),
                                sibling.display().to_string(),
                            ),
                        ],
                        cwd: Some(&worktree),
                        stdin: Stdio::null(),
                        stdout: Stdio::piped(),
                        stderr: Stdio::piped(),
                    })
                    .expect("spawn confined socket client")
                    .wait_with_output()
                    .expect("wait");
                    if let Some(guard) = guard {
                        guard
                            .verify(None)
                            .expect("managed policy denies stay intact");
                    }
                    orbit_common::test_env::assert_child_test_passed(
                        TEST,
                        output.status,
                        &output.stdout,
                        &output.stderr,
                    );
                } else {
                    orbit_exec::report_bwrap_deferral(TEST, &probe.detail);
                }
                drop(listener);
                std::fs::remove_file(socket).expect("remove fixture socket before next provider");
            }

            #[cfg(target_os = "macos")]
            {
                let mut sbpl =
                    orbit_exec::compile_macos_sandbox_profile(&resolved.fs_profile, provider)
                        .expect("compile profile");
                orbit_exec::append_macos_file_mask(&mut sbpl, &mask.files);
                let physical = orbit_exec::physical_with_missing_tail(&clock_env);
                let deny = format!(
                    "(deny file-read* file-write* (literal \"{}\"))",
                    physical.display()
                );
                assert_eq!(
                    sbpl.lines().last(),
                    Some(deny.as_str()),
                    "{provider} profile must end with the clock.env deny"
                );
            }
        }
    }
}

fn seed_executor(runtime: &OrbitRuntime, provider: &str, sandbox: ExecutorSandboxKind) {
    runtime
        .upsert_executor_def(&ExecutorDef {
            name: provider.to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some(provider.to_string()),
            args: Vec::new(),
            inspection_args: None,
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            auth_probe: None,
            env: Default::default(),
            sandbox: Some(sandbox),
            allow_fallback: false,
            host_os: None,
            created_at: None,
            updated_at: None,
        })
        .expect("seed executor");
}
