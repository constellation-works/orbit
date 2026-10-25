//! `orbit mcp init` / `orbit mcp remove` against an external Orbit data root.
//!
//! `orbit --root <dir> workspace init` registers a checkout whose Orbit root
//! lives outside the repository, so nothing under the checkout marks it as an
//! Orbit workspace. Only the workspace registry in that root knows the pair,
//! and these tests pin that both setup commands resolve the checkout through
//! it: the client config lands in the repository, carries the registered
//! `ws_*` binding and, when that catalog is not the host-global default, the
//! `--root` the generated process needs to open it. A root that names no
//! single registered checkout refuses instead of writing somewhere else
//! (ORB-12121).

#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_cmd::mcp_clients::{McpClient, McpClientScope, client_config_paths};
use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use toml_edit::DocumentMut;

/// One machine with an external Orbit root and a single checkout registered
/// against it.
struct ExternalRootFixture {
    _temp: TempDir,
    home: PathBuf,
    orbit_root: PathBuf,
    checkout: PathBuf,
    elsewhere: PathBuf,
}

impl ExternalRootFixture {
    fn init() -> Self {
        Self::init_at(None)
    }

    /// Lay out the external root, home and outside directories without running
    /// any `orbit` process, for helpers that only inspect paths.
    fn paths_at(home_suffix: Option<&str>) -> Self {
        // `--root` is recorded as given, and the assertions below expect the
        // resolved spelling, so root the fixture at a resolved temp directory
        // (the default macOS one sits behind the `/var` symlink).
        let temp =
            tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir()).expect("tempdir");
        let home = temp.path().join("home");
        // Deliberately nested, so a write to the root's parent is visible.
        let orbit_root = home_suffix
            .map(|suffix| home.join(suffix))
            .unwrap_or_else(|| temp.path().join("orbit-data").join("root"));
        let checkout = temp.path().join("checkout");
        let elsewhere = temp.path().join("elsewhere");
        for directory in [&home, &elsewhere] {
            fs::create_dir_all(directory).expect("fixture directory");
        }

        Self {
            _temp: temp,
            home,
            orbit_root,
            checkout,
            elsewhere,
        }
    }

    fn init_at(home_suffix: Option<&str>) -> Self {
        let fixture = Self::paths_at(home_suffix);
        crate::git_repo::SeededRepo::new(&fixture.checkout).build();

        fixture
            .orbit(
                &fixture.checkout,
                &fixture.rooted(&[
                    "init",
                    "--non-interactive",
                    "--machine-name",
                    "external-root-host",
                    "--task-prefix",
                    "EXR",
                ]),
            )
            .success();
        fixture
            .orbit(
                &fixture.checkout,
                &fixture.rooted(&["workspace", "init", "--name", "wsname"]),
            )
            .success();
        fixture.assert_external_root_layout();
        fixture
    }

    /// Confirm the setup produced the layout these tests are about: the
    /// registry in `orbit_root` binds the checkout, and the checkout itself
    /// carries no `.orbit` for a filesystem walk-up to find.
    fn assert_external_root_layout(&self) {
        let assert = self
            .orbit(
                &self.checkout,
                &self.rooted(&["workspace", "show", "--format", "json"]),
            )
            .success();
        let shown: Value =
            serde_json::from_slice(&assert.get_output().stdout).expect("workspace show json");
        assert_eq!(shown["workspace"]["id"], "ws_wsname");
        assert_eq!(
            shown["checkout"]["repo_root"],
            canonical_str(&self.checkout)
        );
        assert_eq!(
            shown["checkout"]["orbit_dir"],
            canonical_str(&self.orbit_root)
        );
        assert!(!self.checkout.join(".orbit").exists());
    }

    /// Prefix `args` with the explicit data-root selector this layout requires.
    fn rooted(&self, args: &[&str]) -> Vec<String> {
        let mut rooted = vec![
            "--root".to_string(),
            self.orbit_root
                .to_str()
                .expect("utf8 orbit root")
                .to_string(),
        ];
        rooted.extend(argv(args));
        rooted
    }

    fn orbit(&self, cwd: &Path, args: &[String]) -> assert_cmd::assert::Assert {
        self.orbit_with_env(cwd, args, &[])
    }

    fn orbit_with_env(
        &self,
        cwd: &Path,
        args: &[String],
        env: &[(&str, &Path)],
    ) -> assert_cmd::assert::Assert {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env_remove("ORBIT_ROOT")
            .env_remove("ORBIT_WORKSPACE")
            .args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        command.assert()
    }

    fn claude_config(&self) -> PathBuf {
        self.checkout.join(".mcp.json")
    }

    fn claude_settings(&self) -> PathBuf {
        self.checkout.join(".claude").join("settings.json")
    }

    /// Every directory that a mis-resolved run has historically written into.
    fn outside_config_directories(&self) -> [PathBuf; 4] {
        [
            self.orbit_root.clone(),
            self.orbit_root
                .parent()
                .expect("orbit root parent")
                .to_path_buf(),
            self.elsewhere.clone(),
            self.home.clone(),
        ]
    }

    /// Reject MCP client config outside the checkout while allowing the
    /// designed sibling `.claude/skills` links for an overridden root.
    fn assert_no_client_config_outside_the_checkout(&self) {
        let mut stray_configs = Vec::new();
        for directory in self.outside_config_directories() {
            for name in [".mcp.json", ".claude.json"] {
                let config = directory.join(name);
                match fs::symlink_metadata(&config) {
                    Ok(_) => stray_configs.push(config),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        panic!("inspect client config path {}: {error}", config.display())
                    }
                }
            }

            let claude_dir = directory.join(".claude");
            match fs::read_dir(&claude_dir) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = entry.expect("read .claude entry");
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with("settings") && name.ends_with(".json") {
                            stray_configs.push(entry.path());
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!(
                    "inspect .claude directory {}: {error}",
                    claude_dir.display()
                ),
            }
        }
        assert!(
            stray_configs.is_empty(),
            "MCP client config written outside the checkout: {}",
            stray_configs
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );

        self.assert_sibling_claude_contains_only_skill_links();
    }

    /// The external-root sibling `.claude` is reserved for skill links.
    fn assert_sibling_claude_contains_only_skill_links(&self) {
        let claude_dir = self
            .orbit_root
            .parent()
            .expect("orbit root parent")
            .join(".claude");
        let metadata = match fs::symlink_metadata(&claude_dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("read sibling .claude metadata: {error}"),
        };
        assert!(
            metadata.file_type().is_dir(),
            "sibling .claude must be a directory containing only skills links: {}",
            claude_dir.display()
        );

        for entry in fs::read_dir(&claude_dir).expect("read sibling .claude") {
            let entry = entry.expect("read sibling .claude entry");
            assert_eq!(
                entry.file_name(),
                "skills",
                "sibling .claude may contain only the skills directory: {}",
                entry.path().display()
            );
            assert!(
                entry.file_type().expect("read skills entry type").is_dir(),
                "sibling .claude/skills must be a directory: {}",
                entry.path().display()
            );
            for skill in fs::read_dir(entry.path()).expect("read sibling skill links") {
                let skill = skill.expect("read sibling skill link");
                assert!(
                    skill
                        .file_type()
                        .expect("read sibling skill link type")
                        .is_symlink(),
                    "sibling .claude/skills may contain only skill links: {}",
                    skill.path().display()
                );
            }
        }
    }
}

