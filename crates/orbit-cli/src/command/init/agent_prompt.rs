//! Interactive prompts that collect the default crew and, when more than one
//! cheap-tier family is detected, the system crew during `orbit init`.

use std::io::{self, Write};

use orbit_config::CrewSeed;

use super::agent_detect::{DetectedAgents, default_model_for, default_provider};
use super::prompt_stdin;

pub trait Prompter {
    fn message(&mut self, text: &str) -> io::Result<()>;
    fn prompt(&mut self, prompt: &str) -> io::Result<String>;
}

pub struct StdinPrompter;

impl Prompter for StdinPrompter {
    fn message(&mut self, text: &str) -> io::Result<()> {
        let stderr = io::stderr();
        let mut out = stderr.lock();
        writeln!(out, "{text}")?;
        Ok(())
    }

    fn prompt(&mut self, prompt: &str) -> io::Result<String> {
        let stderr = io::stderr();
        let mut out = stderr.lock();
        prompt_stdin::read_trimmed_line(prompt, &mut out)
    }
}

/// Choose the single assignment written as `[crews.custom]` and selected by
/// `workflow.default_crew` for a fresh interactive installation.
pub fn collect_crew_setting(
    detected: &DetectedAgents,
    prompter: &mut dyn Prompter,
) -> io::Result<CrewSeed> {
    let recommended = recommended_crew_setting(detected);
    prompter.message(&intro_text(detected, &recommended))?;
    if yes_by_default(&prompter.prompt("Use this default crew? [Y/n]: ")?) {
        return Ok(recommended);
    }

    let options = agent_options(detected);
    prompter.message(&format_agent_options(&options))?;
    loop {
        let choice = prompter.prompt("Choice [1]: ")?;
        let choice = choice.trim();
        if choice.eq_ignore_ascii_case("custom") || choice.eq_ignore_ascii_case("c") {
            return collect_custom_crew(detected, prompter);
        }
        if choice
            .parse::<usize>()
            .is_ok_and(|number| number == options.len() + 1)
        {
            return collect_custom_crew(detected, prompter);
        }

        let selected = if choice.is_empty() {
            Some(0)
        } else {
            choice.parse::<usize>().ok().and_then(|n| n.checked_sub(1))
        };
        if let Some(option) = selected.and_then(|index| options.get(index)) {
            return Ok(CrewSeed {
                provider: Some(option.provider.to_string()),
                model: collect_model_override(option.model, prompter)?,
            });
        }

        let custom_index = options.len() + 1;
        prompter.message(&format!(
            "Please enter 1-{custom_index}, or `custom` for a manual provider."
        ))?;
    }
}

/// Choose the assignment written as `[crews.system]`. Only cheap-tier options
/// are offered: Codex Luna, Claude Sonnet, Grok, Gemini Flash. No Sol, Opus,
/// Terra, or free-form custom provider.
pub(crate) fn collect_system_crew_setting(
    detected: &DetectedAgents,
    prompter: &mut dyn Prompter,
) -> io::Result<Option<CrewSeed>> {
    let mut options = system_crew_options(detected);
    if options.is_empty() {
        return Ok(None);
    }
    if options.len() == 1 {
        return Ok(Some(options.remove(0)));
    }

    prompter.message(&format_system_crew_options(&options))?;
    let last = options.len();
    loop {
        let choice = prompter.prompt("System crew [1]: ")?;
        let choice = choice.trim();
        let selected = if choice.is_empty() {
            Some(0)
        } else {
            choice.parse::<usize>().ok().and_then(|n| n.checked_sub(1))
        };
        if let Some(index) = selected.filter(|index| *index < options.len()) {
            return Ok(Some(options.remove(index)));
        }
        prompter.message(&format!("Please enter 1-{last}."))?;
    }
}

