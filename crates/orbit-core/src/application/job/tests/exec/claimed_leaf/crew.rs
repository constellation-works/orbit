//! A pulled claimed leaf runs on the owner task's crew.
//!
//! A follower executes a task that lives in the owner's store. Its own store
//! either lacks that task or holds an unrelated record under the same id, so
//! the crew must come from the owner's claim snapshot, with the same
//! precedence a local run uses: an explicit run crew, then the task's crew,
//! then this host's `default_crew` only when the task names none.
//!
//! The owner and executor share one store here. The peer gives the owner's
//! snapshot a crew the local task record does not name, which is exactly the
//! view a follower with a same-id local record has. The leaf runs through the real pipeline worker, and the
//! provider is a recording stand-in, so the crew the run records and the model
//! the provider was launched with are both observed rather than inferred.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_store::contracts::*;
use orbit_types::workflow::JobRunState;

use super::*;

const DEFAULT_MODEL: &str = "grok-build";
const OWNER_MODEL: &str = "sol-model";

/// The follower's config: its `default_crew` differs from the owner's crew,
/// and both run the same provider so only the model tells them apart.
fn follower_config() -> String {
    format!(
        "{}default_crew = \"fixture\"\n\
         [crews.fixture]\nprovider = \"grok\"\nmodel = \"{DEFAULT_MODEL}\"\n\
         [crews.sol]\nprovider = \"grok\"\nmodel = \"{OWNER_MODEL}\"\n",
        workspace_config()
    )
}

/// Wrap the claimed implementer so every launch appends its argv to
/// `argv_log`, and pass the crew's model on that argv.
fn install_recording_implementer(runtime: &OrbitRuntime, dir: &Path, argv_log: &Path) {
    use std::os::unix::fs::PermissionsExt;

    install_claimed_implementer(runtime, dir);
    let inner = dir.join("grok");
    // The runner infers the provider from the command name, so the recorder
    // is `grok` too, in a directory of its own.
    let recorder_dir = dir.join("recording");
    std::fs::create_dir_all(&recorder_dir).expect("recorder dir");
    let recorder = recorder_dir.join("grok");
    std::fs::write(
        &recorder,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            argv_log.display(),
            inner.display()
        ),
    )
    .expect("write recording implementer");
    std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o755))
        .expect("make recorder executable");
    let now = chrono::Utc::now();
    runtime
        .upsert_executor_def(&orbit_types::workflow::ExecutorDef {
            name: "grok".to_string(),
            executor_type: orbit_types::workflow::ExecutorType::DirectAgent,
            command: Some(recorder.display().to_string()),
            args: Vec::new(),
            stdout_format: None,
            model_pair_override: None,
            model_flag: Some("--model".to_string()),
            timeout_seconds: Some(120),
            env: std::collections::HashMap::new(),
            sandbox: None,
            allow_fallback: false,
            created_at: Some(now),
            updated_at: Some(now),
        })
        .expect("seed recording implementer executor");
}

/// The owner, answering with a task snapshot whose crew is `owner_crew`.
struct OwnerCrewPeer<'a> {
    owner: OwnerPullPeer<'a>,
    owner_crew: Option<&'static str>,
}

impl PullPeer for OwnerCrewPeer<'_> {
    fn request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
    ) -> Result<AdmissionReceipt, OrbitError> {
        let mut receipt = self.owner.request(destination, request)?;
        if let Some(task) = receipt.task.as_mut() {
            task.crew = self.owner_crew.map(ToOwned::to_owned);
        }
        Ok(receipt)
    }
    fn bind(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        self.owner.bind(admission)
    }
    fn settle(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        self.owner.settle(admission)
    }
    fn lookup(
        &self,
        destination: &PullDestination,
        request_id: &str,
    ) -> Result<AdmissionLookup, OrbitError> {
        self.owner.lookup(destination, request_id)
    }
}

/// Runs the bound leaf through the real pipeline worker in this process, so
/// the run's crew is resolved and recorded exactly as a launched worker does.
struct WorkerLauncher<'a> {
    real: LeafPullLauncher<'a>,
    launched: RefCell<Vec<String>>,
    outcome: RefCell<Option<Result<(), String>>>,
}

impl PullLauncher for WorkerLauncher<'_> {
    fn cancel_queued(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        self.real.cancel_queued(admission)
    }
    fn launch(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        let bound = self.real.bound_runtime(admission)?;
        let run_id = bound
            .worker_invocation()
            .expect("bound")
            .bound_run_id
            .clone();
        self.launched.borrow_mut().push(run_id.clone());
        *self.outcome.borrow_mut() = Some(
            bound
                .execute_pipeline_run_worker(&run_id)
                .map_err(|error| error.to_string()),
        );
        Ok(())
    }
}

struct PulledLeaf {
    _root: tempfile::TempDir,
    runtime: OrbitRuntime,
    leaf: String,
    outcome: Result<(), String>,
    argv_log: PathBuf,
}

