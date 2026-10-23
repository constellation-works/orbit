//! Executor sandbox resolution: the executor's declared sandbox kind plus the
//! activity's fsProfile become the `ResolvedSandbox` the CLI runner enforces,
//! anchored at the active worktree where one applies, with Orbit runtime
//! stores and provider state roots granted around the policy's own rules.

#[cfg(target_os = "linux")]
mod provider_state;
mod resolve;
#[cfg(target_os = "linux")]
mod runtime_grants;
#[cfg(target_os = "linux")]
mod runtime_paths;
#[cfg(test)]
mod tests;
mod worktree;

pub(crate) use resolve::resolve_executor_sandbox;
