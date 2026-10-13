//! One required validation command, run the way both validation steps run it
//! [ORB-13915], in an environment resolved independently of the launcher and
//! classified when it fails for lack of a tool [ORB-13987].
//!
//! `claim_validate` and `candidate_validate` share this runner, so they see the
//! same shell, environment, timeout, output capture and failure text. The
//! environment is [`RuntimeHost::validation_subprocess_environment`]: PATH
//! and toolchain locators from the owner's login shell (or configuration)
//! over the allowlisted agent environment. Which source decided PATH is
//! recorded on every run.
//!
//! A command that fails because a tool is missing — shell exit 127, a
//! `command not found`/`not found` diagnostic, `make`'s `Error 127`, a missing
//! cargo subcommand, or a guardrail's "`<tool>` is required … install" line
//! for a tool absent from the validation PATH — says nothing about the
//! candidate. Its failure carries the typed
//! [`VALIDATION_ENVIRONMENT_MARKER`] with the PATH and the missing tool, so
//! recovery, the failure handoff and claim settlement treat it as the host's
//! problem rather than the code's.
//!
//! A supervisor stop at the output capture limit or by a signal also says
//! nothing about the candidate. Its structured execution metadata supplies the
//! cause and carries the same validation-environment marker. Child output,
//! including printed supervision diagnostics, cannot establish a stop. The
//! refusal still names the first failure the captured output shows, so a stop
//! caused by a flood of identical failures says what they were [ORB-15870].
//!
//! On Linux a command builds in its own Cargo target directory,
//! [`host_validation_target`] outside its writable checkout. A managed
//! Linux sandbox binds the checkout's `target/` at the stable build mount, and
//! the compiler-cache wrapper rewrites paths such as `CARGO_BIN_EXE_*` onto
//! that mount for what it compiles there. Cargo's freshness check never sees
//! the rewrite, so a host run in the shared `target/` reused test binaries
//! that spawn paths existing only inside the sandbox. No sandboxed step builds
//! into this directory: even custom targets under the checkout can contain
//! rewritten source paths after recovery [F2026-10-342]. Host-owned state and
//! a canonical source-path key isolate those artifacts without deleting them;
//! dependencies still come back through the compiler cache.
//!
//! Captured output keeps a head and a tail of each stream. A long stdout
//! cannot drop the stderr failure before that classification, and the end of
//! the recorded text still holds it. The failures a failed run names are read
//! from the complete streams first ([`RequiredCommandRun::failures`]), so a
//! failure in the dropped middle still counts [ORB-15375].
//! Host coverage refusals are also read from the complete streams, even on
//! nonzero exits: known-red failed tests cannot waive a skipped path.
//!
//! A command that exits zero after printing a `DEFERRED:` notice returned
//! without running a sandbox-confined path, because the host it ran on could
//! not apply the sandbox. Both validation steps run on the owner, outside any
//! agent sandbox, so a deferral there means the path was never executed
//! before delivery. [`RequiredCommandRun::deferral_failure`] refuses that
//! pass as the validation environment's failure. A command replayed for an
//! implementer's Bubblewrap deferral must also report executed tests
//! ([`RequiredCommandRun::replay_failure`]) [ORB-15287].
//! Replay coverage is checked before accepting known-red failures too.
//!
//! A command may also say what it ran [ORB-15131]. Every run gets
//! [`VALIDATION_SUMMARY_ENV`], naming a fresh file outside the checkout; a
//! command that writes a [`ValidationSummary`] there reports the selection it
//! tested and how many tests executed. A run given a selection gets it in
//! [`VALIDATION_SELECTION_ENV`] and must test exactly that, so a base rerun
//! of a diff-selecting command (`make ci-test-affected`) tests what the
//! candidate run tested rather than the base's own, empty, diff.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_common::text::{ceil_char_boundary, floor_char_boundary};
use orbit_exec::{
    EnvironmentMode, ExecRequest, NoSandbox, StdinMode, ValidationEnvironment, program_on_path,
    run_process_supervised,
};
use orbit_types::workflow::{
    BaselineRedFailures, HOST_TEST_DEFERRED_PREFIX, HostEvidenceRefusal, HostSandboxCommand,
    SEATBELT_DEFERRAL_PREFIX, VALIDATION_ENVIRONMENT_MARKER, judge_host_test_output,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::base_claim::{failure_identities, normalize_output};

/// The file a required command may write its [`ValidationSummary`] to.
pub(crate) const VALIDATION_SUMMARY_ENV: &str = "ORBIT_VALIDATION_SUMMARY";
/// A selection, as a candidate run's summary reported it, that the command
/// must test instead of choosing its own.
pub(crate) const VALIDATION_SELECTION_ENV: &str = "ORBIT_VALIDATION_SELECTION";
/// Host-owned Cargo output, separate from every sandbox-writable target.
/// Key by the canonical *source* path: Cargo freshness alone cannot detect
/// compiler-wrapper rewrites of embedded paths, or a different source checkout.
/// The same checkout's repair/retry/resume can safely reuse its host output.
pub(super) fn host_validation_target(checkout: &Path) -> Result<PathBuf, OrbitError> {
    let source = checkout.canonicalize()?;
    let common = orbit_common::fs::git::git_common_dir(&source)?;
    let primary = common
        .parent()
        .filter(|_| common.file_name().is_some_and(|name| name == ".git"))
        .ok_or_else(|| {
            OrbitError::Execution("host validation needs a primary Git checkout".into())
        })?;
    let key = sha256_hex(source.as_os_str().as_encoded_bytes());
    let mut target = primary.to_path_buf();
    // These are host state, never global compiler/download caches. Refuse
    // redirects rather than creating output through a sandbox-writable symlink.
    for component in [".orbit", "state", "host-validation-targets", &key] {
        target.push(component);
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(OrbitError::PolicyDenied(format!(
                    "host validation target component '{}' must be a directory, not a redirect",
                    target.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match orbit_common::fs::io::create_private_dir(&target) {
                    Ok(()) => {}
                    // Another host command may have created the shared parent.
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let metadata = std::fs::symlink_metadata(&target)?;
                        if !metadata.is_dir() || metadata.file_type().is_symlink() {
                            return Err(OrbitError::PolicyDenied(format!(
                                "host validation target '{}' became a redirect",
                                target.display()
                            )));
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(target)
}

/// Ceiling for one required validation command. Long enough for a real
/// repository check suite, short enough that a wedged command settles the
/// step instead of holding it open indefinitely.
pub(super) const VALIDATION_TIMEOUT_MS: u64 = 45 * 60 * 1000;
/// Captured output kept per command, split across stdout and stderr. The log
/// is reader evidence, not a build log archive, so a runaway command cannot
/// balloon the task bundle. See [`capture`].
const MAX_CAPTURED_OUTPUT_BYTES: usize = 256 * 1024;
/// Diagnostic messages carry summaries and artifact references, never logs.
pub(super) const MAX_VALIDATION_MESSAGE_BYTES: usize = 16 * 1024;

/// Bound a diagnostic field on a UTF-8 boundary, leaving room for evidence.
pub(super) fn bounded_validation_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    format!(
        "{}…",
        &text[..floor_char_boundary(text, limit.saturating_sub(3))]
    )
}

/// The line a failed run's captured output ends with, naming the `PATH` it
/// ran under so a missing tool can be diagnosed.
pub(super) const VALIDATION_PATH_LINE: &str = "Required validation PATH=";

/// One required command's captured result on the candidate.
pub(super) struct RequiredCommandRun {
    pub(super) command: String,
    pub(super) exit_code: i32,
    pub(super) timed_out: bool,
    pub(super) passed: bool,
    pub(super) output: String,
    /// Why the complete output cannot count as executed host coverage,
    /// independently of the exit status. Ordinary failed tests stay in
    /// `failures`; skips and sandbox refusals prevent known-red and flaky-test
    /// continuation.
    pub(super) host_output_refusal: Option<HostEvidenceRefusal>,
    /// The first `DEFERRED:` or Seatbelt `SKIP: sandbox-exec cannot apply a
    /// profile:` line the run printed: a test returned without running its
    /// sandbox-confined path. A failed run keeps it too, so its failures are
    /// never read as known red over a path that never ran [ORB-15546].
    pub(super) deferred_notice: Option<String>,
    /// The environment the command ran in.
    pub(super) environment: ValidationEnvironment,
    /// Set when the command failed because a tool was missing.
    pub(super) missing_tool: Option<MissingTool>,
    /// The supervisor's reason for stopping before a conclusive test result.
    pub(super) infrastructure_failure: Option<String>,
    /// The first failure the complete output shows, for a run the supervisor
    /// stopped ([`first_failure_cause`]).
    pub(super) first_failure: Option<String>,
    /// Reruns after a network-inconclusive failure [ORB-14258].
    pub(super) network_retries: u32,
    /// The line showing the failure was the network's, when it still was
    /// after every rerun.
    pub(super) network_evidence: Option<String>,
    /// What the command reported it ran, when it reports that.
    pub(super) summary: Option<ValidationSummary>,
    /// The failures a failed run's complete output names
    /// ([`failure_identities`]), before capture bounded it [ORB-15375].
    /// Empty for a run that passed.
    pub(super) failures: BTreeSet<String>,
    /// The timeout allowance in milliseconds the command ran under [ORB-15350].
    pub(super) timeout_ms: u64,
}

/// What a command reported about its own run in [`VALIDATION_SUMMARY_ENV`]:
/// `{"schema_version": 1, "selection": ..., "tests_run": n | null}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationSummary {
    /// What the command chose to test, in its own terms. Only compared for
    /// equality and handed back through [`VALIDATION_SELECTION_ENV`].
    pub selection: Value,
    /// Tests executed, or `None` when the command could not count them.
    pub tests_run: Option<u64>,
}

impl ValidationSummary {
    fn read(path: &Path) -> Option<Self> {
        #[derive(Deserialize)]
        struct Recorded {
            schema_version: u32,
            selection: Value,
            tests_run: Option<u64>,
        }
        let bytes = std::fs::read(path).ok()?;
        match serde_json::from_slice::<Recorded>(&bytes) {
            Ok(recorded) if recorded.schema_version == 1 && !recorded.selection.is_null() => {
                Some(Self {
                    selection: recorded.selection,
                    tests_run: recorded.tests_run,
                })
            }
            Ok(_) | Err(_) => {
                tracing::warn!(
                    path = %path.display(),
                    "ignoring a required validation summary that is not schema version 1"
                );
                None
            }
        }
    }
}

/// Evidence that a failed command lacked a tool, not a passing candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MissingTool {
    /// The tool's name, when the output names it.
    pub(crate) tool: Option<String>,
    /// The output line (or exit status) that showed it.
    pub(crate) evidence: String,
}

impl RequiredCommandRun {
    /// The refusal both validation steps report for a command that did not
    /// pass, carrying a bounded summary. A missing tool is reported as the
    /// validation environment's failure, with the marker, full searched PATH
    /// and tool. PATH is not bounded: operators need every searched directory.
    pub(super) fn failure(&self, candidate: &str, baseline: &str, logs: &str) -> OrbitError {
        let summary = self.failure_summary(candidate, baseline, logs);
        if let Some(cause) = &self.infrastructure_failure {
            return OrbitError::Execution(format!(
                "{VALIDATION_ENVIRONMENT_MARKER} {summary} Stopped: {}.{} The candidate was not \
                 judged.",
                bounded_validation_text(cause, 1024),
                self.first_failure
                    .as_deref()
                    .map_or_else(String::new, |first| format!(
                        " First failure before the stop: {}",
                        bounded_validation_text(first, 1024)
                    )),
            ));
        }
        let Some(missing) = &self.missing_tool else {
            return OrbitError::Execution(format!(
                "{summary} The required validation did not pass."
            ));
        };
        OrbitError::Execution(format!(
            "{VALIDATION_ENVIRONMENT_MARKER} {summary} {} the validation environment (source: {}, \
             Required validation PATH={}). The candidate was \
             not judged: make the tool available to the owner's login shell or set \
             `workflow.validation_env.path`, then resume the run. Evidence: {}",
            missing.tool.as_deref().map_or_else(
                || "a tool it runs is missing from".to_string(),
                |tool| format!(
                    "required tool `{}` is missing from",
                    bounded_validation_text(tool, 256)
                )
            ),
            self.environment.source.as_str(),
            self.environment.path().unwrap_or("<unset>"),
            bounded_validation_text(&missing.evidence, 1024),
        ))
    }

    /// Mandatory evidence first, with independently bounded fields so even
    /// oversized commands and test identities cannot displace the log paths.
    pub(super) fn failure_summary(&self, candidate: &str, baseline: &str, logs: &str) -> String {
        let failures = BaselineRedFailures::new(self.failures.iter().cloned(), "")
            .describe()
            .unwrap_or_else(|| "no failing test named".to_string());
        format!(
            "required validation '{}' on candidate {} (exit {}{}; {}). {} Base verdict: {}",
            bounded_validation_text(&self.command, 4096),
            bounded_validation_text(candidate, 128),
            self.exit_code,
            if self.timed_out { ", timed out" } else { "" },
            failures,
            bounded_validation_text(logs, 2048),
            bounded_validation_text(baseline, 2048),
        )
    }

    /// Coverage refusals use the same evidence as nonzero exits.
    pub(super) fn coverage_failure_with_evidence(
        &self,
        candidate: &str,
        replay: bool,
        baseline: &str,
        logs: &str,
    ) -> Option<OrbitError> {
        self.coverage_failure(candidate, replay).map(|error| {
            let message = format!(
                "{} {}",
                self.failure_summary(candidate, baseline, logs),
                bounded_validation_text(&error.to_string(), 2048)
            );
            // Preserve policy refusals and the environment marker for
            // recovery and settlement.
            match error {
                OrbitError::PolicyDenied(_) => OrbitError::PolicyDenied(message),
                _ => OrbitError::Execution(message),
            }
        })
    }

    /// The refusal for a command that exited zero but printed a deferral
    /// notice, so a sandbox-confined path never ran on the host that validates
    /// the candidate. Only a `DEFERRED:` line or the Seatbelt guard's
    /// `SKIP: sandbox-exec cannot apply a profile:` line counts [ORB-15409]:
    /// other `SKIP:` and `skipping` lines are common in other tools' output
    /// and stay with the host judge. Typed as the validation environment's
    /// failure, which no recovery repairs: the candidate was not judged.
    pub(super) fn deferral_failure(&self, candidate: &str) -> Option<OrbitError> {
        let notice = self.deferred_notice.as_ref().filter(|_| self.passed)?;
        Some(OrbitError::Execution(format!(
            "{VALIDATION_ENVIRONMENT_MARKER} required validation '{}' exited zero on candidate \
             {candidate} but deferred a sandbox-confined path, so it never ran before delivery \
             (`{notice}`). The candidate was not judged: this host cannot apply the sandbox the \
             path needs. Run the command outside any agent sandbox on a host of the OS that \
             printed the notice, then resume the run.",
            self.command
        )))
    }

    /// The refusal for a replayed deferral [ORB-15287] whose native run did
    /// not report executing a test, so it replayed nothing: the command is
    /// not an affected-test gate, or it selected nothing on this candidate.
    pub(super) fn replay_failure(&self, candidate: &str) -> Option<OrbitError> {
        let executed = self.summary.as_ref().and_then(|summary| summary.tests_run);
        (executed.unwrap_or(0) == 0).then(|| {
            OrbitError::PolicyDenied(format!(
                "required validation '{}' was handed off to replay the sandbox tests an \
                 agent sandbox deferred, but its native run on candidate {candidate} reported \
                 {} through `{VALIDATION_SUMMARY_ENV}`, so it replayed nothing. Only an \
                 affected-test gate that reports what it executed can carry a deferral.",
                self.command,
                executed.map_or_else(
                    || "no executed-test count".to_string(),
                    |_| { "0 executed tests".to_string() }
                )
            ))
        })
    }

    /// Coverage that must be resolved before either a pass or a known-red
    /// continuation: a successful deferral, or a replay that executed nothing.
    /// Missing tools, supervisor stops and timeouts keep their ordinary
    /// failure handling; none can reach a known-red continuation.
    pub(super) fn coverage_failure(&self, candidate: &str, replay: bool) -> Option<OrbitError> {
        if self.missing_tool.is_some() || self.infrastructure_failure.is_some() || self.timed_out {
            return None;
        }
        self.deferral_failure(candidate)
            .or_else(|| replay.then(|| self.replay_failure(candidate)).flatten())
    }

    /// How the environment was resolved, as recorded on logs and step output.
    pub(super) fn environment_record(&self) -> Value {
        environment_record(&self.environment)
    }

    /// `environment` when the command stopped, lacked a tool or deferred a path,
    /// `candidate` when it otherwise failed, `null` when it passed.
    pub(super) fn failure_kind(&self) -> Value {
        if self.infrastructure_failure.is_some() {
            return json!("environment");
        }
        match (&self.missing_tool, self.passed) {
            (_, true) if self.deferred_notice.is_some() => json!("environment"),
            (_, true) => Value::Null,
            (Some(_), false) => json!("environment"),
            (None, false) => json!("candidate"),
        }
    }

    pub(super) fn missing_tool_name(&self) -> Option<&str> {
        self.missing_tool
            .as_ref()
            .and_then(|missing| missing.tool.as_deref())
    }

    /// The selection the command reported testing, for a base rerun.
    pub(super) fn selection(&self) -> Option<&Value> {
        self.summary.as_ref().map(|summary| &summary.selection)
    }
}

/// The recorded shape of a resolved validation environment: its source, PATH
/// and login-shell outcome.
pub(crate) fn environment_record(environment: &ValidationEnvironment) -> Value {
    json!({
        "source": environment.source.as_str(),
        "path": environment.path(),
        "login_shell": environment
            .login_shell
            .as_ref()
            .map(|shell| shell.display().to_string()),
        "login_shell_enabled": environment.login_shell_enabled,
        "login_shell_error": environment.login_shell_error,
        "probe_mode": environment.probe_mode.map(|mode| mode.as_str()),
        "fallback_reason": environment.fallback_reason,
        "config_path": environment.config_path,
        "path_mode": environment.path_mode.as_str(),
        "cargo_target_dir": environment.env.iter()
            .find(|(name, _)| name == "CARGO_TARGET_DIR")
            .map(|(_, value)| value),
    })
}

/// Run one required command in `workspace_path`, handing it `selection`
/// to test when one is given, for at most `timeout_ms` (normally
/// [`VALIDATION_TIMEOUT_MS`]).
pub(super) fn run_required_command<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    command: &str,
    selection: Option<&Value>,
    timeout_ms: u64,
) -> Result<RequiredCommandRun, OrbitError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(OrbitError::PolicyDenied(
            "owner required validation contains an empty command".to_string(),
        ));
    }
    let mut environment = host.validation_subprocess_environment();
    if cfg!(target_os = "linux") {
        environment.env.retain(|(key, _)| key != "CARGO_TARGET_DIR");
        environment.env.push((
            "CARGO_TARGET_DIR".to_string(),
            host_validation_target(workspace_path)?
                .to_string_lossy()
                .into_owned(),
        ));
    }
    let validation_path = environment
        .path()
        .unwrap_or("<unset; /bin/sh uses its default search path>")
        .to_string();
    // Outside the checkout, which must stay clean while the suite runs.
    let summary_dir = tempfile::Builder::new()
        .prefix("orbit-validation-")
        .tempdir()
        .map_err(|error| {
            OrbitError::Execution(format!("create the validation summary directory: {error}"))
        })?;
    let summary_path = summary_dir.path().join("summary.json");
    let mut env = environment
        .env
        .iter()
        .filter(|(key, _)| key != VALIDATION_SUMMARY_ENV && key != VALIDATION_SELECTION_ENV)
        .cloned()
        .collect::<Vec<_>>();
    env.push((
        VALIDATION_SUMMARY_ENV.to_string(),
        summary_path.to_string_lossy().into_owned(),
    ));
    if let Some(selection) = selection {
        env.push((VALIDATION_SELECTION_ENV.to_string(), selection.to_string()));
    }
    let supervised = run_process_supervised(
        &ExecRequest {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), command.to_string()],
            current_dir: Some(workspace_path.to_string_lossy().into_owned()),
            timeout_ms: Some(timeout_ms),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(env),
            debug: false,
        },
        &NoSandbox,
    )?;
    let infrastructure_failure = supervised.stop_cause.map(|cause| cause.to_string());
    let outcome = supervised.result;
    let summary = ValidationSummary::read(&summary_path);
    // Judge the complete streams before `capture` bounds the log. A skip or
    // deferral in the middle of a large output must still refuse a host pass
    // or known-red continuation. Exit status is judged separately by `passed`
    // and `failures`, so ask the host judge about coverage even on a failed
    // run (its ordinary nonzero branch precedes its skip scan).
    let passed = outcome.success && !outcome.timed_out;
    let host_output_refusal = judge_host_test_output(
        &HostSandboxCommand::RequiredValidation(command.to_string()),
        true,
        outcome.timed_out,
        &format!("{}\n{}", outcome.stdout, outcome.stderr),
    )
    .err();
    let deferred_notice = outcome
        .stdout
        .lines()
        .chain(outcome.stderr.lines())
        .find(|line| {
            let line = line.trim_start();
            line.starts_with(HOST_TEST_DEFERRED_PREFIX)
                || line.starts_with(SEATBELT_DEFERRAL_PREFIX)
        })
        .map(|line| line.trim().to_string());
    let failures = if passed {
        BTreeSet::new()
    } else {
        failure_identities(
            &format!("{}\n{}", outcome.stdout, outcome.stderr),
            workspace_path,
        )
    };
    let first_failure = infrastructure_failure
        .as_ref()
        .and_then(|_| first_failure_cause(&format!("{}\n{}", outcome.stdout, outcome.stderr)));
    let mut output = capture(&outcome.stdout, &outcome.stderr);
    let missing_tool = (!passed && !outcome.timed_out)
        .then(|| missing_tool(outcome.exit_code, &output, environment.path().unwrap_or("")))
        .flatten();
    // Include failures below a build script too (e.g. make exits 2 when a
    // guardrail cannot resolve rg), rather than relying on shell exit 127.
    if !outcome.success {
        output.push_str(&format!("\n{VALIDATION_PATH_LINE}{validation_path}"));
    }
    Ok(RequiredCommandRun {
        command: command.to_string(),
        exit_code: outcome
            .exit_code
            .unwrap_or(if outcome.success { 0 } else { -1 }),
        timed_out: outcome.timed_out,
        passed,
        output,
        host_output_refusal,
        deferred_notice,
        environment,
        missing_tool,
        infrastructure_failure,
        first_failure,
        network_retries: 0,
        network_evidence: None,
        summary,
        failures,
        timeout_ms,
    })
}

