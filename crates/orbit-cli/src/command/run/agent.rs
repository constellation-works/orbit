//! `orbit run agent` — operator-only host exploration invocation [ORB-11354].
//!
//! A thin collector. The admission, the workspace/cwd validation, the crew and
//! timeout rules, and the durable submission all live in
//! `OrbitRuntime::submit_agent_invoke_run`, which the MCP tool calls too — so
//! this command cannot drift from the tool's behavior or relax its checks.

use clap::Args;
use orbit_core::{AgentInvokeRequest, OrbitRuntime};
use orbit_types::tool::ToolSessionContext;
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
#[command(
    about = "Invoke an agent on the host for exploration or debugging",
    override_usage = "orbit run agent <PROMPT> [OPTIONS]",
    after_help = "\
The agent runs OUTSIDE Orbit's filesystem sandbox, as the same OS user as Orbit,
so it can read and write anything that user can. That is the point of the
command and it is not an isolation boundary — invoke it the way you would run a
shell command yourself. Each invocation is admitted separately and requires
operator capability; a managed run cannot invoke one.

The agent investigates and reports. It changes no task, makes no commit, opens
no pull request, and dispatches no further work.

Submission is asynchronous: this prints the durable run ID and returns. Track
and read the invocation with the ordinary run surfaces:
  orbit run show <RUN_ID>      structured state, outcome, and bounded preview
  orbit run logs <RUN_ID>      the complete captured output
  orbit run cancel <RUN_ID>    stop it and terminate its process tree

Examples:
  orbit run agent 'why does the sweep clock keep restarting?'
  orbit run agent 'explain this failure' --cwd /srv/checkout --crew qa
  orbit run agent 'long investigation' --timeout 3600 --json
  orbit run agent 'retryable submit' --idempotency-key incident-4821
  orbit run agent 'read Cargo.toml' --provider-sandbox read-only"
)]
pub struct RunAgentArgs {
    /// What to investigate.
    #[arg(value_name = "PROMPT")]
    pub prompt: String,

    /// Absolute directory the agent starts in. Defaults to the current
    /// directory; must be inside this workspace's checkout.
    #[arg(long)]
    pub cwd: Option<String>,

    /// Configured crew selecting provider, model, and reasoning effort.
    /// Defaults to the workspace's default crew.
    #[arg(long)]
    pub crew: Option<String>,

    /// Wall-clock bound in seconds. Defaults to 1800; the maximum is 7200.
    #[arg(long)]
    pub timeout: Option<u64>,

    /// Retry handle. Resubmitting with a key a recent submission already used
    /// resolves that run instead of starting a second agent.
    #[arg(long)]
    pub idempotency_key: Option<String>,

    /// Per-invocation provider inner-sandbox override (Codex: read-only,
    /// workspace-write, or danger-full-access). Values the provider does not
    /// support are refused.
    #[arg(long)]
    pub provider_sandbox: Option<String>,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

impl Execute for RunAgentArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        // An omitted `--cwd` resolves here, in the operator's own process,
        // rather than in Core: Core deliberately never reads a caller's cwd,
        // because its other caller is an MCP server in an unrelated directory.
        let cwd = match self.cwd {
            Some(cwd) => cwd,
            None => std::env::current_dir()
                .map_err(|error| {
                    orbit_core::OrbitError::InvalidInput(format!(
                        "could not read the current directory; pass --cwd explicitly: {error}"
                    ))
                })?
                .to_string_lossy()
                .into_owned(),
        };
        let session_context = ToolSessionContext::default();
        let submission = runtime.submit_agent_invoke_run(AgentInvokeRequest {
            prompt: &self.prompt,
            cwd: &cwd,
            crew: self.crew.as_deref(),
            timeout_seconds: self.timeout,
            idempotency_key: self.idempotency_key.as_deref(),
            provider_sandbox: self.provider_sandbox.as_deref(),
            actor: None,
            session_context: &session_context,
        })?;

        let doc = json!({
            "run_id": submission.run_id,
            "job_id": submission.job_id,
            "submitted_at": submission.submitted_at,
            "state": if submission.queued { "queued" } else { "submitted" },
            "deduplicated": submission.deduplicated,
            "timeout_seconds": submission.timeout_seconds,
            "authorized_by": submission.admission.authorized_by,
            "authorizer_provenance": submission.admission.authorizer_provenance,
            "workspace_path": submission.admission.workspace_path,
            "cwd": submission.admission.cwd,
            "sandboxed": false,
            "provider_sandbox": submission.provider_sandbox,
            "warnings": submission.warnings,
        });
        let mut lines = Vec::new();
        if submission.deduplicated {
            lines.push(format!(
                "resolved existing agent run {} for idempotency key (no new agent started)",
                submission.run_id
            ));
        } else {
            lines.push(format!(
                "submitted agent run {} ({}) in {}",
                submission.run_id,
                if submission.queued {
                    "queued"
                } else {
                    "running"
                },
                submission.admission.cwd
            ));
            lines.push(format!(
                "running outside the executor sandbox, bounded at {}s, authorized by {} ({})",
                submission.timeout_seconds,
                submission.admission.authorized_by,
                submission.admission.authorizer_provenance
            ));
            lines.push(format!("provider sandbox: {}", submission.provider_sandbox));
            lines.extend(submission.warnings.iter().cloned());
        }
        lines.push(format!(
            "track it: orbit run show {run_id}  |  orbit run logs {run_id}  |  orbit run cancel {run_id}",
            run_id = submission.run_id
        ));
        Ok(Payload::detail(doc, lines.join("\n")).into())
    }
}
