//! Environment checks: `execution.env.pass` variables runs do not receive,
//! judged from this process plus `~/.orbit/clock.env`, and whether required
//! validation resolves the same toolchain as agents.

use std::collections::BTreeSet;

use orbit_common::security::operator_env::clock_env_file_names;

use crate::doctor::*;

/// `execution.env.pass` variables runs do not receive [ORB-14777, ORB-25291].
///
/// Agents inherit the launching environment through the allowlist, so a
/// pass-listed variable missing there (a dedicated worker token, say) silently
/// leaves agents on another login. Clock-started runs additionally read
/// `~/.orbit/clock.env`, so a name that file supplies counts as held here even
/// when this evaluating process (a service or MCP process for `orbit
/// attention`, say, which never loads `clock.env`) does not hold it. A
/// warning, never an error: optional provider keys can legitimately be absent.
/// Names only, never values.
///
/// A crew whose `[crews.<name>] env` reads an unset source is reported in the
/// same row: drains exclude that crew and its activities are refused.
pub(in crate::doctor) fn doctor_check_env_pass(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let unset = runtime.unset_env_pass_names();
    // Judge the environment runs actually receive: names `clock.env` supplies
    // count as held. Names only; values never leave the file.
    let clock_held = clock_held_pass_names(runtime, &unset);
    let effective_unset: Vec<String> = unset
        .into_iter()
        .filter(|name| !clock_held.contains(name))
        .collect();
    // A `file:` source is reported below by the file's own state, so only
    // variable sources are asked about this environment.
    let crew_unset: Vec<_> = runtime
        .unset_crew_env()
        .into_iter()
        .filter_map(|mut unset| {
            unset.env.retain(|entry| entry.source.variable().is_some());
            (!unset.env.is_empty()).then_some(unset)
        })
        .collect();
    let crews = runtime.configured_crew_env_sources();
    let service_warnings =
        crate::service_environment::crew_source_environment_warnings(runtime, &crews);
    if effective_unset.is_empty() && crew_unset.is_empty() && service_warnings.is_empty() {
        return check(
            "env-pass",
            WorkspaceDoctorStatus::Ok,
            "every operator-added `execution.env.pass` variable and crew `env` source is set in \
             this process's environment or supplied by ~/.orbit/clock.env for clock-started runs \
             (this row judges the evaluating process plus clock.env, not the drain launcher alone)"
                .to_string(),
        );
    }
    let mut problems = service_warnings;
    if !effective_unset.is_empty() {
        problems.push(format!(
            "`execution.env.pass` names variables unset or empty in this process's environment \
             and not supplied by ~/.orbit/clock.env: {}; agents started from a process without \
             them will not receive them (clock-started runs additionally read ~/.orbit/clock.env, \
             whose names count as held here)",
            effective_unset.join(", ")
        ));
    }
    if !crew_unset.is_empty() {
        problems.push(format!(
            "{} unset or empty in this environment; drains exclude {} and {} activities are \
             refused",
            crew_unset
                .iter()
                .map(orbit_types::workflow::CrewEnvUnset::describe)
                .collect::<Vec<_>>()
                .join("; "),
            if crew_unset.len() == 1 {
                "that crew"
            } else {
                "those crews"
            },
            if crew_unset.len() == 1 {
                "its"
            } else {
                "their"
            },
        ));
    }
    actionable_check(
        "env-pass",
        WorkspaceDoctorStatus::Warning,
        problems.join("; "),
        "This row judges the evaluating process's environment plus ~/.orbit/clock.env (names \
         there count as held for clock-started runs; values are never read). Export the missing \
         variables before starting the drain or service (clock-started runs also read them from \
         ~/.orbit/clock.env). Drains inherit their launching process's environment: use a login \
         shell for CLI starts, or set the variables in the service environment and restart it, \
         or remove the names from `execution.env.pass` or the crew's `env`, then rerun `orbit \
         doctor`."
            .to_string(),
    )
}

