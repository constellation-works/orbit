//! Compact inherited options without changing a command's own help.
//!
//! Grouped command lists (`orbit`, `orbit run`, `orbit task`) share one
//! layout: a single description column per template derived from the longest
//! command name in that template, plus a two-space gap. On a terminal the
//! descriptions wrap to the available width with continuation lines indented
//! to the column, or stack with a smaller indent when the terminal is narrow.
//! Off a terminal (width 0, the piped/golden path) every row stays on one line.
//! The column is computed from the real names, not hand-typed spaces, so a
//! longer name widens the column instead of drifting [ORB-19912].
//!
//! Everything else Clap renders (flag descriptions, about text, its command
//! lists) is wrapped by Clap itself once `wrap_help` is on and the command
//! carries the sink width. Footer text is the exception: Clap re-flows a long
//! footer line from column 0, which strands a continuation of an indented
//! example away from its command, so [`hang_wrap_footers`] wraps footers first.

use clap::{Command, Id};

/// Long help always puts argument descriptions on separate lines in clap.
/// Render the inherited options as a compact reference instead, ahead of any
/// existing footer. Root help remains the source of their full descriptions.
pub(crate) fn compact_globals(mut command: Command, global_ids: &[Id]) -> Command {
    let mut section = String::from("Global options (see 'orbit --help' for descriptions):");
    for id in global_ids {
        if let Some(arg) = command.get_arguments().find(|arg| arg.get_id() == id) {
            section.push_str(&format!("\n  --{}", arg.get_long().unwrap_or(id.as_str())));
            if arg.get_action().takes_values() {
                let value = arg
                    .get_value_names()
                    .and_then(|names| names.first())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| id.as_str().to_uppercase());
                section.push_str(&format!(" <{value}>"));
            }
        }
    }
    let short = match command.get_after_help() {
        Some(footer) => format!("{section}\n\n{footer}"),
        None => section.clone(),
    };
    let long = command
        .get_after_long_help()
        .map(|footer| format!("{section}\n\n{footer}"));
    for id in global_ids {
        // Keep the flags in usage and suggestions; only their help is custom.
        command = command.mut_arg(id, |arg| arg.hide_short_help(true).hide_long_help(true));
    }
    command = command.after_help(short);
    if let Some(long) = long {
        command = command.after_long_help(long);
    }
    command
}

/// Wrap every footer in `command`'s tree to `width` columns with a hanging
/// indent: a continuation of an indented line sits two columns deeper than the
/// line it continues, so it stays with its example. Width `0` (piped) leaves
/// the footers untouched.
pub(crate) fn hang_wrap_footers(mut command: Command, width: usize) -> Command {
    if width == 0 {
        return command;
    }
    if let Some(footer) = command.get_after_help().map(ToString::to_string) {
        command = command.after_help(hang_wrap_text(&footer, width));
    }
    if let Some(footer) = command.get_after_long_help().map(ToString::to_string) {
        command = command.after_long_help(hang_wrap_text(&footer, width));
    }
    let names: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_string())
        .collect();
    for name in names {
        command = command.mut_subcommand(name, |sub| hang_wrap_footers(sub, width));
    }
    command
}

/// Extra columns a continuation sits under the line it continues, when that
/// line is itself indented.
const HANG_INDENT: usize = 2;

/// Wrap the lines of `text` that are wider than `width` at word boundaries.
/// Lines that fit are left exactly as written, so aligned columns survive. A
/// prose paragraph hard-wrapped in the source (unindented lines, each ending
/// mid-sentence) is re-flowed as one when any of its lines is too wide, so the
/// narrow wrap does not leave a ragged stub after every source line break.
fn hang_wrap_text(text: &str, width: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let flush = |paragraph: &mut Vec<&str>, out: &mut Vec<String>| {
        if paragraph.iter().any(|line| line.chars().count() > width) {
            out.push(hang_wrap_line(&paragraph.join(" "), width));
        } else {
            out.extend(paragraph.iter().map(|line| line.to_string()));
        }
        paragraph.clear();
    };
    for line in text.split('\n') {
        let prose = !line.is_empty() && !line.starts_with(' ');
        if !prose {
            flush(&mut paragraph, &mut out);
            out.push(hang_wrap_line(line, width));
            continue;
        }
        paragraph.push(line);
        // A line that ends a sentence or introduces a list closes its paragraph.
        if line.ends_with(['.', ':', ';']) {
            flush(&mut paragraph, &mut out);
        }
    }
    flush(&mut paragraph, &mut out);
    out.join("\n")
}

