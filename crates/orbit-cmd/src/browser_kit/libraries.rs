//! Chromium's Linux system libraries and fonts.
//!
//! `ldd` names what the headless shell cannot resolve, with the kit's
//! unpacked library directories ahead of the system's. Three ways close the
//! gap: the operator's explicit `--with-deps` runs Playwright's
//! `install-deps` under `sudo`; on Debian or Ubuntu, without root, the
//! packages Playwright lists (and the packages they depend on that the host
//! lacks) are downloaded and unpacked into `root/`; anywhere else the
//! libraries and the command that provides them are reported.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::host::{find_on_path, run_interactive, run_probe, run_streaming};
use super::install::LibraryOutcome;
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, create_private_dir_all};
use orbit_core::bootstrap::browser_kit::KitLayout;

const PROBE_DEADLINE: Duration = Duration::from_secs(60);
/// Packages a server without fonts needs so Chromium does not crash in
/// Skia/FontConfig.
const FONT_PACKAGES: [&str; 3] = ["fontconfig-config", "libfontconfig1", "fonts-dejavu-core"];
const NO_ROOT_TOOLS: [&str; 4] = ["apt-get", "apt-cache", "dpkg-deb", "dpkg-query"];

/// Libraries the headless shell at `executable` cannot resolve.
pub(super) fn unresolved(layout: &KitLayout, executable: &Path) -> Result<Vec<String>, OrbitError> {
    let mut command = Command::new("ldd");
    command.arg(executable);
    if let Some(path) = library_path(layout) {
        command.env("LD_LIBRARY_PATH", path);
    }
    let output = run_probe(&mut command, PROBE_DEADLINE)?;
    Ok(parse_not_found(&String::from_utf8_lossy(&output.stdout)))
}

/// The kit's unpacked library directories ahead of the caller's
/// `LD_LIBRARY_PATH`, the way `env.sh` sets it.
fn library_path(layout: &KitLayout) -> Option<String> {
    let mut entries: Vec<String> = layout
        .unpacked_library_dirs()
        .iter()
        .map(|dir| dir.display().to_string())
        .collect();
    if entries.is_empty() {
        return None;
    }
    if let Some(existing) = std::env::var("LD_LIBRARY_PATH")
        .ok()
        .filter(|value| !value.is_empty())
    {
        entries.push(existing);
    }
    Some(entries.join(":"))
}

fn parse_not_found(ldd: &str) -> Vec<String> {
    let mut missing: Vec<String> = ldd
        .lines()
        .filter(|line| line.contains("=> not found"))
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

/// `fc-list` is missing or lists no font.
fn fonts_missing() -> bool {
    let Some(fc_list) = find_on_path("fc-list") else {
        return true;
    };
    run_probe(&mut Command::new(fc_list), PROBE_DEADLINE)
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().is_empty())
        .unwrap_or(true)
}

/// Debian, Ubuntu or a derivative, by `/etc/os-release`.
fn debian_like() -> bool {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    release.lines().any(|line| {
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        matches!(key, "ID" | "ID_LIKE")
            && value
                .trim_matches(['"', '\''])
                .split_whitespace()
                .any(|id| id == "debian" || id == "ubuntu")
    })
}

fn no_root_path_available() -> bool {
    debian_like()
        && NO_ROOT_TOOLS
            .iter()
            .all(|tool| find_on_path(tool).is_some())
}

/// The command that resolves `unresolved` on this host.
pub(super) fn fix(unresolved: &[String]) -> String {
    if debian_like() {
        "orbit browser-kit install (unpacks the packages into the kit without root), or \
         orbit browser-kit install --with-deps (installs them system-wide with sudo)"
            .to_string()
    } else {
        format!(
            "Install the distribution packages that provide {} with the system package manager \
             (for example `dnf provides '*/{}'`), then rerun `orbit browser-kit install`",
            unresolved.join(", "),
            unresolved.first().map(String::as_str).unwrap_or("libX.so")
        )
    }
}

