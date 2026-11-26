//! Host execution mechanisms: claim-gated remote commands, tool authorization
//! and policy context, the Git write sandbox, workspace script grants, and
//! embedded activity/job assets.

mod activity_catalog;
pub mod assets;
pub mod command_exec;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod git_sandbox;
pub mod scripts;
pub mod tool_exec;
