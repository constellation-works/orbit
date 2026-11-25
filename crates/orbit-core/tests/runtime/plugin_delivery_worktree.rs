//! Authenticated single-task worktree binding for deterministic plugin delivery
//! calls [ORB-28130].
//!
//! Verifies that a plugin delivery job can run a mutating plugin tool confined
//! to its admitted task worktree and complete the normal PR/CI delivery route
//! with no agent, that missing/mismatched/unowned/invalid/multi-task bindings
//! fail closed before spawning the plugin backend, and that existing plugin
//! calls outside deterministic task delivery keep their primary-checkout
//! routing.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_core::application::plugin::{PluginAddOptions, install_plugin};
use orbit_core::application::task::TaskAddParams;
use orbit_core::runtime::WorkspaceRuntimeBinding;
use orbit_core::{OrbitRuntime, ShipMode, TaskComplexity, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::{ActivityBinding, ToolCaller, ToolContext};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

fn bulk_root() -> PathBuf {
    let dir = orbit_common::test_env::bulk_write_temp_dir();
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: stdout={} stderr={}",
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn write_executable(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct PluginDeliveryFixture {
    _root: TempDir,
    root_path: PathBuf,
    global_root: PathBuf,
    repo: PathBuf,
    remote: PathBuf,
    runtime: OrbitRuntime,
    gh_log: PathBuf,
    plugin_state_dir: PathBuf,
    _env: orbit_common::test_env::ScopedEnv,
}

impl PluginDeliveryFixture {
    fn new(required_validation: &[&str]) -> Self {
        let root = TempDir::new_in(bulk_root()).unwrap();
        let root_path = std::fs::canonicalize(root.path()).unwrap();
        let global_root = root_path.join("home/.orbit");
        let repo = root_path.join("repo");
        let remote = root_path.join("origin.git");
        let workspace_orbit = repo.join(".orbit");
        std::fs::create_dir_all(&global_root).unwrap();
        std::fs::create_dir_all(&workspace_orbit).unwrap();
        std::fs::create_dir_all(&remote).unwrap();

        // Initialize real Git repo and local bare origin rewritten from a
        // GitHub URL so PR-mode admission, push, pr_open and pr_complete all
        // exercise the real forge route hermetically.
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "fixture@example.com"]);
        git(&repo, &["config", "user.name", "Fixture"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(&repo, &["config", "maintenance.auto", "false"]);
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::create_dir_all(repo.join("docs/standards")).unwrap();
        std::fs::write(repo.join("docs/standards/STD-01.md"), "base-standard\n").unwrap();
        std::fs::write(repo.join("AGENTS.md"), "# Primary checkout\nbase-block\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "initial base"]);

        git(&remote, &["init", "--bare", "-q"]);
        git(&remote, &["config", "maintenance.auto", "false"]);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repository.git",
            ],
        );
        git(
            &repo,
            &[
                "config",
                &format!("url.{}.insteadOf", remote.display()),
                "https://github.com/owner/repository.git",
            ],
        );
        git(&repo, &["push", "-q", "-u", "origin", "main"]);

        // Configure workspace with NO crews (proving agent-free delivery) and
        // optional required_validation_commands.
        let required_json = serde_json::to_string(required_validation).unwrap();
        std::fs::write(
            workspace_orbit.join("config.toml"),
            format!(
                "[workflow]\nrequired_validation_commands = {required_json}\n[review]\ntiming = \"off\"\n"
            ),
        )
        .unwrap();

        // Copy shipped deterministic delivery activities into global resources.
        let activities_dir = global_root.join("resources/activities");
        std::fs::create_dir_all(&activities_dir).unwrap();
        for name in [
            "worktree_setup",
            "git_commit",
            "pr_prepare",
            "git_rebase",
            "candidate_validate",
            "git_push",
            "pr_open",
            "pr_promote",
            "pr_complete",
        ] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("assets/activities/{name}.yaml")),
                activities_dir.join(format!("{name}.yaml")),
            )
            .unwrap();
        }

        // Install a fake `gh` CLI implementing the normal PR + CI + merge route.
        let bin_dir = root_path.join("bin");
        let gh_log = root_path.join("gh-calls.log");
        let pr_state_file = root_path.join("pr-state.json");
        let gh_script = format!(
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> '{gh_log}'
STATE_FILE='{pr_state}'
REMOTE='{remote}'
sub1="${{1:-}}"
sub2="${{2:-}}"
if [ "$sub1" = "pr" ] && [ "$sub2" = "list" ]; then
  if [ -f "$STATE_FILE" ]; then
    printf '[%s]\n' "$(cat "$STATE_FILE")"
  else
    printf '[]\n'
  fi
  exit 0
fi
if [ "$sub1" = "pr" ] && [ "$sub2" = "create" ]; then
  head_branch=""
  base_branch="main"
  shift 2
  while [ $# -gt 0 ]; do
    case "$1" in
      --head) head_branch="$2"; shift 2 ;;
      --base) base_branch="$2"; shift 2 ;;
      *) shift ;;
    esac
  done
  head_oid="$(git --git-dir="$REMOTE" rev-parse "refs/heads/$head_branch")"
  cat > "$STATE_FILE" <<EOF
{{"number":42,"state":"OPEN","headRefName":"$head_branch","headRefOid":"$head_oid","baseRefName":"$base_branch","url":"https://github.com/owner/repository/pull/42","autoMergeRequest":null,"mergedAt":null,"mergeCommit":null,"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[{{"__typename":"CheckRun","name":"ci/standards","status":"COMPLETED","conclusion":"SUCCESS"}}],"reviewDecision":"","isDraft":false}}
EOF
  printf 'https://github.com/owner/repository/pull/42\n'
  exit 0
fi
if [ "$sub1" = "pr" ] && [ "$sub2" = "view" ]; then
  cat "$STATE_FILE"
  exit 0
fi
if [ "$sub1" = "pr" ] && [ "$sub2" = "merge" ]; then
  head_branch="$(sed -n 's/.*"headRefName":"\([^"]*\)".*/\1/p' "$STATE_FILE")"
  base_branch="$(sed -n 's/.*"baseRefName":"\([^"]*\)".*/\1/p' "$STATE_FILE")"
  head_oid="$(git --git-dir="$REMOTE" rev-parse "refs/heads/$head_branch")"
  git --git-dir="$REMOTE" update-ref "refs/heads/$base_branch" "$head_oid"
  cat > "$STATE_FILE" <<EOF
{{"number":42,"state":"MERGED","headRefName":"$head_branch","headRefOid":"$head_oid","baseRefName":"$base_branch","url":"https://github.com/owner/repository/pull/42","autoMergeRequest":null,"mergedAt":"2026-11-25T00:00:00Z","mergeCommit":{{"oid":"$head_oid"}},"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[{{"__typename":"CheckRun","name":"ci/standards","status":"COMPLETED","conclusion":"SUCCESS"}}],"reviewDecision":"","isDraft":false}}
EOF
  exit 0
fi
if [ "$sub1" = "repo" ] && [ "$sub2" = "view" ]; then
  printf '{{"nameWithOwner":"owner/repository"}}\n'
  exit 0
fi
if [ "$sub1" = "api" ]; then
  case "$*" in
    *"/pulls/42/merge"*)
      head_branch="$(sed -n 's/.*"headRefName":"\([^"]*\)".*/\1/p' "$STATE_FILE")"
      base_branch="$(sed -n 's/.*"baseRefName":"\([^"]*\)".*/\1/p' "$STATE_FILE")"
      head_oid="$(git --git-dir="$REMOTE" rev-parse "refs/heads/$head_branch")"
      git --git-dir="$REMOTE" update-ref "refs/heads/$base_branch" "$head_oid"
      cat > "$STATE_FILE" <<EOF
{{"number":42,"state":"MERGED","headRefName":"$head_branch","headRefOid":"$head_oid","baseRefName":"$base_branch","url":"https://github.com/owner/repository/pull/42","autoMergeRequest":null,"mergedAt":"2026-11-25T00:00:00Z","mergeCommit":{{"oid":"$head_oid"}},"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[{{"__typename":"CheckRun","name":"ci/standards","status":"COMPLETED","conclusion":"SUCCESS"}}],"reviewDecision":"","isDraft":false}}
EOF
      printf '{{"merged":true,"sha":"%s","message":"merged"}}\n' "$head_oid"
      exit 0
      ;;
    *"graphql"*"autoMergeAllowed"*)
      base_branch="$(sed -n 's/.*"baseRefName":"\([^"]*\)".*/\1/p' "$STATE_FILE")"
      printf '{{"data":{{"repository":{{"autoMergeAllowed":false,"mergeCommitAllowed":true,"rebaseMergeAllowed":false,"squashMergeAllowed":false,"pullRequest":{{"baseRefName":"%s","baseRef":{{"branchProtectionRule":{{"requiresLinearHistory":false}}}}}}}}}}}}\n' "$base_branch"
      exit 0
      ;;
    *"graphql"*)
      head_oid="$(sed -n 's/.*"headRefOid":"\([^"]*\)".*/\1/p' "$STATE_FILE")"
      printf '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"%s","commits":{{"nodes":[{{"commit":{{"oid":"%s","statusCheckRollup":{{"contexts":{{"pageInfo":{{"hasNextPage":false}},"nodes":[{{"__typename":"CheckRun","name":"ci/standards","status":"COMPLETED","conclusion":"SUCCESS","isRequired":true}}]}}}},"checkSuites":{{"pageInfo":{{"hasNextPage":false}},"nodes":[]}}}}}}]}}}}}}}}}}\n' "$head_oid" "$head_oid"
      exit 0
      ;;
  esac
  printf '{{"allow_squash_merge":true,"allow_merge_commit":true,"allow_rebase_merge":true,"allow_auto_merge":false,"default_branch":"main"}}\n'
  exit 0
fi
echo "unexpected gh invocation: $*" >&2
exit 99
"#,
            gh_log = gh_log.display(),
            pr_state = pr_state_file.display(),
            remote = remote.display(),
        );
        write_executable(&bin_dir.join("gh"), &gh_script);

        let path = std::env::join_paths(std::iter::once(bin_dir).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .unwrap();
        let env = orbit_common::test_env::scoped([
            ("PATH", Some(path.to_str().unwrap())),
            ("ORBIT_OPERATOR", Some("1")),
        ]);

        let binding = WorkspaceRuntimeBinding {
            logical_workspace_id: "ws_plugin_delivery".to_string(),
            task_partition_id: "ws_plugin_delivery".to_string(),
            owner_machine_id: None,
            checkout_role: None,
            repo_root: repo.clone(),
            ship_mode: ShipMode::Pr,
            base_branch: Some("main".to_string()),
        };
        let initial =
            OrbitRuntime::from_roots_with_binding(&global_root, &workspace_orbit, binding.clone())
                .unwrap()
                .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm());

        install_syncproof_plugin(&initial, &root_path, &repo);

        // Reload runtime so newly installed plugin tools, activities and jobs
        // are active on the catalog and registry.
        let runtime =
            OrbitRuntime::from_roots_with_binding(&global_root, &workspace_orbit, binding)
                .unwrap()
                .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm());
        let plugin_state_dir = global_root.join("state/plugins/syncproof");

        Self {
            _root: root,
            root_path,
            global_root,
            repo,
            remote,
            runtime,
            gh_log,
            plugin_state_dir,
            _env: env,
        }
    }

    fn seed_task(&self, title: &str, tags: &[&str]) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: title.to_string(),
                description: format!("Sync task: {title}"),
                acceptance_criteria: vec!["Vendored standards are synced via PR.".to_string()],
                plan: "Run deterministic syncproof plugin delivery.".to_string(),
                tags: tags.iter().map(ToString::to_string).collect(),
                complexity: TaskComplexity::Low,
                context_files: vec!["file:docs/standards/STD-01.md".into()],
                task_type: Some(orbit_core::TaskType::Chore),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .expect("seed task")
            .id
    }

    fn plugin_job_path(&self, job_name: &str) -> PathBuf {
        self.global_root
            .join("plugins/syncproof/1.0.0/definitions/jobs")
            .join(format!("{job_name}.yaml"))
    }

    fn deterministic_context(&self, run_id: &str) -> ToolContext {
        let mut ctx =
            RuntimeHost::tool_context_for_activity(&self.runtime, Some(run_id), None, None, None);
        ctx.caller = ToolCaller::DeterministicStep(Default::default());
        ctx.activity_binding = Some(ActivityBinding {
            job_run_id: run_id.to_string(),
            task_id: None,
        });
        ctx
    }

    fn start_delivery_run_with_worktree(
        &self,
        job_name: &str,
        task_ids: &[String],
    ) -> (String, PathBuf) {
        let job_path = self.plugin_job_path(job_name);
        let yaml = std::fs::read_to_string(&job_path).unwrap();
        let run_input = json!({
            "task_ids": task_ids,
            "base_branch": "main",
            "base_sync": "remote",
        });
        let run = self
            .runtime
            .insert_job_run(job_name, 1, Utc::now(), Some(run_input.clone()), None)
            .unwrap();
        self.runtime
            .write_run_state(
                &run.run_id,
                &PipelineState::new(run.run_id.clone(), run.job_id.clone(), run_input),
            )
            .unwrap();
        let snapshot_path = self
            .repo
            .join(".orbit/state/job-runs")
            .join(format!("{}.job.yaml", run.run_id));
        std::fs::create_dir_all(snapshot_path.parent().unwrap()).unwrap();
        std::fs::write(&snapshot_path, yaml).unwrap();
        orbit_store::compose::workspace_job_run_store(
            self.runtime.sqlite_store().unwrap(),
            self.runtime.workspace_id().unwrap(),
        )
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();

        let setup_output = orbit_engine::execute_deterministic_action(
            &self.runtime,
            "worktree_setup",
            &json!({}),
            &json!({
                "run_id": run.run_id,
                "job_run_id": run.run_id,
                "task_ids": task_ids,
                "base": "main",
                "base_sync": "remote",
            }),
            false,
            &std::collections::HashMap::new(),
            None,
        )
        .expect("worktree_setup");

        let mut state = self.runtime.read_run_state(&run.run_id).unwrap().unwrap();
        state.record_step(0, JobRunState::Success, Some(setup_output.clone()), None);
        state.record_pipeline_output("worktree", setup_output.clone());
        self.runtime.write_run_state(&run.run_id, &state).unwrap();

        let worktree_path = PathBuf::from(setup_output["workspace_path"].as_str().unwrap());
        (run.run_id, worktree_path)
    }

    fn invocation_marker(&self) -> PathBuf {
        self.plugin_state_dir.join("invoked.log")
    }
}

fn install_syncproof_plugin(runtime: &OrbitRuntime, root_path: &Path, primary_repo: &Path) {
    let source = root_path.join("syncproof-plugin-source");
    let plugin = orbit_types::plugin::plugin_root_in(&source);
    let jobs = plugin.join("definitions/jobs");
    let activities = plugin.join("definitions/activities");
    let backend_dir = plugin.join("bin");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::create_dir_all(&activities).unwrap();
    std::fs::create_dir_all(&backend_dir).unwrap();

    std::fs::write(
        plugin.join("plugin.yaml"),
        r#"schemaVersion: 2
kind: Plugin
metadata:
  name: syncproof
  version: 1.0.0
  description: Deterministic worktree delivery fixture plugin.
spec:
  backend:
    type: exec
    command: bin/backend.sh
  permissions:
    fs:
      read: ["{{workspace}}"]
      write: ["{{workspace}}/docs/standards", "{{plugin_state}}"]
      write_files: ["{{workspace}}/AGENTS.md"]
  tools:
    - name: sync
      description: Sync vendored standards inside the bound workspace.
      execution_kind: mutating
      mcp_scope: workspace
      input_schema:
        type: object
        properties:
          canon_commit:
            type: string
            description: Pinned canon commit to sync.
        required: [canon_commit]
    - name: probe
      description: Read-only probe reporting the bound workspace.
      execution_kind: read_only
      mcp_scope: workspace
      input_schema:
        type: object
        properties: {}
  definitions:
    activities: [definitions/activities/*.yaml]
    jobs: [definitions/jobs/*.yaml]
"#,
    )
    .unwrap();

    // Backend runs under the real OS sandbox (Landlock on Linux / Seatbelt on
    // macOS) with no `unsandboxed` grant. It writes to `{{workspace}}/docs/standards`
    // and `{{workspace}}/AGENTS.md`, tests whether the primary checkout is
    // writable (must be denied when bound to a worktree), and returns all
    // observed envelope and environment fields.
    let backend = format!(
        r#"#!/bin/sh
set -eu
ENVELOPE="$(cat)"
printf '%s\n' "$ORBIT_TOOL_NAME" >> "$ORBIT_PLUGIN_STATE/invoked.log"
PWD_PHYS="$(pwd -P)"
WS_ROOT="$(printf '%s' "$ENVELOPE" | sed -n 's/.*"workspace_root":"\([^"]*\)".*/\1/p')"
TASK_ID="$(printf '%s' "$ENVELOPE" | sed -n 's/.*"task_id":"\([^"]*\)".*/\1/p')"
JOB_RUN_ID="$(printf '%s' "$ENVELOPE" | sed -n 's/.*"job_run_id":"\([^"]*\)".*/\1/p')"
CANON_COMMIT="$(printf '%s' "$ENVELOPE" | sed -n 's/.*"canon_commit":"\([^"]*\)".*/\1/p')"

if [ "$ORBIT_TOOL_NAME" = "syncproof.sync" ]; then
  printf 'synced:%s\n' "$CANON_COMMIT" > "$WS_ROOT/docs/standards/STD-01.md"
  printf '# Synced\nsynced:%s\n' "$CANON_COMMIT" > "$WS_ROOT/AGENTS.md"
  PRIMARY_WRITE_DENIED="false"
  if [ "$WS_ROOT" != '{primary}' ]; then
    if ! (printf 'escape\n' > '{primary}/docs/standards/escape.txt') 2>/dev/null \
       && ! (printf 'escape\n' > '{primary}/AGENTS.md') 2>/dev/null; then
      PRIMARY_WRITE_DENIED="true"
    fi
  fi
  printf '{{"ok":true,"output":{{"workspace_root":"%s","env_workspace_root":"%s","env_tool_cwd":"%s","pwd":"%s","task_id":"%s","job_run_id":"%s","canon_commit":"%s","primary_write_denied":%s}}}}\n' \
    "$WS_ROOT" "$ORBIT_WORKSPACE_ROOT" "$ORBIT_TOOL_CWD" "$PWD_PHYS" "$TASK_ID" "$JOB_RUN_ID" "$CANON_COMMIT" "$PRIMARY_WRITE_DENIED"
  exit 0
fi

printf '{{"ok":true,"output":{{"workspace_root":"%s","env_workspace_root":"%s","pwd":"%s","task_id":"%s","job_run_id":"%s"}}}}\n' \
  "$WS_ROOT" "$ORBIT_WORKSPACE_ROOT" "$PWD_PHYS" "$TASK_ID" "$JOB_RUN_ID"
"#,
        primary = primary_repo.display()
    );
    write_executable(&backend_dir.join("backend.sh"), &backend);

    std::fs::write(
        activities.join("sync.yaml"),
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: syncproof_sync
spec:
  type: deterministic
  description: Run syncproof.sync against the admitted task worktree.
  input_schema_json:
    type: object
  action: plugin.tool_call
  config:
    tool: syncproof.sync
    input: {}
"#,
    )
    .unwrap();

    std::fs::write(
        activities.join("probe.yaml"),
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: syncproof_probe
spec:
  type: deterministic
  description: Run read-only syncproof.probe.
  input_schema_json:
    type: object
  action: plugin.tool_call
  config:
    tool: syncproof.probe
    input: {}
"#,
    )
    .unwrap();

    // PR-mode delivery job with NO agent step: worktree -> plugin sync ->
    // commit -> prepare_branch -> sync_base -> validate -> push -> pr_open ->
    // promote_tasks -> complete_pr.
    std::fs::write(
        jobs.join("syncproof_pr_delivery.yaml"),
        r#"schemaVersion: 2
kind: Job
metadata:
  name: syncproof_pr_delivery
spec:
  state: enabled
  owns_task_worktree: true
  task_delivery:
    modes: [pr]
  kind: workflow
  default_input:
    task_ids: []
    base_branch: main
    base_sync: remote
    dependency_delivery: enforce
    landing_branch: ""
    completion: review
    auto_fix: false
    allowed_crews: []
    canon_commit: ""
  steps:
    - id: worktree
      target: activity:worktree_setup
      default_input:
        task_ids: "{{ input.task_ids }}"
        base: "{{ input.base_branch }}"
        base_sync: "{{ input.base_sync }}"
        dependency_delivery: "{{ input.dependency_delivery }}"
    - id: sync
      target: activity:syncproof_sync
      default_input:
        job_run_id: "{{ steps.worktree.output.job_run_id }}"
        task_ids: "{{ input.task_ids }}"
        workspace_path: "{{ steps.worktree.output.workspace_path }}"
        input:
          canon_commit: "{{ input.canon_commit }}"
    - id: commit
      target: activity:git_commit
      default_input:
        job_run_id: "{{ steps.worktree.output.job_run_id }}"
        scope: all
        workspace_path: "{{ steps.worktree.output.workspace_path }}"
        base_ref: "{{ steps.worktree.output.base_ref }}"
        base_sha: "{{ steps.worktree.output.base_sha }}"
    - id: prepare_branch
      target: activity:pr_prepare
      default_input:
        authenticate_preparation: true
        job_run_id: "{{ steps.worktree.output.job_run_id }}"
        base: "{{ input.base_branch }}"
        base_sync: "{{ input.base_sync }}"
        completed_task_ids: "{{ input.task_ids }}"
        workspace_path: "{{ steps.worktree.output.workspace_path }}"
    - id: sync_base
      target: activity:git_rebase
      default_input:
        step_id: sync_base
        preparation: "{{ steps.prepare_branch.output }}"
        final_validation: true
        job_run_id: "{{ steps.worktree.output.job_run_id }}"
        completed_task_ids: "{{ input.task_ids }}"
        workspace_path: "{{ steps.worktree.output.workspace_path }}"
        head: "{{ steps.prepare_branch.output.head }}"
        head_sha: "{{ steps.prepare_branch.output.head_sha }}"
        base: "{{ steps.prepare_branch.output.base }}"
        base_ref: "{{ steps.prepare_branch.output.base_ref }}"
        base_sha: "{{ steps.prepare_branch.output.base_sha }}"
        remote_sha: "{{ steps.prepare_branch.output.remote_sha }}"
        commits_behind: "{{ steps.prepare_branch.output.commits_behind }}"
        sync_required: "{{ steps.prepare_branch.output.sync_required }}"
    - id: validate
      target: activity:candidate_validate
      default_input:
        job_run_id: "{{ steps.worktree.output.job_run_id }}"
        completed_task_ids: "{{ input.task_ids }}"
        workspace_path: "{{ steps.worktree.output.workspace_path }}"
        base_sha: "{{ steps.sync_base.output.base_sha }}"
        base_ref: "{{ steps.sync_base.output.base_ref }}"
    - id: push
      target: activity:git_push
      default_input:
        job_run_id: "{{ steps.worktree.output.job_run_id }}"
        completed_task_ids: "{{ input.task_ids }}"
        workspace_path: "{{ steps.worktree.output.workspace_path }}"
        branch: "{{ steps.sync_base.output.head }}"
        base: "{{ steps.sync_base.output.base }}"
        base_ref: "{{ steps.sync_base.output.base_ref }}"
        rewrite_performed: "{{ steps.sync_base.output.rewritten }}"
        rewrite_head_before: "{{ steps.sync_base.output.head_sha_before }}"
        expected_remote_sha: "{{ steps.sync_base.output.remote_sha_before }}"
    - id: pr_open
      target: activity:pr_open
      default_input:
        job_run_id: "{{ steps.worktree.output.job_run_id }}"
        completed_task_ids: "{{ input.task_ids }}"
        workspace_path: "{{ steps.worktree.output.workspace_path }}"
        base: "{{ steps.sync_base.output.base }}"
        base_ref: "{{ steps.sync_base.output.base_ref }}"
        base_sha: "{{ steps.sync_base.output.base_sha }}"
        base_sync: "{{ input.base_sync }}"
        landing_branch: "{{ input.landing_branch }}"
        head: "{{ steps.sync_base.output.head }}"
    - id: promote_tasks
      target: activity:pr_promote
      default_input:
        job_run_id: "{{ steps.worktree.output.job_run_id }}"
        completed_task_ids: "{{ input.task_ids }}"
        workspace_path: "{{ steps.worktree.output.workspace_path }}"
        head: "{{ steps.sync_base.output.head }}"
        base: "{{ steps.sync_base.output.base }}"
        base_ref: "{{ steps.sync_base.output.base_ref }}"
        base_sha: "{{ steps.sync_base.output.base_sha }}"
        base_sync: "{{ input.base_sync }}"
        landing_branch: "{{ input.landing_branch }}"
        pr_number: "{{ steps.pr_open.output.pr_number }}"
        pr_url: "{{ steps.pr_open.output.pr_url }}"
    - id: complete_pr
      when: "{{ input.completion }} == done"
      target: activity:pr_complete
      default_input:
        job_run_id: "{{ steps.worktree.output.job_run_id }}"
        completed_task_ids: "{{ input.task_ids }}"
        workspace_path: "{{ steps.worktree.output.workspace_path }}"
        pr_number: "{{ steps.pr_open.output.pr_number }}"
        completion: "{{ input.completion }}"
        auto_fix: "{{ input.auto_fix }}"
        allowed_crews: "{{ input.allowed_crews }}"
        head: "{{ steps.sync_base.output.head }}"
        published_head_sha: "{{ steps.push.output.local_sha }}"
        previous_published_head_sha: "{{ steps.push.output.remote_sha_before }}"
        base: "{{ steps.sync_base.output.base }}"
        base_sync: "{{ input.base_sync }}"
"#,
    )
    .unwrap();

    // Multi-task capable custom delivery job (uses `git_commit scope: per_task`
    // so job admission does not reject 2 tasks up front, proving `plugin.tool_call`
    // itself still fails closed on multi-task bindings before invoking the plugin).
    std::fs::write(
        jobs.join("syncproof_batch_delivery.yaml"),
        r#"schemaVersion: 2
kind: Job
metadata:
  name: syncproof_batch_delivery
spec:
  state: enabled
  owns_task_worktree: true
  task_delivery:
    modes: [local]
  kind: workflow
  default_input:
    task_ids: []
    base_branch: main
    base_sync: remote
  steps:
    - id: worktree
      target: activity:worktree_setup
      default_input:
        task_ids: "{{ input.task_ids }}"
        base: "{{ input.base_branch }}"
        base_sync: "{{ input.base_sync }}"
    - id: sync
      target: activity:syncproof_sync
      default_input:
        job_run_id: "{{ steps.worktree.output.job_run_id }}"
        task_ids: "{{ input.task_ids }}"
        workspace_path: "{{ steps.worktree.output.workspace_path }}"
        input:
          canon_commit: "batch-commit"
"#,
    )
    .unwrap();

    for (name, prefix) in [
        ("syncproof_default_prefix", "{{ input.branch_prefix }}"),
        ("syncproof_override_prefix", "step-prefix"),
    ] {
        let yaml = std::fs::read_to_string(jobs.join("syncproof_batch_delivery.yaml"))
            .unwrap()
            .replace("name: syncproof_batch_delivery", &format!("name: {name}"))
            .replace("    base_branch: main", "    branch_prefix: default-prefix\n    base_branch: main")
            .replace("        base: \"{{ input.base_branch }}\"", &format!("        branch_prefix: \"{prefix}\"\n        base: \"{{{{ input.base_branch }}}}\""));
        std::fs::write(jobs.join(format!("{name}.yaml")), yaml).unwrap();
    }

    // Delivery job with no worktree_setup step at all: mutating call must fail
    // closed while a read-only call without workspace_path keeps existing routing.
    std::fs::write(
        jobs.join("syncproof_no_worktree_mutating.yaml"),
        r#"schemaVersion: 2
kind: Job
metadata:
  name: syncproof_no_worktree_mutating
spec:
  state: enabled
  task_delivery:
    modes: [pr]
  kind: workflow
  steps:
    - id: sync
      target: activity:syncproof_sync
      default_input:
        task_ids: "{{ input.task_ids }}"
        input:
          canon_commit: "no-wt"
"#,
    )
    .unwrap();

    std::fs::write(
        jobs.join("syncproof_read_only_delivery.yaml"),
        r#"schemaVersion: 2
kind: Job
metadata:
  name: syncproof_read_only_delivery
spec:
  state: enabled
  task_delivery:
    modes: [local]
  kind: workflow
  steps:
    - id: probe
      target: activity:syncproof_probe
"#,
    )
    .unwrap();

    // Non-delivery maintenance job (no `spec.task_delivery`): calling the same
    // mutating plugin tool without `workspace_path` must keep primary-checkout routing.
    std::fs::write(
        jobs.join("syncproof_maintenance.yaml"),
        r#"schemaVersion: 2
kind: Job
metadata:
  name: syncproof_maintenance
spec:
  state: enabled
  kind: workflow
  steps:
    - id: sync
      target: activity:syncproof_sync
      default_input:
        input:
          canon_commit: "maintenance-rev"
"#,
    )
    .unwrap();

    install_plugin(
        runtime,
        source.to_str().expect("plugin source path"),
        &PluginAddOptions {
            enable: true,
            grants: vec!["fs".to_string()],
            ..Default::default()
        },
    )
    .expect("install syncproof plugin");
}

/// End-to-end PR-mode plugin delivery with no agent: routes through
/// `resolve_delivery_job`, creates the task worktree, runs the mutating plugin
/// confined to that worktree (leaving the primary checkout untouched and
/// denying primary-checkout writes inside the sandbox), commits, validates with
/// workspace required commands, pushes, opens PR, checks CI and merges to done.
fn pr_mode_plugin_delivery_binds_worktree_and_completes_pr_ci_route_without_agent() {
    let fixture = PluginDeliveryFixture::new(&[
        "test -f docs/standards/STD-01.md && grep -qx 'synced:canon-abc123' docs/standards/STD-01.md && grep -qx 'synced:canon-abc123' AGENTS.md",
    ]);
    let task_id = fixture.seed_task(
        "Sync vendored standards",
        &["delivery:syncproof_pr_delivery"],
    );

    // Route resolution selects the plugin delivery job in PR mode.
    let route = fixture
        .runtime
        .run_deterministic(
            "resolve_delivery_job",
            &json!({}),
            &json!({"task_ids": [&task_id], "mode": "pr"}),
            ToolContext::default(),
        )
        .expect("resolve PR plugin delivery route");
    assert_eq!(route["job_name"], "syncproof_pr_delivery", "{route}");
    assert_eq!(route["plugin"], "syncproof", "{route}");

    let result = fixture
        .runtime
        .run_job_v2_from_yaml(
            &fixture.plugin_job_path("syncproof_pr_delivery"),
            json!({
                "task_ids": [&task_id],
                "base_branch": "main",
                "base_sync": "remote",
                "completion": "done",
                "canon_commit": "canon-abc123",
            }),
        )
        .expect("run PR-mode plugin delivery job");
    assert!(result.success, "expected PR delivery success: {result:?}");

    let worktree_path = PathBuf::from(
        result.pipeline["worktree"]["workspace_path"]
            .as_str()
            .expect("worktree workspace_path"),
    );
    let canonical_primary = std::fs::canonicalize(&fixture.repo).unwrap();
    let expected_worktree_parent = canonical_primary.join(".orbit/state/worktrees");
    assert_eq!(
        worktree_path.parent(),
        Some(expected_worktree_parent.as_path())
    );
    assert_ne!(worktree_path, canonical_primary);

    // Plugin saw matching admitted worktree context, cwd, task identity,
    // pinned input, and kernel sandbox denial against the primary checkout.
    let sync = &result.pipeline["sync"];
    assert_eq!(
        sync["workspace_root"],
        worktree_path.to_string_lossy().as_ref(),
        "{sync}"
    );
    assert_eq!(
        sync["env_workspace_root"],
        worktree_path.to_string_lossy().as_ref(),
        "{sync}"
    );
    assert_eq!(
        sync["env_tool_cwd"],
        worktree_path.to_string_lossy().as_ref(),
        "{sync}"
    );
    assert_eq!(
        sync["pwd"],
        worktree_path.to_string_lossy().as_ref(),
        "{sync}"
    );
    assert_eq!(sync["task_id"], task_id, "{sync}");
    assert_eq!(sync["job_run_id"], result.run_id, "{sync}");
    assert_eq!(sync["canon_commit"], "canon-abc123", "{sync}");
    assert_eq!(
        sync["primary_write_denied"], true,
        "sandbox grants must bind to the admitted worktree and deny writes to the primary checkout: {sync}"
    );

    // Primary checkout stayed untouched.
    assert_eq!(git(&fixture.repo, &["status", "--porcelain=v1"]), "");
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("docs/standards/STD-01.md")).unwrap(),
        "base-standard\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("AGENTS.md")).unwrap(),
        "# Primary checkout\nbase-block\n"
    );
    assert!(!fixture.repo.join("docs/standards/escape.txt").exists());

    // Normal PR, validation, CI and merge steps completed with no agent.
    assert_eq!(result.pipeline["commit"]["committed"], true);
    assert_eq!(result.pipeline["validate"]["decision"], "passed");
    assert_eq!(result.pipeline["pr_open"]["pr_number"], "42");
    assert_eq!(result.pipeline["promote_tasks"]["decision"], "performed");
    assert_eq!(result.pipeline["complete_pr"]["merge"]["merged"], true);
    assert_eq!(
        result.pipeline["complete_pr"]["completed_task_ids"],
        json!([&task_id])
    );

    let head_branch = result.pipeline["worktree"]["head_ref"].as_str().unwrap();
    let pushed_std = git(
        &fixture.remote,
        &[
            "show",
            &format!("refs/heads/{head_branch}:docs/standards/STD-01.md"),
        ],
    );
    let pushed_agents = git(
        &fixture.remote,
        &["show", &format!("refs/heads/{head_branch}:AGENTS.md")],
    );
    assert_eq!(pushed_std, "synced:canon-abc123");
    assert_eq!(pushed_agents, "# Synced\nsynced:canon-abc123");

    let gh_calls = std::fs::read_to_string(&fixture.gh_log).unwrap();
    assert!(gh_calls.contains("pr create"), "{gh_calls}");
    assert!(gh_calls.contains("pr view 42"), "{gh_calls}");
    assert!(gh_calls.contains("graphql"), "{gh_calls}");
    assert!(
        gh_calls.contains("pr merge 42") || gh_calls.contains("/pulls/42/merge"),
        "{gh_calls}"
    );

    let task = fixture.runtime.get_task(&task_id).unwrap();
    assert_eq!(task.status, TaskStatus::Done);
    assert_eq!(task.github_pr_number(), Some("42"));
    assert!(
        task.context_files.iter().any(|s| s.contains("AGENTS.md")),
        "commit widened context_files over synced AGENTS.md: {:?}",
        task.context_files
    );
    let run = fixture
        .runtime
        .show_job_run_observed(&result.run_id)
        .unwrap();
    assert!(run.resolved_crew.is_none(), "no agent crew ran: {run:?}");
}

/// Missing, mismatched, unowned, invalid, symlinked, and multi-task bindings
/// all fail closed before invoking the mutating plugin backend; a matching
/// singleton binds grants to the admitted checkout.
fn missing_mismatched_unowned_invalid_and_multi_task_bindings_fail_closed_before_dispatch() {
    let fixture = PluginDeliveryFixture::new(&[]);
    let task_1 = fixture.seed_task("Singleton task one", &["delivery:syncproof_pr_delivery"]);
    let task_2 = fixture.seed_task("Singleton task two", &["delivery:syncproof_pr_delivery"]);

    let (run_1, wt_1) = fixture
        .start_delivery_run_with_worktree("syncproof_pr_delivery", std::slice::from_ref(&task_1));
    let (run_2, wt_2) = fixture
        .start_delivery_run_with_worktree("syncproof_pr_delivery", std::slice::from_ref(&task_2));

    let outside_dir = fixture.root_path.join("outside-unowned");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let unregistered_dir = fixture
        .repo
        .join(".orbit/state/worktrees/orbit-unregistered");
    std::fs::create_dir_all(&unregistered_dir).unwrap();
    let symlink_to_wt1 = fixture.root_path.join("symlink-to-wt1");
    std::os::unix::fs::symlink(&wt_1, &symlink_to_wt1).unwrap();

    let config = json!({"tool": "syncproof.sync", "input": {}});
    let mut_call = |run_id: &str, step_input: Value| {
        let ctx = fixture.deterministic_context(run_id);
        fixture
            .runtime
            .run_deterministic("plugin.tool_call", &config, &step_input, ctx)
    };

    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "missing workspace_path",
            json!({"job_run_id": &run_1, "task_ids": [&task_1], "input": {"canon_commit": "x"}}),
            "requires an explicit `workspace_path`",
        ),
        (
            "missing task binding",
            json!({"job_run_id": &run_1, "workspace_path": &wt_1, "input": {"canon_commit": "x"}}),
            "missing task binding",
        ),
        (
            "empty task_ids",
            json!({"job_run_id": &run_1, "task_ids": [], "workspace_path": &wt_1, "input": {"canon_commit": "x"}}),
            "empty",
        ),
        (
            "multiple distinct task_ids",
            json!({"job_run_id": &run_1, "task_ids": [&task_1, &task_2], "workspace_path": &wt_1, "input": {"canon_commit": "x"}}),
            "supports exactly one admitted task, got 2",
        ),
        (
            "duplicate task_ids",
            json!({"job_run_id": &run_1, "task_ids": [&task_1, &task_1], "workspace_path": &wt_1, "input": {"canon_commit": "x"}}),
            "supports exactly one admitted task, got 2",
        ),
        (
            "mismatched task_id",
            json!({"job_run_id": &run_1, "task_id": &task_2, "workspace_path": &wt_1, "input": {"canon_commit": "x"}}),
            "does not match delivery run",
        ),
        (
            "conflicting task_id and task_ids",
            json!({"job_run_id": &run_1, "task_id": &task_1, "task_ids": [&task_2], "workspace_path": &wt_1, "input": {"canon_commit": "x"}}),
            "mismatched task binding",
        ),
        (
            "mismatched job_run_id",
            json!({"job_run_id": &run_2, "task_ids": [&task_1], "workspace_path": &wt_1, "input": {"canon_commit": "x"}}),
            "does not match authenticated worktree owner",
        ),
        (
            "primary checkout as workspace_path",
            json!({"job_run_id": &run_1, "task_ids": [&task_1], "workspace_path": &fixture.repo, "input": {"canon_commit": "x"}}),
            "does not match the authenticated worktree",
        ),
        (
            "another run worktree as workspace_path",
            json!({"job_run_id": &run_1, "task_ids": [&task_1], "workspace_path": &wt_2, "input": {"canon_commit": "x"}}),
            "does not match the authenticated worktree",
        ),
        (
            "unowned outside directory",
            json!({"job_run_id": &run_1, "task_ids": [&task_1], "workspace_path": &outside_dir, "input": {"canon_commit": "x"}}),
            "does not match the authenticated worktree",
        ),
        (
            "unregistered directory inside worktrees root",
            json!({"job_run_id": &run_1, "task_ids": [&task_1], "workspace_path": &unregistered_dir, "input": {"canon_commit": "x"}}),
            "does not match the authenticated worktree",
        ),
        (
            "symlink to admitted worktree",
            json!({"job_run_id": &run_1, "task_ids": [&task_1], "workspace_path": &symlink_to_wt1, "input": {"canon_commit": "x"}}),
            "must not be a symbolic link",
        ),
        (
            "relative workspace_path",
            json!({"job_run_id": &run_1, "task_ids": [&task_1], "workspace_path": "relative/worktree", "input": {"canon_commit": "x"}}),
            "must be an absolute path",
        ),
        (
            "parent traversal workspace_path",
            json!({"job_run_id": &run_1, "task_ids": [&task_1], "workspace_path": format!("{}/../{}", wt_1.display(), wt_1.file_name().unwrap().to_string_lossy()), "input": {"canon_commit": "x"}}),
            "parent traversal",
        ),
    ];

    for (label, step_input, expected) in cases {
        let error =
            mut_call(&run_1, step_input).expect_err(&format!("case '{label}' must fail closed"));
        assert!(
            error.to_string().contains(expected),
            "case '{label}' expected '{expected}', got: {error}"
        );
    }

    // Delivery job with no worktree_setup step fails closed before invoking plugin.
    let no_wt_task = fixture.seed_task(
        "No worktree task",
        &["delivery:syncproof_no_worktree_mutating"],
    );
    let no_wt_error = fixture
        .runtime
        .run_job_v2_from_yaml(
            &fixture.plugin_job_path("syncproof_no_worktree_mutating"),
            json!({"task_ids": [&no_wt_task]}),
        )
        .expect_err("mutating delivery job without worktree_setup must fail closed");
    assert!(
        no_wt_error
            .to_string()
            .contains("requires an explicit `workspace_path`"),
        "{no_wt_error}"
    );

    // Multi-task delivery run fails closed before invoking plugin.
    let batch_a = fixture.seed_task("Batch A", &["delivery:syncproof_batch_delivery"]);
    let batch_b = fixture.seed_task("Batch B", &["delivery:syncproof_batch_delivery"]);
    let batch_error = fixture
        .runtime
        .run_job_v2_from_yaml(
            &fixture.plugin_job_path("syncproof_batch_delivery"),
            json!({"task_ids": [&batch_a, &batch_b]}),
        )
        .expect_err("multi-task delivery run must fail closed before mutating plugin call");
    assert!(
        batch_error
            .to_string()
            .contains("supports exactly one admitted task, got 2"),
        "{batch_error}"
    );

    // Explicit workspace_path outside a task-delivery job fails closed.
    let no_run_error = fixture
        .runtime
        .run_deterministic(
            "plugin.tool_call",
            &config,
            &json!({"workspace_path": &wt_1, "task_id": &task_1, "input": {"canon_commit": "x"}}),
            ToolContext::default(),
        )
        .expect_err("unbound caller passing workspace_path must fail closed");
    assert!(
        no_run_error
            .to_string()
            .contains("requires a host-attested job run"),
        "{no_run_error}"
    );

    // Prove none of the refused calls ever spawned the plugin backend.
    assert!(
        !fixture.invocation_marker().exists(),
        "refused calls must fail before invoking the plugin backend"
    );

    // Now the matching singleton call succeeds and binds grants to wt_1 only.
    let ok = mut_call(
        &run_1,
        json!({
            "job_run_id": &run_1,
            "task_ids": [&task_1],
            "workspace_path": &wt_1,
            "input": {"canon_commit": "singleton-ok"},
        }),
    )
    .expect("matching singleton delivery call must succeed");
    let canonical_wt1 = std::fs::canonicalize(&wt_1).unwrap();
    assert_eq!(
        ok["workspace_root"],
        canonical_wt1.to_string_lossy().as_ref(),
        "{ok}"
    );
    assert_eq!(ok["task_id"], task_1, "{ok}");
    assert_eq!(ok["primary_write_denied"], true, "{ok}");
    assert_eq!(
        std::fs::read_to_string(wt_1.join("docs/standards/STD-01.md")).unwrap(),
        "synced:singleton-ok\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("docs/standards/STD-01.md")).unwrap(),
        "base-standard\n"
    );
}

/// Plugin calls outside deterministic task delivery (non-delivery jobs) and
/// read-only delivery steps without `workspace_path` keep their primary-checkout
/// routing.
fn non_delivery_and_unbound_read_only_plugin_calls_keep_existing_routing() {
    let fixture = PluginDeliveryFixture::new(&[]);
    let canonical_primary = std::fs::canonicalize(&fixture.repo).unwrap();

    // Non-delivery job calling a mutating plugin tool keeps primary-checkout routing.
    let maintenance = fixture
        .runtime
        .run_job_v2_from_yaml(&fixture.plugin_job_path("syncproof_maintenance"), json!({}))
        .expect("non-delivery mutating plugin job keeps primary checkout routing");
    assert!(maintenance.success, "{maintenance:?}");
    assert_eq!(
        maintenance.pipeline["sync"]["workspace_root"],
        canonical_primary.to_string_lossy().as_ref(),
        "{:?}",
        maintenance.pipeline
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("docs/standards/STD-01.md")).unwrap(),
        "synced:maintenance-rev\n"
    );

    // Read-only delivery job omitting workspace_path keeps existing routing.
    let ro = fixture
        .runtime
        .run_job_v2_from_yaml(
            &fixture.plugin_job_path("syncproof_read_only_delivery"),
            json!({}),
        )
        .expect("read-only delivery job without workspace_path keeps existing routing");
    assert!(ro.success, "{ro:?}");
    assert_eq!(
        ro.pipeline["probe"]["workspace_root"],
        canonical_primary.to_string_lossy().as_ref(),
        "{:?}",
        ro.pipeline
    );
}

fn setup_prefix_defaults_and_step_overrides_bind_plugin_worktree() {
    let fixture = PluginDeliveryFixture::new(&[]);
    for (job, run_prefix, expected_prefix) in [
        ("syncproof_default_prefix", None, "default-prefix"),
        (
            "syncproof_override_prefix",
            Some("run-prefix"),
            "step-prefix",
        ),
    ] {
        let task = fixture.seed_task(job, &[&format!("delivery:{job}")]);
        let mut input = json!({"task_ids": [&task]});
        if let Some(prefix) = run_prefix {
            input["branch_prefix"] = json!(prefix);
        }
        let result = fixture
            .runtime
            .run_job_v2_from_yaml(&fixture.plugin_job_path(job), input)
            .expect("configured prefix must bind the setup worktree");
        assert!(result.success, "{result:?}");
        let path = result.pipeline["worktree"]["workspace_path"]
            .as_str()
            .unwrap();
        assert!(
            Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(expected_prefix),
            "{path}"
        );
        assert_eq!(result.pipeline["sync"]["workspace_root"], path);
        assert_eq!(result.pipeline["sync"]["task_id"], task);
        assert_eq!(result.pipeline["sync"]["primary_write_denied"], true);
    }
}

crate::scenario_table!(
    plugin_delivery_worktree_scenarios,
    pr_mode_plugin_delivery_binds_worktree_and_completes_pr_ci_route_without_agent,
    missing_mismatched_unowned_invalid_and_multi_task_bindings_fail_closed_before_dispatch,
    non_delivery_and_unbound_read_only_plugin_calls_keep_existing_routing,
    setup_prefix_defaults_and_step_overrides_bind_plugin_worktree
);
