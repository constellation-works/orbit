//! Native read surfaces for task pilots, independent of MCP grants.

use orbit_types::workflow::activity_job::{Provider, requires_native_inspection};
use serde_json::Value;

use super::super::super::dispatcher::DispatchError;

/// The one instruction every provider gets for a native inspection run:
/// each agent CLI ships its own file-read and shell tools, separate from
/// the Orbit MCP tool list. Read-only stays enforced by the `reviewer`
/// filesystem profile, and the apply step's observed-revision proof — not
/// the provider name — establishes that the run read the pinned source.
const GENERIC_INSTRUCTION: &str = "Provider inspection surface: use this CLI's own native file-read and shell/command tools to read files and run read-only git/rg commands in the checkout. These native tools are separate from the Orbit MCP tool list.";

/// Refuse an inspection that cannot start before executor lookup: a profile
/// other than `reviewer`, or a provider with no CLI runtime. A null revision
/// still denotes a non-Git inspection.
pub(in crate::activity_job::cli_runner) fn admit_inspection(
    provider: &str,
    input: &Value,
    fs_profile: Option<&str>,
) -> Result<(), DispatchError> {
    if !requires_native_inspection(input) {
        return Ok(());
    }
    if fs_profile != Some("reviewer") {
        return Err(unavailable(
            provider,
            "native inspection requires the reviewer filesystem profile",
        ));
    }
    if !Provider::parse(provider).is_ok_and(Provider::has_cli_runtime) {
        return Err(unavailable(
            provider,
            "this provider has no CLI runtime to inspect with; configure a crew whose provider has one",
        ));
    }
    Ok(())
}

/// The native tools an admitted inspection run starts with: the arguments
/// its executor declares (`inspection_args`), before allocating a checkout
/// or starting a session. An executor that declares none is refused here,
/// so a crew that cannot read the pinned source never launches only to fail
/// its observed-revision proof.
pub(in crate::activity_job::cli_runner) fn prepare_inspection_tools(
    provider: &str,
    input: &Value,
    inspection_args: Option<&[String]>,
) -> Result<Option<InspectionTools>, DispatchError> {
    // These are the required task-pilot input fields. Other read-only
    // inspection callers retain their existing provider/tool contracts.
    if !requires_native_inspection(input) {
        return Ok(None);
    }
    let args = inspection_args.ok_or_else(|| {
        unavailable(
            provider,
            "its executor declares no `inspection_args`; add them to the executor definition, \
             once its native file-read and shell tools are known to work unattended, or draw \
             another crew",
        )
    })?;
    Ok(Some(InspectionTools {
        args: args.to_vec(),
        instruction: GENERIC_INSTRUCTION,
    }))
}

fn unavailable(provider: &str, reason: &str) -> DispatchError {
    DispatchError::InspectionToolsUnavailable {
        provider: provider.to_string(),
        reason: reason.to_string(),
    }
}

pub(in crate::activity_job::cli_runner) struct InspectionTools {
    pub(in crate::activity_job::cli_runner) args: Vec<String>,
    pub(in crate::activity_job::cli_runner) instruction: &'static str,
}
