//! Unchanged-head CI retries, reserved before external mutation.
use crate::task::TaskHistoryEntry;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Durable reservation, including unknown provider outcomes.
pub const CI_RERUN_RESERVED_EVENT: &str = "ci_check_rerun_reserved";

/// The independently classified root of a workflow-wide failed-jobs retry.
/// `classification` is `transient`, `flaky` or `unattributed`: a failure
/// whose cause no evidence established yet, rerun once per head so its
/// [`CiFailureClass`](super::CiFailureClass) can be told apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CiRerunRoot {
    pub job_id: u64,
    pub check: String,
    pub classification: String,
    pub signature: String,
}

/// Complete, provider-authenticated action identity and original observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CiRerunPlan {
    pub schema_version: u32,
    pub repository: String,
    pub repository_id: u64,
    pub pr_number: String,
    pub head_sha: String,
    pub head_branch: String,
    pub base_branch: String,
    pub accepted_base: String,
    pub run_id: u64,
    pub run_attempt: u64,
    pub workflow_id: u64,
    pub roots: Vec<CiRerunRoot>,
    /// Exact failed jobs, including authenticated dependency-only waiters.
    pub failed_jobs: Vec<Value>,
    /// Source blobs, policy, checks, run and complete listing used for admission.
    pub authentication: Value,
}

/// A durable hold before another infrastructure retry is admitted.
pub const CI_INFRASTRUCTURE_RETRY_BACKOFF_EVENT: &str = "ci_infrastructure_retry_backoff";

/// A backoff field observed in a legacy note. Unreadable values include
/// explicit `null`; neither they nor missing fields acquire invented defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CiInfrastructureRetryBackoffField<T> {
    Missing,
    Known(T),
    Unreadable(Value),
}

impl<T> CiInfrastructureRetryBackoffField<T> {
    /// The typed observation, if the field was present and readable.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Missing | Self::Unreadable(_) => None,
        }
    }

    fn is_missing(&self) -> bool {
        match self {
            Self::Missing => true,
            Self::Known(_) | Self::Unreadable(_) => false,
        }
    }
}

/// The only classification written for an infrastructure backoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CiInfrastructureRetryBackoffClass {
    #[serde(rename = "transient")]
    Transient,
}

/// The `ci_infrastructure_retry_backoff` history note. Fields serialize in
/// the producer's historical insertion order, preserving exact-note deduplication.
///
/// [`Self::from_history_note`] parses JSON once without requiring today's
/// complete producer shape. Missing and unreadable fields remain explicit:
/// readers check head/PR identity before requiring a readable deadline, and
/// ignore run/attempt, classification and reason when deciding the hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CiInfrastructureRetryBackoffRecord {
    #[serde(skip_serializing_if = "CiInfrastructureRetryBackoffField::is_missing")]
    pub head: CiInfrastructureRetryBackoffField<String>,
    /// The PR identity is stored as text, not as a JSON number.
    #[serde(skip_serializing_if = "CiInfrastructureRetryBackoffField::is_missing")]
    pub pr_number: CiInfrastructureRetryBackoffField<String>,
    #[serde(skip_serializing_if = "CiInfrastructureRetryBackoffField::is_missing")]
    pub run_id: CiInfrastructureRetryBackoffField<u64>,
    #[serde(skip_serializing_if = "CiInfrastructureRetryBackoffField::is_missing")]
    pub run_attempt: CiInfrastructureRetryBackoffField<u64>,
    #[serde(skip_serializing_if = "CiInfrastructureRetryBackoffField::is_missing")]
    pub classification: CiInfrastructureRetryBackoffField<CiInfrastructureRetryBackoffClass>,
    #[serde(skip_serializing_if = "CiInfrastructureRetryBackoffField::is_missing")]
    pub not_before_ms: CiInfrastructureRetryBackoffField<u64>,
    #[serde(skip_serializing_if = "CiInfrastructureRetryBackoffField::is_missing")]
    pub reason: CiInfrastructureRetryBackoffField<String>,
}

