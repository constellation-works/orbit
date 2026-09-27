use clap::{CommandFactory, Parser};

use crate::command::task::TaskPublicationSubcommand;
use crate::command::{Cli, Commands};

use super::super::TaskSubcommand;

#[test]
fn task_publication_help_covers_the_complete_operator_lifecycle() {
    let command = Cli::command();
    let publication = command
        .find_subcommand("task")
        .and_then(|task| task.find_subcommand("publication"))
        .expect("task publication command");
    let help = publication.clone().render_long_help().to_string();
    for verb in ["publish", "status", "inspect", "restore"] {
        assert!(help.contains(verb), "missing {verb} from help:\n{help}");
    }

    let publish = publication
        .find_subcommand("publish")
        .expect("publish command")
        .clone()
        .render_long_help()
        .to_string();
    assert!(publish.contains("safe default"), "{publish}");
    assert!(publish.contains("allow-unscanned-attachments"), "{publish}");

    let restore = publication
        .find_subcommand("restore")
        .expect("restore command")
        .clone()
        .render_long_help()
        .to_string();
    assert!(restore.contains("--confirm"), "{restore}");
    assert!(restore.contains("--allow-identical-retry"), "{restore}");
}

#[test]
fn task_publication_json_forms_parse_for_every_surface() {
    for verb in ["publish", "status"] {
        let cli = Cli::try_parse_from(["orbit", "task", "publication", verb, "--json"])
            .expect("parse owner publication command");
        let Commands::Task(task) = cli.command else {
            panic!("expected task command");
        };
        let TaskSubcommand::Publication(_) = task.command else {
            panic!("expected task publication command");
        };
    }

    let pairing = [
        "--workspace-id",
        "ws_example",
        "--source-remote",
        "ssh://source.test/example.git",
        "--publication-id",
        "pub_example",
        "--authority-machine-id",
        "hm_example",
        "--remote",
        "ssh://publication.test/example.git",
    ];
    for verb in ["inspect", "restore"] {
        let mut args = vec!["orbit", "task", "publication", verb];
        args.extend(pairing);
        if verb == "restore" {
            args.push("--confirm");
        }
        args.push("--json");
        let cli = Cli::try_parse_from(args).expect("parse consumer publication command");
        let Commands::Task(task) = cli.command else {
            panic!("expected task command");
        };
        let TaskSubcommand::Publication(publication) = task.command else {
            panic!("expected task publication command");
        };
        assert!(matches!(
            publication.command,
            TaskPublicationSubcommand::Inspect(_) | TaskPublicationSubcommand::Restore(_)
        ));
    }
}

#[test]
fn publication_completion_reloads_and_revalidates_catalog() {
    const CHILD: &str = "ORBIT_PUBLICATION_COMPLETION_FIXTURE";
    if std::env::var_os(CHILD).is_none() {
        let temp = tempfile::tempdir().expect("fixture home");
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"));
        orbit_common::test_env::clear_inherited_authority(|name| {
            child.env_remove(name);
        });
        let output = child.args([
            "--exact",
            "command::task::tests::publication::publication_completion_reloads_and_revalidates_catalog",
            "--nocapture",
        ]).env(CHILD, "1").env("HOME", temp.path()).env("USERPROFILE", temp.path())
            .current_dir(temp.path()).output().expect("isolated fixture");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use super::super::publication::record_success_at_registry_path;
    use chrono::Utc;
    use orbit_core::bootstrap::task_publication::{
        PublicationPublishOutcome, PublicationPublishStatus,
    };
    use orbit_registry::workspace_registry as registry;
    use orbit_types::workspace::{
        Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus,
    };

    for mutation in [
        "unrelated",
        "rebind",
        "unbind",
        "remove",
        "newer-success",
        "same-success",
    ] {
        let root = tempfile::tempdir().expect("case");
        let path = root.path().join("workspaces.json");
        std::fs::write(
            root.path().join("config.toml"),
            "[machine]\nid = \"hm_owner\"\nname = \"owner\"\ntask_prefix = \"ORB\"\n",
        )
        .expect("machine");
        let mut state = WorkspaceRegistry::default();
        registry::register_workspace(
            &mut state,
            Workspace {
                id: "ws_target".into(),
                name: "target".into(),
                owner_machine_id: Some("hm_owner".into()),
                git_remote: Some("ssh://source.test/repo.git".into()),
                ship_mode: None,
                base_branch: "main".into(),
                status: WorkspaceStatus::Active,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        )
        .expect("workspace");
        registry::register_checkout(
            &mut state,
            WorkspaceCheckout::owner(
                "ws_target".into(),
                root.path().join("repo"),
                root.path().join("repo/.orbit"),
            ),
        )
        .expect("checkout");
        let expected = registry::bind_publication_by_id(
            &mut state,
            "ws_target",
            "ssh://publication.test/tasks.git",
            "main",
            "pub_original",
            Some("hm_owner"),
        )
        .expect("binding");
        registry::save_registry_to(&state, &path).expect("initial catalog");
        let outcome = PublicationPublishOutcome {
            status: PublicationPublishStatus::Initialized,
            branch: "refs/heads/main".into(),
            commit_id: "a".repeat(40),
            generation: 1,
            previous_publication: None,
            observed_tip: None,
            included_attachment_bytes: 0,
            omitted_attachment_bytes: 0,
        };
        // These commits occur after publish captured its binding and before
        // its external operation returns.
        registry::with_registry_lock(&path, || {
            let mut current = registry::load_registry_from(&path)?;
            match mutation {
                "unrelated" => {
                    let mut added = current.workspaces[0].clone();
                    added.id = "ws_added".into();
                    added.name = "added".into();
                    registry::register_workspace(&mut current, added)?;
                }
                "rebind" => {
                    registry::rebind_publication_by_id(
                        &mut current,
                        "ws_target",
                        "ssh://publication.test/other.git",
                        "main",
                        "pub_replaced",
                        Some("hm_owner"),
                    )?;
                }
                "unbind" => {
                    registry::unbind_publication_by_id(
                        &mut current,
                        "ws_target",
                        Some("hm_owner"),
                    )?;
                }
                "remove" => {
                    registry::remove_workspace(&mut current, "ws_target")?;
                }
                "newer-success" | "same-success" => {
                    let generation = if mutation == "newer-success" { 2 } else { 1 };
                    registry::record_publication_success_by_id(
                        &mut current,
                        "ws_target",
                        generation,
                        &outcome.commit_id,
                        Some("hm_owner"),
                    )?;
                }
                _ => unreachable!(),
            }
            registry::save_registry_to(&current, &path)
        })
        .expect("intervening commit");
        let before = std::fs::read(&path).expect("committed bytes");
        let result = record_success_at_registry_path(&path, &expected, &outcome);
        if ["unrelated", "same-success"].contains(&mutation) {
            result.expect("record success on unchanged destination");
            let current = registry::load_registry_from(&path).expect("result");
            let binding = registry::find_publication_binding_by_id(&current, "ws_target").unwrap();
            assert_eq!(binding.last_success_generation, Some(1));
            assert_eq!(
                binding.last_success_commit.as_deref(),
                Some(outcome.commit_id.as_str())
            );
            if mutation == "unrelated" {
                assert!(registry::find_workspace_by_id(&current, "ws_added").is_some());
            }
        } else {
            assert!(result.is_err(), "{mutation} must refuse stale completion");
            assert_eq!(
                std::fs::read(&path).expect("preserved catalog"),
                before,
                "{mutation}"
            );
        }
    }
}
