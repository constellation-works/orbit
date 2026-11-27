//! High-confidence coverage fingerprints and completed-repair deferral for
//! security-alert task candidates.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{CodeScanningAlertEvidence, Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    CommitObservationStatus, LandingObservationStatus, PipelineState, RunDeliveryObservation,
    SuccessfulCheckpoint,
};
use serde_json::{Value, json};

use crate::application::sweep_filing::duplicate_tasks::{
    CoverageAnchor, CoverageFingerprint, DuplicateCandidate, DuplicateTaskLookup,
};
use crate::application::sweep_filing::{bounded_error, full_revision};
use crate::application::task::EXTERNAL_MERGE_SETTLED_EVENT;

use super::code_groups::alert_key;
use super::filing::{
    CODE_KEY_PREFIX, DEPENDABOT_KEY_PREFIX, SECRET_KEY_PREFIX, alert_number, field, line_suffix,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AlertDisposition {
    Fixed,
    AlreadyCovered,
    NotReproducibleInSource,
}

impl AlertDisposition {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::AlreadyCovered => "already-covered",
            Self::NotReproducibleInSource => "not-reproducible-in-source",
        }
    }
}

/// Deferred pre-fix code-scanning alert awaiting a hosted post-fix scan.
pub(super) struct AwaitingPostFixScan {
    pub(super) key: String,
    pub(super) alert_number: u64,
    pub(super) repository: String,
    pub(super) git_ref: String,
    pub(super) analyzed_sha: String,
    pub(super) covering_task_id: String,
    pub(super) landed_sha: String,
    pub(super) disposition: AlertDisposition,
}

impl AwaitingPostFixScan {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "family": "code_scanning",
            "key": self.key,
            "alert_number": self.alert_number,
            "repository": self.repository,
            "ref": self.git_ref,
            "analyzed_sha": self.analyzed_sha,
            "covering_task_id": self.covering_task_id,
            "landed_sha": self.landed_sha,
            "disposition": self.disposition.as_str(),
            "status": "awaiting_post_fix_scan",
        })
    }
}

struct CompletedCodeTaskIdentity {
    repository: String,
    git_ref: String,
    alert_numbers: BTreeSet<u64>,
}

struct LandedDelivery {
    branch: String,
    landed_sha: String,
}

struct CompletedCodeTaskRecord {
    identity: CompletedCodeTaskIdentity,
    dispositions: BTreeMap<u64, Option<AlertDisposition>>,
    delivery: Option<LandedDelivery>,
}

pub(super) trait CompletedRepairEnv {
    fn get_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError>;

    fn find_task_delivery(
        &self,
        task_id: &str,
    ) -> Result<Option<RunDeliveryObservation>, OrbitError>;

    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError>;

    fn is_ancestor(&self, older: &str, newer: &str) -> bool;
}

/// Checks uncovered code-scanning alerts against completed repairs in one sweep,
/// memoizing parsed task records and git ancestry checks.
pub(super) struct CodeScanningDeferralChecker<'a> {
    env: &'a dyn CompletedRepairEnv,
    tasks: BTreeMap<String, Option<CompletedCodeTaskRecord>>,
    ancestry: BTreeMap<(String, String), bool>,
}

impl<'a> CodeScanningDeferralChecker<'a> {
    pub(super) fn new(env: &'a dyn CompletedRepairEnv) -> Self {
        Self {
            env,
            tasks: BTreeMap::new(),
            ancestry: BTreeMap::new(),
        }
    }