impl PulledLeaf {
    fn run(&self) -> orbit_types::workflow::JobRun {
        self.runtime.show_job_run(&self.leaf).expect("leaf run")
    }

    fn launches(&self) -> Vec<String> {
        std::fs::read_to_string(&self.argv_log)
            .map(|log| log.lines().map(ToOwned::to_owned).collect())
            .unwrap_or_default()
    }
}

/// Admit one claim whose owner task names `owner_crew`, on a host whose local
/// record of that task names this host's default crew, and run its leaf.
fn run_pulled_leaf(owner_crew: Option<&'static str>) -> PulledLeaf {
    let (root, runtime, repo_root, global_root) =
        super::super::test_runtime_with_workspace_config(&follower_config());
    seed_default_catalogs(&global_root);
    init_remoteless_repo(&repo_root);
    let runtime = runtime.with_automation_machine_identity(Some(MACHINE.to_string()));
    let argv_log = root.path().join("provider-argv.log");
    install_recording_implementer(&runtime, root.path(), &argv_log);
    let task_id = seed_claimable_task(&runtime);
    // The local record is stamped with this host's default crew; a resolver
    // that read it instead of the owner's snapshot would run `fixture`.
    assert_eq!(
        runtime.get_task(&task_id).expect("task").crew.as_deref(),
        Some("fixture"),
    );
    let drain = drain_run(&runtime);
    let destination = destination(&runtime, MACHINE);
    let template = admission_request(&runtime, &drain, "local");
    let peer = OwnerCrewPeer {
        owner: OwnerPullPeer { runtime: &runtime },
        owner_crew,
    };
    let launcher = WorkerLauncher {
        real: LeafPullLauncher { runtime: &runtime },
        launched: RefCell::new(Vec::new()),
        outcome: RefCell::new(None),
    };
    let admitted = PullDrain {
        jobs: runtime.stores().jobs(),
        peer: &peer,
        launcher: &launcher,
    }
    .refill(&destination, &template, 1)
    .expect("one admission");
    assert_eq!(admitted, 1);
    let leaf = launcher.launched.borrow()[0].clone();
    let outcome = launcher.outcome.borrow_mut().take().expect("leaf executed");
    PulledLeaf {
        _root: root,
        runtime,
        leaf,
        outcome,
        argv_log,
    }
}

fn assert_ran_on(pulled: &PulledLeaf, crew: &str, model: &str, other_model: &str) {
    assert_eq!(pulled.outcome, Ok(()), "the leaf runs to its handoff");
    let run = pulled.run();
    assert_eq!(run.state, JobRunState::Success);
    assert_eq!(run.resolved_crew.as_deref(), Some(crew));
    assert_eq!(run.crew_model.as_deref(), Some(model));
    let projection = crate::application::job::job_run_to_json(&run, None);
    assert_eq!(projection["resolved_run_crew"]["crew"], crew);
    assert_eq!(projection["resolved_run_crew"]["model"], model);

    let launches = pulled.launches();
    assert_eq!(
        launches.len(),
        1,
        "implement_one launched once: {launches:?}"
    );
    assert!(
        launches[0].contains(&format!("--model {model}")),
        "the provider ran the recorded crew's model: {launches:?}"
    );
    assert!(!launches[0].contains(other_model), "{launches:?}");
}

#[test]
fn a_pulled_leaf_runs_on_the_owner_task_crew_not_the_follower_default() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::crew::a_pulled_leaf_runs_on_the_owner_task_crew_not_the_follower_default",
    ) {
        return;
    }
    let pulled = run_pulled_leaf(Some("sol"));
    assert_ran_on(&pulled, "sol", OWNER_MODEL, DEFAULT_MODEL);
}

#[test]
fn a_pulled_leaf_whose_task_names_no_crew_runs_on_the_follower_default() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::crew::a_pulled_leaf_whose_task_names_no_crew_runs_on_the_follower_default",
    ) {
        return;
    }
    let pulled = run_pulled_leaf(None);
    assert_ran_on(&pulled, "fixture", DEFAULT_MODEL, OWNER_MODEL);
}

#[test]
fn a_pulled_leaf_whose_crew_this_host_lacks_fails_before_implementing() {
    if isolated_claimed_test(
        "application::job::tests::exec::claimed_leaf::crew::a_pulled_leaf_whose_crew_this_host_lacks_fails_before_implementing",
    ) {
        return;
    }
    let pulled = run_pulled_leaf(Some("ghost"));
    let error = pulled
        .outcome
        .clone()
        .expect_err("an unconfigured owner crew fails the leaf");
    assert!(error.contains("`ghost`"), "names the crew: {error}");
    let run = pulled.run();
    assert_eq!(run.state, JobRunState::Failed);
    assert_eq!(run.resolved_crew, None, "no crew was selected: {run:?}");
    assert!(
        run.steps
            .iter()
            .all(|step| step.target_id != "agent_implement"),
        "implement_one never started: {:?}",
        run.steps
    );
    assert!(
        pulled.launches().is_empty(),
        "no provider launched, default_crew included: {:?}",
        pulled.launches()
    );
}
