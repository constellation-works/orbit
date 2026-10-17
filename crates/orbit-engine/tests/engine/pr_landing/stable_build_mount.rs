//! Host validation never runs test binaries a Linux sandbox built through its
//! stable build mount [ORB-15870], including a recovery custom target whose
//! compiled manifest path exists only through the source alias [F2026-10-342].
//! Real Cargo/rustc compile and execute both arms; disposable symlinks model
//! the namespace lifetime deterministically without requiring nested bwrap.
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
            for (route, target_path) in [
                ("candidate_validate", "target/orbit-host-validation"),
                ("claim_validate", "target/orbit-host-validation"),
                ("candidate_validate", "target"),
                ("claim_validate", "target"),
                ("host_evidence", "target/orbit-host-validation"),
            ] {
                let fx = spawning_package(sandbox);
                let mut env = toolchain_env();
                let target = fx.repo.join(target_path);
                env.push(("CARGO_TARGET_DIR".into(), path_str(&target).into()));
                build_through_stable_mount(&fx, sandbox, &env, &target, target_path != "target");
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
                let mut environment = ValidationEnvironment::launcher(env);
                environment.build_output_env = vec!["CARGO_TARGET_DIR".into()];
                host.resolve_validation_env(environment);
                let run = || {
                    if route == "host_evidence" {
                        let candidate = orbit_types::workflow::automation::SourceRevision {
                            commit: fx.candidate.clone(),
                            tree: git(&fx.repo, &["rev-parse", "HEAD^{tree}"]),
                        };
                        let admitted = orbit_types::workflow::HostSandboxCommand::admit(
                            COMMAND,
                            &[COMMAND.into()],
                        )
                        .unwrap();
                        let evidence = review_gate::run_host_sandbox_test(
                            &host, &fx.repo, &candidate, &admitted, RUN_ID,
                        )
                        .unwrap();
                        assert!(evidence.judgement.is_ok(), "{evidence:?}");
                        return evidence.environment;
                    }
                    let input = if route == "candidate_validate" {
                        fx.validate_input()
                    } else {
                        *host.ship_mode.lock().unwrap() = "pr".into();
                        json!({"workspace_path": fx.repo, "base_sync": "local", "base_sha": fx.base_sha})
                    };
                    action(&host, route, &input)
                        .unwrap_or_else(|error| panic!("{route} reused the sandbox build: {error}\nPoisoned target output:\n{}", output_text(&shared)))
                        ["validation_env"]
                        .clone()
                };
                let first = run();
                let host_target =
                    PathBuf::from(first["build_output"]["directory"].as_str().unwrap());
                assert!(
                    !host_target.starts_with(fx.repo.join("target")),
                    "F2026-10-342: recovery can write every target below the checkout"
                );
                assert_ne!(host_target, target);
                let binaries = fs::read_dir(host_target.join("debug/deps"))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| {
                        path.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with("spawn-")
                            && path.extension().is_none()
                    })
                    .map(|path| {
                        let time = fs::metadata(&path).unwrap().modified().unwrap();
                        (path, time)
                    })
                    .collect::<Vec<_>>();
                assert!(
                    !binaries.is_empty(),
                    "the corrected route built a real integration-test binary"
                );
                let incremental = host_target.join("debug/incremental");
                let cache_units = fs::read_dir(&incremental)
                    .map(|entries| entries.count())
                    .unwrap_or(0);
                assert_eq!(
                    cache_units, 0,
                    "a host validation target built by delivery must not contain incremental output"
                );
                let second = run();
                assert_eq!(
                    first["build_output"]["directory"], second["build_output"]["directory"],
                    "ordinary retry reuses its source namespace"
                );
                if route == "host_evidence" {
                    // Evidence sources are freshly checked out on each run;
                    // ordinary candidate sources retain timestamps for reuse.
                    let ordinary =
                        action(&host, "candidate_validate", &fx.validate_input()).unwrap();
                    assert_ne!(
                        ordinary["validation_env"]["build_output"]["directory"],
                        first["build_output"]["directory"],
                        "different compiled source paths must have different host targets"
                    );
                } else {
                    for (path, time) in binaries {
                        assert_eq!(
                            fs::metadata(&path).unwrap().modified().unwrap(),
                            time,
                            "ordinary host reuse does not rebuild {path:?}"
                        );
                    }
                }
                assert!(
                    !cargo_test(&fx.repo, &toolchain_env_with_target(&target), None)
                        .status
                        .success(),
                    "the original sandbox artifacts remain intact, not cleaned or accepted"
                );
                if route == "candidate_validate" && target_path == "target/orbit-host-validation" {
                    let saved = host_target.with_extension("saved");
                    fs::rename(&host_target, &saved).unwrap();
                    std::os::unix::fs::symlink(&target, &host_target).unwrap();
                    let refusal = action(&host, route, &fx.validate_input());
                    fs::remove_file(&host_target).unwrap();
                    fs::rename(&saved, &host_target).unwrap();
                    assert!(
                        matches!(refusal, Err(OrbitError::PolicyDenied(_))),
                        "a host target redirect must never admit sandbox-built output: {refusal:?}"
                    );
                }
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
    fx.commit_on_base(".gitignore", "/target/\n/Cargo.lock\n/.orbit/\n");
    fx.commit_on_base(
        "Cargo.toml",
        "[package]\nname = \"spawnee\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    fx.commit_on_base("src/main.rs", "fn main() {}\n");
    fx.commit_on_base(
        "tests/spawn.rs",
        "#[test]\nfn spawns_its_binary() {\n    \
         let manifest = env!(\"CARGO_MANIFEST_DIR\");\n    \
         std::fs::read_to_string(std::path::Path::new(manifest).join(\"README.md\"))\n        \
         .unwrap_or_else(|error| panic!(\"compiled manifest path {manifest}: {error}\"));\n    \
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

/// Compile real artifacts with either the stable build alias or source-only
/// rewriting into the legacy custom target, as the compiler wrapper does.
/// Removing just the disposable alias models leaving the sandbox namespace;
/// all compiled artifacts remain for the host freshness reproduction.
fn build_through_stable_mount(
    fx: &Fixture,
    sandbox: &Path,
    env: &[(String, String)],
    target: &Path,
    rewrite_source: bool,
) {
    fs::create_dir_all(target).unwrap();
    let mount = if rewrite_source {
        target.to_path_buf()
    } else {
        sandbox.join("stable-build")
    };
    let source = if rewrite_source {
        sandbox.join("stable-source")
    } else {
        fx.repo.clone()
    };
    if rewrite_source {
        std::os::unix::fs::symlink(&fx.repo, &source).unwrap();
    }
    if !rewrite_source {
        std::os::unix::fs::symlink(target, &mount).unwrap();
    }
    let wrapper = sandbox.join("rewrite-onto-stable-mount.sh");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\
             for name in $(env | sed -n 's/^\\(CARGO_BIN_EXE_[A-Za-z0-9_]*\\)=.*/\\1/p'); do\n\
             eval \"value=\\${{$name}}\"\n\
             case \"$value\" in '{target}'/*) export \"$name={mount}${{value#'{target}'}}\";; esac\n\
             done\n\
             export CARGO_MANIFEST_DIR='{source}'\n\
             exec \"$@\"\n",
            target = target.display(),
            mount = mount.display(),
            source = source.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let sandboxed = cargo_test(&fx.repo, env, Some(&wrapper));
    assert!(sandboxed.status.success(), "{}", output_text(&sandboxed));
    if !rewrite_source {
        fs::remove_file(&mount).unwrap();
    }
    if rewrite_source {
        fs::remove_file(&source).unwrap();
    }
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

fn toolchain_env_with_target(target: &Path) -> Vec<(String, String)> {
    let mut env = toolchain_env();
    env.push(("CARGO_TARGET_DIR".into(), path_str(target).into()));
    env
}

fn cargo_test(
    repo: &Path,
    env: &[(String, String)],
    wrapper: Option<&Path>,
) -> orbit_common::process::CapturedOutput {
    let mut command = Command::new("cargo");
    command
        .args(["test", "--offline"])
        .current_dir(repo)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)));
    if let Some(wrapper) = wrapper {
        command.env("RUSTC_WRAPPER", wrapper);
    }
    run_bounded_capped(&mut command, Duration::from_secs(60), 256 * 1024).unwrap()
}

