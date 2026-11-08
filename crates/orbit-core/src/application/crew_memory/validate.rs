//! The five write rules every crew memory write passes (design §4). A write
//! that breaks one is refused whole, naming the rule and, for the lints, the
//! matched token. The lints are lexical and deliberately coarse.

use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use super::{MemoryKey, MemoryWrite, MemoryWriter};

/// Most files a model space holds.
pub const MODEL_SPACE_MAX_FILES: usize = 8;
/// Most UTF-8 bytes a model space holds across its files.
pub const MODEL_SPACE_MAX_BYTES: usize = 6 * 1024;
/// Most files the global memory holds.
pub const GLOBAL_MAX_FILES: usize = 1;
/// Most UTF-8 bytes the global memory holds.
pub const GLOBAL_MAX_BYTES: usize = 3 * 1024;
/// Longest file name stem before `.md`.
const MAX_NAME_STEM: usize = 40;

/// Scorecard words refused with any suffix (`scores`, `ranked`, `weights`).
const SCORE_STEMS: &[&str] = &["score", "rank", "weight"];
/// Scorecard phrases refused as whole words.
const SCORE_PHRASES: &[&str] = &["mvp", "fleet average", "per 100"];
/// Phrases that address the agent harness rather than the work.
const INJECTION_PHRASES: &[&str] = &[
    "ignore previous",
    "ignore all previous",
    "ignore the above",
    "ignore your instructions",
    "disregard previous",
    "disregard the above",
    "disregard your instructions",
    "override your instructions",
    "system prompt",
    "developer message",
    "you are now",
    "new instructions",
    "you must not review",
    "do not review",
    "skip review",
    "skip the review",
];
/// Fence info strings that mark a shell command block.
const SHELL_FENCES: &[&str] = &[
    "",
    "sh",
    "bash",
    "shell",
    "zsh",
    "fish",
    "console",
    "shell-session",
    "sh-session",
    "powershell",
    "pwsh",
    "ps1",
    "cmd",
    "bat",
    "terminal",
];

/// One of the five write rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRule {
    /// File count, total size and file names.
    Caps,
    /// Evidence citation.
    Provenance,
    /// No scorecard vocabulary and no other crew's or model's name.
    ScoreBlind,
    /// No URL, Orbit tool name, shell command block or injection phrase.
    NotInstructions,
    /// A write carries the whole space, each file once.
    RewriteWhole,
}

impl MemoryRule {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Caps => "caps",
            Self::Provenance => "provenance",
            Self::ScoreBlind => "score_blind",
            Self::NotInstructions => "not_instructions",
            Self::RewriteWhole => "rewrite_whole",
        }
    }
}

/// Why a write was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryWriteRefusal {
    pub rule: MemoryRule,
    /// The file the rule failed on, when one did.
    pub file: Option<String>,
    /// The matched token, for the lints.
    pub token: Option<String>,
    pub message: String,
}

impl fmt::Display for MemoryWriteRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "crew memory write refused by rule `{}`: {}",
            self.rule.as_str(),
            self.message
        )
    }
}

/// The names the score-blind lint refuses, resolved by the caller from the
/// scorecards and the configured crews.
#[derive(Debug, Clone, Default)]
pub struct MemoryLintVocabulary {
    /// Scorecard term keys and labels.
    pub scorecard_terms: Vec<String>,
    /// Crew names and model ids other than the written space's own.
    pub other_names: Vec<String>,
}

/// What a write is checked against.
pub struct MemoryWriteContext<'a> {
    pub key: &'a MemoryKey,
    pub writer: &'a MemoryWriter,
    pub vocabulary: &'a MemoryLintVocabulary,
    /// The writing run's evidence pack task ids: a dreaming run may cite
    /// only these. `None` for an operator write.
    pub evidence_pack: Option<&'a BTreeSet<String>>,
}

