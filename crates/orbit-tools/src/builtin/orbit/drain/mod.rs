//! The drain noun's MCP guarded-mode table.
//!
//! The generic operation tool serves every verb from the shared registry.

use orbit_common::OrbitError;
use orbit_common::governance::drain::DrainVerb;
use orbit_common::governance::operation::OperationInput;
use serde_json::Value;

use super::operation::GuardedModes;
use crate::ToolContext;

impl GuardedModes for DrainVerb {
    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
        match self {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::Claims => None,
        }
    }

    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::Claims => None,
        }
    }

    fn shape_input(self, _ctx: &ToolContext, _input: &mut Value) -> Result<(), OrbitError> {
        match self {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::Claims => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::Claims => None,
        }
    }
}