    /// Defer `alert` when exact repository/ref/alert identity matches a
    /// remediated per-alert disposition on a `Done` repair whose verified
    /// delivery landed on the same branch after the reported analysis SHA.
    pub(super) fn defer_if_awaiting_post_fix_scan<L>(
        &mut self,
        lookup: &L,
        repository: &str,
        alert: &Value,
    ) -> Result<Option<AwaitingPostFixScan>, OrbitError>
    where
        L: DuplicateTaskLookup + ?Sized,
    {
        if repository.trim().is_empty() || repository == "unknown repository" {
            return Ok(None);
        }
        let number = alert_number(alert);
        if number == 0 {
            return Ok(None);
        }
        let alert_ref = field(alert, "ref");
        let Some(alert_branch) = branch_name(&alert_ref).map(str::to_owned) else {
            return Ok(None);
        };
        let analyzed_sha = field(alert, "commit_sha");
        if !full_revision(&analyzed_sha) {
            return Ok(None);
        }

        let key = alert_key(repository, alert);
        let tag = format!("{CODE_KEY_PREFIX}{key}");
        let mut completed: Vec<Task> = lookup
            .list_tasks_by_tags(std::slice::from_ref(&tag))?
            .into_iter()
            .filter(|task| task.status == TaskStatus::Done)
            .collect();
        if completed.is_empty() {
            return Ok(None);
        }
        completed.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        });

        for task in completed {
            let Some(record) = self.completed_record(&task)? else {
                continue;
            };
            if record.identity.repository != repository
                || record.identity.git_ref != alert_ref
                || !record.identity.alert_numbers.contains(&number)
            {
                continue;
            }
            let Some(disposition) = record.dispositions.get(&number).copied().flatten() else {
                continue;
            };
            if !matches!(
                disposition,
                AlertDisposition::Fixed | AlertDisposition::AlreadyCovered
            ) {
                continue;
            }
            let Some(delivery) = &record.delivery else {
                continue;
            };
            if branch_name(&delivery.branch) != Some(alert_branch.as_str()) {
                continue;
            }
            let landed_sha = delivery.landed_sha.clone();
            if analyzed_sha == landed_sha || !self.strict_ancestor(&analyzed_sha, &landed_sha) {
                continue;
            }
            return Ok(Some(AwaitingPostFixScan {
                key,
                alert_number: number,
                repository: repository.to_string(),
                git_ref: alert_ref,
                analyzed_sha,
                covering_task_id: task.id,
                landed_sha,
                disposition,
            }));
        }
        Ok(None)
    }

    fn completed_record(
        &mut self,
        task: &Task,
    ) -> Result<Option<&CompletedCodeTaskRecord>, OrbitError> {
        if !self.tasks.contains_key(&task.id) {
            let loaded = load_completed_task_record(self.env, task)?;
            self.tasks.insert(task.id.clone(), loaded);
        }
        Ok(self.tasks.get(&task.id).and_then(Option::as_ref))
    }

    fn strict_ancestor(&mut self, older: &str, newer: &str) -> bool {
        let pair = (older.to_string(), newer.to_string());
        if let Some(&cached) = self.ancestry.get(&pair) {
            return cached;
        }
        let result = older != newer && self.env.is_ancestor(older, newer);
        self.ancestry.insert(pair, result);
        result
    }
}

pub(super) fn dependabot_duplicate_candidate(
    key: &str,
    package: &str,
    manifest_path: &str,
) -> DuplicateCandidate {
    DuplicateCandidate::new(
        format!("{DEPENDABOT_KEY_PREFIX}{key}"),
        vec![
            CoverageFingerprint::new(
                "dependency_title",
                vec![CoverageAnchor::new(
                    "dependency_and_manifest",
                    format!("update {package} in {manifest_path}"),
                )],
            ),
            CoverageFingerprint::new(
                "dependency_fields",
                vec![
                    CoverageAnchor::new("package", format!("package {package}")),
                    CoverageAnchor::new("manifest_path", format!("manifest path {manifest_path}")),
                ],
            ),
        ],
    )
}

pub(super) fn code_duplicate_candidate(key: &str, alert: &Value) -> DuplicateCandidate {
    let number = alert_number(alert);
    let rule = field(alert, "rule_id");
    let path = field(alert, "path");
    let alert_anchor = format!("alert {number}");
    // Alert number and location are one bullet in a grouped ledger. They are
    // colocated so a rejected group cannot suppress an alert whose location
    // moved onto a sibling's recorded line. The rule is one per group and is
    // read from the task as a whole. A changed rule or location stops
    // matching and the alert is filed afresh. A single-alert body has no
    // ledger, so those two anchors still match across the whole task text.
    let fields = vec![
        CoverageAnchor::new("alert_number", alert_anchor),
        CoverageAnchor::new("rule", format!("rule {rule}")),
        CoverageAnchor::new("location", format!("location {path}{}", line_suffix(alert))),
    ];
    DuplicateCandidate::new(
        format!("{CODE_KEY_PREFIX}{key}"),
        vec![
            CoverageFingerprint::new(
                "code_scanning_title_and_alert",
                vec![
                    CoverageAnchor::new("rule_and_path", format!("fix {rule} in {path}")),
                    CoverageAnchor::new("alert_number", format!("alert {number}")),
                ],
            ),
            CoverageFingerprint::new("code_scanning_fields", fields.clone()),
        ],
    )
    .with_rejected_owner_fingerprint(
        CoverageFingerprint::new("code_scanning_unchanged_after_rejection", fields)
            .with_colocated_fields(&["alert_number", "location"]),
    )
}

