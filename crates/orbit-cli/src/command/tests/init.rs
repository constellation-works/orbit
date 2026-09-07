use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use orbit_common::fs::io::create_dir_symlink;
use orbit_core::OrbitRuntime;

use crate::InitCommand;
use crate::command::executor::ExecutorShowArgs;
use crate::command::{CommandOutput, Execute};
use crate::tests::env_isolation::EnvGuard;

fn seed_discovery_sentinel(home: &Path, agent_dir: &str) -> (PathBuf, PathBuf) {
    let target = home.join("live-sentinels").join(agent_dir).join("orbit");
    fs::create_dir_all(&target).expect("create sentinel target");
    let link = home.join(agent_dir).join("skills").join("orbit");
    fs::create_dir_all(link.parent().expect("sentinel link parent"))
        .expect("create sentinel link parent");
    create_dir_symlink(&target, &link).expect("create sentinel discovery link");
    (link, target)
}

fn assert_discovery_sentinel(link: &Path, target: &Path) {
    assert_eq!(
        fs::read_link(link).expect("read sentinel discovery link"),
        target,
        "temporary-root validation must not rewrite live skill discovery links",
    );
}

/// End-to-end: initializing against a non-global (temporary/custom) orbit
/// root must leave the invoking account's home-scoped skill link
/// directories untouched entirely — no removal, no re-creation, no
/// replacement — while it still produces a fresh config.toml with generated
/// crew tables in the custom root.
#[test]
fn non_interactive_init_against_non_global_root_leaves_home_skill_links_untouched() {
    let live_home = tempdir().expect("live home tempdir");
    let validation_home = tempdir().expect("validation home tempdir");
    let validation_root = tempdir().expect("validation root tempdir");
    // Detection reads the real `PATH` otherwise, so the seeded crews below
    // would depend on which agent CLIs this developer has installed.
    let empty_path = tempdir().expect("empty PATH tempdir");

    let env = EnvGuard::acquire()
        .home(live_home.path())
        .path(empty_path.path());
    let (agents_link, agents_target) = seed_discovery_sentinel(live_home.path(), ".agents");
    let (claude_link, claude_target) = seed_discovery_sentinel(live_home.path(), ".claude");

    let outcome = env.with_home(validation_home.path(), || {
        InitCommand {
            force: false,
            non_interactive: true,
            host_name: Some("validation-host".to_string()),
            task_prefix: Some("VA".to_string()),
        }
        .execute_without_runtime(Some(&validation_root.path().join(".orbit")))
    });

    outcome.expect("init succeeded");

    // Non-interactive init created the machine identity in the isolated root.
    let host_toml = validation_root.path().join(".orbit").join("host.toml");
    let host_contents = fs::read_to_string(&host_toml).expect("read host.toml");
    assert!(
        host_contents.contains("schema_version = 2"),
        "{host_contents}"
    );
    assert!(
        host_contents.contains("host_id = \"validation-host\""),
        "{host_contents}"
    );
    assert!(
        host_contents.contains("task_prefix = \"VA\""),
        "{host_contents}"
    );
    assert!(!host_contents.contains("mode ="), "{host_contents}");

    assert_discovery_sentinel(&agents_link, &agents_target);
    assert_discovery_sentinel(&claude_link, &claude_target);
    assert!(
        !validation_home
            .path()
            .join(".agents")
            .join("skills")
            .join("orbit")
            .exists(),
        "a non-global root must not create home-scoped skill links"
    );
    assert!(
        !validation_home
            .path()
            .join(".claude")
            .join("skills")
            .join("orbit")
            .exists(),
        "a non-global root must not create home-scoped skill links"
    );

    let config_path = validation_root.path().join(".orbit").join("config.toml");
    let contents = fs::read_to_string(&config_path).expect("read config");
    for line in contents.lines() {
        assert!(
            !line.trim_start().starts_with("[agent."),
            "unexpected uncommented agent section: {line}",
        );
    }
    let config = toml::from_str::<toml::Value>(&contents).expect("seeded config parses");
    let crews = config.get("crews").and_then(toml::Value::as_table);
    let expected = [
        ("opus", "claude", "opus"),
        ("sonnet", "claude", "sonnet"),
        ("fable", "claude", "fable"),
        ("sol", "codex", "gpt-5.6-sol"),
        ("terra", "codex", "gpt-5.6-terra"),
        ("luna", "codex", "gpt-5.6-luna"),
        ("gemini", "gemini", "gemini-3.7-flash"),
        ("grok", "grok", "grok-4.6"),
        ("system", "", ""),
    ];
    if let Some(crews) = crews {
        assert!(!crews.contains_key("claude"));
        assert!(!crews.contains_key("codex"));
        for (name, crew) in crews {
            let (_, provider, model) = expected
                .iter()
                .find(|(expected_name, _, _)| expected_name == name)
                .unwrap_or_else(|| panic!("unexpected seeded crew {name}"));
            // [ORB-10801] Seeded crews carry no retired backend key.
            assert!(crew.get("backend").is_none());
            if name == "system" {
                // Preference order: codex luna, then claude sonnet, then grok,
                // then gemini flash. Cheapest tier per family, not the
                // family default.
                let (provider, model) = if crews.contains_key("luna") {
                    ("codex", "gpt-5.6-luna")
                } else if crews.contains_key("sonnet") {
                    ("claude", "sonnet")
                } else if crews.contains_key("grok") {
                    ("grok", "grok-4.6")
                } else {
                    ("gemini", "gemini-3.7-flash")
                };
                assert_eq!(
                    crew.get("provider").and_then(toml::Value::as_str),
                    Some(provider),
                );
                assert_eq!(crew.get("model").and_then(toml::Value::as_str), Some(model),);
            } else {
                assert_eq!(
                    crew.get("provider").and_then(toml::Value::as_str),
                    Some(*provider),
                );
                assert_eq!(
                    crew.get("model").and_then(toml::Value::as_str),
                    Some(*model),
                );
            }
        }
    }
    assert!(!contents.contains("[crews.qa]"));
    let default_crew = config
        .get("workflow")
        .and_then(|workflow| workflow.get("default_crew"))
        .and_then(toml::Value::as_str);
    assert_eq!(
        default_crew.is_some(),
        crews.is_some_and(|crews| !crews.is_empty()),
    );
    if let Some(default_crew) = default_crew {
        assert!(crews.is_some_and(|crews| crews.contains_key(default_crew)));
    }
    drop(validation_root);
    drop(validation_home);
    assert_discovery_sentinel(&agents_link, &agents_target);
    assert_discovery_sentinel(&claude_link, &claude_target);
}

