//! A workspace's delivery conventions: the templates Orbit renders pull
//! request titles and bodies, commit messages and branch names from, and the
//! switches for Orbit's own sections and trailers.
//!
//! Every template uses `{{ name }}` placeholders from one fixed vocabulary
//! ([`Placeholder`]). A template is parsed once, when the configuration loads
//! (or, for a body template read from the repository, when it is read), and
//! an unknown or unterminated placeholder is refused then, naming the setting
//! key. Rendering a parsed template cannot leave a placeholder behind.
//!
//! [`DeliveryConventions::default`] is Orbit's built-in behaviour: no
//! templates, every section and trailer on.

use std::collections::BTreeMap;
use std::fmt;

use super::error::ConventionTemplateError;

/// Task artifact the implement step writes the agent-filled pull request
/// sections to: `{"sections": {"<heading>": "<markdown>"}}`.
pub const PR_SECTIONS_ARTIFACT: &str = "pr-sections.json";

/// Task history event naming the required pull request sections a delivery
/// published without filled content.
pub const PR_SECTION_GAP_EVENT: &str = "pr_section_gap";

/// The decision recorded on [`PR_SECTION_GAP_EVENT`]: which required
/// sections a delivery had no content for, what the configured policy did,
/// and, when this delivery created the pull request with that body, the
/// publication it binds to. The rendered history note and the body's
/// missing-section note are presentation; this record is the evidence draft
/// recovery and handed-off landing read.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PrSectionGap {
    /// Configured names of the required sections published without content.
    pub missing_sections: Vec<String>,
    /// `pr.missing_required_section` when the gap was found.
    pub action: MissingSectionPolicy,
    /// Job run (execution) that rendered the body.
    pub run_id: String,
    /// Head branch the delivery publishes from.
    pub head_branch: String,
    /// The pull request this delivery created with the gap body. Absent when
    /// it refused, or reused a pull request without publishing this body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<PrSectionGapPublication>,
}

/// The exact body a delivery created its pull request with. Only a live body
/// with this digest on a pull request from this head branch is that
/// publication; a human edit changes the digest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PrSectionGapPublication {
    /// Head branch of the created pull request.
    pub head_branch: String,
    /// SHA-256 of the body sent to the forge's create operation.
    pub body_sha256: String,
}

impl PrSectionGap {
    /// Whether this decision published the live body of a pull request from
    /// `head_branch`.
    pub fn published_body(&self, head_branch: &str, body_sha256: &str) -> bool {
        self.publication.as_ref().is_some_and(|publication| {
            publication.head_branch == head_branch && publication.body_sha256 == body_sha256
        })
    }

    /// Whether this decision created a pull request from `head_branch`.
    pub fn published_from(&self, head_branch: &str) -> bool {
        self.publication
            .as_ref()
            .is_some_and(|publication| publication.head_branch == head_branch)
    }
}

/// History event explaining a reused pull request's draft recovery decision.
pub const PR_DRAFT_RECOVERY_EVENT: &str = "pr_draft_recovery";

/// Why a reused draft was kept, or which recovery mutation was reached.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrDraftRecoveryOutcome {
    /// The workspace deliberately creates drafts.
    ConfiguredDraft,
    /// No section-gap provenance authorizes changing this draft.
    UnrelatedDraft,
    /// Required content is still unavailable.
    MissingSections,
    /// The body mutation is reserved durably before updating the forge.
    RefreshingBody,
    /// The forge accepted the ready-for-review mutation.
    Ready,
}

/// Durable evidence for a draft recovery, bound to the PR and rendered body.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PrDraftRecovery {
    /// Exact forge URL, preventing PR-number collisions across repositories.
    pub pr_url: String,
    /// Recovery decision or mutation phase.
    pub outcome: PrDraftRecoveryOutcome,
    /// Digest of the body authorized by the recovery reservation.
    pub body_sha256: Option<String>,
}

/// `pr.body_template` value that selects the repository's own GitHub pull
/// request template.
pub const REPO_PR_BODY_TEMPLATE: &str = "repo";