impl CiInfrastructureRetryBackoffRecord {
    /// Construct the complete producer shape with its fixed classification.
    pub fn from_plan(plan: &CiRerunPlan, not_before_ms: u64, reason: String) -> Self {
        use CiInfrastructureRetryBackoffField::Known;
        Self {
            head: Known(plan.head_sha.clone()),
            pr_number: Known(plan.pr_number.clone()),
            run_id: Known(plan.run_id),
            run_attempt: Known(plan.run_attempt),
            classification: Known(CiInfrastructureRetryBackoffClass::Transient),
            not_before_ms: Known(not_before_ms),
            reason: Known(reason),
        }
    }

    /// Decode one history note, retaining the original JSON syntax error
    /// even when the note could not have matched the requested head or PR.
    /// A missing note has the same parse error as empty text.
    pub fn from_history_note(note: Option<&str>) -> Result<Self, serde_json::Error> {
        let note = serde_json::from_str::<Value>(note.unwrap_or_default())?;
        Ok(Self {
            head: ci_retry_backoff_field(&note, "head", json_text),
            pr_number: ci_retry_backoff_field(&note, "pr_number", json_text),
            run_id: ci_retry_backoff_field(&note, "run_id", Value::as_u64),
            run_attempt: ci_retry_backoff_field(&note, "run_attempt", Value::as_u64),
            classification: ci_retry_backoff_field(&note, "classification", |value| {
                (value.as_str() == Some("transient"))
                    .then_some(CiInfrastructureRetryBackoffClass::Transient)
            }),
            not_before_ms: ci_retry_backoff_field(&note, "not_before_ms", Value::as_u64),
            reason: ci_retry_backoff_field(&note, "reason", json_text),
        })
    }

    /// Encode the history note in the unchanged seven-field producer order.
    /// Legacy missing fields stay absent; unreadable fields retain their value.
    pub fn to_history_note(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

fn ci_retry_backoff_field<T>(
    note: &Value,
    key: &str,
    parse: fn(&Value) -> Option<T>,
) -> CiInfrastructureRetryBackoffField<T> {
    match note.as_object().and_then(|object| object.get(key)) {
        None => CiInfrastructureRetryBackoffField::Missing,
        Some(value) => match parse(value) {
            Some(parsed) => CiInfrastructureRetryBackoffField::Known(parsed),
            None => CiInfrastructureRetryBackoffField::Unreadable(value.clone()),
        },
    }
}

/// A failed-jobs retry that was sent: the durable `ci_check_rerun` receipt.
pub const CI_CHECK_RERUN_EVENT: &str = "ci_check_rerun";

/// The provider event a sent retry reran. Receipts written before the event
/// was recorded omit it ([`CiCheckRerunRecord::event`] is `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiCheckRerunEvent {
    PullRequest,
    Other(String),
}

impl CiCheckRerunEvent {
    fn from_token(token: &str) -> Self {
        match token {
            "pull_request" => Self::PullRequest,
            other => Self::Other(other.to_string()),
        }
    }

    fn as_token(&self) -> &str {
        match self {
            Self::PullRequest => "pull_request",
            Self::Other(other) => other,
        }
    }
}

impl Serialize for CiCheckRerunEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_token())
    }
}

/// One rerun root in a [`CiCheckRerunRecord`]. Every field is optional on
/// read: older receipts omit the attempt and job timing, and a malformed
/// value reads as absent rather than failing the whole receipt. The
/// producer always writes every key; absent timing is written as `null`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct CiCheckRerunJob {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_attempt: Option<u64>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

impl CiCheckRerunJob {
    fn from_legacy(job: &Value) -> Self {
        Self {
            job_id: job["job_id"].as_u64(),
            check: json_text(&job["check"]),
            classification: json_text(&job["classification"]),
            signature: json_text(&job["signature"]),
            run_attempt: job["run_attempt"].as_u64(),
            started_at: json_text(&job["started_at"]),
            completed_at: json_text(&job["completed_at"]),
        }
    }

    /// When the rerun's original attempt completed, if recorded as RFC 3339.
    pub fn completed_time(&self) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        self.completed_at
            .as_deref()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
    }
}