fn mcp_setup_expands_tilde_and_anchors_relative_roots_for_flags_and_environment() {
    for (raw, home_suffix) in [
        ("~", Some("")),
        ("~/x", Some("x")),
        ("~//x", Some("x")),
        ("~/.orbit", Some(".orbit")),
        ("../orbit-data/root", None),
    ] {
        let fixture = ExternalRootFixture::init_at(home_suffix);
        for use_flag in [false, true] {
            let env_root = if use_flag { "unused-env-root" } else { raw };
            let env = [("ORBIT_ROOT", Path::new(env_root))];
            let selected = |args: &[&str]| {
                let mut args = argv(args);
                if use_flag {
                    args.splice(0..0, argv(&["--root", raw]));
                }
                fixture.orbit_with_env(&fixture.checkout, &args, &env)
            };
            selected(&["mcp", "init", "--claude"]).success();
            assert_eq!(
                generated_server_args(&fixture.claude_config()),
                expected_local_args(&fixture, "ws_wsname"),
                "{raw} must bind the checkout registered at the expanded root"
            );
            fixture.assert_no_client_config_outside_the_checkout();
            selected(&["mcp", "remove", "--claude"]).success();
            assert!(!fixture.claude_config().exists());
            assert!(!fixture.checkout.join("~").exists());
            assert!(!fixture.checkout.join("unused-env-root").exists());
        }
    }
}

#[test]
fn outside_client_config_guard_rejects_config_files_in_scanned_directories() {
    // The silenced panic hook is process-wide, so the body runs in a child.
    if crate::isolated_test::run_in_child(
        "mcp_setup_root::outside_client_config_guard_rejects_config_files_in_scanned_directories",
    ) {
        return;
    }
    let fixture = ExternalRootFixture::paths_at(None);

    for directory in fixture.outside_config_directories() {
        for relative_path in [
            ".mcp.json",
            ".claude.json",
            ".claude/settings.json",
            ".claude/settings.local.json",
            ".claude/settings.extra.json",
        ] {
            let config = directory.join(relative_path);
            fs::create_dir_all(config.parent().expect("config parent"))
                .expect("create config parent");
            fs::write(&config, "{}").expect("write stray client config");

            let prev_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                fixture.assert_no_client_config_outside_the_checkout();
            }));
            std::panic::set_hook(prev_hook);
            let message = result
                .expect_err("outside client config guard must reject the config")
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default();
            assert!(
                message.contains("MCP client config written outside the checkout")
                    && message.contains(&config.display().to_string()),
                "unexpected guard failure for {}: {message}",
                config.display()
            );

            fs::remove_file(config).expect("remove stray client config");
        }
    }

    fixture.assert_no_client_config_outside_the_checkout();
}

/// Run a federated setup command and return its text and JSON renderings.
fn federated_setup_outputs(
    fixture: &ExternalRootFixture,
    cwd: &Path,
    rooted: bool,
    args: &[&str],
) -> (String, Value) {
    let run = |extra: &[&str]| {
        let mut full = argv(args);
        full.extend(argv(extra));
        if rooted {
            let mut with_root = fixture.rooted(&[]);
            with_root.extend(full);
            full = with_root;
        }
        let assert = fixture.orbit(cwd, &full).success();
        String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout")
    };
    let text = run(&[]);
    let json = serde_json::from_str(&run(&["--format", "json"])).expect("setup json output");
    (text, json)
}

fn federated_home_setup_works_outside_a_workspace_and_reports_no_binding() {
    let fixture = ExternalRootFixture::init();
    let codex_config = fixture.home.join(".codex").join("config.toml");

    let (text, json) = federated_setup_outputs(
        &fixture,
        &fixture.elsewhere,
        false,
        &[
            "mcp",
            "init",
            "--federated",
            "--client",
            "codex",
            "--scope",
            "home",
        ],
    );
    assert!(
        !text.contains("ws_") && !text.contains("bound to"),
        "federated init names no workspace binding: {text}"
    );
    assert!(json["workspace_id"].is_null(), "json binding: {json}");
    assert!(json["repo_root"].is_null(), "json repo_root: {json}");
    let written = fs::read_to_string(&codex_config).expect("federated codex config");
    assert!(written.contains("orbit-federated"), "{written}");
    assert!(
        !written.contains("--workspace"),
        "the federated entry is unbound: {written}"
    );
    assert!(
        !written.contains("--root"),
        "a default federated entry does not carry a local root: {written}"
    );

    let (text, json) = federated_setup_outputs(
        &fixture,
        &fixture.elsewhere,
        false,
        &[
            "mcp",
            "remove",
            "--federated",
            "--client",
            "codex",
            "--scope",
            "home",
        ],
    );
    assert!(
        !text.contains("ws_") && !text.contains("bound to"),
        "federated remove names no workspace binding: {text}"
    );
    assert!(json["workspace_id"].is_null(), "json binding: {json}");
    let remaining = fs::read_to_string(&codex_config).unwrap_or_default();
    assert!(!remaining.contains("orbit-federated"), "{remaining}");
}

