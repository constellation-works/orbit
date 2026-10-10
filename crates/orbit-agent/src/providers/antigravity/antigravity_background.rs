//! Antigravity (`agy`) background-task classification. [ORB-15243]
//!
//! `agy` can start a command as a background task and end its headless turn
//! while that task still runs: since 1.3 a `-p` run waits for background tasks
//! only until `--print-timeout`, capped at 30 minutes, then exits 0 with
//! whatever it has. The stream carries no Orbit envelope, so completion is
//! already refused; this module only names the cause. It reads the tail of
//! the raw stdout for the agent's last `manage_task` style status reports
//! (`task-34 ... running`) and never decides success.

use std::time::Duration;

use serde_json::Value;

use super::antigravity_output::antigravity_terminal_response;

/// Only the newest lines can say what a task was doing when the turn ended.
const TAIL_LINES: usize = 200;
/// Longest slice of one line read for a status report.
const LINE_CHAR_LIMIT: usize = 16 * 1024;
/// Longest task id copied into a diagnostic; ids come from agent output.
const TASK_ID_LIMIT_CHARS: usize = 64;
/// Most running tasks a diagnostic names.
const NAMED_TASK_LIMIT: usize = 5;

const TASK_PREFIX: &str = "task-";
const RUNNING_WORDS: &[&str] = &["running", "in_progress", "in progress", "active"];
const FINISHED_WORDS: &[&str] = &[
    "completed",
    "complete",
    "succeeded",
    "finished",
    "failed",
    "exited",
    "killed",
    "terminated",
    "stopped",
    "cancelled",
    "canceled",
    "done",
];

/// Background tasks the stream last reported as running, oldest first.
///
/// A status line is a line that names a `task-<id>` token and a status word
/// and sits next to a mention of `manage_task` or a background task or command
/// (same line or the one before, so a tool call and its result pair up). The
/// newest report per id wins. Output without such a report yields nothing, so
/// ordinary progress text such as "Background command still running" does not
/// classify.
pub(crate) fn running_background_tasks(stdout: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(stdout);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let start = lines.len().saturating_sub(TAIL_LINES);
    let mut states: Vec<(String, bool)> = Vec::new();
    let mut previous_mentions_background = false;
    for line in &lines[start..] {
        let flat = flatten_line(line).to_lowercase();
        let mentions_background = mentions_background(&flat);
        if mentions_background || previous_mentions_background {
            for (id, running) in task_statuses(&flat) {
                match states.iter_mut().find(|(known, _)| *known == id) {
                    Some(state) => state.1 = running,
                    None => states.push((id, running)),
                }
            }
        }
        previous_mentions_background = mentions_background;
    }
    states
        .into_iter()
        .filter_map(|(id, running)| running.then_some(id))
        .collect()
}

/// Provider-gated step diagnostic for an `agy` run that exited 0 without an
/// Orbit envelope while a background task it started was still running.
///
/// The agent's last words come from the terminal `result` response, when the
/// stream has one, passed through `bound` (the caller's redaction and size
/// limit) before they join the text. Returns `None` for another provider or when no running task was reported,
/// so the generic completion violation stands.
pub fn antigravity_background_task_diagnostic(
    provider: &str,
    stdout: &[u8],
    elapsed: Duration,
    bound: impl Fn(&str) -> String,
) -> Option<String> {
    if provider != "antigravity" && provider != "agy" {
        return None;
    }
    let running = running_background_tasks(stdout);
    if running.is_empty() {
        return None;
    }
    let named: Vec<&str> = running
        .iter()
        .rev()
        .take(NAMED_TASK_LIMIT)
        .rev()
        .map(String::as_str)
        .collect();
    let more = running.len() - named.len();
    let mut tasks = named.join(", ");
    if more > 0 {
        tasks.push_str(&format!(" and {more} more"));
    }
    let message = match antigravity_terminal_response(stdout) {
        Some(text) => format!(" Last message: {}", bound(&text)),
        None => String::new(),
    };
    Some(format!(
        "agent step did not complete: antigravity ended its turn and exited 0 after {} ms \
         without a terminating Orbit response envelope while background task {tasks} was still \
         running. The agent started work it did not wait for (agy waits for background tasks \
         only until its --print-timeout, at most 30 minutes), so any `SUCCESS` wrapper, exit \
         code 0 or persisted summary is not completion; only what the run persisted before \
         stopping is durable.{message}",
        elapsed.as_millis(),
    ))
}