/// The `ci_check_rerun` history note: the head, PR and provider run a sent
/// retry targeted, with the roots it reran.
///
/// [`Self::from_history_note`] is the one compatibility decoder. It keeps
/// the identity optional so a reader can tell a receipt that omits or
/// garbles its head or run from a valid one, reads older PR-number aliases
/// into [`Self::legacy_pull_requests`], and never requires today's complete
/// producer shape. Serialization writes the producer shape unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct CiCheckRerunRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<CiCheckRerunEvent>,
    pub jobs: Vec<CiCheckRerunJob>,
    /// PR numbers an older receipt named under another key or shape. Read
    /// only; never written.
    #[serde(skip)]
    pub legacy_pull_requests: Vec<u64>,
}

impl CiCheckRerunRecord {
    /// Decode a history note. `None` when the note is missing or is not JSON
    /// (an unreadable receipt); fields that are absent or of the wrong shape
    /// read as `None`.
    pub fn from_history_note(note: Option<&str>) -> Option<Self> {
        let note = serde_json::from_str::<Value>(note?).ok()?;
        let number = |value: &Value| {
            value
                .as_u64()
                .or_else(|| value["number"].as_u64())
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        };
        let mut legacy_pull_requests = Vec::new();
        for key in ["pull_request_numbers", "pull_requests"] {
            if let Some(list) = note[key].as_array() {
                legacy_pull_requests.extend(list.iter().filter_map(number));
            }
        }
        for key in ["pull_request_number", "pr"] {
            legacy_pull_requests.extend(number(&note[key]));
        }
        Some(Self {
            head: json_text(&note["head"]),
            pr_number: number(&note["pr_number"]),
            run_id: note["run_id"].as_u64(),
            event: note["event"].as_str().map(CiCheckRerunEvent::from_token),
            jobs: note["jobs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(CiCheckRerunJob::from_legacy)
                .collect(),
            legacy_pull_requests,
        })
    }

    /// The history note the producer writes.
    pub fn to_history_note(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Every PR number the receipt names, current key and legacy aliases.
    pub fn pull_request_numbers(&self) -> impl Iterator<Item = u64> + '_ {
        self.pr_number
            .into_iter()
            .chain(self.legacy_pull_requests.iter().copied())
    }

    /// The action identity: a full 40-hex head and a provider run id.
    /// `None` when the receipt omitted or malformed either.
    pub fn action_identity(&self) -> Option<(&str, u64)> {
        let head = self.head.as_deref()?;
        let valid = head.len() == 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit());
        valid.then_some((head, self.run_id?))
    }

    /// Whether this receipt reran a `pull_request` run. A receipt that
    /// predates the recorded event counts as one.
    pub fn reran_pull_request(&self) -> bool {
        self.event
            .as_ref()
            .is_none_or(|event| *event == CiCheckRerunEvent::PullRequest)
    }
}

fn json_text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

/// Evidence that a failure may be inherited from the base: the durable
/// `ci_base_suspect` history note.
pub const CI_BASE_SUSPECT_EVENT: &str = "ci_base_suspect";

/// One legacy field of a [`CiBaseSuspectRecord`], keeping a key that was
/// absent, an explicit `null`, a readable value and an unreadable value
/// apart. The producer shape writes every key, so `Missing` and `Null`
/// both serialize as `null` (the shape the job-evidence observations the
/// producers project already wrote); an unreadable value round-trips its
/// original bytes instead of being silently defaulted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CiBaseSuspectLegacy<T> {
    /// The note omitted the key, or the job carried no such field.
    #[default]
    Missing,
    /// The note or the job carried an explicit `null`.
    Null,
    /// A readable value.
    Known(T),
    /// A present value of the wrong shape, preserved verbatim.
    Unreadable(Value),
}

impl<T> CiBaseSuspectLegacy<T> {
    /// The readable value, when there is one.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            _ => None,
        }
    }
}

impl<T: Serialize> Serialize for CiBaseSuspectLegacy<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Missing | Self::Null => serializer.serialize_none(),
            Self::Known(value) => value.serialize(serializer),
            Self::Unreadable(value) => value.serialize(serializer),
        }
    }
}