fn federated_setup_inside_a_registered_checkout_reports_no_binding() {
    let fixture = ExternalRootFixture::init();

    for scope in ["home", "workspace"] {
        for action in ["init", "remove"] {
            let (text, json) = federated_setup_outputs(
                &fixture,
                &fixture.checkout,
                true,
                &[
                    "mcp",
                    action,
                    "--federated",
                    "--client",
                    "codex",
                    "--scope",
                    scope,
                ],
            );
            assert!(
                !text.contains("ws_wsname") && !text.contains("bound to"),
                "federated {action} --scope {scope} names a binding: {text}"
            );
            assert!(
                json["workspace_id"].is_null(),
                "federated {action} --scope {scope} json binding: {json}"
            );
            if action == "init" {
                let config = match scope {
                    "home" => fixture.home.join(".codex").join("config.toml"),
                    "workspace" => fixture.checkout.join(".codex").join("config.toml"),
                    other => panic!("unexpected federated scope {other}"),
                };
                let written = fs::read_to_string(&config).expect("federated codex config");
                assert_eq!(
                    toml_server_args(&written, "orbit-federated"),
                    vec![
                        "mcp".to_string(),
                        "serve".to_string(),
                        "--mode".to_string(),
                        "federated".to_string()
                    ],
                    "federated {scope} must not receive the local catalog root: {written}"
                );
            }
        }
    }
}

#[test]
fn mcp_init_binds_an_external_root_checkout_and_remove_reverses_it() {
    let fixture = ExternalRootFixture::init();
    let original_mcp = serde_json::json!({
        "mcpServers": { "other": { "command": "other-client" } },
        "userSetting": true,
    });
    let original_settings = serde_json::json!({
        "permissions": {
            "allow": ["Read(./notes/**)"],
            "deny": ["Read(./private/**)"],
        },
        "userSetting": true,
    });
    let settings_dir = fixture.checkout.join(".claude");
    fs::create_dir_all(&settings_dir).expect("create user settings directory");
    fs::write(fixture.claude_config(), original_mcp.to_string()).expect("seed user MCP config");
    fs::write(fixture.claude_settings(), original_settings.to_string())
        .expect("seed user settings");
    let entries = |directory: &Path| {
        fs::read_dir(directory)
            .expect("read config directory")
            .map(|entry| entry.expect("read config entry").file_name())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let checkout_entries = entries(&fixture.checkout);
    let settings_entries = entries(&settings_dir);
    let read_json = |path: &Path| -> Value {
        serde_json::from_slice(&fs::read(path).expect("read client config"))
            .expect("parse client JSON")
    };
    // An in-place rewrite would change these open files too. Atomic replacement
    // must leave the old descriptors' contents intact on Unix.
    #[cfg(unix)]
    let old_mcp = fs::File::open(fixture.claude_config()).expect("open original MCP config");
    #[cfg(unix)]
    let old_settings = fs::File::open(fixture.claude_settings()).expect("open original settings");

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "init", "--claude", "--scope", "workspace"]),
        )
        .success();

    let launch_args = expected_local_args(&fixture, "ws_wsname");
    assert_eq!(
        generated_server_args(&fixture.claude_config()),
        launch_args,
        "the generated server must carry the registered workspace binding and catalog root"
    );
    let mut expected_mcp = original_mcp.clone();
    expected_mcp["mcpServers"]["orbit"] = serde_json::json!({
        "command": "orbit",
        "args": launch_args,
    });
    assert_eq!(read_json(&fixture.claude_config()), expected_mcp);
    let settings = read_json(&fixture.claude_settings());
    let allow = settings["permissions"]["allow"]
        .as_array()
        .expect("allow array");
    assert!(allow.contains(&serde_json::json!("Read(./notes/**)")));
    assert!(
        allow.iter().any(|value| value
            .as_str()
            .is_some_and(|permission| permission.starts_with("mcp__orbit__"))),
        "project settings must include Orbit MCP permissions"
    );
    let mut user_settings = settings.clone();
    user_settings["permissions"]["allow"] = original_settings["permissions"]["allow"].clone();
    assert_eq!(
        user_settings, original_settings,
        "preserve unrelated settings"
    );
    assert_eq!(
        entries(&fixture.checkout),
        checkout_entries,
        "no MCP staging files left behind"
    );
    assert_eq!(
        entries(&settings_dir),
        settings_entries,
        "no settings staging files left behind"
    );
    #[cfg(unix)]
    {
        assert_eq!(
            serde_json::from_reader::<_, Value>(old_mcp).expect("read old MCP descriptor"),
            original_mcp
        );
        assert_eq!(
            serde_json::from_reader::<_, Value>(old_settings)
                .expect("read old settings descriptor"),
            original_settings
        );
    }
    fixture.assert_no_client_config_outside_the_checkout();

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "init", "--claude"]),
        )
        .success();
    assert_eq!(read_json(&fixture.claude_config()), expected_mcp);
    assert_eq!(
        read_json(&fixture.claude_settings()),
        settings,
        "init is idempotent"
    );
    assert_eq!(entries(&fixture.checkout), checkout_entries);
    assert_eq!(entries(&settings_dir), settings_entries);

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "remove", "--claude"]),
        )
        .success();

    assert_eq!(read_json(&fixture.claude_config()), original_mcp);
    assert_eq!(read_json(&fixture.claude_settings()), original_settings);
    assert_eq!(entries(&fixture.checkout), checkout_entries);
    assert_eq!(entries(&settings_dir), settings_entries);
}

fn mcp_init_resolves_the_registered_checkout_from_an_unrelated_directory() {
    let fixture = ExternalRootFixture::init();

    fixture
        .orbit(
            &fixture.elsewhere,
            &fixture.rooted(&["mcp", "init", "--claude"]),
        )
        .success();

    assert_eq!(
        generated_server_args(&fixture.claude_config()),
        expected_local_args(&fixture, "ws_wsname")
    );
    fixture.assert_no_client_config_outside_the_checkout();

    fixture
        .orbit(
            &fixture.elsewhere,
            &fixture.rooted(&["mcp", "remove", "--claude"]),
        )
        .success();

    assert!(!fixture.claude_config().exists());
    assert!(!fixture.checkout.join(".claude").exists());
}

