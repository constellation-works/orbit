//! Hold a spec that leaves a consequential decision for the implementer.
//!
//! The activity prompt asks the inspecting pilot to return this finding,
//! including when the omission is visible only by comparing pinned source
//! with the spec. Apply still attaches it when the description itself states
//! the decision and the model did not. That is how an open ownership policy
//! [ORB-21111] and an unenumerated surface a stop rule reaches [ORB-21174]
//! arrived at an implementer with no hold. Ordinary implementation choices,
//! a resolved choice, and a stop-and-report that is itself the specified
//! handling do not match.

use serde_json::{Value, json};

use super::promotion::UNDECIDED_CONSEQUENTIAL_DECISION;

const STOP_WINDOW_CHARS: usize = 160;

const OPEN_MARKERS: [&str; 9] = [
    "choose whether",
    "undecided",
    "left open",
    "unsettled",
    "not decided",
    "must choose",
    "does not choose",
    "do not choose",
    "decision required",
];

/// Same-sentence phrases that cancel an ownership or policy reading.
/// "ordinary implementation" keeps a naming policy that the spec already
/// classifies as an implementer choice from holding.
const OWNERSHIP_NEGATIONS: [&str; 6] = [
    "no ownership",
    "no policy",
    "not an ownership",
    "not a policy",
    "not undecided",
    "ordinary implementation",
];

/// Later-sentence verbs that state which ownership or policy to keep.
/// A same-sentence open marker still wins. A negated verb ("do not
/// preserve", "doesn't keep") is not a resolution [ORB-23274]. "not only ...
/// but also ..." affirms the verb.
const OWNERSHIP_RESOLUTION_VERBS: [&str; 9] = [
    "preserve",
    "preserves",
    "preserving",
    "keep",
    "keeps",
    "keeping",
    "retain",
    "retains",
    "retaining",
];

const UNLISTED_CUES: [&str; 9] = [
    "unlisted",
    "does not enumerate",
    "do not enumerate",
    "not enumerated",
    "does not list",
    "do not list",
    "not listed",
    "does not name",
    "do not name",
];

const SURFACE_CUES: [&str; 4] = [
    "surface",
    "behavioural difference",
    "behavioral difference",
    "difference table",
];

/// A sentence that already says what an unnamed difference does. The
/// code-health stop-and-report ("not a judgement call") is the specified
/// handling, and a table that keeps current behavior for unnamed fields is
/// a resolved surface [ORB-21174].
const RESOLUTION_CUES: [&str; 12] = [
    "does not stop",
    "do not stop",
    "keeps its current",
    "keep its current",
    "keeps their current",
    "keep their current",
    "retains current",
    "retain current",
    "preserve unspecified",
    "preserves unspecified",
    "not a judgement call",
    "not a judgment call",
];

/// Attach holding `utility_warnings` for each consequential decision the
/// description states and does not resolve. A model finding that already
/// names the same decision is stamped so it holds; unrelated warnings,
/// including an advisory ordinary choice, stay as the model returned them.
pub(super) fn attach_undecided_consequential_decisions(assessment: &mut Value, description: &str) {
    let decisions = decisions_in(description);
    if decisions.is_empty() {
        return;
    }
    let Some(warnings) = utility_warnings_mut(assessment) else {
        return;
    };
    for decision in decisions {
        let finding = finding_value(&decision);
        if let Some(existing) = warnings
            .iter_mut()
            .find(|item| finding_names(item, &decision.name))
        {
            stamp_existing(existing, &finding);
        } else {
            warnings.push(finding);
        }
    }
}

struct Decision {
    /// Substring that identifies this decision in an existing finding.
    name: String,
    text: String,
}

fn decisions_in(description: &str) -> Vec<Decision> {
    let mut decisions = Vec::new();
    if let Some(name) = ownership_name(description) {
        decisions.push(Decision {
            text: format!(
                "Undecided consequential decision: {name}. The spec leaves this choice open, \
                 so the task is held until the description states it. An ordinary implementation \
                 choice (approach, file split, naming, test placement) is not this hold."
            ),
            name: name.to_string(),
        });
    }
    if let Some(subject) = surface_subject(description) {
        decisions.push(Decision {
            text: format!(
                "Undecided consequential decision: {subject} is a surface difference the stop \
                 rule reaches and the spec does not enumerate. The task is held until the \
                 description enumerates it or states that an unlisted difference keeps current \
                 behavior."
            ),
            name: subject,
        });
    }
    decisions
}

