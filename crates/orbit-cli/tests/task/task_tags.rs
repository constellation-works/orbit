#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::process::Output;

use serde_json::{Value, json};

fn task_cli_roundtrips_filters_and_replaces_tags() {
    let workspace = workspace();
    let perf = workspace.add_task_cli_with_tags("Perf task", &["perf"]);
    workspace.add_task_with_tags("Bench task", &["bench"]);
    let both = workspace.add_task_cli_with_tags("Perf bench task", &["  Perf ", "BENCH"]);

    assert_eq!(both["tags"], json!(["perf", "bench"]));

    // ORB-10310: freshly-added tasks are `proposed`; status-neutral listing must
    // surface them without `--all` or `--status proposed`.
    let perf_list = workspace.run_with_stdin(
        &["task", "list", "--tag", "perf", "--json"],
        None,
        "list perf tasks",
    );
    assert_task_titles(&perf_list, &["Perf task", "Perf bench task"]);

    // ORB-00202: `orbit task search` was deleted; the substring+tag case
    // migrates to `orbit search --kind task --tag <...>`. Results land
    // under `output["results"]` rather than at the top level.
    let both_search = workspace.run_with_stdin(
        &[
            "search",
            "tag-search",
            "--kind",
            "task",
            "--tag",
            "perf",
            "--tag",
            "bench",
            "--json",
        ],
        None,
        "orbit search perf+bench tasks",
    );
    assert_orbit_search_titles(&both_search, &["Perf bench task"]);

    let perf_id = perf["id"].as_str().expect("perf task id");
    let updated = workspace.run_with_stdin(
        &["task", "update", perf_id, "--tag", "docs", "--json"],
        None,
        "replace tags",
    );
    let updated: Value = serde_json::from_slice(&updated.stdout).expect("update JSON");
    assert_eq!(updated["tags"], json!(["docs"]));
}

fn search_status_modes_include_blocked_and_deferred_tasks() {
    let workspace = workspace();
    let routing = workspace.run_with_stdin(
        &["workspace", "show", "--format", "json"],
        None,
        "verify fixture routing",
    );
    let routing: Value = serde_json::from_slice(&routing.stdout).expect("workspace JSON");
    assert_eq!(routing["registered"], true);
    assert_eq!(
        routing["checkout"]["repo_root"],
        fs::canonicalize(&workspace.work)
            .expect("canonical fixture checkout")
            .to_string_lossy()
            .to_string()
    );

    let seeds = [
        ("Proposed task", orbit_types::task::TaskStatus::Proposed),
        ("Blocked task", orbit_types::task::TaskStatus::Blocked),
        ("Someday task", orbit_types::task::TaskStatus::Someday),
        ("Rejected task", orbit_types::task::TaskStatus::Rejected),
    ]
    .into_iter()
    .map(|(title, status)| {
        let mut seed = workspace.task_seed(title);
        seed.status = status;
        seed
    })
    .collect::<Vec<_>>();
    workspace.seed_tasks(&seeds);

    // Exercise both public entry points. Explicit statuses must keep their
    // precedence over all, including the open alias and comma-separated sets.
    for (all, status, expected) in [
        (false, None, vec!["Proposed task", "Blocked task"]),
        (
            true,
            None,
            vec![
                "Proposed task",
                "Blocked task",
                "Someday task",
                "Rejected task",
            ],
        ),
        (
            true,
            Some("task:open"),
            vec!["Proposed task", "Blocked task"],
        ),
        (true, Some("task:someday"), vec!["Someday task"]),
        (
            true,
            Some("task:blocked,task:someday"),
            vec!["Blocked task", "Someday task"],
        ),
    ] {
        let mut args = vec!["search", "tag-search", "--kind", "task", "--json"];
        let mut input = json!({"query": "tag-search", "kind": "task", "all": all});
        if all {
            args.push("--all");
        }
        if let Some(status) = status {
            args.extend(["--status", status]);
            input["status"] = json!([status]);
        }
        let cli = workspace.run_with_stdin(&args, None, "CLI search status mode");
        assert_orbit_search_titles(&cli, &expected);

        let input = input.to_string();
        let tool = workspace.run_with_stdin(
            &["tool", "run", "orbit.search", "--input", &input],
            None,
            "tool search status mode",
        );
        assert_orbit_search_titles(&tool, &expected);
    }
}