/// Whether a failed command's exit status and output show a missing tool.
///
/// Name-bearing shell diagnostics come first so the tool is reported; a
/// guardrail line only counts when the tool it names is absent from `path`,
/// so a test that merely prints "… is required" is still a candidate failure.
pub(crate) fn missing_tool(
    exit_code: Option<i32>,
    output: &str,
    path: &str,
) -> Option<MissingTool> {
    struct Patterns {
        not_found: Regex,
        zsh: Regex,
        exec_failed: Regex,
        cargo_subcommand: Regex,
        guardrail: Regex,
        make_127: Regex,
    }
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    let patterns = PATTERNS
        .get_or_init(|| {
            Some(Patterns {
                // bash: line 1: rg: command not found / /bin/sh: 1: rg: not found
                not_found: Regex::new(
                    r#"(?m)^(?:.*?([^\s:'"`]+): command not found|\S*sh: (?:line )?\d+: ([^\s:'"`]+): not found)\s*$"#,
                )
                .ok()?,
                // zsh: command not found: rg
                zsh: Regex::new(r"(?m)command not found: (\S+)\s*$").ok()?,
                // make: rg: No such file or directory / env: 'rg': No such …
                exec_failed: Regex::new(
                    r"(?m)^(?:make(?:\[\d+\])?|env|xargs|nohup|exec): '?([^\s:']+)'?: No such file or directory\s*$",
                )
                .ok()?,
                // error: no such command: `nextest`
                cargo_subcommand: Regex::new(r"no such (?:sub)?command: `([^`\s]+)`").ok()?,
                // ci-guardrails: ripgrep (rg) is required; install it before running
                guardrail: Regex::new(
                    r"(?im)^\s*(?:[\w./-]+:\s+)?(?:error:\s+)?([A-Za-z][\w.+-]*)(?:\s+\(([A-Za-z][\w.+-]*)\))?\s+is required\b.*\binstall",
                )
                .ok()?,
                // make: *** [Makefile:3: ci] Error 127
                make_127: Regex::new(r"(?m)^make(?:\[\d+\])?: \*\*\* .*Error 127\s*$").ok()?,
            })
        })
        .as_ref()?;

    let named = |regex: &Regex| {
        regex.captures(output).map(|captures| MissingTool {
            tool: captures
                .iter()
                .skip(1)
                .flatten()
                .next()
                .map(|tool| tool.as_str().to_string()),
            evidence: captures
                .get(0)
                .map_or(String::new(), |line| line.as_str().trim().to_string()),
        })
    };
    if let Some(found) = named(&patterns.not_found)
        .or_else(|| named(&patterns.zsh))
        .or_else(|| named(&patterns.exec_failed))
    {
        return Some(found);
    }
    if let Some(captures) = patterns.cargo_subcommand.captures(output) {
        return Some(MissingTool {
            tool: captures
                .get(1)
                .map(|name| format!("cargo-{}", name.as_str())),
            evidence: captures
                .get(0)
                .map_or(String::new(), |line| line.as_str().trim().to_string()),
        });
    }
    for captures in patterns.guardrail.captures_iter(output) {
        let tool = captures
            .get(2)
            .or_else(|| captures.get(1))
            .map(|tool| tool.as_str().to_string());
        if tool
            .as_deref()
            .is_some_and(|tool| !program_on_path(tool, path))
        {
            return Some(MissingTool {
                tool,
                evidence: captures
                    .get(0)
                    .map_or(String::new(), |line| line.as_str().trim().to_string()),
            });
        }
    }
    if exit_code == Some(127) {
        return Some(MissingTool {
            tool: None,
            evidence: "the shell exited 127 (command not found)".to_string(),
        });
    }
    patterns.make_127.find(output).map(|line| MissingTool {
        tool: None,
        evidence: line.as_str().trim().to_string(),
    })
}

