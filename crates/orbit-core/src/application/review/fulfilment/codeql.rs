//! The `codeql` side of owner fulfilment: admit the local CodeQL script's
//! command, check the held commit out into a standalone shallow repository,
//! run the script there confined by Bubblewrap, and judge its SARIF.

use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Child, Stdio};

use orbit_common::OrbitError;
use orbit_common::fs::git::{
    GIT_CHECKOUT_TIMEOUT, GIT_LOCAL_TIMEOUT, GIT_REMOTE_TIMEOUT, run_git_within,
};
use orbit_common::text::ceil_char_boundary;
#[cfg(target_os = "linux")]
use orbit_exec::{EnvironmentMode, ExecRequest, Sandbox, StdinMode, run_process};
#[cfg(target_os = "linux")]
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::workflow::ReviewEvidenceRequirement;
use serde_json::{Value, json};

use super::{EvidenceRun, FulfilmentRefusal};
use crate::OrbitRuntime;
use crate::application::task::recovery_checkout_path;

/// The only `codeql` command a fulfilment runs, relative to the held checkout.
const CODEQL_SCRIPT: &str = "scripts/codeql-rust-local.sh";
/// Ceiling for one CodeQL run: toolchain preparation, extraction, analysis.
const CODEQL_TIMEOUT_MS: u64 = 3 * 60 * 60 * 1000;
/// Captured output kept per stream in the log artifact.
const MAX_CAPTURED_STREAM_BYTES: usize = 128 * 1024;
/// The script's own markers: where it keeps this run, and that analysis ran.
const RUN_DIRECTORY_MARKER: &str = "codeql-rust-local: run directory: ";
const ANALYSIS_COMPLETED_MARKER: &str = "codeql-rust-local: analysis completed";

/// Bubblewrap confines candidate-controlled CodeQL scripts to the disposable
/// evidence checkout. The checkout is the only writable task-controlled path;
/// Cargo's ambient download caches are remounted read-only because this script
/// uses a run-local `CARGO_HOME`.
#[cfg(target_os = "linux")]
struct EvidenceCodeqlSandbox {
    checkout: PathBuf,
    profile: ResolvedFsProfile,
}

#[cfg(target_os = "linux")]
impl EvidenceCodeqlSandbox {
    fn new(checkout: &Path) -> Self {
        let checkout = checkout.to_path_buf();
        let mut modify = vec![checkout.display().to_string()];
        for cargo_home in cargo_home_candidates() {
            for relative in ["registry", "git"] {
                let path = cargo_home.join(relative);
                if path.exists() {
                    modify.push(format!("!{}/**", path.display()));
                }
            }
            for relative in [".package-cache", ".package-cache-mutate"] {
                let path = cargo_home.join(relative);
                if path.exists() {
                    modify.push(format!("!{}", path.display()));
                }
            }
        }
        Self {
            profile: ResolvedFsProfile {
                name: "review-evidence-fulfilment".to_string(),
                read: Vec::new(),
                modify,
            },
            checkout,
        }
    }
}

#[cfg(target_os = "linux")]
fn cargo_home_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("CARGO_HOME").filter(|home| !home.is_empty()) {
        candidates.push(PathBuf::from(home));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        candidates.push(PathBuf::from(home).join(".cargo"));
    }
    let current_dir = std::env::current_dir().ok();
    let mut canonical = std::collections::BTreeSet::new();
    candidates
        .into_iter()
        .filter_map(|path| {
            let absolute = if path.is_absolute() {
                path
            } else {
                current_dir.as_ref()?.join(path)
            };
            let path = absolute.canonicalize().ok()?;
            path.is_dir().then_some(path)
        })
        .filter(|path| canonical.insert(path.clone()))
        .collect()
}

#[cfg(target_os = "linux")]
impl Sandbox for EvidenceCodeqlSandbox {
    fn validate(&self, request: &ExecRequest) -> Result<(), OrbitError> {
        if request.current_dir.as_deref() != Some(self.checkout.to_string_lossy().as_ref()) {
            return Err(OrbitError::PolicyDenied(
                "CodeQL must run from its evidence checkout".to_string(),
            ));
        }
        if request.program != self.checkout.join(CODEQL_SCRIPT).to_string_lossy().as_ref() {
            return Err(OrbitError::PolicyDenied(
                "CodeQL program must be the admitted checkout script".to_string(),
            ));
        }
        Ok(())
    }

