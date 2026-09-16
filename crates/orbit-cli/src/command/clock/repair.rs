//! `orbit clock repair` — point the installed clock unit at this binary.

use std::path::Path;

use orbit_core::application::routines::{
    ClockUnitConvergence, ClockUnitDrift, converge_clock_unit,
};
use serde_json::json;

use crate::command::{CommandOut, Payload};

pub(crate) fn execute(global_root: &Path) -> CommandOut {
    let convergence = converge_clock_unit(global_root)?;
    let mut text = convergence.summary();
    for step in convergence.manual_steps() {
        text.push_str(&format!("\n  run: {step}"));
    }
    // A rewritten unit the manager would not re-register still does not tick,
    // so the caller — `orbit update` convergence included — has to see this as
    // unfinished work rather than a clean repair. Re-running the command after
    // that retries the registration rather than reporting the unit current.
    let exit_code = i32::from(convergence.needs_follow_up());
    Ok(Payload::detail(document(&convergence), text)
        .with_exit_code(exit_code)
        .into())
}

fn document(convergence: &ClockUnitConvergence) -> serde_json::Value {
    match convergence {
        ClockUnitConvergence::NoUnitInstalled => json!({
            "action": "no_unit_installed",
            "summary": convergence.summary(),
        }),
        ClockUnitConvergence::AlreadyCurrent { unit_path, program } => json!({
            "action": "already_current",
            "summary": convergence.summary(),
            "unit_path": display(unit_path),
            "program": display(program),
        }),
        ClockUnitConvergence::Rewritten(rewrite) => json!({
            "action": "rewritten",
            "summary": convergence.summary(),
            "unit_path": display(&rewrite.unit_path),
            "program": display(&rewrite.program),
            "previous_program": rewrite.drift.previous_program().map(display),
            "drift": drift_name(&rewrite.drift),
            "files_written": rewrite.files_written.iter().map(|path| display(path)).collect::<Vec<_>>(),
            "reactivated": rewrite.reactivated,
            "manual_steps": rewrite.manual_steps,
        }),
        ClockUnitConvergence::Reloaded(reload) => json!({
            "action": "reloaded",
            "summary": convergence.summary(),
            "unit_path": display(&reload.unit_path),
            "program": display(&reload.program),
            "reactivated": reload.reactivated,
            "manual_steps": reload.manual_steps,
        }),
    }
}

fn drift_name(drift: &ClockUnitDrift) -> &'static str {
    match drift {
        ClockUnitDrift::ProgramUnreadable => "program_unreadable",
        ClockUnitDrift::ProgramMissing { .. } => "program_missing",
        ClockUnitDrift::ProgramMoved { .. } => "program_moved",
        ClockUnitDrift::InvocationStale => "invocation_stale",
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}
