use orbit_types::record::OrbitEvent;

use crate::{OrbitError, OrbitRuntime};

impl OrbitRuntime {
    /// `pub` for the direct v2 activity runner in `orbit-cmd` [ORB-10016],
    /// which records the standard activity-run lifecycle events.
    pub fn record_event(&self, event: OrbitEvent) -> Result<(), OrbitError> {
        self.event_log.append(event);
        Ok(())
    }

    pub fn with_mutation<F, T>(&self, f: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<(T, OrbitEvent), OrbitError>,
    {
        self.with_optional_mutation_event(|| f().map(|(result, event)| (result, Some(event))))
    }

    /// Publish a mutation event only when the operation changed state.
    pub(crate) fn with_optional_mutation_event<F, T>(&self, f: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<(T, Option<OrbitEvent>), OrbitError>,
    {
        let (result, event) = f()?;
        if let Some(event) = event {
            self.event_log.append(event);
        }
        Ok(result)
    }
}
