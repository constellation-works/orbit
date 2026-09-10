//! Bounded logical remediation groups for Code scanning alerts.
//!
//! GitHub reports one alert per location, so a single repair — one tainted
//! source, one unsafe construct — arrives as several alerts at sibling line
//! offsets or across several files. One task per alert produced dozens of
//! records for one edit and sent executors to reconcile work another task had
//! already landed (F2026-09-097, F2026-09-105).
//!
//! Alerts join one group when their *scan provenance* (tool identity and
//! analyzed ref), their *rule* and their *scanner message* all agree. No one
//! of those carries the decision: the same rule twice in one file with
//! different messages describes two findings and stays split, and the same
//! message under a different rule or from a different analysis stays split
//! too. Line and column offsets are deliberately absent from the signature —
//! that is what lets sibling locations of one data flow collapse into one
//! repair.
//!
//! Coverage stays per alert. Every alert keeps the generated key it has always
//! had, a group task carries the key of every member, and the sweep asks the
//! shared duplicate check once per alert. Existing per-alert tasks therefore
//! keep covering their alert with no migration, and an alert that appears
//! after a group was filed becomes explicit delta work instead of silently
//! widening a task somebody may already be running.

use std::collections::{BTreeMap, BTreeSet};

use orbit_types::task::{TaskComplexity, TaskRelation, TaskRelationType, TaskStatus, TaskType};
use serde_json::{Value, json};

use crate::application::task::TaskAddParams;

use super::{
    CODE_GROUP_KEY_PREFIX, CODE_KEY_PREFIX, CODE_TAG, CODE_TITLE_PREFIX, alert_number,
    append_collection_bounds, digest, display, field, line_suffix, priority_for_rank,
    repository_name, severity_rank, truncate_chars,
};

/// One leaf task has to stay executable, so a group is bounded both by how
/// many alerts it carries and by how many distinct files it spans. A cause
/// that exceeds either bound splits into consecutive groups rather than
/// growing into a task nobody can finish in one pass.
const MAX_GROUP_ALERTS: usize = 12;
const MAX_GROUP_PATHS: usize = 5;

/// How many alert locations an acceptance criterion names inline before it
/// defers to the ledger in the description.
const MAX_INLINE_LOCATIONS: usize = 5;

/// A set of Code scanning alerts that the collected evidence says share one
/// repair.
pub(super) struct CodeAlertGroup {
    pub(super) cause_key: String,
    pub(super) alerts: Vec<Value>,
}

/// An alert of the same cause that an open task already owns.
pub(super) struct CoveredAlert {
    pub(super) number: u64,
    pub(super) task_id: String,
}

/// Everything the sweep and the consolidation path both need to render one
/// group task through the same builder.
pub(super) struct CodeGroupTaskRequest<'a> {
    /// A snapshot-shaped value: `repository.full_name` names the repository
    /// and `code_scanning.truncation`, when present, records collection bounds.
    pub(super) snapshot: &'a Value,
    pub(super) group: &'a CodeAlertGroup,
    /// Same-cause alerts left out of this task because open work owns them.
    pub(super) covered_siblings: &'a [CoveredAlert],
    pub(super) crew: Option<String>,
}

/// The generated per-alert coverage key. This is the identity the sweep has
/// always deduplicated on, so a group task carrying it covers exactly the
/// alerts it lists and nothing else.
pub(super) fn alert_key(repository: &str, alert: &Value) -> String {
    digest(&[
        "code-scanning",
        repository,
        &alert_number(alert).to_string(),
    ])
}

/// The shared-repair identity: scan provenance, rule, and canonical scanner
/// message together. Every component must agree for two alerts to group.
pub(super) fn cause_key(repository: &str, alert: &Value) -> String {
    digest(&[
        "code-scanning-cause",
        repository,
        &field(alert, "tool_name").to_ascii_lowercase(),
        &field(alert, "tool_guid").to_ascii_lowercase(),
        &field(alert, "ref"),
        &field(alert, "rule_id"),
        &canonical_message(&field(alert, "message")),
    ])
}

/// Partition `alerts` into bounded groups, ordered by their lowest alert
/// number so a snapshot and its reordering produce the same sequence.
pub(super) fn group_code_alerts(repository: &str, alerts: Vec<Value>) -> Vec<CodeAlertGroup> {
    let mut by_cause: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for alert in alerts {
        by_cause
            .entry(cause_key(repository, &alert))
            .or_default()
            .push(alert);
    }

    let mut groups = by_cause
        .into_iter()
        .flat_map(|(cause_key, alerts)| split_within_bounds(cause_key, alerts))
        .collect::<Vec<_>>();
    groups.sort_by_key(lowest_alert_number);
    groups
}

