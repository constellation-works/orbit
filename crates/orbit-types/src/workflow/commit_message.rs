//! The one commit-message grammar Orbit writes, for every commit it makes and
//! every squash message it hands a forge.
//!
//! ```text
//! <type>: <summary> [ID]… (#PR)
//!
//! <full title, only when the subject cut it>
//!
//! <execution summary; for bundles one `- ID: summary` line per task>
//!
//! Orbit-Task: ID
//! <registered trailers, in registry order>
//! Co-authored-by: orbit[<model>] <<role>@orbit.invalid>
//! ```
//!
//! The subject is at most [`COMMIT_SUBJECT_BUDGET`] characters, PR number
//! included, cut at a word boundary and never with an ellipsis. `type` comes
//! from the task type ([`CommitType::for_task_type`]). Trailers form exactly
//! one final paragraph, keyed from [`TrailerKey`] in its declaration order.
//! [`validate`] checks a rendered message against all of that.

use std::fmt;

use super::COMMIT_SUBJECT_BUDGET;
use crate::task::TaskType;

/// The fewest summary characters a subject keeps before ids leave it: a
/// bundle with more ids than fit still names its work, and every id stays
/// in an `Orbit-Task` trailer.
const MIN_SUMMARY_CHARS: usize = 16;

/// The conventional-commit type of an Orbit commit, taken from the task type
/// alone, never from the kind of commit (review repair, recovery, finalize).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommitType {
    /// A feature task.
    Feat,
    /// A bug task.
    Fix,
    /// A refactor task.
    Refactor,
    /// A chore task.
    Chore,
    /// Documentation work.
    Docs,
}

impl CommitType {
    /// Every type, in bundle precedence order.
    pub const ALL: [Self; 5] = [
        Self::Feat,
        Self::Fix,
        Self::Refactor,
        Self::Chore,
        Self::Docs,
    ];

    /// The type a task of `task_type` commits under.
    pub fn for_task_type(task_type: TaskType) -> Self {
        match task_type {
            TaskType::Feature => Self::Feat,
            TaskType::Bug => Self::Fix,
            TaskType::Refactor => Self::Refactor,
            TaskType::Chore => Self::Chore,
        }
    }

    /// The type of a commit that delivers several tasks: the first of
    /// [`Self::ALL`] any of them has, so a bundle carrying a feature reads as
    /// one. `chore` for an empty bundle.
    pub fn for_bundle(task_types: impl IntoIterator<Item = TaskType>) -> Self {
        task_types
            .into_iter()
            .map(Self::for_task_type)
            .min()
            .unwrap_or(Self::Chore)
    }

    /// The subject spelling (`feat`, `fix`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Feat => "feat",
            Self::Fix => "fix",
            Self::Refactor => "refactor",
            Self::Chore => "chore",
            Self::Docs => "docs",
        }
    }

    /// Parse a subject spelling.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }
}

impl fmt::Display for CommitType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The trailer keys Orbit writes. Declaration order is the order they appear
/// in the one trailer block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrailerKey {
    /// `Orbit-Task`: a task the commit delivers, once per task.
    OrbitTask,
    /// `Fixes`: the landed commit a regression fix repairs.
    Fixes,
    /// `Regression-From`: the task that introduced the defect.
    RegressionFrom,
    /// `Regression`: the regression's nature, escape and severity.
    Regression,
    /// `Orbit-Review-Attempt`: the review attempt a repair belongs to.
    OrbitReviewAttempt,
    /// `Orbit-Review-Crew`: the crew that reviewed and repaired.
    OrbitReviewCrew,
    /// `Orbit-Review-Run`: the run whose stopped review a partial repair keeps.
    OrbitReviewRun,
    /// `Orbit-Review-Step`: the review step a partial repair was cut from.
    OrbitReviewStep,
    /// `Orbit-Recovery-Run`: the run a step recovery repaired.
    OrbitRecoveryRun,
    /// `Orbit-Recovery-Step`: the failed step a recovery repaired.
    OrbitRecoveryStep,
    /// `Orbit-Recovery-Activity`: the recovery activity that made the repair.
    OrbitRecoveryActivity,
    /// `Orbit-Publication`: the task-record publication a snapshot commit
    /// belongs to.
    OrbitPublication,
    /// `Orchestrated-By`: the orchestrator that filed the work.
    OrchestratedBy,
    /// `Co-authored-by`: an agent or person who authored part of the change.
    CoAuthoredBy,
}