pub(super) fn secret_duplicate_candidate(key: &str, alert: &Value) -> DuplicateCandidate {
    let number = alert_number(alert);
    let secret_type = field(alert, "secret_type");
    let alert_anchor = format!("alert {number}");
    let mut fingerprints = vec![CoverageFingerprint::new(
        "secret_scanning_title",
        vec![CoverageAnchor::new(
            "secret_type_and_alert",
            format!("rotate exposed {secret_type} from alert {number}"),
        )],
    )];
    if let Some(location) = alert
        .get("locations")
        .and_then(Value::as_array)
        .and_then(|locations| locations.first())
    {
        fingerprints.push(CoverageFingerprint::new(
            "secret_scanning_fields",
            vec![
                CoverageAnchor::new("alert_number", alert_anchor),
                CoverageAnchor::new("secret_type", format!("secret type {secret_type}")),
                CoverageAnchor::new(
                    "location",
                    format!("path {}{}", field(location, "path"), line_suffix(location)),
                ),
            ],
        ));
    }
    DuplicateCandidate::new(format!("{SECRET_KEY_PREFIX}{key}"), fingerprints)
}

pub(super) fn duplicate_lookup_error(family: &str, key: &str, error: &OrbitError) -> OrbitError {
    OrbitError::Execution(format!(
        "dependabot_alert_sweep retryable: {}",
        json!({
            "outcome": "retryable_error",
            "stage": "dedupe_lookup",
            "errors": [{
                "stage": "registration",
                "operation": "find_covering_task",
                "family": family,
                "key": key,
                "retryable": true,
                "message": bounded_error(&error.to_string()),
            }],
        })
    ))
}

fn load_completed_task_record(
    env: &dyn CompletedRepairEnv,
    task: &Task,
) -> Result<Option<CompletedCodeTaskRecord>, OrbitError> {
    let history = env.get_task_history(&task.id)?;
    let Some(identity) = extract_task_identity(task, &history) else {
        return Ok(None);
    };
    let dispositions = parse_execution_summary_dispositions(&task.execution_summary);
    if dispositions.is_empty() {
        return Ok(None);
    }
    let delivery = resolve_authoritative_delivery(env, task, &history, &identity.repository)?;
    Ok(Some(CompletedCodeTaskRecord {
        identity,
        dispositions,
        delivery,
    }))
}

fn extract_task_identity(
    task: &Task,
    history: &[TaskHistoryEntry],
) -> Option<CompletedCodeTaskIdentity> {
    match CodeScanningAlertEvidence::from_history(history) {
        Ok(Some(evidence)) => {
            if valid_identity(&evidence.repository, &evidence.git_ref) && evidence.number > 0 {
                return Some(CompletedCodeTaskIdentity {
                    repository: evidence.repository,
                    git_ref: evidence.git_ref,
                    alert_numbers: BTreeSet::from([evidence.number]),
                });
            }
            return None;
        }
        // A mismatched creation decision must never fall back to prose.
        Err(_) => return None,
        Ok(None) => {}
    }

    if let (Some(shared), Some(ledger)) = (
        section_fields(&task.description, "Shared cause"),
        section_lines(&task.description, "Per-alert evidence ledger"),
    ) {
        let repository = shared
            .get("Repository")
            .and_then(|value| first_backtick(value))?;
        let git_ref = shared.get("Ref").and_then(|value| first_backtick(value))?;
        if !valid_identity(&repository, &git_ref) {
            return None;
        }
        let mut alert_numbers = BTreeSet::new();
        for line in ledger {
            let trimmed = line.trim();
            if let Some(bullet) = trimmed.strip_prefix("- ") {
                alert_numbers.extend(extract_hash_alert_numbers(bullet));
            }
        }
        if !alert_numbers.is_empty() {
            return Some(CompletedCodeTaskIdentity {
                repository,
                git_ref,
                alert_numbers,
            });
        }
    }

    if let Some(single) = section_fields(&task.description, "Alert evidence") {
        let repository = single
            .get("Repository")
            .and_then(|value| first_backtick(value))?;
        let git_ref = single.get("Ref").and_then(|value| first_backtick(value))?;
        let number = single
            .get("Alert")
            .and_then(|value| first_backtick(value))?
            .trim_start_matches('#')
            .parse::<u64>()
            .ok()
            .filter(|&number| number > 0)?;
        if valid_identity(&repository, &git_ref) {
            return Some(CompletedCodeTaskIdentity {
                repository,
                git_ref,
                alert_numbers: BTreeSet::from([number]),
            });
        }
    }

    None
}