fn explicit_root_outranks_a_different_registered_cwd_checkout() {
    let fixture = RegisteredCheckoutPairFixture::init();
    let root = fixture
        .checkout_b
        .join(".orbit")
        .canonicalize()
        .expect("canonicalize B Orbit root");

    fixture
        .orbit(
            &fixture.checkout_a,
            &argv(&[
                "--root",
                root.to_str().expect("utf8 B Orbit root"),
                "mcp",
                "init",
                "--claude",
            ]),
        )
        .success();

    assert!(!fixture.checkout_a.join(".mcp.json").exists());
    assert_eq!(
        generated_server_args(&fixture.checkout_b.join(".mcp.json")),
        vec![
            "mcp".to_string(),
            "serve".to_string(),
            "--workspace".to_string(),
            "ws_beta".to_string()
        ],
        "a repo-local .orbit is not the catalog, so the launch keeps the default root"
    );

    let env = [("ORBIT_ROOT", root.as_path())];
    fixture
        .orbit_with_env(
            &fixture.checkout_a,
            &argv(&["mcp", "remove", "--claude"]),
            &env,
        )
        .success();

    assert!(!fixture.checkout_a.join(".mcp.json").exists());
    assert!(!fixture.checkout_b.join(".mcp.json").exists());
}

fn shared_root_mcp_setup_uses_cwd_checkout_for_init_and_remove() {
    let fixture = SharedRootCheckoutPairFixture::init();
    let root = fixture.orbit_root.to_str().expect("utf8 shared Orbit root");

    let outside = fixture
        .orbit(
            &fixture.elsewhere,
            &argv(&["--root", root, "mcp", "init", "--claude"]),
        )
        .failure();
    let stderr = String::from_utf8_lossy(&outside.get_output().stderr);
    assert!(
        stderr.contains("does not identify exactly one registered checkout"),
        "unexpected outside-checkout failure: {stderr}"
    );
    assert!(!fixture.checkout_a.join(".mcp.json").exists());
    assert!(!fixture.checkout_b.join(".mcp.json").exists());

    fixture
        .orbit(
            &fixture.checkout_a,
            &argv(&["--root", root, "mcp", "init", "--claude"]),
        )
        .success();
    assert_eq!(
        generated_server_args(&fixture.checkout_a.join(".mcp.json")),
        expected_local_args_for_root(&fixture.orbit_root, "ws_alpha")
    );
    assert!(!fixture.checkout_b.join(".mcp.json").exists());

    fixture
        .orbit(
            &fixture.checkout_b,
            &argv(&["--root", root, "mcp", "init", "--claude"]),
        )
        .success();
    assert_eq!(
        generated_server_args(&fixture.checkout_b.join(".mcp.json")),
        expected_local_args_for_root(&fixture.orbit_root, "ws_beta")
    );

    fixture
        .orbit(
            &fixture.checkout_a,
            &argv(&["--root", root, "mcp", "remove", "--claude"]),
        )
        .success();
    assert!(!fixture.checkout_a.join(".mcp.json").exists());
    assert!(fixture.checkout_b.join(".mcp.json").exists());

    fixture
        .orbit(
            &fixture.checkout_b,
            &argv(&["--root", root, "mcp", "remove", "--claude"]),
        )
        .success();
    assert!(!fixture.checkout_b.join(".mcp.json").exists());
}

fn a_root_without_a_registered_checkout_refuses_instead_of_writing() {
    let fixture = ExternalRootFixture::init();
    let unrelated_root = fixture
        .orbit_root
        .parent()
        .expect("orbit root parent")
        .join("unregistered");
    fs::create_dir_all(&unrelated_root).expect("create unregistered root");

    let assert = fixture
        .orbit(
            &fixture.elsewhere,
            &argv(&[
                "--root",
                unrelated_root.to_str().expect("utf8 root"),
                "mcp",
                "init",
                "--claude",
            ]),
        )
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("does not identify exactly one registered checkout"),
        "unexpected failure message: {stderr}"
    );
    assert!(!unrelated_root.join(".mcp.json").exists());
    fixture.assert_no_client_config_outside_the_checkout();
    assert!(!fixture.claude_config().exists());
}

fn an_unregistered_root_reports_the_root_and_registered_candidates() {
    let fixture = RegisteredCheckoutPairFixture::init();
    let root = fixture.checkout_a.join("not-registered");
    let assert = fixture.orbit(
        &fixture.checkout_a,
        &argv(&[
            "--root",
            root.to_str().expect("utf8 unregistered root"),
            "mcp",
            "init",
            "--claude",
        ]),
    );
    let stderr = String::from_utf8_lossy(&assert.failure().get_output().stderr).to_string();

    assert!(
        stderr.contains(root.to_str().expect("utf8 unregistered root")),
        "failure must name the explicit root: {stderr}"
    );
    assert!(
        stderr.contains(fixture.checkout_a.to_str().expect("utf8 checkout A"))
            && stderr.contains(fixture.checkout_b.to_str().expect("utf8 checkout B")),
        "failure must name candidate checkouts: {stderr}"
    );
}

/// Symlink the home-scope client config at `config_relative` to a dotfiles
/// file seeded with `seed`. Returns the link and the file it points at.
#[cfg(unix)]
fn link_home_config_to_dotfiles(
    fixture: &ExternalRootFixture,
    config_relative: &str,
    seed: &str,
) -> (PathBuf, PathBuf) {
    let config = fixture.home.join(config_relative);
    let config_dir = config.parent().expect("config parent");
    let dotfiles = fixture.home.join("dotfiles");
    let dotfile = dotfiles.join("client-config");
    fs::create_dir_all(&dotfiles).expect("create dotfiles directory");
    fs::create_dir_all(config_dir).expect("create client directory");
    fs::write(&dotfile, seed).expect("seed dotfiles config");
    std::os::unix::fs::symlink(&dotfile, &config).expect("link client config into dotfiles");
    (config, dotfile)
}

