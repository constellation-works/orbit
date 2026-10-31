//! Experiments have no guarded modes; their shared spec owns input
//! validation, and the capability chokepoint refuses agent writes.
use super::operation::GuardedModes;
use crate::ToolContext;
use orbit_common::{
    OrbitError,
    governance::{experiment::ExperimentVerb, operation::OperationInput},
};
use serde_json::Value;

impl GuardedModes for ExperimentVerb {
    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Create
            | Self::Update
            | Self::Clone
            | Self::Start
            | Self::Pause
            | Self::Resume
            | Self::Assign
            | Self::Reweight
            | Self::Conclude
            | Self::Abandon
            | Self::Show
            | Self::List
            | Self::Readout => None,
        }
    }

    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Create
            | Self::Update
            | Self::Clone
            | Self::Start
            | Self::Pause
            | Self::Resume
            | Self::Assign
            | Self::Reweight
            | Self::Conclude
            | Self::Abandon
            | Self::Show
            | Self::List
            | Self::Readout => None,
        }
    }

    fn shape_input(self, _ctx: &ToolContext, _input: &mut Value) -> Result<(), OrbitError> {
        match self {
            Self::Create
            | Self::Update
            | Self::Clone
            | Self::Start
            | Self::Pause
            | Self::Resume
            | Self::Assign
            | Self::Reweight
            | Self::Conclude
            | Self::Abandon
            | Self::Show
            | Self::List
            | Self::Readout => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            Self::Create
            | Self::Update
            | Self::Clone
            | Self::Start
            | Self::Pause
            | Self::Resume
            | Self::Assign
            | Self::Reweight
            | Self::Conclude
            | Self::Abandon
            | Self::Show
            | Self::List
            | Self::Readout => None,
        }
    }
}
