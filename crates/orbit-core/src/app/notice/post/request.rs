use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_types::record::{
    AuthorClass, NoticeEvidence, NoticeKind, NoticeLinkedTask, NoticeScope,
    resident_notice_key_prefix,
};

use crate::OrbitRuntime;
use crate::app::notice::authority::NoticePoster;
use crate::app::notice::{limit as limits, refuse};

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
#[derive(Debug, Clone, serde::Serialize)]
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

/// Raw notice-post fields as one surface collected them, before the core
/// parse. `kind` and `expires_at` are still strings; optional strings are
/// uninterpreted, so the blank rule below is the only one.
#[derive(Debug, Clone, Default)]
pub struct NoticePostRawFields {
    pub kind: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub scope: Option<String>,
    pub dedupe_key: Option<String>,
    pub linked_task: Option<String>,
    pub close_on_linked_done: Option<bool>,
    pub evidence: Vec<NoticeEvidence>,
    pub hosts: Vec<String>,
    pub expires_in_hours: Option<u64>,
    pub expires_at: Option<String>,
    /// The dashboard form submits empty fields: a blank optional string
    /// means absent. Tool input refuses a present blank instead.
    pub blank_means_absent: bool,
}

impl NoticePostRequest {
    /// Parse the raw fields every post surface collects into a request.
    /// Unknown `kind`/`scope`, a bad `expires_at` and (when
    /// `blank_means_absent` is false) a blank optional string refuse with
    /// `NoticeRefused` / `invalid_field`. The mutual-exclusion of
    /// `expires_in_hours` and `expires_at` stays in expiry resolution, so
    /// an unchanged retry is still recognised before it is refused.
    pub fn parse_raw(raw: NoticePostRawFields) -> Result<Self, OrbitError> {
        let kind = raw
            .kind
            .as_deref()
            .and_then(NoticeKind::parse)
            .ok_or_else(|| {
                refuse(
                    NoticeRefusalCode::InvalidField,
                    format!("`kind` must be {}", NoticeKind::NAMES),
                )
            })?;
        let scope = match blank_optional(raw.scope, "scope", raw.blank_means_absent)? {
            None => None,
            Some(value) => Some(NoticeScope::parse(&value).ok_or_else(|| {
                refuse(
                    NoticeRefusalCode::InvalidField,
                    format!("`scope` must be {}", NoticeScope::NAMES),
                )
            })?),
        };
        let expires_at = match blank_optional(raw.expires_at, "expires_at", raw.blank_means_absent)?
        {
            None => None,
            Some(value) => Some(
                DateTime::parse_from_rfc3339(&value)
                    .map(|at| at.with_timezone(&Utc))
                    .map_err(|_| {
                        refuse(
                            NoticeRefusalCode::InvalidField,
                            "`expires_at` must be an RFC 3339 timestamp",
                        )
                    })?,
            ),
        };
        Ok(Self {
            kind,
            title: raw.title,
            body: raw.body,
            scope,
            dedupe_key: blank_optional(raw.dedupe_key, "dedupe_key", raw.blank_means_absent)?,
            linked_task: blank_optional(raw.linked_task, "linked_task", raw.blank_means_absent)?,
            close_on_linked_done: raw.close_on_linked_done,
            evidence: raw.evidence,
            hosts: raw.hosts,
            expires_in_hours: raw.expires_in_hours,
            expires_at,
        })
    }
}

/// The content a post sets, validated against the poster and the limits.
pub(super) struct ValidatedPost {
    pub(super) kind: NoticeKind,
    pub(super) title: String,
    pub(super) body: String,
    pub(super) scope: Option<NoticeScope>,
    pub(super) dedupe_key: String,
    /// The post writes its resident poster's status line.
    pub(super) status_line: bool,
    pub(super) linked_task: Option<NoticeLinkedTask>,
    pub(super) close_on_linked_done: bool,
    pub(super) evidence: Vec<NoticeEvidence>,
    pub(super) hosts: Vec<String>,
    /// Request values, with omitted defaults resolved except for scope, which
    /// depends on the matched notice. Keep expiry relative when requested so
    /// lost-reply retries compare the request rather than a new expiry instant.
    fingerprint_request: NoticePostRequest,
}

