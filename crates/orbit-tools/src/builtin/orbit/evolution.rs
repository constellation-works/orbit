use super::operation::GuardedModes;
use crate::ToolContext;
use orbit_common::{
    OrbitError,
    governance::{evolution::EvolutionVerb, operation::OperationInput},
};
use serde_json::Value;

impl GuardedModes for EvolutionVerb {
    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Add | Self::Decline | Self::Show | Self::List => None,
        }
    }

    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Add | Self::Decline | Self::Show | Self::List => None,
        }
    }

    fn shape_input(self, ctx: &ToolContext, input: &mut Value) -> Result<(), OrbitError> {
        let _ = (ctx, input);
        Ok(())
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            Self::Add | Self::Decline | Self::Show | Self::List => None,
        }
    }
}
