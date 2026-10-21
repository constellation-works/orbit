//! The task review and lifecycle verbs: `reconcile_review`, `review_reset`,
//! `requeue`, `block_triage`, `retry_handoff`, `reject` and `delete`.
//!
//! The two review verbs and `requeue` are operator commands on MCP and on the CLI, and
//! refuse an undeclared field, as their hand-written tools did. A managed
//! run is refused before any field check by `orbit-tools`' surface table.
//! `reconcile_review` multiplexes four actions behind `action`; on the CLI
//! each action is a subcommand of `orbit task reconcile-review` with its own
//! arguments. `reject` and `delete` have no CLI and no MCP placement.
//!
//! **Every string below is shipped contract.**

use orbit_types::tool::McpToolScope;

use super::operations::{IDENTITY_PARAMS, TASK_HOST_HELP, TaskOperation, TaskVerb};
use super::write_operations::WRITE_PROJECTION_PARAMS;
use crate::governance::operation::{
    CliAction, CliArgKind, CliHelp, Effect, EnumValue, ParamSpec, ParamType, UnknownFields, both,
    cli_actions, cli_only, free_text, mcp_only, required,
};

/// `--host`, inherited by every reconcile-review action.
const HOST_PARAM: ParamSpec = cli_only(
    "host",
    ParamType::String,
    CliArgKind::Flag {
        long: "host",
        delimiter: None,
        aliases: &[],
        visible_aliases: &[],
        value_name: Some("HOST"),
        multiple: false,
    },
    TASK_HOST_HELP,
);

/// The optional selector of the review verbs, which resolve an id without it.
const WORKSPACE_PARAM: ParamSpec = mcp_only(
    "workspace",
    ParamType::String,
    "Workspace selector from discovery. Omitted, a bound session uses its workspace and an unbound one resolves the task id through this host's task registry",
);

const fn flag_named(long: &'static str, value_name: &'static str) -> CliArgKind {
    CliArgKind::Flag {
        long,
        delimiter: None,
        aliases: &[],
        visible_aliases: &[],
        value_name: Some(value_name),
        multiple: false,
    }
}

/// An action's task id positional.
const fn action_id(help: &'static str) -> ParamSpec {
    required(cli_only(
        "id",
        ParamType::String,
        CliArgKind::Positional,
        help,
    ))
}

const RECONCILE_ACTION_VALUES: &[EnumValue] = &[
    EnumValue::plain("inspect"),
    EnumValue::plain("submit"),
    EnumValue::plain("status"),
    EnumValue::plain("accept_baseline"),
];

const RECONCILE_ID_HELP: &str = "Task in review whose foreign delivery merged";

/// `orbit task reconcile-review`'s subcommands, one per action.
const RECONCILE_CLI_ACTIONS: &[CliAction] = &[
    CliAction {
        name: "inspect",
        value: "inspect",
        cli_about: "Show whether the task's merged delivery can be reconciled, and why not",
        params: &[action_id(RECONCILE_ID_HELP)],
    },
    CliAction {
        name: "submit",
        value: "submit",
        cli_about: "Admit a reconciliation run; resubmitting the same key replays it",
        params: &[
            action_id(RECONCILE_ID_HELP),
            required(cli_only(
                "request_key",
                ParamType::String,
                flag_named("request", "REQUEST"),
                "Operator-chosen request key; resubmitting it replays the same reconciliation",
            )),
        ],
    },
    CliAction {
        name: "status",
        value: "status",
        cli_about: "Show reconciliation outcomes, their runs and the next step",
        params: &[
            action_id("Task whose reconciliations to show"),
            cli_only(
                "reconciliation_id",
                ParamType::String,
                flag_named("reconciliation", "RECONCILIATION"),
                "Show only this reconciliation",
            ),
        ],
    },
    CliAction {
        name: "accept-baseline",
        value: "accept_baseline",
        cli_about: "Record an audited disposition of a failure the base already had",
        params: &[
            action_id("Task the reconciliation belongs to"),
            required(cli_only(
                "reconciliation_id",
                ParamType::String,
                flag_named("reconciliation", "RECONCILIATION"),
                "Reconciliation waiting for a baseline disposition",
            )),
            required(cli_only(
                "command",
                ParamType::String,
                CliArgKind::flag("command"),
                "Required command whose failure reproduced at the base",
            )),
            required(cli_only(
                "remediation_commit",
                ParamType::String,
                flag_named("remediation", "REMEDIATION"),
                "Commit on the landing branch, landed on top of the delivery's merge or squash commit, that remediates the failure",
            )),
            required(cli_only(
                "reason",
                ParamType::String,
                CliArgKind::flag("reason"),
                "Why the baseline failure is accepted; recorded with the disposition",
            )),
        ],
    },
];