    fn spawn(&self, request: &ExecRequest) -> Result<Child, OrbitError> {
        let environment = match &request.environment_mode {
            EnvironmentMode::ClearAndSet(environment) => environment.clone(),
            EnvironmentMode::Inherit => {
                return Err(OrbitError::PolicyDenied(
                    "CodeQL sandbox requires an explicit environment".to_string(),
                ));
            }
        };
        let stdin = match &request.stdin_mode {
            StdinMode::Inherit => Stdio::inherit(),
            StdinMode::Null => Stdio::null(),
            StdinMode::Bytes(_) => Stdio::piped(),
        };
        let mut plan = orbit_exec::compile_linux_bwrap_argv(
            &self.profile,
            &request.program,
            &request.args,
            Some(&self.checkout),
            true,
        )?;
        if !plan.dropped_grants.is_empty() {
            return Err(OrbitError::PolicyDenied(format!(
                "CodeQL sandbox could not enforce writable checkout grants: {:?}",
                plan.dropped_grants
            )));
        }
        if let Some(guard) = plan.take_post_run_guard() {
            // This profile has only exact subtree rules whose writable roots
            // exist, so no post-run check is expected. Refuse instead of
            // silently dropping a future policy boundary.
            return Err(OrbitError::PolicyDenied(format!(
                "CodeQL sandbox unexpectedly needs a post-run guard: {guard:?}"
            )));
        }
        let child = orbit_exec::spawn_under_linux_bwrap(orbit_exec::LinuxBwrapSpawnRequest {
            plan: &plan,
            env: &environment,
            cwd: Some(&self.checkout),
            stdin,
            stdout: Stdio::piped(),
            stderr: Stdio::piped(),
        })?;
        Ok(child)
    }
}

impl OrbitRuntime {
    /// Check `commit` out into a standalone shallow repository fetched from
    /// the owner's checkout, replacing a leftover of the same run. Its Git
    /// metadata lives inside it, unlike a linked worktree's, whose gitdir is
    /// in the owner's `.git`: the confined script resolves `HEAD` and
    /// `ls-files` from the one writable tree it is given, and never needs,
    /// or writes, the owner's repository, which the sandbox does not show
    /// when it sits under the `/tmp` the sandbox replaces.
    pub(super) fn create_evidence_checkout(
        &self,
        checkout_id: &str,
        commit: &str,
    ) -> Result<PathBuf, OrbitError> {
        self.remove_evidence_checkout(checkout_id)?;
        let path = recovery_checkout_path(&self.paths().state_dir, checkout_id)?;
        std::fs::create_dir_all(&path).map_err(|error| {
            OrbitError::Execution(format!(
                "create evidence checkout {}: {error}",
                path.display()
            ))
        })?;
        let git_dir = format!("--git-dir={}", path.join(".git").display());
        let source = self.paths().repo_root.to_string_lossy().into_owned();
        // Every command after `init` names the new repository explicitly, so
        // a failed `init` can never fall through to an enclosing one.
        for (args, deadline) in [
            (vec!["init", "--quiet", "--template="], GIT_LOCAL_TIMEOUT),
            (
                vec![
                    &git_dir,
                    "fetch",
                    "--quiet",
                    "--depth=1",
                    "--no-tags",
                    "--end-of-options",
                    &source,
                    commit,
                ],
                GIT_REMOTE_TIMEOUT,
            ),
            (
                vec![
                    &git_dir,
                    "-c",
                    "core.hooksPath=/dev/null",
                    "checkout",
                    "--quiet",
                    "--detach",
                    commit,
                ],
                GIT_CHECKOUT_TIMEOUT,
            ),
        ] {
            let output = run_git_within(&path, &args, deadline)?;
            if !output.success {
                return Err(OrbitError::Execution(format!(
                    "prepare evidence checkout {} at {commit}: git {}: {}",
                    path.display(),
                    args.join(" "),
                    output.stderr.trim()
                )));
            }
        }
        path.canonicalize().map_err(|error| {
            OrbitError::Execution(format!(
                "resolve evidence checkout {}: {error}",
                path.display()
            ))
        })
    }

