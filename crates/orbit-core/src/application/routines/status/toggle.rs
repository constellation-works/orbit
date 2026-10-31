use super::*;

/// Why a routine selection (toggle or trigger edit) was refused before any
/// definition was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineToggleRefusal {
    /// A replica lists the routine, but only its owner may change it.
    OwnerAuthority {
        /// Operator-facing explanation naming the owner.
        reason: String,
        /// The machine that owns the workspace's coordination store.
        owner_machine: String,
    },
    /// The report lists no schedulable routine of that name.
    NotFound,
    /// The routine comes from another source workspace than the caller's.
    WorkspaceMismatch {
        /// The routine's actual source workspace name.
        source_workspace: String,
    },
}

/// Select routine `name` from `report` for a definition edit.
///
/// The one selection every edit surface runs (toggle and trigger edit): an
/// owner-only definition is refused with the owner named, an unlisted name is
/// not found, and `source` (workspace name and `.orbit` directory, when the
/// report spans more than the caller's checkout) must match the routine's.
pub fn select_listed_routine<'a>(
    report: &'a RoutineStatusReport,
    name: &str,
    source: Option<(&str, &Path)>,
) -> Result<&'a LoadedRoutine, RoutineToggleRefusal> {
    if let Some(owned) = report
        .owner_only
        .iter()
        .find(|owned| owned.routine.definition.name == name)
    {
        return Err(RoutineToggleRefusal::OwnerAuthority {
            reason: owned.reason.clone(),
            owner_machine: owned.owner_machine.clone(),
        });
    }
    let Some(status) = report
        .statuses
        .iter()
        .find(|status| status.routine.definition.name == name)
    else {
        return Err(RoutineToggleRefusal::NotFound);
    };
    let routine = &status.routine;
    if source.is_some_and(|(workspace, orbit_dir)| {
        routine.source_workspace != workspace || routine.source_orbit_dir != orbit_dir
    }) {
        return Err(RoutineToggleRefusal::WorkspaceMismatch {
            source_workspace: routine.source_workspace.clone(),
        });
    }
    Ok(routine)
}

/// Select routine `name` from `report` and toggle its versioned `enabled`.
///
/// Selection is [`select_listed_routine`]; the caller's `target` must still
/// be the definition's before [`set_routine_enabled`] writes. The outer error
/// is a failed write.
pub fn toggle_listed_routine(
    report: &RoutineStatusReport,
    name: &str,
    source: Option<(&str, &Path)>,
    target: &str,
    expected_enabled: bool,
    enabled: bool,
) -> Result<Result<RoutineToggleOutcome, RoutineToggleRefusal>, OrbitError> {
    let routine = match select_listed_routine(report, name, source) {
        Ok(routine) => routine,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if routine.definition.target.as_ref_string() != target {
        return Ok(Ok(RoutineToggleOutcome::TargetConflict {
            actual_target: routine.definition.target.clone(),
        }));
    }
    set_routine_enabled(routine, expected_enabled, enabled).map(Ok)
}

/// The routine's next scheduled occurrence, rendered host-local.
///
/// This is the schedule coming around again, not the sweep's catch-up
/// eligibility: a routine holding a missed slot under `catch_up_once` is due
/// for that earlier slot while this still points forward. The projection comes
/// from the shared cron owner so routine status and auto-task status pin slots
/// to the minute identically. An unparseable cron projects nothing; the display
/// state reports why.
pub(crate) fn next_scheduled_occurrence(cron: &str, now: &DateTime<Local>) -> Option<String> {
    parse_cron(cron)
        .and_then(|cron| next_occurrence(&cron, now))
        .ok()
        .map(|slot| slot.to_rfc3339())
}

/// Optimistic outcome for a versioned routine-definition toggle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineToggleOutcome {
    /// The definition was atomically changed.
    Changed,
    /// The desired state already matched the current definition.
    Unchanged,
    /// The caller's expected state was stale, so no write occurred.
    Conflict { actual_enabled: bool },
    /// The definition now dispatches a different target than the one the
    /// caller selected, so no write occurred.
    TargetConflict { actual_target: RoutineTarget },
}