pub(super) const RECONCILE_REVIEW: TaskOperation = TaskOperation {
    verb: TaskVerb::ReconcileReview,
    cli_path: &["reconcile-review"],
    tool_name: "orbit.task.reconcile_review",
    tool_description: "Reconcile the already-merged head of a recovered follower delivery whose pull request merged at a head other than the handed-off candidate: run required validation and an independent read-only review of exactly that head and record owner-held evidence the desktop review can complete from. `inspect` shows eligibility, `submit` admits a run (resubmitting the same request_key replays it), `status` reports outcomes and next steps, `accept_baseline` records an operator disposition of a failure the base already had. Requires trusted operator authority; unavailable to managed runs.",
    cli_about: "Validate and independently review the already-merged head of a recovered follower delivery so its task can complete from review",
    cli_help: CliHelp::NONE,
    // `inspect` and `status` only read, but every action keeps the
    // read-write runtime the command always opened.
    effect: Effect::Write,
    params: &[&[
        HOST_PARAM,
        WORKSPACE_PARAM,
        required(cli_actions(
            mcp_only(
                "action",
                ParamType::Enum(RECONCILE_ACTION_VALUES),
                "inspect, submit, status, or accept_baseline",
            ),
            RECONCILE_CLI_ACTIONS,
        )),
        required(mcp_only("id", ParamType::String, RECONCILE_ID_HELP)),
        mcp_only(
            "request_key",
            ParamType::String,
            "submit: operator-chosen key; resubmitting it replays the same reconciliation",
        ),
        mcp_only(
            "reconciliation_id",
            ParamType::String,
            "status, accept_baseline: the reconciliation to act on",
        ),
        // Selects an already-authorized stored command, so it is kept
        // byte-for-byte for exact evidence matching.
        mcp_only(
            "command",
            ParamType::String,
            "accept_baseline: the required command whose failure reproduced at the base",
        ),
        mcp_only(
            "remediation_commit",
            ParamType::String,
            "accept_baseline: commit on the landing branch, landed on top of the delivery's merge or squash commit, that remediates that failure",
        ),
        free_text(mcp_only(
            "reason",
            ParamType::String,
            "accept_baseline: why the baseline failure is accepted",
        )),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

pub(super) const REVIEW_RESET: TaskOperation = TaskOperation {
    verb: TaskVerb::ReviewReset,
    cli_path: &["review-reset"],
    tool_name: "orbit.task.review_reset",
    tool_description: "Record an operator decision closing the open review attempt and renewing one explicitly selected lineage budget while preserving every attempt. Requires trusted operator authority; unavailable to managed runs. Inspect the returned ledger after a lost reply before retrying.",
    cli_about: "Reset a review lineage budget with an audited operator reason",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        HOST_PARAM,
        WORKSPACE_PARAM,
        required(both(
            "id",
            ParamType::String,
            "Task in the selected lineage",
            CliArgKind::Positional,
            "Task belonging to the selected lineage",
        )),
        required(both(
            "lineage_key",
            ParamType::String,
            "Exact lineage key from the admission refusal or review manifest",
            flag_named("lineage", "LINEAGE"),
            "Exact lineage key from the refusal or review manifest",
        )),
        required(free_text(both(
            "reason",
            ParamType::String,
            "Required explanation retained in the decision history",
            CliArgKind::flag("reason"),
            "Explanation recorded with the operator decision",
        ))),
        both(
            "adopt_configured_budget",
            ParamType::Switch,
            "Explicitly adopt current configured review limits; default retains the captured limits",
            CliArgKind::flag("adopt-configured-budget"),
            "Adopt the current configured budget instead of the captured budget",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

/// Requeue a blocked task onto the candidate its stopped run published, or
/// the latest candidate a claim of the task kept.
pub(super) const REQUEUE: TaskOperation = TaskOperation {
    verb: TaskVerb::Requeue,
    cli_path: &["requeue"],
    tool_name: "orbit.task.requeue",
    tool_description: "Return a blocked task to the backlog so its next run resumes the candidate and pull request its stopped run published, re-authenticating that publication exactly as automatic adoption does, or the latest candidate a claim of the task kept. A later claim that ended without a candidate does not replace that one. Requires trusted operator authority; unavailable to managed runs. A task whose run has not stopped, or whose claim is unsettled, is refused. A changed task specification is refused with the option to set discard_candidate, which closes the task's undelivered Orbit pull requests and lets the next run implement fresh.",
    cli_about: "Requeue a blocked task onto its published candidate",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        HOST_PARAM,
        WORKSPACE_PARAM,
        required(both(
            "id",
            ParamType::String,
            "Blocked task to requeue",
            CliArgKind::Positional,
            "Blocked task to requeue",
        )),
        required(free_text(both(
            "reason",
            ParamType::String,
            "Required explanation retained in the task history",
            CliArgKind::flag("reason"),
            "Explanation recorded with the operator decision",
        ))),
        both(
            "discard_candidate",
            ParamType::Switch,
            "Discard the published or kept candidate, close the task's undelivered Orbit pull requests and let the next run implement fresh",
            CliArgKind::flag("discard-candidate"),
            "Discard the published or kept candidate and rerun from scratch",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

const BLOCK_TRIAGE_ACTION_VALUES: &[EnumValue] = &[
    EnumValue::plain("candidates"),
    EnumValue::plain("requeue"),
    EnumValue::plain("propose"),
    EnumValue::plain("report"),
];

/// Read the block episodes awaiting triage, or record one triage decision on
/// a blocked task: the blocked-task triage's agent surface.
pub(super) const BLOCK_TRIAGE: TaskOperation = TaskOperation {
    verb: TaskVerb::BlockTriage,
    cli_path: &[],
    tool_name: "orbit.task.block_triage",
    tool_description: "Triage escalated blocks. `candidates` lists every blocked task with whether its current block episode awaits triage: its final-recovery decision escalated, or it has none and the backstop will never decide it; an episode already triaged or touched by a human after the block is excluded. Each other action records one decision on a qualifying episode, as one task comment plus a history marker, and refuses any other episode. `requeue` returns the task to the backlog as a final-recovery requeue, counted against the shared final-recovery requeue bound (2 per 24 hours): past the bound, for a task tagged security or security-review, or for a run another machine executed, it is recorded as refused and the task stays blocked; when the count cannot be read nothing is written. `propose` records the quoted blocker, two or three options with consequences and ready-to-apply replacement text, and a recommendation, and changes nothing else. `report` records the human action and changes nothing else. Never completes, rejects or archives a task.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    // `candidates` only reads, but the decisions write.
    effect: Effect::Write,
    params: &[
        &[
            WORKSPACE_PARAM,
            required(mcp_only(
                "action",
                ParamType::Enum(BLOCK_TRIAGE_ACTION_VALUES),
                "candidates, requeue, propose, or report",
            )),
            mcp_only(
                "id",
                ParamType::String,
                "requeue, propose, report: the blocked task",
            ),
            free_text(mcp_only(
                "blocker",
                ParamType::String,
                "requeue, propose, report: the escalation reason, quoted in the comment",
            )),
            free_text(mcp_only(
                "reason",
                ParamType::String,
                "requeue: the repair applied and why the task can run again; report: the specific action only a human can take",
            )),
            mcp_only(
                "options",
                ParamType::Array,
                "propose: two or three {summary, consequences, replacement} objects; replacement is the exact text for each changed description section or criterion",
            ),
            mcp_only(
                "recommended",
                ParamType::Integer,
                "propose: 1-based index of the recommended option",
            ),
        ],
        IDENTITY_PARAMS,
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};

/// The task a lifecycle tool acts on.
const TASK_ID_PARAM: ParamSpec = required(mcp_only("id", ParamType::String, "task ID"));

// Task rejection is a human/operator decision: CLI and dashboard only.
pub(super) const REJECT: TaskOperation = TaskOperation {
    verb: TaskVerb::Reject,
    cli_path: &[],
    tool_name: "orbit.task.reject",
    tool_description: "Reject an Orbit task and return the updated task JSON",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[
        &[
            TASK_ID_PARAM,
            required(free_text(mcp_only(
                "note",
                ParamType::String,
                "Required rejection note",
            ))),
            free_text(mcp_only(
                "comment",
                ParamType::String,
                "Optional task comment to append",
            )),
        ],
        WRITE_PROJECTION_PARAMS,
        IDENTITY_PARAMS,
    ],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};

pub(super) const DELETE: TaskOperation = TaskOperation {
    verb: TaskVerb::Delete,
    cli_path: &[],
    tool_name: "orbit.task.delete",
    tool_description: "Permanently delete an Orbit task and return confirmation JSON",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        TASK_ID_PARAM,
        mcp_only(
            "force",
            ParamType::Boolean,
            "When true, allow permanent deletion outside proposed or rejected status",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
};

/// Retry an existing authorized handoff, preserving its immutable foreign binding.
pub(super) const RETRY_HANDOFF: TaskOperation = TaskOperation {
    verb: TaskVerb::RetryHandoff,
    cli_path: &["retry-handoff"],
    tool_name: "orbit.task.retry_handoff",
    tool_description: "Retry a stopped or uncertain authorized handoff on its explicit owner workspace. Requires trusted operator authority; unavailable to managed runs. Compare the exact accepted candidate/base and latest attempt/state, supply a reason and a durable request_key. Replaying identical input returns the original landing run. The normal landing job reconciles external intent and rechecks all merge gates.",
    cli_about: "Retry a stopped authorized handoff through normal owner landing",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        required(cli_only(
            "owner_workspace",
            ParamType::String,
            flag_named("owner-workspace", "WORKSPACE"),
            "Explicit owner logical workspace ID or absolute checkout path",
        )),
        required(mcp_only(
            "workspace",
            ParamType::String,
            "Explicit owner logical workspace ID or absolute checkout path",
        )),
        required(both(
            "handoff_id",
            ParamType::String,
            "Immutable accepted handoff identity",
            CliArgKind::Positional,
            "Accepted handoff identity",
        )),
        required(both(
            "expected_candidate_commit",
            ParamType::String,
            "Accepted candidate commit the operator observed",
            flag_named("candidate", "COMMIT"),
            "Expected accepted candidate commit",
        )),
        required(both(
            "expected_base_commit",
            ParamType::String,
            "Accepted base commit the operator observed",
            flag_named("base", "COMMIT"),
            "Expected accepted base commit",
        )),
        required(both(
            "expected_latest_attempt",
            ParamType::Integer,
            "Latest landing attempt number the operator observed",
            flag_named("attempt", "NUMBER"),
            "Expected latest landing attempt number",
        )),
        required(both(
            "expected_latest_state",
            ParamType::Enum(&[EnumValue::plain("stopped"), EnumValue::plain("dispatched")]),
            "Latest state; dispatched is eligible only with unresolved intent and no live owner job",
            flag_named("state", "STATE"),
            "Expected latest state: stopped or dispatched with uncertain intent",
        )),
        required(free_text(both(
            "reason",
            ParamType::String,
            "Required operator reason retained durably",
            flag_named("reason", "REASON"),
            "Operator reason recorded with the retry",
        ))),
        required(both(
            "request_key",
            ParamType::String,
            "Durable request key; reuse exactly after a lost reply",
            flag_named("request", "KEY"),
            "Durable replay key; reuse after a lost reply",
        )),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Refuse,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
};