/// Assert `config` is still the symlink to `dotfile`, and that neither its
/// directory nor the dotfiles directory holds a staging file.
#[cfg(unix)]
fn assert_config_still_linked(config: &Path, dotfile: &Path, step: &str) {
    let metadata = fs::symlink_metadata(config).expect("inspect client config");
    assert!(
        metadata.file_type().is_symlink(),
        "{step} replaced the symlinked config with a regular file"
    );
    assert_eq!(
        fs::read_link(config).expect("read config link"),
        dotfile,
        "{step} retargeted the config symlink"
    );
    for directory in [config.parent(), dotfile.parent()].into_iter().flatten() {
        assert_eq!(
            fs::read_dir(directory).expect("read directory").count(),
            1,
            "{step} left staging files in {}",
            directory.display()
        );
    }
}

/// A home-scope client config symlinked from a dotfiles repository keeps its
/// link: init and remove update the file the link points at.
#[cfg(unix)]
fn assert_home_scope_config_symlink_survives(
    client_flag: &str,
    config_relative: &str,
    seed: &str,
    other_entry: &str,
) {
    let fixture = ExternalRootFixture::init();
    let (config, dotfile) = link_home_config_to_dotfiles(&fixture, config_relative, seed);

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "init", client_flag, "--scope", "home"]),
        )
        .success();
    assert_config_still_linked(&config, &dotfile, "init");
    let initialized = fs::read_to_string(&dotfile).expect("read dotfiles config");
    assert!(
        initialized.contains("orbit") && initialized.contains(other_entry),
        "init must write Orbit's entry into the link target and keep the user's: {initialized}"
    );

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "remove", client_flag, "--scope", "home"]),
        )
        .success();
    assert_config_still_linked(&config, &dotfile, "remove");
    let removed = fs::read_to_string(&dotfile).expect("read dotfiles config");
    assert!(
        removed.contains(other_entry) && !removed.contains("orbit"),
        "remove must drop Orbit's entry from the link target and keep the user's: {removed}"
    );
}

/// Init, then remove, Orbit's home-scope entry through a link to an empty
/// dotfile, so the removal leaves the target with nothing in it. Returns the
/// target's content after removal.
#[cfg(unix)]
fn remove_last_entry_through_home_config_symlink(
    client_flag: &str,
    config_relative: &str,
) -> String {
    let fixture = ExternalRootFixture::init();
    let (config, dotfile) = link_home_config_to_dotfiles(&fixture, config_relative, "");

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "init", client_flag, "--scope", "home"]),
        )
        .success();
    assert_config_still_linked(&config, &dotfile, "init");

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "remove", client_flag, "--scope", "home"]),
        )
        .success();
    assert_config_still_linked(&config, &dotfile, "remove");
    fs::read_to_string(&dotfile).expect("read dotfiles config")
}

#[cfg(unix)]
fn json_client_home_config_symlink_survives_init_and_remove() {
    assert_home_scope_config_symlink_survives(
        "--cursor",
        ".cursor/mcp.json",
        r#"{"mcpServers":{"other":{"command":"other-client"}}}"#,
        "other-client",
    );
}

#[cfg(unix)]
fn toml_client_home_config_symlink_survives_init_and_remove() {
    assert_home_scope_config_symlink_survives(
        "--codex",
        ".codex/config.toml",
        "model = \"keep-me\"\n\n[mcp_servers.other]\ncommand = \"other-client\"\n",
        "other-client",
    );
}

#[cfg(unix)]
fn json_client_home_config_symlink_is_emptied_when_removal_leaves_no_entries() {
    let removed = remove_last_entry_through_home_config_symlink("--cursor", ".cursor/mcp.json");
    assert_eq!(
        serde_json::from_str::<Value>(&removed).expect("removed config is valid JSON"),
        serde_json::json!({}),
        "removing the last Orbit entry must leave the link target an empty object"
    );
}

#[cfg(unix)]
#[test]
fn toml_client_home_config_symlink_is_emptied_when_removal_leaves_no_entries() {
    let removed = remove_last_entry_through_home_config_symlink("--codex", ".codex/config.toml");
    let document: DocumentMut = removed.parse().expect("removed config is valid TOML");
    assert!(
        document.as_table().is_empty(),
        "removing the last Orbit entry must leave the link target an empty document: {removed}"
    );
}

fn toml_client_home_config_without_other_entries_is_deleted_when_removal_empties_it() {
    let fixture = ExternalRootFixture::init();
    let config = fixture.home.join(".codex").join("config.toml");

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "init", "--codex", "--scope", "home"]),
        )
        .success();
    assert!(config.is_file(), "init must create the Codex config");

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "remove", "--codex", "--scope", "home"]),
        )
        .success();
    assert!(
        !config.exists(),
        "a regular config that removal empties is deleted"
    );
}

struct SharedRootCheckoutPairFixture {
    _temp: TempDir,
    home: PathBuf,
    orbit_root: PathBuf,
    checkout_a: PathBuf,
    checkout_b: PathBuf,
    elsewhere: PathBuf,
}

impl SharedRootCheckoutPairFixture {
    fn init() -> Self {
        // `--root` is recorded as given, and the assertions expect the resolved
        // spelling, so root the fixture at a resolved temp directory.
        let temp =
            tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir()).expect("tempdir");
        let home = temp.path().join("home");
        let orbit_root = temp.path().join("shared-root");
        let checkout_a = temp.path().join("checkout-a");
        let checkout_b = temp.path().join("checkout-b");
        let elsewhere = temp.path().join("elsewhere");
        for directory in [&home, &elsewhere] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        crate::git_repo::SeededRepo::new(&checkout_a).build();
        crate::git_repo::SeededRepo::new(&checkout_b).build();

