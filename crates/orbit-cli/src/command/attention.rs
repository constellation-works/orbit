//! The root attention CLI derives its inputs from the shared tool operation.
use super::{
    CommandOut, Execute, Payload,
    operation_args::{Invocation, augment_operation_args, input_from_matches},
};
use crate::output::payload::{AttentionRow, Block};
use clap::{Arg, ArgMatches, Args, Command, FromArgMatches};
use orbit_common::governance::{
    attention::{ATTENTION_WAIT_TOKEN_ENV, AttentionVerb},
    operation::OperationVerb,
};
use orbit_core::{
    OrbitRuntime,
    application::attention::{AttentionWaitOutcome, AttentionWaitStatus},
};
use serde_json::{Value, json};

/// Exit code when the wait ended with an empty delta and unchanged `degraded` set.
const WAIT_EXIT_TIMEOUT: i32 = 3;
/// Exit code when the delta is empty but the `degraded` set changed.
const WAIT_EXIT_DEGRADED_CHANGED: i32 = 4;
/// Exit code when every read failed to the deadline, so nothing is known.
const WAIT_EXIT_UNREADABLE: i32 = 5;

/// The timing of `orbit attention wait`.
pub struct AttentionWait {
    timeout: String,
    interval: Option<String>,
}

pub struct AttentionCommand {
    pub query: Invocation<AttentionVerb>,
    /// Set for `orbit attention wait`, whose filters are in `query.input`.
    pub wait: Option<AttentionWait>,
}
impl Args for AttentionCommand {
    fn augment_args(cmd: Command) -> Command {
        // The wait takes the read's own filters and cursor, then adds its timing.
        let wait = augment_operation_args(Command::new("wait"), AttentionVerb::Query.spec())
            // The grant bounds a wait to workspaces on this host.
            .mut_arg("all", |arg| arg.hide(true))
            .mut_arg("host", |arg| arg.hide(true))
            .about("Block until attention changes, with no model turn")
            .long_about(
                "Block until the attention delta since --since is non-empty or its degraded set \
                 changes, or the timeout passes.\n\nThe caller needs a grant from a session: call \
                 orbit.attention with wait_grant true over MCP and export the returned token as \
                 ORBIT_ATTENTION_WAIT_TOKEN. Without one the wait is refused.\n\nExit codes: 0 \
                 changed items (printed with the next cursor), 3 timeout with no change, 4 degraded \
                 set changed, 5 every read failed. A timeout still lists degraded sources: it is \
                 not all-clear while any are listed.",
            )
            .arg(
                Arg::new("timeout")
                    .long("timeout")
                    .value_name("DURATION")
                    .default_value("30m")
                    .help("Longest to wait, such as 90s, 30m or 2h (max 24h)"),
            )
            .arg(
                Arg::new("interval")
                    .long("interval")
                    .value_name("DURATION")
                    .help("Gap between polls (default 15s, minimum 5s)"),
            );
        augment_operation_args(cmd, AttentionVerb::Query.spec()).subcommand(wait)
    }
    fn augment_args_for_update(cmd: Command) -> Command {
        Self::augment_args(cmd)
    }
}
impl FromArgMatches for AttentionCommand {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        let spec = AttentionVerb::Query.spec();
        let wait = matches.subcommand_matches("wait");
        Ok(Self {
            query: Invocation {
                spec,
                input: input_from_matches(spec, wait.unwrap_or(matches)),
            },
            wait: wait.map(|wait| AttentionWait {
                timeout: wait
                    .get_one::<String>("timeout")
                    .cloned()
                    .unwrap_or_else(|| "30m".into()),
                interval: wait.get_one::<String>("interval").cloned(),
            }),
        })
    }
    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}
impl Execute for AttentionCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if let Some(wait) = &self.wait {
            return execute_wait(runtime, &self.query.input, wait);
        }
        let value = runtime.run_tool(self.query.spec.tool_name, Value::Object(self.query.input))?;
        render_attention_envelope(value)
    }
}

fn execute_wait(
    runtime: &OrbitRuntime,
    input: &serde_json::Map<String, Value>,
    wait: &AttentionWait,
) -> CommandOut {
    let token = std::env::var(ATTENTION_WAIT_TOKEN_ENV).unwrap_or_default();
    let outcome = runtime.attention_wait(
        &token,
        Value::Object(input.clone()),
        &wait.timeout,
        wait.interval.as_deref(),
    )?;
    render_attention_wait(outcome)
}

