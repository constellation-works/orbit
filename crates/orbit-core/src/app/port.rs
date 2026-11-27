//! Bind ports owned by lower modules to the apps that supply their answers.

pub(crate) use super::routine::ResidentRoutineStatus;
use crate::application::resident::ResidentRoutineStatusPort;

impl ResidentRoutineStatusPort for ResidentRoutineStatus<'_> {
    fn disabled_resident_cycle_routine_name(&self) -> Option<String> {
        self.disabled_cycle_routine_name()
    }
}