fn finding_value(decision: &Decision) -> Value {
    json!({
        "text": decision.text,
        "holds_promotion": true,
        "hold": UNDECIDED_CONSEQUENTIAL_DECISION,
    })
}

fn stamp_existing(existing: &mut Value, canonical: &Value) {
    if let Some(object) = existing.as_object_mut() {
        object.insert("holds_promotion".to_string(), json!(true));
        object.insert("hold".to_string(), json!(UNDECIDED_CONSEQUENTIAL_DECISION));
        let missing_text = object
            .get("text")
            .and_then(Value::as_str)
            .is_none_or(|text| text.trim().is_empty());
        if missing_text && let Some(text) = canonical.get("text") {
            object.insert("text".to_string(), text.clone());
        }
    } else {
        // A legacy string cannot carry the hold marker. Replace it with the
        // object so a done or dependency id in the prose cannot release it.
        *existing = canonical.clone();
    }
}

fn finding_names(finding: &Value, name: &str) -> bool {
    let Some(text) = finding
        .as_str()
        .or_else(|| finding.get("text").and_then(Value::as_str))
    else {
        return false;
    };
    text.to_ascii_lowercase()
        .contains(&name.to_ascii_lowercase())
}

fn utility_warnings_mut(assessment: &mut Value) -> Option<&mut Vec<Value>> {
    let object = assessment.as_object_mut()?;
    if !object.get("utility_warnings").is_some_and(Value::is_array) {
        object.insert("utility_warnings".to_string(), json!([]));
    }
    object
        .get_mut("utility_warnings")
        .and_then(Value::as_array_mut)
}

/// Ownership or policy left open in one sentence, unless a later sentence
/// resolves that same choice. An open marker wins over settlement words in
/// that sentence: "choose whether to preserve the current ownership policy"
/// is still undecided. A later sentence that only says "undecided" does not
/// attach the choice named in a different sentence. A later keep, preserve
/// or retain of the same choice, with no open marker of its own, clears that
/// choice [ORB-23274]. A resolution that precedes the open marker does not,
/// and a negated verb ("do not preserve") is not a resolution. "not only ...
/// but also ..." affirms the verb and does resolve the choice.
fn ownership_name(description: &str) -> Option<&'static str> {
    let mut open = Vec::new();
    for sentence in sentences(description) {
        let lower = sentence.to_ascii_lowercase();
        if OWNERSHIP_NEGATIONS.iter().any(|cue| lower.contains(cue)) {
            continue;
        }
        let Some(name) = ownership_choice_name(&lower) else {
            continue;
        };
        let identity = ownership_choice_identity(&lower, name);
        if OPEN_MARKERS.iter().any(|cue| lower.contains(cue)) {
            if !open.contains(&(name, identity.clone())) {
                open.push((name, identity));
            }
            continue;
        }
        if ownership_resolution(&lower) {
            open.retain(|current| *current != (name, identity.clone()));
        }
    }
    open.into_iter()
        .max_by_key(|(name, _)| rank(name))
        .map(|(name, _)| name)
}

/// Retain the policy's modifiers independently from its user-facing label.
/// An unqualified policy only resolves another unqualified policy; ambiguity
/// should leave a hold in place rather than clear a potentially different one.
fn ownership_choice_identity(lower: &str, name: &str) -> String {
    if name == "the ownership choice" {
        return name.to_string();
    }
    let mut modifiers = Vec::new();
    for word in lower.split(|c: char| !c.is_alphanumeric() && c != '-') {
        if word == "policy" {
            break;
        }
        if matches!(
            word,
            "" | "the"
                | "a"
                | "an"
                | "current"
                | "existing"
                | "preserve"
                | "preserves"
                | "preserving"
                | "keep"
                | "keeps"
                | "keeping"
                | "retain"
                | "retains"
                | "retaining"
                | "whether"
                | "to"
                | "is"
        ) {
            modifiers.clear();
        } else {
            modifiers.push(word);
        }
    }
    modifiers.join(" ")
}