        let fixture = Self {
            _temp: temp,
            home,
            orbit_root,
            checkout_a,
            checkout_b,
            elsewhere,
        };
        let root = fixture.orbit_root.to_str().expect("utf8 shared Orbit root");
        fixture
            .orbit(
                &fixture.checkout_a,
                &argv(&[
                    "--root",
                    root,
                    "init",
                    "--non-interactive",
                    "--machine-name",
                    "shared-root-host",
                    "--task-prefix",
                    "SHR",
                ]),
            )
            .success();
        fixture
            .orbit(
                &fixture.checkout_a,
                &argv(&["--root", root, "workspace", "init", "--name", "alpha"]),
            )
            .success();
        fixture
            .orbit(
                &fixture.checkout_b,
                &argv(&["--root", root, "workspace", "init", "--name", "beta"]),
            )
            .success();
        fixture.assert_shared_root_layout();
        fixture
    }

    fn orbit(&self, cwd: &Path, args: &[String]) -> assert_cmd::assert::Assert {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env_remove("ORBIT_ROOT")
            .env_remove("ORBIT_WORKSPACE")
            .args(args)
            .assert()
    }

    fn assert_shared_root_layout(&self) {
        for (checkout, workspace_id) in [
            (&self.checkout_a, "ws_alpha"),
            (&self.checkout_b, "ws_beta"),
        ] {
            let assert = self
                .orbit(
                    checkout,
                    &argv(&[
                        "--root",
                        self.orbit_root.to_str().expect("utf8 shared Orbit root"),
                        "workspace",
                        "show",
                        "--format",
                        "json",
                    ]),
                )
                .success();
            let shown: Value =
                serde_json::from_slice(&assert.get_output().stdout).expect("workspace show json");
            assert_eq!(shown["workspace"]["id"], workspace_id);
            assert_eq!(shown["checkout"]["repo_root"], canonical_str(checkout));
            assert_eq!(
                shown["checkout"]["orbit_dir"],
                canonical_str(&self.orbit_root)
            );
            assert!(!checkout.join(".orbit").exists());
        }
    }
}

struct RegisteredCheckoutPairFixture {
    _temp: TempDir,
    home: PathBuf,
    checkout_a: PathBuf,
    checkout_b: PathBuf,
}

impl RegisteredCheckoutPairFixture {
    fn init() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let checkout_a = temp.path().join("checkout-a");
        let checkout_b = temp.path().join("checkout-b");
        fs::create_dir_all(&home).expect("fixture home");
        crate::git_repo::SeededRepo::new(&checkout_a).build();
        crate::git_repo::SeededRepo::new(&checkout_b).build();

        let fixture = Self {
            _temp: temp,
            home,
            checkout_a,
            checkout_b,
        };
        fixture
            .orbit(
                &fixture.checkout_a,
                &argv(&[
                    "init",
                    "--non-interactive",
                    "--machine-name",
                    "local-root-host",
                    "--task-prefix",
                    "LCL",
                ]),
            )
            .success();
        fixture
            .orbit(
                &fixture.checkout_a,
                &argv(&["workspace", "init", "--name", "alpha"]),
            )
            .success();
        fixture
            .orbit(
                &fixture.checkout_b,
                &argv(&["workspace", "init", "--name", "beta"]),
            )
            .success();
        fixture
    }

    fn orbit(&self, cwd: &Path, args: &[String]) -> assert_cmd::assert::Assert {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env_remove("ORBIT_ROOT")
            .env_remove("ORBIT_WORKSPACE")
            .args(args)
            .assert()
    }

    fn orbit_with_env(
        &self,
        cwd: &Path,
        args: &[String],
        env: &[(&str, &Path)],
    ) -> assert_cmd::assert::Assert {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env_remove("ORBIT_ROOT")
            .env_remove("ORBIT_WORKSPACE")
            .args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        command.assert()
    }
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

fn canonical_str(path: &Path) -> String {
    fs::canonicalize(path)
        .expect("canonicalize fixture path")
        .to_str()
        .expect("utf8 fixture path")
        .to_string()
}

fn canonical_root(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_str()
        .expect("utf8 root")
        .to_string()
}

fn expected_local_args_for_root(root: &Path, workspace: &str) -> Vec<String> {
    vec![
        "--root".to_string(),
        canonical_root(root),
        "mcp".to_string(),
        "serve".to_string(),
        "--workspace".to_string(),
        workspace.to_string(),
    ]
}

/// Args for an external-root fixture. A root that is the host-global
/// `~/.orbit` stays off the argv, matching a normal default-root launch.
fn expected_local_args(fixture: &ExternalRootFixture, workspace: &str) -> Vec<String> {
    let default_root = fixture.home.join(".orbit");
    if canonical_root(&fixture.orbit_root) == canonical_root(&default_root) {
        vec![
            "mcp".to_string(),
            "serve".to_string(),
            "--workspace".to_string(),
            workspace.to_string(),
        ]
    } else {
        expected_local_args_for_root(&fixture.orbit_root, workspace)
    }
}

fn toml_server_args(document: &str, server_id: &str) -> Vec<String> {
    let parsed: toml::Value = toml::from_str(document).expect("parse toml client config");
    parsed["mcp_servers"][server_id]["args"]
        .as_array()
        .expect("toml args")
        .iter()
        .map(|value| value.as_str().expect("toml arg").to_string())
        .collect()
}

fn read_registered_args(path: &Path, server_id: &str) -> Vec<String> {
    let raw =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    if path
        .extension()
        .is_some_and(|extension| extension == "toml")
    {
        return toml_server_args(&raw, server_id);
    }
    let config: Value = serde_json::from_str(&raw).expect("parse json client config");
    let servers = config
        .get("mcpServers")
        .or_else(|| config.get("servers"))
        .unwrap_or_else(|| panic!("no server table in {}", path.display()));
    servers[server_id]["args"]
        .as_array()
        .expect("json args")
        .iter()
        .map(|value| value.as_str().expect("json arg").to_string())
        .collect()
}

fn without_root_arg(args: &[String]) -> Vec<String> {
    let mut omitted = Vec::new();
    let mut skip_value = false;
    for arg in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        if arg == "--root" {
            skip_value = true;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--root=") {
            if value.is_empty() {
                skip_value = true;
            }
            continue;
        }
        omitted.push(arg.clone());
    }
    omitted
}

