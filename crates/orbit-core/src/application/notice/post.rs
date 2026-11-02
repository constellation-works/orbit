//! `orbit.notice.post`: validate the request, then decide created, updated
//! or corroborated inside the store's posting transaction, where the
//! one-open-notice-per-key rule, the open counts and the post rates are
//! checked against the same snapshot the write applies to.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_config::NoticeSettings;
use orbit_store::contracts::{
    NoticeEventRow, NoticePostPlan, NoticePostQuery, NoticePostSnapshot, NoticeRow,
    NoticeStoreBackend as _,
};
use orbit_types::record::{
    AuthorClass, EffectiveStatus, Notice, NoticeEvidence, NoticeKind, NoticeLinkedTask,
    NoticeReports, NoticeScope, NoticeWorkspace, PostOutcome,
};

use super::authority::{NoticeCaller, NoticePoster};
use super::{LinkedDone, NoticeAudit, notice_row, parse_notice_row};
use crate::OrbitRuntime;

/// The shortest expiry a post may set.
const MIN_EXPIRY_MINUTES: i64 = 5;
/// The most hosts a maintenance notice may name.
const MAX_HOSTS: usize = 32;
/// The longest dedupe key.
const MAX_DEDUPE_KEY_CHARS: usize = 128;
/// The longest derived dedupe key.
const DERIVED_DEDUPE_KEY_CHARS: usize = 120;
/// The longest evidence value.
const MAX_EVIDENCE_VALUE_CHARS: usize = 256;

/// One post request, as the tool or CLI collected it.
#[derive(Debug, Clone)]
pub struct NoticePostRequest {
    pub kind: NoticeKind,
    pub title: String,
    pub body: Option<String>,
    /// `None` keeps a matched notice's scope, or `workspace` for a new one.
    pub scope: Option<NoticeScope>,
    pub dedupe_key: Option<String>,
    pub linked_task: Option<String>,
    pub close_on_linked_done: Option<bool>,
    pub evidence: Vec<NoticeEvidence>,
    pub hosts: Vec<String>,
    pub expires_in_hours: Option<u64>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// What a post did, and the notice it left.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoticePostResult {
    pub notice: Notice,
    pub outcome: PostOutcome,
}

fn refuse(code: NoticeRefusalCode, message: impl Into<String>) -> OrbitError {
    OrbitError::notice_refused(code, message)
}

fn limit(name: &str, value: u32) -> OrbitError {
    refuse(
        NoticeRefusalCode::LimitExceeded,
        format!("limit `{name}` ({value}) reached"),
    )
}

fn has_control(value: &str, allow_layout: bool) -> bool {
    value
        .chars()
        .any(|ch| ch.is_control() && !(allow_layout && (ch == '\n' || ch == '\t')))
}

/// `task:<linked id>`, else `<kind>:` plus the slugged title; lowercase and
/// at most 120 characters.
pub(crate) fn derive_dedupe_key(kind: NoticeKind, title: &str, linked: Option<&str>) -> String {
    let mut key = match linked {
        Some(task) => format!("task:{}", task.to_ascii_lowercase()),
        None => {
            let mut slug = String::new();
            for ch in title.to_lowercase().chars() {
                if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
                    slug.push(ch);
                } else if !slug.ends_with('-') {
                    slug.push('-');
                }
            }
            format!("{}:{}", kind.as_str(), slug.trim_matches('-'))
        }
    };
    key.retain(dedupe_key_char);
    key.chars().take(DERIVED_DEDUPE_KEY_CHARS).collect()
}

fn dedupe_key_char(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | ':' | '/' | '-')
}

/// The content a post sets, validated against the poster and the limits.
struct ValidatedPost {
    kind: NoticeKind,
    title: String,
    body: String,
    scope: Option<NoticeScope>,
    dedupe_key: String,
    linked_task: Option<NoticeLinkedTask>,
    close_on_linked_done: bool,
    evidence: Vec<NoticeEvidence>,
    hosts: Vec<String>,
    /// `None` keeps a matched notice's expiry, or the default for a new one.
    expires_at: Option<DateTime<Utc>>,
    default_expires_at: DateTime<Utc>,
}

