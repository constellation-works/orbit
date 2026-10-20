//! [ORB-14449] Task ids route to the host their prefix names, and `--host`
//! names the host for `--workspace` and `--pull`.
//!
//! Host A is the fleet's local install (`LB`); host B is a second install
//! (`BR`) reached through the fake `ssh` (see `host_fleet`). Everything B
//! reports — its workspace, its refusals — is what B's own binary answered.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, TaskAddParams,
    TaskUpdateParams,
};
use orbit_core::{OrbitRuntime, TaskComplexity};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, blocked_workflow_failure_update};
use serde_json::{Value, json};

use crate::git_repo;
use crate::host_fleet::{Fleet, Install};

/// A fleet whose local host has registered `bravo`, with one task on each.
struct Routed {
    fleet: Fleet,
    bravo: Install,
    remote_task: String,
    local_task: String,
    local_runtime: OrbitRuntime,
    remote_runtime: OrbitRuntime,
}

impl Routed {
    fn new() -> Self {
        let fleet = Fleet::new();
        let bravo = fleet.install("bravo", "bravo", "BR");
        fleet.route("bravo", &bravo, "plain");
        fleet.json(&["host", "add", "bravo", "--json"]);
        let remote_runtime = owner_runtime(&bravo.home, &bravo.repo());
        let local_runtime = owner_runtime(&fleet.local.home, &fleet.local_repo);
        let remote_task = add_task(&remote_runtime, "held by bravo");
        let local_task = add_task(&local_runtime, "held here");
        assert!(
            remote_task.starts_with("BR-"),
            "bravo mints BR ids: {remote_task}"
        );
        assert!(
            local_task.starts_with("LB-"),
            "this host mints LB ids: {local_task}"
        );
        Self {
            fleet,
            bravo,
            remote_task,
            local_task,
            local_runtime,
            remote_runtime,
        }
    }

    /// Run on host A from its checkout and parse the JSON answer.
    fn json(&self, args: &[&str]) -> Value {
        let output = self.fleet.orbit_ok(&self.fleet.local_repo, args);
        parse(&output.stdout, args)
    }

    /// Run on host A from its checkout, expecting a refusal; returns
    /// `(code, message)`.
    fn refused(&self, args: &[&str]) -> (String, String) {
        self.refused_with(args, &[])
    }

    fn refused_with(&self, args: &[&str], env: &[(&str, &str)]) -> (String, String) {
        let mut command = self
            .fleet
            .orbit_in(&self.fleet.local.home, &self.fleet.local_repo, args);
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command.output().expect("spawn orbit");
        assert!(!output.status.success(), "{args:?} must be refused");
        let error: Value = serde_json::from_slice(&output.stderr)
            .or_else(|_| serde_json::from_slice(&output.stdout))
            .unwrap_or_else(|_| {
                panic!(
                    "{args:?} error JSON: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            });
        (
            error["code"].as_str().unwrap_or_default().to_string(),
            error["error"]
                .as_str()
                .or(error["message"].as_str())
                .unwrap_or_default()
                .to_string(),
        )
    }

    /// The comment bodies on `id` as bravo itself reads them.
    fn comments_on_bravo(&self, id: &str) -> Vec<String> {
        comments_in(&self.remote_runtime, id)
    }

    /// The comment bodies on `id` in host A's own store.
    fn comments_here(&self, id: &str) -> Vec<String> {
        comments_in(&self.local_runtime, id)
    }

    /// The remote commands dialed since the last call, then forget them.
    fn take_remote_argv(&self) -> Vec<String> {
        let log = self.fleet.routes.with_extension("argv");
        let lines = fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(ToOwned::to_owned)
            .collect();
        fs::write(&log, "").expect("reset the argv log");
        lines
    }

    /// The raw host-qualified token for bravo's workspace, as federated
    /// discovery lists it.
    fn bravo_selector(&self) -> String {
        let shown = self.fleet.json(&["host", "show", "bravo", "--json"]);
        let workspace = shown["workspaces"]
            .as_array()
            .and_then(|workspaces| workspaces.iter().find(|ws| ws["name"] == "bravo-ws"))
            .unwrap_or_else(|| panic!("bravo lists bravo-ws: {shown}"));
        format!(
            "{}/{}",
            self.bravo.machine_id,
            workspace["id"].as_str().expect("workspace id")
        )
    }
}

fn parse(stdout: &[u8], args: &[&str]) -> Value {
    serde_json::from_slice(stdout).unwrap_or_else(|error| {
        panic!(
            "{args:?} JSON: {error}: {}",
            String::from_utf8_lossy(stdout)
        )
    })
}

// Setup and state probes use the owning public runtime; the routed CLI/MCP
// calls below still exercise parsing, transport, output and refusals.
fn owner_runtime(home: &Path, repo: &Path) -> OrbitRuntime {
    orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_resolved_roots(
        orbit_core::runtime::OrbitRuntimeRoots {
            global_root: home.join(".orbit"),
            shared_root: repo.join(".orbit"),
            local_root: repo.join(".orbit"),
        },
    )
    .expect("registered fixture owner runtime")
}

fn add_task(runtime: &OrbitRuntime, title: &str) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.into(),
            complexity: TaskComplexity::Low,
            ..Default::default()
        })
        .expect("fixture task")
        .id
        .to_string()
}

