//! Shared parsing of positive validation-tool requirements. Prose is evidence,
//! never tool authority; quoted examples and expected refusals are excluded.

use orbit_types::task::Task;

/// Lowercase wording that marks a sentence as expecting a refusal rather than
/// requiring a call. A negative test names the tool it expects to be denied,
/// which is a correct criterion, not a contradiction.
const DENIAL_MARKERS: &[&str] = &[
    "deny",
    "denie",
    "denial",
    "reject",
    "refus",
    "unavailable",
    "not granted",
    "not in the allowlist",
    "must not",
    "cannot",
];

/// The one-based criterion and exact registered tool positively required by it.
pub(crate) fn positive_validation_tools<'a>(
    task: &Task,
    registered: &'a [String],
) -> Vec<(usize, &'a str)> {
    let mut requirements = Vec::new();
    for (criterion, sentence) in positive_validation_sentences(task) {
        for tool in registered {
            if names_tool(&sentence, tool) && !requirements.contains(&(criterion, tool.as_str())) {
                requirements.push((criterion, tool.as_str()));
            }
        }
    }
    requirements
}

/// Preserve fenced quoting across criteria split into lines by task input
/// normalization. Otherwise a copied tool name between two fence-only
/// criteria would look like a positive requirement at admission.
pub(crate) fn positive_validation_sentences(task: &Task) -> Vec<(usize, String)> {
    let combined = task.acceptance_criteria.join("\n");
    let blanked = blank_quoted_regions(&combined);
    let mut characters = blanked.chars();
    let mut positive = Vec::new();
    for (index, criterion) in task.acceptance_criteria.iter().enumerate() {
        let criterion: String = characters
            .by_ref()
            .take(criterion.chars().count())
            .collect();
        positive.extend(
            sentences(&criterion)
                .into_iter()
                .filter(|sentence| !expects_denial(sentence))
                .map(|sentence| (index + 1, sentence.to_string())),
        );
        characters.next();
    }
    positive
}

/// Blank the regions a criterion uses to quote something rather than to
/// require it: fenced code blocks and double-quoted strings.
///
/// A tool name inside a copied transcript or an expected error message is an
/// example of what the work will observe, not a call the lane has to support.
/// Inline single backticks are deliberately left alone: canonical tool names
/// are conventionally written that way, so masking them would blind the check
/// to nearly every real criterion.
///
/// Blanking preserves length and line structure so the sentence split below
/// still sees the surrounding prose as the author wrote it.
fn blank_quoted_regions(text: &str) -> String {
    const FENCE: &str = "```";

    let mut blanked = String::with_capacity(text.len());
    let mut quoting = Quoting::None;
    let mut chars = text.char_indices();
    while let Some((index, character)) = chars.next() {
        if matches!(quoting, Quoting::None | Quoting::Fence) && text[index..].starts_with(FENCE) {
            blanked.push_str("   ");
            // The scanner already consumed the first backtick.
            chars.next();
            chars.next();
            quoting = match quoting {
                Quoting::Fence => Quoting::None,
                _ => Quoting::Fence,
            };
            continue;
        }
        match (quoting, character) {
            (Quoting::None, '"') => {
                quoting = Quoting::Double;
                blanked.push(' ');
            }
            // An unterminated quote ends at the line break, so a stray `"`
            // cannot blank the rest of the criterion.
            (Quoting::Double, '"' | '\n') => {
                quoting = Quoting::None;
                blanked.push(blank(character));
            }
            (Quoting::None, _) => blanked.push(character),
            _ => blanked.push(blank(character)),
        }
    }
    blanked
}

#[derive(Clone, Copy)]
enum Quoting {
    None,
    Fence,
    Double,
}

fn blank(character: char) -> char {
    if character == '\n' { '\n' } else { ' ' }
}

/// Split into sentences at a terminator followed by whitespace, or at a line
/// break. Requiring the trailing whitespace keeps a canonical tool name's own
/// dots inside one sentence.
fn sentences(text: &str) -> Vec<&str> {
    let mut split = Vec::new();
    let mut start = 0;
    for (index, character) in text.char_indices() {
        let terminates = matches!(character, '.' | ';' | '!' | '?')
            && text[index + character.len_utf8()..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace);
        if character == '\n' || terminates {
            split.push(&text[start..index]);
            start = index + character.len_utf8();
        }
    }
    split.push(&text[start..]);
    split.retain(|sentence| !sentence.trim().is_empty());
    split
}