/// Check `write` against every rule, in order: rewrite-whole, caps,
/// provenance, score-blind, not-instructions. The first broken rule refuses
/// the whole write.
pub fn validate_memory_write(
    write: &MemoryWrite,
    context: &MemoryWriteContext<'_>,
) -> Result<(), MemoryWriteRefusal> {
    rewrite_whole(write)?;
    caps(write, context.key)?;
    provenance(write, context.writer, context.evidence_pack)?;
    for file in &write.files {
        let text = normalized(&file.text);
        score_blind(&file.path, &text, context.vocabulary)?;
        not_instructions(&file.path, &file.text, &text)?;
    }
    Ok(())
}

fn refusal(
    rule: MemoryRule,
    file: Option<&str>,
    token: Option<&str>,
    message: String,
) -> MemoryWriteRefusal {
    MemoryWriteRefusal {
        rule,
        file: file.map(str::to_string),
        token: token.map(str::to_string),
        message,
    }
}

fn rewrite_whole(write: &MemoryWrite) -> Result<(), MemoryWriteRefusal> {
    let mut seen = BTreeSet::new();
    for file in &write.files {
        if !seen.insert(file.path.as_str()) {
            return Err(refusal(
                MemoryRule::RewriteWhole,
                Some(&file.path),
                None,
                format!(
                    "file `{}` is given twice; a write carries every file of the space once and replaces the whole space",
                    file.path
                ),
            ));
        }
    }
    Ok(())
}

fn caps(write: &MemoryWrite, key: &MemoryKey) -> Result<(), MemoryWriteRefusal> {
    let (max_files, max_bytes, space) = if key.is_global() {
        (GLOBAL_MAX_FILES, GLOBAL_MAX_BYTES, "the global memory")
    } else {
        (
            MODEL_SPACE_MAX_FILES,
            MODEL_SPACE_MAX_BYTES,
            "a model space",
        )
    };
    for file in &write.files {
        if !valid_file_name(&file.path) {
            return Err(refusal(
                MemoryRule::Caps,
                Some(&file.path),
                None,
                format!(
                    "file name `{}` must match [a-z0-9-]{{1,{MAX_NAME_STEM}}}.md",
                    file.path
                ),
            ));
        }
    }
    if write.files.len() > max_files {
        return Err(refusal(
            MemoryRule::Caps,
            None,
            None,
            format!(
                "{} files exceed {space}'s cap of {max_files}",
                write.files.len()
            ),
        ));
    }
    let bytes: usize = write.files.iter().map(|file| file.text.len()).sum();
    if bytes > max_bytes {
        return Err(refusal(
            MemoryRule::Caps,
            None,
            None,
            format!("{bytes} bytes exceed {space}'s cap of {max_bytes} bytes of UTF-8"),
        ));
    }
    Ok(())
}