/// Where `pr.body_template = "repo"` looks for the repository's pull request
/// template, in order: GitHub's `.github/`, root and `docs/` locations. Names
/// match case-insensitively.
pub const REPO_PR_TEMPLATE_LOCATIONS: &[(&str, &str)] = &[
    (".github", "pull_request_template.md"),
    ("", "pull_request_template.md"),
    ("docs", "pull_request_template.md"),
];

/// Task artifact prefix a delivery publishes as the pull request's
/// screenshots: `screenshots/<view>-<theme>-<width>[-before|-after].<ext>`.
pub const SCREENSHOT_ARTIFACT_PREFIX: &str = "screenshots/";

/// The pull request body heading Orbit renders the screenshots under.
pub const SCREENSHOTS_SECTION: &str = "Screenshots";

/// Task tag that turns the screenshot section on under
/// [`ScreenshotSection::Auto`].
pub const UI_TASK_TAG: &str = "ui";

/// Namespace of the permanent per-task refs holding published screenshots on
/// `origin`. Outside `refs/heads`, so they are never listed or merged as a
/// branch, and nothing deletes them, so a merged pull request keeps its images.
pub const EVIDENCE_REF_PREFIX: &str = "refs/orbit/evidence/";

/// The branch prefix a delivery uses when its workflow supplies none.
pub const DEFAULT_BRANCH_PREFIX: &str = "orbit";

/// Built-in single-task branch shape: `<prefix>/<task>-<hash8>`.
pub const DEFAULT_BRANCH_TEMPLATE: &str = "{{prefix}}/{{task_id}}-{{hash}}";

/// Characters a templated commit subject may use; longer subjects are
/// shortened with [`SUBJECT_ELLIPSIS`].
pub const COMMIT_SUBJECT_BUDGET: usize = 72;

/// Marks a shortened commit subject.
pub const SUBJECT_ELLIPSIS: char = '…';

/// Hex digits of the run hash in a branch name.
pub const BRANCH_HASH_LEN: usize = 8;

/// One `{{ name }}` a template may reference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Placeholder {
    /// `task_id`: the Orbit task id.
    TaskId,
    /// `title`: the task title.
    Title,
    /// `type`: the conventional-commit type of the task (`feat`, `fix`,
    /// `refactor` or `chore`).
    Type,
    /// `description`: the task description.
    Description,
    /// `acceptance_criteria`: the criteria as a Markdown list.
    AcceptanceCriteria,
    /// `execution_summary`: the implementer's execution summary, empty when
    /// it is a placeholder.
    ExecutionSummary,
    /// `validation`: the validation report.
    Validation,
    /// `freshness`: the branch freshness list.
    Freshness,
    /// `task_url`: the task's link, empty when there is none.
    TaskUrl,
    /// `ext.<system>`: the id of the task's external ref in `<system>`
    /// (`ext.jira`), empty when the task has none.
    ExternalRef(String),
    /// `prefix`: the workflow's branch prefix (branch templates only).
    Prefix,
    /// `hash`: the run hash that keeps a branch name unique per run and
    /// stable across its retries (branch templates only).
    Hash,
}

impl Placeholder {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "task_id" => Self::TaskId,
            "title" => Self::Title,
            "type" => Self::Type,
            "description" => Self::Description,
            "acceptance_criteria" => Self::AcceptanceCriteria,
            "execution_summary" => Self::ExecutionSummary,
            "validation" => Self::Validation,
            "freshness" => Self::Freshness,
            "task_url" => Self::TaskUrl,
            "prefix" => Self::Prefix,
            "hash" => Self::Hash,
            other => {
                let system = other.strip_prefix("ext.")?;
                let valid = !system.is_empty()
                    && system
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                return valid.then(|| Self::ExternalRef(system.to_ascii_lowercase()));
            }
        })
    }

    fn admitted_on(&self, surface: TemplateSurface) -> bool {
        match surface {
            TemplateSurface::Text => !matches!(self, Self::Prefix | Self::Hash),
            TemplateSurface::Branch => matches!(
                self,
                Self::Prefix | Self::TaskId | Self::Hash | Self::ExternalRef(_)
            ),
        }
    }
}

