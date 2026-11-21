//! Rewriting pre-family tool names in stored tasks and local auto-task
//! definitions, and the once-per-root upgrade pass on open.

use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_types::task::Task;
use orbit_types::tool::is_tool_family_operation;

use crate::OrbitRuntime;
use crate::application::auto_tasks::cursor_state_path;

use super::auto_task_catalog::heal_auto_task_catalog;
use super::plugin_manifest::{run_once, run_plugin_manifest_migration_once};
use super::{
    TASK_ENVELOPE_BATCH, TOOL_NAME_MIGRATION_MARKER, TOOL_NAMES_MIGRATION_ACTOR, ToolNameRewrite,
};

fn task_needs_tool_name_migration(required: &[String], granted: &[String]) -> bool {
    required
        .iter()
        .chain(granted)
        .any(|name| is_tool_family_operation(name))
}

impl OrbitRuntime {
    /// Rewrite the pre-family tool names in this checkout's auto-task
    /// definitions under the cursor lock, and record each rewrite.
    pub(crate) fn heal_local_auto_task_tool_names(
        &self,
    ) -> Result<Vec<ToolNameRewrite>, OrbitError> {
        let local_dir = self.paths().local_dir.clone();
        if heal_auto_task_catalog(&local_dir, false)?.is_empty() {
            return Ok(Vec::new());
        }
        let state_path = cursor_state_path(&self.paths().state_dir);
        with_exclusive_file_lock(&state_path, "auto-task cursor", || {
            heal_auto_task_catalog(&local_dir, true)
        })
    }

    /// [`Self::heal_local_auto_task_tool_names`] for a read or mint path:
    /// a failure is logged, never fails the caller, which still reads old
    /// names as family actions.
    pub(crate) fn heal_local_auto_task_tool_names_best_effort(&self) {
        if let Err(error) = self.heal_local_auto_task_tool_names() {
            tracing::warn!(
                target: "orbit.core.tool_names",
                %error,
                "skipped rewriting pre-family tool names in auto-task definitions"
            );
        }
    }

    /// Rewrite `task`'s stored pre-family tool names to family actions when
    /// it holds any, recording the rewrite in its history. Returns whether
    /// the stored task changed. A host that cannot write the task (a
    /// follower, a read-only store) leaves it to the owner; admission reads
    /// the old names as family actions either way.
    pub(crate) fn heal_task_tool_names(&self, task: &Task) -> bool {
        if !task_needs_tool_name_migration(&task.required_tools, &task.granted_tools) {
            return false;
        }
        self.migrate_task_tool_names_by_id(&task.id)
    }

    fn migrate_task_tool_names_by_id(&self, id: &str) -> bool {
        if self.ensure_coordination_task_write_permitted().is_err() {
            return false;
        }
        match self
            .stores()
            .task_documents()
            .migrate_task_tool_names(id, TOOL_NAMES_MIGRATION_ACTOR)
        {
            Ok(changed) => {
                if changed {
                    tracing::info!(
                        target: "orbit.core.tool_names",
                        task_id = id,
                        "rewrote the task's stored pre-family tool names to family actions"
                    );
                }
                changed
            }
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.tool_names",
                    task_id = id,
                    %error,
                    "skipped rewriting the task's stored pre-family tool names"
                );
                false
            }
        }
    }

    /// Rewrite every task in this workspace whose stored `required_tools` or
    /// `granted_tools` names a pre-family operation. Returns how many changed.
    pub(crate) fn migrate_stored_task_tool_names(&self) -> Result<usize, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let store = self.stores().tasks();
        let ids = match store.registered_task_bundle_ids()? {
            Some(ids) => ids,
            None => store
                .list_tasks()?
                .into_iter()
                .map(|task| task.id)
                .collect(),
        };
        let mut migrated = 0;
        for batch in ids.chunks(TASK_ENVELOPE_BATCH) {
            let batch = batch.iter().cloned().collect();
            // One unreadable batch never stops the rest converging, nor
            // makes every later open rescan the store.
            let envelopes = match store.task_envelopes_for_ids(&batch) {
                Ok(envelopes) => envelopes,
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.core.tool_names",
                        %error,
                        "skipped a batch of tasks while rewriting pre-family tool names"
                    );
                    continue;
                }
            };
            for envelope in envelopes {
                if task_needs_tool_name_migration(&envelope.required_tools, &envelope.granted_tools)
                    && self.migrate_task_tool_names_by_id(&envelope.id)
                {
                    migrated += 1;
                }
            }
        }
        Ok(migrated)
    }

    /// The upgrade pass, once per root: on the first writable open after an
    /// upgrade, rewrite pre-family tool names in the global root's plugin
    /// manifests, and in this workspace's auto-task definitions and tasks.
    /// A root whose pass fails keeps no marker and retries on the next open;
    /// the failure never fails the open, since every reader still accepts the
    /// old names.
    pub(crate) fn migrate_pre_family_tool_names_on_open(&self) {
        let global_state = self.global_root().join("state");
        run_plugin_manifest_migration_once(&global_state, || {
            self.migrate_plugin_manifest_tool_names().map(|_| ())
        });
        let workspace_state = self.paths().state_dir.clone();
        run_once(
            &workspace_state,
            TOOL_NAME_MIGRATION_MARKER,
            "workspace definitions and tasks",
            || {
                self.heal_local_auto_task_tool_names()?;
                if self.ensure_coordination_task_write_permitted().is_ok() {
                    self.migrate_stored_task_tool_names()?;
                }
                Ok(())
            },
        );
    }
}
