//! What automation needs from the auto-task catalog [ORB-23334].
//!
//! Delivery evaluation, condition facts and explain, inspection and the
//! after-landing consumer read the catalog, validated definitions, their
//! discovery paths, accepted-result history, open-instance dedupe and the
//! template-to-task mapping. `auto_tasks` owns all of them, and its scheduler
//! evaluates delivery and conditions through automation, so a direct call from
//! here would close a cycle. This port is the way in: `auto_tasks` implements
//! it on [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime binds
//! it.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_types::workflow::{AutoTaskAcceptedHistory, AutoTaskDefinition};

use crate::application::task::TaskAddParams;

/// Auto-task catalog, history, dedupe and mint reads automation code calls,
/// implemented by `auto_tasks`.
pub(in crate::application) trait AutomationAutoTaskPort {
    /// Every auto-task definition a list surface shows by default, in load
    /// order. One whose seeding plugin is off in this workspace, or on the
    /// host, is omitted.
    fn listed_auto_task_definitions(&self) -> Result<Vec<AutoTaskDefinition>, OrbitError>;

    /// One effective, validated definition by name, preferring the
    /// checkout-local file over the shared workspace catalog. `None` means
    /// neither has the name.
    fn validated_auto_task_definition(
        &self,
        name: &str,
    ) -> Result<Option<AutoTaskDefinition>, OrbitError>;

    /// The actual discovery path supplying `name`, across the same read roots
    /// as [`Self::validated_auto_task_definition`]. Invalid or ambiguous local
    /// entries refuse lookup.
    fn auto_task_discovery_path(&self, name: &str) -> Result<Option<PathBuf>, OrbitError>;

    /// The latest result `name`'s declared completion contract accepted. No
    /// accepted result and an unreadable history are different answers.
    fn auto_task_completion_history(&self, name: &str) -> AutoTaskAcceptedHistory;

    /// The id of the first still-open instance of `definition`'s prior
    /// mints, in task-list order, if any: the `skip_if_open` dedupe rule.
    fn open_auto_task_instance_id(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<Option<String>, OrbitError>;

    /// The task parameters `definition`'s template maps to, with its
    /// `auto-task:<name>` provenance tag and no plugin provenance.
    fn auto_task_template_task_params(&self, definition: &AutoTaskDefinition) -> TaskAddParams;

    /// The template's task parameters plus the seeding plugin's `plugin:<ns>`
    /// provenance tag, as a scheduled mint carries them.
    fn auto_task_mint_task_params(&self, definition: &AutoTaskDefinition) -> TaskAddParams;
}