/// `--force` replaces a legacy config instead of carrying its compatibility
/// crew forward into a newly generated configuration.
#[test]
fn forced_non_interactive_init_does_not_regenerate_legacy_qa_crew() {
    let home = tempdir().expect("home tempdir");
    let _env = EnvGuard::acquire().home(home.path());
    let root = home.path().join(".orbit");

    init_host(&root, Some("force-host"), Some("FC")).expect("initial init");
    fs::write(
        root.join("config.toml"),
        "[crews.qa]\nprovider = \"codex\"\nmodel = \"gpt-5.6-terra\"\n",
    )
    .expect("write legacy config");

    InitCommand {
        force: true,
        non_interactive: true,
        host_name: Some("force-host".to_string()),
        task_prefix: Some("FC".to_string()),
    }
    .execute_without_runtime(Some(&root))
    .expect("forced init");

    let contents = fs::read_to_string(root.join("config.toml")).expect("read regenerated config");
    assert!(!contents.contains("[crews.qa]"), "{contents}");
}

fn init_host(
    root: &Path,
    host_name: Option<&str>,
    task_prefix: Option<&str>,
) -> Result<(), orbit_core::OrbitError> {
    InitCommand {
        force: false,
        non_interactive: true,
        host_name: host_name.map(str::to_string),
        task_prefix: task_prefix.map(str::to_string),
    }
    .execute_without_runtime(Some(root))
    .map(|_| ())
}