fn valid_identity(repository: &str, git_ref: &str) -> bool {
    !repository.is_empty()
        && repository != "unknown repository"
        && repository != "unknown"
        && !git_ref.is_empty()
        && git_ref != "unknown"
}

fn section_lines<'a>(text: &'a str, heading_name: &str) -> Option<Vec<&'a str>> {
    let mut lines = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            if lines.is_some() {
                break;
            }
            if heading.trim() == heading_name {
                lines = Some(Vec::new());
            }
            continue;
        }
        if let Some(collected) = lines.as_mut() {
            collected.push(line);
        }
    }
    lines
}

fn section_fields(text: &str, heading_name: &str) -> Option<BTreeMap<String, String>> {
    let lines = section_lines(text, heading_name)?;
    let mut fields = BTreeMap::new();
    for line in lines {
        let trimmed = line.trim();
        if let Some(entry) = trimmed.strip_prefix("- ")
            && let Some((key, value)) = entry.split_once(": ")
        {
            fields
                .entry(key.trim().to_string())
                .or_insert_with(|| value.trim().to_string());
        }
    }
    Some(fields)
}

fn first_backtick(value: &str) -> Option<String> {
    value.split('`').nth(1).map(str::to_string)
}

fn parse_execution_summary_dispositions(summary: &str) -> BTreeMap<u64, Option<AlertDisposition>> {
    let mut dispositions: BTreeMap<u64, Option<AlertDisposition>> = BTreeMap::new();
    let mut in_section = false;

    for line in summary.lines() {
        let trimmed = line.trim();
        if is_markdown_heading(trimmed) {
            in_section = heading_names_disposition(trimmed);
            continue;
        }
        if is_label_header(trimmed) {
            in_section = heading_names_disposition(trimmed);
            continue;
        }
        if !in_section {
            continue;
        }
        let Some(bullet) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        else {
            continue;
        };
        let Some((subject, verdict_part)) = split_disposition_bullet(bullet) else {
            continue;
        };
        let numbers = extract_hash_alert_numbers(subject);
        if numbers.is_empty() {
            continue;
        }
        let parsed = parse_disposition_token(verdict_part);
        for number in numbers {
            dispositions
                .entry(number)
                .and_modify(|existing| {
                    if *existing != parsed {
                        *existing = None;
                    }
                })
                .or_insert(parsed);
        }
    }
    dispositions
}

fn is_markdown_heading(line: &str) -> bool {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    hashes >= 1 && line[hashes..].starts_with(' ')
}

fn is_label_header(line: &str) -> bool {
    if line.is_empty() || line.starts_with("- ") || line.starts_with("* ") {
        return false;
    }
    let clean = line.trim_matches('*').trim();
    clean.ends_with(':')
}

fn heading_names_disposition(line: &str) -> bool {
    line.to_ascii_lowercase().contains("disposition")
}

