use super::*;

/// Status event recorded when `orbit task recheck-blocked --confirm` returns a
/// task blocked by a missing provider launcher to backlog because the launcher
/// now resolves.
pub(crate) const INFRA_BLOCK_CLEARED_EVENT: &str = "infra_block_cleared";

impl OrbitRuntime {
    /// Return every task blocked by a missing provider launcher that now
    /// resolves to `backlog`, recording which launcher cleared which block.
    /// Tasks whose launcher is still missing, and every task blocked for any
    /// other reason, are left alone.
    ///
    /// Each task is re-classified under its write lock and written with a
    /// `blocked` compare-and-set, so an operator decision or a newer block
    /// that lands after the scan wins over this requeue.
    pub fn requeue_cleared_infra_blocked_tasks(&self) -> Result<Vec<InfraBlockedTask>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let mut requeued = Vec::new();
        for scanned in self.infra_blocked_tasks()? {
            if scanned.launcher.is_none() {
                continue;
            }
            let mut cleared = None;
            self.stores()
                .tasks()
                .with_task_write_lock(&scanned.task_id, &mut || {
                    let task = self.get_task(&scanned.task_id)?;
                    let Some(current) = self.infra_block_of(&task)? else {
                        return Ok(());
                    };
                    let Some(launcher) = current.launcher.as_ref() else {
                        return Ok(());
                    };
                    if current.blocked_at != scanned.blocked_at {
                        return Ok(());
                    }
                    let note = format!(
                        "infra block cleared by `orbit task recheck-blocked`: provider launcher \
                         `{}` for provider `{}` now resolves at {}; the block recorded at {} \
                         (run_id={}) no longer reproduces",
                        current.program,
                        current.provider,
                        launcher.display(),
                        current.blocked_at.to_rfc3339(),
                        current.run_id.as_deref().unwrap_or("-"),
                    );
                    self.with_mutation(|| {
                        let task = self.stores().task_records().update(
                            &current.task_id,
                            StoreTaskUpdateParams {
                                actor: SYSTEM_ACTOR_LABEL.to_string(),
                                status_event: Some(INFRA_BLOCK_CLEARED_EVENT.to_string()),
                                status_note: Some(note.clone()),
                                expected_status: Some(vec![TaskStatus::Blocked]),
                                ..StoreTaskUpdateParams::from(TaskUpdateParams {
                                    status: Some(TaskStatus::Backlog),
                                    ..Default::default()
                                })
                            },
                        )?;
                        Ok((
                            task,
                            OrbitEvent::TaskUpdated {
                                id: current.task_id.clone(),
                            },
                        ))
                    })?;
                    cleared = Some(current);
                    Ok(())
                })?;
            requeued.extend(cleared);
        }
        Ok(requeued)
    }
}