fn output_text(output: &orbit_common::process::CapturedOutput) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn workspace_build_output_is_opt_in_and_toolchain_neutral() {
    isolated(
        "stable_build_mount::workspace_build_output_is_opt_in_and_toolchain_neutral",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            fx.commit(".gitignore", "/.orbit/\n");
            let candidate = fx.commit("validate.sh", "test -z \"${CARGO_TARGET_DIR+x}\" && mkdir -p \"$DIST_DIR\" && printf built > \"$DIST_DIR/output\"\n");
            let fx = Fixture { candidate, ..fx };
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let env = vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("DIST_DIR".into(), "/not-an-output".into()),
            ];
            host.resolve_validation_env(ValidationEnvironment::launcher(env.clone()));
            let unconfigured = review_gate::run_required_validation(
                &host,
                &fx.repo,
                "test -z \"${CARGO_TARGET_DIR+x}\" && test \"$DIST_DIR\" = /not-an-output",
            )
            .unwrap();
            assert!(unconfigured.passed, "{}", unconfigured.output);
            assert!(unconfigured.environment["build_output"]["directory"].is_null());
            assert!(
                !fx.repo
                    .join(".orbit/state/host-validation-targets")
                    .exists()
            );
            let mut environment = ValidationEnvironment::launcher(env);
            environment.build_output_env = vec!["DIST_DIR".into()];
            host.resolve_validation_env(environment);
            let command = "sh validate.sh";
            host.require_commands(&[command]);
            let candidate = review_gate::revision(&fx.repo, &fx.candidate).unwrap();
            let mut sources = Vec::new();
            for route in ["candidate_validate", "claim_validate", "host_evidence"] {
                let record = if route == "host_evidence" {
                    let admitted = orbit_types::workflow::HostSandboxCommand::admit(
                        command,
                        &[command.into()],
                    )
                    .unwrap();
                    let run = review_gate::run_host_sandbox_test(
                        &host, &fx.repo, &candidate, &admitted, RUN_ID,
                    )
                    .unwrap();
                    assert_eq!(run.exit_code, Some(0), "{}", run.output);
                    run.environment
                } else {
                    let input = if route == "candidate_validate" {
                        fx.validate_input()
                    } else {
                        *host.ship_mode.lock().unwrap() = "pr".into();
                        json!({"workspace_path": fx.repo, "base_sync": "local", "base_sha": fx.base_sha})
                    };
                    action(&host, route, &input).unwrap()["validation_env"].clone()
                };
                let output = PathBuf::from(record["build_output"]["directory"].as_str().unwrap());
                assert!(!output.starts_with(fx.repo.join("target")));
                assert_eq!(fs::read_to_string(output.join("output")).unwrap(), "built");
                assert_eq!(record["build_output"]["env"], json!(["DIST_DIR"]));
                assert_eq!(record["build_output"]["context"], "host");
                sources.push(output);
            }
            assert_eq!(
                sources[0], sources[1],
                "delivery routes share their host source namespace"
            );
            assert_ne!(
                sources[0], sources[2],
                "detached sources cannot reuse checkout output"
            );
        },
    );
}