fn comments_in(runtime: &OrbitRuntime, id: &str) -> Vec<String> {
    runtime
        .get_task_comments(id)
        .expect("owning task comments")
        .into_iter()
        .map(|comment| comment.message)
        .collect()
}

fn task_show_projections_match_the_owning_host() {
    let routed = Routed::new();
    let remote = routed.remote_task.as_str();
    let selector = routed.bravo_selector();
    routed
        .remote_runtime
        .update_task_with_identity(
            remote,
            TaskUpdateParams {
                comment: Some("a comment for the array projection".into()),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("owning runtime fixture comment");

    let local_json = |fields: &str| {
        let args = ["task", "show", remote, "--fields", fields, "--json"];
        let output = routed
            .fleet
            .orbit_in(&routed.bravo.home, &routed.bravo.repo(), &args)
            .output()
            .expect("spawn local task show on bravo");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse(&output.stdout, &args)
    };
    let routes: &[&[&str]] = &[
        &[],
        &["--workspace", &selector],
        &["--host", "bravo", "--workspace", "bravo-ws"],
    ];
    let mut local_projections: std::collections::BTreeMap<&str, Value> =
        std::collections::BTreeMap::new();
    for fields in [
        "title",
        "comments",
        "readiness",
        "terminal",
        "title,comments,readiness,terminal",
    ] {
        let local = local_json(fields);
        match fields {
            "title" => assert_eq!(local, "held by bravo"),
            "comments" => assert!(!local.as_array().expect("comments array").is_empty()),
            "readiness" => {
                assert!(local["ready"].is_boolean(), "readiness: {local}");
                assert!(local["gaps"].is_array(), "readiness: {local}");
            }
            "terminal" => assert_eq!(local, json!({ "terminal": false })),
            _ => {
                let object = local.as_object().expect("multiple fields stay keyed");
                assert_eq!(object.len(), 4);
                for field in fields.split(',') {
                    let single = &local_projections[field];
                    let expected = if field == "terminal" {
                        &single[field]
                    } else {
                        single
                    };
                    assert_eq!(&object[field], expected);
                }
            }
        }
        // The task stays unchanged after its initial comment. Keep the actual
        // local CLI results for multi-field and human projection comparisons.
        local_projections.insert(fields, local.clone());
        let args = ["task", "show", remote, "--fields", fields, "--json"];
        assert_eq!(routed.json(&args), local, "{args:?}");
    }

    // All projection shapes cross the prefix route above. The other actual
    // selector spellings converge before the same renderer; compare every
    // field in one real payload on each, preserving destination/forwarding.
    for route in &routes[1..] {
        let args: Vec<_> = [
            "task",
            "show",
            remote,
            "--fields",
            "title,comments,readiness,terminal",
            "--json",
        ]
        .into_iter()
        .chain(route.iter().copied())
        .collect();
        assert_eq!(
            routed.json(&args),
            local_projections["title,comments,readiness,terminal"],
            "{args:?}"
        );
    }

    let readiness = &local_projections["readiness"];
    let args = ["task", "show", remote, "--fields", "readiness"];
    let output = routed.fleet.orbit_ok(&routed.fleet.local_repo, &args);
    let text = String::from_utf8(output.stdout).expect("human task show is UTF-8");
    let value = text
        .trim()
        .strip_prefix("readiness: ")
        .unwrap_or_else(|| panic!("readiness field line: {text}"));
    let shown: Value = serde_json::from_str(value).expect("full readiness object");
    assert_eq!(&shown, readiness, "{args:?}");
}

/// A run-blocked task with a recorded final-recovery decision. The decision
/// does not replace the workflow-failure block `next_step` reads.
fn block_with_final_recovery(runtime: &OrbitRuntime, repo: &Path, id: &str, run_id: &str) {
    runtime
        .apply_task_automation_update(
            id,
            TaskAutomationUpdate {
                job_run_id: Some(run_id.into()),
                ..blocked_workflow_failure_update(
                    "task_pr_pipeline",
                    run_id,
                    Some("STEP_FAILED"),
                    Some("step `implement` completed with success=false"),
                )
            },
        )
        .unwrap();
    let task = runtime.get_task(id).unwrap();
    let observed = runtime.final_recovery_revision(&task).unwrap();
    let outcome = runtime
        .apply_final_recovery(
            &FinalRecoveryRequest {
                task_id: id.to_string(),
                run_id: run_id.to_string(),
                observed,
                repo_root: repo.to_path_buf(),
                base_ref: "main".into(),
                completion: FinalRecoveryCompletion::Review,
                requeue_bound: Default::default(),
            },
            Some(&json!({
                "decision": "escalate",
                "diagnosis": "the failure needs an operator",
                "human_action": "move the task back to backlog after fixing the cause",
            })),
        )
        .unwrap();
    assert_eq!(outcome, FinalRecoveryOutcome::Escalated { reason: None });
}

fn guidance_lines(id: &str, run_id: &str, at: &str) -> (String, String) {
    let formatted = DateTime::parse_from_rfc3339(at)
        .unwrap()
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    (
        format!(
            "Next: move the task back to backlog with `orbit task update {id} --status backlog` to run it again"
        ),
        format!("Final Recovery: escalate (escalated) by run {run_id} at {formatted}"),
    )
}

/// Routed `orbit task show` text prints the same `Next:` and `Final Recovery:`
/// words as the local CLI, and its JSON carries both objects.
fn routed_task_show_prints_the_same_blocked_guidance() {
    let routed = Routed::new();
    let id = add_task(&routed.remote_runtime, "blocked on bravo");
    let run_id = "jrun-routed-guidance";
    block_with_final_recovery(&routed.remote_runtime, &routed.bravo.repo(), &id, run_id);
    let shown = routed.json(&["task", "show", &id, "--json"]);
    assert_eq!(shown["host"]["name"], "bravo");
    assert_eq!(
        shown["next_step"],
        json!({
            "run_id": run_id,
            "resume_command": null,
            "requeue_command": format!("orbit task update {id} --status backlog"),
        })
    );
    assert_eq!(shown["final_recovery"]["decision"], "escalate");
    assert_eq!(shown["final_recovery"]["outcome"], "escalated");
    assert_eq!(shown["final_recovery"]["run_id"], run_id);
    let output = routed
        .fleet
        .orbit_ok(&routed.fleet.local_repo, &["task", "show", &id]);
    let text = String::from_utf8(output.stdout).expect("routed task show is UTF-8");
    let (next_line, recovery_line) =
        guidance_lines(&id, run_id, shown["final_recovery"]["at"].as_str().unwrap());
    assert!(
        text.lines().any(|line| line == next_line),
        "routed show Next: line:\n{text}"
    );
    assert!(
        text.lines().any(|line| line == recovery_line),
        "routed show Final Recovery: line:\n{text}"
    );
}

fn cli_task_ids_route_to_the_host_their_prefix_names() {
    let routed = Routed::new();
    let remote = routed.remote_task.as_str();
    routed.take_remote_argv();

    let shown = routed.json(&["task", "show", remote, "--json"]);
    assert_eq!(shown["id"], remote);
    assert_eq!(shown["host"]["name"], "bravo", "B answered: {shown}");
    assert_eq!(shown["host"]["machine_id"], routed.bravo.machine_id);
    assert_eq!(
        shown["workspace"]["name"], "bravo-ws",
        "the answer reports B's workspace: {shown}"
    );

    let updated = routed.json(&[
        "task",
        "update",
        remote,
        "--comment",
        "routed from A",
        "--json",
    ]);
    assert_eq!(updated["host"]["name"], "bravo");
    assert!(
        routed
            .comments_on_bravo(remote)
            .contains(&"routed from A".to_string()),
        "the comment landed in B's store"
    );

    let tool = routed.json(&[
        "tool",
        "run",
        "orbit.task.show",
        "--input",
        &json!({ "id": remote }).to_string(),
    ]);
    assert_eq!(tool["id"], remote, "`tool run` routes the same way: {tool}");
    assert!(
        !routed.take_remote_argv().is_empty(),
        "the remote calls went over SSH"
    );

    let local = routed.json(&["task", "show", &routed.local_task, "--json"]);
    assert_eq!(local["id"], routed.local_task);
    assert!(local.get("host").is_none(), "a local id runs in-process");
    assert!(
        routed.take_remote_argv().is_empty(),
        "a local-prefix id opens no remote session"
    );

    let (code, message) = routed.refused(&["task", "show", "ZZ-1", "--json"]);
    assert_eq!(code, "unknown_task_prefix", "{message}");
    assert!(
        message.contains("ZZ"),
        "the refusal names the prefix: {message}"
    );
    assert!(
        routed.take_remote_argv().is_empty(),
        "an unregistered prefix dials no host"
    );

    routed.fleet.unroute("bravo");
    let (code, message) = routed.refused(&["task", "show", remote, "--json"]);
    assert_eq!(
        code, "owner_unreachable",
        "an unreachable owner is not a missing task: {message}"
    );
    assert!(
        message.contains("bravo"),
        "the refusal names the host: {message}"
    );
}

fn federated_mcp_routes_an_id_only_call_by_prefix() {
    let routed = Routed::new();
    let remote = routed.remote_task.as_str();
    let mut client = StdioClient::spawn(&routed.fleet, &["mcp", "serve", "--mode", "federated"]);

    let shown = client.call_ok("orbit_task_show", json!({ "id": remote }));
    assert_eq!(shown["id"], remote);
    assert_eq!(
        shown["workspace"]["name"], "bravo-ws",
        "an id-only federated read reaches B: {shown}"
    );
    client.call_ok(
        "orbit_task_update",
        json!({ "id": remote, "comment": "routed over federated MCP" }),
    );
    assert!(
        routed
            .comments_on_bravo(remote)
            .contains(&"routed over federated MCP".to_string()),
        "the federated write landed in B's store"
    );

    routed.take_remote_argv();
    let local = client.call_ok("orbit_task_show", json!({ "id": routed.local_task }));
    assert_eq!(local["id"], routed.local_task);
    assert!(
        routed.take_remote_argv().is_empty(),
        "a local-prefix id is served in-process"
    );

    let unknown = client.call_err("orbit_task_show", json!({ "id": "ZZ-1" }));
    assert_eq!(unknown["code"], "unknown_task_prefix", "{unknown}");
    drop(client);

    // A v1 server does not relay: it names the holder instead of reporting
    // the task missing.
    let mut v1 = StdioClient::spawn(&routed.fleet, &["mcp", "serve"]);
    routed.take_remote_argv();
    let refused = v1.call_err("orbit_task_show", json!({ "id": remote }));
    assert_eq!(refused["code"], "task_prefix_remote", "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .is_some_and(|message| message.contains("bravo")),
        "the refusal names the holder: {refused}"
    );
    assert!(
        routed.take_remote_argv().is_empty(),
        "a v1 server dials nothing"
    );
}

fn a_routed_call_carries_the_callers_authority_and_no_more() {
    let routed = Routed::new();
    let remote = routed.remote_task.as_str();
    let reset = [
        "task",
        "review-reset",
        remote,
        "--lineage",
        "review:none",
        "--reason",
        "routed",
        "--json",
    ];
    routed.take_remote_argv();

    // An agent that also sets the operator override is still an agent: the
    // destination is asked for no operator session and refuses an
    // operator-only call as it refuses any agent's.
    let (code, message) = routed.refused_with(
        &reset,
        &[
            ("ORBIT_OPERATOR", "1"),
            // A canonical family: a non-canonical name logs an attribution
            // warning on stderr, which is not the refusal JSON this parses.
            ("ORBIT_AGENT_NAME", "codex"),
        ],
    );
    let dialed = routed.take_remote_argv();
    assert!(
        !dialed.is_empty() && dialed.iter().all(|line| !line.contains("--operator")),
        "an agent session never asks for operator authority: {dialed:?}"
    );
    assert!(
        ["capability_denied", "capability_refused"].contains(&code.as_str()),
        "B refuses the agent: {code}: {message}"
    );

    // The same call as an operator gets past authorization on B and fails
    // on the missing lineage instead.
    let (operator_code, operator_message) = routed.refused_with(&reset, &[("ORBIT_OPERATOR", "1")]);
    let dialed = routed.take_remote_argv();
    assert!(
        dialed.iter().any(|line| line.contains("--operator")),
        "an operator CLI is an operator on B: {dialed:?}"
    );
    assert!(
        !["capability_denied", "capability_refused"].contains(&operator_code.as_str()),
        "the operator gets past authorization on B: {operator_code}: {operator_message}"
    );
    assert_eq!(
        operator_code, "invalid_input",
        "the operator fails on lineage resolution on B: {operator_message}"
    );
    assert!(
        operator_message.contains("lineage"),
        "the operator refusal names the missing lineage (backend.rs review_reset no-ledger): {operator_message}"
    );
}

fn a_write_through_a_mirror_selector_is_refused_by_that_hosts_sole_writer_rule() {
    let routed = Routed::new();
    // B keeps a replica checkout of A's workspace: a mirror of A's tasks.
    let mirror = routed.bravo.home.join("mirror-of-a");
    git_repo::init(&mirror);
    let init = routed
        .fleet
        .orbit_in(
            &routed.bravo.home,
            &mirror,
            &[
                "workspace",
                "init",
                "--name",
                "a-mirror",
                "--role",
                "replica",
                "--owner",
                &routed.fleet.local.machine_id,
            ],
        )
        .output()
        .expect("spawn orbit");
    assert!(
        init.status.success(),
        "replica init on B: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let local = routed.local_task.as_str();
    let (code, message) = routed.refused(&[
        "task",
        "update",
        local,
        "--host",
        "bravo",
        "--workspace",
        "a-mirror",
        "--comment",
        "written on a mirror",
        "--json",
    ]);
    // The route refuses on the capability classes B advertises for its
    // replica checkout, before any write is delivered.
    assert_eq!(code, "capability_refused", "B's replica refuses: {message}");
    assert!(
        message.contains("control_plane"),
        "the refusal is the replica's write class: {message}"
    );
    // And B's own store refuses the same write when it is handed one.
    let direct = routed
        .fleet
        .orbit_in(
            &routed.bravo.home,
            &mirror,
            &[
                "task",
                "update",
                local,
                "--workspace",
                "a-mirror",
                "--comment",
                "x",
                "--json",
            ],
        )
        .output()
        .expect("spawn orbit");
    assert!(!direct.status.success(), "B refuses a write on its mirror");
    let refusal: Value = serde_json::from_slice(&direct.stderr)
        .or_else(|_| serde_json::from_slice(&direct.stdout))
        .expect("refusal JSON");
    assert_eq!(refusal["code"], "capability_refused", "{refusal}");
    assert!(
        refusal["error"]
            .as_str()
            .is_some_and(|error| error.contains(&routed.fleet.local.machine_id)),
        "B names the task's writer, this host: {refusal}"
    );
    assert!(
        !routed
            .comments_here(local)
            .contains(&"written on a mirror".to_string()),
        "nothing reached the owner's copy"
    );
}

fn local_task_commands_refuse_remote_selection_without_delivering_or_mutating() {
    let routed = Routed::new();
    let selector = routed.bravo_selector();
    let local_before = routed.local_runtime.list_tasks().expect("local tasks");
    let remote_before = routed.remote_runtime.list_tasks().expect("remote tasks");
    assert!(local_before.iter().any(|task| task.id == routed.local_task));
    assert!(
        remote_before
            .iter()
            .any(|task| task.id == routed.remote_task)
    );
    routed.take_remote_argv();

    for command in [
        vec!["task", "list"],
        vec![
            "task",
            "add",
            "--title",
            "must not be created",
            "--complexity",
            "low",
        ],
    ] {
        for host_flag in [vec!["--host", "bravo"], vec!["--host=bravo"]] {
            let mut args = command.clone();
            args.extend(host_flag);
            args.extend(["--workspace", "bravo-ws", "--json"]);
            let (code, message) = routed.refused(&args);
            assert_eq!(code, "usage_error", "{args:?}: {message}");
            assert!(message.contains("--host"), "the refusal names the flag");
            let mut expected = vec!["orbit"];
            expected.extend(command.iter().copied());
            expected.extend(["--workspace", "bravo-ws", "--json"]);
            assert_eq!(
                ssh_hint_remote_argv(&message),
                expected,
                "the refusal gives the original command on the registered host: {message}"
            );
        }

        let mut args = command;
        args.extend(["--workspace", &selector, "--json"]);
        let (code, message) = routed.refused(&args);
        assert_eq!(code, "invalid_input", "{args:?}: {message}");
        assert!(
            message.contains("--workspace"),
            "the refusal names the selector flag"
        );
        assert!(
            message.contains("--host"),
            "the refusal explains the host restriction"
        );
        assert!(
            message.contains("orbit tool run"),
            "the refusal names the supported remote surface: {message}"
        );
    }

    assert!(
        routed.take_remote_argv().is_empty(),
        "refused local commands never contact the remote host"
    );
    assert_eq!(
        routed.local_runtime.list_tasks().expect("local tasks"),
        local_before,
        "refused commands leave the local tasks unchanged"
    );
    assert_eq!(
        routed.remote_runtime.list_tasks().expect("remote tasks"),
        remote_before,
        "refused commands leave the remote tasks unchanged"
    );
}

/// The backtick-quoted commands a refusal message offers, in order.
fn hinted_commands(message: &str) -> Vec<String> {
    message
        .split('`')
        .skip(1)
        .step_by(2)
        .map(ToOwned::to_owned)
        .collect()
}

fn routed_task_update_refusals_offer_a_command_that_works() {
    let routed = Routed::new();
    let id = routed.remote_task.as_str();
    let bravo_before = routed.comments_on_bravo(id);
    routed.take_remote_argv();
    let ssh_argv = |hint: &str| {
        let words = posix_shell_split(hint);
        assert_eq!(&words[..2], &["ssh", "bravo"], "{hint}");
        posix_shell_split(&words[2..].join(" "))
    };

    // `--artifact` is not an `orbit.task.update` field; the routed command is
    // `orbit task artifact put`, one per artifact path.
    let (code, message) = routed.refused(&[
        "task",
        "update",
        id,
        "--artifact",
        "notes.md=a=b",
        "--artifact",
        "dir/two words.md=x",
        "--json",
    ]);
    assert_eq!(code, "invalid_input", "{message}");
    let puts = hinted_commands(&message)
        .iter()
        // The placeholder is for the caller to fill in; make it one shell word.
        .map(|hint| posix_shell_split(&hint.replace("<source-file>", "SRC")))
        .collect::<Vec<_>>();
    assert_eq!(
        puts,
        vec![
            vec![
                "orbit", "task", "artifact", "put", id, "SRC", "--path", "notes.md"
            ],
            vec![
                "orbit",
                "task",
                "artifact",
                "put",
                id,
                "SRC",
                "--path",
                "dir/two words.md"
            ],
        ],
        "each artifact gets its routed put: {message}"
    );

    // `--approve` is a human lifecycle action on the owning host.
    let (code, message) = routed.refused(&["task", "update", id, "--approve", "--json"]);
    assert_eq!(code, "invalid_input", "{message}");
    let hints = hinted_commands(&message);
    assert_eq!(hints.len(), 1, "{message}");
    assert_eq!(
        ssh_argv(&hints[0]),
        ["orbit", "task", "update", id, "--approve"],
        "{message}"
    );
    let (_, message) = routed.refused(&[
        "task",
        "update",
        id,
        "--approve",
        "--note",
        "ok by me",
        "--json",
    ]);
    assert_eq!(
        ssh_argv(&hinted_commands(&message)[0]),
        [
            "orbit",
            "task",
            "update",
            id,
            "--approve",
            "--note",
            "ok by me"
        ],
        "the note survives both shells: {message}"
    );

    // `--force` and `--discard-candidate` keep the ssh remedy.
    for flag in [
        vec!["--status", "backlog", "--force"],
        vec!["--discard-candidate"],
    ] {
        let mut args = vec!["task", "update", id];
        args.extend(&flag);
        args.push("--json");
        let (code, message) = routed.refused(&args);
        assert_eq!(code, "invalid_input", "{args:?}: {message}");
        let hints = hinted_commands(&message);
        assert_eq!(hints.len(), 1, "{message}");
        assert_eq!(
            ssh_argv(&hints[0]),
            ["orbit", "task", "update", id, "…"],
            "{message}"
        );
    }

    assert!(
        routed.take_remote_argv().is_empty(),
        "refused updates never contact the owning host"
    );
    assert_eq!(
        routed.comments_on_bravo(id),
        bravo_before,
        "refused updates write nothing on the owning host"
    );
}

fn host_flag_resolves_to_the_selector_that_host_lists() {
    let routed = Routed::new();
    let remote = routed.remote_task.as_str();
    let selector = routed.bravo_selector();

    let by_host = routed.json(&[
        "task",
        "show",
        remote,
        "--host",
        "bravo",
        "--workspace",
        "bravo-ws",
        "--json",
    ]);
    let by_selector = routed.json(&["task", "show", remote, "--workspace", &selector, "--json"]);
    assert_eq!(by_host, by_selector, "`--host` is the raw selector");
    assert_eq!(by_host["workspace"]["name"], "bravo-ws");

    let list_input = |workspace: &str| json!({ "workspace": workspace }).to_string();
    let by_host = routed.json(&[
        "tool",
        "run",
        "orbit.task.list",
        "--host",
        &routed.bravo.machine_id,
        "--input",
        &list_input("bravo-ws"),
    ]);
    let by_selector = routed.json(&[
        "tool",
        "run",
        "orbit.task.list",
        "--input",
        &list_input(&selector),
    ]);
    assert_eq!(
        by_host, by_selector,
        "a host's machine id resolves the same way"
    );

    // `--pull` resolves to the same selector, so both forms meet the same
    // pull admission on this (non-replica) checkout.
    let pull = |args: &[&str]| {
        let output = routed.fleet.orbit(&routed.fleet.local_repo, args);
        assert!(
            !output.status.success(),
            "{args:?}: this checkout is no replica"
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    assert_eq!(
        pull(&["run", "auto", "--pull", "bravo-ws", "--host", "bravo"]),
        pull(&["run", "auto", "--pull", &selector]),
        "`--pull <ws> --host` meets the raw selector's admission"
    );

    routed.take_remote_argv();
    let (code, _) = routed.refused(&[
        "task",
        "update",
        remote,
        "--host",
        "nowhere",
        "--workspace",
        "bravo-ws",
        "--comment",
        "never",
        "--json",
    ]);
    assert_eq!(code, "unknown_host");
    assert!(
        routed.take_remote_argv().is_empty(),
        "an unknown host dials nothing"
    );

    let (code, message) = routed.refused(&[
        "task",
        "update",
        remote,
        "--host",
        "bravo",
        "--workspace",
        "no-such-ws",
        "--comment",
        "never",
        "--json",
    ]);
    assert_eq!(code, "stale_route", "{message}");
    assert!(
        message.contains("bravo-ws"),
        "the refusal lists what the host does list: {message}"
    );
    assert!(
        !routed
            .comments_on_bravo(remote)
            .contains(&"never".to_string()),
        "a refused resolution delivers nothing"
    );

    let (code, message) = routed.refused(&["run", "auto", "--pull", "bravo-ws", "--json"]);
    assert!(
        message.contains("--host"),
        "a bare `--pull` workspace asks for its host: {code}: {message}"
    );

    let output = routed.fleet.orbit(
        &routed.fleet.local_repo,
        &["workspace", "list", "--host", "bravo"],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "a host-local command takes no --host"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ssh bravo orbit workspace list"),
        "the usage error says where to run it: {stderr}"
    );

    routed.fleet.unroute("bravo");
    let (code, message) = routed.refused(&[
        "task",
        "show",
        remote,
        "--host",
        "bravo",
        "--workspace",
        "bravo-ws",
        "--json",
    ]);
    assert_eq!(code, "unreachable_destination", "{message}");
}

/// Parse a command string using POSIX shell word splitting and quote removal.
fn posix_shell_split(command: &str) -> Vec<String> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg("eval \"set -- $1\"; for arg in \"$@\"; do printf '%s\\0' \"$arg\"; done")
        .arg("sh")
        .arg(command)
        .output()
        .expect("run sh to split words");
    assert!(output.status.success(), "sh split failed: {output:?}");
    let slice = output
        .stdout
        .strip_suffix(&[0])
        .expect("shell emits NUL-terminated words");
    slice
        .split(|&b| b == 0)
        .map(|chunk| String::from_utf8(chunk.to_vec()).expect("argv is UTF-8"))
        .collect()
}

fn ssh_hint_remote_argv(message: &str) -> Vec<String> {
    let hint_prefix = "run it on that host: `";
    let start = message
        .find(hint_prefix)
        .expect("error contains host suggestion")
        + hint_prefix.len();
    let end = message[start..]
        .rfind('`')
        .expect("closing backtick for suggestion")
        + start;
    let local_words = posix_shell_split(&message[start..end]);
    assert_eq!(&local_words[..2], &["ssh", "bravo"]);
    // SSH joins the local command arguments with spaces before the remote
    // login shell parses them. Both shell boundaries must preserve the argv.
    posix_shell_split(&local_words[2..].join(" "))
}

fn host_ssh_command_hint_quotes_metacharacters_and_roundtrips_posix_argv() {
    let routed = Routed::new();
    let marker = routed.fleet.temp.path().join("injected_marker");
    let titles = [
        format!("fix Daniel's build; touch {}", marker.display()),
        format!("fix the build; touch {}", marker.display()),
        format!(
            "literal $HOME $(touch {}) `touch {}` * \"quoted\"",
            marker.display(),
            marker.display()
        ),
        "".to_string(),
        "~/build # comment".to_string(),
        "修复 build".to_string(),
    ];

    for title in titles {
        let output = routed.fleet.orbit(
            &routed.fleet.local_repo,
            &[
                "task",
                "add",
                "--title",
                &title,
                "--complexity",
                "low",
                "--host",
                "bravo",
            ],
        );
        assert_eq!(
            output.status.code(),
            Some(2),
            "a host-local command takes no --host"
        );
        assert!(
            !marker.exists(),
            "shell metacharacters must not execute when constructing the command"
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            ssh_hint_remote_argv(&stderr),
            &[
                "orbit",
                "task",
                "add",
                "--title",
                &title,
                "--complexity",
                "low"
            ],
            "hint round-trips through POSIX shell word splitting to the original argv"
        );
        assert!(
            !marker.exists(),
            "evaluating the hint through a POSIX shell must not execute embedded metacharacters"
        );
    }
}

/// A line-delimited MCP client over a spawned server's stdio.
struct StdioClient {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    next_id: i64,
}

impl StdioClient {
    fn spawn(fleet: &Fleet, args: &[&str]) -> Self {
        let mut child = fleet
            .process(&fleet.local.home, &fleet.local_repo, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn MCP server");
        let stdin = child.stdin.take().expect("server stdin");
        let stdout = child.stdout.take().expect("server stdout");
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            lines,
            next_id: 0,
        };
        client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "host-routing", "version": "0" },
            }),
        );
        client.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        client
    }

    fn send(&mut self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .expect("write to server");
        self.stdin.flush().expect("flush server stdin");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|error| panic!("no answer to {method}: {error}"));
            let Ok(response) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if response["id"].as_i64() == Some(id) {
                return response;
            }
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{name} returned no result: {response}"))
    }

    fn call_ok(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.call(name, arguments);
        assert_eq!(result["isError"], false, "{name} failed: {result}");
        result["structuredContent"].clone()
    }

    fn call_err(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.call(name, arguments);
        assert_eq!(result["isError"], true, "{name} succeeded: {result}");
        result["structuredContent"].clone()
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn host_routing_contracts_0() {
    if crate::isolated_test::run_in_child("host_routing::host_routing_contracts_0") {
        return;
    }
    for (name, check) in [
        (
            "task_show_projections_match_the_owning_host",
            task_show_projections_match_the_owning_host as fn(),
        ),
        (
            "cli_task_ids_route_to_the_host_their_prefix_names",
            cli_task_ids_route_to_the_host_their_prefix_names as fn(),
        ),
        (
            "federated_mcp_routes_an_id_only_call_by_prefix",
            federated_mcp_routes_an_id_only_call_by_prefix as fn(),
        ),
        (
            "a_routed_call_carries_the_callers_authority_and_no_more",
            a_routed_call_carries_the_callers_authority_and_no_more as fn(),
        ),
        (
            "routed_task_show_prints_the_same_blocked_guidance",
            routed_task_show_prints_the_same_blocked_guidance as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}

#[test]
fn host_routing_contracts_1() {
    if crate::isolated_test::run_in_child("host_routing::host_routing_contracts_1") {
        return;
    }
    for (name, check) in [
        (
            "a_write_through_a_mirror_selector_is_refused_by_that_hosts_sole_writer_rule",
            a_write_through_a_mirror_selector_is_refused_by_that_hosts_sole_writer_rule as fn(),
        ),
        (
            "local_task_commands_refuse_remote_selection_without_delivering_or_mutating",
            local_task_commands_refuse_remote_selection_without_delivering_or_mutating as fn(),
        ),
        (
            "routed_task_update_refusals_offer_a_command_that_works",
            routed_task_update_refusals_offer_a_command_that_works as fn(),
        ),
        (
            "host_flag_resolves_to_the_selector_that_host_lists",
            host_flag_resolves_to_the_selector_that_host_lists as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}

#[test]
fn host_routing_contracts_2() {
    if crate::isolated_test::run_in_child("host_routing::host_routing_contracts_2") {
        return;
    }
    tracing::info!(
        case = "host_ssh_command_hint_quotes_metacharacters_and_roundtrips_posix_argv",
        "CLI contract row"
    );
    host_ssh_command_hint_quotes_metacharacters_and_roundtrips_posix_argv();
}