fn hang_wrap_line(line: &str, width: usize) -> String {
    if line.chars().count() <= width {
        return line.to_string();
    }
    let lead = line.chars().take_while(|c| *c == ' ').count();
    let continuation = if lead > 0 { lead + HANG_INDENT } else { 0 };
    // A hang so deep it leaves no room would never make progress; fall back to
    // the line's own indent.
    let continuation = if continuation + 1 >= width {
        lead.min(width.saturating_sub(1))
    } else {
        continuation
    };
    let mut lines: Vec<String> = Vec::new();
    let mut current = " ".repeat(lead.min(width.saturating_sub(1)));
    let mut has_word = false;
    for word in line.split_whitespace() {
        let word_len = word.chars().count();
        let current_len = current.chars().count();
        if !has_word {
            current.push_str(word);
            has_word = true;
        } else if current_len + 1 + word_len <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::replace(&mut current, " ".repeat(continuation)));
            current.push_str(word);
        }
    }
    lines.push(current);
    lines.join("\n")
}

/// Spaces before a grouped row.
const GROUP_INDENT: usize = 2;
/// Minimum spaces between the longest name and its description.
const GROUP_GAP: usize = 2;

/// One grouped row: the command spelling and its short help.
type GroupedRow = (&'static str, &'static str);

const RUN_WORKFLOWS: &[GroupedRow] = &[
    (
        "auto",
        "Drain the workspace backlog for a window; --stop ends new admissions",
    ),
    (
        "ship",
        "Ship backlog or explicitly selected tasks through the gated task pipeline",
    ),
    (
        "task-pilot",
        "Preflight proposed/backlog tasks and persist validated selectors",
    ),
    ("job", "Run an arbitrary job by ID"),
    (
        "agent",
        "Invoke an agent on the host for exploration or debugging (deprecated)",
    ),
];

const RUN_AUDITS: &[GroupedRow] = &[
    (
        "history",
        "Show recent job runs filtered by job, task, state or time",
    ),
    (
        "show",
        "Show structured state and step summary for a job run",
    ),
    ("logs", "Print raw stdout/stderr captured for a job run"),
    ("events", "Show audit events recorded for a job run"),
    ("trace", "Show audit event parent/child trace for a job run"),
    (
        "settlements",
        "Show failure and release settlements claimed leaves sent this owner",
    ),
];

const RUN_MAINTENANCE: &[GroupedRow] = &[
    ("cancel", "Cancel a pending or running job run"),
    ("concurrency", "Retune a running drain's admission ceiling"),
    (
        "extend",
        "Lengthen a running drain's window, and optionally retune it",
    ),
    (
        "readiness",
        "Explain why backlog tasks are waiting in auto-drain",
    ),
    (
        "profiles",
        "Show what each kind of delivery costs on this host, minute by minute",
    ),
];

const TASK_TASKS: &[GroupedRow] = &[
    ("add", "Create a new task"),
    (
        "update",
        "Update task fields; `--approve` takes the next approval step",
    ),
    ("archive", "Archive a task"),
    ("list", "List tasks with optional filters"),
    (
        "eligible",
        "List backlog/proposed tasks that collide with no in-flight work",
    ),
    (
        "show",
        "Show one task in detail, by ID, across registered workspaces",
    ),
    ("artifact", "Manage task artifact files"),
];

const TASK_HEALTH: &[GroupedRow] = &[
    (
        "lint",
        "Lint tasks for context-file and acceptance-criteria problems",
    ),
    (
        "flow",
        "Show filed-vs-closed rates over time — is the backlog draining?",
    ),
    (
        "locks",
        "Inspect, reserve, and release the file locks that gate dispatch",
    ),
    (
        "review-reset",
        "Reset one review lineage budget with an audited reason",
    ),
    (
        "reconcile-review",
        "Validate and review a recovered delivery's changed merged head",
    ),
    (
        "retry-handoff",
        "Retry a stopped authorized handoff through normal owner landing",
    ),
    (
        "recover-claim",
        "Recover a live claim to blocked or backlog with an audited reason",
    ),
    (
        "revoke-handoff",
        "Revoke a handoff's completion authority with an audited reason",
    ),
    (
        "requeue",
        "Requeue a blocked task onto its published candidate",
    ),
    (
        "recheck-blocked",
        "Requeue tasks whose infrastructure block has cleared",
    ),
    (
        "settle-merged",
        "Settle tasks whose delivery pull request merged outside Orbit",
    ),
];