impl OrbitRuntime {
    /// Post a notice for `caller`, recording an audit row for the write or
    /// the refusal.
    pub fn post_notice(
        &self,
        caller: &NoticeCaller,
        request: NoticePostRequest,
    ) -> Result<NoticePostResult, OrbitError> {
        let mut audit = NoticeAudit::new("post", None);
        audit.detail = serde_json::json!({
            "kind": request.kind.as_str(),
            "scope": request.scope.map(NoticeScope::as_str),
            "dedupe_key": request.dedupe_key,
            "linked_task": request.linked_task,
        });
        let result = self.post_notice_unaudited(caller, request, &mut audit);
        audit.record(
            self,
            &result.as_ref().map(|posted| posted.notice.id.clone()),
        );
        result
    }

    fn post_notice_unaudited(
        &self,
        caller: &NoticeCaller,
        request: NoticePostRequest,
        audit: &mut NoticeAudit,
    ) -> Result<NoticePostResult, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let poster = self.resolve_notice_poster(caller)?;
        audit.poster = Some(poster.class);
        let settings = *self.notice_settings();
        let now = Utc::now();
        let post = self.validate_notice_post(&poster, &settings, request, now)?;
        audit.detail["dedupe_key"] = serde_json::Value::String(post.dedupe_key.clone());
        let workspace_id = self.workspace_id()?;
        let workspace = NoticeWorkspace {
            name: self.notice_workspace_name(&workspace_id),
            id: workspace_id,
        };
        let store = self.sqlite_store()?;
        let linked_done = self.linked_done_tasks(
            &store.notice_rows(Some(std::slice::from_ref(&workspace.id)), true)?,
        );
        let events_since = match poster.class {
            AuthorClass::Agent => now - Duration::hours(24),
            AuthorClass::Orchestrator => now - Duration::hours(1),
            AuthorClass::Operator | AuthorClass::System => now,
        };
        let purge_before = now - Duration::days(i64::from(settings.retention_days));
        let query = NoticePostQuery {
            workspace_id: &workspace.id,
            poster_task_id: match poster.class {
                AuthorClass::Agent => poster.task_id.as_deref(),
                _ => None,
            },
            poster_crew: match poster.class {
                AuthorClass::Orchestrator => poster.crew.as_deref(),
                _ => None,
            },
            events_since,
            purge_before: Some(purge_before),
        };
        let outcome = std::cell::Cell::new(PostOutcome::Created);
        let decide = |snapshot: &NoticePostSnapshot| -> Result<NoticePostPlan, OrbitError> {
            let (plan, decided) = decide_post(
                &poster,
                &settings,
                &workspace,
                &post,
                snapshot,
                &linked_done,
                now,
            )?;
            outcome.set(decided);
            Ok(plan)
        };
        let row = store.post_notice(&query, &decide)?;
        let mut notice = parse_notice_row(&row)?;
        notice.resolve_effective_status(now, None);
        let outcome = outcome.get();
        audit.detail["outcome"] = serde_json::Value::String(outcome.as_str().to_string());
        Ok(NoticePostResult { notice, outcome })
    }

    fn validate_notice_post(
        &self,
        poster: &NoticePoster,
        settings: &NoticeSettings,
        request: NoticePostRequest,
        now: DateTime<Utc>,
    ) -> Result<ValidatedPost, OrbitError> {
        let NoticePostRequest {
            kind,
            title,
            body,
            scope,
            dedupe_key,
            linked_task,
            close_on_linked_done,
            evidence,
            hosts,
            expires_in_hours,
            expires_at,
        } = request;
        if let Some(scope) = scope
            && !poster.may_post_scope(scope)
        {
            return Err(refuse(
                NoticeRefusalCode::ScopeRefused,
                "a managed agent may post only workspace-scoped notices",
            ));
        }
        if !poster.may_post_kind(kind) {
            return Err(refuse(
                NoticeRefusalCode::KindRefused,
                "a managed agent may post only incident or info notices",
            ));
        }
        let title = title.trim().to_string();
        if title.is_empty() || has_control(&title, false) {
            return Err(refuse(
                NoticeRefusalCode::InvalidField,
                "`title` must be non-empty single-line text",
            ));
        }
        if title.chars().count() > settings.max_title_chars as usize {
            return Err(limit("max_title_chars", settings.max_title_chars));
        }
        let body = body.unwrap_or_default();
        if has_control(&body, true) {
            return Err(refuse(
                NoticeRefusalCode::InvalidField,
                "`body` may not contain control characters other than newline and tab",
            ));
        }
        if body.len() > settings.max_body_bytes as usize {
            return Err(limit("max_body_bytes", settings.max_body_bytes));
        }
        if evidence.len() > settings.max_evidence as usize {
            return Err(limit("max_evidence", settings.max_evidence));
        }
        for item in &evidence {
            if !NoticeEvidence::TYPES.contains(&item.kind.as_str()) {
                return Err(refuse(
                    NoticeRefusalCode::InvalidField,
                    format!(
                        "evidence type `{}` is not one of {}",
                        item.kind,
                        NoticeEvidence::TYPES.join(", ")
                    ),
                ));
            }
            let value = item.value.trim();
            if value.is_empty()
                || value.chars().count() > MAX_EVIDENCE_VALUE_CHARS
                || has_control(value, false)
            {
                return Err(refuse(
                    NoticeRefusalCode::InvalidField,
                    format!("evidence values are 1 to {MAX_EVIDENCE_VALUE_CHARS} characters"),
                ));
            }
        }
        let evidence: Vec<NoticeEvidence> = evidence
            .into_iter()
            .map(|item| NoticeEvidence {
                kind: item.kind,
                value: item.value.trim().to_string(),
            })
            .collect();
        let hosts: Vec<String> = hosts
            .iter()
            .map(|host| host.trim().to_string())
            .filter(|host| !host.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !hosts.is_empty() && kind != NoticeKind::Maintenance {
            return Err(refuse(
                NoticeRefusalCode::InvalidField,
                "`hosts` applies only to maintenance notices",
            ));
        }
        if hosts.len() > MAX_HOSTS {
            return Err(refuse(
                NoticeRefusalCode::InvalidField,
                format!("a notice names at most {MAX_HOSTS} hosts"),
            ));
        }
        let linked_task = match linked_task
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            Some(id) => {
                let task = self.get_task(id).map_err(|_| {
                    refuse(
                        NoticeRefusalCode::InvalidField,
                        format!("linked task `{id}` is not a task in this workspace"),
                    )
                })?;
                Some(NoticeLinkedTask {
                    id: task.id.to_string(),
                    workspace_id: self.workspace_id()?,
                    status: None,
                })
            }
            None => None,
        };
        if poster.class == AuthorClass::Agent && linked_task.is_none() && evidence.is_empty() {
            return Err(refuse(
                NoticeRefusalCode::EvidenceRequired,
                "a managed agent's notice needs a linked task or at least one evidence reference",
            ));
        }
        let dedupe_key = match dedupe_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            Some(key) => {
                if key.chars().count() > MAX_DEDUPE_KEY_CHARS || !key.chars().all(dedupe_key_char) {
                    return Err(refuse(
                        NoticeRefusalCode::InvalidField,
                        format!(
                            "`dedupe_key` is 1 to {MAX_DEDUPE_KEY_CHARS} characters of a-z, 0-9 and . _ : / -"
                        ),
                    ));
                }
                key.to_string()
            }
            None => derive_dedupe_key(
                kind,
                &title,
                linked_task.as_ref().map(|task| task.id.as_str()),
            ),
        };
        if dedupe_key.is_empty() {
            return Err(refuse(
                NoticeRefusalCode::InvalidField,
                "no dedupe key could be derived; pass `dedupe_key`",
            ));
        }
        let cap = poster.expiry_cap_hours(settings, kind);
        let earliest = now + Duration::minutes(MIN_EXPIRY_MINUTES);
        let expires_at = match (expires_in_hours, expires_at) {
            (Some(_), Some(_)) => {
                return Err(refuse(
                    NoticeRefusalCode::InvalidField,
                    "pass `expires_in_hours` or `expires_at`, not both",
                ));
            }
            (Some(hours), None) => {
                if hours == 0 {
                    return Err(refuse(
                        NoticeRefusalCode::ExpiryInvalid,
                        "a notice must stay in force for at least five minutes",
                    ));
                }
                if hours > u64::from(cap) {
                    return Err(refuse(
                        NoticeRefusalCode::ExpiryExceedsCap,
                        format!("{hours} hours exceeds this poster's {cap}-hour cap"),
                    ));
                }
                Some(now + Duration::hours(i64::from(u32::try_from(hours).unwrap_or(cap))))
            }
            (None, Some(at)) => {
                if at < earliest {
                    return Err(refuse(
                        NoticeRefusalCode::ExpiryInvalid,
                        "`expires_at` must be at least five minutes in the future",
                    ));
                }
                if at > now + Duration::hours(i64::from(cap)) {
                    return Err(refuse(
                        NoticeRefusalCode::ExpiryExceedsCap,
                        format!("`expires_at` exceeds this poster's {cap}-hour cap"),
                    ));
                }
                Some(at)
            }
            (None, None) => None,
        };
        let default_expires_at =
            now + Duration::hours(i64::from(poster.default_expiry_hours(settings, kind)));
        Ok(ValidatedPost {
            kind,
            title,
            body,
            scope,
            dedupe_key,
            close_on_linked_done: linked_task.is_some() && close_on_linked_done.unwrap_or(true),
            linked_task,
            evidence,
            hosts,
            expires_at,
            default_expires_at,
        })
    }
}