const MCP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// Speak one stdio MCP session to `orbit` launched with `args`.
///
/// `ORBIT_ROOT` is removed so the session can reach a custom catalog only
/// through the argv the registration emitted.
fn mcp_tool_result(home: &Path, cwd: &Path, args: &[String], tool: &str) -> Value {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("orbit"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_ROOT")
        .env_remove("ORBIT_WORKSPACE")
        .env_remove("RUST_LOG")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::child_guard::ChildGuard::spawn(&mut command)
        .expect("spawn the emitted orbit mcp serve");
    let mut stdin = child.stdin.take().expect("server stdin");
    let stdout = child.stdout.take().expect("server stdout");
    let stderr = child.stderr.take().expect("server stderr");
    let (stdout_tx, stdout_rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if stdout_tx.send(line).is_err() {
                break;
            }
        }
    });
    let (stderr_tx, stderr_rx) = channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if stderr_tx.send(line.clone()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let initialize = mcp_request(
        &mut stdin,
        &stdout_rx,
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "orbit-mcp-root-regression", "version": "0" }
        }),
    );
    assert!(
        initialize.get("error").is_none(),
        "initialize failed: {initialize}; stderr: {}",
        drain_stderr(&stderr_rx)
    );
    mcp_notify(&mut stdin, "notifications/initialized");
    let response = mcp_request(
        &mut stdin,
        &stdout_rx,
        2,
        "tools/call",
        json!({ "name": tool, "arguments": {} }),
    );
    drop(child);
    response.get("result").cloned().unwrap_or_else(|| {
        panic!(
            "tools/call returned no result: {response}; stderr: {}",
            drain_stderr(&stderr_rx)
        )
    })
}

fn mcp_notify(stdin: &mut dyn Write, method: &str) {
    let mut line = serde_json::to_string(&json!({ "jsonrpc": "2.0", "method": method }))
        .expect("serialize notification");
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .expect("write notification");
    stdin.flush().expect("flush notification");
}

fn mcp_request(
    stdin: &mut dyn Write,
    stdout: &Receiver<String>,
    id: i64,
    method: &str,
    params: Value,
) -> Value {
    let mut line = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
    .expect("serialize request");
    line.push('\n');
    stdin.write_all(line.as_bytes()).expect("write request");
    stdin.flush().expect("flush request");
    let started = std::time::Instant::now();
    loop {
        let remaining = MCP_RESPONSE_TIMEOUT.saturating_sub(started.elapsed());
        let line = stdout.recv_timeout(remaining).unwrap_or_else(|error| {
            panic!("no response to `{method}` within {MCP_RESPONSE_TIMEOUT:?}: {error}")
        });
        if line.trim().is_empty() {
            continue;
        }
        let response: Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|error| panic!("server emitted invalid JSON ({error}): {line}"));
        if response.get("id").and_then(Value::as_i64) == Some(id) {
            return response;
        }
    }
}

fn drain_stderr(stderr: &Receiver<String>) -> String {
    let mut lines = Vec::new();
    while let Ok(line) = stderr.try_recv() {
        lines.push(line);
    }
    lines.join("")
}

fn seed_operator_registration(path: &Path, args: &[&str], key: &str) {
    fs::create_dir_all(path.parent().expect("config parent")).expect("config directory");
    let content = if path
        .extension()
        .is_some_and(|extension| extension == "toml")
    {
        format!(
            "# preserve formatting\n[mcp_servers.orbit]\ncommand = '/custom/orbit'\nargs = {}\nenabled = true\n",
            serde_json::to_string(args).expect("seed args")
        )
    } else {
        json!({
            key: {
                "orbit": {
                    "command": "/custom/orbit",
                    "args": args,
                    "env": { "CUSTOM_SETTING": "keep" }
                }
            }
        })
        .to_string()
    };
    fs::write(path, content).expect("seed operator registration");
}

fn generated_server_args(config_path: &Path) -> Vec<String> {
    let config: Value = serde_json::from_str(
        &fs::read_to_string(config_path).expect("read generated client config"),
    )
    .expect("parse generated client config");
    config["mcpServers"]["orbit"]["args"]
        .as_array()
        .expect("generated args array")
        .iter()
        .map(|value| value.as_str().expect("arg is a string").to_string())
        .collect()
}