impl fmt::Display for Placeholder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TaskId => f.write_str("task_id"),
            Self::Title => f.write_str("title"),
            Self::Type => f.write_str("type"),
            Self::Description => f.write_str("description"),
            Self::AcceptanceCriteria => f.write_str("acceptance_criteria"),
            Self::ExecutionSummary => f.write_str("execution_summary"),
            Self::Validation => f.write_str("validation"),
            Self::Freshness => f.write_str("freshness"),
            Self::TaskUrl => f.write_str("task_url"),
            Self::ExternalRef(system) => write!(f, "ext.{system}"),
            Self::Prefix => f.write_str("prefix"),
            Self::Hash => f.write_str("hash"),
        }
    }
}

/// Which placeholders a template may use: a text template (PR title and
/// body, commit subject and body) every task field except the branch-only
/// `prefix` and `hash`; a branch template only `prefix`, `task_id`, `hash`
/// and `ext.<system>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateSurface {
    Text,
    Branch,
}

const TEXT_PLACEHOLDERS: &str = "task_id, title, type, description, acceptance_criteria, \
                                 execution_summary, validation, freshness, task_url, ext.<system>";
const BRANCH_PLACEHOLDERS: &str = "prefix, task_id, hash, ext.<system>";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Placeholder(Placeholder),
}

/// A parsed template: literal text and placeholders, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConventionTemplate {
    source: String,
    segments: Vec<Segment>,
}

impl ConventionTemplate {
    /// Parse `text` for `surface`, refusing an unknown, misplaced or
    /// unterminated placeholder; `key` names the setting in the error.
    pub fn parse(
        key: &str,
        text: &str,
        surface: TemplateSurface,
    ) -> Result<Self, ConventionTemplateError> {
        let mut segments = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("{{") {
            if start > 0 {
                segments.push(Segment::Literal(rest[..start].to_string()));
            }
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                return Err(ConventionTemplateError::Unterminated {
                    key: key.to_string(),
                });
            };
            let name = after[..end].trim();
            let placeholder = Placeholder::parse(name)
                .filter(|placeholder| placeholder.admitted_on(surface))
                .ok_or_else(|| ConventionTemplateError::UnknownPlaceholder {
                    key: key.to_string(),
                    placeholder: name.to_string(),
                    known: match surface {
                        TemplateSurface::Text => TEXT_PLACEHOLDERS,
                        TemplateSurface::Branch => BRANCH_PLACEHOLDERS,
                    },
                })?;
            segments.push(Segment::Placeholder(placeholder));
            rest = &after[end + 2..];
        }
        if !rest.is_empty() {
            segments.push(Segment::Literal(rest.to_string()));
        }
        Ok(Self {
            source: text.to_string(),
            segments,
        })
    }

    /// Parse a branch-name template. It must name `{{task_id}}`, so readers
    /// can find the owning task, and `{{hash}}`, so each run gets its own
    /// ref and a retry of the run addresses the same one.
    pub fn parse_branch(key: &str, text: &str) -> Result<Self, ConventionTemplateError> {
        let template = Self::parse(key, text, TemplateSurface::Branch)?;
        for required in [Placeholder::TaskId, Placeholder::Hash] {
            if !template.uses(&required) {
                return Err(ConventionTemplateError::Invalid {
                    key: key.to_string(),
                    reason: format!("a branch template must contain `{{{{{required}}}}}`"),
                });
            }
        }
        // Refuse what the template's own text makes unsafe now; what a task's
        // values make unsafe is refused when the branch is named.
        let sample = template.render(|placeholder| match placeholder {
            Placeholder::Prefix => DEFAULT_BRANCH_PREFIX.to_string(),
            Placeholder::Hash => "0".repeat(BRANCH_HASH_LEN),
            _ => "TASK-1".to_string(),
        });
        if let Some(reason) = unsafe_branch_name_reason(&sample) {
            return Err(ConventionTemplateError::Invalid {
                key: key.to_string(),
                reason: format!("renders branch names like '{sample}', and a branch name {reason}"),
            });
        }
        Ok(template)
    }

    /// Parse a commit subject template. It must carry the bracketed task id
    /// `[{{task_id}}]`: Orbit decides whether a task's work reached a branch
    /// from that marker in commit messages, which survives squash and rebase.
    pub fn parse_commit_subject(key: &str, text: &str) -> Result<Self, ConventionTemplateError> {
        let template = Self::parse(key, text, TemplateSurface::Text)?;
        let marked = template.segments.windows(3).any(|window| {
            matches!(
                window,
                [
                    Segment::Literal(before),
                    Segment::Placeholder(Placeholder::TaskId),
                    Segment::Literal(after),
                ] if before.ends_with('[') && after.starts_with(']')
            )
        });
        if !marked {
            return Err(ConventionTemplateError::Invalid {
                key: key.to_string(),
                reason: "a commit subject template must contain the task marker `[{{task_id}}]`"
                    .to_string(),
            });
        }
        Ok(template)
    }

    /// The text the template was parsed from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether the template references `placeholder`.
    pub fn uses(&self, placeholder: &Placeholder) -> bool {
        self.segments
            .iter()
            .any(|segment| matches!(segment, Segment::Placeholder(p) if p == placeholder))
    }

    /// Whether the template references any `{{ext.<system>}}`.
    pub fn uses_external_ref(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| matches!(segment, Segment::Placeholder(Placeholder::ExternalRef(_))))
    }

    /// Render with `value` supplying each placeholder.
    pub fn render(&self, value: impl Fn(&Placeholder) -> String) -> String {
        self.segments
            .iter()
            .map(|segment| match segment {
                Segment::Literal(text) => text.clone(),
                Segment::Placeholder(placeholder) => value(placeholder),
            })
            .collect()
    }

    /// Whether `branch` is a branch this template names for `task_id` under
    /// `prefix`. The complete template must match, including the eight-digit
    /// hash and any suffix, so a task token inside an external ref or a prefix
    /// of the owning task id does not establish ownership. An external ref is
    /// matched verbatim and may contain `/`.
    pub fn branch_names_task(&self, branch: &str, prefix: &str, task_id: &str) -> bool {
        !task_id.is_empty()
            && match_owner(&self.segments, branch, prefix, &TaskMatch::Exact(task_id)).is_some()
    }

    /// The task id `branch` names under this template and `prefix`: the
    /// first candidate `accept` admits, shortest first. A repeated task
    /// placeholder must carry that same id at every occurrence, so a captured
    /// owner always satisfies [`Self::branch_names_task`].
    pub fn branch_task_id(
        &self,
        branch: &str,
        prefix: &str,
        accept: &dyn Fn(&str) -> bool,
    ) -> Option<String> {
        match_owner(&self.segments, branch, prefix, &TaskMatch::Capture(accept))
    }
}

