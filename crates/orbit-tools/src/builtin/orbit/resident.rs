//! Resident mandates have no guarded modes; their shared spec owns input
//! validation and core decides from the session whether a widening applies
//! or becomes a decision request. Grant takes the session's configured
//! orchestrator as its relay attribution default, like notices.
use super::apply_session_orchestrator_default;
use super::operation::GuardedModes;
use crate::ToolContext;
use orbit_common::{
    OrbitError,
    governance::{operation::OperationInput, resident::ResidentVerb},
};
use serde_json::Value;

impl GuardedModes for ResidentVerb {
    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Grant
            | Self::Checkpoint
            | Self::Show
            | Self::List
            | Self::Metrics
            | Self::Digest => None,
        }
    }

    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Grant
            | Self::Checkpoint
            | Self::Show
            | Self::List
            | Self::Metrics
            | Self::Digest => None,
        }
    }

    fn shape_input(self, ctx: &ToolContext, input: &mut Value) -> Result<(), OrbitError> {
        match self {
            Self::Grant => {
                apply_session_orchestrator_default(ctx, input);
                Ok(())
            }
            Self::Checkpoint | Self::Show | Self::List | Self::Metrics | Self::Digest => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            Self::Grant
            | Self::Checkpoint
            | Self::Show
            | Self::List
            | Self::Metrics
            | Self::Digest => None,
        }
    }
}
