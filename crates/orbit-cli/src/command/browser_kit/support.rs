use orbit_cmd::browser_kit::{KitProblem, KitStatus, VerifyReport};
use orbit_core::OrbitError;
use orbit_core::bootstrap::browser_kit::PLAYWRIGHT_VERSION;

pub(super) fn to_json(value: &impl serde::Serialize) -> Result<serde_json::Value, OrbitError> {
    serde_json::to_value(value)
        .map_err(|error| OrbitError::Execution(format!("encode browser kit report: {error}")))
}

/// The status record plus every problem with its fix.
pub(super) fn status_json(status: &KitStatus) -> Result<serde_json::Value, OrbitError> {
    let mut value = to_json(status)?;
    if let Some(record) = value.as_object_mut() {
        record.insert("problems".to_string(), to_json(&all_problems(status))?);
    }
    Ok(value)
}

pub(super) fn all_problems(status: &KitStatus) -> Vec<KitProblem> {
    status
        .problems()
        .into_iter()
        .chain(status.library_problem())
        .collect()
}

pub(super) fn status_text(status: &KitStatus) -> String {
    let kit = &status.inspection;
    let mut lines = vec![format!("browser kit: {}", status.state.replace('_', " "))];
    lines.push(format!("  location:   {}", kit.root.display()));
    if !kit.present {
        lines.push(format!(
            "  next:       orbit config browser-kit install (Playwright {PLAYWRIGHT_VERSION})"
        ));
        return lines.join("\n");
    }
    lines.push(format!(
        "  playwright: {} (this release pins {PLAYWRIGHT_VERSION})",
        kit.playwright_version.as_deref().unwrap_or("not installed")
    ));
    if let Some(revision) = &kit.chromium_revision {
        lines.push(format!(
            "  chromium:   chromium_headless_shell-{revision} ({})",
            if kit.browser_installed {
                "installed"
            } else {
                "missing"
            }
        ));
    }
    lines.push(format!(
        "  env.sh:     {}",
        match (kit.env_script_present, kit.env_script_managed) {
            (false, _) => "missing",
            (true, true) => "written by Orbit",
            (true, false) => "written by the operator",
        }
    ));
    lines.push(format!(
        "  node:       {}",
        status.node.as_ref().map_or_else(
            || "not on PATH".to_string(),
            |node| node.display().to_string()
        )
    ));
    if let Some(unresolved) = &status.unresolved_libraries {
        lines.push(format!(
            "  libraries:  {}",
            if unresolved.is_empty() {
                "all resolved".to_string()
            } else {
                format!("unresolved: {}", unresolved.join(", "))
            }
        ));
    }
    lines.push(format!(
        "  verified:   {}",
        match (&kit.verification, status.verified) {
            (Some(verification), true) => verification.verified_at.clone(),
            (Some(_), false) => "no (the kit changed after its last pass)".to_string(),
            (None, _) => "no".to_string(),
        }
    ));
    lines.push(format!(
        "  workers:    {}",
        if status.worker_env.is_empty() {
            "receive nothing".to_string()
        } else {
            format!("receive {}", status.worker_env.join(", "))
        }
    ));
    let problems = all_problems(status);
    if !problems.is_empty() {
        lines.push("problems:".to_string());
        for problem in problems {
            lines.push(format!("  - {}", problem.problem));
            lines.push(format!("    fix: {}", problem.fix));
        }
    }
    lines.join("\n")
}

pub(super) fn verify_text(report: &VerifyReport) -> String {
    let mut lines = Vec::new();
    if report.passed {
        lines.push(format!(
            "kit ok: Chromium {} rendered a loopback page{}.",
            report.browser_version.as_deref().unwrap_or("?"),
            report
                .screenshot
                .as_ref()
                .map(|path| format!("; screenshot: {}", path.display()))
                .unwrap_or_default()
        ));
        lines.push("Workers now receive the kit's variables.".to_string());
    } else {
        lines.push(
            "The smoke test failed; workers receive none of the kit's variables.".to_string(),
        );
        if let Some(diagnosis) = &report.diagnosis {
            lines.push(format!("Likely cause: {diagnosis}"));
        }
        if let Some(output) = &report.output {
            lines.push(output.clone());
        }
    }
    lines.push(report.note.to_string());
    lines.join("\n")
}