/// ORB-10310: `orbit task list` is status-neutral and bounded by `--limit`.
fn task_list_is_status_neutral_and_bounded_by_limit() {
    let workspace = workspace();
    // Every task starts in `proposed`; the default listing must include them
    // all with no `--status`/`--all`.
    workspace.seed_tasks(
        &(0..3)
            .map(|i| workspace.task_seed(&format!("Task {i}")))
            .collect::<Vec<_>>(),
    );

    let default_list = workspace.run_with_stdin(&["task", "list", "--json"], None, "default list");
    let default_tasks: Value = serde_json::from_slice(&default_list.stdout).expect("list JSON");
    let default_items = default_tasks.as_array().expect("array");
    assert_eq!(
        default_items.len(),
        3,
        "all proposed tasks are listed by default: {default_tasks}"
    );

    // `--limit` bounds the response.
    let limited = workspace.run_with_stdin(
        &["task", "list", "--limit", "2", "--json"],
        None,
        "limited list",
    );
    let limited_tasks: Value = serde_json::from_slice(&limited.stdout).expect("list JSON");
    let limited_items = limited_tasks.as_array().expect("array");
    assert_eq!(limited_items.len(), 2);

    // A zero limit is a clear input error, not an empty success.
    let rejected = crate::task_workspace::run_orbit(
        &workspace.work,
        &workspace.home,
        &["task", "list", "--limit", "0"],
        None,
        false,
    );
    assert!(
        !rejected.status.success(),
        "`--limit 0` must be rejected: {}",
        String::from_utf8_lossy(&rejected.stdout)
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("limit"),
        "error must mention the limit: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

fn task_update_refuses_dropping_system_identity_tag_without_override() {
    let workspace = workspace();
    let task = workspace.add_task_with_tags(
        "CI failure task",
        &["ci-failure:8002487a4e972736", "ci-failure-sweep"],
    );
    let task_id = task["id"].as_str().expect("task id");

    // 1. Refusal with --json: fails, stderr has structured error with code and tag
    let rejected_json = workspace.run_raw(&[
        "task",
        "update",
        task_id,
        "--tag",
        "ci-failure-sweep",
        "--tag",
        "github-actions",
        "--json",
    ]);
    assert!(
        !rejected_json.status.success(),
        "dropping ci-failure tag without override must fail"
    );
    let err_val: Value = serde_json::from_slice(&rejected_json.stderr).expect("stderr JSON");
    assert_eq!(err_val["code"], "system_identity_tag_dropped");
    assert_eq!(err_val["tag"], "ci-failure:8002487a4e972736");
    assert!(
        err_val["error"]
            .as_str()
            .expect("error string")
            .contains("ci-failure:8002487a4e972736")
    );

    // 2. Refusal without --json: fails, stderr names the tag and override flag
    let rejected_plain = workspace.run_raw(&[
        "task",
        "update",
        task_id,
        "--tag",
        "ci-failure-sweep",
        "--tag",
        "github-actions",
    ]);
    assert!(!rejected_plain.status.success());
    let stderr = String::from_utf8_lossy(&rejected_plain.stderr);
    assert!(
        stderr.contains("ci-failure:8002487a4e972736"),
        "stderr must name the tag: {stderr}"
    );
    assert!(
        stderr.contains("allow-drop-system-tags"),
        "stderr must mention override flag: {stderr}"
    );

    // 3. Success with --allow-drop-system-tags
    let allowed = workspace.run_with_stdin(
        &[
            "task",
            "update",
            task_id,
            "--tag",
            "ci-failure-sweep",
            "--tag",
            "github-actions",
            "--allow-drop-system-tags",
            "--json",
        ],
        None,
        "update tags with override flag",
    );
    let updated: Value = serde_json::from_slice(&allowed.stdout).expect("update JSON");
    assert_eq!(
        updated["tags"],
        json!(["ci-failure-sweep", "github-actions"])
    );

    // 4. Keeping the system identity tag does not require the flag
    let kept = workspace.run_with_stdin(
        &[
            "task",
            "update",
            task_id,
            "--tag",
            "ci-failure:8002487a4e972736",
            "--tag",
            "other-tag",
            "--json",
        ],
        None,
        "update tags retaining system identity tag",
    );
    let kept_updated: Value = serde_json::from_slice(&kept.stdout).expect("update JSON");
    assert_eq!(
        kept_updated["tags"],
        json!(["ci-failure:8002487a4e972736", "other-tag"])
    );
}

#[test]
fn task_update_tool_refuses_dropping_system_identity_tag_without_override() {
    let workspace = workspace();
    let task =
        workspace.add_task_with_tags("Tool CI failure task", &["ci-failure:testkey123", "docs"]);
    let task_id = task["id"].as_str().expect("task id");

    // 1. Tool update dropping system tag without override fails
    let tool_reject = json!({
        "id": task_id,
        "tags": ["docs", "perf"]
    })
    .to_string();
    let rejected =
        workspace.run_raw(&["tool", "run", "orbit.task.update", "--input", &tool_reject]);
    assert!(
        !rejected.status.success(),
        "tool run dropping ci-failure tag without override must fail"
    );
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        stderr.contains("ci-failure:testkey123"),
        "stderr must name the tag: {stderr}"
    );

    // 2. Tool update with allow_drop_system_tags: true succeeds
    let tool_allow = json!({
        "id": task_id,
        "tags": ["docs", "perf"],
        "allow_drop_system_tags": true
    })
    .to_string();
    let allowed = workspace.run_with_stdin(
        &["tool", "run", "orbit.task.update", "--input", &tool_allow],
        None,
        "tool run dropping ci-failure tag with override",
    );
    let updated: Value = serde_json::from_slice(&allowed.stdout).expect("tool output JSON");
    assert_eq!(updated["tags"], json!(["docs", "perf"]));
}

/// [ORB-16855] `skip-review` and `skip-validation` skip delivery gates, so
/// only filing or an operator sets them: an agent's update, such as the
/// implementer's own during its run, is refused and changes nothing.
fn light_delivery_tags_are_set_only_by_filing_or_an_operator() {
    let workspace = workspace();
    let filed = workspace.add_task_cli_with_tags("Docs task", &["skip-review"]);
    assert_eq!(filed["tags"], json!(["skip-review"]), "filing sets the tag");
    let task_id = filed["id"].as_str().expect("task id");

    let input = json!({
        "id": task_id, "model": "claude",
        "tags": ["skip-review", "skip-validation"],
    })
    .to_string();
    let mut agent = assert_cmd::cargo::cargo_bin_cmd!("orbit");
    orbit_common::test_env::clear_inherited_authority(|name| {
        agent.env_remove(name);
    });
    let refused = agent
        .current_dir(&workspace.work)
        .env("HOME", &workspace.home)
        .env("USERPROFILE", &workspace.home)
        .env("ORBIT_AGENT_NAME", "claude")
        .env("ORBIT_AGENT_MODEL", "claude")
        .env("ORBIT_TASK_ID", task_id)
        .args(["tool", "run", "orbit.task.update", "--input", &input])
        .output()
        .expect("run orbit as the implementing agent");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "an agent adding `skip-validation` to its own task must be refused"
    );
    let error: Value = serde_json::from_slice(&refused.stderr).expect("stderr JSON");
    assert_eq!(error["code"], "capability_denied", "{stderr}");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|error| error.contains("skip-validation")),
        "names the tag: {stderr}"
    );
    let shown = workspace.run(&["task", "show", task_id, "--json"], "show task");
    let shown: Value = serde_json::from_slice(&shown.stdout).expect("show JSON");
    assert_eq!(
        shown["tags"],
        json!(["skip-review"]),
        "the refusal wrote nothing"
    );

    let approved = workspace.run_as_operator(
        &[
            "task",
            "update",
            task_id,
            "--tag",
            "skip-review",
            "--tag",
            "skip-validation",
            "--json",
        ],
        "operator adds skip-validation",
    );
    let approved: Value = serde_json::from_slice(&approved.stdout).expect("update JSON");
    assert_eq!(approved["tags"], json!(["skip-review", "skip-validation"]));
}