/// The first failure `output` shows, on one line: the first test panic with
/// the message lines after it (libtest and nextest both print
/// `thread '<test>' panicked at <location>:` and then the message), or else
/// the first compiler error with its location.
fn first_failure_cause(output: &str) -> Option<String> {
    const MESSAGE_LINES: usize = 4;
    let lines = output.lines().map(normalize_output).collect::<Vec<_>>();
    let cause = if let Some(at) = lines
        .iter()
        .position(|line| line.starts_with("thread '") && line.contains(" panicked at "))
    {
        let message = lines[at + 1..]
            .iter()
            .take_while(|line| {
                !line.is_empty()
                    && !line.starts_with("note:")
                    && !line.starts_with("stack backtrace")
            })
            .take(MESSAGE_LINES);
        std::iter::once(&lines[at])
            .chain(message)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        let at = lines
            .iter()
            .position(|line| line.starts_with("error: ") || line.starts_with("error["))?;
        let location = lines[at + 1..]
            .iter()
            .take(3)
            .find(|line| line.starts_with("--> "));
        match location {
            Some(location) => format!("{} {location}", lines[at]),
            None => lines[at].clone(),
        }
    };
    Some(bounded_validation_text(&cause, 1024))
}

/// Join what the command wrote, bounded per stream.
///
/// Each non-empty stream gets a share of [`MAX_CAPTURED_OUTPUT_BYTES`]. A
/// stream that fits in its share is kept whole, so a short stderr survives a
/// stdout that would otherwise fill the cap. A stream over its share keeps a
/// head and a tail, and the text says what was omitted. Stderr stays after
/// stdout: `missing_tool` reads this string, and the repair excerpt reads its
/// end, so the failure has to be in the kept tail [ORB-14086].
pub(super) fn capture(stdout: &str, stderr: &str) -> String {
    let stdout = trimmed_stream(stdout);
    let stderr = trimmed_stream(stderr);
    let separator = usize::from(!stdout.is_empty() && !stderr.is_empty());
    if stdout.len() + stderr.len() + separator <= MAX_CAPTURED_OUTPUT_BYTES {
        return join_streams(stdout, stderr);
    }
    let (stdout_budget, stderr_budget) = stream_budgets(stdout.len(), stderr.len());
    join_streams(
        &bound_stream(stdout, stdout_budget, "stdout"),
        &bound_stream(stderr, stderr_budget, "stderr"),
    )
}