/// The bounds a caller can report so an operator can see why a cause split.
pub(super) fn group_bounds() -> Value {
    json!({"max_alerts": MAX_GROUP_ALERTS, "max_paths": MAX_GROUP_PATHS})
}

pub(super) fn group_paths(group: &CodeAlertGroup) -> Vec<String> {
    distinct_paths(&group.alerts)
}

pub(super) fn group_alert_numbers(group: &CodeAlertGroup) -> Vec<u64> {
    let mut numbers = group.alerts.iter().map(alert_number).collect::<Vec<_>>();
    numbers.sort_unstable();
    numbers
}

pub(super) fn code_group_task_params(request: &CodeGroupTaskRequest<'_>) -> TaskAddParams {
    let repository = repository_name(request.snapshot);
    let alerts = &request.group.alerts;
    let rule = field(&alerts[0], "rule_id");

    let mut tags = vec![CODE_TAG.to_string()];
    tags.extend(
        alerts
            .iter()
            .map(|alert| format!("{CODE_KEY_PREFIX}{}", alert_key(repository, alert))),
    );
    tags.push(format!(
        "{CODE_GROUP_KEY_PREFIX}{}",
        request.group.cause_key
    ));
    tags.push("security".to_string());

    TaskAddParams {
        title: group_title(&rule, request.group),
        description: group_description(request),
        acceptance_criteria: group_acceptance_criteria(&rule, request.group),
        tags,
        // Scanner locations are evidence about where the finding was observed,
        // not a reliable claim about which files the repair will modify.
        // Task-pilot resolves those selectors from its pinned checkout.
        context_files: Vec::new(),
        relations: covering_relations(request.covered_siblings),
        required_tools: Vec::new(),
        crew: request.crew.clone(),
        priority: priority_for_rank(highest_severity_rank(alerts)),
        complexity: TaskComplexity::Unassessed,
        task_type: Some(TaskType::Bug),
        status: Some(TaskStatus::Backlog),
        system_created: true,
        ..TaskAddParams::default()
    }
}

/// Locations of one cause are ordered file by file so a bound-forced split
/// lands on a file boundary whenever the bounds allow it.
fn split_within_bounds(cause_key: String, mut alerts: Vec<Value>) -> Vec<CodeAlertGroup> {
    alerts.sort_by_key(|alert| (field(alert, "path"), start_line(alert), alert_number(alert)));

    let mut groups = Vec::new();
    let mut members: Vec<Value> = Vec::new();
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for alert in alerts {
        let path = field(&alert, "path");
        let bound_reached = members.len() >= MAX_GROUP_ALERTS
            || (paths.len() >= MAX_GROUP_PATHS && !paths.contains(&path));
        if bound_reached {
            groups.push(CodeAlertGroup {
                cause_key: cause_key.clone(),
                alerts: std::mem::take(&mut members),
            });
            paths.clear();
        }
        paths.insert(path);
        members.push(alert);
    }
    if !members.is_empty() {
        groups.push(CodeAlertGroup {
            cause_key,
            alerts: members,
        });
    }
    groups
}

fn lowest_alert_number(group: &CodeAlertGroup) -> u64 {
    group
        .alerts
        .iter()
        .map(alert_number)
        .min()
        .unwrap_or_default()
}

fn highest_severity_rank(alerts: &[Value]) -> u8 {
    alerts
        .iter()
        .filter_map(|alert| severity_rank(&field(alert, "security_severity").to_ascii_lowercase()))
        .max()
        .unwrap_or(1)
}

fn distinct_paths(alerts: &[Value]) -> Vec<String> {
    let mut paths = alerts
        .iter()
        .map(|alert| field(alert, "path"))
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn start_line(alert: &Value) -> u64 {
    alert
        .get("start_line")
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

/// Lowercase the scanner message into a token sequence. Punctuation and
/// spacing vary between analyses of one finding; the words do not. Digits are
/// kept, so `CWE-79` and `CWE-89` remain distinct causes.
fn canonical_message(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut separated = true;
    for character in message.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            out.push(character);
            separated = false;
        } else if !separated {
            out.push(' ');
            separated = true;
        }
    }
    out.trim_end().to_string()
}