fn ci_base_suspect_legacy_field<T>(
    note: &Value,
    key: &str,
    parse: fn(&Value) -> Option<T>,
) -> CiBaseSuspectLegacy<T> {
    match note.as_object().and_then(|object| object.get(key)) {
        None => CiBaseSuspectLegacy::Missing,
        Some(Value::Null) => CiBaseSuspectLegacy::Null,
        Some(value) => match parse(value) {
            Some(parsed) => CiBaseSuspectLegacy::Known(parsed),
            None => CiBaseSuspectLegacy::Unreadable(value.clone()),
        },
    }
}

/// The deduplication identity of a [`CiBaseSuspectRecord`]: the exact
/// failure a task records base suspicion for at most once, even when the
/// base advances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CiBaseSuspectIdentity<'a> {
    pub head: &'a str,
    pub check: &'a str,
    pub signature: &'a str,
}

/// The `ci_base_suspect` history note: the base and head, the failed
/// check and signature, and the provider run, job, attempt, PR, event and
/// timing the suspicion was observed on, with its reason.
///
/// [`Self::from_history_note`] is the one compatibility decoder. It never
/// requires today's complete producer shape: absent or malformed text
/// reads as `None`, unreadable legacy fields stay
/// [`CiBaseSuspectLegacy::Unreadable`], and PR numbers an older note named
/// under an alias key or shape are read into
/// [`Self::legacy_pull_requests`]. Serialization writes the producer shape
/// unchanged, every key in its historical order.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct CiBaseSuspectRecord {
    pub base: Option<String>,
    pub head: Option<String>,
    pub check: Option<String>,
    pub signature: Option<String>,
    pub run_id: CiBaseSuspectLegacy<u64>,
    pub job_id: CiBaseSuspectLegacy<u64>,
    pub run_attempt: CiBaseSuspectLegacy<u64>,
    pub pr_number: Option<u64>,
    pub event: CiBaseSuspectLegacy<String>,
    pub started_at: CiBaseSuspectLegacy<String>,
    pub completed_at: CiBaseSuspectLegacy<String>,
    pub reason: Option<String>,
    /// PR numbers an older note named under another key or shape. Read
    /// only; never written.
    #[serde(skip)]
    pub legacy_pull_requests: Vec<u64>,
}

impl CiBaseSuspectRecord {
    /// Decode a history note. `None` when the note is missing or is not
    /// JSON (an unreadable record); fields that are absent or of the wrong
    /// shape read as missing.
    pub fn from_history_note(note: Option<&str>) -> Option<Self> {
        let note = serde_json::from_str::<Value>(note?).ok()?;
        let number = |value: &Value| {
            value
                .as_u64()
                .or_else(|| value["number"].as_u64())
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        };
        let mut legacy_pull_requests = Vec::new();
        for key in ["pull_request_numbers", "pull_requests"] {
            if let Some(list) = note[key].as_array() {
                legacy_pull_requests.extend(list.iter().filter_map(number));
            }
        }
        for key in ["pull_request_number", "pr"] {
            legacy_pull_requests.extend(number(&note[key]));
        }
        let legacy_number = |key: &str| ci_base_suspect_legacy_field(&note, key, Value::as_u64);
        let legacy_text = |key: &str| ci_base_suspect_legacy_field(&note, key, json_text);
        Some(Self {
            base: json_text(&note["base"]),
            head: json_text(&note["head"]),
            check: json_text(&note["check"]),
            signature: json_text(&note["signature"]),
            run_id: legacy_number("run_id"),
            job_id: legacy_number("job_id"),
            run_attempt: legacy_number("run_attempt"),
            pr_number: number(&note["pr_number"]),
            event: legacy_text("event"),
            started_at: legacy_text("started_at"),
            completed_at: legacy_text("completed_at"),
            reason: json_text(&note["reason"]),
            legacy_pull_requests,
        })
    }

    /// The history note the producer writes.
    pub fn to_history_note(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// The deduplication identity, when head, check and signature are all
    /// recorded text. The base is deliberately absent: an advanced base
    /// does not create a new key.
    pub fn dedupe_identity(&self) -> Option<CiBaseSuspectIdentity<'_>> {
        Some(CiBaseSuspectIdentity {
            head: self.head.as_deref()?,
            check: self.check.as_deref()?,
            signature: self.signature.as_deref()?,
        })
    }