    /// Run one admitted CodeQL command in `checkout` and judge its result.
    pub(super) fn run_codeql(
        &self,
        checkout: &Path,
        requirement: &ReviewEvidenceRequirement,
        args: Vec<String>,
    ) -> EvidenceRun {
        let mut run = EvidenceRun::new(requirement);
        let scratch = checkout.join(".orbit/tmp");
        if let Err(error) = std::fs::create_dir_all(&scratch) {
            run.refusal = Some(FulfilmentRefusal::CommandFailed);
            run.detail = format!("prepare scratch {}: {error}", scratch.display());
            return run;
        }
        let mut env = self.validation_environment().env;
        env.retain(|(name, _)| name != "ORBIT_SCRATCH_DIR");
        env.push((
            "ORBIT_SCRATCH_DIR".to_string(),
            scratch.to_string_lossy().into_owned(),
        ));
        #[cfg(target_os = "linux")]
        let outcome = {
            let request = ExecRequest {
                program: checkout.join(CODEQL_SCRIPT).to_string_lossy().into_owned(),
                args,
                current_dir: Some(checkout.to_string_lossy().into_owned()),
                timeout_ms: Some(CODEQL_TIMEOUT_MS),
                stdin_mode: StdinMode::Null,
                environment_mode: EnvironmentMode::ClearAndSet(env),
                debug: false,
            };
            run_process(&request, &EvidenceCodeqlSandbox::new(checkout))
        };
        #[cfg(not(target_os = "linux"))]
        let outcome: Result<orbit_types::tool::ExecutionResult, OrbitError> = {
            let _ = (args, env);
            Err(OrbitError::PolicyDenied(
                "CodeQL fulfilment requires Linux Bubblewrap".to_string(),
            ))
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                run.refusal = Some(FulfilmentRefusal::CommandFailed);
                run.detail = error.to_string();
                return run;
            }
        };
        run.exit_code = outcome.exit_code;
        run.timed_out = outcome.timed_out;
        run.log.extend([
            ("stdout".to_string(), json!(tail(&outcome.stdout))),
            ("stderr".to_string(), json!(tail(&outcome.stderr))),
        ]);
        let output = format!("{}\n{}", outcome.stdout, outcome.stderr);
        let problem = |pattern: &str| {
            output
                .lines()
                .find(|line| line.contains(pattern))
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        let judged = if outcome.timed_out {
            Err((
                FulfilmentRefusal::TimedOut,
                format!("exceeded {CODEQL_TIMEOUT_MS} ms"),
            ))
        } else if outcome.exit_code == Some(3) {
            Err((
                FulfilmentRefusal::PlatformRefused,
                problem("codeql-rust-local:"),
            ))
        } else if !outcome.success && output.contains("is required on PATH") {
            Err((
                FulfilmentRefusal::ToolMissing,
                problem("is required on PATH"),
            ))
        } else if !outcome.success && output.contains("incomplete Rust extraction") {
            Err((
                FulfilmentRefusal::AnalysisIncomplete,
                problem("incomplete Rust extraction"),
            ))
        } else if !outcome.success {
            Err((
                FulfilmentRefusal::CommandFailed,
                format!(
                    "exit {:?}: {}",
                    outcome.exit_code,
                    problem("codeql-rust-local:")
                ),
            ))
        } else if !output.contains(ANALYSIS_COMPLETED_MARKER) {
            Err((
                FulfilmentRefusal::AnalysisIncomplete,
                "the run exited zero without reporting completed analysis".to_string(),
            ))
        } else {
            judge_sarif(checkout, &output)
        };
        match judged {
            Ok(sarif) => {
                run.log.insert("sarif".to_string(), sarif);
            }
            Err((refusal, detail)) => {
                run.log.insert("sarif".to_string(), Value::Null);
                run.refusal = Some(refusal);
                run.detail = detail;
            }
        }
        run
    }
}