fn group_title(rule: &str, group: &CodeAlertGroup) -> String {
    let budget = 120usize.saturating_sub(CODE_TITLE_PREFIX.chars().count());
    let paths = distinct_paths(&group.alerts);
    let count = group.alerts.len();
    let body = match paths.as_slice() {
        _ if count == 1 => format!(
            "Fix {} in {}",
            display(rule),
            display(&field(&group.alerts[0], "path"))
        ),
        [path] => format!("Fix {} at {count} locations in {path}", display(rule)),
        paths => format!(
            "Fix {} at {count} locations across {} files",
            display(rule),
            paths.len()
        ),
    };
    format!("{CODE_TITLE_PREFIX}{}", truncate_chars(&body, budget))
}

fn group_acceptance_criteria(rule: &str, group: &CodeAlertGroup) -> Vec<String> {
    let remediate = if let [alert] = group.alerts.as_slice() {
        format!(
            "Remediate Code scanning rule `{}` at `{}`{} with a real code or configuration fix; do not suppress, dismiss, or exclude the finding.",
            display(rule),
            display(&field(alert, "path")),
            line_suffix(alert),
        )
    } else {
        format!(
            "Remediate Code scanning rule `{}` at every alert location listed in the task description ({}) with a real code or configuration fix; do not suppress, dismiss, or exclude any of them.",
            display(rule),
            inline_locations(&group.alerts),
        )
    };

    vec![
        remediate,
        "Preserve the intended behavior while removing the data flow or unsafe construct identified in the inline alert evidence.".to_string(),
        "Record one disposition for every alert listed in the task description: `fixed`, `already-covered`, or `not-reproducible-in-source`. An `already-covered` disposition must name the covering commit, show that commit is an ancestor of the HEAD you validated, and cite the current source carrying the repair; never present another task's commit as this task's own change.".to_string(),
        "Run the repository's documented validation and security checks and confirm the identified rule no longer reports at the affected locations. Report source-side coverage and hosted alert closure separately: do not record a hosted alert as closed without a hosted rescan.".to_string(),
    ]
}

fn inline_locations(alerts: &[Value]) -> String {
    let mut rendered = alerts
        .iter()
        .take(MAX_INLINE_LOCATIONS)
        .map(|alert| format!("`{}`{}", display(&field(alert, "path")), line_suffix(alert)))
        .collect::<Vec<_>>()
        .join(", ");
    if alerts.len() > MAX_INLINE_LOCATIONS {
        rendered.push_str(&format!(
            ", and {} more in the ledger",
            alerts.len() - MAX_INLINE_LOCATIONS
        ));
    }
    rendered
}

fn group_description(request: &CodeGroupTaskRequest<'_>) -> String {
    let alerts = &request.group.alerts;
    let mut out = "This task was filed from bounded Code scanning evidence collected on the engine-private host boundary. It does not require agent-side GitHub access.\n\n".to_string();

    if let [alert] = alerts.as_slice() {
        append_single_alert_evidence(&mut out, request.snapshot, alert);
    } else {
        append_shared_cause(&mut out, request.snapshot, alerts);
        append_alert_ledger(&mut out, alerts);
    }

    append_disposition_contract(&mut out);
    append_covered_siblings(&mut out, request.covered_siblings);
    append_collection_bounds(
        &mut out,
        request.snapshot.pointer("/code_scanning/truncation"),
    );

    out.push_str(if alerts.len() == 1 {
        "\nRemediate the identified rule at the affected location without suppressing or dismissing the finding, then run the repository's normal validation.\n"
    } else {
        "\nRemediate the shared cause once, confirm every location in the ledger, and run the repository's normal validation.\n"
    });
    out
}

/// The single-alert record. Its `- Key: value` bullets are also the format the
/// consolidation path reads back out of an existing per-alert task, so the
/// keys and their order are a parsed contract, not presentation.
fn append_single_alert_evidence(out: &mut String, snapshot: &Value, alert: &Value) {
    out.push_str(&format!(
        "## Alert evidence\n\n- Repository: `{}`\n- Alert: `#{}`\n- Rule: `{}` ({})\n- Security severity: `{}`\n- Tool: `{}` (version `{}`, guid `{}`)\n- Message: {}\n- Ref: `{}`\n- Commit: `{}`\n- Location: `{}`{}\n- Created: `{}`\n- Updated: `{}`\n- Alert URL: {}\n",
        repository_name(snapshot),
        field(alert, "number"),
        display(&field(alert, "rule_id")),
        display(&field(alert, "rule_name")),
        display(&field(alert, "security_severity")),
        display(&field(alert, "tool_name")),
        display(&field(alert, "tool_version")),
        display(&field(alert, "tool_guid")),
        display(&field(alert, "message")),
        display(&field(alert, "ref")),
        display(&field(alert, "commit_sha")),
        display(&field(alert, "path")),
        line_suffix(alert),
        display(&field(alert, "created_at")),
        display(&field(alert, "updated_at")),
        display(&field(alert, "html_url")),
    ));
}

