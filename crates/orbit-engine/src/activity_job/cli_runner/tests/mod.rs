#![allow(missing_docs)]

// Everything but `stdout_preview` drives `#!/bin/sh` fake agents.
#[cfg(unix)]
mod orchestrator_env;
mod stdout_preview;
#[cfg(unix)]
mod supervisor;
#[cfg(unix)]
pub(in crate::activity_job::cli_runner) mod test_support;
#[cfg(unix)]
mod trusted_host;
