//! The kit's state as `orbit config browser-kit status` and `orbit doctor` report
//! it, and its removal.

use std::path::PathBuf;

use crate::bootstrap::browser_kit::{
    KitInspection, KitLayout, PIXEL_HELPER_PACKAGE, PIXEL_HELPER_VERSION, PLAYWRIGHT_VERSION,
    WORKER_ENV_NAMES,
};
use orbit_common::{Env, OrbitError};
use serde::Serialize;

use super::host::{find_on_path, require_host_operator};

/// The kit, the tools around it, and what workers receive from it.
#[derive(Debug, Clone, Serialize)]
pub struct KitStatus {
    #[serde(flatten)]
    pub inspection: KitInspection,
    /// `not_installed`, `incomplete`, `unverified` or `verified`.
    pub state: &'static str,
    pub matches_pin: bool,
    pub verified: bool,
    /// `node` on this process's PATH.
    pub node: Option<PathBuf>,
    /// Libraries the headless shell cannot resolve. `None` when not checked:
    /// not Linux, or no installed shell to check.
    pub unresolved_libraries: Option<Vec<String>>,
    /// The variables every agent child environment receives now.
    pub worker_env: Vec<&'static str>,
}

/// One thing wrong with an installed kit, and the command that fixes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KitProblem {
    pub problem: String,
    pub fix: String,
}

pub fn kit_status(layout: &KitLayout, env: &Env) -> KitStatus {
    let inspection = KitInspection::inspect(layout);
    let verified = inspection.is_verified();
    let state = if !inspection.present {
        "not_installed"
    } else if !inspection.module_present
        || !inspection.browser_installed
        || inspection.executable.is_none()
        || !inspection.env_script_present
    {
        "incomplete"
    } else if verified {
        "verified"
    } else {
        "unverified"
    };
    let unresolved_libraries = unresolved_libraries(layout, &inspection, env);
    KitStatus {
        matches_pin: inspection.matches_pin(),
        verified,
        state,
        node: find_on_path(env, "node"),
        unresolved_libraries,
        worker_env: if verified {
            WORKER_ENV_NAMES.to_vec()
        } else {
            Vec::new()
        },
        inspection,
    }
}

#[cfg(target_os = "linux")]
fn unresolved_libraries(
    layout: &KitLayout,
    inspection: &KitInspection,
    env: &Env,
) -> Option<Vec<String>> {
    let executable = inspection.executable.as_deref()?;
    super::libraries::unresolved(layout, executable, env).ok()
}

#[cfg(not(target_os = "linux"))]
fn unresolved_libraries(
    _layout: &KitLayout,
    _inspection: &KitInspection,
    _env: &Env,
) -> Option<Vec<String>> {
    None
}

impl KitStatus {
    /// Everything wrong with an installed kit except its libraries, which
    /// [`Self::library_problem`] reports. Empty for a verified kit on the pin
    /// and for no kit at all.
    pub fn problems(&self) -> Vec<KitProblem> {
        let kit = &self.inspection;
        if !kit.present {
            return Vec::new();
        }
        let install = |problem: String, fix: &str| KitProblem {
            problem,
            fix: fix.to_string(),
        };
        let mut problems = Vec::new();
        match kit.playwright_version.as_deref() {
            None => problems.push(install(
                "the Playwright module is missing from kit/".to_string(),
                "orbit config browser-kit install",
            )),
            Some(version) if version != PLAYWRIGHT_VERSION => problems.push(install(
                format!("Playwright {version} is installed; this Orbit release pins {PLAYWRIGHT_VERSION}"),
                "orbit config browser-kit install (moves the kit to the pin and deletes the old browser revisions)",
            )),
            Some(_) => {}
        }
        match kit.pixel_helper_version.as_deref() {
            Some(PIXEL_HELPER_VERSION) => {}
            Some(version) => problems.push(install(
                format!(
                    "the pixel-compare helper {PIXEL_HELPER_PACKAGE} {version} is installed; this Orbit release pins {PIXEL_HELPER_VERSION}, so screenshot pixel checks are unavailable"
                ),
                "orbit config browser-kit install",
            )),
            None => problems.push(install(
                format!(
                    "the pixel-compare helper {PIXEL_HELPER_PACKAGE} is missing from kit/, so screenshot pixel checks are unavailable"
                ),
                "orbit config browser-kit install",
            )),
        }
        if kit.module_present && (!kit.browser_installed || kit.executable.is_none()) {
            problems.push(install(
                format!(
                    "the browser revision chromium_headless_shell-{} is incomplete or missing its executable under browsers/",
                    kit.chromium_revision.as_deref().unwrap_or("?")
                ),
                "orbit config browser-kit install",
            ));
        }
        if !kit.env_script_present {
            problems.push(install(
                "env.sh is missing".to_string(),
                "orbit config browser-kit install",
            ));
        }
        if !kit.stale_revisions.is_empty() {
            problems.push(install(
                format!(
                    "old browser revisions remain: {}",
                    kit.stale_revisions.join(", ")
                ),
                "orbit config browser-kit install",
            ));
        }
        if problems.is_empty() && !self.verified {
            problems.push(install(
                "the kit has not passed `verify` at its installed version, so workers receive none of its variables"
                    .to_string(),
                "orbit config browser-kit verify",
            ));
        }
        problems
    }

    /// The Linux libraries Chromium cannot resolve, with their fix.
    pub fn library_problem(&self) -> Option<KitProblem> {
        let unresolved = self.unresolved_libraries.as_ref()?;
        if unresolved.is_empty() {
            return None;
        }
        Some(KitProblem {
            problem: format!(
                "Chromium cannot resolve these system libraries: {}",
                unresolved.join(", ")
            ),
            fix: library_fix(unresolved),
        })
    }
}

#[cfg(target_os = "linux")]
pub(super) fn library_fix(unresolved: &[String]) -> String {
    super::libraries::fix(unresolved)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn library_fix(_unresolved: &[String]) -> String {
    "orbit config browser-kit install".to_string()
}

/// Delete the kit. Nothing else on the host refers to it, and workers stop
/// receiving its variables. `false` when there was no kit.
pub fn remove(layout: &KitLayout, env: &Env) -> Result<bool, OrbitError> {
    require_host_operator(env)?;
    let root = layout.root();
    if !root.exists() {
        return Ok(false);
    }
    std::fs::remove_dir_all(root)
        .map_err(|error| OrbitError::Io(format!("remove {}: {error}", root.display())))?;
    Ok(true)
}
