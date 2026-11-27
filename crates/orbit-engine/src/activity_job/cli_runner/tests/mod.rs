#![allow(missing_docs)]

// Everything but `stdout_preview` drives `#!/bin/sh` fake agents.
#[cfg(unix)]
mod inspection;
#[cfg(unix)]
mod orchestrator_env;
mod spawn_diagnostics;
mod stdout_preview;

pub(in crate::activity_job::cli_runner) mod test_support;
