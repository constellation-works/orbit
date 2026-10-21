//! The tool-only drain operation registry.
//!
//! Names, descriptions, parameter order and registration retain the shipped
//! tool contract. These operations have no derived CLI or dashboard surface.

use crate::governance::operation::{
    CliHelp, Description, Effect, NounSpec, OperationSpec, ParamSpec, ParamType, Redaction,
    ToolOnlyRegistration, UnknownFields, operation_registry,
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
        param(
            "caller_version",
            ParamType::String,
            false,
            "Optional. The calling binary's version. When supplied, the response reports whether it matches this owner instead of leaving the caller to compare.",
        ),
        param(
            "caller_schema",
            ParamType::Integer,
            false,
            "Optional. The caller's distributed-drain wire-protocol schema version. This is not the MCP protocol revision and not the orchestration schema version.",
        ),
        param(
            "caller_fingerprint",
            ParamType::String,
            false,
            "Optional. Type-derived pull request schema fingerprint. Discover the owner's protocol_fingerprint before declaring this field; a mismatch raises protocol_skew before any pull.",
        ),
        param(
            "caller_before_pr",
            ParamType::Boolean,
            false,
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
        param(
            "request_id",
            ParamType::String,
            true,
            "The durable request ID the original admission was made with. It is read verbatim; the stored receipt is never rewritten to match the caller's current binary.",
        ),
        param(
            "machine_id",
            ParamType::String,
            false,
            "Optional receipt namespace: the machine the original request was made from. Defaults to this session's own machine. Naming another machine is cross-attempt inspection and requires operator capability.",
        ),
        param(
            "lookup_schema",
            ParamType::Integer,
            false,
            "Optional lookup protocol version; defaults to 1. Versioned independently of admission, so an upgraded client can reconcile an old request. An incompatible version is refused — reconcile through owner claim inspection instead of minting a replacement request.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
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
        param(
            "request_id",
            ParamType::String,
            true,
            "Durable unique ID for one intended admission, persisted by the caller before sending. Reuse it unchanged after any uncertainty: a retry replays the stored receipt and never admits a second task.",
        ),
        param(
            "caller_version",
            ParamType::String,
            true,
            "The calling binary's version. It must equal the owner's.",
        ),
        param(
            "caller_schema",
            ParamType::Integer,
            true,
            "The caller's distributed-drain wire-protocol schema version.",
        ),
        param(
            "caller_before_pr",
            ParamType::Boolean,
            true,
            "Whether the executor's drain captured `review.timing = before_pr` at submission. Diagnostic only: a claimed leaf runs the before-PR review the owner's `ship` contract captures, never the executor's own. After-landing review never affects admission.",
        ),
        param(
            "caller_fingerprint",
            ParamType::String,
            false,
            "Type-derived pull request fingerprint negotiated with orbit.drain.probe. A mismatch is refused as protocol_skew before deserialization; optional for historical requests.",
        ),
        param(
            "review_gate",
            ParamType::Boolean,
            false,
            "Whether the executor's claimed PR leaf runs the before-PR review the `ship` contract captures. An owner with `review.timing = before_pr` refuses an executor that does not declare it as `before_pr_unsupported`. Omitted: `false`.",
        ),
        param(
            "caller_review_policy",
            ParamType::String,
            false,
            "Deprecated and ignored: the review-policy label distributed-drain protocol revisions before 5 sent. Accepted only so an older caller is refused as `protocol_mismatch` rather than for an unknown field.",
        ),
        param(
            "run_context",
            ParamType::Object,
            true,
            "The calling drain: `run_id`, `job_name`, and an optional diagnostic `machine_name`.",
        ),
        param(
            "ship",
            ParamType::Object,
            true,
            "The ship contract this owner resolves, exactly as `orbit.drain.probe` reported it. A new request carrying anything else is refused as `ship_contract_mismatch`, because the receipt freezes it.",
        ),
        param(
            "crews",
            ParamType::Object,
            false,
            "The crews the executor can run: `runnable` (its window preflight's crew names), `default_crew` (what a task naming no crew runs as there) and `excluded` (crews it cannot run, with why). A task whose crew it cannot run is skipped and stays in the backlog. Omitted: every crew is admissible.",
        ),
        param(
            "os",
            ParamType::String,
            false,
            "The executor's host OS: `linux`, `macos` or `windows`. A task tagged `os:<name>` is admitted only to an executor whose OS it names, and is otherwise skipped and stays in the backlog. Omitted: only tasks without an `os:` tag are admissible.",
        ),
        param(
            "browser_kit",
            ParamType::Boolean,
            false,
            "Whether the executor's host has a verified headless browser kit. A task tagged `ui` is admitted only to an executor that declares one, and is otherwise skipped and stays in the backlog. Omitted: `false`.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
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
        param(
            "claim_id",
            ParamType::String,
            true,
            "The claim `orbit.task.pull` admitted to this machine.",
        ),
        param(
            "run_id",
            ParamType::String,
            true,
            "The one local leaf run the executor created for this claim.",
        ),
        param(
            "ship",
            ParamType::Object,
            true,
            "The ship contract the claim was admitted under, from its receipt.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
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
        param(
            "run_id",
            ParamType::String,
            false,
            "The leaf run bound to this claim, when one was bound.",
        ),
        param(
            "settlement",
            ParamType::Object,
            true,
            "The executor's durable settlement: `{\"AcceptHandoff\": <typed handoff>}` for a published pull request, `{\"Fail\": <evidence>}`, or `{\"Release\": <evidence>}` to hand an unfinished claim back. Nothing else settles a claim.",
        ),
        param(
            "crew",
            ParamType::String,
            false,
            "Optional. The crew the leaf run resolved. Attribution only: the owner records it on the claim's settlement record, after any crew the settlement's own evidence names, and never uses it to validate or decide the settlement.",
        ),
        param(
            "crew_model",
            ParamType::String,
            false,
            "Optional. The model the leaf's crew resolved to. Attribution only, like `crew`.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    unknown_fields: UnknownFields::Warn,
    mcp_scope: None,
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
};

/// A structural tool parameter, with no CLI binding or input rewriting.
const fn param(
    name: &'static str,
    param_type: ParamType,
    required: bool,
    description: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        aliases: &[],
        param_type,
        required,
        preserve_empty: false,
        default: None,
        redact: Redaction::None,
        mcp_description: Some(Description::Static(description)),
        cli: None,
    }
}