/// Host-bound validation forces incremental compilation off at the spawn.
/// Required validation, its scratch base-rerun and recovery checkouts, and
/// detached host evidence all bind through the same runner. No binding
/// leaves the variable alone. `cargo_incremental` is the operator opt-in.
#[test]
fn host_validation_forces_incremental_off_unless_the_operator_opts_in() {
    isolated(
        "stable_build_mount::host_validation_forces_incremental_off_unless_the_operator_opts_in",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            fx.commit(".gitignore", "/.orbit/\n");
            let candidate = fx.commit(
                "check.sh",
                "if [ -z \"${CARGO_INCREMENTAL+x}\" ]; then printf unset; else printf '%s' \"$CARGO_INCREMENTAL\"; fi\n",
            );
            let fx = Fixture { candidate, ..fx };
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let command = "sh check.sh";
            host.require_commands(&[command]);
            let path = vec![("PATH".into(), "/usr/bin:/bin".into())];

            let mut ambient = path.clone();
            ambient.push(("CARGO_INCREMENTAL".into(), "1".into()));
            host.resolve_validation_env(ValidationEnvironment::launcher(ambient.clone()));
            let unbound = review_gate::run_required_validation(&host, &fx.repo, command).unwrap();
            assert!(unbound.passed, "{}", unbound.output);
            assert_eq!(unbound.output, "1");
            assert!(unbound.environment["build_output"]["directory"].is_null());
            assert!(
                !fx.repo
                    .join(".orbit/state/host-validation-targets")
                    .exists(),
                "no binding allocates no host target"
            );

            let mut bound = ValidationEnvironment::launcher(ambient.clone());
            bound.build_output_env = vec!["CARGO_TARGET_DIR".into()];
            host.resolve_validation_env(bound);
            let forced = review_gate::run_required_validation(&host, &fx.repo, command).unwrap();
            assert!(forced.passed, "{}", forced.output);
            assert_eq!(
                forced.output, "0",
                "an inherited CARGO_INCREMENTAL is replaced when a host target is bound"
            );
            let checkout_target = PathBuf::from(
                forced.environment["build_output"]["directory"]
                    .as_str()
                    .unwrap(),
            );
            assert!(checkout_target.is_dir());

            let revision = orbit_types::workflow::automation::SourceRevision {
                commit: fx.candidate.clone(),
                tree: git(&fx.repo, &["rev-parse", "HEAD^{tree}"]),
            };
            let admitted =
                orbit_types::workflow::HostSandboxCommand::admit(command, &[command.to_string()])
                    .unwrap();
            let evidence =
                review_gate::run_host_sandbox_test(&host, &fx.repo, &revision, &admitted, RUN_ID)
                    .unwrap();
            assert!(evidence.judgement.is_ok(), "{evidence:?}");
            assert_eq!(evidence.output, "0");
            assert_ne!(
                evidence.environment["build_output"]["directory"],
                forced.environment["build_output"]["directory"],
                "a detached evidence checkout binds its own host target and still forces incremental off"
            );

            let mut opted = ValidationEnvironment::launcher(ambient);
            opted.build_output_env = vec!["CARGO_TARGET_DIR".into()];
            opted.cargo_incremental = true;
            host.resolve_validation_env(opted);
            let kept = review_gate::run_required_validation(&host, &fx.repo, command).unwrap();
            assert!(kept.passed, "{}", kept.output);
            assert_eq!(
                kept.output, "1",
                "cargo_incremental leaves an explicit CARGO_INCREMENTAL in place"
            );

            host.resolve_validation_env(ValidationEnvironment::launcher(path));
            let untouched = review_gate::run_required_validation(&host, &fx.repo, command).unwrap();
            assert!(untouched.passed, "{}", untouched.output);
            assert_eq!(untouched.output, "unset");
            assert!(untouched.environment["build_output"]["directory"].is_null());
        },
    );
}
