//! Fleet notices have no guarded modes; their shared spec owns input
//! validation and core derives the poster class from the session. Post and
//! close take the session's configured orchestrator as their attribution
//! default, like task creation.
use super::apply_session_orchestrator_default;
use super::operation::GuardedModes;
use crate::ToolContext;
use orbit_common::{
    OrbitError,
    governance::{notice::NoticeVerb, operation::OperationInput},
};
use serde_json::Value;

impl GuardedModes for NoticeVerb {
    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Post | Self::Close | Self::List | Self::Show => None,
        }
    }

    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Post | Self::Close | Self::List | Self::Show => None,
        }
    }

    fn shape_input(self, ctx: &ToolContext, input: &mut Value) -> Result<(), OrbitError> {
        match self {
            Self::Post | Self::Close => {
                apply_session_orchestrator_default(ctx, input);
                Ok(())
            }
            Self::List | Self::Show => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            Self::Post | Self::Close | Self::List | Self::Show => None,
        }
    }
}
