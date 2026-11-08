//! Experiment's side of the tag-instruction overlay port [ORB-23019].

use orbit_types::workflow::ExperimentActivityOverlay;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::tag_instructions::{TagInstructionExperimentPort, TagInstructionFailure};

impl TagInstructionExperimentPort for OrbitRuntime {
    fn tag_instruction_experiment_overlays(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Result<Vec<ExperimentActivityOverlay>, TagInstructionFailure> {
        // Selection has no dispatch model. Crew-memory delivery stays on the
        // adapter's direct `experiment_overlays_for_activity` call, which
        // passes one; this forward keeps the `None` the selection call used.
        self.experiment_overlays_for_activity(run_id, activity_name, input, None)
    }
}
