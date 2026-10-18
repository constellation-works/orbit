//! `orbit browser-kit install`: build, upgrade or adopt the kit, then
//! verify it.
//!
//! Every step is skipped when the kit already satisfies it, so a hand-built
//! kit on the pinned version is adopted as it is: its package, browser,
//! unpacked libraries and the operator's own `env.sh` stay untouched, and
//! only the verification record is added.

use std::path::Path;
use std::process::Command;

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, create_private_dir_all};
use orbit_core::bootstrap::browser_kit::{
    BROWSERS_PATH_ENV, KitInspection, KitLayout, MANAGED_ENV_SCRIPT_HEADER, MODULE_ENV,
    PLAYWRIGHT_VERSION,
};
use serde::Serialize;

use super::host::{require_host_operator, require_tool, run_streaming, sh_quote};
use super::status::{KitStatus, kit_status};
use super::verify::{VerifyReport, verify};

#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOptions {
    /// Linux: install Chromium's system libraries with `sudo` (Playwright's
    /// `install-deps`) instead of unpacking them into the kit.
    pub with_deps: bool,
}

/// How `install` left Chromium's libraries.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryOutcome {
    /// `resolved` (nothing was missing), `sudo`, `unpacked` or `unresolved`.
    pub method: &'static str,
    /// Packages unpacked into the kit.
    pub unpacked_packages: Vec<String>,
    /// What still cannot be resolved.
    pub unresolved: Vec<String>,
    /// The fix for `unresolved`.
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallReport {
    /// A kit with a Playwright module was already there.
    pub adopted: bool,
    /// What each step did, in order.
    pub steps: Vec<String>,
    /// Linux only.
    pub libraries: Option<LibraryOutcome>,
    /// `None` when unresolved libraries kept the smoke test from running.
    pub verification: Option<VerifyReport>,
    pub status: KitStatus,
}

impl InstallReport {
    /// Workers receive the kit.
    pub fn ready(&self) -> bool {
        self.status.verified
    }
}

