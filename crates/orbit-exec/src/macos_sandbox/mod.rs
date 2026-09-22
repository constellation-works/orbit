//! macOS `sandbox-exec` primitive: SBPL compilation + sandboxed spawn.
//!
//! Translates a [`ResolvedFsProfile`] into a Sandbox Profile Language (SBPL)
//! payload and wraps a child process in `sandbox-exec -f <profile>`. This is
//! the OS-level enforcement seam for `backend: cli` activities.
//!
//! # Why not `--sandbox` flags on each agent CLI?
//!
//! Codex ships its own `--sandbox` flag, gemini has `-s`, claude has nothing
//! at the OS level. Building enforcement on three different CLI surfaces
//! produces three different audit stories and an asymmetric trust model.
//! Wrapping each invocation in `sandbox-exec` gives one declarative source
//! of truth — the activity's `FsProfile` — and one enforcement seam.
//!
//! # SBPL caveats
//!
//! Apple deprecated SBPL but the kernel still honors it (codex itself uses
//! it). v1 accepts that risk; the design doc records the choice. Negated
//! `!path` rules from `denyRead` / `denyModify` are emitted as explicit
//! `(deny file-read* (subpath ...))` / `(deny file-write* (subpath ...))`
//! clauses appended after the broad allows so they win in last-match-wins.

//!
//! The `compile` submodule assembles SBPL profiles from resolved filesystem rules.
//! The `provider_dirs` submodule emits provider state-directory allowances.
//! The `sbpl_filter` submodule owns SBPL escaping and path-filter rendering.
//! The `spawn` submodule launches processes through trusted `sandbox-exec` paths.

pub(crate) mod compile;
pub(crate) mod provider_dirs;
pub(crate) mod sbpl_filter;
pub(crate) mod spawn;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use compile::{
    MacosLoginKeychainAccess, MacosNetworkAccess, append_macos_network_access,
    append_macos_read_boundary, compile_macos_sandbox_profile, macos_login_keychain_access,
};
pub use provider_dirs::{claude_state_dir_from_env, grok_state_dir_from_env};
pub use spawn::{
    MacosSandboxSpawnRequest, sandbox_exec_available, sandbox_exec_path,
    sandbox_exec_program_for_audit, sandbox_exec_unavailable_message, spawn_under_macos_sandbox,
};
