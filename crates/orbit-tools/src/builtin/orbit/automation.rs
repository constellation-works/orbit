//! The automation verbs have no guarded modes; their shared specs own input validation.
use super::operation::GuardedModes;
use crate::ToolContext;
use orbit_common::{
    OrbitError,
    governance::{automation::AutomationVerb, operation::OperationInput},
};
use serde_json::Value;

impl GuardedModes for AutomationVerb {
    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Explain | Self::ClearStall => None,
        }
    }

    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Explain | Self::ClearStall => None,
        }
    }

    fn shape_input(self, _ctx: &ToolContext, _input: &mut Value) -> Result<(), OrbitError> {
        match self {
            Self::Explain | Self::ClearStall => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            Self::Explain | Self::ClearStall => None,
        }
    }
}
