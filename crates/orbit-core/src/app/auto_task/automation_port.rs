//! Auto-task's side of the automation-owned auto-task port [ORB-23334].

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_types::workflow::{AutoTaskAcceptedHistory, AutoTaskDefinition};

use crate::OrbitRuntime;
use crate::application::automation::AutomationAutoTaskPort;
use crate::application::task::TaskAddParams;

pub(crate) struct AutomationPort<'a>(pub(crate) &'a OrbitRuntime);

/// The catalog reads automation makes, forwarded to this module's policy and
/// discovery [ORB-23334]: `auto_task` sits above `automation`.
impl AutomationAutoTaskPort for AutomationPort<'_> {
    fn listed_auto_task_definitions(&self) -> Result<Vec<AutoTaskDefinition>, OrbitError> {
        Ok(super::scheduler::auto_task_listing(self.0, false)?
            .into_iter()
            .map(|listed| listed.definition)
            .collect())
    }

    fn validated_auto_task_definition(
        &self,
        name: &str,
    ) -> Result<Option<AutoTaskDefinition>, OrbitError> {
        super::crud::auto_task_show(self.0, name)
    }

    fn auto_task_discovery_path(&self, name: &str) -> Result<Option<PathBuf>, OrbitError> {
        Ok(super::read::auto_task_read_source(self.0, name)?.map(|(_, path)| path))
    }

    fn auto_task_completion_history(&self, name: &str) -> AutoTaskAcceptedHistory {
        super::scheduler::auto_task_accepted_history(self.0, name)
    }

    fn open_auto_task_instance_id(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<Option<String>, OrbitError> {
        super::scheduler::open_auto_task_instance(self.0, definition)
    }

    fn auto_task_template_task_params(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<TaskAddParams, OrbitError> {
        super::scheduler::template_params(self.0, definition)
    }

    fn auto_task_mint_task_params(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<TaskAddParams, OrbitError> {
        super::scheduler::mint_params(self.0, definition)
    }

    fn auto_task_stale_contract_hold(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<(), OrbitError> {
        super::setting::refuse_stale_auto_task_contract(self.0, definition)
    }
}