enum TaskMatch<'a> {
    Exact(&'a str),
    Capture(&'a dyn Fn(&str) -> bool),
}

fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Match the complete rendered shape, returning the task id matched. Every
/// occurrence of a repeated task placeholder must carry the same id. Fixed
/// literals and the hash length establish the task boundary even when the
/// template puts digits or the hash immediately after the task id. An external
/// ref may contain `/`; later segments still bound where it ends.
fn match_owner(
    segments: &[Segment],
    text: &str,
    prefix: &str,
    task: &TaskMatch<'_>,
) -> Option<String> {
    fn go(
        segments: &[Segment],
        text: &str,
        prefix: &str,
        task: &TaskMatch<'_>,
        captured: Option<&str>,
    ) -> Option<String> {
        let Some((first, rest)) = segments.split_first() else {
            return captured.filter(|_| text.is_empty()).map(ToOwned::to_owned);
        };
        match first {
            Segment::Literal(literal) => go(
                rest,
                text.strip_prefix(literal.as_str())?,
                prefix,
                task,
                captured,
            ),
            Segment::Placeholder(Placeholder::Prefix) => {
                go(rest, text.strip_prefix(prefix)?, prefix, task, captured)
            }
            Segment::Placeholder(Placeholder::Hash) => {
                let hash = text.get(..BRANCH_HASH_LEN)?;
                hash.chars()
                    .all(|c| c.is_ascii_hexdigit())
                    .then(|| go(rest, &text[BRANCH_HASH_LEN..], prefix, task, captured))?
            }
            // A repeated task placeholder renders the same id every time, so
            // a later occurrence must repeat the id the first one bound.
            // Capture would otherwise keep the last candidate, naming a task
            // the exact matcher refuses.
            Segment::Placeholder(Placeholder::TaskId) => match (task, captured) {
                (_, Some(bound)) => {
                    let tail = text.strip_prefix(bound)?;
                    go(rest, tail, prefix, task, Some(bound))
                }
                (TaskMatch::Exact(id), None) => {
                    let tail = text.strip_prefix(id)?;
                    go(rest, tail, prefix, task, Some(id))
                }
                (TaskMatch::Capture(accept), None) => {
                    let run = text.find(|c: char| !is_id_char(c)).unwrap_or(text.len());
                    (1..=run).find_map(|end| {
                        let (candidate, tail) = text.split_at(end);
                        accept(candidate).then(|| go(rest, tail, prefix, task, Some(candidate)))?
                    })
                }
            },
            // Branch templates reach this arm only for `ext.<system>`. The id
            // is rendered verbatim and may contain `/`. The first completing
            // split wins, and a slash-free remainder is searched exactly as
            // far as `find('/')` used to search.
            Segment::Placeholder(_) => (0..=text.len())
                .filter(|end| text.is_char_boundary(*end))
                .find_map(|end| go(rest, &text[end..], prefix, task, captured)),
        }
    }
    go(segments, text, prefix, task, None)
}

/// What a delivery does when a required pull request section has no filled
/// content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingSectionPolicy {
    /// Open the pull request as a draft, with a note under the heading.
    #[default]
    Draft,
    /// Refuse to open the pull request, naming the section.
    Refuse,
}

