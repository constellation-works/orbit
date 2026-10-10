//! The agent sandbox mask hides the clock credentials file, resolved under
//! the configured global root rather than `~/.orbit`.
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
    let root = tempfile::tempdir().expect("tempdir");
    let global = root.path().join("relocated/orbit-global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::create_dir_all(&workspace).expect("workspace root");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("runtime");
    let repo_root = root.path().join("repo");
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
                .resolve_executor_sandbox(provider, fs_profile, Some(&repo_root))
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
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            auth_probe: None,
            env: Default::default(),
            sandbox: Some(sandbox),
            allow_fallback: false,
            created_at: None,
            updated_at: None,
        })
        .expect("seed executor");
}
