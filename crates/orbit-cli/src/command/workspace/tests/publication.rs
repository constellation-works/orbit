use clap::{CommandFactory, Parser};

use crate::command::workspace::WorkspacePublicationSubcommand;
use crate::command::{Cli, Commands};

use super::super::WorkspaceSubcommand;

#[test]
fn workspace_publication_help_covers_explicit_binding_lifecycle() {
    let command = Cli::command();
    let publication = command
        .find_subcommand("workspace")
        .and_then(|workspace| workspace.find_subcommand("publication"))
        .expect("workspace publication command");
    let help = publication.clone().render_long_help().to_string();
    for verb in ["bind", "show", "rebind", "remove"] {
        assert!(help.contains(verb), "missing {verb} from help:\n{help}");
    }
    let remove = publication
        .find_subcommand("remove")
        .expect("remove command")
        .clone()
        .render_long_help()
        .to_string();
    assert!(remove.contains("--confirm"), "{remove}");
}

#[test]
fn workspace_publication_json_forms_parse_for_every_surface() {
    for verb in ["bind", "rebind"] {
        let cli = Cli::try_parse_from([
            "orbit",
            "workspace",
            "publication",
            verb,
            "--remote",
            "ssh://publication.test/example.git",
            "--publication-id",
            "pub_example",
            "--json",
        ])
        .expect("parse publication binding mutation");
        let Commands::Workspace(workspace) = cli.command else {
            panic!("expected workspace command");
        };
        let WorkspaceSubcommand::Publication(publication) = workspace.command else {
            panic!("expected publication command");
        };
        assert!(matches!(
            publication.command,
            WorkspacePublicationSubcommand::Bind(_) | WorkspacePublicationSubcommand::Rebind(_)
        ));
    }

    for args in [
        vec!["orbit", "workspace", "publication", "show", "--json"],
        vec![
            "orbit",
            "workspace",
            "publication",
            "remove",
            "--confirm",
            "--json",
        ],
    ] {
        Cli::try_parse_from(args).expect("parse publication binding read/remove");
    }
}

// Run the coordinator and each writer in isolated child processes so runtime
// bootstrap cannot inherit the executor's task-store routing.
#[test]
fn catalog_writers_serialize_with_registration_and_removal() {
    use std::process::Command;
    const CHILD: &str = "ORBIT_CATALOG_LOCK_FIXTURE";
    const TEST: &str = "command::workspace::tests::publication::catalog_writers_serialize_with_registration_and_removal";
    if let Ok(mode) = std::env::var(CHILD) {
        if mode == "coordinator" {
            catalog_lock_coordinator();
        } else {
            catalog_lock_writer(&mode);
        }
        return;
    }
    let temp = tempfile::tempdir().expect("fixture");
    let mut child = Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    let output = child
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, "coordinator")
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .current_dir(temp.path())
        .output()
        .expect("isolated coordinator");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn catalog_lock_writer(mode: &str) {
    use crate::command::Execute;
    use orbit_core::{OrbitRuntime, WorkspaceRuntimeBinding};
    let root = std::path::PathBuf::from(std::env::var_os("ORBIT_CATALOG_FIXTURE_ROOT").unwrap());
    let runtime = OrbitRuntime::from_roots_with_binding(
        &root.join("global"),
        &root.join("repo/.orbit"),
        WorkspaceRuntimeBinding {
            logical_workspace_id: "ws_target".into(),
            task_partition_id: "ws_target".into(),
            owner_machine_id: Some("hm_owner".into()),
            repo_root: root.join("repo"),
            ship_mode: orbit_types::workflow::ShipMode::Pr,
            base_branch: None,
        },
    )
    .expect("runtime");
    let mut args = vec!["orbit", "workspace"];
    match mode {
        "remove" => args.extend(["remove", "target"]),
        "teardown" => args.extend(["teardown", "target", "--confirm"]),
        "role" => args.extend(["role", "target", "owner"]),
        "bind" | "rebind" => args.extend([
            "publication",
            mode,
            "--remote",
            "ssh://publication.test/tasks.git",
            "--publication-id",
            "pub_next",
        ]),
        "unbind" => args.extend(["publication", "remove", "--confirm"]),
        _ => panic!("unknown fixture mode"),
    }
    let cli = Cli::try_parse_from(args).expect("parse writer");
    let Commands::Workspace(command) = cli.command else {
        panic!("workspace")
    };
    std::fs::write(root.join("ready"), b"ready").expect("signal before execute");
    command.execute(&runtime).expect("execute writer");
}

