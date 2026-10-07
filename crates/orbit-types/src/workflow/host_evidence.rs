//! Host-run review evidence that a confined agent lane cannot produce
//! [ORB-14478].
//!
//! Tests that exercise Orbit's own sandbox paths need to apply a sandbox: a
//! Seatbelt profile on macOS, Bubblewrap namespaces on Linux. Inside an agent
//! lane, which is already confined, the kernel refuses the nested sandbox, so
//! those tests skip or fail without saying anything about the candidate. A
//! reviewer names such a check as a `host_sandbox_test` requirement with the
//! host OS it needs, and Orbit's own machinery on a host of that OS runs it
//! outside the agent sandbox. One contract serves both hosts: the requirement
//! kind, [`EvidenceHostOs`], the closed command grammar
//! ([`HostSandboxCommand::admit`]) and the output judgement
//! ([`judge_host_test_output`]).

use serde::{Deserialize, Serialize};

/// The host operating system a host-run check must execute on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceHostOs {
    Linux,
    Macos,
}

impl EvidenceHostOs {
    /// This process's host OS, when it is one a check can name.
    pub fn current() -> Option<Self> {
        match std::env::consts::OS {
            "linux" => Some(Self::Linux),
            "macos" => Some(Self::Macos),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
        }
    }

    /// The evidence OS of an admission host OS, when a check can name it.
    pub fn of_host(os: Option<crate::task::HostOs>) -> Option<Self> {
        match os? {
            crate::task::HostOs::Linux => Some(Self::Linux),
            crate::task::HostOs::Macos => Some(Self::Macos),
            crate::task::HostOs::Windows => None,
        }
    }
}

/// Why a host did not produce a passing `host_sandbox_test` result. Every
/// reason except [`Self::TestFailed`] leaves the evidence missing, so the
/// review holds rather than accepts; a failed test is a failed check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostEvidenceReason {
    /// The command contains a character outside the admitted word set: a
    /// shell metacharacter, quote, escape or control character. Never run.
    ShellMetacharacter,
    /// The command names a program or shape outside the allowlist. Never run.
    CommandNotAllowed,
    /// An argument is outside the `cargo test` grammar. Never run.
    ArgumentNotAllowed,
    /// The requirement names another host OS. Never run here.
    OsMismatch,
    /// The checkout is not clean at the held commit. Never run.
    CandidateChanged,
    /// The kernel refused the sandbox the test needs on this host too.
    SandboxUnavailable,
    /// The run passed but a test reported that it skipped or deferred.
    SelfSkipped,
    /// The run passed without executing a single test.
    NoTestsRan,
    /// A tool the command needs is missing from the validation environment.
    ToolMissing,
    /// The command exceeded its time limit.
    TimedOut,
    /// The command could not be started.
    RunFailed,
    /// The command ran and failed.
    TestFailed,
}

impl HostEvidenceReason {
    /// Stable name recorded in certificates, logs and escalations.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShellMetacharacter => "shell_metacharacter",
            Self::CommandNotAllowed => "command_not_allowed",
            Self::ArgumentNotAllowed => "argument_not_allowed",
            Self::OsMismatch => "os_mismatch",
            Self::CandidateChanged => "candidate_changed",
            Self::SandboxUnavailable => "sandbox_unavailable",
            Self::SelfSkipped => "self_skipped",
            Self::NoTestsRan => "no_tests_ran",
            Self::ToolMissing => "tool_missing",
            Self::TimedOut => "timed_out",
            Self::RunFailed => "run_failed",
            Self::TestFailed => "test_failed",
        }
    }

    /// Whether the command was refused before anything ran.
    pub fn refused_before_run(self) -> bool {
        matches!(
            self,
            Self::ShellMetacharacter
                | Self::CommandNotAllowed
                | Self::ArgumentNotAllowed
                | Self::OsMismatch
                | Self::CandidateChanged
        )
    }
}

/// A typed refusal with the detail that explains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEvidenceRefusal {
    pub reason: HostEvidenceReason,
    pub detail: String,
}

impl HostEvidenceRefusal {
    fn new(reason: HostEvidenceReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }
}

/// A `host_sandbox_test` command a host admits to run outside the agent
/// sandbox. Every word is restricted to `[A-Za-z0-9._/:@+=-]`, so the
/// command is the same argv whether or not a shell reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostSandboxCommand {
    /// `cargo test -p <crate> --test <target> [<filter>]`: one integration
    /// test target, optionally filtered.
    CargoTest {
        package: String,
        target: String,
        filter: Option<String>,
    },
    /// One of the owner's captured required validation commands, verbatim.
    /// The leaf's own validation step runs the same command on the host.
    RequiredValidation(String),
}

