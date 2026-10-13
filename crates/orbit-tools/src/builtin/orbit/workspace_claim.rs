//! The workspace claim noun's MCP guarded-mode table.
//!
//! The generic operation tool serves every verb from the shared registry.

use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationInput;
use orbit_common::governance::workspace_claim::WorkspaceClaimVerb;
use serde_json::Value;

use super::operation::GuardedModes;
use crate::ToolContext;

impl GuardedModes for WorkspaceClaimVerb {
    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => None,
        }
    }

    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => None,
        }
    }

    fn shape_input(self, _ctx: &ToolContext, _input: &mut Value) -> Result<(), OrbitError> {
        match self {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => None,
        }
    }
}
