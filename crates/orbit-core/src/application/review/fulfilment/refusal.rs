//! Why a fulfilment did not attach a result.

use orbit_types::workflow::HostEvidenceReason;

/// Why a fulfilment did not attach a result. The hold stays in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FulfilmentRefusal {
    /// The hold is gone, superseded, already satisfied, not this run's, or
    /// names evidence other than `codeql` or a Linux `host_sandbox_test`.
    HoldNotCurrent,
    /// A requirement names a program or command shape outside its kind's
    /// allowlist: anything but the CodeQL script for `codeql`, anything but
    /// `cargo test` or an owner-required command for `host_sandbox_test`.
    CommandNotAllowed,
    /// A `host_sandbox_test` command contains a shell metacharacter, quote,
    /// escape or control character. Never run.
    ShellMetacharacter,
    /// A `host_sandbox_test` argument is outside the `cargo test` grammar.
    /// Never run.
    ArgumentNotAllowed,
    /// An evidence or log locator is invalid or names a reserved review artifact.
    ArtifactNotAllowed,
    /// This process is not the task owner that should fulfil its evidence.
    NotOwner,
    /// This host is not Linux, so no run here can be complete.
    HostNotLinux,
    /// Bubblewrap is unavailable or cannot create its required namespaces.
    SandboxUnavailable,
    /// The scratch filesystem has less free space than the run requires.
    DiskInsufficient,
    /// The held commit could not be fetched, or its tree differs.
    CandidateUnreachable,
    /// A tool the script needs is not on the validation PATH.
    ToolMissing,
    /// The script refused the host platform (exit 3).
    PlatformRefused,
    /// Extraction skipped semantic analysis, or analysis did not complete.
    AnalysisIncomplete,
    /// The command failed for another reason.
    CommandFailed,
    /// The command exceeded its time limit.
    TimedOut,
    /// The run's SARIF could not be read.
    ResultsUnreadable,
    /// The analysis reported results, which a fresh review must judge.
    FindingsReported,
    /// A host sandbox test passed but reported that it skipped or deferred
    /// its confined path, so it is no evidence of that path.
    SelfSkipped,
    /// A `cargo test` host run passed without executing a single test.
    NoTestsRan,
    /// A host sandbox test ran and failed.
    TestFailed,
}

impl FulfilmentRefusal {
    /// Stable name recorded in step output, audit and comments.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::HoldNotCurrent => "hold_not_current",
            Self::CommandNotAllowed => "command_not_allowed",
            Self::ShellMetacharacter => "shell_metacharacter",
            Self::ArgumentNotAllowed => "argument_not_allowed",
            Self::ArtifactNotAllowed => "artifact_not_allowed",
            Self::NotOwner => "not_owner",
            Self::HostNotLinux => "host_not_linux",
            Self::SandboxUnavailable => "sandbox_unavailable",
            Self::DiskInsufficient => "disk_insufficient",
            Self::CandidateUnreachable => "candidate_unreachable",
            Self::ToolMissing => "tool_missing",
            Self::PlatformRefused => "platform_refused",
            Self::AnalysisIncomplete => "analysis_incomplete",
            Self::CommandFailed => "command_failed",
            Self::TimedOut => "timed_out",
            Self::ResultsUnreadable => "results_unreadable",
            Self::FindingsReported => "findings_reported",
            Self::SelfSkipped => "self_skipped",
            Self::NoTestsRan => "no_tests_ran",
            Self::TestFailed => "test_failed",
        }
    }

    /// The refusal a host sandbox test's typed reason is here. An OS
    /// mismatch cannot reach a run: only Linux requirements are fulfilled.
    pub(super) fn of_host(reason: HostEvidenceReason) -> Self {
        match reason {
            HostEvidenceReason::ShellMetacharacter => Self::ShellMetacharacter,
            HostEvidenceReason::CommandNotAllowed => Self::CommandNotAllowed,
            HostEvidenceReason::ArgumentNotAllowed => Self::ArgumentNotAllowed,
            HostEvidenceReason::OsMismatch => Self::HoldNotCurrent,
            HostEvidenceReason::CandidateChanged => Self::CandidateUnreachable,
            HostEvidenceReason::SandboxUnavailable => Self::SandboxUnavailable,
            HostEvidenceReason::SelfSkipped => Self::SelfSkipped,
            HostEvidenceReason::NoTestsRan => Self::NoTestsRan,
            HostEvidenceReason::ToolMissing => Self::ToolMissing,
            HostEvidenceReason::TimedOut => Self::TimedOut,
            HostEvidenceReason::RunFailed => Self::CommandFailed,
            HostEvidenceReason::TestFailed => Self::TestFailed,
        }
    }

    /// Whether a later tick may try the same hold again.
    pub(super) fn retryable(self) -> bool {
        matches!(self, Self::DiskInsufficient | Self::CandidateUnreachable)
    }
}