impl TrailerKey {
    /// Every key, in registry order.
    pub const ALL: [Self; 14] = [
        Self::OrbitTask,
        Self::Fixes,
        Self::RegressionFrom,
        Self::Regression,
        Self::OrbitReviewAttempt,
        Self::OrbitReviewCrew,
        Self::OrbitReviewRun,
        Self::OrbitReviewStep,
        Self::OrbitRecoveryRun,
        Self::OrbitRecoveryStep,
        Self::OrbitRecoveryActivity,
        Self::OrbitPublication,
        Self::OrchestratedBy,
        Self::CoAuthoredBy,
    ];

    /// The key as Orbit writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OrbitTask => "Orbit-Task",
            Self::Fixes => "Fixes",
            Self::RegressionFrom => "Regression-From",
            Self::Regression => "Regression",
            Self::OrbitReviewAttempt => "Orbit-Review-Attempt",
            Self::OrbitReviewCrew => "Orbit-Review-Crew",
            Self::OrbitReviewRun => "Orbit-Review-Run",
            Self::OrbitReviewStep => "Orbit-Review-Step",
            Self::OrbitRecoveryRun => "Orbit-Recovery-Run",
            Self::OrbitRecoveryStep => "Orbit-Recovery-Step",
            Self::OrbitRecoveryActivity => "Orbit-Recovery-Activity",
            Self::OrbitPublication => "Orbit-Publication",
            Self::OrchestratedBy => "Orchestrated-By",
            Self::CoAuthoredBy => "Co-authored-by",
        }
    }

    /// Parse a key, ignoring ASCII case as Git does (`Co-Authored-By`).
    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(key.trim()))
    }
}

/// One `Key: value` trailer line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trailer {
    /// The registered key.
    pub key: TrailerKey,
    /// The single-line value.
    pub value: String,
}

impl Trailer {
    /// A trailer whose value is folded onto one line.
    pub fn new(key: TrailerKey, value: impl AsRef<str>) -> Self {
        Self {
            key,
            value: single_line(value.as_ref()),
        }
    }

    /// Parse one `Key: value` line with a registered key and a value.
    pub fn parse_line(line: &str) -> Option<Self> {
        let (key, value) = trailer_shape(line)?;
        let key = TrailerKey::parse(key)?;
        Some(Self::new(key, value))
    }

    /// The `Key: value` line.
    pub fn render(&self) -> String {
        format!("{}: {}", self.key.as_str(), self.value)
    }
}

/// Who an Orbit-authored commit or co-author line names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommitRole {
    /// The agent that implemented the task.
    Implementer,
    /// The reviewer that repaired the candidate.
    Reviewer,
    /// A step recovery that repaired the working tree.
    Recovery,
    /// Orbit itself: committer, mixed-author commits and publications.
    Orbit,
}

impl CommitRole {
    /// The address's local part.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implementer => "implementer",
            Self::Reviewer => "reviewer",
            Self::Recovery => "recovery",
            Self::Orbit => "orbit",
        }
    }
}

/// The one Orbit identity scheme: `orbit[<model>] <<role>@orbit.invalid>`,
/// or `orbit <<role>@orbit.invalid>` when no model is known. Returns
/// `(name, email)`.
pub fn orbit_identity(role: CommitRole, model: Option<&str>) -> (String, String) {
    let email = format!("{}@orbit.invalid", role.as_str());
    let model = model
        .map(|model| model.rsplit(" / ").next().unwrap_or(model))
        .map(single_line)
        .filter(|model| !model.is_empty());
    let name = match model {
        Some(model) => format!("orbit[{}]", model.replace(['<', '>'], "")),
        None => "orbit".to_string(),
    };
    (name, email)
}

/// Recognize the four current role addresses at `orbit.invalid` and the
/// historical `orbit.local` scheme: family addresses were implementers,
/// `<family>-reviewer` reviewers, `orbit-recovery` recoveries and `orbit` Orbit.
/// Other addresses, including other `orbit.invalid` local parts, return `None`
/// so their original identity is preserved.
pub fn orbit_role_for_email(email: &str) -> Option<CommitRole> {
    let email = email.trim().to_ascii_lowercase();
    let (local, domain) = email.rsplit_once('@')?;
    match domain {
        "orbit.invalid" => match local {
            "implementer" => Some(CommitRole::Implementer),
            "reviewer" => Some(CommitRole::Reviewer),
            "recovery" => Some(CommitRole::Recovery),
            "orbit" => Some(CommitRole::Orbit),
            _ => None,
        },
        "orbit.local" => Some(if local == "orbit" {
            CommitRole::Orbit
        } else if local == "orbit-recovery" {
            CommitRole::Recovery
        } else if local.ends_with("-reviewer") {
            CommitRole::Reviewer
        } else {
            CommitRole::Implementer
        }),
        _ => None,
    }
}