/// Names in `names` that `~/.orbit/clock.env` supplies a non-empty value for.
/// Names only, never values; a missing, empty or refused file counts as holding
/// nothing here (refusals surface through the clock rows).
fn clock_held_pass_names(runtime: &OrbitRuntime, names: &[String]) -> BTreeSet<String> {
    if names.is_empty() {
        return BTreeSet::new();
    }
    clock_env_file_names(&runtime.global_root(), names)
        .ok()
        .flatten()
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// Where required validation finds the user's toolchain [ORB-13987].
///
/// Required commands run with PATH and toolchain locators resolved from the
/// owner's login shell, independent of whatever launches the worker. This row
/// resolves that environment the way a delivery run would and warns when the
/// login shell cannot be probed (validation then falls back to the launcher's
/// PATH), when resolution is disabled without a configured PATH, or when the
/// resolved PATH drops login-shell entries. It also reports the successful
/// probe mode, interactive fallback reason and tool locations, warning when a
/// later PATH entry contains a different executable shadowed by an earlier one,
/// or when an agent session would resolve `python3`, `git`, `make` or `cargo`
/// to a different executable than required validation.
pub(in crate::doctor) fn doctor_check_validation_env(
    runtime: &OrbitRuntime,
) -> WorkspaceDoctorResult {
    if runtime.workflow_required_validation_commands().is_empty() {
        return check(
            "validation-env",
            WorkspaceDoctorStatus::Skipped,
            "no `workflow.required_validation_commands`; required validation does not run here"
                .to_string(),
        );
    }
    let environment = runtime.validation_environment();
    let mut details = vec![format!(
        "required validation PATH comes from {}{}; probe mode: {}; PATH={}",
        environment.source.as_str(),
        environment
            .login_shell
            .as_ref()
            .map(|shell| format!(" ({})", shell.display()))
            .unwrap_or_default(),
        environment.probe_mode.map_or(
            if environment.login_shell_enabled {
                "failed"
            } else {
                "disabled"
            },
            |mode| mode.as_str()
        ),
        environment.path().unwrap_or("<unset>")
    )];
    let mut warning = false;
    if let Some(reason) = &environment.fallback_reason {
        details.push(format!("interactive fallback reason: {reason}"));
        warning = true;
    }
    if let Some(reason) = environment.preflight_warning() {
        details.push(reason);
        warning = true;
    }
    for tool in ["python3", "git", "make"] {
        let paths = environment.program_paths(tool);
        if let Some((first, later)) = paths.split_first() {
            details.push(format!("{tool} resolves to {}", first.display()));
            if !later.is_empty() {
                details.push(format!(
                    "{tool}: {} shadows later PATH executables: {}",
                    first.display(),
                    later
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                warning = true;
            }
        } else {
            details.push(format!("{tool} is absent from PATH"));
        }
    }
    // Implementer and reviewer sessions start from one environment. A tool it
    // resolves differently from required validation is what made a reviewer
    // hand-build a PATH wrapper before its gates would pass [ORB-15293].
    let mut session = environment.clone();
    session.env = runtime.agent_session_environment(&[]);
    for tool in ["python3", "git", "make", "cargo"] {
        let validation = environment.program_paths(tool).into_iter().next();
        let agent = session.program_paths(tool).into_iter().next();
        if agent != validation {
            let shown = |path: Option<std::path::PathBuf>| {
                path.map_or_else(|| "nothing".to_string(), |path| path.display().to_string())
            };
            details.push(format!(
                "{tool}: agent sessions (implement and review lanes) resolve {}, required \
                 validation resolves {}",
                shown(agent),
                shown(validation)
            ));
            warning = true;
        }
    }
    let message = details.join("; ");
    if warning {
        actionable_check(
            "validation-env",
            WorkspaceDoctorStatus::Warning,
            message,
            "Make the interactive login shell export the intended toolchain PATH (`$SHELL \
             -i -l -c 'echo $PATH'` should list it first), fix rc failures, or set \
             `workflow.validation_env.path` with `orbit config set`, then rerun \
             `orbit doctor`."
                .to_string(),
        )
    } else {
        check("validation-env", WorkspaceDoctorStatus::Ok, message)
    }
}
