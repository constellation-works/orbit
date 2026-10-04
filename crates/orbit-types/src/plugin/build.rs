//! `spec.build`: how a plugin built from source produces its backend, and the
//! build record a host keeps for the bytes it produced.
//!
//! Design: `docs/design/plugins/3_install_time_build.md`. This module holds
//! the manifest shape, its pure validation, and the record and digest
//! preimage; running a build belongs to `orbit-tools` and `orbit-exec`, and
//! consent to `orbit-core`.

use serde::{Deserialize, Serialize};

use super::manifest::{MANIFEST_FILE_NAME, PluginManifestError, validate_plugin_relative_path};
use super::template::template_references;

/// The one template a build argv may use: the build directory (§1).
pub const BUILD_DIR_TEMPLATE: &str = "{{build_dir}}";

/// The default wall-clock bound of one build phase (§4, resource exhaustion).
pub const DEFAULT_PLUGIN_BUILD_TIMEOUT_MS: u64 = 1_200_000;

/// The largest `spec.build.timeout_ms` a manifest may declare.
pub const PLUGIN_BUILD_TIMEOUT_CEILING_MS: u64 = 3_600_000;

/// The literal consent a build record names: the flag the operator passed.
pub const PLUGIN_BUILD_CONSENT_FLAG: &str = "--allow-build";

/// The Linux build profile a record names (§3.2).
pub const PLUGIN_BUILD_PROFILE_LINUX: &str = "linux-bwrap-build-v1";

/// The macOS build profile a record names (§3.2).
pub const PLUGIN_BUILD_PROFILE_MACOS: &str = "macos-sandbox-build-v1";

/// Domain separator of the artifact digest (§3.6). Bumping it changes every
/// recorded digest, so a record from another scheme never compares equal.
pub const PLUGIN_BUILD_DIGEST_DOMAIN: &str = "orbit.plugin.build.v1";

/// `spec.build` (§1): argv arrays, never a shell string, so what consent
/// displays is what runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginBuildSpec {
    /// Host programs the phases run, resolved against the consenting
    /// operator's `PATH` like `requires.programs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub programs: Vec<String>,
    /// The optional dependency download: the only phase with a network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<Vec<String>>,
    /// The offline build.
    pub command: Vec<String>,
    /// Files the build produces, copied into the plugin root. Nothing else
    /// the build writes is installed.
    pub outputs: Vec<PluginBuildOutput>,
    /// Bound of each phase; [`DEFAULT_PLUGIN_BUILD_TIMEOUT_MS`] when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// One declared build output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginBuildOutput {
    /// Relative to `{{build_dir}}`.
    pub from: String,
    /// Relative to the plugin root.
    pub to: String,
}

impl PluginBuildSpec {
    /// The bound each phase runs under.
    pub fn effective_timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(DEFAULT_PLUGIN_BUILD_TIMEOUT_MS)
    }

    /// Structural checks that need no filesystem.
    pub fn validate(&self) -> Result<(), PluginManifestError> {
        for (index, program) in self.programs.iter().enumerate() {
            validate_program(program, &format!("spec.build.programs[{index}]"))?;
        }
        if let Some(fetch) = &self.fetch {
            validate_argv(fetch, "spec.build.fetch")?;
            self.validate_declared_program(fetch, "spec.build.fetch")?;
        }
        validate_argv(&self.command, "spec.build.command")?;
        self.validate_declared_program(&self.command, "spec.build.command")?;
        if self.outputs.is_empty() {
            return Err(PluginManifestError::new(
                "spec.build.outputs",
                "a build must declare at least one output; nothing else it writes is installed",
            ));
        }
        let mut targets = std::collections::BTreeSet::new();
        for (index, output) in self.outputs.iter().enumerate() {
            let field = format!("spec.build.outputs[{index}]");
            validate_output_path(&output.from, &format!("{field}.from"))?;
            validate_output_path(&output.to, &format!("{field}.to"))?;
            let to = output.to.trim();
            if to == MANIFEST_FILE_NAME {
                return Err(PluginManifestError::new(
                    format!("{field}.to"),
                    format!("a build output must not replace {MANIFEST_FILE_NAME}"),
                ));
            }
            if !targets.insert(to) {
                return Err(PluginManifestError::new(
                    format!("{field}.to"),
                    format!("'{to}' is declared as an output more than once"),
                ));
            }
        }
        if let Some(timeout) = self.timeout_ms
            && !(1..=PLUGIN_BUILD_TIMEOUT_CEILING_MS).contains(&timeout)
        {
            return Err(PluginManifestError::new(
                "spec.build.timeout_ms",
                format!("must be between 1 and {PLUGIN_BUILD_TIMEOUT_CEILING_MS} milliseconds"),
            ));
        }
        Ok(())
    }
}

