//! Admission-time evaluation of constrained application operations
//! (application-approvals design §5.1) and the optional per-scope rate limit.
//!
//! Every check reads current state through the kernel's `ApplicationPort`
//! forwarders. A refusal names the scope, the field and the constraint kind
//! that failed, never a value the call sent.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::authorization::APPLICATION_BACKLOG_TRANSITION_SOURCES;
use orbit_types::tool::{
    ApplicationArtifactEvidence, ApplicationConsent, ApplicationConstraint,
    ApplicationPredicateEvidence, ApplicationRateLimit, ApplicationTaskPredicate,
    ApplicationTextDigest,
};
use serde_json::Value;

use crate::RuntimeKernel;

/// Fields that name the object a call acts on; their admitted values are
/// recorded as bindings.
const IDENTITY_FIELDS: &[&str] = &["id", "name", "path", "draft"];

/// What a constrained operation admitted: the evidence its invocation carries.
#[derive(Debug, Default)]
pub(crate) struct ConstrainedAdmission {
    pub(crate) bindings: BTreeMap<String, Value>,
    pub(crate) text_digests: BTreeMap<String, ApplicationTextDigest>,
    pub(crate) predicate: Option<ApplicationPredicateEvidence>,
    /// The task a callback's `pin` predicate binds, when none was pinned yet.
    pub(crate) pin: Option<String>,
}

/// What a callback is evaluated against: the entry's admitted values and the
/// task the invocation has pinned so far.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CallbackBindings<'a> {
    pub(crate) bindings: &'a BTreeMap<String, Value>,
    pub(crate) pinned_task: Option<&'a str>,
}

/// The first field of a constrained operation that failed, and why. The
/// field is a consent-authored name, or `None` for a field the call sent that
/// the consent does not list (its name is call input and is not echoed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConstraintRefusal {
    pub(crate) field: Option<String>,
    pub(crate) constraint: &'static str,
}

impl ConstraintRefusal {
    fn new(field: &str, constraint: &'static str) -> Self {
        Self {
            field: Some(field.to_owned()),
            constraint,
        }
    }

    /// `field `status` failed `transition``; never a call value.
    pub(crate) fn describe(&self) -> String {
        match &self.field {
            Some(field) => format!("field `{field}` failed `{}`", self.constraint),
            None => "the call sends a field the scope does not list".to_owned(),
        }
    }
}

/// Evaluate a constrained operation's `fields` against the call input on
/// current state: an entry when `callback` is `None`, otherwise a callback
/// against the entry's bindings. `Ok(Err(_))` is a refusal; `Err(_)` is a
/// host failure.
pub(crate) fn evaluate_constrained(
    kernel: &RuntimeKernel,
    consent: &ApplicationConsent,
    fields: &BTreeMap<String, ApplicationConstraint>,
    input: &Value,
    callback: Option<CallbackBindings<'_>>,
) -> Result<Result<ConstrainedAdmission, ConstraintRefusal>, OrbitError> {
    let Some(object) = input.as_object() else {
        return Ok(Err(ConstraintRefusal {
            field: None,
            constraint: "object",
        }));
    };
    if object.keys().any(|key| !fields.contains_key(key)) {
        return Ok(Err(ConstraintRefusal {
            field: None,
            constraint: "unlisted",
        }));
    }
    let mut admitted = ConstrainedAdmission::default();
    for (field, constraint) in fields {
        let Some(value) = object.get(field) else {
            if matches!(constraint, ApplicationConstraint::Optional(_)) {
                continue;
            }
            return Ok(Err(ConstraintRefusal::new(field, "missing")));
        };
        let required = constraint.required_form();
        let check = FieldCheck {
            kernel,
            consent,
            input,
            field,
            callback,
        };
        if let Err(refusal) = check.value(required, value, &mut admitted)? {
            return Ok(Err(refusal));
        }
        // Bindings are the entry's; a callback is held to them, not adding.
        if callback.is_none()
            && (IDENTITY_FIELDS.contains(&field.as_str())
                || matches!(
                    required,
                    ApplicationConstraint::Task(_)
                        | ApplicationConstraint::Sha256(_)
                        | ApplicationConstraint::Token(_)
                ))
        {
            admitted.bindings.insert(field.clone(), value.clone());
        }
    }
    Ok(Ok(admitted))
}

