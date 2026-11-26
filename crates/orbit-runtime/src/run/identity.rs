//! Run-layer execution identity on [`RuntimeKernel`].

use std::sync::Arc;

use crate::RuntimeKernel;

impl RuntimeKernel {
    /// Registry-owning composition supplies stable machine identity; Core never discovers it.
    pub fn with_automation_machine_identity(self, machine_id: Option<String>) -> Self {
        self.with_automation_execution_location(machine_id.map(|machine_id| {
            orbit_types::task::ExecutionLocation {
                machine_id,
                machine_name: None,
            }
        }))
    }

    /// Supply the executing machine's stable identity and configured display name.
    pub fn with_automation_execution_location(
        mut self,
        location: Option<orbit_types::task::ExecutionLocation>,
    ) -> Self {
        self.stored_mut().set_execution_location(location.clone());
        self.run_mut().automation_execution_location = location.map(Arc::new);
        self
    }

    pub fn automation_machine_identity(&self) -> Option<&str> {
        self.automation_execution_location()
            .map(|location| location.machine_id.as_str())
    }

    pub fn automation_execution_location(&self) -> Option<&orbit_types::task::ExecutionLocation> {
        self.run().automation_execution_location.as_deref()
    }
}
