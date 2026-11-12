//! The tool-only drain operation registry.
//!
//! Names, descriptions, parameter order and registration retain the shipped
//! tool contract. These operations have no derived CLI or dashboard surface.

use orbit_types::tool::McpIdRouteDeclaration;

use crate::governance::operation::{
    CliHelp, Description, Effect, NounSpec, OperationSpec, ParamSpec, ParamType, Redaction,
    ToolOnlyRegistration, UnknownFields, mcp_only, operation_registry, required,
};

operation_registry! {
    noun: DRAIN;
    /// The drain verbs, joining their specs to exhaustive surface tables.
    pub enum DrainVerb;
    /// Operations in their original registration order.
    pub const DRAIN_OPERATIONS;
    {
        /// `orbit.drain.probe`.
        Probe => PROBE,
        /// `orbit.drain.receipt.lookup`.
        ReceiptLookup => RECEIPT_LOOKUP,
        /// `orbit.task.pull`.
        Pull => PULL,
        /// `orbit.drain.claim.bind`.
        ClaimBind => CLAIM_BIND,
        /// `orbit.drain.claim.settle`.
        ClaimSettle => CLAIM_SETTLE,
        /// `orbit.drain.claims`.
        Claims => CLAIMS,
        /// `orbit.drain.claim.effort`.
        ClaimEffort => CLAIM_EFFORT,
    }
}

/// The tool-only drain noun.
/// The common `orbit` prefix accommodates the historical `orbit.task.pull`
/// name alongside `orbit.drain.*`; full tool names live on each operation.
pub const DRAIN: NounSpec<DrainVerb> = NounSpec {
    cli_name: "drain",
    tool_prefix: "orbit",
    tool_only_registration: ToolOnlyRegistration::Active,
    cli_about: "",
    audit_target_type: "drain",
    audit_target_param: "",
    cli_groups: &[],
    operations: DRAIN_OPERATIONS,
};

const PROBE: OperationSpec<DrainVerb> = OperationSpec {
    verb: DrainVerb::Probe,
    cli_path: &[],
    tool_name: "orbit.drain.probe",
    tool_description: "Read-only preflight for the distributed drain, served by the workspace owner. Reports the owner machine, binary version, distributed-drain protocol schema, the capabilities this session holds, the diagnostic caller machine, the owner-resolved ship configuration, and both review switches (`review.timing` and the after-landing auto-task). Declaring `caller_version`, `caller_schema`, or `caller_before_pr` also reports the first refusal an admission would raise, in the order admission applies it. It creates no request receipt, reservation, claim, or task transition, and it is never a health check for admission itself.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[
        mcp_only(
            "caller_version",
            ParamType::String,
            "Optional. The calling binary's version. When supplied, the response reports whether it matches this owner instead of leaving the caller to compare.",
        ),
        mcp_only(
            "caller_schema",
            ParamType::Integer,
            "Optional. The caller's distributed-drain wire-protocol schema version. This is not the MCP protocol revision and not the orchestration schema version.",
        ),
        mcp_only(
            "caller_fingerprint",
            ParamType::String,
            "Optional. Type-derived pull request schema fingerprint. Discover the owner's protocol_fingerprint before declaring this field; a mismatch raises protocol_skew before any pull.",
        ),
        mcp_only(
            "caller_before_pr",
            ParamType::Boolean,
            "Optional. Whether the executor's `review.timing` is `before_pr`. Diagnostic only: a claimed leaf runs the before-PR review the owner's ship contract captures, never the executor's own. After-landing review never affects admission.",
        ),
        // Revision-4 followers still send this ignored legacy field.
        ParamSpec {
            name: "caller_review_policy",
            aliases: &[],
            param_type: ParamType::String,
            required: false,
            preserve_empty: false,
            default: None,
            redact: Redaction::None,
            mcp_description: None,
            cli: None,
        },
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};