const TASK_BUNDLES: &[GroupedRow] = &[
    (
        "export",
        "Export task bundles to a portable tar.zst archive",
    ),
    ("import", "Import task bundles from a tar.zst archive"),
    (
        "publication",
        "Publish, inspect, diagnose, or restore task snapshots",
    ),
    (
        "reindex",
        "Rebuild the registry index from on-disk task bundles",
    ),
];

const ROOT_ENVIRONMENT: &[GroupedRow] = &[
    ("init", "Initialize the global Orbit root (~/.orbit)"),
    ("workspace", "Manage workspaces"),
    ("config", "Show or update Orbit configuration"),
    (
        "host",
        "Register and inspect the remote Orbit hosts this machine reaches over SSH",
    ),
    (
        "browser-kit",
        "Install, verify and remove the headless browser kit workers check UIs with",
    ),
    ("plugin", "Install and manage Orbit plugins"),
    (
        "migrate",
        "Apply or inspect pending .orbit layout/schema migrations",
    ),
    (
        "update",
        "Install a published Orbit release and converge to it",
    ),
];

const ROOT_KNOWLEDGE: &[GroupedRow] = &[
    ("task", "Create, update, and manage tasks"),
    (
        "visual",
        "Capture the working tree beside a task's mocks for comparison",
    ),
    (
        "decision",
        "List, show and answer human decision requests on held work",
    ),
    ("friction", "Report, list, and triage friction records"),
    ("search", "Search tasks and frictions"),
];

const ROOT_OPERATE: &[GroupedRow] = &[
    ("run", "Run workflows, drain the backlog, and inspect runs"),
    ("job", "Define, list, and manage job workflows"),
    (
        "script",
        "Grant and inspect workspace scripts a job step may run",
    ),
    (
        "experiment",
        "Define, run and conclude experiments over agent treatments",
    ),
    ("lease", "Coordinate path leases with the workspace owner"),
    (
        "tool",
        "Manage and run Orbit tools, including external MCP plugins",
    ),
    ("gc", "Inspect and explicitly reap Orbit-managed garbage"),
];

const ROOT_OBSERVE: &[GroupedRow] = &[
    ("audit", "Query the audit event log"),
    (
        "codebase",
        "Rank the repository paths that are risky for agents to change",
    ),
    (
        "agents",
        "Score orchestrators, implementers, reviewers and pilots by crew and model",
    ),
    (
        "crew",
        "Show how crews are laddered across complexity tiers",
    ),
    ("log", "Tail the unified Orbit log feed"),
    (
        "doctor",
        "Diagnose workspace health, provider CLIs, and filesystem access",
    ),
];

const ROOT_SCHEDULER: &[GroupedRow] = &[
    (
        "clock",
        "Inspect, control, and manually tick the machine scheduler",
    ),
    ("sweep", "Compatibility alias for `orbit clock tick`"),
    (
        "routine",
        "Inspect and control scheduled routines on this machine",
    ),
    (
        "auto-task",
        "Define recurring auto-task templates (the scheduler primitive)",
    ),
    (
        "automation",
        "Explain what condition-triggered routines and auto-tasks would match",
    ),
];

const ROOT_SERVICES: &[GroupedRow] = &[
    (
        "mcp",
        "Register MCP client integrations and run the MCP server",
    ),
    ("web", "Run the Orbit dashboard"),
];

/// Display width of a row name in terminal columns (character count is the
/// portable approximation; names are ASCII and descriptions use single-column
/// glyphs).
fn name_width(name: &str) -> usize {
    name.chars().count()
}

/// Description column for a template: indent plus the longest name in the
/// template plus the gap. One column per template keeps every section aligned
/// even when sections have different longest names.
fn template_column(sections: &[&[GroupedRow]]) -> usize {
    let longest = sections
        .iter()
        .flat_map(|rows| rows.iter().map(|(name, _)| name_width(name)))
        .max()
        .unwrap_or(0);
    GROUP_INDENT + longest + GROUP_GAP
}