struct FieldCheck<'a> {
    kernel: &'a RuntimeKernel,
    consent: &'a ApplicationConsent,
    input: &'a Value,
    field: &'a str,
    callback: Option<CallbackBindings<'a>>,
}

impl FieldCheck<'_> {
    fn value(
        &self,
        constraint: &ApplicationConstraint,
        value: &Value,
        admitted: &mut ConstrainedAdmission,
    ) -> Result<Result<(), ConstraintRefusal>, OrbitError> {
        let refuse = || Ok(Err(ConstraintRefusal::new(self.field, constraint.kind())));
        let passed = match constraint {
            ApplicationConstraint::Const(expected) => value == expected,
            ApplicationConstraint::OneOf(values) => values.contains(value),
            ApplicationConstraint::Text(text) => match admitted_text(value, text.max_chars) {
                Some(digest) => {
                    admitted.text_digests.insert(self.field.to_owned(), digest);
                    true
                }
                None => false,
            },
            ApplicationConstraint::Token(token) => value.as_str().is_some_and(|s| {
                !s.is_empty()
                    && s.len() <= token.max_len as usize
                    && s.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-')
                    })
            }),
            ApplicationConstraint::Sha256(_) => value.as_str().is_some_and(is_sha256_hex),
            ApplicationConstraint::Workspace(_) => value
                .as_str()
                .is_some_and(|selector| self.selects_consent_workspace(selector)),
            ApplicationConstraint::Task(predicate) => match value.as_str() {
                Some(id) => {
                    self.pin_admits(id, predicate, admitted)
                        && self.task_matches(id, predicate, admitted)?
                }
                None => false,
            },
            ApplicationConstraint::Transition(transition) => {
                value.as_str() == Some(transition.to.as_str())
                    && self.transition_source_matches(&transition.from, admitted)?
            }
            ApplicationConstraint::SubsetOf(allowed) => value.as_array().is_some_and(|items| {
                let mut seen = std::collections::BTreeSet::new();
                items.iter().all(|item| {
                    item.as_str()
                        .is_some_and(|s| allowed.iter().any(|a| a == s) && seen.insert(s))
                })
            }),
            // The entry's admitted value; an `id` is also held to the pin.
            // An entry never admits it.
            ApplicationConstraint::Bound(entry_field) => self.callback.is_some_and(|callback| {
                callback.bindings.get(entry_field) == Some(value)
                    && (self.field != "id"
                        || callback
                            .pinned_task
                            .is_none_or(|pinned| value.as_str() == Some(pinned)))
            }),
            // Artifact pointers are admitted from slice 5; nested optionals
            // are refused at consent.
            ApplicationConstraint::ArtifactJson(_) | ApplicationConstraint::Optional(_) => false,
        };
        if passed { Ok(Ok(())) } else { refuse() }
    }

    fn selects_consent_workspace(&self, selector: &str) -> bool {
        selector == self.consent.workspace_id
            || Path::new(selector)
                .canonicalize()
                .is_ok_and(|path| path == Path::new(&self.consent.workspace_root))
    }

    /// A `pin` predicate on a callback admits the task already pinned, or
    /// any task when none is, recording it to pin. Without `pin` this passes.
    fn pin_admits(
        &self,
        id: &str,
        predicate: &ApplicationTaskPredicate,
        admitted: &mut ConstrainedAdmission,
    ) -> bool {
        if !predicate.pin {
            return true;
        }
        let Some(callback) = self.callback else {
            return false;
        };
        match callback.pinned_task {
            Some(pinned) => pinned == id,
            None => {
                admitted.pin = Some(id.to_owned());
                true
            }
        }
    }

    /// The task named by `id` exists here and its current tags, status and
    /// (when bound) artifact digest satisfy the predicate.
    fn task_matches(
        &self,
        id: &str,
        predicate: &ApplicationTaskPredicate,
        admitted: &mut ConstrainedAdmission,
    ) -> Result<bool, OrbitError> {
        // A failed read (unknown id, another host's prefix) is a refusal; its
        // message would echo the id, so it is not surfaced.
        let Ok(task) = self.kernel.get_task(id) else {
            return Ok(false);
        };
        if task.id != id {
            return Ok(false);
        }
        let has = |tag: &String| task.tags.iter().any(|t| t == tag);
        if !predicate.tags_all.iter().all(has)
            || (!predicate.tags_any.is_empty() && !predicate.tags_any.iter().any(has))
        {
            return Ok(false);
        }
        let status = task.status.to_string();
        if !predicate.status.contains(&status) {
            return Ok(false);
        }
        let mut artifact = None;
        if let Some(digest) = &predicate.artifact_sha256 {
            let Some(claimed) = self.input.get(&digest.field).and_then(Value::as_str) else {
                return Ok(false);
            };
            let Ok(Some(actual)) = self.kernel.task_artifact_sha256(&task.id, &digest.path) else {
                return Ok(false);
            };
            if actual != claimed {
                return Ok(false);
            }
            artifact = Some(ApplicationArtifactEvidence {
                path: digest.path.clone(),
                sha256: actual,
            });
        }
        let tags_matched = task
            .tags
            .iter()
            .filter(|tag| predicate.tags_all.contains(tag) || predicate.tags_any.contains(tag))
            .cloned()
            .collect();
        admitted.predicate = Some(ApplicationPredicateEvidence {
            task: task.id,
            status_seen: status,
            tags_matched,
            artifact,
        });
        Ok(true)
    }

    /// The current status of the task this call's `id` names is in `from`.
    /// Approving work for dispatch (`backlog` from anything but review or
    /// in-progress) is refused here too, whatever the consent says.
    fn transition_source_matches(
        &self,
        from: &[String],
        admitted: &mut ConstrainedAdmission,
    ) -> Result<bool, OrbitError> {
        let Some(id) = self.input.get("id").and_then(Value::as_str) else {
            return Ok(false);
        };
        let Ok(task) = self.kernel.get_task(id) else {
            return Ok(false);
        };
        if task.id != id {
            return Ok(false);
        }
        let status = task.status.to_string();
        if !from.contains(&status) {
            return Ok(false);
        }
        if self.input.get("status").and_then(Value::as_str) == Some("backlog")
            && !APPLICATION_BACKLOG_TRANSITION_SOURCES.contains(&status.as_str())
        {
            return Ok(false);
        }
        match &mut admitted.predicate {
            Some(evidence) if evidence.task == task.id && evidence.status_seen != status => {
                return Ok(false);
            }
            Some(_) => {}
            None => {
                admitted.predicate = Some(ApplicationPredicateEvidence {
                    task: task.id,
                    status_seen: status,
                    tags_matched: Vec::new(),
                    artifact: None,
                });
            }
        }
        Ok(true)
    }
}

