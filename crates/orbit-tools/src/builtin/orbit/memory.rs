//! Crew memory has no guarded modes: its shared spec owns input validation,
//! core owns the write rules, and the capability chokepoint refuses agent
//! writes.
use super::operation::GuardedModes;
use crate::ToolContext;
use orbit_common::{
    OrbitError,
    governance::{memory::MemoryVerb, operation::OperationInput},
};
use serde_json::Value;

impl GuardedModes for MemoryVerb {
    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Show | Self::History | Self::Export | Self::Write | Self::Reset | Self::Copy => {
                None
            }
        }
    }

    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Show | Self::History | Self::Export | Self::Write | Self::Reset | Self::Copy => {
                None
            }
        }
    }

    fn shape_input(self, _ctx: &ToolContext, _input: &mut Value) -> Result<(), OrbitError> {
        match self {
            Self::Show | Self::History | Self::Export | Self::Write | Self::Reset | Self::Copy => {
                Ok(())
            }
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            Self::Show | Self::History | Self::Export | Self::Write | Self::Reset | Self::Copy => {
                None
            }
        }
    }
}
