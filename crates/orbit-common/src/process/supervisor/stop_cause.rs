//! Stop causes the supervisor reports, independently of child output.

/// Prefix of the supervisor note when output capture stops a process.
pub const OUTPUT_CAPTURE_LIMIT_NOTE: &str = "process output capture limit exceeded on ";
/// Prefix of the supervisor note when an incoming signal stops a process.
pub const INTERRUPTED_SIGNAL_NOTE: &str = "process interrupted by signal ";
/// Prefix of the supervisor note when a child terminates without an exit code.
pub const TERMINATION_STATUS_NOTE: &str = "process terminated with ";

/// A stop observed by the process supervisor, independently of child output.
///
/// Deadlines are reported separately by the outcome's `timed_out` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStopCause {
    /// A caller's policy check requested termination. The reason identifies
    /// that policy independently of anything the child printed.
    Requested { reason: &'static str },
    /// A captured stream exceeded its byte ceiling, including during drain
    /// after the child exited.
    OutputCaptureLimit { stream: &'static str },
    /// An incoming signal interrupted supervision.
    InterruptedSignal { signal: i32 },
    /// The child terminated without an exit code, such as by a Unix signal.
    TerminationStatus { status: std::process::ExitStatus },
}

impl std::fmt::Display for ProcessStopCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Requested { reason } => write!(f, "process stop requested: {reason}"),
            Self::OutputCaptureLimit { stream } => write!(f, "{OUTPUT_CAPTURE_LIMIT_NOTE}{stream}"),
            Self::InterruptedSignal { signal } => {
                #[cfg(unix)]
                {
                    f.write_str(&super::signal_guard::signal_message(*signal))
                }
                #[cfg(not(unix))]
                {
                    write!(f, "{INTERRUPTED_SIGNAL_NOTE}{signal}")
                }
            }
            Self::TerminationStatus { status } => write!(f, "{TERMINATION_STATUS_NOTE}{status}"),
        }
    }
}