const RECEIPT_LOOKUP: OperationSpec<DrainVerb> = OperationSpec {
    verb: DrainVerb::ReceiptLookup,
    cli_path: &[],
    tool_name: "orbit.drain.receipt.lookup",
    tool_description: "Read-only reconciliation of one distributed-drain admission request, served by the workspace owner. Returns the original receipt and the current claim phase (`found`), a non-reusable tombstone (`expired`), or `not_found`. It creates no receipt, binds no run, and grants no execution authority: a receipt is historical evidence, and `not_found` is not proof that an earlier request cannot still arrive, so it never licenses a replacement request under a new ID.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[
        required(mcp_only(
            "request_id",
            ParamType::String,
            "The durable request ID the original admission was made with. It is read verbatim; the stored receipt is never rewritten to match the caller's current binary.",
        )),
        mcp_only(
            "machine_id",
            ParamType::String,
            "Optional receipt namespace: the machine the original request was made from. Defaults to this session's own machine. Naming another machine is cross-attempt inspection and requires operator capability.",
        ),
        mcp_only(
            "lookup_schema",
            ParamType::Integer,
            "Optional lookup protocol version; defaults to 1. Versioned independently of admission, so an upgraded client can reconcile an old request. An incompatible version is refused — reconcile through owner claim inspection instead of minting a replacement request.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};

const PULL: OperationSpec<DrainVerb> = OperationSpec {
    verb: DrainVerb::Pull,
    cli_path: &[],
    tool_name: "orbit.task.pull",
    tool_description: "Distributed-drain admission, served by the workspace owner. Selects the first ready, valid, conflict-free backlog task in the owner's order and, in one transaction, reserves its footprint, records an execution claim for the calling machine, moves it to `in-progress` and stores the request receipt. Answers `idle` (a receipt with no claim) when nothing is admissible. The calling machine is the session's, never a field of this input. A claim grants execution of that one attempt only; completion still needs owner approval.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        required(mcp_only(
            "request_id",
            ParamType::String,
            "Durable unique ID for one intended admission, persisted by the caller before sending. Reuse it unchanged after any uncertainty: a retry replays the stored receipt and never admits a second task.",
        )),
        required(mcp_only(
            "caller_version",
            ParamType::String,
            "The calling binary's version. It must equal the owner's.",
        )),
        required(mcp_only(
            "caller_schema",
            ParamType::Integer,
            "The caller's distributed-drain wire-protocol schema version.",
        )),
        required(mcp_only(
            "caller_before_pr",
            ParamType::Boolean,
            "Whether the executor's drain captured `review.timing = before_pr` at submission. Diagnostic only: a claimed leaf runs the before-PR review the owner's `ship` contract captures, never the executor's own. After-landing review never affects admission.",
        )),
        mcp_only(
            "caller_fingerprint",
            ParamType::String,
            "Type-derived pull request fingerprint negotiated with orbit.drain.probe. A mismatch is refused as protocol_skew before deserialization; optional for historical requests.",
        ),
        mcp_only(
            "review_gate",
            ParamType::Boolean,
            "Whether the executor's claimed PR leaf runs the before-PR review the `ship` contract captures. An owner with `review.timing = before_pr` refuses an executor that does not declare it as `before_pr_unsupported`. Omitted: `false`.",
        ),
        mcp_only(
            "visual_resolution",
            ParamType::Integer,
            "Retired with visual review: accepted from an executor on an earlier build and ignored. Omit it.",
        ),
        mcp_only(
            "caller_review_policy",
            ParamType::String,
            "Deprecated and ignored: the review-policy label distributed-drain protocol revisions before 5 sent. Accepted only so an older caller is refused as `protocol_mismatch` rather than for an unknown field.",
        ),
        required(mcp_only(
            "run_context",
            ParamType::Object,
            "The calling drain: `run_id`, `job_name`, and an optional diagnostic `machine_name`.",
        )),
        required(mcp_only(
            "ship",
            ParamType::Object,
            "The ship contract this owner resolves, exactly as `orbit.drain.probe` reported it. A new request carrying anything else is refused as `ship_contract_mismatch`, because the receipt freezes it.",
        )),
        mcp_only(
            "crews",
            ParamType::Object,
            "The crews the executor can run: `runnable` (its window preflight's crew names), `default_crew` (what a task naming no crew runs as there) and `excluded` (crews it cannot run, with why). A task whose crew it cannot run is skipped and stays in the backlog. Omitted: every crew is admissible.",
        ),
        mcp_only(
            "os",
            ParamType::String,
            "The executor's host OS: `linux`, `macos` or `windows`. A task tagged `os:<name>` is admitted only to an executor whose OS it names, and is otherwise skipped and stays in the backlog. Omitted: only tasks without an `os:` tag are admissible.",
        ),
        mcp_only(
            "browser_kit",
            ParamType::Boolean,
            "Whether the executor's host has a verified headless browser kit. A task tagged `ui` is admitted only to an executor that declares one, and is otherwise skipped and stays in the backlog. Omitted: `false`.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};

const CLAIM_BIND: OperationSpec<DrainVerb> = OperationSpec {
    verb: DrainVerb::ClaimBind,
    cli_path: &[],
    tool_name: "orbit.drain.claim.bind",
    tool_description: "Bind an executor's local leaf run to its execution claim on the owner, moving the claim from `claimed` to `running`. Idempotent: repeating the same run returns the recorded outcome, and a different run is refused, so one claim never has two leaves. The owner fences the claim against the session's own machine; a claim admitted to another machine, or already settled or revoked, is refused as stale.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        required(mcp_only(
            "claim_id",
            ParamType::String,
            "The claim `orbit.task.pull` admitted to this machine.",
        )),
        required(mcp_only(
            "run_id",
            ParamType::String,
            "The one local leaf run the executor created for this claim.",
        )),
        required(mcp_only(
            "ship",
            ParamType::Object,
            "The ship contract the claim was admitted under, from its receipt.",
        )),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};