impl HostSandboxCommand {
    /// Admit `command` or refuse it with a typed reason. The allowlist is
    /// closed: the `cargo test` shape above, or an exact entry of
    /// `required_validation_commands` (the owner's captured list).
    pub fn admit(
        command: &str,
        required_validation_commands: &[String],
    ) -> Result<Self, HostEvidenceRefusal> {
        use HostEvidenceReason as R;
        if let Some(bad) = command.chars().find(|c| !plain_char(*c) && *c != ' ') {
            return Err(HostEvidenceRefusal::new(
                R::ShellMetacharacter,
                format!("`{command}` contains {bad:?}, outside [A-Za-z0-9._/:@+=-] and spaces"),
            ));
        }
        let words = command
            .split(' ')
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();
        if words.is_empty() {
            return Err(HostEvidenceRefusal::new(
                R::CommandNotAllowed,
                "the command is empty",
            ));
        }
        let canonical = words.join(" ");
        if required_validation_commands
            .iter()
            .any(|required| required.trim() == canonical)
        {
            return Ok(Self::RequiredValidation(canonical));
        }
        if words[..2.min(words.len())] != ["cargo", "test"] {
            return Err(HostEvidenceRefusal::new(
                R::CommandNotAllowed,
                format!(
                    "`{canonical}` is neither `cargo test -p <crate> --test <target> [<filter>]` \
                     nor one of the owner's required validation commands"
                ),
            ));
        }
        let mut package = None;
        let mut target = None;
        let mut filter = None;
        let mut rest = words[2..].iter();
        while let Some(word) = rest.next() {
            let slot = match *word {
                "-p" | "--package" => &mut package,
                "--test" => &mut target,
                _ if word.starts_with('-') => {
                    return Err(HostEvidenceRefusal::new(
                        R::ArgumentNotAllowed,
                        format!("option `{word}` is not admitted"),
                    ));
                }
                _ => {
                    if filter.replace(word.to_string()).is_some() {
                        return Err(HostEvidenceRefusal::new(
                            R::ArgumentNotAllowed,
                            "only one test filter is admitted",
                        ));
                    }
                    continue;
                }
            };
            let Some(value) = rest.next().filter(|value| !value.starts_with('-')) else {
                return Err(HostEvidenceRefusal::new(
                    R::ArgumentNotAllowed,
                    format!("`{word}` needs a value"),
                ));
            };
            if slot.replace(value.to_string()).is_some() {
                return Err(HostEvidenceRefusal::new(
                    R::ArgumentNotAllowed,
                    format!("`{word}` is given twice"),
                ));
            }
        }
        match (package, target) {
            (Some(package), Some(target)) => Ok(Self::CargoTest {
                package,
                target,
                filter,
            }),
            _ => Err(HostEvidenceRefusal::new(
                R::ArgumentNotAllowed,
                "`cargo test` needs both `-p <crate>` and `--test <target>`",
            )),
        }
    }

    /// The command line the host runs. A `cargo test` run adds
    /// `-- --nocapture`, so a test's own skip notice reaches the judgement.
    pub fn host_command(&self) -> String {
        match self {
            Self::CargoTest {
                package,
                target,
                filter,
            } => {
                let mut words = vec!["cargo", "test", "-p", package, "--test", target];
                words.extend(filter.as_deref());
                words.extend(["--", "--nocapture"]);
                words.join(" ")
            }
            Self::RequiredValidation(command) => command.clone(),
        }
    }
}

fn plain_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | ':' | '@' | '+' | '=' | '-')
}

/// Text a sandbox wrapper or a sandbox test prints when this host cannot
/// apply the sandbox at all.
const SANDBOX_UNAVAILABLE_MARKERS: &[&str] = &[
    "sandbox_apply: Operation not permitted",
    "sandbox-exec cannot apply a profile",
    "No permissions to create a new namespace",
];

/// Line prefixes a test prints when it returns without exercising its path.
const SKIP_PREFIXES: &[&str] = &["SKIP:", "DEFERRED:", "skipping:", "skipping "];

/// Judge one host run from its full output. Returns the number of tests the
/// libtest summaries report passed, or why the run is not passing evidence.
/// A `cargo test` run must report at least one passed test; a required
/// validation command need not run libtest at all. Neither may report a
/// skipped or deferred test, or a sandbox the host could not apply.
pub fn judge_host_test_output(
    command: &HostSandboxCommand,
    success: bool,
    timed_out: bool,
    output: &str,
) -> Result<u64, HostEvidenceRefusal> {
    use HostEvidenceReason as R;
    if timed_out {
        return Err(HostEvidenceRefusal::new(
            R::TimedOut,
            "the command exceeded its time limit",
        ));
    }
    if let Some(line) = output.lines().find(|line| {
        SANDBOX_UNAVAILABLE_MARKERS
            .iter()
            .any(|marker| line.contains(marker))
    }) {
        return Err(HostEvidenceRefusal::new(R::SandboxUnavailable, line.trim()));
    }
    if !success {
        return Err(HostEvidenceRefusal::new(
            R::TestFailed,
            output
                .lines()
                .rev()
                .find(|line| line.contains("test result: FAILED"))
                .unwrap_or("the command exited nonzero")
                .trim(),
        ));
    }
    if let Some(line) = output.lines().find(|line| {
        let line = line.trim_start();
        SKIP_PREFIXES.iter().any(|prefix| line.starts_with(prefix))
    }) {
        return Err(HostEvidenceRefusal::new(R::SelfSkipped, line.trim()));
    }
    let passed = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("test result: ok. "))
        .filter_map(|rest| rest.split(' ').next()?.parse::<u64>().ok())
        .sum::<u64>();
    if passed == 0 && matches!(command, HostSandboxCommand::CargoTest { .. }) {
        return Err(HostEvidenceRefusal::new(
            R::NoTestsRan,
            "no libtest summary reports a passed test",
        ));
    }
    Ok(passed)
}

/// One `host_sandbox_test` requirement as the executing host judged it, kept
/// on the review certificate. A passed record names its result and log
/// artifacts, which a claimed leaf's handoff pins for the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostEvidenceRecord {
    pub name: String,
    /// The requirement's command, as the reviewer named it.
    pub command: String,
    /// The command line the host ran, absent when it was refused first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_command: Option<String>,
    pub os: EvidenceHostOs,
    /// The tree the command ran on.
    pub tree: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<HostEvidenceReason>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The result artifact, present only when the run passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// The log artifact, present whenever a log was attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_artifact: Option<String>,
}
