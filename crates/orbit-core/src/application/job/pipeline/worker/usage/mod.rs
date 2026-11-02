pub(super) mod sampler;
pub(super) mod tree;

// Only macOS starts the watchdog (Linux contains a worker with its cgroup
// scope), but the module compiles on Linux too, so Linux CodeQL extraction
// analyzes it instead of skipping a production file. The process-tree reader
// is the caller's, because only macOS has a session-tree snapshot.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) mod watchdog;