fn ownership_choice_name(lower: &str) -> Option<&'static str> {
    let ownership = contains_term(lower, "ownership");
    let policy = contains_term(lower, "policy");
    if !ownership && !policy {
        return None;
    }
    Some(if lower.contains("ownership policy") {
        "the ownership policy"
    } else if ownership {
        "the ownership choice"
    } else {
        "the policy choice"
    })
}

/// A keep, preserve or retain that is not itself negated. "do not preserve"
/// and "doesn't keep" leave the choice open. "not only preserve ... but also ..."
/// affirms the verb and resolves the choice.
fn ownership_resolution(lower: &str) -> bool {
    OWNERSHIP_RESOLUTION_VERBS
        .iter()
        .any(|verb| resolution_verb_applies(lower, verb))
}

fn resolution_verb_applies(lower: &str, verb: &str) -> bool {
    let mut from = 0;
    while let Some(relative) = lower[from..].find(verb) {
        let at = from + relative;
        let end = at + verb.len();
        let word = (at == 0 || !lower.as_bytes()[at - 1].is_ascii_alphanumeric())
            && (end >= lower.len() || !lower.as_bytes()[end].is_ascii_alphanumeric());
        if word && !resolution_verb_negated(lower, at) {
            return true;
        }
        from = end;
        if from >= lower.len() {
            break;
        }
    }
    false
}

fn resolution_verb_negated(lower: &str, verb_at: usize) -> bool {
    // Walk back over any intervening adverbs ("do not automatically
    // preserve", "never automatically keep"); a negation within that
    // short window negates the verb, an unrelated word ends the scan so
    // an affirmative verb preceded by an adverb still resolves.
    // "only" ends in "ly", so the walk reaches "not" in "not only
    // preserve ... but also ...". That correlative affirms the verb.
    // A typographic apostrophe ("don’t") reads as the ASCII one, and "no"
    // negates only as "no longer".
    let prefix = lower[..verb_at].trim_end();
    let mut scanned = 0;
    let mut only_in_window = false;
    let mut after_longer = false;
    for token in prefix.rsplit(|c: char| c.is_whitespace()) {
        let token = token
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '\u{2019}')
            .replace('\u{2019}', "'");
        let token = token.as_str();
        if token.is_empty() {
            continue;
        }
        scanned += 1;
        if scanned > 4 {
            break;
        }
        let no_longer = token == "no" && after_longer;
        after_longer = token == "longer";
        if no_longer
            || token == "not"
            || token == "never"
            || token == "cannot"
            || token.ends_with("n't")
        {
            if token == "not" && only_in_window && not_only_but_also_in_sentence(lower, verb_at) {
                only_in_window = false;
                continue;
            }
            return true;
        }
        if token == "only" {
            only_in_window = true;
        }
        if token.ends_with("ly")
            || matches!(
                token,
                "ever"
                    | "just"
                    | "also"
                    | "always"
                    | "even"
                    | "still"
                    | "longer"
                    | "now"
                    | "again"
                    | "or"
                    | "and"
            )
            || OWNERSHIP_RESOLUTION_VERBS.contains(&token)
        {
            continue;
        }
        break;
    }
    false
}

/// "not only <verb> ... but also ..." affirms the verb. The correlative has to
/// sit in the same sentence; a later sentence's "but also" does not.
fn not_only_but_also_in_sentence(lower: &str, verb_at: usize) -> bool {
    let start = lower[..verb_at]
        .rfind(['.', '!', '?', '\n'])
        .map_or(0, |index| index + 1);
    let end = lower[verb_at..]
        .find(['.', '!', '?', '\n'])
        .map_or(lower.len(), |index| verb_at + index);
    contains_term(&lower[start..end], "but also")
}

fn rank(name: &str) -> u8 {
    match name {
        "the ownership policy" => 3,
        "the ownership choice" => 2,
        _ => 1,
    }
}

/// A stop rule that reaches an unlisted surface, plus a concrete omission,
/// and no sentence that already resolves unnamed differences.
fn surface_subject(description: &str) -> Option<String> {
    let split = sentences(description);
    if surface_resolved(&split) {
        return None;
    }
    let flat = split.join(" ").to_ascii_lowercase();
    if !stop_rule_reaches_unlisted_surface(&flat) {
        return None;
    }
    let subject = first_omission(&split)?;
    usable_subject(&subject).then_some(subject)
}

