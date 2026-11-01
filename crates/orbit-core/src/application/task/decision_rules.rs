//! Curated corpus decision rules: parse, consolidate, and cite.
//!
//! A rule lives in a decision document's optional `## Rules` section, one
//! fenced YAML document per rule. Consolidation returns proposed file text.
//! It never writes the corpus. Only a `current` rule whose review date has
//! not passed and whose `depends_on` digests still match may be cited or
//! auto-answer. `auto` is never turned on here.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use orbit_types::identity::lower_hex;

const RULES_HEADING: &str = "## Rules";
const REVIEW_WINDOW_DAYS: i64 = 90;
const OPERATOR_AUTO: &str = "operator";

/// Status of one curated rule. Document frontmatter status is a different field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusRuleStatus {
    /// May be cited, and may auto-answer when the operator set `auto`.
    Current,
    /// A dependency changed or `review_by` has passed. Not authority.
    Stale,
    /// A later human answer chose a different option. Not authority.
    Superseded,
}

impl CorpusRuleStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "current" => Some(Self::Current),
            "stale" => Some(Self::Stale),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Superseded => "superseded",
        }
    }
}

/// One human answer a rule cites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusRuleCitation {
    /// Task that recorded the human answer.
    pub task: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// Option id the human chose.
    pub option: String,
}

/// A path or config key the rule stops being current without, plus the digest
/// last confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusRuleDependency {
    /// `path` or `config`.
    pub kind: String,
    /// Repo-relative path, or a dotted config key.
    pub name: String,
    /// SHA-256 hex of the bytes last confirmed.
    pub digest: String,
}

/// One curated rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusDecisionRule {
    /// Stable id inside its topic file.
    pub id: String,
    /// Question pattern this rule answers.
    pub question: String,
    /// Option id the rule chooses.
    pub answer: String,
    /// Why that option is the rule.
    pub why: String,
    /// Human answers the rule was built from.
    pub citations: Vec<CorpusRuleCitation>,
    /// Dependencies last confirmed with this rule.
    pub depends_on: Vec<CorpusRuleDependency>,
    /// Whether the rule may be cited.
    pub status: CorpusRuleStatus,
    /// `YYYY-MM-DD`. Past this date the rule is stale.
    pub review_by: String,
    /// Operator opt-in. Consolidation never sets this true.
    pub auto: bool,
    /// `operator` when the operator set [`Self::auto`]. Absent otherwise.
    pub auto_set_by: Option<String>,
}

/// A corpus decision document the consolidator reads. `path` is relative to
/// `docs/agents/corpus/`, such as `decisions/pricing.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusDecisionDocument {
    /// Path relative to the corpus root.
    pub path: String,
    /// Full markdown, including frontmatter.
    pub markdown: String,
}

/// One human answer eligible to merge into a rule. Precedent answers are not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsolidationAnswer {
    /// Task the answer completed.
    pub task_id: String,
    /// Structured topic or `file/rule` id.
    pub topic: String,
    /// Question text.
    pub question: String,
    /// Chosen option id.
    pub answer_id: String,
    /// Operator note, when they left one.
    pub note: Option<String>,
    /// `YYYY-MM-DD`.
    pub answered_on: String,
}

/// Current digest of one dependency, supplied by the caller so a test is
/// deterministic and this function writes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusDigest {
    /// `path` or `config`.
    pub kind: String,
    /// Repo-relative path, or a dotted config key.
    pub name: String,
    /// SHA-256 hex.
    pub digest: String,
}