fn append_shared_cause(out: &mut String, snapshot: &Value, alerts: &[Value]) {
    let lead = &alerts[0];
    let paths = distinct_paths(alerts);
    let severity = severity_name(highest_severity_rank(alerts));
    out.push_str(&format!(
        "## Shared cause\n\n- Repository: `{}`\n- Rule: `{}` ({})\n- Highest open security severity: `{severity}`\n- Tool: `{}` (version `{}`, guid `{}`)\n- Ref: `{}`\n- Message: {}\n- Grouped because: {} alerts across {} report this rule with this message from the same analysis, so the evidence puts them behind one repair. Their line and column offsets differ; the construct to repair does not.\n",
        repository_name(snapshot),
        display(&field(lead, "rule_id")),
        display(&field(lead, "rule_name")),
        display(&field(lead, "tool_name")),
        display(&field(lead, "tool_version")),
        display(&field(lead, "tool_guid")),
        display(&field(lead, "ref")),
        display(&field(lead, "message")),
        alerts.len(),
        match paths.len() {
            0 => "an unreported location".to_string(),
            1 => format!("`{}`", paths[0]),
            count => format!("{count} files"),
        },
    ));
}

fn append_alert_ledger(out: &mut String, alerts: &[Value]) {
    out.push_str("\n## Per-alert evidence ledger\n\n");
    for alert in alerts {
        out.push_str(&format!(
            "- **Alert `#{}`** — location `{}`{}; security severity `{}`; analyzed commit `{}`; created `{}`; updated `{}`; alert URL: {}\n",
            field(alert, "number"),
            display(&field(alert, "path")),
            line_suffix(alert),
            display(&field(alert, "security_severity")),
            display(&field(alert, "commit_sha")),
            display(&field(alert, "created_at")),
            display(&field(alert, "updated_at")),
            display(&field(alert, "html_url")),
        ));
    }
}

/// The reconciliation contract. A clean checkout whose repair already landed
/// is a real outcome; manufacturing a commit for it, or claiming another
/// task's commit, is not (F2026-09-097, F2026-09-101, F2026-09-105).
fn append_disposition_contract(out: &mut String) {
    out.push_str(
        "\n## Per-alert disposition\n\n\
         Record one outcome for every alert listed above before handoff:\n\n\
         - `fixed` — this task changed the source and the rule no longer reports at that location.\n\
         - `already-covered` — the repair already landed. Name the covering commit, show it is an ancestor of the HEAD you validated, and cite the current source that carries it. Do not present another task's commit as this task's own change, and do not manufacture an empty or unrelated commit to satisfy delivery.\n\
         - `not-reproducible-in-source` — the reported location does not match the current source (a stale path, line, or message). Record what that location holds today.\n\n\
         Source-side coverage and hosted alert closure are separate outcomes: do not record a hosted alert as closed without a hosted rescan.\n",
    );
}

fn append_covered_siblings(out: &mut String, covered: &[CoveredAlert]) {
    if covered.is_empty() {
        return;
    }
    out.push_str(
        "\n## Already covered elsewhere\n\n\
         Open work already owns these alerts of the same cause. Leave them to their owner: do not repeat the repair here, and do not widen that task.\n\n",
    );
    for sibling in covered {
        out.push_str(&format!(
            "- Alert `#{}` — owned by {}\n",
            sibling.number, sibling.task_id
        ));
    }
}

fn covering_relations(covered: &[CoveredAlert]) -> Vec<TaskRelation> {
    let mut targets = covered
        .iter()
        .map(|sibling| sibling.task_id.clone())
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    targets
        .into_iter()
        .map(|target| TaskRelation {
            relation_type: TaskRelationType::RelatedTo,
            target,
        })
        .collect()
}

fn severity_name(rank: u8) -> &'static str {
    match rank {
        4.. => "critical",
        3 => "high",
        2 => "moderate",
        _ => "low",
    }
}
