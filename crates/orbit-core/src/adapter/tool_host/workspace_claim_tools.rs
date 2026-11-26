//! Handler table for the workspace-claim operation registry: the tool call
//! becomes the workspace app's command and calls its one `exec`.

use orbit_common::OrbitError;
use orbit_common::governance::workspace_claim::WorkspaceClaimVerb;
use serde_json::Value;

use crate::Core;
use crate::app::workspace::{Command, Outcome, exec};

pub(super) fn dispatch(
    core: &Core,
    verb: WorkspaceClaimVerb,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let Outcome::Answer(answer) = exec(
        core,
        Command::Claim {
            verb,
            input,
            agent,
            model,
        },
    )?
    else {
        unreachable!("workspace claim returned an unrelated outcome");
    };
    Ok(answer)
}