/// `text` with trailing whitespace removed, or empty when it is only whitespace.
fn trimmed_stream(text: &str) -> &str {
    if text.trim().is_empty() {
        ""
    } else {
        text.trim_end()
    }
}

/// Byte budgets for the two streams. Their kept content, plus one separator
/// when both are non-empty, stays within [`MAX_CAPTURED_OUTPUT_BYTES`].
/// Truncation notices sit outside that count.
fn stream_budgets(stdout_len: usize, stderr_len: usize) -> (usize, usize) {
    let separator = usize::from(stdout_len > 0 && stderr_len > 0);
    let max = MAX_CAPTURED_OUTPUT_BYTES.saturating_sub(separator);
    if stderr_len == 0 {
        return (max.min(stdout_len), 0);
    }
    if stdout_len == 0 {
        return (0, max.min(stderr_len));
    }
    let half = max / 2;
    if stderr_len <= half {
        (max - stderr_len, stderr_len)
    } else if stdout_len <= half {
        (stdout_len, max - stdout_len)
    } else {
        let stderr_budget = half;
        (max - stderr_budget, stderr_budget)
    }
}

/// Keep `text` when it fits in `budget`. Otherwise keep a head and a tail
/// whose content bytes fit, and say how much was omitted.
fn bound_stream(text: &str, budget: usize, stream: &str) -> String {
    if text.len() <= budget {
        return text.to_string();
    }
    let tail_budget = budget / 2;
    let head_budget = budget - tail_budget;
    let head_end = floor_char_boundary(text, head_budget);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(tail_budget));
    if tail_start <= head_end {
        let end = floor_char_boundary(text, budget.min(text.len()));
        return text[..end].to_string();
    }
    let head = &text[..head_end];
    let tail = &text[tail_start..];
    let omitted = tail_start - head_end;
    let head_len = head.len();
    let tail_len = tail.len();
    let total = text.len();
    let mut bounded = String::with_capacity(head_len + tail_len + 64);
    if !head.is_empty() {
        bounded.push_str(head);
        bounded.push('\n');
    }
    bounded.push_str(&format!(
        "[{stream} truncated: kept {head_len}+{tail_len} of {total} bytes, {omitted} omitted]"
    ));
    if !tail.is_empty() {
        bounded.push('\n');
        bounded.push_str(tail);
    }
    bounded
}

fn join_streams(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => {
            let mut combined = String::with_capacity(stdout.len() + stderr.len() + 1);
            combined.push_str(stdout);
            combined.push('\n');
            combined.push_str(stderr);
            combined
        }
    }
}