/// [ORB-16872] A user-launched MCP server has no agent envelope. A tool
/// update that still names an agent model is an attributed agent write, so
/// it cannot add `skip-review` or `skip-validation`, including when the write
/// is a guarded start. Filing, an operator update, and removal still succeed,
/// and a refusal leaves the stored tags unchanged.
fn attributed_agent_without_envelope_cannot_add_light_delivery_tags() {
    let workspace = workspace();
    let filed = workspace.add_task_cli_with_tags(
        "Filed with both light-delivery tags",
        &["skip-review", "skip-validation"],
    );
    assert_eq!(
        filed["tags"],
        json!(["skip-review", "skip-validation"]),
        "filing sets both tags"
    );

    let reviewed = workspace.add_task_cli_with_tags("Reviewed docs task", &["skip-review"]);
    let reviewed_id = reviewed["id"].as_str().expect("reviewed task id");
    let plain = workspace.add_task_cli_with_tags("Plain docs task", &[]);
    let plain_id = plain["id"].as_str().expect("plain task id");
    assert_eq!(
        plain["tags"],
        json!([]),
        "filing without the tags stores none"
    );

    let add_validation = run_tool_update_without_agent_envelope(
        &workspace,
        &json!({
            "id": reviewed_id,
            "model": "claude",
            "tags": ["skip-review", "skip-validation"],
        }),
    );
    assert_light_delivery_addition_refused(&add_validation, "skip-validation");
    assert_eq!(
        shown_tags(&workspace, reviewed_id),
        json!(["skip-review"]),
        "the refused update wrote nothing"
    );

    let add_review = run_tool_update_without_agent_envelope(
        &workspace,
        &json!({
            "id": plain_id,
            "model": "grok",
            "tags": ["skip-review"],
        }),
    );
    assert_light_delivery_addition_refused(&add_review, "skip-review");
    assert_eq!(
        shown_tags(&workspace, plain_id),
        json!([]),
        "the refused update wrote nothing"
    );

    let guarded_start = run_tool_update_without_agent_envelope(
        &workspace,
        &json!({
            "id": reviewed_id,
            "model": "codex",
            "status": "in-progress",
            "plan": "Exercise guarded-start admission.",
            "tags": ["skip-review", "skip-validation"],
        }),
    );
    assert_light_delivery_addition_refused(&guarded_start, "skip-validation");
    let shown = show_task(&workspace, reviewed_id);
    assert_eq!(
        shown["tags"],
        json!(["skip-review"]),
        "the refused start wrote nothing"
    );
    assert_eq!(
        shown["status"], "proposed",
        "the refused start did not move the task"
    );

    let removed = run_tool_update_without_agent_envelope(
        &workspace,
        &json!({
            "id": reviewed_id,
            "model": "claude",
            "tags": ["docs"],
        }),
    );
    assert!(
        removed.status.success(),
        "an attributed agent may remove a light-delivery tag\n{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let removed: Value = serde_json::from_slice(&removed.stdout).expect("removal JSON");
    assert_eq!(removed["tags"], json!(["docs"]));

    let approved = workspace.run_as_operator(
        &[
            "task",
            "update",
            reviewed_id,
            "--tag",
            "docs",
            "--tag",
            "skip-review",
            "--tag",
            "skip-validation",
            "--json",
        ],
        "operator adds both light-delivery tags",
    );
    let approved: Value = serde_json::from_slice(&approved.stdout).expect("operator update JSON");
    assert_eq!(
        approved["tags"],
        json!(["docs", "skip-review", "skip-validation"])
    );
}

/// Tool update in a child with no agent envelope and no operator override.
/// Attribution comes only from `model` in the tool input.
fn run_tool_update_without_agent_envelope(workspace: &TestWorkspace, input: &Value) -> Output {
    let payload = input.to_string();
    let mut command = assert_cmd::cargo::cargo_bin_cmd!("orbit");
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(&workspace.work)
        .env("HOME", &workspace.home)
        .env("USERPROFILE", &workspace.home)
        .args(["tool", "run", "orbit.task.update", "--input", &payload])
        .output()
        .expect("run attributed tool update without an agent envelope")
}

fn assert_light_delivery_addition_refused(output: &Output, tag: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "attributed agent adding `{tag}` without an agent envelope must be refused\n{stderr}"
    );
    let error: Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|_| {
        panic!("stderr is not the tool error JSON: {stderr}");
    });
    assert_eq!(error["code"], "capability_denied", "{stderr}");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains(tag)),
        "names `{tag}`: {stderr}"
    );
}