fn catalog_lock_coordinator() {
    use chrono::Utc;
    use orbit_registry::workspace_registry as registry;
    use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceStatus};
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    for mode in ["remove", "teardown", "role", "bind", "rebind", "unbind"] {
        let temp = tempfile::tempdir().expect("case");
        let root = temp.path().canonicalize().expect("root");
        let global = root.join("global");
        std::fs::create_dir_all(&global).expect("global");
        std::fs::create_dir_all(root.join("repo/.orbit")).expect("checkout");
        std::fs::write(
            global.join("config.toml"),
            "[machine]\nid = \"hm_owner\"\nname = \"owner\"\ntask_prefix = \"ORB\"\n",
        )
        .expect("machine");
        let path = registry::registry_path_for(&global);
        let mut state = orbit_types::workspace::WorkspaceRegistry::default();
        for name in ["target", "removed"] {
            registry::register_workspace(
                &mut state,
                Workspace {
                    id: format!("ws_{name}"),
                    name: name.into(),
                    owner_machine_id: Some("hm_owner".into()),
                    git_remote: Some(format!("ssh://source.test/{name}.git")),
                    ship_mode: None,
                    base_branch: "main".into(),
                    status: WorkspaceStatus::Active,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            )
            .expect("register");
        }
        registry::register_checkout(
            &mut state,
            WorkspaceCheckout::owner(
                "ws_target".into(),
                root.join("repo"),
                root.join("repo/.orbit"),
            ),
        )
        .expect("checkout");
        if ["rebind", "unbind"].contains(&mode) {
            registry::bind_publication_by_id(
                &mut state,
                "ws_target",
                "ssh://publication.test/old.git",
                "main",
                "pub_old",
                Some("hm_owner"),
            )
            .expect("initial binding");
        }
        registry::save_registry_to(&state, &path).expect("save fixture");
        let mut child = registry::with_registry_lock(&path, || {
            let mut command = Command::new(std::env::current_exe().expect("test binary"));
            orbit_common::test_env::clear_inherited_authority(|name| { command.env_remove(name); });
            let mut child = command.args([
                "--exact",
                "command::workspace::tests::publication::catalog_writers_serialize_with_registration_and_removal",
                "--nocapture",
            ]).env("ORBIT_CATALOG_LOCK_FIXTURE", mode)
                .env("ORBIT_CATALOG_FIXTURE_ROOT", &root)
                .env("HOME", &root).env("USERPROFILE", &root)
                .current_dir(&root).spawn().expect("writer");
            let deadline = Instant::now() + Duration::from_secs(20);
            while !root.join("ready").exists() {
                assert!(child.try_wait().expect("status").is_none(), "{mode} exited before ready");
                assert!(Instant::now() < deadline, "{mode} never became ready");
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(200));
            assert!(child.try_wait().expect("status").is_none(),
                "{mode} must wait for the shared registry transaction");
            // Commit both an init-style registration and a removal while the
            // command is waiting. Its fresh read must preserve both.
            let mut current = registry::load_registry_from(&path)?;
            let mut added = current.workspaces[0].clone();
            added.id = "ws_added".into();
            added.name = "added".into();
            registry::register_workspace(&mut current, added)?;
            registry::remove_workspace(&mut current, "ws_removed")?;
            registry::save_registry_to(&current, &path)?;
            Ok(child)
        }).expect("competing transaction");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().expect("writer status") {
                assert!(status.success(), "{mode} failed");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().expect("stop deadlocked fixture");
                child.wait().expect("reap fixture");
                panic!("{mode} deadlocked");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let current = registry::load_registry_from(&path).expect("result");
        assert!(
            registry::find_workspace_by_id(&current, "ws_added").is_some(),
            "{mode} lost registration"
        );
        assert!(
            registry::find_workspace_by_id(&current, "ws_removed").is_none(),
            "{mode} resurrected removal"
        );
        match mode {
            "remove" | "teardown" => {
                assert!(registry::find_workspace_by_id(&current, "ws_target").is_none())
            }
            "bind" | "rebind" => assert_eq!(
                registry::find_publication_binding_by_id(&current, "ws_target")
                    .unwrap()
                    .publication_id,
                "pub_next"
            ),
            "unbind" => {
                assert!(registry::find_publication_binding_by_id(&current, "ws_target").is_none())
            }
            "role" => assert_eq!(
                registry::find_checkout_by_id(&current, "ws_target")
                    .unwrap()
                    .role,
                Some(orbit_types::workspace::WorkspaceCheckoutRole::Owner)
            ),
            _ => unreachable!(),
        }
        if mode == "teardown" {
            assert!(!root.join("repo/.orbit").exists());
            assert!(
                orbit_cmd::bound_partition_id(&global, &root.join("repo/.orbit"))
                    .unwrap()
                    .is_none()
            );
        }
    }
}
