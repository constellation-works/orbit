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

const BLOCK_TRIAGE_SUBJECT_VALUES: &[EnumValue] = &[EnumValue::plain("pilot_warning")];

const BLOCK_TRIAGE_ACTION_VALUES: &[EnumValue] = &[
    EnumValue::plain("candidates"),
    EnumValue::plain("propose"),
    EnumValue::plain("report"),
];

/// Read the pilot warning holds awaiting triage, or record one triage
/// decision on one: the agent surface of the pilot-warning escalation triage.
/// The tool keeps its name; blocked tasks are decided by final recovery.
pub(super) const BLOCK_TRIAGE: TaskOperation = TaskOperation {
    verb: TaskVerb::BlockTriage,
    cli_path: &[],
    tool_name: "orbit.task.block_triage",
    tool_description: "Triage proposed tasks a task-pilot warning holds. `candidates` lists those awaiting triage; `propose` opens a decision request with a short summary, two or three options (each a one-sentence summary, a one-line consequence, an optional field rewrite and an optional action: requeue, re_pilot, change_crew, split, reject or archive) and a recommendation, which the operator answers with one click; `report` records the exact human action when only something outside Orbit resolves the hold. Prefer `propose` whenever the operator resolves the hold by choosing. Never approves, rejects or archives a task itself. Blocked tasks are no longer triaged here: final recovery decides them. A decision on a task a live triage run holds needs that run's inspection proof. Policy detail, the pilot-warning marker and the proof recipe: the orbit skill's references/tool-surface.md, section `Pilot-warning triage`.",
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
                "candidates, propose, or report",
            )),
            mcp_only(
                "subject",
                ParamType::Enum(BLOCK_TRIAGE_SUBJECT_VALUES),
                "what is triaged: pilot_warning (the default and only subject), a proposed task's pilot warning hold",
            ),
            mcp_only(
                "id",
                ParamType::String,
                "propose and report: the pilot-held task",
            ),
            free_text(mcp_only(
                "summary",
                ParamType::String,
                "propose and report: one or two plain-language sentences naming the decision needed, readable without opening the code",
            )),
            free_text(mcp_only(
                "reason",
                ParamType::String,
                "report: the exact action only a human outside Orbit can take",
            )),
            mcp_only(
                "options",
                ParamType::Array,
                "propose: two or three {summary, consequence, rewrite, action} objects; rewrite is {description?, acceptance_criteria?} with the field's complete new value; action is {kind: requeue | re_pilot | change_crew (crew) | split (title, description, acceptance_criteria) | reject | archive}; every option needs a rewrite, an action or both",
            ),
            mcp_only(
                "recommended",
                ParamType::Integer,
                "propose: 1-based index of the recommended option",
            ),
            mcp_only(
                "source_revision",
                ParamType::String,
                "inspection proof: the pinned commit id the caller inspected, or null in a non-Git workspace; supply with observed_revision and observed_tree",
            ),
            mcp_only(
                "observed_revision",
                ParamType::String,
                "inspection proof: `git rev-parse HEAD` in the inspection checkout",
            ),
            mcp_only(
                "observed_tree",
                ParamType::String,
                "inspection proof: `git rev-parse 'HEAD^{tree}'` in the inspection checkout",
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

const RECOVER_STATUS_VALUES: &[EnumValue] =
    &[EnumValue::plain("blocked"), EnumValue::plain("backlog")];

/// Recover a follower claim to blocked or backlog, fencing its attempt.
pub(super) const RECOVER_CLAIM: TaskOperation = TaskOperation {
    verb: TaskVerb::RecoverClaim,
    cli_path: &["recover-claim"],
    tool_name: "orbit.task.recover_claim",
    tool_description: "Recover a live execution claim on its owner workspace to blocked or backlog, fencing the attempt, invalidating pending landing authority and releasing its reservation in one transaction. Requires trusted operator authority; unavailable to managed runs. Supply the claim phase you observed, the target status, a required reason and a durable request_key; resubmitting the same request_key with the same input replays the original recovery.",
    cli_about: "Recover a live claim to blocked or backlog with an audited reason",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        HOST_PARAM,
        WORKSPACE_PARAM,
        required(both(
            "claim_id",
            ParamType::String,
            "Live claim to recover",
            CliArgKind::Positional,
            "Claim to recover",
        )),
        required(both(
            "expected_phase",
            ParamType::String,
            "Claim phase you observed; a claim that moved on is refused",
            flag_named("phase", "PHASE"),
            "Expected claim phase you observed",
        )),
        required(both(
            "status",
            ParamType::Enum(RECOVER_STATUS_VALUES),
            "Target status: blocked for diagnosis or backlog for retry",
            flag_named("status", "STATUS"),
            "Move the task to blocked or backlog",
        )),
        required(free_text(both(
            "reason",
            ParamType::String,
            "Required operator reason retained durably",
            flag_named("reason", "REASON"),
            "Operator reason recorded with the recovery",
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

/// Revoke a handoff's completion authority.
pub(super) const REVOKE_HANDOFF: TaskOperation = TaskOperation {
    verb: TaskVerb::RevokeHandoff,
    cli_path: &["revoke-handoff"],
    tool_name: "orbit.task.revoke_handoff",
    tool_description: "Revoke a handoff's completion authority on its owner workspace, cancelling its pending landing request while the task stays in review. Requires trusted operator authority; unavailable to managed runs. Compare the exact accepted candidate/base, supply a required reason and a durable request_key; resubmitting the same request_key with the same input replays the original revocation. Refused while an external merge intent is unresolved.",
    cli_about: "Revoke a handoff's completion authority with an audited reason",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        HOST_PARAM,
        WORKSPACE_PARAM,
        required(both(
            "handoff_id",
            ParamType::String,
            "Accepted handoff whose authority to revoke",
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
        required(free_text(both(
            "reason",
            ParamType::String,
            "Required operator reason retained durably",
            flag_named("reason", "REASON"),
            "Operator reason recorded with the revocation",
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