/// Cheap-tier system options in the same preference order as
/// `orbit-config::default_system_crew`: Codex Luna, Claude Sonnet, Grok,
/// Antigravity Flash, Gemini Flash, Copilot Haiku, Cursor, Pi, OpenCode.
fn system_crew_options(detected: &DetectedAgents) -> Vec<CrewSeed> {
    use orbit_common::model_defaults::{
        ANTIGRAVITY_CREW_MODEL, CLAUDE_DEFAULT_WEAK, CODEX_LUNA_MODEL, COPILOT_CREW_MODEL,
        CURSOR_CREW_MODEL, GEMINI_CREW_MODEL, GROK_DEFAULT_MODEL, OPENCODE_CREW_MODEL,
        PI_CREW_MODEL,
    };
    let mut options = Vec::new();
    for (enabled, provider, model) in [
        (detected.codex_cli, "codex", CODEX_LUNA_MODEL),
        (detected.claude_cli, "claude", CLAUDE_DEFAULT_WEAK),
        (detected.grok_cli, "grok", GROK_DEFAULT_MODEL),
        (
            detected.antigravity_cli,
            "antigravity",
            ANTIGRAVITY_CREW_MODEL,
        ),
        (detected.gemini_cli, "gemini", GEMINI_CREW_MODEL),
        (detected.copilot_cli, "copilot", COPILOT_CREW_MODEL),
        (detected.cursor_cli, "cursor", CURSOR_CREW_MODEL),
        (detected.pi_cli, "pi", PI_CREW_MODEL),
        (detected.opencode_cli, "opencode", OPENCODE_CREW_MODEL),
    ] {
        if enabled {
            options.push(CrewSeed {
                provider: Some(provider.to_string()),
                model: Some(model.to_string()),
            });
        }
    }
    options
}

fn format_system_crew_options(options: &[CrewSeed]) -> String {
    let mut lines = vec![
        "Choose the cheap-tier agent for the system crew (recovery, triage, qa-sweep):".to_string(),
        String::new(),
    ];
    for (index, option) in options.iter().enumerate() {
        lines.push(format!(
            "  {}. {:<8} {}",
            index + 1,
            system_crew_label(option),
            option.model.as_deref().unwrap_or("(not set)")
        ));
    }
    lines.join("\n")
}

fn system_crew_label(option: &CrewSeed) -> &'static str {
    match option.provider.as_deref() {
        Some("codex") => "Codex",
        Some("claude") => "Claude",
        Some("grok") => "Grok",
        Some("antigravity") => "Antigravity",
        Some("gemini") => "Gemini",
        Some("copilot") => "Copilot",
        Some("cursor") => "Cursor",
        Some("pi") => "Pi",
        Some("opencode") => "OpenCode",
        _ => "Agent",
    }
}

fn recommended_crew_setting(detected: &DetectedAgents) -> CrewSeed {
    let provider = default_provider(detected);
    CrewSeed {
        provider: Some(provider.to_string()),
        model: default_model_for(provider).map(str::to_string),
    }
}

fn collect_custom_crew(
    detected: &DetectedAgents,
    prompter: &mut dyn Prompter,
) -> io::Result<CrewSeed> {
    let provider_default = default_provider(detected);
    let provider = take_or_default(
        prompter.prompt(&format!("Provider [{provider_default}]: "))?,
        provider_default,
    );
    let model_default = default_model_for(&provider).unwrap_or("");
    let model = collect_model_override(model_default, prompter)?;
    Ok(CrewSeed {
        provider: Some(provider),
        model,
    })
}

fn collect_model_override(
    model_default: &str,
    prompter: &mut dyn Prompter,
) -> io::Result<Option<String>> {
    let prompt = if model_default.is_empty() {
        "Model: ".to_string()
    } else {
        format!("Model [{model_default}]: ")
    };
    loop {
        let model = take_or_default(prompter.prompt(&prompt)?, model_default);
        if !model.is_empty() {
            return Ok(Some(model));
        }
        prompter.message("Model is required for a crew assignment.")?;
    }
}