/// Bounded free text: at most `max_chars` scalar values, no control
/// characters except newline and tab, and nothing the credential redactor
/// would change.
fn admitted_text(value: &Value, max_chars: u32) -> Option<ApplicationTextDigest> {
    let text = value.as_str()?;
    let chars = text.chars().count();
    if chars > max_chars as usize
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        || orbit_common::security::redaction::redact_sensitive_env_json(value.clone()) != *value
    {
        return None;
    }
    Some(ApplicationTextDigest {
        sha256: orbit_common::security::release::sha256_hex(text.as_bytes()),
        chars: u32::try_from(chars).ok()?,
    })
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Record one admission against a scope's rate limit, or refuse it when the
/// window is full. The admission log is a private file beside the consent
/// witness, rewritten under an exclusive lock so concurrent processes count
/// each other.
pub(crate) fn admit_within_rate_limit(
    log_path: &Path,
    scope_index: usize,
    limit: &ApplicationRateLimit,
    now: DateTime<Utc>,
) -> Result<bool, OrbitError> {
    orbit_common::fs::io::with_exclusive_file_lock(
        log_path,
        "application admission log",
        || -> Result<bool, OrbitError> {
            let mut log: BTreeMap<String, Vec<i64>> = match std::fs::read(log_path) {
                Ok(raw) => serde_json::from_slice(&raw).map_err(|_| {
                    OrbitError::CapabilityDenied("application admission log is unreadable".into())
                })?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(error) => return Err(error.into()),
            };
            let now_secs = now.timestamp();
            let window_start = now_secs - i64::from(limit.window_seconds);
            let entries = log.entry(scope_index.to_string()).or_default();
            entries.retain(|at| *at > window_start && *at <= now_secs);
            if entries.len() >= limit.admissions as usize {
                return Ok(false);
            }
            entries.push(now_secs);
            log.retain(|_, entries| !entries.is_empty());
            let json = serde_json::to_string(&log).map_err(|_| {
                OrbitError::Execution("application admission log cannot be written".into())
            })?;
            orbit_common::fs::io::atomic_write_text(log_path, &json)?;
            Ok(true)
        },
    )
}
