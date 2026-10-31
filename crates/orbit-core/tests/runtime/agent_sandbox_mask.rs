//! The agent sandbox hides clock credentials and the decision-channel key
//! store under the configured global root, and Linux SSH masters under
//! `~/.orbit`, even when those roots differ.
//!
//! The clock tick hands a workspace only the `clock.env` names its
//! `execution.env.pass` admits, so the file itself must stay out of every
//! sandboxed agent's reach. The decision-channel store mirrors every
//! channel's token key, so it is masked whole, including a registration
//! written after the agent started [ORB-19338]. The compiled plans are pinned
//! by the sandbox goldens; this checks the resolution through the public
//! runtime API.

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
        // The host registers a channel once this agent is running, then
        // releases it on stdin.
        let mut released = String::new();
        std::io::stdin()
            .read_line(&mut released)
            .expect("wait for the host's registration");
        assert_eq!(released, "registered\n");
        let global = std::path::PathBuf::from(
            std::env::var_os("ORBIT_AGENT_MASK_GLOBAL").expect("global root"),
        );
        assert!(
            orbit_core::runtime::plugin::sandbox_mask::decision_channel_store_masked(&global),
            "a nested orbit sees the decision-channel store as masked"
        );
        let store = global.join("state/decision-channels");
        let names: Vec<_> = std::fs::read_dir(&store)
            .expect("the sentinel stands in for the store")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(
            names,
            vec![std::ffi::OsString::from(".orbit-brokered")],
            "neither the registration nor a staged write is visible"
        );
        assert!(std::fs::read(store.join("channels.json")).is_err());
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
    let canonical_global = global.canonicalize().expect("canonical global root");
    let clock_env =
        canonical_global.join(orbit_common::security::operator_env::CLOCK_ENV_FILE_NAME);
    let decision_store = canonical_global.join("state/decision-channels");
    #[cfg(target_os = "linux")]
    let mut registrations = 0;
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
            assert!(
                mask.targets.contains(&decision_store),
                "{provider} ({fs_profile:?}) must mask the decision-channel store under the \
                 configured global root: {:?}",
                mask.targets
            );
            assert!(
                decision_store.is_dir(),
                "the host creates the store before launch, so a later registration lands under \
                 the mask"
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
                    // Even an explicit grant cannot reopen the master or the
                    // decision-channel store.
                    profile.modify.push(format!("{}/**", sockets.display()));
                    profile
                        .modify
                        .push(format!("{}/**", decision_store.display()));
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
                    let mut child = orbit_exec::spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
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
                            (
                                "ORBIT_AGENT_MASK_GLOBAL".to_string(),
                                canonical_global.display().to_string(),
                            ),
                        ],
                        cwd: Some(&worktree),
                        stdin: Stdio::piped(),
                        stdout: Stdio::piped(),
                        stderr: Stdio::piped(),
                    })
                    .expect("spawn confined socket client");
                    // Register through the operator path while the agent
                    // runs: the store and its staged write land on the host.
                    registrations += 1;
                    runtime
                        .register_decision_channel(
                            &format!("mask-{registrations}"),
                            "human:operator",
                            "decision-mail",
                            vec!["ops@example.com".to_string()],
                        )
                        .expect("register a channel after launch");
                    assert!(
                        std::fs::read_to_string(decision_store.join("channels.json"))
                            .expect("host store")
                            .contains("key_hex"),
                        "the host store holds the key the agent must not read"
                    );
                    {
                        use std::io::Write;
                        let mut stdin = child.stdin.take().expect("child stdin");
                        stdin
                            .write_all(b"registered\n")
                            .expect("release the confined child");
                    }
                    let output = child.wait_with_output().expect("wait");
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
                // An explicit grant on the store does not reopen it: the mask
                // is appended after every allow.
                let mut profile = resolved.fs_profile.clone();
                profile
                    .modify
                    .push(format!("{}/**", decision_store.display()));
                let mut sbpl = orbit_exec::compile_macos_sandbox_profile(&profile, provider)
                    .expect("compile profile");
                orbit_exec::append_macos_subpath_mask(&mut sbpl, &mask.targets);
                orbit_exec::append_macos_file_mask(&mut sbpl, &mask.files);
                let store = orbit_exec::physical_with_missing_tail(&decision_store);
                let store_deny = format!(
                    "(deny file-read* file-write* (subpath \"{}\"))",
                    store.display()
                );
                assert!(
                    sbpl.lines().any(|line| line == store_deny),
                    "{provider} profile must deny the decision-channel store:\n{sbpl}"
                );
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
            image_input: None,
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