    /// Whether `prior` already records this record's exact failure. A
    /// record without a complete identity duplicates nothing.
    pub fn same_dedupe_identity(&self, prior: &Self) -> bool {
        self.dedupe_identity()
            .is_some_and(|identity| prior.dedupe_identity() == Some(identity))
    }

    /// Every PR number the note names, current key and legacy aliases.
    pub fn pull_request_numbers(&self) -> impl Iterator<Item = u64> + '_ {
        self.pr_number
            .into_iter()
            .chain(self.legacy_pull_requests.iter().copied())
    }

    /// When the suspicious attempt completed, if recorded as RFC 3339. A
    /// recorded timestamp that does not parse reads as no completion.
    pub fn completed_time(&self) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        self.completed_at
            .value()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
    }
}

/// Owner-configured budget for logged infrastructure/transient failures.
/// Conclusion-only cancellations/timeouts, a failure whose log is only a
/// `ci-await` timeout, and flaky failures retain one rerun.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CiInfrastructureRetryPolicy {
    pub max_attempts: u32,
    pub backoff_seconds: u64,
}

impl Default for CiInfrastructureRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff_seconds: 60,
        }
    }
}

/// Normalized prefix of a failure log that is only a `ci-await` timeout.
/// The adapter still authenticates that log line; it is not a `concluded `
/// signature, and it stays on the one shared rerun.
const CI_AWAIT_TIMEOUT_SIGNATURE_PREFIX: &str = "ci-await: timed out";

/// A logged transient set can use the infrastructure budget. A candidate or
/// flaky sibling, a conclusion-only stop, or a pure `ci-await` timeout cannot
/// expand its authorization.
pub fn ci_rerun_is_infrastructure(plan: &CiRerunPlan) -> bool {
    !plan.roots.is_empty()
        && plan.roots.iter().all(|root| {
            root.classification == "transient" && !signature_keeps_single_rerun(&root.signature)
        })
}

/// A provider conclusion, or a log whose only signal is a `ci-await` timeout.
fn signature_keeps_single_rerun(signature: &str) -> bool {
    signature.starts_with("concluded ") || signature.starts_with(CI_AWAIT_TIMEOUT_SIGNATURE_PREFIX)
}

/// Signature prefix of a root whose evidence is the provider's failed-step
/// name, not its log: the job stopped in a step the workspace declares as a
/// dependency handoff (`workflow.ci_infrastructure_retry.handoff_steps`).
/// It is a logged-transient root for the infrastructure budget.
pub const CI_HANDOFF_STEP_SIGNATURE_PREFIX: &str = "handoff step: ";

/// The rerun-root signature of a job that failed in handoff step `step`.
pub fn ci_handoff_step_signature(step: &str) -> String {
    format!("{CI_HANDOFF_STEP_SIGNATURE_PREFIX}{step}")
}

/// Whether failed step `step` is a declared dependency handoff. Each pattern
/// matches the whole step name, case-sensitively; `*` matches any run of
/// characters. No pattern matches an empty step name.
pub fn ci_handoff_step_matches(patterns: &[String], step: &str) -> bool {
    !step.trim().is_empty()
        && patterns
            .iter()
            .any(|pattern| wildcard_matches(pattern.as_bytes(), step.as_bytes()))
}

/// Whole-string match where `*` matches any (possibly empty) byte run.
fn wildcard_matches(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(byte) if *byte == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|byte| *byte == b'*')
}

/// Admission is atomic under the owning task/claim boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiRerunAdmission {
    Reserved,
    /// A durable prior reservation imposes this absolute not-before.
    Backoff {
        not_before_ms: u64,
    },
    /// No source repair is authorized by exhaustion of infrastructure retries.
    InfrastructureExhausted {
        attempts: u32,
    },
    Observe(Box<CiRerunPlan>),
    Refused(String),
}

/// Legacy one-rerun admission for consumers without infrastructure policy.
pub fn decide_ci_rerun(history: &[TaskHistoryEntry], plan: &CiRerunPlan) -> CiRerunAdmission {
    decide_ci_rerun_with_policy(
        history,
        plan,
        CiInfrastructureRetryPolicy {
            max_attempts: 1,
            backoff_seconds: 0,
        },
        u64::MAX,
    )
}

