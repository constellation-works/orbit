#![allow(missing_docs)]

mod base_claim;
mod delivery_marker;
// The fetch-lock fixture installs an executable fake `ssh` (Unix mode bits).
#[cfg(unix)]
mod git;
#[cfg(unix)]
mod remote_probe;
mod required_command;
