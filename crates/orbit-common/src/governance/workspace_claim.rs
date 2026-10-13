//! The tool-only workspace claim operation registry.
//!
//! Names, descriptions, parameter order and registration retain the shipped
//! tool contract. These operations have no derived CLI or dashboard surface.

use crate::governance::operation::{
    Description, Effect, NounSpec, OperationSpec, ParamSpec, ParamType, Redaction,
    ToolOnlyRegistration, operation_registry,
};

operation_registry! {
    noun: WORKSPACE_CLAIM;
    /// The workspace claim verbs, joining their specs to exhaustive surface tables.
    pub enum WorkspaceClaimVerb;
    /// Operations in their original registration order.
    pub const WORKSPACE_CLAIM_OPERATIONS;
    {
        /// `orbit.workspace.claim.acquire`.
        Acquire => ACQUIRE,
        /// `orbit.workspace.claim.release`.
        Release => RELEASE,
        /// `orbit.workspace.claim.show`.
        Show => SHOW,
    }
}

/// The tool-only workspace claim noun.
pub const WORKSPACE_CLAIM: NounSpec<WorkspaceClaimVerb> = NounSpec {
    cli_name: "workspace-claim",
    tool_prefix: "orbit.workspace.claim",
    tool_only_registration: ToolOnlyRegistration::Inactive,
    cli_about: "",
    audit_target_type: "workspace_claim",
    audit_target_param: "",
    cli_groups: &[],
    operations: WORKSPACE_CLAIM_OPERATIONS,
};

const ACQUIRE: OperationSpec<WorkspaceClaimVerb> = OperationSpec {
    verb: WorkspaceClaimVerb::Acquire,
    cli_path: &[],
    tool_name: "orbit.workspace.claim.acquire",
    tool_description: "Take the exclusive, TTL-bounded workspace claim required to dispatch workflow runs while another operator holds one, and return the claim token to present on subsequent dispatches. Contention rejects with the current holder and expiry.",
    cli_about: "",
    effect: Effect::Write,
    params: &[&[
        ParamSpec {
            name: "ttl_seconds",
            aliases: &["ttlSeconds", "ttl-seconds"],
            param_type: ParamType::UnsignedInteger,
            required: false,
            preserve_empty: false,
            default: None,
            redact: Redaction::None,
            mcp_description: Some(Description::Static(
                "Optional claim TTL in seconds. Defaults to 3600; max 43200.",
            )),
            cli: None,
        },
        param(
            "machine_id",
            ParamType::String,
            false,
            "Optional machine identity, recorded for diagnostics only; the claim is keyed on the returned token.",
        ),
        param(
            "session_id",
            ParamType::String,
            false,
            "Optional session identity, recorded for diagnostics only; never load-bearing, so a reconnecting client does not orphan the workspace.",
        ),
        param(
            "agent",
            ParamType::String,
            false,
            "Deprecated compatibility field. Prefer `model` with the agent family (`codex`, `claude`, `gemini`, or `grok`).",
        ),
        param(
            "model",
            ParamType::String,
            false,
            "Preferred provenance field. Pass the canonical agent family (`codex`, `claude`, `gemini`, or `grok`); full model strings are accepted and auto-normalized.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    mcp_scope: None,
};

const RELEASE: OperationSpec<WorkspaceClaimVerb> = OperationSpec {
    verb: WorkspaceClaimVerb::Release,
    cli_path: &[],
    tool_name: "orbit.workspace.claim.release",
    tool_description: "Release the workspace claim with its token, or force-release it.",
    cli_about: "",
    effect: Effect::Write,
    params: &[&[
        param(
            "claim_token",
            ParamType::String,
            false,
            "The holder's claim token. Required unless `force` is set.",
        ),
        param(
            "force",
            ParamType::Boolean,
            false,
            "Release the claim without its token, for a holder that has gone away. Audited with who forced it and whom they displaced.",
        ),
        param(
            "agent",
            ParamType::String,
            false,
            "Deprecated compatibility field. Prefer `model` with the agent family (`codex`, `claude`, `gemini`, or `grok`).",
        ),
        param(
            "model",
            ParamType::String,
            false,
            "Preferred provenance field. Pass the canonical agent family (`codex`, `claude`, `gemini`, or `grok`); full model strings are accepted and auto-normalized.",
        ),
    ]],
    at_least_one_of: &[],
    retired_fields: &[],
    mcp_scope: None,
};

// Preserve the old tool's conservative mutating execution kind for show.
const SHOW: OperationSpec<WorkspaceClaimVerb> = OperationSpec {
    verb: WorkspaceClaimVerb::Show,
    cli_path: &[],
    tool_name: "orbit.workspace.claim.show",
    tool_description: "Report the active workspace claim — holder, expiry, and diagnostics — or that the workspace is unclaimed.",
    cli_about: "",
    effect: Effect::Write,
    params: &[&[]],
    at_least_one_of: &[],
    retired_fields: &[],
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
