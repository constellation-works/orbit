//! Lease token audit rows, redaction and audit attribution.

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, params};

use crate::scope::LeaseTokenVerb;
use crate::token::LEASE_TOKEN_PREFIX;

/// Who is running a token command, for the audit row: the `sudo` caller when
/// there is one, else the running user. Attribution only, not authority.
pub fn lease_token_actor() -> String {
    let named = |key: &str| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    match (named("SUDO_USER"), named("USER")) {
        (Some(sudo_user), Some(user)) => format!("{sudo_user} as {user}"),
        (Some(sudo_user), None) => sudo_user,
        (None, Some(user)) => user,
        (None, None) => "unknown".to_string(),
    }
}

pub(super) struct AuditRow<'a> {
    pub(super) at: DateTime<Utc>,
    pub(super) event: &'a str,
    pub(super) token_id: Option<&'a str>,
    pub(super) principal: Option<&'a str>,
    pub(super) workspace: Option<&'a str>,
    pub(super) verb: Option<&'a str>,
    pub(super) reason: Option<&'a str>,
    pub(super) actor: Option<&'a str>,
}

pub(super) fn insert_audit(connection: &Connection, row: &AuditRow<'_>) -> Result<(), OrbitError> {
    // A client can send a token where a workspace or a path belongs.
    let workspace = row.workspace.map(audit_text);
    connection
        .execute(
            "INSERT INTO lease_token_audit \
             (at, event, token_id, principal, workspace, verb, reason, actor) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                timestamp(row.at),
                row.event,
                row.token_id,
                row.principal,
                workspace,
                row.verb,
                row.reason,
                row.actor,
            ],
        )
        .map(|_| ())
        .map_err(sqlite_store_error)
}

/// `text` with every word that contains the token prefix replaced, so an
/// audit row never keeps a token a client sent (STD-05). Words are runs of
/// letters, digits and `_`, so a token is caught wherever it sits in a path
/// or query, whatever follows it.
pub(super) fn audit_text(text: &str) -> String {
    let mut redacted = String::with_capacity(text.len());
    let mut word = String::new();
    // The appended separator closes a final word and is removed below.
    for character in text.chars().chain(std::iter::once(' ')) {
        if character.is_ascii_alphanumeric() || character == '_' {
            word.push(character);
            continue;
        }
        if word.contains(LEASE_TOKEN_PREFIX) {
            redacted.push_str("[redacted]");
        } else {
            redacted.push_str(&word);
        }
        word.clear();
        redacted.push(character);
    }
    redacted.pop();
    redacted
}

pub(super) fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

pub(super) fn joined_verbs(verbs: &[LeaseTokenVerb]) -> String {
    verbs
        .iter()
        .map(|verb| verb.as_str())
        .collect::<Vec<_>>()
        .join(",")
}