#[test]
fn emitted_external_root_launch_lists_tasks_without_inherited_orbit_root() {
    let fixture = ExternalRootFixture::init();
    // Grok's shared reader is on unless its config says otherwise. Force the
    // native TOML writer so this pass covers both config families.
    fs::create_dir_all(fixture.home.join(".grok")).expect("grok home");
    fs::write(
        fixture.home.join(".grok/config.toml"),
        "[claude_compat]\nimported = true\n",
    )
    .expect("force grok's native toml writer");

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "init", "--all"]),
        )
        .success();

    let expected = expected_local_args(&fixture, "ws_wsname");
    assert!(
        expected.first().is_some_and(|arg| arg == "--root"),
        "a non-default catalog must be on the launch: {expected:?}"
    );
    for client in McpClient::ALL {
        let paths = client_config_paths(
            McpClientScope::Workspace,
            client,
            &fixture.checkout,
            Some(&fixture.home),
        )
        .expect("client config paths");
        let args = read_registered_args(&paths.mcp_path, "orbit");
        assert_eq!(args, expected, "{client:?} registration");
        assert!(
            !args.iter().any(|arg| arg == "--operator"),
            "{client:?} mcp init must stay agent-only"
        );
        let raw = fs::read_to_string(&paths.mcp_path).expect("read registration");
        assert!(
            !raw.contains("ORBIT_ROOT"),
            "{client:?} must select the catalog by argv: {raw}"
        );
    }

    let emitted = read_registered_args(&fixture.claude_config(), "orbit");
    let listed = mcp_tool_result(
        &fixture.home,
        &fixture.checkout,
        &emitted,
        "orbit_task_list",
    );
    assert_eq!(listed["isError"], false, "task-list failed: {listed}");
    assert!(
        listed["structuredContent"]["tasks"].is_array(),
        "task-list did not reach the bound workspace: {listed}"
    );

    let omitted = without_root_arg(&emitted);
    let missed = mcp_tool_result(
        &fixture.home,
        &fixture.elsewhere,
        &omitted,
        "orbit_task_list",
    );
    assert_eq!(
        missed["isError"], true,
        "omitted root unexpectedly listed tasks: {missed}"
    );
    assert!(
        missed.to_string().contains("unknown workspace selector"),
        "omitted root must miss the external catalog: {missed}"
    );

    let operator_args = [
        "mcp",
        "serve",
        "--operator",
        "--workspace",
        "ws_existing",
        "--orchestrator",
        "custom",
    ];
    let claude = fixture.claude_config();
    let codex = fixture.checkout.join(".codex").join("config.toml");
    seed_operator_registration(&claude, &operator_args, "mcpServers");
    seed_operator_registration(&codex, &operator_args, "mcp_servers");
    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "init", "--claude", "--codex"]),
        )
        .success();
    let mut rooted_operator = vec!["--root".to_string(), canonical_root(&fixture.orbit_root)];
    rooted_operator.extend(operator_args.iter().map(|arg| (*arg).to_string()));
    let claude_config: Value =
        serde_json::from_slice(&fs::read(&claude).expect("read reconciled claude config"))
            .expect("parse reconciled claude config");
    assert_eq!(
        claude_config["mcpServers"]["orbit"]["command"],
        "/custom/orbit"
    );
    assert_eq!(
        claude_config["mcpServers"]["orbit"]["args"],
        json!(rooted_operator)
    );
    assert_eq!(
        claude_config["mcpServers"]["orbit"]["env"]["CUSTOM_SETTING"],
        "keep"
    );
    let codex_raw = fs::read_to_string(&codex).expect("read reconciled codex config");
    assert!(
        codex_raw.contains("# preserve formatting"),
        "reconciliation rewrote the operator document: {codex_raw}"
    );
    assert!(
        codex_raw.contains("/custom/orbit"),
        "reconciliation replaced the operator command: {codex_raw}"
    );
    assert_eq!(toml_server_args(&codex_raw, "orbit"), rooted_operator);

    let claude_bytes = fs::read(&claude).expect("snapshot claude config");
    let codex_bytes = fs::read(&codex).expect("snapshot codex config");
    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "init", "--claude", "--codex"]),
        )
        .success();
    assert_eq!(
        fs::read(&claude).expect("reread claude config"),
        claude_bytes,
        "repeat reconciliation must leave the rooted operator entry unchanged"
    );
    assert_eq!(
        fs::read(&codex).expect("reread codex config"),
        codex_bytes,
        "repeat reconciliation must leave the rooted operator entry unchanged"
    );

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "init", "--federated", "--codex"]),
        )
        .success();
    let federated = fs::read_to_string(&codex).expect("read federated codex config");
    assert_eq!(
        toml_server_args(&federated, "orbit-federated"),
        vec![
            "mcp".to_string(),
            "serve".to_string(),
            "--mode".to_string(),
            "federated".to_string()
        ],
        "federated registration must not take the local catalog: {federated}"
    );
    assert_eq!(
        toml_server_args(&federated, "orbit"),
        rooted_operator,
        "federated registration must leave the local operator entry in place"
    );

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "remove", "--all"]),
        )
        .success();
    fs::create_dir_all(fixture.checkout.join(".claude")).expect("detect claude");
    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["workspace", "init", "--name", "wsname", "--force", "--mcp"]),
        )
        .success();
    let mut operator_launch = expected_local_args(&fixture, "ws_wsname");
    let workspace_at = operator_launch
        .iter()
        .position(|arg| arg == "--workspace")
        .expect("workspace flag");
    operator_launch.insert(workspace_at, "--operator".to_string());
    assert_eq!(
        generated_server_args(&fixture.claude_config()),
        operator_launch,
        "workspace init --mcp must keep operator authority and the catalog root"
    );
}

#[test]
fn setup_root_expansion_and_config_confinement() {
    for (name, check) in [
        (
            "mcp_setup_expands_tilde_and_anchors_relative_roots_for_flags_and_environment",
            mcp_setup_expands_tilde_and_anchors_relative_roots_for_flags_and_environment as fn(),
        ),
        (
            "federated_home_setup_works_outside_a_workspace_and_reports_no_binding",
            federated_home_setup_works_outside_a_workspace_and_reports_no_binding as fn(),
        ),
    ] {
        tracing::info!(case = name, "MCP contract row");
        check();
    }
}

#[test]
fn federated_and_external_root_resolution() {
    for (name, check) in [
        (
            "federated_setup_inside_a_registered_checkout_reports_no_binding",
            federated_setup_inside_a_registered_checkout_reports_no_binding as fn(),
        ),
        (
            "mcp_init_resolves_the_registered_checkout_from_an_unrelated_directory",
            mcp_init_resolves_the_registered_checkout_from_an_unrelated_directory as fn(),
        ),
    ] {
        tracing::info!(case = name, "MCP contract row");
        check();
    }
}

#[test]
fn shared_and_explicit_root_precedence() {
    for (name, check) in [
        (
            "explicit_root_outranks_a_different_registered_cwd_checkout",
            explicit_root_outranks_a_different_registered_cwd_checkout as fn(),
        ),
        (
            "shared_root_mcp_setup_uses_cwd_checkout_for_init_and_remove",
            shared_root_mcp_setup_uses_cwd_checkout_for_init_and_remove as fn(),
        ),
        (
            "a_root_without_a_registered_checkout_refuses_instead_of_writing",
            a_root_without_a_registered_checkout_refuses_instead_of_writing as fn(),
        ),
    ] {
        tracing::info!(case = name, "MCP contract row");
        check();
    }
}

#[test]
fn missing_root_guidance_and_empty_config_removal() {
    for (name, check) in [
        (
            "an_unregistered_root_reports_the_root_and_registered_candidates",
            an_unregistered_root_reports_the_root_and_registered_candidates as fn(),
        ),
        (
            "toml_client_home_config_without_other_entries_is_deleted_when_removal_empties_it",
            toml_client_home_config_without_other_entries_is_deleted_when_removal_empties_it
                as fn(),
        ),
    ] {
        tracing::info!(case = name, "MCP contract row");
        check();
    }
}

#[cfg(unix)]
#[test]
fn symlinked_client_configuration() {
    for (name, check) in [
        (
            "json_client_home_config_symlink_survives_init_and_remove",
            json_client_home_config_symlink_survives_init_and_remove as fn(),
        ),
        (
            "toml_client_home_config_symlink_survives_init_and_remove",
            toml_client_home_config_symlink_survives_init_and_remove as fn(),
        ),
        (
            "json_client_home_config_symlink_is_emptied_when_removal_leaves_no_entries",
            json_client_home_config_symlink_is_emptied_when_removal_leaves_no_entries as fn(),
        ),
    ] {
        tracing::info!(case = name, "MCP contract row");
        check();
    }
}
