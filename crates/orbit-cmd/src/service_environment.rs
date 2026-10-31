//! Names-only checks of the managed launcher's actual environment. A login
//! shell is not evidence about a service that was started earlier.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::PathBuf;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::Duration;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use orbit_common::process::run_bounded_capped;
use orbit_common::security::child_env::crew_env_file_problem;
use orbit_core::OrbitRuntime;
use orbit_types::identity::{CrewEnvSource, CrewEnvVar};

/// Diagnose a successful `crews.<crew>.env` write using its newly resolved
/// value, rather than the runtime's configuration from before the write.
pub fn crew_env_config_write_warnings(
    runtime: &OrbitRuntime,
    key: &str,
    value: &serde_json::Value,
) -> Vec<String> {
    let Some(name) = key
        .strip_prefix("crews.")
        .and_then(|key| key.strip_suffix(".env"))
    else {
        return Vec::new();
    };
    let entries = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter_map(|entry| CrewEnvVar::parse(entry).ok())
        .collect();
    crew_source_environment_warnings(runtime, &[(name.to_string(), entries)])
}

/// Warnings for declared crew sources missing from the current shell or a
/// managed service. Probe failures are reported as unknown, never as success.
/// A `file:` source is read by the host itself whatever launches it, so it is
/// reported by the file's own state and no launcher is asked about it.
pub fn crew_source_environment_warnings(
    runtime: &OrbitRuntime,
    crews: &[(String, Vec<CrewEnvVar>)],
) -> Vec<String> {
    if crews.iter().all(|(_, entries)| entries.is_empty()) {
        return Vec::new();
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = runtime;
    let mut warnings = Vec::new();
    for (crew, entries) in crews {
        for entry in entries {
            match &entry.source {
                CrewEnvSource::Variable(source) => {
                    if std::env::var_os(source).is_none_or(|value| value.is_empty()) {
                        warnings.push(format!(
                            "crew `{crew}` env source {source} is unset or empty in the current \
                             shell"
                        ));
                    }
                }
                CrewEnvSource::File(_) => {
                    if let Some(problem) = crew_env_file_problem(entry) {
                        warnings.push(format!(
                            "crew `{crew}` env source {} (for {}) is unusable: {problem}; the \
                             crew is unavailable until the file is fixed",
                            entry.source, entry.name
                        ));
                    }
                }
            }
        }
    }
    if source_names(crews).is_empty() {
        return warnings;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return warnings;
    };
    #[cfg(target_os = "linux")]
    for unit in ["orbit-web.service", "orbit-sweep.service"] {
        let Some(path) = [
            home.join(".config/systemd/user"),
            home.join(".local/share/systemd/user"),
            PathBuf::from("/etc/systemd/user"),
            PathBuf::from("/usr/local/lib/systemd/user"),
            PathBuf::from("/usr/lib/systemd/user"),
        ]
        .into_iter()
        .map(|directory| directory.join(unit))
        .find(|path| path.is_file()) else {
            continue;
        };
        let location = format!(
            "{unit} (systemd user manager, {} and its Environment=/EnvironmentFile= drop-ins)",
            path.display()
        );
        let clock = if unit == "orbit-sweep.service" {
            clock_source_names(runtime, crews, &mut warnings)
        } else {
            BTreeSet::new()
        };
        let sources = source_names(crews);
        let held = if sources.iter().all(|source| clock.contains(source)) {
            Some(clock)
        } else {
            linux_service_names(unit, &path).map(|mut held| {
                held.extend(clock);
                held
            })
        };
        match held {
            Some(held) => {
                for (crew, entries) in crews {
                    for source in entries.iter().filter_map(|entry| entry.source.variable()) {
                        if !held.contains(source) {
                            warnings.push(format!("crew `{crew}` env source {source} is unset or empty in the managed service environment: {location}; export it in the service environment and restart the service before starting a new drain"));
                        }
                    }
                }
            }
            None => warnings.push(format!("cannot verify crew env sources {} in the managed service environment: {location}; service is stopped or its live environment is unreadable; a login shell export does not supply a service", source_names(crews).join(", "))),
        }
    }
    #[cfg(target_os = "macos")]
    for label in ["com.orbit.web", "com.orbit.sweep"] {
        let path = home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        if !path.is_file() {
            continue;
        }
        let clock = if label == "com.orbit.sweep" {
            clock_source_names(runtime, crews, &mut warnings)
        } else {
            BTreeSet::new()
        };
        let sources = source_names(crews);
        let held = if sources.iter().all(|source| clock.contains(source)) {
            Some(clock)
        } else {
            launchd_service_names(label).map(|mut held| {
                held.extend(clock);
                held
            })
        };
        match held {
            Some(held) => for (crew, entries) in crews {
                for source in entries.iter().filter_map(|entry| entry.source.variable()) {
                    if !held.contains(source) {
                        warnings.push(format!("crew `{crew}` env source {source} is unset or empty in the managed service environment: {label} (launchd, {} EnvironmentVariables; clock runs also read {}/clock.env); provide the source there and reload the LaunchAgent before starting a new drain", path.display(), runtime.global_root().display()));
                    }
                }
            },
            None => warnings.push(format!("cannot verify crew env sources {} in {label}'s managed launchd environment ({} EnvironmentVariables; clock runs also read {}/clock.env); a login shell export does not supply a LaunchAgent", sources.join(", "), path.display(), runtime.global_root().display())),
        }
    }
    warnings
}

/// The source variables the crews read, which a launcher's environment must
/// supply; `file:` sources need nothing from a launcher.
fn source_names(crews: &[(String, Vec<CrewEnvVar>)]) -> Vec<String> {
    crews
        .iter()
        .flat_map(|(_, entries)| entries.iter().filter_map(|entry| entry.source.variable()))
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(target_os = "linux")]
fn linux_service_names(unit: &str, path: &Path) -> Option<BTreeSet<String>> {
    let mut command = Command::new("systemctl");
    command.args([
        "--user",
        "show",
        "--property=MainPID",
        "--property=FragmentPath",
        unit,
    ]);
    let output = run_bounded_capped(&mut command, Duration::from_secs(2), 8192).ok()?;
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?;
    // Do not mistake another user's manager or an overridden unit for the
    // installed launcher we are checking.
    let fragment = text
        .lines()
        .find_map(|line| line.strip_prefix("FragmentPath="))?;
    if std::fs::canonicalize(fragment).ok()? != std::fs::canonicalize(path).ok()? {
        return None;
    }
    let pid = text
        .lines()
        .find_map(|line| line.strip_prefix("MainPID="))?
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0)?;
    let bytes = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    Some(
        bytes
            .split(|byte| *byte == 0)
            .filter_map(|entry| {
                let split = entry.iter().position(|byte| *byte == b'=')?;
                (split + 1 < entry.len())
                    .then(|| {
                        std::str::from_utf8(&entry[..split])
                            .ok()
                            .map(str::to_string)
                    })
                    .flatten()
            })
            .collect(),
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn clock_source_names(
    runtime: &OrbitRuntime,
    crews: &[(String, Vec<CrewEnvVar>)],
    warnings: &mut Vec<String>,
) -> BTreeSet<String> {
    match orbit_common::security::operator_env::clock_env_file_names(
        &runtime.global_root(),
        &source_names(crews),
    ) {
        Ok(names) => names.unwrap_or_default().into_iter().collect(),
        Err(_) => {
            warnings.push(format!("cannot verify crew sources in {}/clock.env: the file is unreadable or fails owner-only checks; run `orbit doctor` for clock environment diagnostics", runtime.global_root().display()));
            BTreeSet::new()
        }
    }
}

#[cfg(target_os = "macos")]
fn launchd_service_names(label: &str) -> Option<BTreeSet<String>> {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let mut command = Command::new("launchctl");
    command.args(["print", &format!("gui/{uid}/{label}")]);
    let output = run_bounded_capped(&mut command, Duration::from_secs(2), 256 * 1024).ok()?;
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?;
    let mut blocks = std::collections::BTreeMap::<String, Vec<(String, bool)>>::new();
    let mut block = None;
    for line in text.lines().map(str::trim) {
        if [
            "default environment = {",
            "inherited environment = {",
            "environment = {",
        ]
        .contains(&line)
        {
            block = Some(line.to_string());
        } else if line == "}" {
            block = None;
        } else if let Some(block) = &block
            && let Some((name, value)) = line.split_once(" => ")
        {
            blocks
                .entry(block.clone())
                .or_default()
                .push((name.to_string(), !value.trim_matches('"').is_empty()));
        }
    }
    if blocks.is_empty() {
        return None;
    }
    let mut held = BTreeSet::new();
    for key in [
        "default environment = {",
        "inherited environment = {",
        "environment = {",
    ] {
        for (name, nonempty) in blocks.get(key).into_iter().flatten() {
            if *nonempty {
                held.insert(name.clone());
            } else {
                held.remove(name);
            }
        }
    }
    Some(held)
}