fn take_or_default(input: String, default: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn yes_by_default(input: &str) -> bool {
    let trimmed = input.trim();
    trimmed.is_empty() || trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentOption {
    label: &'static str,
    provider: &'static str,
    model: &'static str,
}

#[derive(Clone, Copy)]
struct AgentFamily {
    label: &'static str,
    provider: &'static str,
}

const AGENT_FAMILIES: [AgentFamily; 10] = [
    AgentFamily {
        label: "Claude CLI",
        provider: "claude",
    },
    AgentFamily {
        label: "Codex CLI",
        provider: "codex",
    },
    AgentFamily {
        label: "Antigravity CLI",
        provider: "antigravity",
    },
    AgentFamily {
        label: "Gemini CLI",
        provider: "gemini",
    },
    AgentFamily {
        label: "Grok CLI",
        provider: "grok",
    },
    AgentFamily {
        label: "Copilot CLI",
        provider: "copilot",
    },
    AgentFamily {
        label: "Cursor Agent CLI",
        provider: "cursor",
    },
    AgentFamily {
        label: "Pi CLI",
        provider: "pi",
    },
    AgentFamily {
        label: "OpenCode CLI",
        provider: "opencode",
    },
    AgentFamily {
        label: "Ollama CLI",
        provider: "ollama",
    },
];

fn agent_families(detected: &DetectedAgents) -> impl Iterator<Item = (AgentFamily, bool)> {
    AGENT_FAMILIES.into_iter().zip([
        detected.claude_cli,
        detected.codex_cli,
        detected.antigravity_cli,
        detected.gemini_cli,
        detected.grok_cli,
        detected.copilot_cli,
        detected.cursor_cli,
        detected.pi_cli,
        detected.opencode_cli,
        detected.ollama_cli,
    ])
}

fn agent_options(detected: &DetectedAgents) -> Vec<AgentOption> {
    let mut options = Vec::new();
    for (family, enabled) in agent_families(detected) {
        if enabled {
            options.push(agent_option(family.label, family.provider));
        }
    }

    let provider = default_provider(detected);
    if let Some(index) = options
        .iter()
        .position(|option| option.provider == provider)
    {
        let option = options.remove(index);
        options.insert(0, option);
    } else {
        options.insert(0, agent_option("Recommended agent", provider));
    }
    options
}

fn agent_option(label: &'static str, provider: &'static str) -> AgentOption {
    AgentOption {
        label,
        provider,
        model: default_model_for(provider).unwrap_or(""),
    }
}

fn intro_text(detected: &DetectedAgents, recommended: &CrewSeed) -> String {
    format!(
        "Orbit routes every activity through one crew assignment. An activity input may select a different named crew; otherwise it uses the run's resolved crew.\n\nDetected agents:\n{}\n\nRecommended default crew:\n  {:<18} {}",
        detection_lines(detected),
        agent_display_name(recommended),
        recommended.model.as_deref().unwrap_or("(not set)")
    )
}

fn detection_lines(detected: &DetectedAgents) -> String {
    agent_families(detected)
        .map(|(family, found)| {
            let status = if found { "found" } else { "not found" };
            format!("  {:<18} {status}", family.label)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_agent_options(options: &[AgentOption]) -> String {
    let mut lines = vec![
        "Choose an agent for the default crew:".to_string(),
        String::new(),
    ];
    for (index, option) in options.iter().enumerate() {
        let model = if option.model.is_empty() {
            "(model not set)"
        } else {
            option.model
        };
        lines.push(format!("  {}. {:<16} {model}", index + 1, option.label));
    }
    lines.push(format!("  {}. Custom", options.len() + 1));
    lines.join("\n")
}

fn agent_display_name(config: &CrewSeed) -> String {
    let provider = config.provider.as_deref().unwrap_or("custom");
    match AGENT_FAMILIES
        .iter()
        .find(|family| family.provider == provider)
    {
        Some(family) => family.label.to_string(),
        None => format!("{provider} (CLI)"),
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::Prompter;
    use std::collections::VecDeque;
    use std::io;

    #[derive(Debug, Default)]
    pub(crate) struct CannedPrompter {
        answers: VecDeque<String>,
        messages: Vec<String>,
        prompts: Vec<String>,
    }

    impl CannedPrompter {
        pub(crate) fn new<I: IntoIterator<Item = &'static str>>(answers: I) -> Self {
            Self {
                answers: answers.into_iter().map(String::from).collect(),
                messages: Vec::new(),
                prompts: Vec::new(),
            }
        }

        pub(crate) fn transcript(&self) -> String {
            self.messages
                .iter()
                .chain(self.prompts.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    impl Prompter for CannedPrompter {
        fn message(&mut self, text: &str) -> io::Result<()> {
            self.messages.push(text.to_string());
            Ok(())
        }

        fn prompt(&mut self, prompt: &str) -> io::Result<String> {
            self.prompts.push(prompt.to_string());
            self.answers.pop_front().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("no canned answer for prompt `{prompt}`"),
                )
            })
        }
    }
}