impl MissingSectionPolicy {
    /// Accepted spellings of `pr.missing_required_section`.
    pub const VALUES: &'static [&'static str] = &["draft", "refuse"];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(Self::Draft),
            "refuse" => Some(Self::Refuse),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Refuse => "refuse",
        }
    }
}

/// The former `pr.close_on_terminal` default, fixed when the key was removed
/// [ORB-22808]: a task's open Orbit-authored pull requests close when the
/// task reaches done, rejected or archived.
pub const PR_CLOSE_ON_TERMINAL: bool = true;
/// The former `pr.screenshot_section` default, fixed when the key was removed
/// [ORB-22808]: single-task bodies carry the Screenshots table for tasks
/// tagged `ui`.
pub const PR_SCREENSHOT_SECTION: ScreenshotSection = ScreenshotSection::Auto;
/// The former `pr.freshness_section` default, fixed when the key was removed
/// [ORB-22808]: generated bodies carry the Branch Freshness section.
pub const PR_FRESHNESS_SECTION: bool = true;
/// The former `pr.signature` default, fixed when the key was removed
/// [ORB-22808]: generated bodies end with the authoring model's signature.
pub const PR_SIGNATURE: bool = true;
/// The former `pr.orchestration_trailer` default, fixed when the key was
/// removed [ORB-22808]: generated bodies carry the `Orchestrated-By:` line.
pub const PR_ORCHESTRATION_TRAILER: bool = true;
/// The former `pr.draft` default, fixed when the key was removed
/// [ORB-22808]: a pull request opens ready for review, and as a draft only
/// for a required-section gap under [`MissingSectionPolicy::Draft`].
pub const PR_DRAFT: bool = false;
/// The former `commit.orchestrated_by_trailer` default, fixed when the key
/// was removed [ORB-22808]: commit messages carry `Orchestrated-By:`.
pub const COMMIT_ORCHESTRATED_BY_TRAILER: bool = true;
/// The former `commit.regression_trailers` default, fixed when the key was
/// removed [ORB-22808]: commit messages carry the `Fixes`, `Regression-From`
/// and `Regression` trailers for `regression_from` relations.
pub const COMMIT_REGRESSION_TRAILERS: bool = true;

/// When a single-task pull request body carries the screenshot section
/// ([`PR_SCREENSHOT_SECTION`] for configured deliveries).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScreenshotSection {
    /// Only for tasks tagged [`UI_TASK_TAG`].
    #[default]
    Auto,
    /// For every single-task delivery.
    Always,
    /// Never.
    Off,
}