/// A resolution cue settles unnamed differences only in a sentence that also
/// names the unlisted surface. "Any field the table does not name keeps its
/// current behavior" does; "The retry timeout retains current behavior" is
/// about something else and leaves the omitted surface open [ORB-23275].
fn surface_resolved(sentences: &[String]) -> bool {
    sentences.iter().any(|sentence| {
        let lower = sentence.to_ascii_lowercase();
        RESOLUTION_CUES.iter().any(|cue| lower.contains(cue))
            && (lower.contains("unnamed")
                || SURFACE_CUES.iter().any(|cue| lower.contains(cue))
                || UNLISTED_CUES.iter().any(|cue| lower.contains(cue)))
    })
}

fn stop_rule_reaches_unlisted_surface(lower: &str) -> bool {
    let mut from = 0;
    while let Some(relative) = lower[from..].find("stop") {
        let at = from + relative;
        let start = floor_char(lower, at.saturating_sub(STOP_WINDOW_CHARS));
        let end = ceil_char(
            lower,
            (at + "stop".len() + STOP_WINDOW_CHARS).min(lower.len()),
        );
        let window = &lower[start..end];
        let unlisted = UNLISTED_CUES.iter().any(|cue| window.contains(cue));
        let surface = SURFACE_CUES.iter().any(|cue| window.contains(cue));
        if unlisted && surface {
            return true;
        }
        from = at + "stop".len();
        if from >= lower.len() {
            break;
        }
    }
    false
}

/// The subject of the first omission phrase, read from the first sentence that
/// has one, so a phrase wrapped over lines keeps its subject.
fn first_omission(sentences: &[String]) -> Option<String> {
    sentences
        .iter()
        .find_map(|sentence| omission_subject(sentence))
}

/// The subject of the first omission phrase in one sentence. The subject ends
/// at a markdown table cell separator `|`, so a table row yields only the cell
/// that holds the phrase [ORB-25039].
fn omission_subject(sentence: &str) -> Option<String> {
    let lower = sentence.to_ascii_lowercase();
    let mut best: Option<(usize, &str)> = None;
    for cue in ["omits ", "omitted ", "missing from"] {
        if let Some(at) = lower.find(cue)
            && best.is_none_or(|(previous, _)| at < previous)
        {
            best = Some((at, cue));
        }
    }
    let (at, cue) = best?;
    let subject = if cue == "missing from" {
        subject_before(sentence, at)
    } else {
        subject_after(sentence, at + cue.len())
    };
    Some(normalize_subject(&subject))
}

fn subject_after(sentence: &str, start: usize) -> String {
    let rest = &sentence[start..];
    let end = rest.find('|').unwrap_or(rest.len());
    truncate_chars(rest[..end].trim(), 120)
}

fn subject_before(sentence: &str, cue_at: usize) -> String {
    let before = &sentence[..cue_at];
    let start = before.rfind('|').map_or(0, |index| index + 1);
    let mut subject = before[start..].trim().to_string();
    let lower = subject.to_ascii_lowercase();
    for suffix in [" is", " are", " was", " were"] {
        if lower.ends_with(suffix) {
            subject.truncate(subject.len() - suffix.len());
            subject = subject.trim().to_string();
            break;
        }
    }
    if subject.chars().count() > 120 {
        let skip = subject.chars().count() - 120;
        subject = subject.chars().skip(skip).collect();
        if let Some(space) = subject.find(' ') {
            subject = subject[space + 1..].to_string();
        }
    }
    subject
}

