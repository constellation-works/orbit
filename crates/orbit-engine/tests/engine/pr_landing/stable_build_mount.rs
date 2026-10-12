//! Host validation never runs test binaries a Linux sandbox built through its
//! stable build mount [ORB-15870].
//!
//! The sandbox binds `<checkout>/target` at a stable path, and the
//! compiler-cache wrapper rewrites `CARGO_BIN_EXE_*` onto it, so a test binary
//! built there spawns a path that exists only inside the sandbox. Cargo calls
//! that binary fresh at the real path. After step recovery built in the
//! sandbox, the host's next validation reused such binaries and every test
//! that spawns the crate's binary failed with `No such file or directory`.

use super::*;

const COMMAND: &str = "cargo test --offline";

#[test]
fn host_validation_rebuilds_test_binaries_built_through_the_stable_build_mount() {
    isolated(
        "stable_build_mount::host_validation_rebuilds_test_binaries_built_through_the_stable_build_mount",
        |sandbox| {
            for route in ["candidate_validate", "claim_validate"] {
                let fx = spawning_package(sandbox);
                let env = toolchain_env();
                build_through_stable_mount(&fx, sandbox, &env);
                let shared = cargo_test(&fx.repo, &env, None);
                assert!(
                    !shared.status.success()
                        && output_text(&shared).contains("No such file or directory"),
                    "precondition: Cargo reuses the sandbox-built test binary in the shared \
                     target: {}",
                    output_text(&shared)
                );

                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                host.require_commands(&[COMMAND]);
                host.resolve_validation_env(ValidationEnvironment::launcher(env));
                let input = if route == "candidate_validate" {
                    fx.validate_input()
                } else {
                    *host.ship_mode.lock().unwrap() = "pr".into();
                    json!({"workspace_path": fx.repo, "base_sync": "local", "base_sha": fx.base_sha})
                };
                action(&host, route, &input)
                    .unwrap_or_else(|error| panic!("{route} reused the sandbox build: {error}"));
                assert_eq!(fx.head(), fx.candidate, "{route}");
                assert!(
                    git(&fx.repo, &["status", "--porcelain"]).is_empty(),
                    "{route}: host build output stays ignored"
                );
            }
        },
    );
}

/// A base holding a package whose integration test spawns its binary, and a
/// candidate on it. The base's own run passes, so a reused sandbox build is
/// the candidate's failure rather than known red.
fn spawning_package(sandbox: &Path) -> Fixture {
    let fx = Fixture::new(sandbox);
    fx.commit_on_base(".gitignore", "/target/\n/Cargo.lock\n");
    fx.commit_on_base(
        "Cargo.toml",
        "[package]\nname = \"spawnee\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    fx.commit_on_base("src/main.rs", "fn main() {}\n");
    fx.commit_on_base(
        "tests/spawn.rs",
        "#[test]\nfn spawns_its_binary() {\n    \
         let binary = env!(\"CARGO_BIN_EXE_spawnee\");\n    \
         let status = std::process::Command::new(binary)\n        \
         .status()\n        \
         .unwrap_or_else(|error| panic!(\"spawn {binary}: {error}\"));\n    \
         assert!(status.success());\n}\n",
    );
    git(&fx.repo, &["rebase", BASE]);
    git(&fx.repo, &["push", "--force", "origin", BRANCH]);
    let candidate = fx.commit(
        "src/main.rs",
        "fn main() {\n    println!(\"spawned\");\n}\n",
    );
    Fixture {
        base_sha: git(&fx.repo, &["rev-parse", BASE]),
        candidate,
        ..fx
    }
}

/// Build and pass the tests as a sandboxed step does: `target/` aliased at a
/// stable path, and the rustc wrapper rewriting `CARGO_BIN_EXE_*` onto it as
/// `scripts/rustc-compiler-cache.sh` does. Then leave the sandbox, whose
/// mount namespace held the alias.
fn build_through_stable_mount(fx: &Fixture, sandbox: &Path, env: &[(String, String)]) {
    let target = fx.repo.join("target");
    fs::create_dir_all(&target).unwrap();
    let mount = sandbox.join("stable-build");
    std::os::unix::fs::symlink(&target, &mount).unwrap();
    let wrapper = sandbox.join("rewrite-onto-stable-mount.sh");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\
             for name in $(env | sed -n 's/^\\(CARGO_BIN_EXE_[A-Za-z0-9_]*\\)=.*/\\1/p'); do\n\
             eval \"value=\\${{$name}}\"\n\
             case \"$value\" in '{target}'/*) export \"$name={mount}${{value#'{target}'}}\";; esac\n\
             done\n\
             exec \"$@\"\n",
            target = target.display(),
            mount = mount.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let sandboxed = cargo_test(&fx.repo, env, Some(&wrapper));
    assert!(sandboxed.status.success(), "{}", output_text(&sandboxed));
    fs::remove_file(&mount).unwrap();
}

/// One toolchain for the sandboxed and host builds, so only the target path
/// differs between them, and no rustc wrapper from any Cargo configuration.
fn toolchain_env() -> Vec<(String, String)> {
    let path = std::env::var("PATH").unwrap_or_default();
    let path = match std::env::var_os("CARGO").map(PathBuf::from) {
        Some(cargo) => format!("{}:{path}", cargo.parent().unwrap().display()),
        None => path,
    };
    let mut env = vec![
        ("PATH".to_string(), path),
        ("CARGO_NET_OFFLINE".to_string(), "true".to_string()),
        ("RUSTC_WRAPPER".to_string(), String::new()),
    ];
    for key in ["HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN"] {
        if let Ok(value) = std::env::var(key) {
            env.push((key.to_string(), value));
        }
    }
    env
}

fn cargo_test(
    repo: &Path,
    env: &[(String, String)],
    wrapper: Option<&Path>,
) -> std::process::Output {
    let mut command = Command::new("cargo");
    command
        .args(["test", "--offline"])
        .current_dir(repo)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)));
    if let Some(wrapper) = wrapper {
        command.env("RUSTC_WRAPPER", wrapper);
    }
    command.output().unwrap()
}

fn output_text(output: &std::process::Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