/// A commit message in parts. [`Self::render`] orders and deduplicates the
/// trailers, so producers push them in any order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitMessage {
    /// The one-line subject.
    pub subject: String,
    /// Body paragraphs; empty ones are dropped.
    pub body: Vec<String>,
    /// Trailers, rendered as the one final paragraph.
    pub trailers: Vec<Trailer>,
}

impl CommitMessage {
    /// The trailers in registry order, exact duplicates dropped (a
    /// `Co-authored-by` identity repeated in another letter case counts as a
    /// duplicate; the first spelling wins).
    pub fn ordered_trailers(&self) -> Vec<Trailer> {
        let mut ordered: Vec<Trailer> = Vec::new();
        let mut trailers = self
            .trailers
            .iter()
            .filter(|trailer| !trailer.value.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        trailers.sort_by_key(|trailer| trailer.key);
        for trailer in trailers {
            let duplicate = ordered.iter().any(|kept| {
                kept.key == trailer.key
                    && (kept.value == trailer.value
                        || (trailer.key == TrailerKey::CoAuthoredBy
                            && kept.value.eq_ignore_ascii_case(&trailer.value)))
            });
            if !duplicate {
                ordered.push(trailer);
            }
        }
        ordered
    }

    /// Everything after the subject: body paragraphs, then the trailer block.
    pub fn render_body(&self) -> String {
        let mut paragraphs = self
            .body
            .iter()
            .map(|paragraph| paragraph.trim().to_string())
            .filter(|paragraph| !paragraph.is_empty())
            .collect::<Vec<_>>();
        let trailers = self.ordered_trailers();
        if !trailers.is_empty() {
            paragraphs.push(
                trailers
                    .iter()
                    .map(Trailer::render)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        paragraphs.join("\n\n")
    }

    /// The whole message.
    pub fn render(&self) -> String {
        let subject = single_line(&self.subject);
        let body = self.render_body();
        if body.is_empty() {
            subject
        } else {
            format!("{subject}\n\n{body}")
        }
    }
}

/// The title and body Orbit hands a forge for a squash or merge commit, so
/// no merge leaves its message to the forge's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeMessage {
    /// The commit title (subject).
    pub title: String,
    /// The commit body: prose, then the trailer block.
    pub body: String,
}

impl From<&CommitMessage> for MergeMessage {
    fn from(message: &CommitMessage) -> Self {
        Self {
            title: single_line(&message.subject),
            body: message.render_body(),
        }
    }
}

/// A composed subject and whether the summary was cut to fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedSubject {
    /// `<type>: <summary> [ID]… (#PR)`.
    pub subject: String,
    /// The summary lost words, so the body should carry it whole.
    pub summary_cut: bool,
}

/// Compose `<type>: <summary> [ID]… (#PR)` within [`COMMIT_SUBJECT_BUDGET`]
/// characters. The summary is cut at a word boundary (trailing punctuation
/// dropped, no ellipsis); a single word longer than the room is cut at a
/// character. When ids alone leave the summary less than a minimum, trailing
/// ids leave the subject: callers carry every id in `Orbit-Task` trailers.
pub fn compose_subject(
    commit_type: CommitType,
    summary: &str,
    task_ids: &[&str],
    pr_number: Option<u64>,
) -> ComposedSubject {
    let summary = single_line(summary);
    let prefix = format!("{}: ", commit_type.as_str());
    let pr = pr_number
        .map(|number| format!(" (#{number})"))
        .unwrap_or_default();
    let mut ids = task_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    let suffix_of = |ids: &[&str]| -> String {
        let mut suffix = ids.iter().map(|id| format!(" [{id}]")).collect::<String>();
        suffix.push_str(&pr);
        suffix
    };
    let mut suffix = suffix_of(&ids);
    let fixed = |suffix: &str| prefix.chars().count() + suffix.chars().count();
    let wanted = summary.chars().count().min(MIN_SUMMARY_CHARS);
    while ids.len() > 1 && fixed(&suffix) + wanted > COMMIT_SUBJECT_BUDGET {
        ids.pop();
        suffix = suffix_of(&ids);
    }
    let room = COMMIT_SUBJECT_BUDGET.saturating_sub(fixed(&suffix));
    let (kept, summary_cut) = shorten_at_word(&summary, room);
    ComposedSubject {
        subject: format!("{prefix}{kept}{suffix}"),
        summary_cut,
    }
}

/// `text` within `room` characters, cut at the last word boundary that fits
/// with trailing punctuation dropped; a first word longer than `room` is cut
/// at a character. Never adds an ellipsis. The flag says whether it cut.
pub fn shorten_at_word(text: &str, room: usize) -> (String, bool) {
    if text.chars().count() <= room {
        return (text.to_string(), false);
    }
    let mut kept = String::new();
    for word in text.split(' ') {
        let candidate_len = if kept.is_empty() {
            word.chars().count()
        } else {
            kept.chars().count() + 1 + word.chars().count()
        };
        if candidate_len > room {
            break;
        }
        if !kept.is_empty() {
            kept.push(' ');
        }
        kept.push_str(word);
    }
    if kept.is_empty() {
        kept = text.chars().take(room).collect();
    }
    let trimmed = kept
        .trim_end_matches(|character: char| {
            character.is_whitespace() || matches!(character, ':' | ';' | ',' | '-' | '(' | '[')
        })
        .to_string();
    (if trimmed.is_empty() { kept } else { trimmed }, true)
}

/// One way a message breaks the standard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitMessageViolation {
    /// The subject is empty.
    EmptySubject,
    /// The subject is longer than [`COMMIT_SUBJECT_BUDGET`] characters.
    SubjectTooLong(usize),
    /// The subject was shortened with an ellipsis.
    SubjectEllipsis,
    /// The subject does not start with `<type>: ` of a known type.
    SubjectType,
    /// A subject bracket holds several ids (`[A, B]`).
    SharedMarker(String),
    /// The subject is not followed by a blank line.
    NoBlankAfterSubject,
    /// The message has no final trailer paragraph.
    MissingTrailerBlock,
    /// A final-paragraph line is not a registered `Key: value` trailer.
    UnregisteredTrailer(String),
    /// Trailers are out of registry order.
    TrailerOrder(String),
    /// Registered trailers appear in a paragraph before the final one.
    TrailersMidBody(String),
}

