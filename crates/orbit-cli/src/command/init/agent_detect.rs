//! Agent environment detection used to seed `orbit init` prompt defaults.
//!
//! Probes which agent CLIs are on `PATH`, then derives the provider and model
//! defaults the init prompts offer. The detection layer is gated by
//! [`AgentEnvProbe`] so unit tests can simulate a host without touching the
//! real `PATH`.
//!
//! Detection is frozen at `orbit init`: the results become an explicit
//! `orbit_config::ConfigSeed`, and config loading itself never probes the
//! host.

use std::env;
use std::path::PathBuf;

/// Injectable seam for probing the host environment. Real code uses
/// [`RealAgentEnvProbe`]; tests construct `MockAgentEnvProbe`.
pub trait AgentEnvProbe {
    /// Returns true when an executable named `name` is found on `PATH`.
    fn binary_on_path(&self, name: &str) -> bool;
}

/// Real probe: walks the process `PATH` manually (no extra crate dep).
pub struct RealAgentEnvProbe;

impl AgentEnvProbe for RealAgentEnvProbe {
    fn binary_on_path(&self, name: &str) -> bool {
        let Some(path_var) = env::var_os("PATH") else {
            return false;
        };
        for dir in env::split_paths(&path_var) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let candidate: PathBuf = dir.join(name);
            if is_executable_file(&candidate) {
                return true;
            }
            // On Windows the binary may have an extension. Orbit only ships on
            // Unix today, but this keeps the detector honest if that changes.
            #[cfg(windows)]
            for ext in ["exe", "cmd", "bat"] {
                let mut with_ext = candidate.clone();
                with_ext.set_extension(ext);
                if is_executable_file(&with_ext) {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(unix)]
fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && (meta.permissions().mode() & 0o111) != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable_file(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// Snapshot of which agent CLIs are available. Orbit executes agent activities
/// through the CLI agent path only [ORB-10801], so a detected provider CLI is
/// what makes a provider usable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectedAgents {
    pub claude_cli: bool,
    pub codex_cli: bool,
    pub gemini_cli: bool,
    pub grok_cli: bool,
    /// The standalone `copilot` CLI (npm `@github/copilot`). The retired
    /// `gh-copilot` gh extension is deliberately not probed: it is a shell
    /// command *suggester*, not an agent, and Orbit never dispatches to it.
    /// [ORB-10946]
    pub copilot_cli: bool,
    /// Cursor's supported local headless agent binary. The Cursor editor/plugin
    /// control surface is separate and is not evidence that this executable is
    /// installed. [ORB-10945]
    pub cursor_cli: bool,
    /// The Pi coding agent CLI (npm `@earendil-works/pi-coding-agent`), whose
    /// binary is `pi`. Pi's own `--provider` names the model vendor behind a
    /// run, so a detected `pi` is evidence of the Pi execution lane only, never
    /// of an Anthropic/OpenAI/Google lane. [ORB-11296]
    pub pi_cli: bool,
    pub ollama_cli: bool,
}

/// Probe the host environment using `probe` and return a [`DetectedAgents`]
/// snapshot.
pub fn detect(probe: &dyn AgentEnvProbe) -> DetectedAgents {
    DetectedAgents {
        claude_cli: probe.binary_on_path("claude"),
        codex_cli: probe.binary_on_path("codex"),
        gemini_cli: probe.binary_on_path("gemini"),
        grok_cli: probe.binary_on_path("grok"),
        copilot_cli: probe.binary_on_path("copilot"),
        cursor_cli: probe.binary_on_path("cursor-agent"),
        pi_cli: probe.binary_on_path("pi"),
        ollama_cli: probe.binary_on_path("ollama"),
    }
}

/// CLI agent families available for crew-backed config seeding. This is the
/// whole host-derived input `orbit-config` receives.
///
/// The order intentionally mirrors [`default_provider`] for the overlapping
/// families, excluding `ollama` because Orbit does not ship an `ollama` crew.
pub fn available_crew_families(detected: &DetectedAgents) -> Vec<&'static str> {
    let mut families = Vec::new();
    if detected.claude_cli {
        families.push("claude");
    }
    if detected.codex_cli {
        families.push("codex");
    }
    if detected.gemini_cli {
        families.push("gemini");
    }
    if detected.grok_cli {
        families.push("grok");
    }
    if detected.copilot_cli {
        families.push("copilot");
    }
    if detected.cursor_cli {
        families.push("cursor");
    }
    if detected.pi_cli {
        families.push("pi");
    }
    families
}

/// "Latest known good" model per provider. Returned to seed prompt defaults;
/// users can override at the prompt.
///
/// Thin delegate to [`orbit_common::model_defaults::default_model_for_provider`],
/// the single source of truth for production default model names. Update that
/// module when new flagship models ship.
pub fn default_model_for(provider: &str) -> Option<&'static str> {
    orbit_common::model_defaults::default_model_for_provider(provider)
}

/// Pick a default provider for the role given a detection snapshot.
///
/// Preference order: first detected CLI in [claude, codex, gemini, grok,
/// copilot, cursor, pi, ollama], else `claude` as a last resort. New families
/// are appended after the original four so installing them never changes an
/// existing host's default provider. [ORB-10946] [ORB-10945] [ORB-11296]
pub fn default_provider(detected: &DetectedAgents) -> &'static str {
    if detected.claude_cli {
        return "claude";
    }
    if detected.codex_cli {
        return "codex";
    }
    if detected.gemini_cli {
        return "gemini";
    }
    if detected.grok_cli {
        return "grok";
    }
    if detected.copilot_cli {
        return "copilot";
    }
    if detected.cursor_cli {
        return "cursor";
    }
    if detected.pi_cli {
        return "pi";
    }
    if detected.ollama_cli {
        return "ollama";
    }
    "claude"
}

#[cfg(test)]
pub(crate) mod testing {
    //! In-crate test double exposed at `pub(crate)` so the `init` tests can
    //! reuse it without copying the implementation.

    use super::AgentEnvProbe;
    use std::collections::HashSet;

    /// Test double with a seedable PATH.
    #[derive(Debug, Default, Clone)]
    pub(crate) struct MockAgentEnvProbe {
        binaries: HashSet<String>,
    }

    impl MockAgentEnvProbe {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn with_binary(mut self, name: &str) -> Self {
            self.binaries.insert(name.to_string());
            self
        }
    }

    impl AgentEnvProbe for MockAgentEnvProbe {
        fn binary_on_path(&self, name: &str) -> bool {
            self.binaries.contains(name)
        }
    }
}
