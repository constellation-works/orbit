use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ExecutorResourceSpec is the persisted wire shape; ExecutorDef is the runtime shape.
use crate::resource::ExecutorResourceSpec;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorType {
    AgentCli,
    DirectAgent,
    /// Deterministic local command execution. Dispatched by the `local_shell`
    /// deterministic action, never by the agent CLI runner: it carries no
    /// model, prompt, or agent tool authority.
    ///
    /// `cli_command` is the pre-[ORB-11294] spelling of this same executor
    /// family and stays accepted on load, so a bundled or user-authored
    /// definition written before the rename keeps working. Definitions are
    /// re-serialized under the canonical `local_shell` name.
    #[serde(alias = "cli_command")]
    LocalShell,
    /// Generic out-of-process executor speaking the External Executor Protocol
    /// v1 (see `docs/design/executors/specs/external-executor-protocol.md`).
    /// Lets operators register a homegrown binary/script without forking core.
    /// Shares the `direct_agent` subprocess transport but carries no
    /// agent-family `model_pair` semantics. See ADR-0196 / [ORB-00384].
    External,
}

impl ExecutorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentCli => "agent_cli",
            Self::DirectAgent => "direct_agent",
            Self::LocalShell => "local_shell",
            Self::External => "external",
        }
    }
}

impl fmt::Display for ExecutorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Executor sandbox choice: a concrete OS wrapper or an explicit opt-out.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutorSandboxKind {
    /// Persist an operator's choice to disable outer and provider-inner sandboxing.
    Off,
    MacosSandboxExec,
    LinuxBwrap,
}

impl ExecutorSandboxKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::MacosSandboxExec => "macos-sandbox-exec",
            Self::LinuxBwrap => "linux-bwrap",
        }
    }

    /// OS this sandbox primitive can be applied on, named to match
    /// `std::env::consts::OS` (e.g. `"macos"`, `"linux"`).
    ///
    /// Concrete wrappers are single-OS; explicit off has no OS requirement.
    pub fn target_os(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::MacosSandboxExec => Some("macos"),
            Self::LinuxBwrap => Some("linux"),
        }
    }

    /// Whether this sandbox primitive applies to a given host OS, named to
    /// match `std::env::consts::OS`. Injecting the platform lets shipped-executor
    /// seed-time selection (see `orbit-core`) be tested deterministically on
    /// either OS without a `#[cfg]` split (see [ORB-10112]).
    pub fn is_available_on(self, target_os: &str) -> bool {
        self.target_os()
            .is_none_or(|required| required == target_os)
    }
}

impl fmt::Display for ExecutorSandboxKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum StdoutFormat {
    Envelope,
    Json,
    Text,
}

impl StdoutFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Envelope => "envelope",
            Self::Json => "json",
            Self::Text => "text",
        }
    }
}

impl fmt::Display for StdoutFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutorDef {
    pub name: String,
    /// Executor family, serialized as "agent_cli", "direct_agent",
    /// "local_shell", or "external".
    pub executor_type: ExecutorType,
    /// For agent_cli: the CLI command (e.g., "claude", "codex")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Expected stdout format, serialized as "envelope", "json", or "text".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_format: Option<StdoutFormat>,
    /// Legacy override for the agent family's `AgentModelPair` resolution used
    /// by older/user-authored definitions for audit canonicalization, envelope
    /// rendering, and review attribution. Fresh shipped defaults use
    /// crew-selected models instead.
    ///
    /// Does NOT control which model the subprocess actually runs; operators
    /// should encode runtime model selection in `args`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_pair_override: Option<ModelPairOverride>,
    /// CLI flag name used to pass `JobStep.model` to a direct-agent subprocess.
    ///
    /// Carries only the flag name, for example `"-m"` or `"--model"`. At
    /// invocation time, when both `model_flag` and the step's runtime model are
    /// present, `direct_agent` appends `[model_flag, step.model]` after the
    /// operator-declared `args`. Orbit does not inspect `args` for duplicates;
    /// the CLI's own last-wins behavior resolves repeated model flags. When
    /// either field is absent, nothing is injected, so operators can still
    /// hardcode fixed model arguments such as `--model X` in `args`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_flag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Sandbox choice. `Off` persistently disables outer and provider-inner
    /// sandboxing. `None` is unspecified: installed Linux defaults migrate to
    /// Bubblewrap during seeding; other executors retain provider behavior.
    /// Concrete kinds wrap the invocation using the activity's `FsProfile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<ExecutorSandboxKind>,
    /// When `sandbox` is set but the platform's trusted sandbox primitive is
    /// unavailable (e.g. `/usr/bin/sandbox-exec` is missing), should the runner
    /// degrade to bare exec? Default `false` (fail-closed).
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_fallback: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

/// Legacy override for an agent family's strong/weak `AgentModelPair`.
///
/// Controls how Orbit canonicalizes the agent's model for audit trail,
/// envelope rendering, and review automation attribution.
///
/// Does NOT control which model the subprocess actually runs. Older or
/// customized definitions may retain this field; new definitions should use
/// crew-selected models and `model_flag`. Operators can also set
/// `ORBIT_AGENT_MODEL` via `env:` for explicit audit attribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct ModelPairOverride {
    pub strong: String,
    pub weak: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ExecutorDef {
    pub fn from_resource_spec(
        name: String,
        spec: ExecutorResourceSpec,
        created_at: Option<DateTime<Utc>>,
        updated_at: Option<DateTime<Utc>>,
    ) -> Self {
        let ExecutorResourceSpec {
            executor_type,
            command,
            args,
            stdout_format,
            model_pair_override,
            model_flag,
            timeout_seconds,
            env,
            sandbox,
            allow_fallback,
            created_at: spec_created_at,
            updated_at: spec_updated_at,
        } = spec;

        Self {
            name,
            executor_type,
            command,
            args,
            stdout_format,
            model_pair_override,
            model_flag,
            timeout_seconds,
            env,
            sandbox,
            allow_fallback,
            created_at: created_at.or(spec_created_at),
            updated_at: updated_at.or(spec_updated_at),
        }
    }

    pub fn model_pair_override(&self) -> Option<&ModelPairOverride> {
        self.model_pair_override.as_ref()
    }
}