fn mentions_background(lowercase: &str) -> bool {
    lowercase.contains("manage_task") || lowercase.contains("background")
}

/// One stdout line as plain text: every string leaf of a JSON event, or the
/// raw line when it is not JSON.
fn flatten_line(line: &str) -> String {
    let flat = match serde_json::from_str::<Value>(line) {
        Ok(value) => {
            let mut out = String::new();
            push_strings(&value, &mut out);
            out
        }
        Err(_) => line.to_string(),
    };
    flat.chars().take(LINE_CHAR_LIMIT).collect()
}

fn push_strings(value: &Value, out: &mut String) {
    match value {
        Value::String(text) => {
            out.push_str(text);
            out.push(' ');
        }
        Value::Array(items) => items.iter().for_each(|item| push_strings(item, out)),
        Value::Object(object) => object.values().for_each(|item| push_strings(item, out)),
        _ => {}
    }
}

/// `(task id, running)` for each `task-<id>` token in lowercase text. The
/// status is the first status word after the token and before the next one;
/// a key-sorted JSON event can put it before the id, so the nearest word
/// before the token is the fallback. Tokens with no status word are skipped.
fn task_statuses(lowercase: &str) -> Vec<(String, bool)> {
    let mut tokens: Vec<(usize, usize, String)> = Vec::new();
    let mut from = 0;
    while let Some(offset) = lowercase[from..].find(TASK_PREFIX) {
        let start = from + offset;
        let after = start + TASK_PREFIX.len();
        from = after;
        let boundary = lowercase[..start]
            .chars()
            .next_back()
            .is_none_or(|prev| !prev.is_alphanumeric() && prev != '_');
        if !boundary {
            continue;
        }
        let suffix: String = lowercase[after..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        let suffix = suffix.trim_end_matches(['-', '_']);
        if suffix.is_empty() {
            continue;
        }
        let end = after + suffix.len();
        let id: String = format!("{TASK_PREFIX}{suffix}")
            .chars()
            .take(TASK_ID_LIMIT_CHARS)
            .collect();
        tokens.push((start, end, id));
    }
    let mut statuses = Vec::new();
    for (index, (start, end, id)) in tokens.iter().enumerate() {
        let next_start = tokens
            .get(index + 1)
            .map_or(lowercase.len(), |(next, _, _)| *next);
        let previous_end = index
            .checked_sub(1)
            .map_or(0, |previous| tokens[previous].1);
        let running = status_after(&lowercase[*end..next_start])
            .or_else(|| status_before(&lowercase[previous_end..*start]));
        if let Some(running) = running {
            statuses.push((id.clone(), running));
        }
    }
    statuses
}

fn status_after(segment: &str) -> Option<bool> {
    status_words(segment)
        .min_by_key(|(at, _)| *at)
        .map(|(_, running)| running)
}

fn status_before(segment: &str) -> Option<bool> {
    status_words(segment)
        .max_by_key(|(at, _)| *at)
        .map(|(_, running)| running)
}

/// Whole-word status matches as `(position, is_running)`.
fn status_words(segment: &str) -> impl Iterator<Item = (usize, bool)> + '_ {
    let running = RUNNING_WORDS.iter().map(|word| (*word, true));
    let finished = FINISHED_WORDS.iter().map(|word| (*word, false));
    running.chain(finished).flat_map(move |(word, is_running)| {
        segment.match_indices(word).filter_map(move |(at, _)| {
            let before = segment[..at].chars().next_back();
            let after = segment[at + word.len()..].chars().next();
            let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
            (edge(before) && edge(after)).then_some((at, is_running))
        })
    })
}
