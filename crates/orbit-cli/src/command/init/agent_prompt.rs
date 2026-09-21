//! Interactive prompts that choose, by name, which seeded crew is the default
//! crew and which is the system crew during `orbit init`.
//!
//! Every option comes from the [`ConfigSeed`] the detection step built, so a
//! prompt can only name a crew the seeded file defines. Recommendations are
//! the seed's own — the same ones `--non-interactive` writes — so answering
//! every prompt with Enter produces exactly the non-interactive file.

use std::collections::BTreeMap;
use std::io::{self, Write};

use orbit_config::ConfigSeed;
use orbit_types::identity::Crew;

use super::agent_detect::DetectedAgents;
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

/// Choose the seeded crew written as `workflow.default_crew`. Returns `None`
/// when the seed writes no crews, in which case there is nothing to choose
/// and no prompt runs.
pub fn collect_default_crew(
    detected: &DetectedAgents,
    seed: &ConfigSeed,
    prompter: &mut dyn Prompter,
) -> io::Result<Option<String>> {
    let crews = seed.seeded_crews();
    let Some(recommended) = seed.recommended_default_crew() else {
        prompter.message(&no_crew_text(detected))?;
        return Ok(None);
    };
    let recommended_crew = crews.get(recommended).ok_or_else(|| {
        io::Error::other(format!("recommended crew `{recommended}` is not seeded"))
    })?;
    prompter.message(&intro_text(detected, recommended_crew))?;
    if yes_by_default(&prompter.prompt("Use this default crew? [Y/n]: ")?) {
        return Ok(Some(recommended.to_string()));
    }

    // The recommendation leads the list so an empty answer keeps it.
    let options = std::iter::once(recommended)
        .chain(
            crews
                .keys()
                .map(String::as_str)
                .filter(|name| *name != recommended),
        )
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    prompter.message(&format_crew_options(
        "Choose the default crew:",
        &options,
        &crews,
    ))?;
    choose_crew(options, "Choice [1]: ", prompter).map(Some)
}

/// Choose the seeded crew written as `workflow.system_crew`. Only the cheap
/// tier of each detected family is offered, in the seed's preference order;
/// a host with exactly one candidate takes it without a prompt.
pub(crate) fn collect_system_crew(
    seed: &ConfigSeed,
    prompter: &mut dyn Prompter,
) -> io::Result<Option<String>> {
    let options = seed
        .system_crew_options()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if options.len() <= 1 {
        return Ok(options.into_iter().next());
    }

    prompter.message(&format_crew_options(
        "Choose the crew for bounded system work (recovery, task pilot, qa-sweep):",
        &options,
        &seed.seeded_crews(),
    ))?;
    choose_crew(options, "System crew [1]: ", prompter).map(Some)
}

/// Read a 1-based choice from `options` until one is valid; an empty answer
/// takes the first entry.
fn choose_crew(
    mut options: Vec<String>,
    prompt: &str,
    prompter: &mut dyn Prompter,
) -> io::Result<String> {
    let last = options.len();
    loop {
        let choice = prompter.prompt(prompt)?;
        let choice = choice.trim();
        let selected = if choice.is_empty() {
            Some(0)
        } else {
            choice.parse::<usize>().ok().and_then(|n| n.checked_sub(1))
        };
        if let Some(index) = selected.filter(|index| *index < options.len()) {
            return Ok(options.swap_remove(index));
        }
        prompter.message(&format!("Please enter 1-{last}."))?;
    }
}

fn format_crew_options(
    heading: &str,
    options: &[String],
    crews: &BTreeMap<String, Crew>,
) -> String {
    let mut lines = vec![heading.to_string(), String::new()];
    for (index, name) in options.iter().enumerate() {
        let line = crews
            .get(name)
            .map(crew_line)
            .unwrap_or_else(|| name.to_string());
        lines.push(format!("  {:>2}. {line}", index + 1));
    }
    lines.join("\n")
}

/// `name  provider  model`, padded so the columns line up in a list.
fn crew_line(crew: &Crew) -> String {
    format!(
        "{:<12} {:<12} {}",
        crew.name, crew.assignment.provider, crew.assignment.model
    )
}

fn yes_by_default(input: &str) -> bool {
    let trimmed = input.trim();
    trimmed.is_empty() || trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes")
}

const AGENT_FAMILY_LABELS: [&str; 10] = [
    "Claude CLI",
    "Codex CLI",
    "Antigravity CLI",
    "Gemini CLI",
    "Grok CLI",
    "Copilot CLI",
    "Cursor Agent CLI",
    "Pi CLI",
    "OpenCode CLI",
    "Ollama CLI",
];

fn agent_families(detected: &DetectedAgents) -> impl Iterator<Item = (&'static str, bool)> {
    AGENT_FAMILY_LABELS.into_iter().zip([
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

fn intro_text(detected: &DetectedAgents, recommended: &Crew) -> String {
    format!(
        "Orbit routes every activity through one crew assignment. An activity input may select a different named crew; otherwise it uses the run's resolved crew.\n\nDetected agents:\n{}\n\nRecommended default crew:\n  {}",
        detection_lines(detected),
        crew_line(recommended)
    )
}

fn no_crew_text(detected: &DetectedAgents) -> String {
    format!(
        "Detected agents:\n{}\n\nNo agent CLI Orbit ships a crew for was found, so config.toml is written with an empty [crews] registry and no default crew. Define crews under [crews.<name>] and set workflow.default_crew once an agent CLI is installed.",
        detection_lines(detected)
    )
}

fn detection_lines(detected: &DetectedAgents) -> String {
    agent_families(detected)
        .map(|(label, found)| {
            let status = if found { "found" } else { "not found" };
            format!("  {label:<18} {status}")
        })
        .collect::<Vec<_>>()
        .join("\n")
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
