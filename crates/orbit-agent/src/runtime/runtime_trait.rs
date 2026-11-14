use orbit_common::OrbitError;

use crate::types::{AgentInvocationSpec, AgentRequest};

pub trait AgentRuntime {
    fn invoke(&self, req: AgentRequest) -> Result<AgentInvocationSpec, OrbitError>;

    fn model_name(&self) -> Option<&str> {
        None
    }

    fn supports_continuity(&self) -> bool {
        false
    }

    fn resume_failed(&self, exit_code: Option<i32>, _stdout: &[u8], _stderr: &[u8]) -> bool {
        exit_code != Some(0)
    }
}