/// Change only the typed `enabled` field of a routine definition.
///
/// The path comes from a freshly loaded [`LoadedRoutine`], never from a
/// transport payload. That routine is also the caller's selection: a file
/// retargeted since it was loaded is refused before its `enabled` state is
/// considered. The surgical edit preserves comments and field ordering;
/// the rewritten document is parsed and compared before the atomic rename so a
/// toggle cannot accidentally alter any other routine behavior.
pub fn set_routine_enabled(
    routine: &LoadedRoutine,
    expected_enabled: bool,
    enabled: bool,
) -> Result<RoutineToggleOutcome, OrbitError> {
    let raw = std::fs::read_to_string(&routine.path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", routine.path.display())))?;
    let current = parse_routine_yaml(&raw)?;
    if current.name != routine.definition.name {
        return Err(OrbitError::InvalidInput(format!(
            "routine definition at {} changed identity from '{}' to '{}'",
            routine.path.display(),
            routine.definition.name,
            current.name
        )));
    }
    if current.target != routine.definition.target {
        return Ok(RoutineToggleOutcome::TargetConflict {
            actual_target: current.target,
        });
    }
    if current.enabled != expected_enabled {
        return Ok(RoutineToggleOutcome::Conflict {
            actual_enabled: current.enabled,
        });
    }
    if current.enabled == enabled {
        return Ok(RoutineToggleOutcome::Unchanged);
    }

    let rendered = rewrite_enabled_line(&raw, enabled)?;
    let rewritten = parse_routine_yaml(&rendered)?;
    let mut expected = current;
    expected.enabled = enabled;
    if rewritten != expected {
        return Err(OrbitError::Execution(
            "routine toggle validation changed fields other than enabled".to_string(),
        ));
    }
    atomic_write_text(&routine.path, &rendered)
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", routine.path.display())))?;
    Ok(RoutineToggleOutcome::Changed)
}

/// Rewrite only the top-level `enabled:` line of a routine document, keeping
/// every other byte — comments, ordering, trailing comment on that line —
/// intact. Managed-routine refresh uses it too, so a shipped-template change
/// never silently flips an operator's opt-in back to the template default.
pub(crate) fn rewrite_enabled_line(raw: &str, enabled: bool) -> Result<String, OrbitError> {
    let newline = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let has_enabled = raw
        .lines()
        .any(|line| line.trim_end_matches('\r').starts_with("enabled:"));
    let mut rendered = String::with_capacity(raw.len() + 16);
    let mut replaced = false;
    for line in raw.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let ending = &line[content.len()..];
        if !replaced && content.starts_with("enabled:") {
            let suffix = content
                .split_once('#')
                .map(|(_, comment)| format!(" # {}", comment.trim()))
                .unwrap_or_default();
            rendered.push_str(&format!("enabled: {enabled}{suffix}{ending}"));
            replaced = true;
        } else {
            rendered.push_str(line);
            if !has_enabled && !replaced && content.starts_with("name:") {
                if ending.is_empty() {
                    rendered.push_str(newline);
                }
                rendered.push_str(&format!("enabled: {enabled}{newline}"));
                replaced = true;
            }
        }
    }
    if !replaced {
        return Err(OrbitError::InvalidInput(
            "routine definition has no canonical top-level `name:` or `enabled:` field".to_string(),
        ));
    }
    Ok(rendered)
}

/// Pause a routine on this host (host-local, never synced). Returns `false`
/// when it was already paused.
pub fn pause_routine(global_root: &Path, name: &str, actor: &str) -> Result<bool, OrbitError> {
    let store = super::super::open_routine_store(global_root)?;
    store.routine_pause(name, actor)
}

/// Clear a host-local pause. Returns `false` when it was not paused.
pub fn resume_routine(global_root: &Path, name: &str) -> Result<bool, OrbitError> {
    let store = super::super::open_routine_store(global_root)?;
    store.routine_resume(name)
}