impl PluginBuildSpec {
    /// Each phase runs a declared program, so the plan shown at consent names
    /// every executable resolved on the operator's `PATH`.
    fn validate_declared_program(
        &self,
        argv: &[String],
        field: &str,
    ) -> Result<(), PluginManifestError> {
        match argv.first() {
            Some(program) if !self.programs.contains(program) => Err(PluginManifestError::new(
                format!("{field}[0]"),
                format!("'{program}' must be declared in spec.build.programs"),
            )),
            _ => Ok(()),
        }
    }
}

fn validate_program(program: &str, field: &str) -> Result<(), PluginManifestError> {
    if program.trim().is_empty()
        || program != program.trim()
        || program.starts_with('-')
        || program.chars().any(|c| c.is_control() || c == '=')
    {
        return Err(PluginManifestError::new(
            field,
            format!("'{program}' is not a program name or absolute path"),
        ));
    }
    if program.contains('/') && !program.starts_with('/') {
        return Err(PluginManifestError::new(
            field,
            format!("'{program}' must be a bare program name or an absolute path"),
        ));
    }
    Ok(())
}

fn validate_argv(argv: &[String], field: &str) -> Result<(), PluginManifestError> {
    let Some(program) = argv.first() else {
        return Err(PluginManifestError::new(
            field,
            "must be a non-empty argv array",
        ));
    };
    if program.trim().is_empty() || program.contains("{{") {
        return Err(PluginManifestError::new(
            format!("{field}[0]"),
            "the program must be a literal, non-empty name",
        ));
    }
    for (index, arg) in argv.iter().enumerate() {
        if arg.contains('\0') {
            return Err(PluginManifestError::new(
                format!("{field}[{index}]"),
                "must not contain a NUL byte",
            ));
        }
        // An unterminated `{{` would otherwise run as a literal argument.
        if arg.matches("{{").count() != arg.matches("}}").count() {
            return Err(PluginManifestError::new(
                format!("{field}[{index}]"),
                format!("'{arg}' has an unterminated template reference"),
            ));
        }
        for reference in template_references(arg) {
            if reference != "build_dir" {
                return Err(PluginManifestError::new(
                    format!("{field}[{index}]"),
                    format!(
                        "'{{{{{reference}}}}}' is not available in a build argv; only \
                         {BUILD_DIR_TEMPLATE} is"
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_output_path(value: &str, field: &str) -> Result<(), PluginManifestError> {
    validate_plugin_relative_path(value, field)?;
    if value.contains('*') || value.contains("{{") {
        return Err(PluginManifestError::new(
            field,
            format!("'{value}' must name one file, without a wildcard or template"),
        ));
    }
    if value.split('/').any(|component| component == ".") {
        return Err(PluginManifestError::new(
            field,
            format!("'{value}' must not contain a '.' path component"),
        ));
    }
    Ok(())
}

/// Replace [`BUILD_DIR_TEMPLATE`] in each argument. Validation has refused
/// every other reference, so nothing else is substituted.
pub fn render_build_argv(argv: &[String], build_dir: &str) -> Vec<String> {
    argv.iter()
        .map(|arg| arg.replace(BUILD_DIR_TEMPLATE, build_dir))
        .collect()
}

/// Render an argv vector for a terminal-facing build plan or audit row.
/// Rust's string debug representation escapes control characters, so a
/// manifest cannot use a newline or ANSI escape to forge consent-plan lines.
pub fn format_plugin_build_argv(argv: &[String]) -> String {
    format!("{argv:?}")
}

/// Whether `value` is a full Git commit object id: 40 (SHA-1) or 64
/// (SHA-256) hex characters. An abbreviation is not one: it names whatever
/// object the repository resolves it to at fetch time.
pub fn is_full_commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `(url, commit)` when `source` is `git+<url>#<full commit id>`, the only
/// source form that may build (§3.1). The commit is lowercased.
pub fn git_commit_source(source: &str) -> Option<(&str, String)> {
    let (url, reference) = source.strip_prefix("git+")?.split_once('#')?;
    (!url.trim().is_empty() && is_full_commit_id(reference))
        .then(|| (url, reference.to_ascii_lowercase()))
}

/// What a host recorded about the build that produced an installed plugin
/// (§3.6). Kept on the `plugins` row and mirrored into a host-owned witness
/// file, which the loader checks the row back against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginBuildRecord {
    /// The `git+` source as given, with any URL credentials removed.
    pub source: String,
    /// The verified commit object id.
    pub commit: String,
    /// The fetch argv exactly as run, when the manifest declared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<Vec<String>>,
    /// The build argv exactly as run.
    pub command: Vec<String>,
    /// Each declared program and the canonical path it resolved to.
    #[serde(default)]
    pub programs: Vec<PluginBuildProgram>,
    /// The toolchain roots shown at consent and readable to the build.
    #[serde(default)]
    pub toolchain_roots: Vec<String>,
    /// [`PLUGIN_BUILD_PROFILE_LINUX`] or [`PLUGIN_BUILD_PROFILE_MACOS`].
    pub profile: String,
    /// The Landlock ABI the Linux `fetch` phase confined TCP with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landlock_abi: Option<i64>,
    /// Each installed output.
    pub outputs: Vec<PluginBuildOutputRecord>,
    /// `sha256:<hex>` over the outputs ([`artifact_digest_preimage`]).
    pub artifact_digest: String,
    pub consent: PluginBuildConsent,
    /// The capped build log this host kept.
    pub log: String,
}

/// One `spec.build.programs` entry as resolved at consent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginBuildProgram {
    pub name: String,
    pub path: String,
}

/// One installed build output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginBuildOutputRecord {
    /// Relative to the plugin root.
    pub to: String,
    /// Permission bits as installed (special bits already cleared).
    pub mode: u32,
    /// Hex SHA-256 of the file.
    pub sha256: String,
}

/// Who consented to a build, and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginBuildConsent {
    /// RFC 3339 time of the consenting command.
    pub at: String,
    pub os_user: String,
    pub orbit_version: String,
    /// The literal consent: [`PLUGIN_BUILD_CONSENT_FLAG`].
    pub flag: String,
}

/// The bytes the artifact digest hashes (§3.6): the domain line, then one
/// `<to>\0<mode as octal>\0<sha256 hex>\n` line per output sorted by `to`,
/// so the value is independent of declaration and copy order.
pub fn artifact_digest_preimage(outputs: &[PluginBuildOutputRecord]) -> String {
    let mut sorted: Vec<&PluginBuildOutputRecord> = outputs.iter().collect();
    sorted.sort_by(|left, right| left.to.cmp(&right.to));
    let mut preimage = format!("{PLUGIN_BUILD_DIGEST_DOMAIN}\n");
    for output in sorted {
        preimage.push_str(&format!(
            "{}\0{:o}\0{}\n",
            output.to, output.mode, output.sha256
        ));
    }
    preimage
}
