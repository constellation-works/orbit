//! The kit's state as `orbit browser-kit status` and `orbit doctor` report
//! it, and its removal.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_core::bootstrap::browser_kit::{
    KitInspection, KitLayout, PLAYWRIGHT_VERSION, WORKER_ENV_NAMES,
};
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

pub fn kit_status(layout: &KitLayout) -> KitStatus {
    let inspection = KitInspection::inspect(layout);
    let verified = inspection.is_verified();
    let state = if !inspection.present {
        "not_installed"
    } else if !inspection.module_present
        || !inspection.browser_installed
        || !inspection.env_script_present
    {
        "incomplete"
    } else if verified {
        "verified"
    } else {
        "unverified"
    };
    let unresolved_libraries = unresolved_libraries(layout, &inspection);
    KitStatus {
        matches_pin: inspection.matches_pin(),
        verified,
        state,
        node: find_on_path("node"),
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
fn unresolved_libraries(layout: &KitLayout, inspection: &KitInspection) -> Option<Vec<String>> {
    let executable = inspection.executable.as_deref()?;
    super::libraries::unresolved(layout, executable).ok()
}

#[cfg(not(target_os = "linux"))]
fn unresolved_libraries(_layout: &KitLayout, _inspection: &KitInspection) -> Option<Vec<String>> {
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
                "orbit browser-kit install",
            )),
            Some(version) if version != PLAYWRIGHT_VERSION => problems.push(install(
                format!("Playwright {version} is installed; this Orbit release pins {PLAYWRIGHT_VERSION}"),
                "orbit browser-kit install (moves the kit to the pin and deletes the old browser revisions)",
            )),
            Some(_) => {}
        }
        if kit.module_present && !kit.browser_installed {
            problems.push(install(
                format!(
                    "the browser revision chromium_headless_shell-{} is not installed under browsers/",
                    kit.chromium_revision.as_deref().unwrap_or("?")
                ),
                "orbit browser-kit install",
            ));
        }
        if !kit.env_script_present {
            problems.push(install(
                "env.sh is missing".to_string(),
                "orbit browser-kit install",
            ));
        }
        if !kit.stale_revisions.is_empty() {
            problems.push(install(
                format!(
                    "old browser revisions remain: {}",
                    kit.stale_revisions.join(", ")
                ),
                "orbit browser-kit install",
            ));
        }
        if problems.is_empty() && !self.verified {
            problems.push(install(
                "the kit has not passed `verify` at its installed version, so workers receive none of its variables"
                    .to_string(),
                "orbit browser-kit verify",
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
    "orbit browser-kit install".to_string()
}

/// Delete the kit. Nothing else on the host refers to it, and workers stop
/// receiving its variables. `false` when there was no kit.
pub fn remove(layout: &KitLayout) -> Result<bool, OrbitError> {
    require_host_operator()?;
    let root = layout.root();
    if !root.exists() {
        return Ok(false);
    }
    std::fs::remove_dir_all(root)
        .map_err(|error| OrbitError::Io(format!("remove {}: {error}", root.display())))?;
    Ok(true)
}