/// Whether a stored-open row is in force at `now`.
fn effective_open(row: &NoticeRow, now: DateTime<Utc>, linked_done: &LinkedDone) -> bool {
    row.closed_at.is_none()
        && row.expires_at > now
        && !(row.close_on_linked_done
            && row
                .linked_task_id
                .as_ref()
                .is_some_and(|task| linked_done.contains_key(task)))
}

fn new_notice_id() -> Result<String, OrbitError> {
    let value = getrandom::u64()
        .map_err(|error| OrbitError::Execution(format!("mint notice id: {error}")))?;
    Ok(format!("ntc_{value:016x}"))
}

#[allow(clippy::too_many_arguments)]
fn decide_post(
    poster: &NoticePoster,
    settings: &NoticeSettings,
    workspace: &NoticeWorkspace,
    post: &ValidatedPost,
    snapshot: &NoticePostSnapshot,
    linked_done: &LinkedDone,
    now: DateTime<Utc>,
) -> Result<(NoticePostPlan, PostOutcome), OrbitError> {
    match poster.class {
        AuthorClass::Agent
            if snapshot.poster_events.len() >= settings.agent_posts_per_task_per_day as usize =>
        {
            return Err(limit(
                "agent_posts_per_task_per_day",
                settings.agent_posts_per_task_per_day,
            ));
        }
        AuthorClass::Orchestrator
            if snapshot.poster_events.len() >= settings.orchestrator_posts_per_hour as usize =>
        {
            return Err(limit(
                "orchestrator_posts_per_hour",
                settings.orchestrator_posts_per_hour,
            ));
        }
        _ => {}
    }
    let open: Vec<&NoticeRow> = snapshot
        .unclosed
        .iter()
        .filter(|row| effective_open(row, now, linked_done))
        .collect();
    let open_globals = open.iter().filter(|row| row.scope == "global").count();
    let matched = open
        .iter()
        .find(|row| row.workspace_id == workspace.id && row.dedupe_key == post.dedupe_key);
    let event = |notice_id: &str, outcome: PostOutcome, revision: u32| NoticeEventRow {
        workspace_id: workspace.id.clone(),
        notice_id: notice_id.to_string(),
        at: now,
        action: outcome.as_str().to_string(),
        poster_class: poster.class.as_str().to_string(),
        poster_crew: poster.crew.clone(),
        poster_task_id: poster.task_id.clone(),
        detail_json: serde_json::json!({"revision": revision}).to_string(),
    };
    let Some(matched) = matched else {
        let scope = post.scope.unwrap_or(NoticeScope::Workspace);
        let home_open = open
            .iter()
            .filter(|row| row.workspace_id == workspace.id)
            .count();
        if home_open >= settings.max_open_per_workspace as usize {
            return Err(limit(
                "max_open_per_workspace",
                settings.max_open_per_workspace,
            ));
        }
        if scope == NoticeScope::Global
            && open_globals >= settings.max_open_global_per_owner as usize
        {
            return Err(limit(
                "max_open_global_per_owner",
                settings.max_open_global_per_owner,
            ));
        }
        if poster.class == AuthorClass::Agent {
            let task_open = open
                .iter()
                .filter(|row| row.author_task_id.is_some() && row.author_task_id == poster.task_id)
                .count();
            if task_open >= settings.max_open_per_agent_task as usize {
                return Err(limit(
                    "max_open_per_agent_task",
                    settings.max_open_per_agent_task,
                ));
            }
        }
        let notice = Notice {
            id: new_notice_id()?,
            workspace: workspace.clone(),
            scope,
            kind: post.kind,
            title: post.title.clone(),
            body: post.body.clone(),
            dedupe_key: post.dedupe_key.clone(),
            author: poster.author(),
            linked_task: post.linked_task.clone(),
            close_on_linked_done: post.close_on_linked_done,
            evidence: post.evidence.clone(),
            hosts: post.hosts.clone(),
            expires_at: post.expires_at.unwrap_or(post.default_expires_at),
            opened_at: now,
            updated_at: now,
            revision: 1,
            reports: NoticeReports {
                count: 1,
                last_at: now,
            },
            closed: None,
            status: EffectiveStatus::Open,
        };
        let row = notice_row(&notice)?;
        let event = event(&notice.id, PostOutcome::Created, 1);
        return Ok((NoticePostPlan::Insert { row, event }, PostOutcome::Created));
    };
    let mut notice = parse_notice_row(matched)?;
    let expected_revision = matched.revision;
    notice.reports.count = notice.reports.count.saturating_add(1);
    notice.reports.last_at = now;
    if !poster.may_edit(&matched.author_class, matched.author_task_id.as_deref()) {
        let row = notice_row(&notice)?;
        let event = event(&notice.id, PostOutcome::Corroborated, notice.revision);
        return Ok((
            NoticePostPlan::Update {
                row,
                expected_revision,
                event,
            },
            PostOutcome::Corroborated,
        ));
    }
    let scope = post.scope.unwrap_or(notice.scope);
    if scope == NoticeScope::Global && notice.scope == NoticeScope::Workspace {
        if !poster.may_post_scope(NoticeScope::Global) {
            return Err(refuse(
                NoticeRefusalCode::ScopeRefused,
                "this poster may not widen a notice to global scope",
            ));
        }
        if open_globals >= settings.max_open_global_per_owner as usize {
            return Err(limit(
                "max_open_global_per_owner",
                settings.max_open_global_per_owner,
            ));
        }
    }
    if scope == NoticeScope::Workspace
        && notice.scope == NoticeScope::Global
        && notice.author.class != poster.class
    {
        return Err(refuse(
            NoticeRefusalCode::ScopeRefused,
            "a global notice another class authored cannot be narrowed",
        ));
    }
    let before = notice.clone();
    notice.scope = scope;
    notice.kind = post.kind;
    notice.title = post.title.clone();
    notice.body = post.body.clone();
    notice.evidence = post.evidence.clone();
    notice.hosts = post.hosts.clone();
    if post.linked_task.is_some() {
        notice.linked_task = post.linked_task.clone();
        notice.close_on_linked_done = post.close_on_linked_done;
    }
    if let Some(expires_at) = post.expires_at {
        notice.expires_at = expires_at;
    }
    let changed = {
        let mut compare = before.clone();
        compare.reports = notice.reports.clone();
        compare != notice
    };
    if changed {
        notice.revision = notice.revision.saturating_add(1);
        notice.updated_at = now;
    }
    let row = notice_row(&notice)?;
    let event = event(&notice.id, PostOutcome::Updated, notice.revision);
    Ok((
        NoticePostPlan::Update {
            row,
            expected_revision,
            event,
        },
        PostOutcome::Updated,
    ))
}
