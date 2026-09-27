use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_tools::plugin::BrokeredCaller;
use orbit_types::workflow::ExecutorSandboxKind;

use super::super::super::dispatcher::ResolvedSandbox;
use super::super::plugin_broker::RunPluginBroker;
use super::test_support::{capture_events, sandbox_for_test};
use crate::context::{PLUGIN_BROKER_ENV, PluginBrokerHandle, PluginBrokerRun, RuntimeHost};

/// What the host saw of its broker.
#[derive(Default)]
struct Observed {
    started_for: Mutex<Vec<String>>,
    bound_to: Mutex<Vec<u32>>,
    dropped: AtomicBool,
}

struct FakeBroker {
    socket: PathBuf,
    bind_error: Option<String>,
    observed: Arc<Observed>,
}

impl PluginBrokerHandle for FakeBroker {
    fn socket_path(&self) -> &Path {
        &self.socket
    }

    fn bind_sandbox(&self, sandbox_pid: u32) -> Result<(), OrbitError> {
        self.observed
            .bound_to
            .lock()
            .expect("bound")
            .push(sandbox_pid);
        match &self.bind_error {
            Some(error) => Err(OrbitError::Execution(error.clone())),
            None => Ok(()),
        }
    }
}

impl Drop for FakeBroker {
    fn drop(&mut self) {
        self.observed.dropped.store(true, Ordering::SeqCst);
    }
}

enum Starts {
    Binds { bind_error: Option<String> },
    Fails(String),
}

struct BrokerHost {
    starts: Starts,
    observed: Arc<Observed>,
}

impl BrokerHost {
    fn new(starts: Starts) -> Self {
        Self {
            starts,
            observed: Arc::default(),
        }
    }
}

impl RuntimeHost for BrokerHost {
    fn start_plugin_broker(
        &self,
        run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
        self.observed
            .started_for
            .lock()
            .expect("started")
            .push(run.run_id.clone());
        match &self.starts {
            Starts::Binds { bind_error } => Ok(Some(Box::new(FakeBroker {
                socket: PathBuf::from("/orbit/state/plugin-broker/0123/broker.sock"),
                bind_error: bind_error.clone(),
                observed: Arc::clone(&self.observed),
            }))),
            Starts::Fails(cause) => Err(OrbitError::Execution(cause.clone())),
        }
    }
}

fn run() -> PluginBrokerRun {
    PluginBrokerRun {
        run_id: "run-1".to_string(),
        job_run_id: Some("run-1".to_string()),
        task_id: None,
        activity_name: "agent_implement".to_string(),
        agent_name: None,
        model_name: None,
        workspace: None,
        allowed_tools: Vec::new(),
        tool_deny_policy: None,
        caller: BrokeredCaller {
            worktree: PathBuf::from("/repo"),
            fs_profile: sandbox_for_test().fs_profile,
            proc_allowed_programs: Vec::new(),
            proc_disallowed_programs: None,
        },
    }
}

fn sandbox(kind: ExecutorSandboxKind) -> ResolvedSandbox {
    ResolvedSandbox {
        kind,
        ..sandbox_for_test()
    }
}

#[test]
fn a_sandboxed_launch_exports_the_bound_socket_and_tears_it_down_on_drop() {
    for kind in [
        ExecutorSandboxKind::LinuxBwrap,
        ExecutorSandboxKind::MacosSandboxExec,
    ] {
        let host = BrokerHost::new(Starts::Binds { bind_error: None });

        let broker = RunPluginBroker::start(&host, "run-1", Some(&run()), Some(&sandbox(kind)));
        broker.bind_sandbox(4242);

        assert_eq!(
            broker.env(),
            Some((
                PLUGIN_BROKER_ENV.to_string(),
                "/orbit/state/plugin-broker/0123/broker.sock".to_string()
            ))
        );
        assert_eq!(
            *host.observed.started_for.lock().expect("started"),
            ["run-1"]
        );
        assert_eq!(*host.observed.bound_to.lock().expect("bound"), [4242]);
        assert!(!host.observed.dropped.load(Ordering::SeqCst));
        drop(broker);
        assert!(
            host.observed.dropped.load(Ordering::SeqCst),
            "dropping the run's broker must tear the host's down"
        );
    }
}

#[test]
fn an_unsandboxed_launch_starts_no_broker() {
    let off = sandbox(ExecutorSandboxKind::Off);
    for sandbox in [None, Some(&off)] {
        let host = BrokerHost::new(Starts::Binds { bind_error: None });

        let broker = RunPluginBroker::start(&host, "run-1", Some(&run()), sandbox);
        broker.bind_sandbox(4242);

        assert_eq!(broker.env(), None);
        assert!(
            host.observed
                .started_for
                .lock()
                .expect("started")
                .is_empty()
        );
        assert!(host.observed.bound_to.lock().expect("bound").is_empty());
    }
}

#[test]
fn a_broker_that_cannot_bind_leaves_no_variable_and_warns_with_its_cause() {
    let host = BrokerHost::new(Starts::Fails("broker directory is a symlink".to_string()));

    let (broker, events) = capture_events(|| {
        RunPluginBroker::start(
            &host,
            "run-1",
            Some(&run()),
            Some(&sandbox(ExecutorSandboxKind::LinuxBwrap)),
        )
    });

    assert_eq!(broker.env(), None);
    assert!(
        events.iter().any(|event| event
            .field("cause")
            .is_some_and(|cause| cause.contains("broker directory is a symlink"))),
        "the warning must name the cause: {events:?}"
    );
}

#[test]
fn a_sandbox_the_broker_cannot_identify_is_warned_and_the_launch_continues() {
    let host = BrokerHost::new(Starts::Binds {
        bind_error: Some("sandbox exited before its namespace was read".to_string()),
    });
    let broker = RunPluginBroker::start(
        &host,
        "run-1",
        Some(&run()),
        Some(&sandbox(ExecutorSandboxKind::LinuxBwrap)),
    );

    let ((), events) = capture_events(|| broker.bind_sandbox(4242));

    assert!(
        events.iter().any(|event| event
            .field("cause")
            .is_some_and(|cause| cause.contains("namespace was read"))),
        "the warning must name the cause: {events:?}"
    );
    assert!(broker.env().is_some(), "the socket stays; it refuses peers");
}

#[test]
fn a_sandboxed_launch_without_a_worktree_starts_no_broker_and_warns() {
    let host = BrokerHost::new(Starts::Binds { bind_error: None });

    let (broker, events) = capture_events(|| {
        RunPluginBroker::start(
            &host,
            "run-1",
            None,
            Some(&sandbox(ExecutorSandboxKind::LinuxBwrap)),
        )
    });

    assert_eq!(broker.env(), None);
    assert!(
        host.observed
            .started_for
            .lock()
            .expect("started")
            .is_empty()
    );
    assert!(
        events.iter().any(|event| event
            .field("cause")
            .is_some_and(|cause| cause.contains("no worktree"))),
        "the warning must name the cause: {events:?}"
    );
}