const CLAIM_SETTLE: OperationSpec<DrainVerb> = OperationSpec {
    verb: DrainVerb::ClaimSettle,
    cli_path: &[],
    tool_name: "orbit.drain.claim.settle",
    tool_description: "Settle an execution claim on the owner. A typed handoff is accepted only after the owner reads the pull request from the provider and resolves its candidate and base in the owner's own checkout, and only with digest-pinned validation evidence; it moves the task to `review`, which awaits owner approval — it does not merge. A failure records its evidence and blocks the task. A release (a cancelled drain giving back work it will not finish) returns the task to the backlog with its reason as a comment. Each releases only this claim's reservation. Retries replay the recorded outcome.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        ParamSpec {
            name: "claim_id",
            aliases: &[],
            param_type: ParamType::String,
            required: true,
            preserve_empty: false,
            default: None,
            redact: Redaction::None,
            mcp_description: Some(Description::Static("The claim being settled.")),
            cli: None,
        },
        mcp_only(
            "run_id",
            ParamType::String,
            "The leaf run bound to this claim, when one was bound.",
        ),
        required(mcp_only(
            "settlement",
            ParamType::Object,
            "The executor's durable settlement: `{\"AcceptHandoff\": <typed handoff>}` for a published pull request, `{\"Fail\": <evidence>}`, or `{\"Release\": <evidence>}` to hand an unfinished claim back. Nothing else settles a claim.",
        )),
        mcp_only(
            "crew",
            ParamType::String,
            "Optional. The crew the leaf run resolved. Attribution only: the owner records it on the claim's settlement record, after any crew the settlement's own evidence names, and never uses it to validate or decide the settlement.",
        ),
        mcp_only(
            "crew_model",
            ParamType::String,
            "Optional. The model the leaf's crew resolved to. Attribution only, like `crew`.",
        ),
        mcp_only(
            "branch",
            ParamType::String,
            "Optional. The branch the leaf run's worktree recorded. Attribution only, like `crew`; the owner shows it so an operator can inspect the run.",
        ),
        mcp_only(
            "worktree_path",
            ParamType::String,
            "Optional. The absolute path of the leaf run's worktree on the executing machine. Attribution only, like `branch`.",
        ),
        mcp_only(
            "reasoning_effort",
            ParamType::String,
            "Optional. The effective reasoning effort of the leaf's main implementing invocation: its name (`low`, `medium`, `high`, `xhigh` or `max`), or `provider_default` when the provider's default applied. Omit it when the effort is unknown. Attribution only, like `crew`: the owner's agent scorecards count the attempt under it.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};

const CLAIMS: OperationSpec<DrainVerb> = OperationSpec {
    verb: DrainVerb::Claims,
    cli_path: &[],
    tool_name: "orbit.drain.claims",
    tool_description: "List this workspace's execution claims for inspection and deliberate recovery: phase, age, reservation expiry, execution machine, bound run, last recorded event, unresolved merge intent, and landing invalidation. Age, an expired reservation, and an absent local run are diagnostics, not proof that a worker died — nothing here reclaims, rebinds, or repairs a claim.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Read,
    params: &[&[]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};

const CLAIM_EFFORT: OperationSpec<DrainVerb> = OperationSpec {
    verb: DrainVerb::ClaimEffort,
    cli_path: &[],
    tool_name: "orbit.drain.claim.effort",
    tool_description: "Report, after the fact, the reasoning effort and implementing model a claimed leaf ran at when its settlement carried none, so the owner's agent scorecards stop counting the attempt as effort unknown or model unknown. Accepted only from the machine that ran the claim, for the leaf run bound to it, once the claim is settled; a claim that settled before the owner kept settlement records is accepted only when the task's history names it as pulled. Attribution only: it changes no claim, task, run or settlement outcome. A value the claim already holds is never overwritten: the same value replays, and a different one is refused as `effort_conflict` or `model_conflict`, recording neither.",
    cli_about: "",
    cli_help: CliHelp::NONE,
    effect: Effect::Write,
    params: &[&[
        required(mcp_only(
            "claim_id",
            ParamType::String,
            "The settled claim whose leaf ran on this machine.",
        )),
        required(mcp_only(
            "run_id",
            ParamType::String,
            "The leaf run bound to that claim.",
        )),
        required(mcp_only(
            "reasoning_effort",
            ParamType::String,
            "The effective reasoning effort of the leaf's main implementing invocation, read from this machine's own records: its name (`low`, `medium`, `high`, `xhigh` or `max`), or `provider_default` when the provider's default applied. Never sent for an unknown effort.",
        )),
        mcp_only(
            "crew_model",
            ParamType::String,
            "The model of that same implementing invocation (else of its latest matching launch record), read from this machine's own records. Omitted when unknown; never derived from a crew name or configuration.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
    id_route: McpIdRouteDeclaration::Unspecified,
};