fn expects_denial(sentence: &str) -> bool {
    let lowered = sentence.to_ascii_lowercase();
    DENIAL_MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// Whether `sentence` names exactly `tool`, rather than containing it inside a
/// longer dotted name. Dots count as name characters so `orbit.task.artifact`
/// does not match inside `orbit.task.artifact.get`.
pub(crate) fn names_tool(sentence: &str, tool: &str) -> bool {
    sentence.match_indices(tool).any(|(index, _)| {
        let before = sentence[..index].chars().next_back();
        let after = sentence[index + tool.len()..].chars().next();
        !before.is_some_and(is_tool_name_char) && !after.is_some_and(is_tool_name_char)
    })
}

fn is_tool_name_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
}

/// Words that, immediately before a tool mention, ask the implementer to
/// call that tool to produce evidence: an invocation verb (`run`, `call`,
/// `invoke`, `execute`, `use`) or a route/evidence preposition (`via`,
/// `through`, `with`, `from`, `by`). A validation verb alone (`validate
/// `tool``) does not count: it may only describe checking the tool's own
/// behaviour, and silence is preferred over inventing a requirement.
const INVOCATION_CUES: &[&str] = &[
    "run",
    "runs",
    "running",
    "call",
    "calls",
    "calling",
    "called",
    "invoke",
    "invokes",
    "invoking",
    "invoked",
    "execute",
    "executes",
    "executing",
    "executed",
    "use",
    "uses",
    "using",
    "used",
    "via",
    "through",
    "with",
    "from",
    "by",
];

/// Passive verbs that, after a tool mention, still ask for a call:
/// `` `tool` is invoked to … ``.
const INVOKED_VERBS: &[&str] = &[
    "run", "running", "called", "calling", "invoked", "invoking", "executed", "used",
];

/// Whether `sentence` asks the implementer to call `tool`, rather than
/// naming it as the subject whose behaviour, schema or documentation the
/// task changes.
///
/// Only a cue in the few words around the mention counts — before it an
/// invocation verb or route preposition, after it a passive invocation —
/// so a subject mention such as "An `orbit.workflow` action auto call …"
/// or "… name mode resize on `orbit.workflow` …" stays silent. When no
/// cue is present the distinction cannot be drawn, and silence is
/// preferred over inventing a requirement (see `tools_named_in`).
pub(crate) fn sentence_requires_tool_invocation(sentence: &str, tool: &str) -> bool {
    let lowered = sentence.to_ascii_lowercase();
    lowered.match_indices(tool).any(|(index, _)| {
        let before = sentence[..index].chars().next_back();
        let after = sentence[index + tool.len()..].chars().next();
        if before.is_some_and(is_tool_name_char) || after.is_some_and(is_tool_name_char) {
            return false;
        }
        // Only the clause holding the mention counts, so a cue in an
        // earlier clause of the same sentence cannot invent a call.
        let clause = lowered[..index]
            .rsplit([';', ','])
            .next()
            .unwrap_or_default();
        let before_words: Vec<&str> = clause.split_whitespace().map(normalize_cue_word).collect();
        if before_words
            .iter()
            .rev()
            .take(4)
            .any(|word| INVOCATION_CUES.contains(word))
        {
            return true;
        }
        let after_words: Vec<&str> = lowered[index + tool.len()..]
            .split_whitespace()
            .map(normalize_cue_word)
            .collect();
        // `` `tool` is invoked to validate … ``, but not `` `tool` is
        // called with … ``: the latter describes a call a user makes,
        // whose behaviour the task may be fixing.
        matches!(
            after_words.first().copied(),
            Some("is" | "are" | "was" | "were" | "must" | "shall" | "should" | "be" | "been")
        ) && after_words
            .get(1)
            .is_some_and(|word| INVOKED_VERBS.contains(word))
            && after_words.get(2).copied() == Some("to")
    })
}

/// Trim quoting and punctuation around a cue word without splitting it, so
/// `tools/call` never reads as the invocation verb `call`.
fn normalize_cue_word(word: &str) -> &str {
    word.trim_matches(|character: char| {
        !character.is_ascii_alphanumeric() && !matches!(character, '/' | '_' | '-')
    })
}