impl ScreenshotSection {
    /// Accepted spellings of a screenshot-section policy.
    pub const VALUES: &'static [&'static str] = &["auto", "always", "off"];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "always" => Some(Self::Always),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Off => "off",
        }
    }

    /// Whether a task with `tags` gets the section.
    pub fn applies(self, tags: &[String]) -> bool {
        match self {
            Self::Auto => tags
                .iter()
                .any(|tag| tag.trim().eq_ignore_ascii_case(UI_TASK_TAG)),
            Self::Always => true,
            Self::Off => false,
        }
    }
}

/// Where a pull request body template comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrBodyTemplateSource {
    /// The repository's GitHub pull request template
    /// ([`REPO_PR_TEMPLATE_LOCATIONS`]).
    Repository,
    /// A repository-relative path.
    Path(String),
}

impl PrBodyTemplateSource {
    /// Admit a `pr.body_template` value: `"repo"` or a repository-relative
    /// path that stays inside the repository.
    pub fn parse(key: &str, value: &str) -> Result<Self, ConventionTemplateError> {
        let value = value.trim();
        if value == REPO_PR_BODY_TEMPLATE {
            return Ok(Self::Repository);
        }
        let invalid = |reason: &str| ConventionTemplateError::Invalid {
            key: key.to_string(),
            reason: format!("'{value}' {reason}"),
        };
        if value.is_empty() {
            return Err(invalid(
                "is empty; use \"repo\" or a repository-relative path",
            ));
        }
        if value.starts_with('/') || value.contains('\\') || value.contains(':') {
            return Err(invalid("must be a repository-relative path"));
        }
        if value
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err(invalid(
                "must not contain empty, '.' or '..' path components",
            ));
        }
        Ok(Self::Path(value.to_string()))
    }
}

/// A workspace's delivery conventions, admitted from configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryConventions {
    /// `pr.title_template`.
    pub pr_title: Option<ConventionTemplate>,
    /// `pr.body_template`.
    pub pr_body: Option<PrBodyTemplateSource>,
    /// `pr.agent_sections`: headings the implementer may fill.
    pub agent_sections: Vec<String>,
    /// `pr.required_sections`: headings the implementer must fill.
    pub required_sections: Vec<String>,
    /// `pr.missing_required_section`.
    pub missing_section: MissingSectionPolicy,
    /// When the Screenshots table renders; [`PR_SCREENSHOT_SECTION`].
    pub screenshot_section: ScreenshotSection,
    /// The Branch Freshness section; [`PR_FRESHNESS_SECTION`].
    pub freshness_section: bool,
    /// The `*authored by: <model>*` line; [`PR_SIGNATURE`].
    pub signature: bool,
    /// The `Orchestrated-By:` line of the body; [`PR_ORCHESTRATION_TRAILER`].
    pub pr_orchestration_trailer: bool,
    /// `pr.labels`.
    pub labels: Vec<String>,
    /// `pr.task_tag_labels`: exact allowlist of task tags mirrored to labels.
    pub task_tag_labels: Vec<String>,
    /// `pr.task_type_labels`: canonical task type to label name.
    pub task_type_labels: BTreeMap<String, String>,
    /// `pr.reviewers`.
    pub reviewers: Vec<String>,
    /// Open every pull request as a draft; [`PR_DRAFT`].
    pub draft: bool,
    /// `commit.subject_template`.
    pub commit_subject: Option<ConventionTemplate>,
    /// `commit.body_template`.
    pub commit_body: Option<ConventionTemplate>,
    /// The commit `Orchestrated-By:` trailer; [`COMMIT_ORCHESTRATED_BY_TRAILER`].
    pub orchestrated_by_trailer: bool,
    /// The commit regression trailers; [`COMMIT_REGRESSION_TRAILERS`].
    pub regression_trailers: bool,
    /// `branch.name_template`, else [`DEFAULT_BRANCH_TEMPLATE`].
    pub branch_name: ConventionTemplate,
}

