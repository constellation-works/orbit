//! The orchestrator scorecard has no guarded modes; its shared spec owns input validation.
use super::operation::GuardedModes;
use crate::ToolContext;
use orbit_common::{
    OrbitError,
    governance::{operation::OperationInput, orchestrator::OrchestratorVerb},
};
use serde_json::Value;

impl GuardedModes for OrchestratorVerb {
    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Scorecard | Self::Show => None,
        }
    }

    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Scorecard | Self::Show => None,
        }
    }

    fn shape_input(self, _ctx: &ToolContext, _input: &mut Value) -> Result<(), OrbitError> {
        match self {
            Self::Scorecard | Self::Show => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            Self::Scorecard | Self::Show => None,
        }
    }
}