impl ValidatedPost {
    /// Fingerprint the given request and the complete resolved poster, resolving
    /// only the defaults. In particular, ignore neither a corroborator's content
    /// nor its expiry input merely because that write cannot replace them.
    pub(super) fn request_fingerprint(
        &self,
        poster: &NoticePoster,
        matched_scope: NoticeScope,
    ) -> String {
        let mut request = self.fingerprint_request.clone();
        request.scope = Some(self.scope.unwrap_or(matched_scope));
        let identity =
            serde_json::json!({"version": 1, "author": poster.author(), "request": request});
        blake3::hash(identity.to_string().as_bytes())
            .to_hex()
            .to_string()
    }

    /// Validate and resolve expiry only after ruling out an unchanged retry.
    pub(super) fn resolve_expiry(
        &self,
        poster: &NoticePoster,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, OrbitError> {
        let cap = poster.expiry_cap_hours(self.kind);
        let earliest = now + Duration::minutes(MIN_EXPIRY_MINUTES);
        let requested = (
            self.fingerprint_request.expires_in_hours,
            self.fingerprint_request.expires_at,
        );
        if let Some(resident) = &poster.resident {
            // Every post of a resident's notice renews it, so one that is
            // no longer re-posted lapses: the status line after twice the
            // heartbeat, anything else after its kind's default.
            if self.status_line {
                if requested != (None, None) {
                    return Err(refuse(
                        NoticeRefusalCode::ExpiryInvalid,
                        "a resident's status line expires at twice the mandate's heartbeat; omit `expires_in_hours` and `expires_at`",
                    ));
                }
                return Ok(Some(now + resident.status_line_ttl));
            }
            if requested == (None, None) {
                return Ok(Some(
                    now + Duration::hours(i64::from(poster.default_expiry_hours(self.kind))),
                ));
            }
        }
        match requested {
            (Some(_), Some(_)) => Err(refuse(
                NoticeRefusalCode::InvalidField,
                "pass `expires_in_hours` or `expires_at`, not both",
            )),
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
                Ok(Some(
                    now + Duration::hours(i64::from(u32::try_from(hours).unwrap_or(cap))),
                ))
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
                Ok(Some(at))
            }
            (None, None) => Ok(None),
        }
    }
}