/// Non-interactive `--host-name` + `--task-prefix` create the identity exactly
/// once, and a repeat init preserves the generated machine_id unchanged.
#[test]
fn non_interactive_host_name_and_task_prefix_create_then_repeat_is_stable() {
    let home = tempdir().expect("home tempdir");
    let _env = EnvGuard::acquire().home(home.path());
    let root = home.path().join(".orbit");

    init_host(&root, Some("dk-mac"), Some("DE")).expect("first init");
    let host_toml = root.join("host.toml");
    let first = fs::read_to_string(&host_toml).expect("read host.toml");
    assert!(first.contains("host_id = \"dk-mac\""), "{first}");
    assert!(first.contains("schema_version = 2"), "{first}");
    assert!(first.contains("task_prefix = \"DE\""), "{first}");
    assert!(!first.contains("mode ="), "{first}");
    let machine_line = first
        .lines()
        .find(|line| line.starts_with("machine_id = "))
        .expect("machine_id line")
        .to_string();
    assert!(machine_line.contains("hm_"), "{machine_line}");

    // Repeated init: no prompt, no rewrite, identical machine_id.
    init_host(&root, Some("ignored-on-repeat"), Some("ZZ")).expect("repeat init");
    let second = fs::read_to_string(&host_toml).expect("re-read host.toml");
    assert_eq!(first, second, "repeat init must not rewrite host.toml");
}

/// A schema-v1 identity with an existing task sequence migrates in place on init.
#[test]
fn init_migrates_legacy_host_toml() {
    let home = tempdir().expect("home tempdir");
    let _env = EnvGuard::acquire().home(home.path());
    let root = home.path().join(".orbit");
    fs::create_dir_all(&root).expect("mkdir .orbit");
    fs::create_dir_all(root.join("tasks")).expect("mkdir tasks");
    fs::write(root.join("tasks/index.sqlite"), []).expect("seed task sequence");
    fs::write(
        root.join("host.toml"),
        "schema_version = 1\nmachine_id = \"hm_existing\"\nhost_id = \"legacy-host\"\nmode = \"hub\"\n",
    )
    .expect("seed legacy");

    // No --host-name needed: migration preserves the legacy name.
    init_host(&root, None, None).expect("migrating init");
    let migrated = fs::read_to_string(root.join("host.toml")).expect("read migrated");
    assert!(migrated.contains("schema_version = 2"), "{migrated}");
    assert!(
        migrated.contains("machine_id = \"hm_existing\""),
        "{migrated}"
    );
    assert!(migrated.contains("host_id = \"legacy-host\""), "{migrated}");
    assert!(migrated.contains("task_prefix = \"ORB\""), "{migrated}");
    assert!(!migrated.contains("mode ="), "{migrated}");

    let before = fs::read(root.join("host.toml")).expect("read migrated bytes");
    init_host(&root, None, None).expect("repeat init");
    assert_eq!(
        fs::read(root.join("host.toml")).expect("reread migrated bytes"),
        before
    );
}

/// A fresh host initialized non-interactively without --host-name fails closed.
#[test]
fn non_interactive_missing_host_name_fails_closed() {
    let home = tempdir().expect("home tempdir");
    let _env = EnvGuard::acquire().home(home.path());
    let root = home.path().join(".orbit");

    let error = init_host(&root, None, Some("DE")).expect_err("missing host name must fail closed");
    assert!(error.to_string().contains("--host-name"), "{error}");
    assert!(
        !root.join("host.toml").exists(),
        "no identity should be written on the failure path"
    );
}

/// A fresh non-interactive init requires an explicit task prefix.
#[test]
fn non_interactive_missing_task_prefix_fails_closed() {
    let home = tempdir().expect("home tempdir");
    let _env = EnvGuard::acquire().home(home.path());
    let root = home.path().join(".orbit");

    let error = init_host(&root, Some("dk-mac"), None).expect_err("missing prefix must fail");
    assert!(error.to_string().contains("--task-prefix"), "{error}");
    assert!(!root.join("host.toml").exists());
}