/// Make every library the headless shell needs resolvable, and give a
/// fontless server fonts.
pub(super) fn resolve(
    layout: &KitLayout,
    node: &Path,
    executable: &Path,
    with_deps: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<LibraryOutcome, OrbitError> {
    let missing = unresolved(layout, executable)?;
    let fonts_needed = !layout.fonts_conf().is_file() && fonts_missing();
    if missing.is_empty() && !fonts_needed {
        return Ok(LibraryOutcome {
            method: "resolved",
            unpacked_packages: Vec::new(),
            unresolved: Vec::new(),
            fix: None,
        });
    }
    let (method, unpacked_packages) = if with_deps {
        progress(
            "Installing Chromium's system libraries with sudo (playwright install-deps chromium)",
        );
        let mut command = Command::new("sudo");
        command
            .arg("--")
            .arg(node)
            .arg(layout.cli_script())
            .args(["install-deps", "chromium"]);
        run_interactive(&mut command, "sudo playwright install-deps chromium")?;
        ("sudo", Vec::new())
    } else if no_root_path_available() {
        progress("Unpacking Chromium's missing libraries into the kit (no root needed)");
        (
            "unpacked",
            unpack(layout, node, &missing, fonts_needed, progress)?,
        )
    } else {
        ("unresolved", Vec::new())
    };
    let remaining = unresolved(layout, executable)?;
    let fix_needed = !remaining.is_empty();
    Ok(LibraryOutcome {
        method: if fix_needed { "unresolved" } else { method },
        unpacked_packages,
        fix: fix_needed.then(|| fix(&remaining)),
        unresolved: remaining,
    })
}

/// Download the packages that provide what is missing, plus those they
/// depend on that the host lacks, and unpack them into `root/`.
fn unpack(
    layout: &KitLayout,
    node: &Path,
    missing: &[String],
    fonts_needed: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<Vec<String>, OrbitError> {
    let playwright = if missing.is_empty() {
        DryRun::default()
    } else {
        playwright_packages(layout, node)?
    };
    let fonts: Vec<String> = if fonts_needed {
        FONT_PACKAGES
            .iter()
            .map(|name| (*name).to_string())
            .collect()
    } else {
        Vec::new()
    };
    // apt's simulated install already names every missing dependency; a bare
    // package list still needs its dependencies added.
    let mut wanted = if playwright.resolved {
        let mut wanted = playwright.packages;
        wanted.extend(with_dependencies(&fonts)?);
        wanted
    } else {
        with_dependencies(&[playwright.packages, fonts].concat())?
    };
    wanted.sort();
    wanted.dedup();
    let packages = not_installed(&wanted)?;
    let debs = layout.debs();
    let root = layout.unpacked_root();
    for dir in [&debs, &root] {
        create_private_dir_all(dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
    }
    let before = deb_files(&debs)?;
    if !packages.is_empty() {
        download(&packages, &debs, progress)?;
    }
    for deb in deb_files(&debs)?
        .into_iter()
        .filter(|deb| !before.contains(deb))
    {
        let mut command = Command::new("dpkg-deb");
        command.arg("-x").arg(&deb).arg(&root);
        let output = run_probe(&mut command, PROBE_DEADLINE)?;
        if !output.status.success() {
            return Err(OrbitError::Execution(format!(
                "dpkg-deb -x {} failed: {}",
                deb.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
    }
    if fonts_needed {
        write_fonts_conf(layout)?;
    }
    Ok(packages)
}

/// What Playwright's `install-deps --dry-run chromium` names.
#[derive(Debug, Default)]
struct DryRun {
    packages: Vec<String>,
    /// The list is apt's simulated install: every missing package,
    /// dependencies included.
    resolved: bool,
}

/// The Debian packages Playwright's `install-deps --dry-run chromium` lists.
/// It exits 1 when packages are missing, so only the listing counts.
fn playwright_packages(layout: &KitLayout, node: &Path) -> Result<DryRun, OrbitError> {
    let mut command = Command::new(node);
    command
        .arg(layout.cli_script())
        .args(["install-deps", "--dry-run", "chromium"]);
    let output = run_probe(&mut command, PROBE_DEADLINE)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let dry_run = parse_dry_run(&stdout);
    if dry_run.packages.is_empty() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(OrbitError::Execution(format!(
            "playwright install-deps --dry-run chromium named no packages for the missing \
             libraries: {}",
            [stdout.trim(), detail.trim()]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    Ok(dry_run)
}

/// Playwright's dry run in either form it has printed: the simulated
/// install (`Missing system dependencies (N):` then one indented package
/// per line), or the command it would run (`sudo -- sh -c "apt-get
/// update&& apt-get install -y --no-install-recommends libasound2t64
/// libnss3"`).
fn parse_dry_run(text: &str) -> DryRun {
    let mut dry_run = DryRun::default();
    let mut listing = false;
    for line in text.lines() {
        if line.starts_with("Missing system dependencies") {
            listing = true;
            dry_run.resolved = true;
            continue;
        }
        let tokens: Vec<&str> = if listing && line.starts_with(char::is_whitespace) {
            line.split_whitespace().collect()
        } else if let Some((_, rest)) = line.split_once("apt-get install") {
            listing = false;
            rest.split_whitespace().collect()
        } else {
            listing = false;
            continue;
        };
        for token in tokens {
            let token = token.trim_matches(['"', '\'']);
            if is_package_name(token) && !dry_run.packages.iter().any(|seen| seen == token) {
                dry_run.packages.push(token.to_string());
            }
        }
    }
    dry_run
}

fn is_package_name(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && token.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '+' | '-')
        })
}

/// `packages` and everything they depend on, by `apt-cache depends --recurse`.
fn with_dependencies(packages: &[String]) -> Result<Vec<String>, OrbitError> {
    if packages.is_empty() {
        return Ok(Vec::new());
    }
    let mut command = Command::new("apt-cache");
    command
        .args([
            "depends",
            "--recurse",
            "--no-recommends",
            "--no-suggests",
            "--no-conflicts",
            "--no-breaks",
            "--no-replaces",
            "--no-enhances",
        ])
        .args(packages);
    let output = run_probe(&mut command, Duration::from_secs(180))?;
    let mut all: Vec<String> = packages.to_vec();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        // Unindented lines name packages; indented ones are relations and
        // `<name>` is a virtual package nothing can download.
        let name = line.trim_end();
        if !name.is_empty()
            && !line.starts_with(char::is_whitespace)
            && is_package_name(name)
            && !all.iter().any(|seen| seen == name)
        {
            all.push(name.to_string());
        }
    }
    Ok(all)
}

/// The subset of `packages` dpkg does not report installed.
fn not_installed(packages: &[String]) -> Result<Vec<String>, OrbitError> {
    if packages.is_empty() {
        return Ok(Vec::new());
    }
    let mut command = Command::new("dpkg-query");
    command
        .args(["-W", "-f=${Package} ${db:Status-Abbrev}\\n"])
        .args(packages);
    // Unknown names make dpkg-query exit 1 after listing the known ones.
    let output = run_probe(&mut command, PROBE_DEADLINE)?;
    let listing = String::from_utf8_lossy(&output.stdout);
    let installed: Vec<&str> = listing
        .lines()
        .filter_map(|line| {
            let (name, status) = line.split_once(' ')?;
            status.starts_with("ii").then_some(name)
        })
        .collect();
    Ok(packages
        .iter()
        .filter(|name| !installed.contains(&name.as_str()))
        .cloned()
        .collect())
}

/// `apt-get download` the packages into `debs`, one at a time after a
/// batch failure so one unavailable name does not lose the rest.
fn download(
    packages: &[String],
    debs: &Path,
    progress: &mut dyn FnMut(&str),
) -> Result<(), OrbitError> {
    let mut batch = Command::new("apt-get");
    batch.arg("download").args(packages).current_dir(debs);
    if run_streaming(&mut batch, "apt-get download").is_ok() {
        return Ok(());
    }
    let mut failed = Vec::new();
    for package in packages {
        let mut single = Command::new("apt-get");
        single.args(["download", package]).current_dir(debs);
        if run_streaming(&mut single, "apt-get download").is_err() {
            failed.push(package.as_str());
        }
    }
    if !failed.is_empty() {
        progress(&format!(
            "Could not download {}; continuing with the rest",
            failed.join(", ")
        ));
    }
    Ok(())
}

fn deb_files(dir: &Path) -> Result<Vec<PathBuf>, OrbitError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", dir.display())))?;
    let mut debs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "deb"))
        .collect();
    debs.sort();
    Ok(debs)
}

/// The font configuration `env.sh` points `FONTCONFIG_FILE` at.
fn write_fonts_conf(layout: &KitLayout) -> Result<(), OrbitError> {
    let cache = layout.fontconfig_cache();
    create_private_dir_all(&cache)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", cache.display())))?;
    let root = layout.unpacked_root();
    let conf = format!(
        "<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">\n\
         <fontconfig>\n  <dir>{}</dir>\n  <cachedir>{}</cachedir>\n  \
         <include ignore_missing=\"yes\">{}</include>\n</fontconfig>\n",
        root.join("usr/share/fonts").display(),
        cache.display(),
        root.join("etc/fonts/conf.d").display(),
    );
    atomic_write_text(&layout.fonts_conf(), &conf).map_err(|error| {
        OrbitError::Io(format!("write {}: {error}", layout.fonts_conf().display()))
    })
}
