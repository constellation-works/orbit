//! Decision requests: listing and showing are plain reads. Answering is
//! refused at the tool entry — the operation is registered inactive, and
//! this preempt keeps that refusal explicit if it is ever reached: an
//! agent may see that a human is being asked; it may not answer.
//! Submitting through a registered channel passes to the host, where core
//! binds the caller to the registration and verifies the token; the
//! channel-administration verbs are operator CLI only and refused here
//! like answering.
use super::operation::GuardedModes;
use crate::ToolContext;
use orbit_common::{
    OrbitError,
    governance::{decision::DecisionVerb, operation::OperationInput},
};
use serde_json::Value;

impl GuardedModes for DecisionVerb {
    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::List | Self::Show | Self::Submit => None,
            Self::Answer => Some(Err(OrbitError::PolicyDenied(
                "answering a decision request is the operator's: use `orbit decision answer` or the dashboard".into(),
            ))),
            Self::ChannelRegister
            | Self::ChannelList
            | Self::ChannelRevoke
            | Self::ChannelRotateKey => Some(Err(OrbitError::PolicyDenied(
                "decision channels are registered from the operator CLI: use `orbit decision channel register|list|revoke|rotate-key`".into(),
            ))),
        }
    }

    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::List
            | Self::Show
            | Self::Answer
            | Self::Submit
            | Self::ChannelRegister
            | Self::ChannelList
            | Self::ChannelRevoke
            | Self::ChannelRotateKey => None,
        }
    }

    fn shape_input(self, _ctx: &ToolContext, _input: &mut Value) -> Result<(), OrbitError> {
        match self {
            Self::List
            | Self::Show
            | Self::Answer
            | Self::Submit
            | Self::ChannelRegister
            | Self::ChannelList
            | Self::ChannelRevoke
            | Self::ChannelRotateKey => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            Self::List
            | Self::Show
            | Self::Answer
            | Self::Submit
            | Self::ChannelRegister
            | Self::ChannelList
            | Self::ChannelRevoke
            | Self::ChannelRotateKey => None,
        }
    }
}