impl fmt::Display for CommitMessageViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySubject => formatter.write_str("the subject is empty"),
            Self::SubjectTooLong(chars) => write!(
                formatter,
                "the subject is {chars} characters, over {COMMIT_SUBJECT_BUDGET}"
            ),
            Self::SubjectEllipsis => formatter.write_str("the subject is cut with an ellipsis"),
            Self::SubjectType => formatter.write_str(
                "the subject does not start with `feat:`, `fix:`, `refactor:`, `chore:` or `docs:`",
            ),
            Self::SharedMarker(marker) => write!(
                formatter,
                "subject marker {marker} holds several ids; give each its own brackets"
            ),
            Self::NoBlankAfterSubject => {
                formatter.write_str("the subject is not followed by a blank line")
            }
            Self::MissingTrailerBlock => {
                formatter.write_str("the message has no final trailer paragraph")
            }
            Self::UnregisteredTrailer(line) => {
                write!(formatter, "trailer line '{line}' has no registered key")
            }
            Self::TrailerOrder(line) => {
                write!(formatter, "trailer '{line}' is out of registry order")
            }
            Self::TrailersMidBody(line) => {
                write!(
                    formatter,
                    "trailer '{line}' sits outside the final paragraph"
                )
            }
        }
    }
}

impl std::error::Error for CommitMessageViolation {}

/// Check `subject` alone: budget, no ellipsis, a known type and one id per
/// bracket.
pub fn validate_subject(subject: &str) -> Result<(), CommitMessageViolation> {
    let subject = subject.trim_end();
    if subject.trim().is_empty() {
        return Err(CommitMessageViolation::EmptySubject);
    }
    let chars = subject.chars().count();
    if chars > COMMIT_SUBJECT_BUDGET {
        return Err(CommitMessageViolation::SubjectTooLong(chars));
    }
    if subject.contains('…') {
        return Err(CommitMessageViolation::SubjectEllipsis);
    }
    let typed = subject
        .split_once(": ")
        .and_then(|(kind, rest)| CommitType::parse(kind).filter(|_| !rest.trim().is_empty()));
    if typed.is_none() {
        return Err(CommitMessageViolation::SubjectType);
    }
    for candidate in subject.split('[').skip(1) {
        if let Some((token, _)) = candidate.split_once(']')
            && token.contains(',')
        {
            return Err(CommitMessageViolation::SharedMarker(format!("[{token}]")));
        }
    }
    Ok(())
}