fn normalize_subject(subject: &str) -> String {
    subject.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn usable_subject(subject: &str) -> bool {
    let lower = subject.to_ascii_lowercase();
    lower.chars().count() >= 3
        && lower != "nothing"
        && lower != "none"
        && !lower.starts_with("nothing ")
}

/// Sentences of a description, each with its whitespace collapsed. A soft line
/// break inside a paragraph, list item or blockquote is a space, so a
/// convention wrapped over `>` continuation lines stays one sentence. Sentence
/// punctuation, blank lines, headings, list items, table rows and a blockquote
/// that opens after plain text still end a sentence [ORB-25039].
fn sentences(description: &str) -> Vec<String> {
    let lines: Vec<Line<'_>> = description.lines().map(Line::from_text).collect();
    let table_rows = markdown_table_rows(&lines);
    let mut sentences = Vec::new();
    let mut current = String::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            if block_boundary(
                &lines[index - 1],
                line,
                table_rows[index - 1],
                table_rows[index],
            ) {
                push_sentence(&mut current, &mut sentences);
            } else {
                current.push(' ');
            }
        }
        for character in line.content.chars() {
            if matches!(character, '.' | '!' | '?') {
                push_sentence(&mut current, &mut sentences);
            } else {
                current.push(character);
            }
        }
    }
    push_sentence(&mut current, &mut sentences);
    sentences
}

/// One source line with its indentation and blockquote markers removed.
struct Line<'a> {
    /// The line is inside a blockquote (`>`).
    quoted: bool,
    content: &'a str,
}

impl<'a> Line<'a> {
    fn from_text(line: &'a str) -> Self {
        let mut rest = line.trim_start();
        let quoted = rest.starts_with('>');
        while let Some(after) = rest.strip_prefix('>') {
            rest = after.trim_start();
        }
        Self {
            quoted,
            content: rest.trim_end(),
        }
    }
}

/// Whether a line break between two lines ends the sentence. A soft break
/// inside a paragraph, list item or blockquote does not.
fn block_boundary(
    previous: &Line<'_>,
    next: &Line<'_>,
    previous_is_table_row: bool,
    next_is_table_row: bool,
) -> bool {
    previous.content.is_empty()
        || next.content.is_empty()
        || previous_is_table_row
        || next_is_table_row
        || previous.content.starts_with('#')
        || opens_block(next.content)
        || (next.quoted && !previous.quoted)
}

/// Mark rows in a Markdown table whose delimiter row establishes the table.
/// This handles both table styles with and without leading or trailing pipes.
fn markdown_table_rows(lines: &[Line<'_>]) -> Vec<bool> {
    let mut rows = vec![false; lines.len()];
    for (index, line) in lines.iter().enumerate() {
        if !is_table_delimiter_row(line.content) || index == 0 {
            continue;
        }
        let header = &lines[index - 1];
        if header.content.is_empty() || !header.content.contains('|') {
            continue;
        }
        rows[index - 1] = true;
        rows[index] = true;
        for (row, line) in lines.iter().enumerate().skip(index + 1) {
            if line.content.is_empty() || !line.content.contains('|') {
                break;
            }
            rows[row] = true;
        }
    }
    rows
}

fn is_table_delimiter_row(content: &str) -> bool {
    let cells: Vec<_> = content
        .split('|')
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .collect();
    cells.len() >= 2
        && cells.iter().all(|cell| {
            let alignment = cell.trim_matches(':');
            !alignment.is_empty() && alignment.chars().all(|character| character == '-')
        })
}

/// A line that begins its own block: a table row, a heading or a list item.
fn opens_block(content: &str) -> bool {
    if content.starts_with(['|', '#']) {
        return true;
    }
    let digits = content.bytes().take_while(u8::is_ascii_digit).count();
    let after_marker = if digits > 0 {
        content[digits..].strip_prefix(['.', ')'])
    } else {
        content.strip_prefix(['-', '*', '+'])
    };
    after_marker.is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

fn push_sentence(current: &mut String, sentences: &mut Vec<String>) {
    let sentence = current.split_whitespace().collect::<Vec<_>>().join(" ");
    if !sentence.is_empty() {
        sentences.push(sentence);
    }
    current.clear();
}

fn contains_term(haystack: &str, term: &str) -> bool {
    haystack.match_indices(term).any(|(at, matched)| {
        let before = at == 0 || !haystack.as_bytes()[at - 1].is_ascii_alphanumeric();
        let end = at + matched.len();
        let after = end >= haystack.len() || !haystack.as_bytes()[end].is_ascii_alphanumeric();
        before && after
    })
}

fn floor_char(text: &str, mut index: usize) -> usize {
    if index > text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char(text: &str, mut index: usize) -> usize {
    if index > text.len() {
        return text.len();
    }
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}
