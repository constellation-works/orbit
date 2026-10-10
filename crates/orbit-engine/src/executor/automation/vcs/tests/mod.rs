#![allow(missing_docs)]

mod baseline;
mod delivery_marker;
// The fetch-lock fixture installs an executable fake `ssh` (Unix mode bits).
#[cfg(unix)]
mod git;
mod required_command;