/// Split a disposition bullet at the first `:` or ` — ` / ` – ` outside
/// backticks and parentheses, so `commit.rs:216` inside `(\`commit.rs:216\`)`
/// does not split the subject prematurely.
fn split_disposition_bullet(bullet: &str) -> Option<(&str, &str)> {
    let mut in_backticks = false;
    let mut paren_depth = 0usize;
    let mut dash_split = None;
    for (idx, ch) in bullet.char_indices() {
        if ch == '`' {
            in_backticks = !in_backticks;
            continue;
        }
        if in_backticks {
            continue;
        }
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            ':' if paren_depth == 0 => {
                return Some((&bullet[..idx], &bullet[idx + ch.len_utf8()..]));
            }
            '—' | '–' if paren_depth == 0 && dash_split.is_none() => {
                let before = &bullet[..idx];
                let after = &bullet[idx + ch.len_utf8()..];
                if before.ends_with(' ') && after.starts_with(' ') {
                    dash_split = Some((before, after));
                }
            }
            _ => {}
        }
    }
    dash_split
}

fn extract_hash_alert_numbers(text: &str) -> Vec<u64> {
    let mut numbers = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'#' {
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > start
                && let Ok(number) = text[start..end].parse::<u64>()
                && number > 0
            {
                numbers.push(number);
            }
            index = end;
        } else {
            index += 1;
        }
    }
    numbers
}

fn parse_disposition_token(verdict_part: &str) -> Option<AlertDisposition> {
    let trimmed = verdict_part.trim().trim_start_matches('`');
    let token = trimmed
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-'))
        .next()
        .unwrap_or("");
    match token {
        "fixed" => Some(AlertDisposition::Fixed),
        "already-covered" => Some(AlertDisposition::AlreadyCovered),
        "not-reproducible-in-source" => Some(AlertDisposition::NotReproducibleInSource),
        _ => None,
    }
}

fn resolve_authoritative_delivery(
    env: &dyn CompletedRepairEnv,
    task: &Task,
    history: &[TaskHistoryEntry],
    expected_repository: &str,
) -> Result<Option<LandedDelivery>, OrbitError> {
    let episode_start = history
        .iter()
        .rposition(|entry| entry.from_status == Some(TaskStatus::Done))
        .map_or(0, |index| index + 1);
    let reopened_at: Option<DateTime<Utc>> =
        episode_start.checked_sub(1).map(|idx| history[idx].at);

    let mut deliveries: Vec<LandedDelivery> = Vec::new();
    let mut contradictory = false;

    if let Some(obs) = env.find_task_delivery(&task.id)? {
        let same_episode = match reopened_at {
            None => true,
            Some(reopened) => obs
                .landing
                .observed_at
                .or(obs.run_finished_at)
                .is_some_and(|at| at >= reopened),
        };
        if same_episode && obs.landing.status == LandingObservationStatus::Merged {
            let valid_commit = !obs.commit.status.is_no_change()
                && obs.commit.status != CommitObservationStatus::Unavailable;
            let repo_matches = !obs.repository.as_deref().is_some_and(|repo| {
                (!repo.starts_with("git:") || expected_repository.starts_with("git:"))
                    && repo != expected_repository
            });
            if valid_commit
                && repo_matches
                && let Some(delivery) = run_delivery_from_observation(env, &obs)?
            {
                deliveries.push(delivery);
            } else {
                contradictory = true;
            }
        }
    }

    for entry in &history[episode_start..] {
        if entry.event == EXTERNAL_MERGE_SETTLED_EVENT && entry.to_status == Some(TaskStatus::Done)
        {
            match entry.note.as_deref().and_then(parse_external_merge_note) {
                Some((branch, landed_sha)) => {
                    deliveries.push(LandedDelivery { branch, landed_sha })
                }
                None => contradictory = true,
            }
        } else if entry.event == "landing_completed" {
            let Some(note) = entry.note.as_deref() else {
                contradictory = true;
                continue;
            };
            if is_non_landing_completed_note(note) {
                continue;
            }
            match parse_landing_completed_note(note) {
                Some((branch, landed_sha)) => {
                    deliveries.push(LandedDelivery { branch, landed_sha })
                }
                None => contradictory = true,
            }
        }
    }

    if contradictory || deliveries.is_empty() {
        return Ok(None);
    }

    let first = &deliveries[0];
    let Some(first_branch) = branch_name(&first.branch) else {
        return Ok(None);
    };
    if !deliveries.iter().all(|other| {
        branch_name(&other.branch) == Some(first_branch) && other.landed_sha == first.landed_sha
    }) {
        return Ok(None);
    }

    Ok(Some(LandedDelivery {
        branch: first.branch.clone(),
        landed_sha: first.landed_sha.clone(),
    }))
}

