//! Regression checks at the composition-provided `FederatedDrainOwner`
//! routing boundary [ORB-29594].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::runtime::OrbitRuntime;
use orbit_mcp::McpSessionAuthority;
use orbit_mcp::federated::{
    DEFAULT_PROBE_TIMEOUT, DEFAULT_ROUTED_DELIVERY_TIMEOUT, Destination, DestinationProbe,
    SshDestinationProbe,
};
use orbit_registry::hosts::{HOSTS_FILE, HOSTS_SCHEMA_VERSION};
use orbit_registry::workspace_registry::{
    register_checkout, register_workspace, registry_path_for, save_registry_to,
};
use orbit_store::contract::{
    AdmissionRequest, AdmissionRunContext, AdmissionShipContract, ClaimInvocation, ClaimMutation,
    ClaimRun, DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, HandoffObservation, PullDestination,
    distributed_drain_protocol_fingerprint,
};
use orbit_types::task::TaskStatus;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext, WorkerInvocation};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{
    HandoffCandidate, HandoffDelivery, HandoffReview, TaskHandoff,
};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde_json::json;

use crate::registry_runtime::RegisteredRuntimeFactory;

const OWNER_MACHINE: &str = "hm_0102030405060708";

const OWNER_NAME: &str = "owner-host";

const FOLLOWER_MACHINE: &str = "hm_deadbeefdeadbeef";

const FOLLOWER_NAME: &str = "follower-host";

const OWNER_WORKSPACE: &str = "ws_0102030405060708";

const REPLICA_WORKSPACE: &str = "ws_fedcba9876543210";

const PR_NUMBER: u64 = 11640;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Orbit Test")
        .env("GIT_AUTHOR_EMAIL", "orbit-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Orbit Test")
        .env("GIT_COMMITTER_EMAIL", "orbit-test@example.invalid")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn revision(repo: &Path, rev: &str) -> SourceRevision {
    SourceRevision {
        commit: git(repo, &["rev-parse", rev]),
        tree: git(repo, &["rev-parse", &format!("{rev}^{{tree}}")]),
    }
}

fn init_git_repo(repo: &Path) -> SourceRevision {
    std::fs::create_dir_all(repo).unwrap();
    std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
    std::fs::write(repo.join("src_a.rs"), "pub fn a() -> u32 { 1 }\n").unwrap();
    std::fs::write(repo.join("src_b.rs"), "pub fn b() -> u32 { 1 }\n").unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "initial"]);
    revision(repo, "HEAD")
}

fn commit_candidate_branch(repo: &Path, branch: &str, file: &str, content: &str) -> SourceRevision {
    git(repo, &["checkout", "-q", "-b", branch]);
    std::fs::write(repo.join(file), content).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-q", "-m", "repair candidate"]);
    let rev = revision(repo, "HEAD");
    git(repo, &["checkout", "-q", "main"]);
    rev
}

fn install_poison_ssh(bin: &Path, sentinel: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(bin).unwrap();
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 99\n",
            sentinel.display()
        );
        let ssh = bin.join("ssh");
        std::fs::write(&ssh, script).unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = (bin, sentinel);
}

fn install_fixture_gh(bin: &Path, head_branch: &str, head_sha: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(bin).unwrap();
        let payload = json!({
            "number": PR_NUMBER,
            "state": "OPEN",
            "mergeStateStatus": "CLEAN",
            "mergeable": "MERGEABLE",
            "headRefName": head_branch,
            "baseRefName": "main",
            "headRefOid": head_sha,
            "url": format!("https://github.com/example/orbit/pull/{PR_NUMBER}"),
        });
        let script = format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", payload);
        let gh = bin.join("gh");
        std::fs::write(&gh, script).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = (bin, head_branch, head_sha);
}