/// A file the consolidation pass would put in its pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedCorpusDocument {
    /// Path relative to the corpus root.
    pub path: String,
    /// Proposed markdown. The caller opens the pull request; nothing here writes.
    pub markdown: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuleYaml {
    id: String,
    question: String,
    answer: String,
    why: String,
    #[serde(default)]
    citations: Vec<CitationYaml>,
    #[serde(default)]
    depends_on: Vec<DependencyYaml>,
    status: String,
    review_by: String,
    #[serde(default)]
    auto: bool,
    #[serde(default)]
    auto_set_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CitationYaml {
    task: String,
    date: String,
    option: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DependencyYaml {
    kind: String,
    name: String,
    digest: String,
}

/// SHA-256 hex of `bytes`.
pub fn corpus_content_digest(bytes: &[u8]) -> String {
    lower_hex(Sha256::digest(bytes))
}

/// SHA-256 hex of the config binding `key=value`, or `key=absent`.
pub fn corpus_config_digest(key: &str, value: Option<&str>) -> String {
    let rendered = match value {
        Some(value) => format!("{key}={value}"),
        None => format!("{key}=absent"),
    };
    corpus_content_digest(rendered.as_bytes())
}

/// Rules in `markdown` that may be injected or cited on `today` (`YYYY-MM-DD`).
///
/// A rule qualifies only when its status is `current`, `review_by` is a date
/// that has not passed, and every `depends_on` digest equals the matching
/// entry in `digests`. A missing digest is a change.
pub fn citable_decision_rules(
    markdown: &str,
    today: &str,
    digests: &[CorpusDigest],
) -> Vec<CorpusDecisionRule> {
    parse_decision_rules(markdown)
        .into_iter()
        .filter(|rule| rule_is_citable(rule, today, digests))
        .collect()
}

/// The unique current operator-approved auto rule `topic` names, when its
/// answer is one of `option_ids`. Anything ambiguous, stale, non-auto, or
/// not an offered option is `None`.
pub fn match_auto_decision_rule(
    documents: &[CorpusDecisionDocument],
    topic: &str,
    option_ids: &[String],
    today: &str,
    digests: &[CorpusDigest],
) -> Option<CorpusDecisionRule> {
    let candidates: Vec<(String, CorpusDecisionRule)> = documents
        .iter()
        .flat_map(|document| {
            let stem = document
                .path
                .strip_prefix("decisions/")
                .and_then(|rest| rest.strip_suffix(".md"))
                .unwrap_or("")
                .to_string();
            citable_decision_rules(&document.markdown, today, digests)
                .into_iter()
                .filter(|rule| rule.auto && rule.auto_set_by.as_deref() == Some(OPERATOR_AUTO))
                .map(move |rule| (stem.clone(), rule))
        })
        .collect();
    let rule = if let Some((stem, rule_id)) = topic.split_once('/') {
        if topic.matches('/').count() != 1 {
            return None;
        }
        candidates
            .into_iter()
            .find_map(|(file, rule)| (file == stem && rule.id == rule_id).then_some(rule))
    } else {
        // A bare topic is a rule id. It answers only when that id appears in
        // exactly one rule across every decision file, whatever its status or
        // auto flag, so a collision anywhere keeps the request open. A file
        // stem is not a topic.
        let id_count = documents
            .iter()
            .flat_map(|document| parse_decision_rules(&document.markdown))
            .filter(|rule| rule.id == topic)
            .count();
        let by_id: Vec<_> = candidates
            .into_iter()
            .filter(|(_, rule)| rule.id == topic)
            .map(|(_, rule)| rule)
            .collect();
        if id_count == 1 {
            by_id.into_iter().next()
        } else {
            None
        }
    }?;
    option_ids
        .iter()
        .any(|option| option == &rule.answer)
        .then_some(rule)
}

/// Merge `answers` into `documents` and re-check every rule. Returns only
/// the files a pull request would change, including new topic files. The
/// input documents are not modified.
pub fn consolidate_decision_corpus(
    documents: &[CorpusDecisionDocument],
    answers: &[ConsolidationAnswer],
    digests: &[CorpusDigest],
    today: &str,
) -> Vec<ProposedCorpusDocument> {
    let mut by_path: BTreeMap<String, String> = documents
        .iter()
        .map(|document| (document.path.clone(), document.markdown.clone()))
        .collect();
    let mut rules_by_path: BTreeMap<String, Vec<CorpusDecisionRule>> = BTreeMap::new();
    for (path, markdown) in &by_path {
        rules_by_path.insert(path.clone(), parse_decision_rules(markdown));
    }
    let mut confirmed: BTreeSet<(String, String)> = BTreeSet::new();
    let mut ordered: Vec<ConsolidationAnswer> = answers
        .iter()
        .filter(|answer| answer.answered_on.len() == 10)
        .cloned()
        .collect();
    ordered.sort_by(|left, right| {
        left.answered_on
            .cmp(&right.answered_on)
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    for answer in ordered {
        apply_answer(
            &mut by_path,
            &mut rules_by_path,
            &mut confirmed,
            &answer,
            digests,
        );
    }
    for (path, rules) in rules_by_path.iter_mut() {
        for rule in rules.iter_mut() {
            if confirmed.contains(&(path.clone(), rule.id.clone())) {
                continue;
            }
            mark_stale(rule, today, digests);
        }
        if let Some(markdown) = by_path.get(path) {
            let rewritten = rewrite_rules_section(markdown, rules);
            by_path.insert(path.clone(), rewritten);
        }
    }
    let mut proposals = Vec::new();
    for (path, markdown) in by_path {
        let original = documents
            .iter()
            .find(|document| document.path == path)
            .map(|document| document.markdown.as_str());
        if original == Some(markdown.as_str()) {
            continue;
        }
        proposals.push(ProposedCorpusDocument { path, markdown });
    }
    proposals.sort_by(|left, right| left.path.cmp(&right.path));
    proposals
}

fn apply_answer(
    documents: &mut BTreeMap<String, String>,
    rules_by_path: &mut BTreeMap<String, Vec<CorpusDecisionRule>>,
    confirmed: &mut BTreeSet<(String, String)>,
    answer: &ConsolidationAnswer,
    digests: &[CorpusDigest],
) {
    let Some((stem, rule_id)) = answer_target(rules_by_path, &answer.topic) else {
        return;
    };
    let path = format!("decisions/{stem}.md");
    let rules = rules_by_path.entry(path.clone()).or_default();
    if !documents.contains_key(&path) {
        documents.insert(path.clone(), new_topic_document(&stem));
    }
    let citation = CorpusRuleCitation {
        task: answer.task_id.clone(),
        date: answer.answered_on.clone(),
        option: answer.answer_id.clone(),
    };
    let why = answer
        .note
        .as_deref()
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .unwrap_or("The operator chose this option.")
        .to_string();
    // A replayed answer is history, not a new confirmation. It must not
    // refresh authority or replace a choice with a later citation in its
    // current or superseded history.
    let superseded_prefix = format!("{rule_id}--superseded-");
    if rules.iter().any(|rule| {
        (rule.id == rule_id || rule.id.starts_with(&superseded_prefix))
            && rule
                .citations
                .iter()
                .any(|citation| citation.date.as_str() > answer.answered_on.as_str())
    }) {
        return;
    }
    if rules.iter().any(|rule| {
        (rule.id == rule_id || rule.id.starts_with(&superseded_prefix))
            && rule.citations.contains(&citation)
    }) {
        return;
    }
    if let Some(index) = rules.iter().position(|rule| rule.id == rule_id) {
        if rules[index].status == CorpusRuleStatus::Superseded {
            return;
        }
        if rules[index].answer == answer.answer_id {
            let rule = &mut rules[index];
            rule.citations.push(citation);
            rule.why = why;
            rule.status = CorpusRuleStatus::Current;
            rule.review_by = review_by_from(&answer.answered_on);
            refresh_digests(&mut rule.depends_on, digests);
            confirmed.insert((path, rule_id));
            return;
        }
        let previous = rules[index].clone();
        let mut taken = BTreeSet::new();
        for rule in rules.iter() {
            taken.insert(rule.id.clone());
        }
        rules[index].id = superseded_id(&taken, &rule_id, &answer.answered_on);
        rules[index].status = CorpusRuleStatus::Superseded;
        rules[index].auto = false;
        rules[index].auto_set_by = None;
        let mut depends_on = previous.depends_on;
        refresh_digests(&mut depends_on, digests);
        rules.push(CorpusDecisionRule {
            id: rule_id.clone(),
            question: collapse(&answer.question),
            answer: answer.answer_id.clone(),
            why,
            citations: vec![citation],
            depends_on,
            status: CorpusRuleStatus::Current,
            review_by: review_by_from(&answer.answered_on),
            auto: false,
            auto_set_by: None,
        });
        confirmed.insert((path, rule_id));
        return;
    }
    rules.push(CorpusDecisionRule {
        id: rule_id.clone(),
        question: collapse(&answer.question),
        answer: answer.answer_id.clone(),
        why,
        citations: vec![citation],
        depends_on: Vec::new(),
        status: CorpusRuleStatus::Current,
        review_by: review_by_from(&answer.answered_on),
        auto: false,
        auto_set_by: None,
    });
    confirmed.insert((path, rule_id));
}

fn mark_stale(rule: &mut CorpusDecisionRule, today: &str, digests: &[CorpusDigest]) {
    if rule.status == CorpusRuleStatus::Superseded {
        return;
    }
    let review_past = NaiveDate::parse_from_str(&rule.review_by, "%Y-%m-%d")
        .ok()
        .and_then(|review| {
            NaiveDate::parse_from_str(today, "%Y-%m-%d")
                .ok()
                .map(|today| review < today)
        })
        .unwrap_or(true);
    let dependency_changed = rule.depends_on.iter().any(|dependency| {
        digest_of(digests, &dependency.kind, &dependency.name)
            .is_none_or(|digest| digest != dependency.digest)
    });
    if review_past || dependency_changed {
        rule.status = CorpusRuleStatus::Stale;
    }
}

fn rule_is_citable(rule: &CorpusDecisionRule, today: &str, digests: &[CorpusDigest]) -> bool {
    if rule.status != CorpusRuleStatus::Current {
        return false;
    }
    let Ok(review) = NaiveDate::parse_from_str(&rule.review_by, "%Y-%m-%d") else {
        return false;
    };
    let Ok(today) = NaiveDate::parse_from_str(today, "%Y-%m-%d") else {
        return false;
    };
    if review < today {
        return false;
    }
    rule.depends_on.iter().all(|dependency| {
        digest_of(digests, &dependency.kind, &dependency.name)
            .is_some_and(|digest| digest == dependency.digest)
    })
}

fn digest_of<'a>(digests: &'a [CorpusDigest], kind: &str, name: &str) -> Option<&'a str> {
    digests
        .iter()
        .find(|digest| digest.kind == kind && digest.name == name)
        .map(|digest| digest.digest.as_str())
}

fn refresh_digests(dependencies: &mut [CorpusRuleDependency], digests: &[CorpusDigest]) {
    for dependency in dependencies {
        if let Some(digest) = digest_of(digests, &dependency.kind, &dependency.name) {
            dependency.digest = digest.to_string();
        }
    }
}

fn review_by_from(answered_on: &str) -> String {
    NaiveDate::parse_from_str(answered_on, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.checked_add_signed(chrono::Duration::days(REVIEW_WINDOW_DAYS)))
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| answered_on.to_string())
}

/// The `(file stem, rule id)` an answer's topic names. A qualified topic names
/// both. A bare topic names the one existing rule with that id, in whichever
/// file holds it; with no such rule, or several, it keeps the new-file default
/// of `topic/topic`.
fn answer_target(
    rules_by_path: &BTreeMap<String, Vec<CorpusDecisionRule>>,
    topic: &str,
) -> Option<(String, String)> {
    let default = topic_file_and_rule(topic)?;
    if topic.trim().contains('/') {
        return Some(default);
    }
    let mut holders = rules_by_path.iter().filter_map(|(path, rules)| {
        let holds = rules.iter().any(|rule| rule.id == default.1);
        let stem = path
            .strip_prefix("decisions/")
            .and_then(|rest| rest.strip_suffix(".md"))?;
        holds.then(|| stem.to_string())
    });
    match (holders.next(), holders.next()) {
        (Some(stem), None) => Some((stem, default.1)),
        _ => Some(default),
    }
}

fn topic_file_and_rule(topic: &str) -> Option<(String, String)> {
    let topic = topic.trim();
    if topic.is_empty() || topic.matches('/').count() > 1 {
        return None;
    }
    let (stem, rule) = topic.split_once('/').unwrap_or((topic, topic));
    (valid_slug(stem) && valid_slug(rule)).then(|| (stem.to_string(), rule.to_string()))
}

fn valid_slug(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn superseded_id(taken: &BTreeSet<String>, id: &str, date: &str) -> String {
    let base = format!("{id}--superseded-{date}");
    if !taken.contains(&base) {
        return base;
    }
    let mut suffix = 2u32;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn collapse(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Rules under `## Rules`, ignoring fences that are not a valid rule.
pub fn parse_decision_rules(markdown: &str) -> Vec<CorpusDecisionRule> {
    let Some(section) = rules_section(markdown) else {
        return Vec::new();
    };
    let mut rules = Vec::new();
    let mut lines = section.lines().peekable();
    while let Some(line) = lines.next() {
        let fence = line.trim();
        if fence != "```yaml" && fence != "```yml" {
            continue;
        }
        let mut body = String::new();
        for inner in lines.by_ref() {
            if inner.trim() == "```" {
                break;
            }
            body.push_str(inner);
            body.push('\n');
        }
        if let Some(rule) = parse_rule_yaml(&body) {
            rules.push(rule);
        }
    }
    rules
}

fn parse_rule_yaml(body: &str) -> Option<CorpusDecisionRule> {
    let parsed: RuleYaml = serde_yaml::from_str(body).ok()?;
    let status = CorpusRuleStatus::parse(&parsed.status)?;
    if !valid_slug(&parsed.id)
        || parsed.question.trim().is_empty()
        || parsed.answer.trim().is_empty()
    {
        return None;
    }
    let citations = parsed
        .citations
        .into_iter()
        .filter(|citation| {
            !citation.task.trim().is_empty()
                && NaiveDate::parse_from_str(citation.date.trim(), "%Y-%m-%d").is_ok()
                && !citation.option.trim().is_empty()
        })
        .map(|citation| CorpusRuleCitation {
            task: citation.task.trim().to_string(),
            date: citation.date.trim().to_string(),
            option: citation.option.trim().to_string(),
        })
        .collect();
    let depends_on = parsed
        .depends_on
        .into_iter()
        .filter(|dependency| dependency_name_ok(&dependency.kind, &dependency.name))
        .map(|dependency| CorpusRuleDependency {
            kind: dependency.kind,
            name: dependency.name,
            digest: dependency.digest.trim().to_string(),
        })
        .collect();
    let auto_set_by = parsed.auto_set_by.and_then(|value| {
        let value = value.trim().to_string();
        (value == OPERATOR_AUTO).then_some(value)
    });
    Some(CorpusDecisionRule {
        id: parsed.id,
        question: collapse(&parsed.question),
        answer: parsed.answer.trim().to_string(),
        why: {
            let why = collapse(&parsed.why);
            if why.is_empty() {
                "The operator chose this option.".to_string()
            } else {
                why
            }
        },
        citations,
        depends_on,
        status,
        review_by: parsed.review_by.trim().to_string(),
        auto: parsed.auto,
        auto_set_by: parsed.auto.then_some(auto_set_by).flatten(),
    })
}

fn dependency_name_ok(kind: &str, name: &str) -> bool {
    match kind {
        "path" => {
            !name.is_empty()
                && !name.starts_with('/')
                && !name
                    .split(['/', '\\'])
                    .any(|part| part.is_empty() || part == "." || part == "..")
        }
        "config" => {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        }
        _ => false,
    }
}

fn rules_section(markdown: &str) -> Option<&str> {
    let start = markdown.find(RULES_HEADING)? + RULES_HEADING.len();
    let body = &markdown[start..];
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        if line.starts_with("## ") {
            return Some(&body[..offset]);
        }
        offset += line.len();
    }
    Some(body)
}

// Byte spans are derived from the original lines, including their line endings.
fn rule_fences(section: &str) -> Vec<(usize, usize, CorpusDecisionRule)> {
    let mut fences = Vec::new();
    let mut offset = 0;
    let mut opening = None;
    for line in section.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim();
        if let Some((fence_start, body_start)) = opening {
            if trimmed == "```" {
                if let Some(rule) = parse_rule_yaml(&section[body_start..start]) {
                    fences.push((fence_start, offset, rule));
                }
                opening = None;
            }
        } else if trimmed == "```yaml" || trimmed == "```yml" {
            opening = Some((start, offset));
        }
    }
    // The reader (`parse_decision_rules`) accepts a final fence whose body
    // runs to the end of the section, so the span editor must too.
    if let Some((fence_start, body_start)) = opening
        && let Some(rule) = parse_rule_yaml(&section[body_start..])
    {
        fences.push((fence_start, section.len(), rule));
    }
    fences
}

fn fence_is_unclosed(section: &str, start: usize, end: usize) -> bool {
    section[start..end]
        .trim_end_matches(['\r', '\n'])
        .lines()
        .last()
        .map(|line| line.trim() != "```")
        .unwrap_or(true)
}

fn rewrite_rules_section(markdown: &str, rules: &[CorpusDecisionRule]) -> String {
    let Some(section) = rules_section(markdown) else {
        let mut out = markdown.trim_end().to_string();
        out.push_str("\n\n## Rules\n\n");
        out.push_str(&render_rules(rules));
        return out;
    };
    let section_start = section.as_ptr() as usize - markdown.as_ptr() as usize;
    let mut out = markdown[..section_start].to_string();
    let mut cursor = 0;
    let mut used = 0;
    let newline = if markdown.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    for (start, end, original) in rule_fences(section) {
        out.push_str(&section[cursor..start]);
        if let Some(rule) = rules.get(used) {
            if *rule == original {
                out.push_str(&section[start..end]);
                if fence_is_unclosed(section, start, end) {
                    if !out.ends_with('\n') {
                        out.push_str(newline);
                    }
                    out.push_str("```");
                    out.push_str(newline);
                }
            } else {
                let rendered = render_rules(std::slice::from_ref(rule));
                out.push_str(
                    rendered
                        .trim_end_matches('\n')
                        .replace('\n', newline)
                        .as_str(),
                );
                out.push_str(newline);
            }
            used += 1;
        }
        cursor = end;
    }
    out.push_str(&section[cursor..]);
    if used < rules.len() {
        if !out.ends_with('\n') {
            out.push_str(newline);
        }
        out.push_str(newline);
        out.push_str(&render_rules(&rules[used..]).replace('\n', newline));
    }
    out.push_str(&markdown[section_start + section.len()..]);
    out
}

fn render_rules(rules: &[CorpusDecisionRule]) -> String {
    let mut out = String::new();
    for rule in rules {
        out.push_str("```yaml\n");
        out.push_str(&render_rule(rule));
        out.push_str("```\n\n");
    }
    out
}

fn render_rule(rule: &CorpusDecisionRule) -> String {
    let mut out = String::new();
    push_scalar(&mut out, "id", &rule.id);
    push_scalar(&mut out, "question", &rule.question);
    push_scalar(&mut out, "answer", &rule.answer);
    push_scalar(&mut out, "why", &rule.why);
    out.push_str("citations:\n");
    if rule.citations.is_empty() {
        out.push_str("  []\n");
    } else {
        for citation in &rule.citations {
            out.push_str(&format!("  - task: {}\n", yaml_quote(&citation.task)));
            out.push_str(&format!("    date: {}\n", yaml_quote(&citation.date)));
            out.push_str(&format!("    option: {}\n", yaml_quote(&citation.option)));
        }
    }
    out.push_str("depends_on:\n");
    if rule.depends_on.is_empty() {
        out.push_str("  []\n");
    } else {
        for dependency in &rule.depends_on {
            out.push_str(&format!("  - kind: {}\n", yaml_quote(&dependency.kind)));
            out.push_str(&format!("    name: {}\n", yaml_quote(&dependency.name)));
            out.push_str(&format!("    digest: {}\n", yaml_quote(&dependency.digest)));
        }
    }
    push_scalar(&mut out, "status", rule.status.as_str());
    push_scalar(&mut out, "review_by", &rule.review_by);
    out.push_str(&format!(
        "auto: {}\n",
        if rule.auto { "true" } else { "false" }
    ));
    if rule.auto
        && let Some(actor) = &rule.auto_set_by
    {
        push_scalar(&mut out, "auto_set_by", actor);
    }
    out
}

fn push_scalar(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(": ");
    out.push_str(&yaml_quote(value));
    out.push('\n');
}

fn yaml_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn new_topic_document(stem: &str) -> String {
    let title = stem
        .split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "---\ntype: decision\napplies_to: [agent_implement]\nstatus: active\nlinks: []\n---\n\n# {title}\n\n## Scope\n\nCurated rules for this topic. Only a current rule may be cited. A rule answers a new request only when the operator has set auto on it and a human has merged that edit.\n\n## Entries\n\nRaw decision history is not a rule. Human answers land here only as citations on a rule under Rules.\n\n"
    )
}

/// Replace `auto` on the rule `rule_ref` (`stem` or `stem/id`) inside `markdown`.
/// Returns the rewritten document and the rule. `auto_set_by` is `operator`
/// only when `auto` is true; turning auto off clears it.
pub fn set_rule_auto_in_markdown(
    markdown: &str,
    rule_ref: &str,
    auto: bool,
) -> Option<(String, CorpusDecisionRule)> {
    let (stem_hint, rule_id) = rule_ref.split_once('/').unwrap_or((rule_ref, rule_ref));
    if !valid_slug(rule_id) || (rule_ref.contains('/') && !valid_slug(stem_hint)) {
        return None;
    }
    let section = rules_section(markdown)?;
    let (start, end, mut rule) = rule_fences(section)
        .into_iter()
        .find(|(_, _, rule)| rule.id == rule_id)?;
    if rule.status != CorpusRuleStatus::Current {
        return None;
    }
    rule.auto = auto;
    rule.auto_set_by = auto.then(|| OPERATOR_AUTO.to_string());
    let fence = &section[start..end];
    let newline = if fence.contains("\r\n") { "\r\n" } else { "\n" };
    let mut replacement = String::new();
    let mut found_auto = false;
    let mut found_actor = false;
    let mut found_closing = false;
    for line in fence.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let ending = &line[content.len()..];
        if content.starts_with("auto:") {
            replacement.push_str(if auto { "auto: true" } else { "auto: false" });
            replacement.push_str(ending);
            found_auto = true;
        } else if content.starts_with("auto_set_by:") {
            if auto {
                replacement.push_str("auto_set_by: \"operator\"");
                replacement.push_str(ending);
            }
            found_actor = true;
        } else if content.trim() == "```" {
            if !found_auto {
                replacement.push_str(if auto { "auto: true" } else { "auto: false" });
                replacement.push_str(newline);
            }
            if auto && !found_actor {
                replacement.push_str("auto_set_by: \"operator\"");
                replacement.push_str(newline);
            }
            replacement.push_str(line);
            found_closing = true;
        } else {
            replacement.push_str(line);
        }
    }
    if !found_closing {
        if !replacement.ends_with('\n') {
            replacement.push_str(newline);
        }
        if !found_auto {
            replacement.push_str(if auto { "auto: true" } else { "auto: false" });
            replacement.push_str(newline);
        }
        if auto && !found_actor {
            replacement.push_str("auto_set_by: \"operator\"");
            replacement.push_str(newline);
        }
        replacement.push_str("```");
        replacement.push_str(newline);
    }
    // Refuse forms we cannot safely edit as top-level lines (e.g. flow YAML).
    let replacement_rule = rule_fences(&replacement).into_iter().next()?.2;
    if replacement_rule != rule {
        return None;
    }
    let section_start = section.as_ptr() as usize - markdown.as_ptr() as usize;
    let mut out = markdown[..section_start + start].to_string();
    out.push_str(&replacement);
    out.push_str(&markdown[section_start + end..]);
    Some((out, rule))
}