fn run_delivery_from_observation(
    env: &dyn CompletedRepairEnv,
    obs: &orbit_types::workflow::RunDeliveryObservation,
) -> Result<Option<LandedDelivery>, OrbitError> {
    let Some(landed_sha) = obs
        .landing
        .landed_commit
        .as_deref()
        .filter(|sha| full_revision(sha))
    else {
        return Ok(None);
    };
    let Some(provenance) = obs.landing.provenance.as_ref() else {
        return Ok(None);
    };
    let Some(state) = env.read_run_state(&obs.run_id)? else {
        return Ok(None);
    };
    let SuccessfulCheckpoint::Recorded(output) =
        state.successful_checkpoint(provenance.step_index, &provenance.step_id)
    else {
        return Ok(None);
    };

    let branch = match provenance.activity.as_str() {
        "pr_complete" => {
            if let Some(evidence_commit) = output
                .pointer("/delivery_evidence/merge_commit")
                .and_then(Value::as_str)
                && evidence_commit != landed_sha
            {
                return Ok(None);
            }
            output
                .pointer("/delivery_evidence/base_ref")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|branch| !branch.is_empty())
        }
        "git_merge" => {
            if let Some(checkpoint_commit) = output.get("landed_commit").and_then(Value::as_str)
                && checkpoint_commit != landed_sha
            {
                return Ok(None);
            }
            output
                .get("base")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|branch| !branch.is_empty())
        }
        _ => None,
    };

    Ok(branch.map(|branch| LandedDelivery {
        branch: branch.to_string(),
        landed_sha: landed_sha.to_string(),
    }))
}

fn parse_external_merge_note(note: &str) -> Option<(String, String)> {
    let landed = note.strip_prefix("settled from pull request #")?;
    let (_, merged) = landed.split_once(", merged into ")?;
    let (merged, has_at) = if let Some(rest) = merged.strip_suffix(" outside Orbit") {
        (rest, true)
    } else {
        (merged.strip_suffix(" by owner decision")?, false)
    };
    let (branch_part, commit) = merged.rsplit_once(" as ")?;
    let commit = commit.trim();
    if !full_revision(commit) {
        return None;
    }
    let branch = if has_at {
        let (branch, timestamp) = branch_part.rsplit_once(" at ")?;
        if timestamp.trim().is_empty() {
            return None;
        }
        branch.trim()
    } else {
        branch_part.trim()
    };
    if branch.is_empty() {
        return None;
    }
    Some((branch.to_string(), commit.to_string()))
}

fn is_non_landing_completed_note(note: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(note) else {
        return false;
    };
    matches!(
        value.get("delivery").and_then(Value::as_str),
        Some("no_diff" | "already_landed")
    )
}

fn parse_landing_completed_note(note: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(note).ok()?;
    if let Some(commit) = value
        .pointer("/delivery/merge_commit")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|commit| full_revision(commit))
    {
        let branch = value
            .pointer("/delivery/base_ref")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|branch| !branch.is_empty())?;
        return Some((branch.to_string(), commit.to_string()));
    }
    if value["delivery"] == "local_candidate"
        || value["reconciled"] == "local_target_contains_candidate"
    {
        let commit = value["candidate"]
            .as_str()
            .map(str::trim)
            .filter(|commit| full_revision(commit))?;
        let branch = value["landing_ref"]
            .as_str()
            .map(str::trim)
            .filter(|branch| !branch.is_empty())?;
        return Some((branch.to_string(), commit.to_string()));
    }
    None
}

fn branch_name(reference: &str) -> Option<&str> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(branch) = trimmed.strip_prefix("refs/heads/") {
        return (!branch.is_empty()).then_some(branch);
    }
    if let Some(branch) = trimmed.strip_prefix("refs/remotes/origin/") {
        return (!branch.is_empty()).then_some(branch);
    }
    if trimmed.starts_with("refs/") {
        return None;
    }
    let bare = trimmed.strip_prefix("origin/").unwrap_or(trimmed);
    (!bare.is_empty()).then_some(bare)
}
