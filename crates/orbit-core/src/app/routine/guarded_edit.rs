//! The one guarded write every routine definition edit runs: read the
//! file fresh, refuse a changed identity or retarget, refuse a stale
//! caller expectation, no-op when already as requested, re-parse the
//! rendered text against the expected definition, then atomically write.
//! Field-specific edits (toggle, trigger) supply only their own checks
//! and rendering, so a new edit kind cannot skip any guard.

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_types::workflow::RoutineDefinition;

use super::edit_contract::{RoutineEditOutcome, RoutineEditRefusal};
use super::loader::LoadedRoutine;

/// Run the guarded write for one loaded routine.
///
/// `check_expected` refuses (by returning the caller's stale-expectation
/// refusal) when the freshly read definition no longer matches what the
/// caller observed; `already_as_requested` reports whether the fresh
/// definition already holds the requested value; `render` turns the raw
/// text and the fresh definition into the rendered text, the full
/// definition the render must parse back to, and the `Execution` message
/// naming the fields that must not change. A refusal or an already-as-
/// requested definition writes nothing.
pub(crate) fn guarded_routine_edit(
    routine: &LoadedRoutine,
    check_expected: impl FnOnce(&RoutineDefinition) -> Result<(), RoutineEditRefusal>,
    already_as_requested: impl FnOnce(&RoutineDefinition) -> bool,
    render: impl FnOnce(
        &str,
        RoutineDefinition,
    ) -> Result<(String, RoutineDefinition, &'static str), OrbitError>,
) -> Result<Result<RoutineEditOutcome, RoutineEditRefusal>, OrbitError> {
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
        return Ok(Err(RoutineEditRefusal::TargetMismatch {
            actual_target: current.target,
        }));
    }
    if let Err(refusal) = check_expected(&current) {
        return Ok(Err(refusal));
    }
    if already_as_requested(&current) {
        return Ok(Ok(RoutineEditOutcome { changed: false }));
    }
    let (rendered, expected, changed_fields_message) = render(&raw, current)?;
    if parse_routine_yaml(&rendered)? != expected {
        return Err(OrbitError::Execution(changed_fields_message.to_string()));
    }
    atomic_write_text(&routine.path, &rendered)
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", routine.path.display())))?;
    Ok(Ok(RoutineEditOutcome { changed: true }))
}
