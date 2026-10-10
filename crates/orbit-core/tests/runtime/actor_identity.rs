//! Process-envelope attribution through runtime bootstrap and a persisted task write.
//! Each case runs in a child so environment and mutable Orbit state stay isolated.

use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use orbit_core::application::task::TaskAddParams;
use orbit_core::context::ActorKind;
use orbit_core::{ActorIdentity, OrbitRuntime};
use tracing::{Event, Subscriber};
use tracing_subscriber::{Layer, layer::Context, prelude::*};

type ActorCase<'a> = (&'a [(&'a str, &'a str)], &'a str);

#[test]
fn agent_envelopes_never_fall_back_to_human_attribution() {
    let cases: &[ActorCase<'_>] = &[
        (
            &[
                ("ORBIT_AGENT_NAME", "copilot"),
                ("ORBIT_AGENT_MODEL", "claude-sonnet-5"),
            ],
            "unknown",
        ),
        (
            &[
                ("ORBIT_AGENT_NAME", "claude"),
                ("ORBIT_AGENT_MODEL", "gpt-5"),
            ],
            "unknown",
        ),
        (&[("ORBIT_AGENT_NAME", "agy")], "unknown"),
        (&[("ORBIT_AGENT_NAME", "cursor")], "unknown"),
        (&[("ORBIT_AGENT_MODEL", "unrecognized-model")], "unknown"),
        (&[("ORBIT_MANAGED_RUN_CONTEXT", "1")], "unknown"),
        (&[("ORBIT_TASK_ACTOR_KIND", "agent")], "unknown"),
        (
            &[
                ("ORBIT_AGENT_NAME", "copilot"),
                ("ORBIT_ACTOR", "named-human"),
                ("ORBIT_OPERATOR", "1"),
            ],
            "unknown",
        ),
        (
            &[
                ("ORBIT_MANAGED_RUN_CONTEXT", "1"),
                ("ORBIT_ACTOR", "named-human"),
                ("ORBIT_OPERATOR", "1"),
            ],
            "unknown",
        ),
        (
            &[
                ("ORBIT_AGENT_NAME", "claude"),
                ("ORBIT_AGENT_MODEL", "claude-sonnet-5"),
            ],
            "claude",
        ),
        (&[("ORBIT_AGENT_MODEL", "gpt-5")], "codex"),
        (&[], "human:fixture-user"),
        (&[("ORBIT_ACTOR", "named-human")], "named-human"),
        (&[("ORBIT_OPERATOR", "1")], "operator"),
    ];
    let child = "actor_identity::process_actor_is_used_for_task_attribution";
    for (env, expected) in cases {
        let mut command = Command::new(std::env::current_exe().unwrap());
        orbit_common::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        command
            .args(["--exact", child, "--ignored", "--nocapture"])
            .env("USER", "fixture-user")
            .env("USERNAME", "fixture-user")
            .env("LOGNAME", "fixture-user")
            .env("ORBIT_TEST_EXPECTED_ACTOR", expected)
            .envs(env.iter().copied());
        let output = command.output().unwrap();
        orbit_common::test_env::assert_child_test_passed(
            child,
            output.status,
            &output.stdout,
            &output.stderr,
        );
    }
}

struct ActorWarnings(Arc<AtomicUsize>);

impl<S: Subscriber> Layer<S> for ActorWarnings {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() == "orbit.core.actor"
            && *event.metadata().level() == tracing::Level::WARN
        {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[test]
#[ignore = "isolated child entry point"]
fn process_actor_is_used_for_task_attribution() {
    let expected = std::env::var("ORBIT_TEST_EXPECTED_ACTOR").unwrap();
    let warnings = Arc::new(AtomicUsize::new(0));
    let subscriber = tracing_subscriber::registry().with(ActorWarnings(warnings.clone()));
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let actor = ActorIdentity::from_env();
    assert_eq!(actor.label, expected);
    let expected_kind = match expected.as_str() {
        "unknown" => ActorKind::Unknown,
        "claude" | "codex" => ActorKind::Agent,
        _ => ActorKind::Human,
    };
    assert_eq!(actor.kind, expected_kind);
    assert_eq!(warnings.load(Ordering::SeqCst) > 0, expected == "unknown");

    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Process actor attribution fixture".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(task.created_by.as_deref(), Some(expected.as_str()));
    assert_eq!(
        runtime.get_task(&task.id).unwrap().created_by,
        task.created_by
    );
}