/// Check a whole message against the standard: [`validate_subject`], a blank
/// line after it, and exactly one final trailer paragraph whose every line
/// is a registered trailer in registry order, with no registered trailer
/// paragraph earlier in the body.
pub fn validate(message: &str) -> Result<(), CommitMessageViolation> {
    let message = message.trim_end_matches('\n');
    let mut lines = message.lines();
    let subject = lines.next().unwrap_or_default();
    validate_subject(subject)?;
    if lines.next().is_some_and(|line| !line.trim().is_empty()) {
        return Err(CommitMessageViolation::NoBlankAfterSubject);
    }
    // `paragraphs` starts with the subject; a lone subject has no block.
    let paragraphs = paragraphs(message);
    let Some((last, body)) = paragraphs.split_last().filter(|(_, body)| !body.is_empty()) else {
        return Err(CommitMessageViolation::MissingTrailerBlock);
    };
    let mut previous: Option<TrailerKey> = None;
    for line in last.lines() {
        let Some(trailer) = Trailer::parse_line(line) else {
            return Err(if trailer_shape(line).is_some() {
                CommitMessageViolation::UnregisteredTrailer(line.to_string())
            } else {
                CommitMessageViolation::MissingTrailerBlock
            });
        };
        if previous.is_some_and(|previous| trailer.key < previous) {
            return Err(CommitMessageViolation::TrailerOrder(line.to_string()));
        }
        previous = Some(trailer.key);
    }
    for paragraph in body.iter().skip(1) {
        if let Some(line) = paragraph
            .lines()
            .find(|line| Trailer::parse_line(line).is_some())
            && paragraph.lines().all(|line| trailer_shape(line).is_some())
        {
            return Err(CommitMessageViolation::TrailersMidBody(line.to_string()));
        }
    }
    Ok(())
}

/// The registered trailers of `message`'s final paragraph, in order. Empty
/// when that paragraph is not wholly `Key: value` lines.
pub fn parse_trailers(message: &str) -> Vec<Trailer> {
    let paragraphs = paragraphs(message.trim_end());
    match paragraphs.split_first() {
        Some((_, rest)) if !rest.is_empty() => rest
            .last()
            .filter(|last| last.lines().all(|line| trailer_shape(line).is_some()))
            .map(|last| last.lines().filter_map(Trailer::parse_line).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Every registered trailer in any trailer-shaped paragraph after the
/// subject: what a squash collects from inner commits, including those
/// written before the standard, whose trailers sat mid-body or shared a
/// paragraph with unregistered `Findings:` / `Paths:` lines.
pub fn collect_trailers(message: &str) -> Vec<Trailer> {
    paragraphs(message.trim_end())
        .iter()
        .skip(1)
        .filter(|paragraph| paragraph.lines().all(|line| trailer_shape(line).is_some()))
        .flat_map(|paragraph| paragraph.lines().filter_map(Trailer::parse_line))
        .collect()
}

/// The byte offset where `text`'s final paragraph starts when that paragraph
/// is a trailer block of registered keys only; `None` otherwise.
pub fn trailer_block_start(text: &str) -> Option<usize> {
    let trimmed = text.trim_end();
    // A text that is solely a trailer block has no preceding blank line;
    // its block starts at offset 0.
    let start = trimmed.rfind("\n\n").map(|index| index + 2).unwrap_or(0);
    let block = &trimmed[start..];
    let start = start + (block.len() - block.trim_start_matches('\n').len());
    let block = block.trim_start_matches('\n');
    (!block.is_empty()
        && block
            .lines()
            .all(|line| Trailer::parse_line(line).is_some()))
    .then_some(start)
}

/// `Token: value` with a single-word ASCII key, as Git reads a trailer.
fn trailer_shape(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    let value = value.strip_prefix(' ')?;
    (!key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        && !value.trim().is_empty())
    .then_some((key, value))
}

fn paragraphs(message: &str) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in message.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join("\n"));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join("\n"));
    }
    paragraphs
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
