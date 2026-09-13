//! Hardened non-interactive Git plumbing shared by the task-publication
//! transport and its read-only consumer.
//!
//! `orbit_common::fs::git::run_git` drives a source checkout with the
//! operator's ambient configuration. Publication instead drives an Orbit-owned
//! private cache: it must never prompt for credentials, read system config, run
//! repository hooks, or let content filters rewrite snapshot bytes, and it needs
//! per-invocation environment (index file, deterministic commit identity). That
//! is a different contract, so it keeps its own runner here.

use std::fs;
use std::path::Path;
use std::process::Command;

use orbit_common::OrbitError;
use orbit_common::fs::io::create_private_dir_all;
use orbit_types::workspace::git_remotes_equivalent;

/// Highest-precedence attributes for an Orbit-owned cache. Unsets every
/// conversion Git would otherwise apply from a published `.gitattributes`,
/// `$GIT_DIR/info/attributes` leftover, or ambient attributes file.
///
/// `core.attributesFile=/dev/null` only disables the *global* attributes
/// file. `attr.tree` (empty or invalid) still leaves worktree attributes
/// active on Git 2.43 during `add` and `checkout`. `info/attributes` is the
/// layer that actually wins over `artifacts/files/.gitattributes`.
const LITERAL_ATTRIBUTES: &str =
    "* -text -eol -crlf -ident -filter -diff -merge -working-tree-encoding\n";

/// Result of a Git invocation that is allowed to fail.
pub(super) struct GitAttempt {
    pub(super) success: bool,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

/// Runs `git` with publication-safe defaults and a caller-supplied error label.
pub(super) struct GitRunner<'a> {
    label: &'a str,
    env: Vec<(&'a str, String)>,
}

impl<'a> GitRunner<'a> {
    pub(super) fn new(label: &'a str) -> Self {
        Self {
            label,
            env: Vec::new(),
        }
    }

    /// Additional environment applied to every invocation of this runner.
    pub(super) fn with_env(mut self, env: Vec<(&'a str, String)>) -> Self {
        self.env = env;
        self
    }

    /// Run `git`, mapping a non-zero exit to an error with redacted arguments.
    pub(super) fn run(&self, args: &[&str]) -> Result<String, OrbitError> {
        let attempt = self.try_run(args)?;
        if !attempt.success {
            return Err(self.error(format!(
                "git {} failed: {}",
                redact_args(args),
                attempt.stderr.trim()
            )));
        }
        Ok(attempt.stdout.trim().to_string())
    }

    /// Run `git` and return the outcome even when the command exits non-zero.
    pub(super) fn try_run(&self, args: &[&str]) -> Result<GitAttempt, OrbitError> {
        if let Some(git_dir) = git_dir_from_args(args) {
            isolate_git_dir(git_dir)?;
        }
        let mut command = Command::new("git");
        command
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.excludesFile=/dev/null",
                "-c",
                "core.attributesFile=/dev/null",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.safecrlf=false",
            ])
            .args(args);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        // Isolation wins over caller env and the ambient process: publication
        // must not read operator Git config or honor GIT_CONFIG_COUNT filters.
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GCM_INTERACTIVE", "never")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_ATTR_SOURCE");
        let output = command.output().map_err(|error| {
            OrbitError::Execution(format!(
                "{} failed to run `git {}`: {error}",
                self.label,
                redact_args(args)
            ))
        })?;
        Ok(GitAttempt {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Single Git parent of `commit`. Publication history must stay linear, so
    /// a merge commit is an error rather than something to pick a side of.
    pub(super) fn single_parent(
        &self,
        git_dir: &str,
        commit: &str,
    ) -> Result<Option<String>, OrbitError> {
        let parents = self.run(&[
            "--git-dir",
            git_dir,
            "rev-list",
            "--parents",
            "-n",
            "1",
            commit,
        ])?;
        let mut tokens = parents.split_whitespace();
        let _ = tokens.next();
        let parent = tokens.next().map(str::to_ascii_lowercase);
        if tokens.next().is_some() {
            return Err(self.error(format!(
                "publication history must be linear; commit {commit} has multiple Git parents"
            )));
        }
        Ok(parent)
    }

    pub(super) fn error(&self, message: impl Into<String>) -> OrbitError {
        OrbitError::InvalidInput(format!("{}: {}", self.label, message.into()))
    }
}

fn git_dir_from_args<'a>(args: &'a [&'a str]) -> Option<&'a Path> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if let Some(path) = arg.strip_prefix("--git-dir=") {
            return Some(Path::new(path));
        }
        if *arg == "--git-dir" {
            return args.next().map(Path::new);
        }
    }
    None
}

fn isolate_git_dir(git_dir: &Path) -> Result<(), OrbitError> {
    if !git_dir.is_dir() {
        return Ok(());
    }
    let info = git_dir.join("info");
    create_private_dir_all(&info).map_err(|error| OrbitError::from_write_io(&info, error))?;
    let path = info.join("attributes");
    fs::write(&path, LITERAL_ATTRIBUTES)
        .map_err(|error| OrbitError::from_write_io(&path, error))?;
    Ok(())
}

/// Absolute paths are local filesystem detail; keep them out of error text.
fn redact_args(args: &[&str]) -> String {
    args.iter()
        .filter(|arg| !Path::new(arg).is_absolute())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Report a binding field that disagrees with the observed publication.
pub(super) fn field_mismatch(
    label: &str,
    field: &str,
    expected: &str,
    observed: &str,
) -> Result<(), OrbitError> {
    if expected == observed {
        Ok(())
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{label}: {field} mismatch: expected '{expected}', observed '{observed}'"
        )))
    }
}

pub(super) fn short_branch(branch: &str) -> &str {
    branch.strip_prefix("refs/heads/").unwrap_or(branch)
}

/// A publication remote must never carry embedded credentials.
pub(super) fn remote_has_password(remote: &str) -> bool {
    let Some(rest) = remote.split_once("://").map(|(_, rest)| rest) else {
        return false;
    };
    rest.split_once('@')
        .is_some_and(|(userinfo, _)| userinfo.contains(':'))
}

/// Compare a configured remote with one recorded in an Orbit-owned cache.
pub(super) fn remotes_match(expected: &str, observed: &str) -> bool {
    if expected == observed {
        return true;
    }
    if normalize_local_remote(expected) == normalize_local_remote(observed) {
        return true;
    }
    git_remotes_equivalent(expected, observed).unwrap_or(false)
}

fn normalize_local_remote(remote: &str) -> String {
    let trimmed = remote.trim();
    let path = trimmed.strip_prefix("file://").unwrap_or(trimmed);
    Path::new(path)
        .canonicalize()
        .map(|canonical| canonical.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

pub(super) fn path_str<'a>(path: &'a Path, label: &str) -> Result<&'a str, OrbitError> {
    path.to_str().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "{label}: path '{}' is not valid UTF-8",
            path.display()
        ))
    })
}