impl Default for DeliveryConventions {
    fn default() -> Self {
        Self {
            pr_title: None,
            pr_body: None,
            agent_sections: Vec::new(),
            required_sections: Vec::new(),
            missing_section: MissingSectionPolicy::Draft,
            screenshot_section: PR_SCREENSHOT_SECTION,
            freshness_section: PR_FRESHNESS_SECTION,
            signature: PR_SIGNATURE,
            pr_orchestration_trailer: PR_ORCHESTRATION_TRAILER,
            labels: Vec::new(),
            task_tag_labels: Vec::new(),
            task_type_labels: BTreeMap::new(),
            reviewers: Vec::new(),
            draft: PR_DRAFT,
            commit_subject: None,
            commit_body: None,
            orchestrated_by_trailer: COMMIT_ORCHESTRATED_BY_TRAILER,
            regression_trailers: COMMIT_REGRESSION_TRAILERS,
            branch_name: default_branch_template(),
        }
    }
}

impl DeliveryConventions {
    /// Every agent-filled heading, optional ones first, without duplicates
    /// (compared case-insensitively).
    pub fn agent_filled_sections(&self) -> Vec<&str> {
        let mut sections: Vec<&str> = Vec::new();
        for name in self.agent_sections.iter().chain(&self.required_sections) {
            if !sections.iter().any(|known| same_section_name(known, name)) {
                sections.push(name);
            }
        }
        sections
    }

    /// The implement step's instruction to fill the agent-filled sections,
    /// or `None` when none is configured.
    pub fn implement_instruction(&self) -> Option<String> {
        let sections = self.agent_filled_sections();
        if sections.is_empty() {
            return None;
        }
        let listed = sections
            .iter()
            .map(|name| {
                let required = if self.is_required_section(name) {
                    " (required)"
                } else {
                    ""
                };
                format!("- `{}`{required}", name.trim())
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(format!(
            "Pull request sections\n\nThis workspace's pull requests carry sections only the \
             implementer can write. Before handoff, write `.orbit/tmp/{PR_SECTIONS_ARTIFACT}` as \
             `{{\"sections\": {{\"<heading>\": \"<markdown>\"}}}}` with one entry per heading \
             below, and attach it as task artifact `{PR_SECTIONS_ARTIFACT}` (in claimed mode, \
             list it in `task_artifacts`). Write each section's content without its heading. \
             Delivery publishes only this artifact, never your final message; a required section \
             left out opens the pull request as a draft or holds it.\n\n{listed}"
        ))
    }

    /// Whether `name` is a required section.
    pub fn is_required_section(&self, name: &str) -> bool {
        self.required_sections
            .iter()
            .any(|required| same_section_name(required, name))
    }
}

/// Heading names compare trimmed and case-insensitively.
pub fn same_section_name(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

/// The built-in branch template, [`DEFAULT_BRANCH_TEMPLATE`].
pub fn default_branch_template() -> ConventionTemplate {
    ConventionTemplate {
        source: DEFAULT_BRANCH_TEMPLATE.to_string(),
        segments: vec![
            Segment::Placeholder(Placeholder::Prefix),
            Segment::Literal("/".to_string()),
            Segment::Placeholder(Placeholder::TaskId),
            Segment::Literal("-".to_string()),
            Segment::Placeholder(Placeholder::Hash),
        ],
    }
}

/// Why a rendered branch name is not a safe Git branch, or `None` when it
/// is. Applies the branch-prefix rules (no `..`, no leading `-`) to the
/// whole name, plus Git's ref-name rules for each path component.
pub fn unsafe_branch_name_reason(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("is empty".to_string());
    }
    if name.contains("..") {
        return Some("must not contain '..'".to_string());
    }
    if name.starts_with('-') {
        return Some("must not start with '-'".to_string());
    }
    if name.contains("@{") || name == "@" {
        return Some("must not contain '@{'".to_string());
    }
    if let Some(c) = name
        .chars()
        .find(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(*c))
    {
        return Some(format!("must not contain {c:?}"));
    }
    for component in name.split('/') {
        if component.is_empty() {
            return Some("must not have an empty path component".to_string());
        }
        if component.starts_with('.') || component.ends_with(".lock") {
            return Some(format!(
                "component '{component}' must not start with '.' or end with '.lock'"
            ));
        }
    }
    if name.ends_with('.') {
        return Some("must not end with '.'".to_string());
    }
    None
}