/// Wrap a description to word boundaries fitting `available` columns.
fn wrap_description(description: &str, available: usize) -> Vec<String> {
    if available == 0 {
        return vec![description.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for word in description.split(' ') {
        let word_len = word.chars().count();
        if current.is_empty() {
            current.push_str(word);
            current_len = word_len;
        } else if current_len + 1 + word_len <= available {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + word_len;
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
            current_len = word_len;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Render grouped rows with a uniform description column. `term_width` is the
/// sink width: `0` (piped) keeps every row on one line; otherwise a row that
/// would exceed the width wraps with continuation lines indented to the
/// column so the command/description association stays readable.
fn render_grouped_rows(
    rows: &[GroupedRow],
    column: usize,
    term_width: Option<usize>,
) -> Vec<String> {
    let global_longest = column
        .saturating_sub(GROUP_INDENT)
        .saturating_sub(GROUP_GAP);
    rows.iter()
        .flat_map(|(name, about)| {
            let mut prefix = String::from("  ");
            prefix.push_str(name);
            for _ in name_width(name)..global_longest {
                prefix.push(' ');
            }
            for _ in 0..GROUP_GAP {
                prefix.push(' ');
            }
            match term_width {
                None | Some(0) => vec![format!("{prefix}{about}")],
                Some(width) => {
                    let prefix_len = prefix.chars().count();
                    let about_len = about.chars().count();
                    if prefix_len + about_len <= width {
                        vec![format!("{prefix}{about}")]
                    } else if width <= column
                        || about
                            .split_whitespace()
                            .next()
                            .is_some_and(|word| word.chars().count() > width - column)
                    {
                        // Stack rows when the description column leaves no room.
                        let name_indent = GROUP_INDENT.min(width.saturating_sub(name_width(name)));
                        let indent = GROUP_INDENT.min(width.saturating_sub(1));
                        let mut out = vec![format!("{}{name}", " ".repeat(name_indent))];
                        for part in wrap_description(about, width - indent) {
                            out.push(format!("{}{}", " ".repeat(indent), part));
                        }
                        out
                    } else {
                        let available = width - column;
                        let mut out = Vec::new();
                        for (index, part) in wrap_description(about, available).iter().enumerate() {
                            if index == 0 {
                                out.push(format!("{prefix}{part}"));
                            } else {
                                out.push(format!("{}{}", " ".repeat(column), part));
                            }
                        }
                        out
                    }
                }
            }
        })
        .collect()
}

fn render_section(
    heading: &str,
    rows: &[GroupedRow],
    column: usize,
    term_width: Option<usize>,
) -> String {
    let mut out = String::from(heading);
    out.push_str(":\n");
    out.push_str(&render_grouped_rows(rows, column, term_width).join("\n"));
    out
}

/// Sink width `0` means piped: keep single-line rows for deterministic output.
fn width_option(term_width: u16) -> Option<usize> {
    if term_width == 0 {
        None
    } else {
        Some(usize::from(term_width))
    }
}

/// Grouped `orbit run` help with a uniform column and width-aware wrapping.
pub(crate) fn run_help_template(term_width: u16) -> String {
    let column = template_column(&[RUN_WORKFLOWS, RUN_AUDITS, RUN_MAINTENANCE]);
    let width = width_option(term_width);
    format!(
        "{{about}}\n\n{{usage-heading}} {{usage}}\n\n{}\n\n{}\n\n{}\n\nOptions:\n{{options}}\n{{after-help}}",
        render_section("Workflows", RUN_WORKFLOWS, column, width),
        render_section("Audits", RUN_AUDITS, column, width),
        render_section("Maintenance", RUN_MAINTENANCE, column, width),
    )
}

/// Grouped `orbit task` help with a uniform column and width-aware wrapping.
pub(crate) fn task_help_template(term_width: u16) -> String {
    let column = template_column(&[TASK_TASKS, TASK_HEALTH, TASK_BUNDLES]);
    let width = width_option(term_width);
    format!(
        "{{about}}\n\n{{usage-heading}} {{usage}}\n\n{}\n\n{}\n\n{}\n\nOptions:\n{{options}}\n{{after-help}}",
        render_section("Tasks", TASK_TASKS, column, width),
        render_section("Health", TASK_HEALTH, column, width),
        render_section("Bundles", TASK_BUNDLES, column, width),
    )
}

/// Grouped root help with a uniform column and width-aware wrapping.
pub(crate) fn root_help_template(term_width: u16) -> String {
    let column = template_column(&[
        ROOT_ENVIRONMENT,
        ROOT_KNOWLEDGE,
        ROOT_OPERATE,
        ROOT_OBSERVE,
        ROOT_SCHEDULER,
        ROOT_SERVICES,
    ]);
    let width = width_option(term_width);
    format!(
        "{{name}} {{version}}\n\n{{usage-heading}} {{usage}}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\nOptions:\n{{options}}",
        render_section("Environment", ROOT_ENVIRONMENT, column, width),
        render_section("Knowledge", ROOT_KNOWLEDGE, column, width),
        render_section("Operate", ROOT_OPERATE, column, width),
        render_section("Observe", ROOT_OBSERVE, column, width),
        render_section("Scheduler", ROOT_SCHEDULER, column, width),
        render_section("Services", ROOT_SERVICES, column, width),
    )
}