fn show_task(workspace: &TestWorkspace, task_id: &str) -> Value {
    let shown = workspace.run(&["task", "show", task_id, "--json"], "show task");
    serde_json::from_slice(&shown.stdout).expect("show JSON")
}

fn shown_tags(workspace: &TestWorkspace, task_id: &str) -> Value {
    show_task(workspace, task_id)["tags"].clone()
}

fn assert_task_titles(output: &Output, expected: &[&str]) {
    let value: Value = serde_json::from_slice(&output.stdout).expect("task array JSON");
    let tasks = value.as_array().expect("task array");
    let mut titles = tasks
        .iter()
        .map(|task| task["title"].as_str().expect("task title").to_string())
        .collect::<Vec<_>>();
    titles.sort();

    let mut expected = expected
        .iter()
        .map(|title| (*title).to_string())
        .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(titles, expected);
}

fn assert_orbit_search_titles(output: &Output, expected: &[&str]) {
    let response: Value = serde_json::from_slice(&output.stdout).expect("search response JSON");
    let mut titles = response["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|hit| hit["title"].as_str().expect("hit title").to_string())
        .collect::<Vec<_>>();
    titles.sort();

    let mut expected = expected
        .iter()
        .map(|title| (*title).to_string())
        .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(titles, expected);
}

use crate::task_workspace::{TaskSeed, TestWorkspace};

fn workspace() -> TestWorkspace {
    let mut template = TaskSeed::new("");
    template.description = "Shared tag-search marker.".into();
    TestWorkspace::new("task-tags-test", template)
}

#[test]
fn task_tags_status_filters_and_identity() {
    for (name, check) in [
        (
            "task_cli_roundtrips_filters_and_replaces_tags",
            task_cli_roundtrips_filters_and_replaces_tags as fn(),
        ),
        (
            "search_status_modes_include_blocked_and_deferred_tasks",
            search_status_modes_include_blocked_and_deferred_tasks as fn(),
        ),
        (
            "task_list_is_status_neutral_and_bounded_by_limit",
            task_list_is_status_neutral_and_bounded_by_limit as fn(),
        ),
        (
            "task_update_refuses_dropping_system_identity_tag_without_override",
            task_update_refuses_dropping_system_identity_tag_without_override as fn(),
        ),
        (
            "light_delivery_tags_are_set_only_by_filing_or_an_operator",
            light_delivery_tags_are_set_only_by_filing_or_an_operator as fn(),
        ),
        (
            "attributed_agent_without_envelope_cannot_add_light_delivery_tags",
            attributed_agent_without_envelope_cannot_add_light_delivery_tags as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}