fn setup_registered_host(
    root: &Path,
) -> (
    PathBuf,
    PathBuf,
    Workspace,
    WorkspaceCheckout,
    SourceRevision,
) {
    let global_root = root.join(".orbit");
    std::fs::create_dir_all(&global_root).unwrap();
    std::fs::write(
        global_root.join("config.toml"),
        format!(
            "[machine]\nid = \"{OWNER_MACHINE}\"\nname = \"{OWNER_NAME}\"\n\
             task_prefix = \"ORB\"\n\n\
             [crews.sol]\nprovider = \"codex\"\nmodel = \"gpt-sol\"\n\n\
             [workflow]\ndefault_crew = \"sol\"\n\n\
             [review]\ntiming = \"off\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        global_root.join(HOSTS_FILE),
        format!(
            "schema_version = {HOSTS_SCHEMA_VERSION}\n\n\
             [[hosts]]\nname = \"{FOLLOWER_NAME}\"\nmachine_id = \"{FOLLOWER_MACHINE}\"\n\
             ssh = \"follower-ssh-target\"\ntask_prefix = \"MAC\"\n"
        ),
    )
    .unwrap();

    let owner_repo = root.join("owner-repo");
    let base_rev = init_git_repo(&owner_repo);
    let owner_orbit = owner_repo.join(".orbit");
    std::fs::create_dir_all(&owner_orbit).unwrap();
    std::fs::write(
        owner_orbit.join("config.yaml"),
        format!("schema_version: 1\nworkspace_id: {OWNER_WORKSPACE}\n"),
    )
    .unwrap();
    std::fs::write(
        owner_orbit.join("config.toml"),
        "[review]\ntiming = \"off\"\n\n[workflow]\nrequired_validation_commands = []\n",
    )
    .unwrap();

    let replica_repo = root.join("replica-repo");
    std::fs::create_dir_all(replica_repo.join(".orbit")).unwrap();
    std::fs::write(
        replica_repo.join(".orbit/config.yaml"),
        format!("schema_version: 1\nworkspace_id: {REPLICA_WORKSPACE}\n"),
    )
    .unwrap();

    let now = Utc::now();
    let owner_ws = Workspace {
        id: OWNER_WORKSPACE.to_string(),
        name: "owner-ws".to_string(),
        owner_machine_id: Some(OWNER_MACHINE.to_string()),
        git_remote: None,
        ship_mode: Some("pr".to_string()),
        base_branch: "main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    };
    let owner_checkout =
        WorkspaceCheckout::owner(OWNER_WORKSPACE.to_string(), owner_repo.clone(), owner_orbit);

    let replica_ws = Workspace {
        id: REPLICA_WORKSPACE.to_string(),
        name: "replica-ws".to_string(),
        owner_machine_id: Some(FOLLOWER_MACHINE.to_string()),
        git_remote: None,
        ship_mode: Some("pr".to_string()),
        base_branch: "main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    };
    let replica_checkout = WorkspaceCheckout {
        workspace_id: REPLICA_WORKSPACE.to_string(),
        repo_root: replica_repo.clone(),
        orbit_dir: replica_repo.join(".orbit"),
        role: Some(WorkspaceCheckoutRole::Replica),
        owner_machine_id: Some(FOLLOWER_MACHINE.to_string()),
        path_overrides: Vec::new(),
    };

    let mut registry = WorkspaceRegistry::default();
    register_workspace(&mut registry, owner_ws.clone()).unwrap();
    register_checkout(&mut registry, owner_checkout.clone()).unwrap();
    register_workspace(&mut registry, replica_ws).unwrap();
    register_checkout(&mut registry, replica_checkout).unwrap();
    save_registry_to(&registry, &registry_path_for(&global_root)).unwrap();

    (global_root, owner_repo, owner_ws, owner_checkout, base_rev)
}

fn add_backlog_task(owner: &OrbitRuntime, title: &str, file: &str) -> String {
    let added = owner
        .run_tool(
            "orbit.task.add",
            json!({
                "workspace": owner.paths().repo_root,
                "title": title,
                "description": "Owner repair routing regression fixture.",
                "complexity": "low",
                "model": "codex",
                "context_files": [format!("file:{file}")],
            }),
        )
        .expect("add task");
    let id = added["id"].as_str().expect("task id").to_string();
    owner
        .run_tool(
            "orbit.task.update",
            json!({
                "id": id,
                "plan": format!("Update {file} and validate."),
                "model": "codex",
            }),
        )
        .expect("set task plan");
    owner
        .run_tool(
            "orbit.task.update",
            json!({
                "id": id,
                "status": "backlog",
                "model": "codex",
            }),
        )
        .expect("promote task to backlog");
    id
}

fn seed_stopped_follower_repair_pending(
    owner: &OrbitRuntime,
    task_id: &str,
    base_rev: &SourceRevision,
    follower_rev: &SourceRevision,
) -> String {
    let follower_session = ToolSessionContext {
        caller_machine_id: Some(FOLLOWER_MACHINE.to_string()),
        process_machine_id: Some(OWNER_MACHINE.to_string()),
        transport: Some(McpTransport::SshMcp),
        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
        ..ToolSessionContext::default()
    };
    let ship = AdmissionShipContract {
        mode: "pr".to_string(),
        base_branch: "main".to_string(),
        landing_branch: "main".to_string(),
        before_pr: false,
        before_landing: false,
        completion: "review".to_string(),
        authorization_reference: None,
        review: None,
    };
    let request = AdmissionRequest {
        request_id: "follower-initial-admission".to_string(),
        caller_version: orbit_core::application::distributed::owner_binary_version().to_string(),
        caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        caller_fingerprint: Some(distributed_drain_protocol_fingerprint().to_string()),
        caller_before_pr: false,
        review_gate: true,
        selective_review: false,
        visual_resolution: 0,
        run_context: AdmissionRunContext {
            run_id: "jrun-follower-drain".to_string(),
            job_name: "workspace_pull_pipeline".to_string(),
            machine_name: Some(FOLLOWER_NAME.to_string()),
        },
        ship: ship.clone(),
        crews: None,
        os: None,
        browser_kit: false,
        labels: Vec::new(),
        admissible: None,
    };
    let pull = owner
        .serve_task_pull(&follower_session, &request)
        .expect("admit follower claim");
    let claim = pull.receipt.claim.expect("follower claim");
    assert_eq!(claim.task_id, task_id);

    owner
        .serve_claim_bind(
            &follower_session,
            &claim.claim_id,
            "jrun-follower-leaf",
            ship,
        )
        .expect("bind follower leaf");

    let candidate = HandoffCandidate {
        repository: owner.workspace_id().unwrap(),
        source_branch: "orbit/follower-branch".to_string(),
        base_branch: "main".to_string(),
        landing_branch: "main".to_string(),
        base: base_rev.clone(),
        candidate: follower_rev.clone(),
        delivery: HandoffDelivery::PullRequest { number: PR_NUMBER },
    };
    let handoff = TaskHandoff {
        schema_version: 1,
        workspace_id: owner.workspace_id().unwrap(),
        task_id: task_id.to_string(),
        claim_id: claim.claim_id.clone(),
        machine_id: FOLLOWER_MACHINE.to_string(),
        run_id: "jrun-follower-leaf".to_string(),
        candidate: candidate.clone(),
        validation: Vec::new(),
        review: HandoffReview::not_required(),
        execution_summary: "Follower delivered PR #11640.".to_string(),
        footprint_widening: Vec::new(),
        owed_criteria: Vec::new(),
        regression_from: Vec::new(),
        visual: None,
    };
    let worker_context = ClaimInvocation::trusted_worker(
        task_id.to_string(),
        claim.claim_id.clone(),
        FOLLOWER_MACHINE.to_string(),
        Some(ClaimRun {
            machine_id: FOLLOWER_MACHINE.to_string(),
            run_id: "jrun-follower-leaf".to_string(),
        }),
    );
    let observation = HandoffObservation {
        candidate: candidate.clone(),
        footprint_widening: Vec::new(),
        required_commands: Vec::new(),
        owner_completion_authority: None,
        review: None,
        known_red: Vec::new(),
        light_delivery: Vec::new(),
    };
    owner
        .accept_task_handoff(
            &worker_context,
            "handoff-follower",
            handoff,
            observation.clone(),
        )
        .expect("accept follower handoff");

    let handoff_id = owner
        .accepted_task_handoff(&claim.claim_id)
        .unwrap()
        .handoff_id;
    let landing = ClaimInvocation::trusted_operator(
        task_id.to_string(),
        claim.claim_id.clone(),
        "owner-landing".to_string(),
    )
    .with_handoff_observation(observation);
    for (id, mutation) in [
        (
            "approve-landing",
            ClaimMutation::ApproveHandoff {
                handoff_id: handoff_id.clone(),
                candidate,
            },
        ),
        (
            "dispatch-landing",
            ClaimMutation::DispatchLanding {
                handoff_id: handoff_id.clone(),
                job_run_id: None,
            },
        ),
        (
            "stop-landing",
            ClaimMutation::StopLanding {
                handoff_id,
                reason: "PR #11640 conflicts with agent-main".to_string(),
                repairable: true,
                requeue: false,
            },
        ),
    ] {
        owner
            .mutate_execution_claim(Some(&landing), id, &mutation)
            .expect("advance landing");
    }
    claim.claim_id
}

#[test]
fn federated_drain_owner_routes_local_repair_pull_in_process_and_keeps_remote_and_self_ssh_guards()
{
    if crate::tests::lib::run_isolated_test(concat!(
        module_path!(),
        "::federated_drain_owner_routes_local_repair_pull_in_process_and_keeps_remote_and_self_ssh_guards"
    )) {
        return;
    }

    let home = std::env::var_os("HOME").expect("isolated HOME");
    let root = PathBuf::from(home);
    let (global_root, owner_repo, owner_ws, owner_checkout, base_rev) =
        setup_registered_host(&root);
    let follower_rev = commit_candidate_branch(
        &owner_repo,
        "orbit/follower-branch",
        "src_a.rs",
        "pub fn a() -> u32 { 2 }\n",
    );
    let repair_rev = commit_candidate_branch(
        &owner_repo,
        "orbit/owner-repair-branch",
        "src_a.rs",
        "pub fn a() -> u32 { 3 }\n",
    );

    let poison_bin = root.join("poison-bin");
    let ssh_sentinel = root.join("ssh-invoked.log");
    install_poison_ssh(&poison_bin, &ssh_sentinel);
    install_fixture_gh(&poison_bin, "orbit/owner-repair-branch", &repair_rev.commit);
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = vec![poison_bin.clone()];
    path_entries.extend(std::env::split_paths(&existing_path));
    let joined_path = std::env::join_paths(path_entries).unwrap();
    let joined_path_str = joined_path.to_str().unwrap().to_string();
    let mut path_guard = Some(orbit_common::test_env::scoped([(
        "PATH",
        Some(joined_path_str.as_str()),
    )]));

    // Open through the real composition factory so `worker_coordination::attach`
    // installs the production `FederatedDrainOwner` transport.
    let owner = RegisteredRuntimeFactory::open_registered_checkout(
        &global_root,
        &owner_ws,
        &owner_checkout,
    )
    .expect("open registered owner")
    .into_runtime();
    let transport = owner
        .drain_owner_transport()
        .cloned()
        .expect("FederatedDrainOwner attached");

    // Seed a stopped follower landing in `repair_pending` plus an unrelated
    // ready backlog task to prove the owner fallback admits repairs only.
    let repaired_task = add_backlog_task(&owner, "Stopped follower landing", "src_a.rs");
    let follower_claim_id =
        seed_stopped_follower_repair_pending(&owner, &repaired_task, &base_rev, &follower_rev);
    let backlog_task = add_backlog_task(&owner, "Unrelated backlog task", "src_b.rs");

    // Direct self-SSH refusal on `SshDestinationProbe` must remain intact.
    let direct_ssh = SshDestinationProbe::new(
        OWNER_MACHINE.to_string(),
        DEFAULT_PROBE_TIMEOUT,
        DEFAULT_ROUTED_DELIVERY_TIMEOUT,
        None,
        McpSessionAuthority::Agent,
    );
    let self_ssh_err = direct_ssh
        .open_internal_drain_route(&Destination::local(OWNER_MACHINE, OWNER_NAME))
        .err()
        .expect("SshDestinationProbe must still refuse local destinations");
    assert!(
        self_ssh_err.to_string().contains(&format!(
            "local destination '{OWNER_MACHINE}' cannot be opened over SSH"
        )),
        "unexpected direct self-SSH error: {self_ssh_err}"
    );

    let owner_selector = format!("{OWNER_MACHINE}/{OWNER_WORKSPACE}");
    let owner_jobs = orbit_core::runtime::JobRuns::shared(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let repair_pull = owner_jobs
        .insert_job_run(
            "workspace_pull_pipeline",
            1,
            Utc::now(),
            Some(json!({
                "for_seconds": 0,
                "owner_repair_pull": true,
                "destination": PullDestination {
                    owner_machine_id: OWNER_MACHINE.to_string(),
                    selector: owner_selector.clone(),
                    owner_workspace_id: owner.workspace_id().unwrap(),
                    execution_machine_id: OWNER_MACHINE.to_string(),
                },
            })),
            None,
        )
        .expect("record owner repair pull run");

    let mut repair_claim_id = String::new();
    orbit_core::test_support::with_repair_owner_fallback_elapsed(|| {
        // 1. Probe through the real composition transport: succeeds locally, no SSH.
        let probe = transport
            .call(
                &owner_selector,
                "orbit.drain.probe",
                json!({
                    "caller_version": orbit_core::application::distributed::owner_binary_version(),
                    "caller_schema": DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
                    "caller_fingerprint": distributed_drain_protocol_fingerprint(),
                    "caller_before_pr": false,
                }),
            )
            .expect("local owner drain probe must route in-process without SSH");
        assert_eq!(probe["owner_machine_id"], OWNER_MACHINE, "{probe}");
        assert_eq!(probe["admits"], true, "{probe}");
        assert_eq!(probe["session"]["remote"], false, "{probe}");
        assert_eq!(
            probe["session"]["caller_machine_id"], OWNER_MACHINE,
            "{probe}"
        );
        assert_eq!(
            probe["session"]["capabilities"],
            json!(["agent"]),
            "{probe}"
        );
        assert!(!ssh_sentinel.exists(), "local probe must never spawn ssh");

        let ship: AdmissionShipContract =
            serde_json::from_value(probe["ship"].clone()).expect("probe ship contract");

        // 2. Pull admission: admits the due pending repair and revokes the
        // superseded follower claim.
        let pull_request = AdmissionRequest {
            request_id: "owner-repair-req-1".to_string(),
            caller_version: orbit_core::application::distributed::owner_binary_version()
                .to_string(),
            caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            caller_fingerprint: Some(distributed_drain_protocol_fingerprint().to_string()),
            caller_before_pr: false,
            review_gate: true,
            selective_review: false,
            visual_resolution: 0,
            run_context: AdmissionRunContext {
                run_id: repair_pull.run_id.clone(),
                job_name: "workspace_pull_pipeline".to_string(),
                machine_name: Some(OWNER_NAME.to_string()),
            },
            ship: ship.clone(),
            crews: None,
            os: None,
            browser_kit: false,
            labels: Vec::new(),
            admissible: None,
        };
        let admitted = transport
            .call(
                &owner_selector,
                "orbit.task.pull",
                serde_json::to_value(&pull_request).unwrap(),
            )
            .expect("local owner repair pull must admit without SSH");
        let claim = &admitted["receipt"]["claim"];
        assert_eq!(claim["task_id"], repaired_task, "{admitted}");
        assert_eq!(
            claim["executed_on"]["machine_id"], OWNER_MACHINE,
            "{admitted}"
        );
        assert_eq!(
            claim["repair"]["repairs_claim_id"], follower_claim_id,
            "{admitted}"
        );
        repair_claim_id = claim["claim_id"]
            .as_str()
            .expect("repair claim id")
            .to_string();

        // Single-current-claim: superseded follower claim is now revoked.
        let claims = owner.inspect_execution_claims().unwrap();
        let superseded = claims
            .iter()
            .find(|state| state.claim.claim_id == follower_claim_id)
            .expect("superseded follower claim");
        assert_eq!(
            superseded.claim.phase,
            orbit_store::contract::ExecutionClaimPhase::Revoked
        );

        // Repair-only pull must NOT admit the ordinary backlog task.
        let mut second_request = pull_request.clone();
        second_request.request_id = "owner-repair-req-2".to_string();
        let idle = transport
            .call(
                &owner_selector,
                "orbit.task.pull",
                serde_json::to_value(&second_request).unwrap(),
            )
            .expect("second repair pull returns idle");
        assert!(
            idle["receipt"]["claim"].is_null(),
            "repair-only pull must not admit backlog work: {idle}"
        );
        assert_eq!(
            owner.get_task(&backlog_task).unwrap().status,
            TaskStatus::Backlog
        );

        // 3. Bind through the real composition transport (`claimed -> running`).
        let bound = transport
            .call(
                &owner_selector,
                "orbit.drain.claim.bind",
                json!({
                    "claim_id": repair_claim_id,
                    "run_id": "jrun-owner-repair-leaf",
                    "ship": serde_json::to_value(&ship).unwrap(),
                }),
            )
            .expect("local owner bind must succeed without SSH");
        assert_eq!(bound["phase"], "running", "{bound}");

        // 4. Settle through the real composition transport (`running -> handed_off`)
        // observing the published PR candidate in the owner repo, including when
        // the calling process holds an active claimed-leaf worker binding
        // (`ORBIT_WORKER_CONTEXT_REQUIRED=1`, matching leaf self-settlement).
        let bound_leaf = owner
            .clone()
            .with_worker_invocation(
                WorkerInvocation {
                    owner_machine_id: OWNER_MACHINE.to_string(),
                    owner_workspace_id: owner.workspace_id().unwrap(),
                    owner_destination: owner_selector.clone(),
                    task_id: repaired_task.clone(),
                    claim_id: repair_claim_id.clone(),
                    execution: orbit_types::task::ExecutionLocation {
                        machine_id: OWNER_MACHINE.to_string(),
                        machine_name: Some(OWNER_NAME.to_string()),
                    },
                    bound_run_id: "jrun-owner-repair-leaf".to_string(),
                },
                transport.worker_coordinator(),
            )
            .expect("bind repair leaf worker");
        bound_leaf
            .into_kernel()
            .register_worker_process(std::process::id())
            .expect("register worker process binding");
        path_guard.take();
        path_guard = Some(orbit_common::test_env::scoped([
            ("PATH", Some(joined_path_str.as_str())),
            ("ORBIT_WORKER_CONTEXT_REQUIRED", Some("1")),
        ]));
        let repair_handoff = TaskHandoff {
            schema_version: 1,
            workspace_id: owner.workspace_id().unwrap(),
            task_id: repaired_task.clone(),
            claim_id: repair_claim_id.clone(),
            machine_id: OWNER_MACHINE.to_string(),
            run_id: "jrun-owner-repair-leaf".to_string(),
            candidate: HandoffCandidate {
                repository: owner.workspace_id().unwrap(),
                source_branch: "orbit/owner-repair-branch".to_string(),
                base_branch: "main".to_string(),
                landing_branch: "main".to_string(),
                base: base_rev.clone(),
                candidate: repair_rev.clone(),
                delivery: HandoffDelivery::PullRequest { number: PR_NUMBER },
            },
            validation: Vec::new(),
            review: HandoffReview::not_required(),
            execution_summary: "Owner fallback repaired PR #11640 locally.".to_string(),
            footprint_widening: Vec::new(),
            owed_criteria: Vec::new(),
            regression_from: Vec::new(),
            visual: None,
        };
        let settlement_payload =
            serde_json::to_value(ClaimMutation::AcceptHandoff(repair_handoff)).unwrap();
        let settled = transport
            .call(
                &owner_selector,
                "orbit.drain.claim.settle",
                json!({
                    "claim_id": repair_claim_id,
                    "run_id": "jrun-owner-repair-leaf",
                    "settlement": settlement_payload,
                }),
            )
            .expect("local owner settlement must succeed without SSH");
        assert_eq!(settled["phase"], "handed_off", "{settled}");
        assert_eq!(
            owner.get_task(&repaired_task).unwrap().status,
            TaskStatus::Review
        );
        assert!(
            !ssh_sentinel.exists(),
            "probe, admission, bind and settlement on local owner must never spawn ssh"
        );
    });

    // Local `show_task` also routes in-process without SSH.
    let shown = transport
        .show_task(&owner_selector, json!({"id": repaired_task}))
        .expect("local show_task routes in-process");
    assert_eq!(shown["id"], repaired_task, "{shown}");
    assert!(
        !ssh_sentinel.exists(),
        "local show_task must never spawn ssh"
    );

    // A replica checkout registered on this host is still refused control-plane
    // drain calls locally.
    let replica_selector = format!("{OWNER_MACHINE}/{REPLICA_WORKSPACE}");
    let replica_err = transport
        .call(&replica_selector, "orbit.drain.probe", json!({}))
        .expect_err("local replica checkout must refuse drain probe");
    assert!(
        matches!(replica_err, OrbitError::CapabilityRefused(_)),
        "expected CapabilityRefused for local replica checkout, got: {replica_err:?}"
    );
    assert!(
        !ssh_sentinel.exists(),
        "local replica refusal must not spawn ssh"
    );

    // A remote host selector still routes through SSH (never answered locally).
    #[cfg(unix)]
    {
        let remote_selector = format!("{FOLLOWER_MACHINE}/{OWNER_WORKSPACE}");
        let remote_err = transport
            .call(&remote_selector, "orbit.drain.probe", json!({}))
            .expect_err("remote selector must go through poison ssh stub and fail");
        assert!(
            matches!(remote_err, OrbitError::UnreachableDestination(_)),
            "expected UnreachableDestination from remote SSH stub, got: {remote_err:?}"
        );
        assert!(
            ssh_sentinel.exists(),
            "remote follower route must still invoke ssh"
        );
    }
}