/// Reserved and malformed fresh task-prefix choices fail before identity write.
#[test]
fn invalid_task_prefixes_fail_closed() {
    for prefix in ["ORB", "ADR", "L", "F", "de", "D", "ABCDEF", " DE"] {
        let home = tempdir().expect("home tempdir");
        let _env = EnvGuard::acquire().home(home.path());
        let root = home.path().join(".orbit");

        init_host(&root, Some("dk-mac"), Some(prefix)).expect_err("invalid prefix must fail");
        assert!(!root.join("host.toml").exists(), "prefix {prefix}");
    }
}

/// Ordinary CLI `orbit init` (refresh_defaults, no --force) must keep an
/// operator `spec.sandbox: off`. `--force` may still restore shipped defaults.
#[test]
fn non_interactive_init_preserves_explicit_sandbox_off_without_force() {
    let home = tempdir().expect("home tempdir");
    let empty_path = tempdir().expect("empty PATH tempdir");
    let _env = EnvGuard::acquire()
        .home(home.path())
        .path(empty_path.path());
    let root = home.path().join(".orbit");

    init_host(&root, Some("sandbox-off"), Some("SO")).expect("first init");
    set_executor_sandbox(&root, "grok", Some("off"));
    assert_eq!(show_executor_sandbox(&root, "grok"), "off");

    init_host(&root, Some("sandbox-off"), Some("SO")).expect("repeat init without --force");
    assert_eq!(
        show_executor_sandbox(&root, "grok"),
        "off",
        "ordinary orbit init must not restore shipped sandbox over explicit off"
    );

    InitCommand {
        force: true,
        non_interactive: true,
        host_name: Some("sandbox-off".to_string()),
        task_prefix: Some("SO".to_string()),
    }
    .execute_without_runtime(Some(&root))
    .expect("forced init");
    assert_eq!(
        show_executor_sandbox(&root, "grok"),
        shipped_sandbox_json(),
        "orbit init --force may reset executor sandbox to the shipped default"
    );

    if cfg!(target_os = "linux") {
        set_executor_sandbox(&root, "grok", None);
        init_host(&root, Some("sandbox-off"), Some("SO")).expect("init after omitting sandbox");
        assert_eq!(
            show_executor_sandbox(&root, "grok"),
            "linux-bwrap",
            "omitted/null sandbox on a Linux shipped default still migrates to linux-bwrap"
        );
    }
}

fn set_executor_sandbox(root: &Path, name: &str, sandbox: Option<&str>) {
    let path = root
        .join("resources/executors")
        .join(format!("{name}.yaml"));
    let mut resource: serde_yaml::Value =
        serde_yaml::from_str(&fs::read_to_string(&path).expect("read executor YAML"))
            .expect("parse executor YAML");
    let spec = resource
        .get_mut("spec")
        .and_then(serde_yaml::Value::as_mapping_mut)
        .expect("executor spec mapping");
    match sandbox {
        Some(value) => {
            spec.insert(
                serde_yaml::Value::String("sandbox".to_string()),
                serde_yaml::Value::String(value.to_string()),
            );
        }
        None => {
            spec.remove(serde_yaml::Value::String("sandbox".to_string()));
        }
    }
    fs::write(
        &path,
        serde_yaml::to_string(&resource).expect("encode executor YAML"),
    )
    .expect("write executor YAML");
}

fn show_executor_sandbox(global_root: &Path, name: &str) -> serde_json::Value {
    let workspace = global_root
        .parent()
        .expect("global root parent")
        .join("repo/.orbit");
    fs::create_dir_all(&workspace).expect("workspace root");
    let runtime = OrbitRuntime::from_roots(global_root, &workspace).expect("runtime from roots");
    let CommandOutput::Payload(payload) = ExecutorShowArgs {
        name: name.to_string(),
        json: true,
    }
    .execute(&runtime)
    .expect("executor show") else {
        panic!("executor show must return a payload");
    };
    let (json, _) = payload.into_view();
    json["sandbox"].clone()
}

fn shipped_sandbox_json() -> serde_json::Value {
    match std::env::consts::OS {
        "linux" => serde_json::Value::String("linux-bwrap".to_string()),
        "macos" => serde_json::Value::String("macos-sandbox-exec".to_string()),
        _ => serde_json::Value::Null,
    }
}
