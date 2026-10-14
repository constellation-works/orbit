//! Search has no guarded modes; its shared spec owns input validation.
use super::operation::GuardedModes;
use crate::ToolContext;
use orbit_common::{
    OrbitError,
    governance::{operation::OperationInput, search::SearchVerb},
};
use serde_json::Value;

impl GuardedModes for SearchVerb {
    fn normalize_input(self, input: &mut Value) {
        match self {
            Self::Query => orbit_common::governance::search::normalize_legacy_input(input),
        }
    }

    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Query => None,
        }
    }
    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            Self::Query => None,
        }
    }
    fn shape_input(self, _ctx: &ToolContext, _input: &mut Value) -> Result<(), OrbitError> {
        match self {
            Self::Query => Ok(()),
        }
    }
    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            Self::Query => None,
        }
    }
}
