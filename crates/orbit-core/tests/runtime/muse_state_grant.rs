//! On Linux only the confined Muse executor may write Muse's state roots.
//!
//! Muse keeps its `muse login` credential (`auth.json`) and settings under its
//! XDG config root, and tracing, plugins and skills under its XDG data root.
//! Resolution creates every granted provider root, so a lane that inherited
//! the grant would also leave a `~/.config/muse` behind; this checks both
//! through the public runtime API. The macOS profile is covered beside the
//! `orbit-exec` compiler. [ORB-16577]

use std::path::Path;

use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, ExecutorType};

const TEST: &str = "muse_state_grant::only_the_muse_lane_may_write_muse_state_roots";
const CHILD: &str = "ORBIT_MUSE_STATE_GRANT_FIXTURE";

#[test]
fn only_the_muse_lane_may_write_muse_state_roots() {
    if std::env::var_os(CHILD).is_none() {
        let home = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("isolated home");
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("HOME", home.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME");
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
    let home = orbit_common::fs::path::home_dir()
        .expect("home")
        .canonicalize()
        .expect("canonical home");
    std::env::set_current_dir(&home).expect("fixture cwd");
    let global = home.join("orbit-global");
    let workspace = home.join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::create_dir_all(&workspace).expect("workspace root");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("runtime");
    let worktree = workspace.join("state/worktrees/orbit-jrun-muse-grant");
    std::fs::create_dir_all(&worktree).expect("managed fixture checkout");

    let default_roots = [home.join(".config/muse"), home.join(".local/share/muse")];
    for provider in ["claude", "codex", "opencode", "pi"] {
        let modify = resolved_modify(&runtime, provider, &worktree);
        for root in &default_roots {
            assert!(
                !modify.iter().any(|rule| Path::new(rule).starts_with(root)),
                "{provider} must not inherit Muse's grant {}: {modify:?}",
                root.display()
            );
            assert!(
                !root.exists(),
                "resolving {provider} must not create {}",
                root.display()
            );
        }
    }

    let modify = resolved_modify(&runtime, "muse", &worktree);
    for root in &default_roots {
        assert!(
            modify.contains(&root.display().to_string()),
            "muse must write {}: {modify:?}",
            root.display()
        );
        assert!(root.is_dir(), "muse resolution creates {}", root.display());
    }

    // XDG overrides move both roots; an empty value means unset.
    let config = home.join("xdg-config");
    let data = home.join("xdg-data");
    std::fs::create_dir_all(&config).expect("xdg config");
    std::fs::create_dir_all(&data).expect("xdg data");
    let overridden = {
        let _env = orbit_common::test_env::scoped([
            ("XDG_CONFIG_HOME", Some(config.to_str().expect("utf-8"))),
            ("XDG_DATA_HOME", Some(data.to_str().expect("utf-8"))),
        ]);
        resolved_modify(&runtime, "muse", &worktree)
    };
    for root in [config.join("muse"), data.join("muse")] {
        assert!(
            overridden.contains(&root.display().to_string()),
            "muse must follow XDG to {}: {overridden:?}",
            root.display()
        );
    }
    let empty = {
        let _env = orbit_common::test_env::scoped([
            ("XDG_CONFIG_HOME", Some("")),
            ("XDG_DATA_HOME", Some("")),
        ]);
        resolved_modify(&runtime, "muse", &worktree)
    };
    for root in &default_roots {
        assert!(empty.contains(&root.display().to_string()), "{empty:?}");
    }
}

fn resolved_modify(runtime: &OrbitRuntime, provider: &str, worktree: &Path) -> Vec<String> {
    seed_executor(runtime, provider);
    runtime
        .resolve_executor_sandbox(provider, None, Some(worktree))
        .unwrap_or_else(|error| panic!("{provider} sandbox must resolve: {error}"))
        .unwrap_or_else(|| panic!("{provider} must resolve a sandbox"))
        .fs_profile
        .modify
}

fn seed_executor(runtime: &OrbitRuntime, provider: &str) {
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
            sandbox: Some(ExecutorSandboxKind::LinuxBwrap),
            allow_fallback: false,
            host_os: None,
            created_at: None,
            updated_at: None,
        })
        .expect("seed executor");
}