fn valid_file_name(name: &str) -> bool {
    name.strip_suffix(".md").is_some_and(|stem| {
        (1..=MAX_NAME_STEM).contains(&stem.len())
            && stem
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

fn provenance(
    write: &MemoryWrite,
    writer: &MemoryWriter,
    evidence_pack: Option<&BTreeSet<String>>,
) -> Result<(), MemoryWriteRefusal> {
    if let Some(bad) = write.evidence.iter().find(|id| !valid_task_id(id)) {
        return Err(refusal(
            MemoryRule::Provenance,
            None,
            Some(bad),
            format!("evidence `{bad}` is not a task id (a prefix, a hyphen and a number)"),
        ));
    }
    if matches!(writer, MemoryWriter::Run { .. }) && write.evidence.is_empty() {
        return Err(refusal(
            MemoryRule::Provenance,
            None,
            None,
            "a run's write must cite at least one evidence task id".to_string(),
        ));
    }
    if let Some(pack) = evidence_pack
        && let Some(outside) = write.evidence.iter().find(|id| !pack.contains(*id))
    {
        return Err(refusal(
            MemoryRule::Provenance,
            None,
            Some(outside),
            format!(
                "evidence `{outside}` is not in this run's evidence pack; cite only tasks the pack names"
            ),
        ));
    }
    Ok(())
}

fn valid_task_id(id: &str) -> bool {
    id.split_once('-').is_some_and(|(prefix, number)| {
        prefix.starts_with(|c: char| c.is_ascii_alphabetic())
            && prefix.chars().all(|c| c.is_ascii_alphanumeric())
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
    })
}

fn score_blind(
    path: &str,
    text: &str,
    vocabulary: &MemoryLintVocabulary,
) -> Result<(), MemoryWriteRefusal> {
    let vocabulary_hit = SCORE_STEMS
        .iter()
        .copied()
        .find(|stem| contains_word(text, stem, true, is_word_char))
        .or_else(|| {
            SCORE_PHRASES
                .iter()
                .copied()
                .find(|phrase| contains_word(text, phrase, false, is_word_char))
        })
        .map(str::to_string)
        .or_else(|| {
            vocabulary
                .scorecard_terms
                .iter()
                .map(|term| normalized(term))
                .find(|term| contains_word(text, term, false, is_word_char))
        });
    if let Some(token) = vocabulary_hit {
        return Err(refusal(
            MemoryRule::ScoreBlind,
            Some(path),
            Some(&token),
            format!(
                "file `{path}` uses scorecard vocabulary `{token}`; describe the work, not how it is measured"
            ),
        ));
    }
    if let Some(name) = vocabulary
        .other_names
        .iter()
        .map(|name| name.to_lowercase())
        .find(|name| contains_word(text, name, false, is_name_char))
    {
        return Err(refusal(
            MemoryRule::ScoreBlind,
            Some(path),
            Some(&name),
            format!(
                "file `{path}` names `{name}`, another crew or model; a space may name only its own model"
            ),
        ));
    }
    Ok(())
}

/// Replacement for a token [`strip_score_blind_tokens`] removes.
const STRIPPED: &str = "[…]";

/// `text` with every token the score-blind lint would refuse replaced by
/// `[…]`: scorecard words (with any suffix for the stems), scorecard term
/// keys and labels, and other crews' and models' names. Whitespace runs
/// collapse to one space. The dreaming evidence pack passes its free text
/// (titles, findings, occurrence details) through this, so the pack does not
/// teach the vocabulary a write may not use. Matching is ASCII
/// case-insensitive.
pub(crate) fn strip_score_blind_tokens(text: &str, vocabulary: &MemoryLintVocabulary) -> String {
    let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let word_token = |token: String, allow_suffix: bool| StripToken {
        token,
        allow_suffix,
        word: is_word_char,
    };
    let mut tokens: Vec<StripToken> = Vec::new();
    tokens.extend(
        SCORE_STEMS
            .iter()
            .map(|stem| word_token((*stem).to_string(), true)),
    );
    tokens.extend(
        SCORE_PHRASES
            .iter()
            .map(|phrase| word_token((*phrase).to_string(), false)),
    );
    tokens.extend(
        vocabulary
            .scorecard_terms
            .iter()
            .map(|term| word_token(normalized(term), false)),
    );
    tokens.extend(vocabulary.other_names.iter().map(|name| StripToken {
        token: name.to_ascii_lowercase(),
        allow_suffix: false,
        word: is_name_char,
    }));
    // Longer tokens first, so a label is stripped whole before a stem in it.
    tokens.sort_by_key(|token| std::cmp::Reverse(token.token.len()));
    for token in tokens {
        if token.token.is_empty() {
            continue;
        }
        text = strip_word(&text, &token.token, token.allow_suffix, token.word);
    }
    text
}

/// One token [`strip_score_blind_tokens`] removes: its ASCII-lowercase
/// text, whether a match extends over a suffix, and what counts as a word
/// character at its boundaries.
struct StripToken {
    token: String,
    allow_suffix: bool,
    word: fn(char) -> bool,
}

/// Replace each bounded occurrence of `token` (ASCII lowercase) in `text`,
/// extending a suffix match to the end of its word.
fn strip_word(text: &str, token: &str, allow_suffix: bool, word: fn(char) -> bool) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (at, _) in lower.match_indices(token) {
        if at < cursor {
            continue;
        }
        let before = text[..at].chars().next_back();
        let after_at = at + token.len();
        let after = text[after_at..].chars().next();
        let first = token.chars().next().is_some_and(word);
        let last = token.chars().next_back().is_some_and(word);
        if first && before.is_some_and(word) {
            continue;
        }
        let end = if allow_suffix {
            text[after_at..]
                .find(|c: char| !word(c))
                .map_or(text.len(), |stop| after_at + stop)
        } else {
            if last && after.is_some_and(word) {
                continue;
            }
            after_at
        };
        out.push_str(&text[cursor..at]);
        out.push_str(STRIPPED);
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn not_instructions(path: &str, raw: &str, text: &str) -> Result<(), MemoryWriteRefusal> {
    let refuse = |token: String, what: &str| {
        refusal(
            MemoryRule::NotInstructions,
            Some(path),
            Some(&token),
            format!(
                "file `{path}` contains {what} `{token}`; notes may say how to do the work, not what Orbit or the agent harness should do"
            ),
        )
    };
    if let Some(url) = url_token(text) {
        return Err(refuse(url, "a URL"));
    }
    if let Some(tool) = tool_name_token(text) {
        return Err(refuse(tool, "an Orbit tool name"));
    }
    if let Some(fence) = shell_fence_token(raw) {
        return Err(refuse(fence, "a shell command block"));
    }
    if let Some(phrase) = INJECTION_PHRASES
        .iter()
        .find(|phrase| contains_word(text, phrase, false, is_word_char))
    {
        return Err(refuse((*phrase).to_string(), "the injection phrase"));
    }
    Ok(())
}

fn url_token(text: &str) -> Option<String> {
    ["://", "www."].iter().find_map(|marker| {
        let at = text.find(marker)?;
        let start = text[..at]
            .rfind(char::is_whitespace)
            .map_or(0, |space| space + 1);
        let end = text[at..]
            .find(char::is_whitespace)
            .map_or(text.len(), |space| at + space);
        Some(truncate(&text[start..end]))
    })
}

fn tool_name_token(text: &str) -> Option<String> {
    ["orbit.", "orbit_"].iter().find_map(|prefix| {
        text.match_indices(prefix).find_map(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + prefix.len()..].chars().next();
            let bounded = before.is_none_or(|c| !is_word_char(c));
            (bounded && after.is_some_and(|c| c.is_ascii_lowercase())).then(|| {
                let end = text[at..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_'))
                    .map_or(text.len(), |stop| at + stop);
                truncate(text[at..end].trim_end_matches('.'))
            })
        })
    })
}

/// The opening line of the first fenced block whose info string is empty or
/// a shell, tracking fences so a closing fence is never mistaken for one.
fn shell_fence_token(raw: &str) -> Option<String> {
    let mut open: Option<&str> = None;
    for line in raw.lines() {
        let line = line.trim_start();
        let Some(marker) = ["```", "~~~"].into_iter().find(|m| line.starts_with(m)) else {
            continue;
        };
        if let Some(fence) = open {
            if marker == fence {
                open = None;
            }
            continue;
        }
        open = Some(marker);
        let info = line.trim_start_matches(marker.chars().next().unwrap_or('`'));
        let language = info
            .split(|c: char| c.is_whitespace() || c == '{' || c == ',')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if SHELL_FENCES.contains(&language.as_str()) {
            return Some(truncate(line.trim_end()));
        }
    }
    None
}

/// Lowercase with every whitespace run collapsed to one space.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Crew and model names run on hyphens and dots (`claude-opus-5-5`), so a
/// name inside a longer id is not a match.
fn is_name_char(c: char) -> bool {
    is_word_char(c) || c == '-'
}

/// Whether `token` occurs in `text` starting at a word boundary and, unless
/// `allow_suffix`, ending at one.
fn contains_word(text: &str, token: &str, allow_suffix: bool, word: fn(char) -> bool) -> bool {
    !token.is_empty()
        && text.match_indices(token).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + token.len()..].chars().next();
            let first = token.chars().next().is_some_and(word);
            let last = token.chars().next_back().is_some_and(word);
            (!first || before.is_none_or(|c| !word(c)))
                && (allow_suffix || !last || after.is_none_or(|c| !word(c)))
        })
}

fn truncate(token: &str) -> String {
    const MAX: usize = 60;
    match token.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &token[..cut]),
        None => token.to_string(),
    }
}
