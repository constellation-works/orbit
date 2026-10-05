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

use std::path::Path;
use std::sync::OnceLock;

use orbit_common::OrbitError;
use orbit_common::text::floor_char_boundary;
use orbit_exec::{
    EnvironmentMode, ExecRequest, NoSandbox, StdinMode, ValidationEnvironment, program_on_path,
    run_process,
};
use orbit_types::workflow::VALIDATION_ENVIRONMENT_MARKER;
use regex::Regex;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

/// Ceiling for one required validation command. Long enough for a real
/// repository check suite, short enough that a wedged command settles the
/// step instead of holding it open indefinitely.
const VALIDATION_TIMEOUT_MS: u64 = 45 * 60 * 1000;
/// Captured output kept per command. The log is reader evidence, not a build
/// log archive, so a runaway command cannot balloon the task bundle.
const MAX_CAPTURED_OUTPUT_BYTES: usize = 256 * 1024;

/// One required command's captured result on the candidate.
pub(super) struct RequiredCommandRun {
    pub(super) command: String,
    pub(super) exit_code: i32,
    pub(super) timed_out: bool,
    pub(super) passed: bool,
    pub(super) output: String,
    /// The environment the command ran in.
    pub(super) environment: ValidationEnvironment,
    /// Set when the command failed because a tool was missing.
    pub(super) missing_tool: Option<MissingTool>,
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
    /// pass, carrying its captured output. A missing tool is reported as the
    /// validation environment's failure, with the marker, PATH and tool.
    pub(super) fn failure(&self, candidate: &str) -> OrbitError {
        let Some(missing) = &self.missing_tool else {
            return OrbitError::Execution(format!(
                "required validation '{}' did not pass on candidate {candidate}: {}",
                self.command, self.output
            ));
        };
        OrbitError::Execution(format!(
            "{VALIDATION_ENVIRONMENT_MARKER} required validation '{}' could not run on candidate \
             {candidate}: {} the validation environment (source: {}, PATH={}). The candidate was \
             not judged: make the tool available to the owner's login shell or set \
             `workflow.validation_env.path`, then resume the run. Evidence: {}\n{}",
            self.command,
            missing.tool.as_deref().map_or_else(
                || "a tool it runs is missing from".to_string(),
                |tool| format!("required tool `{tool}` is missing from")
            ),
            self.environment.source.as_str(),
            self.environment.path().unwrap_or("<unset>"),
            missing.evidence,
            self.output
        ))
    }

    /// How the environment was resolved, as recorded on logs and step output.
    pub(super) fn environment_record(&self) -> Value {
        environment_record(&self.environment)
    }

    /// `environment` when the command lacked a tool, `candidate` when it
    /// otherwise failed, `null` when it passed.
    pub(super) fn failure_kind(&self) -> Value {
        match (&self.missing_tool, self.passed) {
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
    })
}

/// Run one required command in `workspace_path`.
pub(super) fn run_required_command<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    command: &str,
) -> Result<RequiredCommandRun, OrbitError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(OrbitError::PolicyDenied(
            "owner required validation contains an empty command".to_string(),
        ));
    }
    let environment = host.validation_subprocess_environment();
    let validation_path = environment
        .path()
        .unwrap_or("<unset; /bin/sh uses its default search path>")
        .to_string();
    let outcome = run_process(
        &ExecRequest {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), command.to_string()],
            current_dir: Some(workspace_path.to_string_lossy().into_owned()),
            timeout_ms: Some(VALIDATION_TIMEOUT_MS),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(environment.env.clone()),
            debug: false,
        },
        &NoSandbox,
    )?;
    let mut output = capture(&outcome.stdout, &outcome.stderr);
    let passed = outcome.success && !outcome.timed_out;
    let missing_tool = (!passed && !outcome.timed_out)
        .then(|| missing_tool(outcome.exit_code, &output, environment.path().unwrap_or("")))
        .flatten();
    // Include failures below a build script too (e.g. make exits 2 when a
    // guardrail cannot resolve rg), rather than relying on shell exit 127.
    if !outcome.success {
        output.push_str(&format!("\nRequired validation PATH={validation_path}"));
    }
    Ok(RequiredCommandRun {
        command: command.to_string(),
        exit_code: outcome
            .exit_code
            .unwrap_or(if outcome.success { 0 } else { -1 }),
        timed_out: outcome.timed_out,
        passed,
        output,
        environment,
        missing_tool,
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

/// Interleave what the command said, bounded. Truncation is reported inside
/// the captured text so a reader never mistakes a clipped log for the whole
/// output.
fn capture(stdout: &str, stderr: &str) -> String {
    let mut combined = String::new();
    if !stdout.trim().is_empty() {
        combined.push_str(stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(stderr.trim_end());
    }
    if combined.len() <= MAX_CAPTURED_OUTPUT_BYTES {
        return combined;
    }
    let cut = floor_char_boundary(&combined, MAX_CAPTURED_OUTPUT_BYTES);
    format!(
        "[truncated to {cut} of {} bytes]\n{}",
        combined.len(),
        &combined[..cut]
    )
}