/// The script's arguments when `command` is exactly the CodeQL script with
/// its own options and one query selector. Every word is restricted to
/// characters with no shell meaning, so no hold can name another program.
pub(super) fn admitted_codeql_args(command: &str) -> Result<Vec<String>, String> {
    let refuse = |why: &str| {
        Err(format!(
            "`{command}` is not an admitted CodeQL command: {why}"
        ))
    };
    let plain = |word: &str| {
        !word.is_empty()
            && word.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | ':' | '@' | '+' | '-')
            })
    };
    if command.chars().any(|c| c.is_whitespace() && c != ' ') {
        return refuse("only single spaces may separate words");
    }
    let mut words = command.split(' ').filter(|word| !word.is_empty());
    if words.next() != Some(CODEQL_SCRIPT) {
        return refuse(&format!("it must run `{CODEQL_SCRIPT}`"));
    }
    let mut args = Vec::new();
    let mut selector = false;
    while let Some(word) = words.next() {
        if !plain(word) {
            return refuse(&format!(
                "`{word}` has characters outside [A-Za-z0-9._/:@+-]"
            ));
        }
        match word {
            "--ram" | "--toolchain" => {
                let Some(value) = words
                    .next()
                    .filter(|value| plain(value) && !value.starts_with('-'))
                else {
                    return refuse(&format!("`{word}` needs a value"));
                };
                args.push(word.to_string());
                args.push(value.to_string());
            }
            _ if word.starts_with('-') => {
                return refuse(&format!("option `{word}` is not admitted"));
            }
            _ if selector => return refuse("it names more than one query selector"),
            _ => {
                selector = true;
                args.push(word.to_string());
            }
        }
    }
    if !selector {
        return refuse("it names no query selector");
    }
    Ok(args)
}

/// The run's SARIF, from the run directory the script printed inside
/// `checkout`. Any result is refused: a fresh review must judge findings.
fn judge_sarif(checkout: &Path, output: &str) -> Result<Value, (FulfilmentRefusal, String)> {
    let incomplete = |detail: String| (FulfilmentRefusal::AnalysisIncomplete, detail);
    let run_dir = output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(RUN_DIRECTORY_MARKER))
        .map(|path| PathBuf::from(path.trim()))
        .ok_or_else(|| incomplete("the run printed no run directory".to_string()))?;
    let canonical_checkout = checkout
        .canonicalize()
        .map_err(|error| incomplete(error.to_string()))?;
    let sarif_path = run_dir
        .join("results.sarif")
        .canonicalize()
        .map_err(|error| {
            incomplete(format!(
                "results.sarif under {}: {error}",
                run_dir.display()
            ))
        })?;
    if !sarif_path.starts_with(&canonical_checkout) {
        return Err(incomplete(format!(
            "{} is outside the evidence checkout",
            sarif_path.display()
        )));
    }
    let unreadable = |detail: String| (FulfilmentRefusal::ResultsUnreadable, detail);
    let bytes = std::fs::read(&sarif_path).map_err(|error| unreadable(error.to_string()))?;
    let sarif: Value =
        serde_json::from_slice(&bytes).map_err(|error| unreadable(error.to_string()))?;
    let runs = sarif
        .get("runs")
        .and_then(Value::as_array)
        .filter(|runs| !runs.is_empty())
        .ok_or_else(|| unreadable("SARIF has no runs".to_string()))?;
    let mut results = 0usize;
    let mut rules = std::collections::BTreeSet::new();
    for run in runs {
        let found = run
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| unreadable("a SARIF run has no results array".to_string()))?;
        results += found.len();
        rules.extend(
            found
                .iter()
                .filter_map(|result| result.get("ruleId").and_then(Value::as_str))
                .map(str::to_string),
        );
    }
    let summary = json!({"runs": runs.len(), "results": results, "rules": rules});
    if results > 0 {
        return Err((
            FulfilmentRefusal::FindingsReported,
            format!(
                "{results} result(s) for rule(s) {}",
                rules.into_iter().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    Ok(summary)
}

/// The last [`MAX_CAPTURED_STREAM_BYTES`] of a stream.
fn tail(stream: &str) -> String {
    if stream.len() <= MAX_CAPTURED_STREAM_BYTES {
        return stream.to_string();
    }
    let start = ceil_char_boundary(stream, stream.len() - MAX_CAPTURED_STREAM_BYTES);
    format!("[… truncated]\n{}", &stream[start..])
}