/// Render a finished wait: the envelope plus a `wait` record naming why it
/// returned, with the exit code that tells a shell caller the same.
fn render_attention_wait(outcome: AttentionWaitOutcome) -> CommandOut {
    let (label, exit_code) = match outcome.status {
        AttentionWaitStatus::Changed => ("changed", 0),
        AttentionWaitStatus::DegradedChanged => ("degraded_changed", WAIT_EXIT_DEGRADED_CHANGED),
        AttentionWaitStatus::TimedOut => ("timeout", WAIT_EXIT_TIMEOUT),
        AttentionWaitStatus::Unreadable => ("unreadable", WAIT_EXIT_UNREADABLE),
    };
    let degraded_count = outcome
        .response
        .as_ref()
        .and_then(|response| response["degraded"].as_array())
        .map_or(0, Vec::len);
    let mut wait = json!({
        "status": label,
        "exit_code": exit_code,
        "waited_seconds": outcome.waited_seconds,
        // An empty `degraded` is the only all-clear a timeout can claim.
        "all_sources_readable": outcome.response.is_some() && degraded_count == 0,
    });
    if let Some(error) = &outcome.error {
        wait["error"] = json!(error);
    }
    let note = match outcome.status {
        AttentionWaitStatus::Changed => "attention changed".to_string(),
        AttentionWaitStatus::DegradedChanged => "the set of unreadable sources changed".to_string(),
        AttentionWaitStatus::TimedOut if degraded_count == 0 => {
            "timed out; nothing changed".to_string()
        }
        AttentionWaitStatus::TimedOut => format!(
            "timed out; nothing changed, but {degraded_count} source(s) were unreadable, so this is not all-clear"
        ),
        AttentionWaitStatus::Unreadable => format!(
            "attention could not be read: {}",
            outcome.error.as_deref().unwrap_or("unknown error")
        ),
    };
    let Some(mut response) = outcome.response else {
        let payload = Payload::detail(json!({"wait": wait}), note);
        return Ok(payload
            .with_exit_code(exit_code)
            .with_audit_message(label)
            .into());
    };
    response["wait"] = wait;
    Ok(attention_payload(response, Some(note))
        .with_exit_code(exit_code)
        .with_audit_message(label)
        .into())
}

/// Render an attention envelope, local or merged by the federated client.
/// A federated envelope (one with `hosts`) includes the host in each item.
pub(crate) fn render_attention_envelope(value: Value) -> CommandOut {
    Ok(attention_payload(value, None).into())
}

fn attention_payload(value: Value, note: Option<String>) -> Payload {
    let federated = value["hosts"]
        .as_array()
        .is_some_and(|hosts| !hosts.is_empty());
    let mut blocks: Vec<Block> = note.map(Block::text).into_iter().collect();
    blocks.push(Block::text(value["message"].as_str().unwrap_or_default()));
    let mut records = value["items"].as_array().cloned().unwrap_or_default();
    let mut rows = Vec::with_capacity(records.len());
    for item in &records {
        let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
        let task = text(&item["refs"]["task_id"]);
        let reference = if task.is_empty() {
            text(&item["refs"]["notice_id"])
        } else {
            task
        };
        rows.push(AttentionRow {
            severity: text(&item["severity"]),
            kind: text(&item["kind"]),
            host: federated.then(|| text(&item["host"]["name"])),
            workspace: text(&item["workspace"]["name"]),
            reference,
            age: item["age_seconds"]
                .as_u64()
                .map(|age| format!("{}h", age / 3600))
                .unwrap_or_default(),
            reason: text(&item["reason"]),
            next_command: text(&item["next"]["command"]),
        });
    }
    if !rows.is_empty() {
        blocks.push(Block::Attention(rows));
    }
    for degraded in value["degraded"].as_array().into_iter().flatten() {
        blocks.push(Block::text(format!(
            "{} {}: {}",
            degraded["scope"].as_str().unwrap_or_default(),
            degraded["kind"].as_str().unwrap_or_default(),
            degraded["message"].as_str().unwrap_or_default()
        )));
    }
    records.push(json!({"summary": {"counts": value["counts"], "cursor": value["cursor"], "degraded": value["degraded"]}}));
    Payload::blocks(value, blocks).with_records(records)
}