pub fn install(
    layout: &KitLayout,
    options: InstallOptions,
    progress: &mut dyn FnMut(&str),
) -> Result<InstallReport, OrbitError> {
    require_host_operator()?;
    let node = require_tool("node")?;
    let npm = require_tool("npm")?;
    let before = KitInspection::inspect(layout);
    let adopted = before.module_present;
    let mut steps = Vec::new();

    for dir in [
        layout.root().to_path_buf(),
        layout.npm_project(),
        layout.browsers(),
    ] {
        create_private_dir_all(&dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
    }
    let package_json = layout.npm_project().join("package.json");
    if !package_json.is_file() {
        write(
            &package_json,
            "{\n  \"name\": \"orbit-browser-kit\",\n  \"private\": true\n}\n",
        )?;
    }

    match before.playwright_version.as_deref() {
        Some(PLAYWRIGHT_VERSION) => steps.push(format!("kept playwright {PLAYWRIGHT_VERSION}")),
        installed => {
            progress(&format!("Installing playwright@{PLAYWRIGHT_VERSION}"));
            let mut command = Command::new(&npm);
            command
                .args(["install", "--no-fund", "--no-audit", "--save-exact"])
                .arg(format!("playwright@{PLAYWRIGHT_VERSION}"))
                .current_dir(layout.npm_project());
            run_streaming(&mut command, "npm install playwright")?;
            steps.push(match installed {
                Some(old) => format!("moved playwright {old} to the pinned {PLAYWRIGHT_VERSION}"),
                None => format!("installed playwright {PLAYWRIGHT_VERSION}"),
            });
        }
    }

    let package = KitInspection::inspect(layout);
    let revision = package.chromium_revision.clone().ok_or_else(|| {
        OrbitError::Execution(
            "the installed playwright package names no chromium-headless-shell revision"
                .to_string(),
        )
    })?;
    if package.browser_installed {
        steps.push(format!("kept chromium_headless_shell-{revision}"));
    } else {
        progress(&format!(
            "Downloading Chromium's headless shell (revision {revision}) from Playwright's CDN"
        ));
        let mut command = Command::new(&node);
        command
            .arg(layout.cli_script())
            .args(["install", "--only-shell", "chromium"])
            .env(BROWSERS_PATH_ENV, layout.browsers())
            .current_dir(layout.npm_project());
        run_streaming(&mut command, "playwright install --only-shell chromium")?;
        steps.push(format!("downloaded chromium_headless_shell-{revision}"));
    }

    let installed = KitInspection::inspect(layout);
    for stale in &installed.stale_revisions {
        let dir = layout.browsers().join(stale);
        std::fs::remove_dir_all(&dir)
            .map_err(|error| OrbitError::Io(format!("remove {}: {error}", dir.display())))?;
        steps.push(format!("removed old revision {stale}"));
    }

    let libraries = resolve_libraries(layout, &node, &installed, options, progress)?;
    steps.push(write_env_script(layout)?);

    let resolved = libraries
        .as_ref()
        .is_none_or(|outcome| outcome.unresolved.is_empty());
    let verification = if resolved {
        progress("Verifying the kit with a loopback screenshot");
        Some(verify(layout)?)
    } else {
        None
    };
    Ok(InstallReport {
        adopted,
        steps,
        libraries,
        verification,
        status: kit_status(layout),
    })
}

#[cfg(target_os = "linux")]
fn resolve_libraries(
    layout: &KitLayout,
    node: &Path,
    installed: &KitInspection,
    options: InstallOptions,
    progress: &mut dyn FnMut(&str),
) -> Result<Option<LibraryOutcome>, OrbitError> {
    let executable = installed.executable.as_deref().ok_or_else(|| {
        OrbitError::Execution(format!(
            "no chrome-headless-shell executable under {}",
            layout.browsers().display()
        ))
    })?;
    super::libraries::resolve(layout, node, executable, options.with_deps, progress).map(Some)
}

#[cfg(not(target_os = "linux"))]
fn resolve_libraries(
    _layout: &KitLayout,
    _node: &Path,
    _installed: &KitInspection,
    _options: InstallOptions,
    _progress: &mut dyn FnMut(&str),
) -> Result<Option<LibraryOutcome>, OrbitError> {
    // macOS supplies Chromium's libraries and fonts.
    Ok(None)
}

/// Write `env.sh` unless the operator wrote their own.
fn write_env_script(layout: &KitLayout) -> Result<String, OrbitError> {
    let path = layout.env_script();
    let wanted = managed_env_script(layout);
    match std::fs::read_to_string(&path) {
        Ok(existing) if !existing.starts_with(MANAGED_ENV_SCRIPT_HEADER) => {
            return Ok("kept the operator's env.sh".to_string());
        }
        Ok(existing) if existing == wanted => return Ok("env.sh is current".to_string()),
        _ => {}
    }
    write(&path, &wanted)?;
    Ok("wrote env.sh".to_string())
}

/// `env.sh` for this kit. The library and font lines appear only when the
/// kit unpacked them, so a check's shell gets them and nothing else does.
fn managed_env_script(layout: &KitLayout) -> String {
    let mut lines = vec![
        MANAGED_ENV_SCRIPT_HEADER.to_string(),
        "# Source this in the shell that runs a Playwright check.".to_string(),
    ];
    let library_dirs = layout.unpacked_library_dirs();
    if !library_dirs.is_empty() {
        let dirs = library_dirs
            .iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
        lines.push(format!(
            "export LD_LIBRARY_PATH={}\"${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}\"",
            sh_quote(&dirs)
        ));
    }
    if layout.fonts_conf().is_file() {
        lines.push(format!(
            "export FONTCONFIG_FILE={}",
            sh_quote(&layout.fonts_conf().display().to_string())
        ));
    }
    lines.push(format!(
        "export {BROWSERS_PATH_ENV}={}",
        sh_quote(&layout.browsers().display().to_string())
    ));
    lines.push(format!(
        "export {MODULE_ENV}={}",
        sh_quote(&layout.module().display().to_string())
    ));
    lines.push(String::new());
    lines.join("\n")
}

fn write(path: &Path, content: &str) -> Result<(), OrbitError> {
    atomic_write_text(path, content)
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))
}