pub(super) fn validate_notice_post(
    rt: &OrbitRuntime,
    poster: &NoticePoster,
    request: NoticePostRequest,
) -> Result<ValidatedPost, OrbitError> {
    let mut fingerprint_request = request.clone();
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
        expires_in_hours: _,
        expires_at: _,
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
            "a managed agent may post only incident or info notices (a resident cycle may also post warn)",
        ));
    }
    let title = title.trim().to_string();
    if title.is_empty() || has_control(&title, false) {
        return Err(refuse(
            NoticeRefusalCode::InvalidField,
            "`title` must be non-empty single-line text",
        ));
    }
    if title.chars().count() > limits::MAX_TITLE_CHARS as usize {
        return Err(limit("max_title_chars", limits::MAX_TITLE_CHARS));
    }
    let body = body.unwrap_or_default();
    if has_control(&body, true) {
        return Err(refuse(
            NoticeRefusalCode::InvalidField,
            "`body` may not contain control characters other than newline and tab",
        ));
    }
    if body.len() > limits::MAX_BODY_BYTES as usize {
        return Err(limit("max_body_bytes", limits::MAX_BODY_BYTES));
    }
    if evidence.len() > limits::MAX_EVIDENCE as usize {
        return Err(limit("max_evidence", limits::MAX_EVIDENCE));
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
            // A link must resolve on this owner: the home workspace or
            // another workspace the same host owns. A task another host
            // holds is refused, because this owner could not evaluate
            // linked-task closure without a cross-host read.
            use orbit_store::RegisteredTaskResolution;
            let task = match rt.resolve_dependency_task(id) {
                Ok(RegisteredTaskResolution::Resolved(task)) => *task,
                Ok(RegisteredTaskResolution::ForeignAuthority) => {
                    return Err(refuse(
                        NoticeRefusalCode::InvalidField,
                        format!(
                            "linked task `{id}` is held by another host; a notice links only a task this workspace's owner holds"
                        ),
                    ));
                }
                Ok(RegisteredTaskResolution::Missing) | Err(_) => {
                    return Err(refuse(
                        NoticeRefusalCode::InvalidField,
                        format!(
                            "linked task `{id}` is not a task any workspace on this host holds"
                        ),
                    ));
                }
            };
            Some(NoticeLinkedTask {
                id: task.id.to_string(),
                workspace_id: rt
                    .registered_task_workspace(&task.id.to_string())
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| rt.workspace_id().unwrap_or_default()),
                status: None,
            })
        }
        None => None,
    };
    // A resident's notices are attributed to its mandate, which is the
    // evidence: its status line has nothing else to cite.
    if poster.class == AuthorClass::Agent
        && poster.resident.is_none()
        && linked_task.is_none()
        && evidence.is_empty()
    {
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
        None => {
            let derived = derive_dedupe_key(
                kind,
                &title,
                linked_task.as_ref().map(|task| task.id.as_str()),
            );
            // A resident's warn is keyed under its own mandate, so it
            // can always replace and close it: `resident:<mandate>:`
            // then the blocker (`task:<id>`, or the slugged title).
            match (kind, &poster.resident, poster.task_id.as_deref()) {
                (NoticeKind::Warn, Some(_), Some(mandate)) => {
                    format!("{}{derived}", resident_notice_key_prefix(mandate))
                        .chars()
                        .take(MAX_DEDUPE_KEY_CHARS)
                        .collect()
                }
                _ => derived,
            }
        }
    };
    if dedupe_key.is_empty() {
        return Err(refuse(
            NoticeRefusalCode::InvalidField,
            "no dedupe key could be derived; pass `dedupe_key`",
        ));
    }
    if !poster.may_post_key(&dedupe_key) {
        return Err(refuse(
            NoticeRefusalCode::InvalidField,
            "`dedupe_key` values under `resident:` are reserved for that resident mandate's own cycles",
        ));
    }
    let status_line = poster.resident_status_key().as_deref() == Some(dedupe_key.as_str());
    if status_line && kind != NoticeKind::Info {
        return Err(refuse(
            NoticeRefusalCode::KindRefused,
            "a resident's status line is an info notice; post an operator ask as a warn under its own key",
        ));
    }
    let close_on_linked_done = linked_task.is_some() && close_on_linked_done.unwrap_or(true);
    fingerprint_request.body.get_or_insert_default();
    fingerprint_request
        .dedupe_key
        .get_or_insert_with(|| dedupe_key.clone());
    fingerprint_request
        .close_on_linked_done
        .get_or_insert(close_on_linked_done);
    Ok(ValidatedPost {
        kind,
        title,
        body,
        scope,
        dedupe_key,
        status_line,
        linked_task,
        close_on_linked_done,
        evidence,
        hosts,
        fingerprint_request,
    })
}

fn blank_optional(
    value: Option<String>,
    field: &str,
    blank_means_absent: bool,
) -> Result<Option<String>, OrbitError> {
    match value {
        None => Ok(None),
        Some(value) if value.trim().is_empty() => {
            if blank_means_absent {
                Ok(None)
            } else {
                Err(refuse(
                    NoticeRefusalCode::InvalidField,
                    format!("`{field}` must not be empty"),
                ))
            }
        }
        Some(value) => Ok(Some(value)),
    }
}

pub(super) fn limit(name: &str, value: u32) -> OrbitError {
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