/// Decide under the owning task/claim lock; unknown sends consume a reservation.
/// Only a newly authenticated provider attempt can spend the next reservation.
pub fn decide_ci_rerun_with_policy(
    history: &[TaskHistoryEntry],
    plan: &CiRerunPlan,
    policy: CiInfrastructureRetryPolicy,
    now_ms: u64,
) -> CiRerunAdmission {
    if plan.schema_version != 1
        || plan.roots.is_empty()
        || plan.failed_jobs.is_empty()
        || plan.roots.iter().any(|root| {
            !matches!(
                root.classification.as_str(),
                "transient" | "flaky" | "unattributed"
            )
        })
    {
        return CiRerunAdmission::Refused("incomplete CI retry authorization".into());
    }
    let mut reserved: Vec<(CiRerunPlan, u64)> = Vec::new();
    let mut legacy_spent = false;
    for entry in history {
        if entry.event == CI_RERUN_RESERVED_EVENT {
            let prior = entry
                .note
                .as_deref()
                .and_then(|note| serde_json::from_str::<CiRerunPlan>(note).ok());
            let Some(prior) = prior else {
                return CiRerunAdmission::Refused("unreadable CI retry reservation".into());
            };
            if prior.schema_version != 1 {
                return CiRerunAdmission::Refused(
                    "unsupported CI retry reservation version".into(),
                );
            }
            if prior.repository_id == plan.repository_id
                && prior.pr_number == plan.pr_number
                && prior.head_sha == plan.head_sha
            {
                if let Some((existing, _)) = reserved.iter().find(|(existing, _)| {
                    existing.run_id == prior.run_id && existing.run_attempt == prior.run_attempt
                }) {
                    if existing != &prior {
                        return CiRerunAdmission::Refused(
                            "conflicting CI retry reservations for this attempt".into(),
                        );
                    }
                } else {
                    reserved.push((
                        prior,
                        u64::try_from(entry.at.timestamp_millis()).unwrap_or(0),
                    ));
                }
            }
        } else if entry.event == CI_CHECK_RERUN_EVENT {
            let Some(prior) = CiCheckRerunRecord::from_history_note(entry.note.as_deref()) else {
                return CiRerunAdmission::Refused("unreadable legacy CI retry history".into());
            };
            let Some((head, _)) = prior.action_identity() else {
                return CiRerunAdmission::Refused(
                    "legacy CI retry history omitted its action identity".into(),
                );
            };
            if head == plan.head_sha {
                legacy_spent = true;
            }
        }
    }
    if let Some((prior, _)) = reserved
        .iter()
        .find(|(prior, _)| prior.run_id == plan.run_id && plan.run_attempt <= prior.run_attempt)
    {
        return CiRerunAdmission::Observe(Box::new(prior.clone()));
    }
    if let Some((prior, _)) = reserved.last() {
        if !ci_rerun_is_infrastructure(plan) {
            // Flaky failures, conclusion-only stops and a pure ci-await timeout
            // retain the shared one-rerun contract: observe its reservation
            // even on a later attempt.
            return CiRerunAdmission::Observe(Box::new(prior.clone()));
        }
        let attempts = u32::try_from(reserved.len()).unwrap_or(u32::MAX);
        if attempts >= policy.max_attempts {
            return CiRerunAdmission::InfrastructureExhausted { attempts };
        }
        let last_ms = reserved.iter().map(|(_, at)| *at).max().unwrap_or(0);
        let delay_ms = policy
            .backoff_seconds
            .saturating_mul(1000)
            .saturating_mul(1_u64 << attempts.saturating_sub(1).min(20));
        let not_before_ms = last_ms.saturating_add(delay_ms);
        if now_ms < not_before_ms {
            return CiRerunAdmission::Backoff { not_before_ms };
        }
    } else if ci_rerun_is_infrastructure(plan) && policy.max_attempts == 0 {
        return CiRerunAdmission::InfrastructureExhausted { attempts: 0 };
    }
    if legacy_spent && reserved.is_empty() {
        return CiRerunAdmission::Refused(
            "the shared unchanged-head CI retry budget is spent".into(),
        );
    }
    CiRerunAdmission::Reserved
}
